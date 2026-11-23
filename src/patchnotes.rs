//! League's patch notes for the in-app reader. They come from Riot's public
//! news pages (the same pages a browser shows), in the app's language. Every
//! page is downloaded once and kept on disk (`cache\patchnotes\<locale>`), so
//! reading and searching old patches costs no bandwidth. The page's article is
//! turned into a small structure (sections, blocks, lines) that the interface
//! draws with its own design: Riot's HTML is never shown as it is.

use crate::utils::app_data_dir;
use scraper::{ElementRef, Html, Node as DomNode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The languages Riot's site has that the app can ask for.
pub const LOCALES: [&str; 7] = ["en-us", "tr-tr", "de-de", "es-es", "fr-fr", "pt-br", "zh-tw"];
const SITE: &str = "https://www.leagueoflegends.com";
/// The list of patches is asked again after this long.
const LIST_AGE: i64 = 3 * 3600;
/// The newest patch's notes get mid-patch updates: read again after a day.
const NEWEST_AGE: i64 = 24 * 3600;
const MAX_PAGE: usize = 6 * 1024 * 1024;
const MAX_HITS: usize = 60;
/// Pictures come from these hosts only.
const IMAGE_HOSTS: [&str; 4] = ["cmsassets.rgpub.io", "ddragon.leagueoflegends.com", "images.contentstack.io", "lolstatic-a.akamaihd.net"];

static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()
        .ok()
});

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs() as i64)
}

fn valid_locale(locale: &str) -> Result<&'static str, &'static str> {
    LOCALES.iter().copied().find(|known| *known == locale).ok_or("patch_notes_failed")
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.len() <= 120 && slug.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

fn root() -> Option<PathBuf> {
    let directory = app_data_dir().ok()?.join("cache").join("patchnotes");
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

fn directory(locale: &str) -> Option<PathBuf> {
    let directory = root()?.join(locale);
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

fn write(file: &PathBuf, bytes: &[u8]) {
    let Some(parent) = file.parent() else { return };
    if let Ok(mut temporary) = tempfile::NamedTempFile::new_in(parent) {
        use std::io::Write;
        if temporary.write_all(bytes).is_ok() {
            let _ = temporary.persist(file);
        }
    }
}

// ---------------------------------------------------------------- pictures

#[derive(Default)]
struct Images {
    loaded: bool,
    dirty: bool,
    urls: HashMap<String, String>,
}

static IMAGES: LazyLock<Mutex<Images>> = LazyLock::new(|| Mutex::new(Images::default()));

fn images() -> std::sync::MutexGuard<'static, Images> {
    let mut images = IMAGES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !images.loaded {
        images.loaded = true;
        if let Some(text) = root().and_then(|root| std::fs::read(root.join("images.json")).ok()) {
            images.urls = serde_json::from_slice(&text).unwrap_or_default();
        }
    }
    images
}

fn save_images() {
    let mut images = images();
    if !images.dirty {
        return;
    }
    images.dirty = false;
    if let (Some(root), Ok(bytes)) = (root(), serde_json::to_vec(&images.urls)) {
        write(&root.join("images.json"), &bytes);
    }
}

/// The address behind an `x-pn/<hash>.<ext>` picture path (asked by the image cache).
pub fn image_url(path: &str) -> Option<String> {
    images().urls.get(path.trim_start_matches('/')).cloned()
}

/// A picture of the notes as a path the app's image cache serves: the address
/// is checked (Riot's picture hosts only) and big pictures are asked smaller
/// (`width` pixels, WebP) from Riot's own image service.
fn image_key(source: &str, width: u32) -> Option<String> {
    let source = source.trim();
    let source = if source.starts_with("//") { format!("https:{source}") } else { source.to_owned() };
    let mut url = reqwest::Url::parse(&source).ok()?;
    // Riot's resizing proxy in front of Data Dragon: the real address is in `f`.
    if url.host_str() == Some("am-a.akamaihd.net") {
        let inner = url.query_pairs().find(|(key, _)| key == "f").map(|(_, value)| value.into_owned())?;
        url = reqwest::Url::parse(&inner).ok()?;
    }
    if !IMAGE_HOSTS.contains(&url.host_str()?) {
        return None;
    }
    // Old notes still write these hosts with http.
    if url.scheme() == "http" {
        url.set_scheme("https").ok()?;
    }
    if url.scheme() != "https" {
        return None;
    }
    let lower = url.path().to_ascii_lowercase();
    let mut extension = match lower.rsplit('.').next()? {
        "png" => "png",
        "jpg" | "jpeg" => "jpg",
        "webp" => "webp",
        _ => return None,
    };
    match url.host_str()? {
        "cmsassets.rgpub.io" if lower.starts_with("/sanity/images/") => {
            url.set_query(Some(&format!("w={width}&fm=webp&q=78")));
            extension = "webp";
        }
        "images.contentstack.io" => {
            url.set_query(Some(&format!("width={width}&format=webply&quality=78")));
            extension = "webp";
        }
        _ => url.set_query(None),
    }
    url.set_fragment(None);
    let address = url.to_string();
    let digest = Sha256::digest(address.as_bytes());
    let name: String = digest.iter().take(16).map(|byte| format!("{byte:02x}")).collect();
    let key = format!("x-pn/{name}.{extension}");
    let mut images = images();
    if images.urls.get(&key) != Some(&address) {
        images.urls.insert(key.clone(), address);
        images.dirty = true;
    }
    Some(key)
}

// ---------------------------------------------------------------- the list

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    /// Last part of the page's address: the notes' name on disk.
    pub slug: String,
    /// Path of the page on Riot's site.
    pub path: String,
    pub title: String,
    /// "26.23", or "" when the title has no number.
    pub version: String,
    /// ISO date of publication.
    pub date: String,
    pub summary: String,
    /// Banner picture (image cache path), or "".
    pub image: String,
    /// The notes are on disk (readable offline, part of the search).
    #[serde(skip_deserializing)]
    pub cached: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StoredList {
    fetched_at: i64,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListView {
    pub entries: Vec<Entry>,
    pub fetched_at: i64,
    /// Riot's site could not be reached: this is the list kept from last time.
    pub offline: bool,
}

fn read_list(locale: &str) -> Option<StoredList> {
    let bytes = std::fs::read(directory(locale)?.join("list.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn view(locale: &str, stored: StoredList, offline: bool) -> ListView {
    let folder = directory(locale);
    let entries = stored
        .entries
        .into_iter()
        .map(|mut entry| {
            entry.cached = folder.as_ref().is_some_and(|folder| folder.join(format!("{}.json", entry.slug)).is_file());
            entry
        })
        .collect();
    ListView { entries, fetched_at: stored.fetched_at, offline }
}

fn fetch(url: &str) -> Result<String, &'static str> {
    use std::io::Read;
    let response = CLIENT.as_ref().ok_or("patch_notes_failed")?.get(url).send().map_err(|_| "patch_notes_offline")?;
    if !response.status().is_success() {
        return Err("patch_notes_failed");
    }
    let mut text = String::new();
    response.take(MAX_PAGE as u64).read_to_string(&mut text).map_err(|_| "patch_notes_offline")?;
    Ok(text)
}

/// The page's data: Riot's site is a Next.js app that ships it as JSON in the page.
fn page_data(html: &str) -> Option<serde_json::Value> {
    let start = html.find("<script id=\"__NEXT_DATA__\"")?;
    let start = start + html[start..].find('>')? + 1;
    let end = start + html[start..].find("</script>")?;
    let data: serde_json::Value = serde_json::from_str(&html[start..end]).ok()?;
    Some(data["props"]["pageProps"]["page"].clone())
}

fn plain(html: &str) -> String {
    let text: String = Html::parse_fragment(html).root_element().text().collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "26.23" from "League of Legends Patch 26.23 Notes" (any language), else from the address.
fn version_of(title: &str, slug: &str) -> String {
    static TITLE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(\d{1,4}\.(?:[Ss]\d\.)?\d{1,2}[a-z]?)").expect("regex"));
    static SLUG: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(\d{1,4})-(s\d-)?(\d{1,2}[a-z]?)(?:-|$)").expect("regex"));
    if let Some(found) = TITLE.captures(title) {
        return found[1].to_owned();
    }
    SLUG.captures(slug)
        .map(|found| format!("{}.{}{}", &found[1], found.get(2).map_or(String::new(), |season| season.as_str().trim_end_matches('-').to_uppercase() + "."), &found[3]))
        .unwrap_or_default()
}

fn fetch_list(locale: &str) -> Result<Vec<Entry>, &'static str> {
    let page = page_data(&fetch(&format!("{SITE}/{locale}/news/tags/patch-notes/"))?).ok_or("patch_notes_failed")?;
    let prefix = format!("/{locale}/news/");
    let mut entries = Vec::new();
    for blade in page["blades"].as_array().into_iter().flatten().filter(|blade| blade["type"] == "articleCardGrid") {
        for item in blade["items"].as_array().into_iter().flatten() {
            let path = item["action"]["payload"]["url"].as_str().unwrap_or_default().trim_end_matches('/');
            let Some(slug) = path.rsplit('/').next().filter(|slug| valid_slug(slug)) else { continue };
            if !path.starts_with(&prefix) || !path.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/')) {
                continue;
            }
            if entries.iter().any(|entry: &Entry| entry.slug == slug) {
                continue;
            }
            let title = item["title"].as_str().unwrap_or_default().trim().to_owned();
            entries.push(Entry {
                slug: slug.to_owned(),
                path: path.to_owned(),
                version: version_of(&title, slug),
                title,
                date: item["publishedAt"].as_str().unwrap_or_default().to_owned(),
                summary: plain(item["description"]["body"].as_str().unwrap_or_default()),
                image: item["media"]["url"].as_str().and_then(|url| image_key(url, 640)).unwrap_or_default(),
                cached: false,
            });
        }
    }
    if entries.is_empty() {
        return Err("patch_notes_failed");
    }
    Ok(entries)
}

/// Every patch Riot lists, newest first. From disk when it was read less than
/// three hours ago (or when the site cannot be reached), unless `refresh`.
pub fn list(locale: &str, refresh: bool) -> Result<ListView, &'static str> {
    let locale = valid_locale(locale)?;
    let stored = read_list(locale);
    if let Some(stored) = &stored {
        if !refresh && now() - stored.fetched_at < LIST_AGE {
            return Ok(view(locale, stored.clone(), false));
        }
    }
    match fetch_list(locale) {
        Ok(entries) => {
            let stored = StoredList { fetched_at: now(), entries };
            if let (Some(folder), Ok(bytes)) = (directory(locale), serde_json::to_vec(&stored)) {
                write(&folder.join("list.json"), &bytes);
            }
            save_images();
            Ok(view(locale, stored, false))
        }
        Err(error) => stored.map(|stored| view(locale, stored, true)).ok_or(error),
    }
}

/// The address of a patch's page on Riot's site (only for a patch of the list).
pub fn page_url(locale: &str, slug: &str) -> Option<String> {
    let locale = valid_locale(locale).ok()?;
    let entry = read_list(locale)?.entries.into_iter().find(|entry| entry.slug == slug)?;
    Some(format!("{SITE}{}/", entry.path))
}

// ---------------------------------------------------------------- one patch

/// A run of text with one look.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Span {
    pub t: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub b: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub i: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub href: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "k", rename_all = "lowercase")]
pub enum Node {
    /// A small heading inside a block ("Q - Disintegrate", "Base Stats"), with its icon.
    H { icon: String, spans: Vec<Span> },
    /// A paragraph.
    P { spans: Vec<Span> },
    /// The designers' note about the change.
    Q { spans: Vec<Span> },
    /// One change ("Damage: 10 ⇒ 20"); `depth` 0 or more for nested lists.
    Li { depth: u8, spans: Vec<Span> },
    /// A picture; `small` for icons.
    Img { src: String, small: bool },
    Hr,
}

/// One card: a champion, an item, a system change.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Block {
    pub title: String,
    pub icon: String,
    /// "champion" or "item" when the card's portrait says so, else "" (systems, modes, skins...).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub kind: String,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Section {
    pub title: String,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notes {
    pub slug: String,
    pub title: String,
    pub version: String,
    pub date: String,
    pub summary: String,
    pub image: String,
    /// The page on Riot's site.
    pub url: String,
    pub sections: Vec<Section>,
    pub fetched_at: i64,
}

/// What is kept on disk: the article as Riot wrote it, so a better reader in a
/// later version of the app needs no new download.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Raw {
    fetched_at: i64,
    path: String,
    title: String,
    date: String,
    summary: String,
    /// The banner picture's address on Riot's site.
    banner: String,
    body: String,
}

fn link(href: &str) -> String {
    let href = href.trim();
    if href.starts_with("https://") {
        href.to_owned()
    } else if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') && !href.starts_with("//") {
        format!("{SITE}{href}")
    } else {
        String::new()
    }
}

fn push_text(out: &mut Vec<Span>, text: &str, bold: bool, italic: bool, href: &str) {
    let mut spaced = out.last().is_none_or(|last| last.t.ends_with([' ', '\n']));
    let mut clean = String::with_capacity(text.len());
    for letter in text.chars() {
        if letter == '\n' && text.len() == 1 {
            clean.push('\n');
            spaced = true;
        } else if letter.is_whitespace() || letter == '\u{a0}' {
            if !spaced {
                clean.push(' ');
                spaced = true;
            }
        } else {
            clean.push(letter);
            spaced = false;
        }
    }
    if clean.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.b == bold && last.i == italic && last.href == href => last.t.push_str(&clean),
        _ => out.push(Span { t: clean, b: bold, i: italic, href: href.to_owned() }),
    }
}

fn inline(element: ElementRef, bold: bool, italic: bool, href: &str, out: &mut Vec<Span>) {
    for child in element.children() {
        match child.value() {
            DomNode::Text(text) => push_text(out, text, bold, italic, href),
            DomNode::Element(inner) => {
                let Some(child) = ElementRef::wrap(child) else { continue };
                match inner.name() {
                    "br" => push_text(out, "\n", bold, italic, href),
                    "strong" | "b" => inline(child, true, italic, href, out),
                    "em" | "i" => inline(child, bold, true, href, out),
                    "a" => {
                        let target = link(inner.attr("href").unwrap_or_default());
                        inline(child, bold, italic, if target.is_empty() { href } else { &target }, out);
                    }
                    "ul" | "ol" | "img" | "script" | "style" | "iframe" | "table" | "video" | "svg" => {}
                    _ => inline(child, bold, italic, href, out),
                }
            }
            _ => {}
        }
    }
}

fn spans(element: ElementRef) -> Vec<Span> {
    let mut out = Vec::new();
    inline(element, false, false, "", &mut out);
    if let Some(first) = out.first_mut() {
        first.t = first.t.trim_start().to_owned();
    }
    if let Some(last) = out.last_mut() {
        last.t = last.t.trim_end().to_owned();
    }
    out.retain(|span| !span.t.is_empty());
    out
}

fn blank(spans: &[Span]) -> bool {
    spans.iter().all(|span| span.t.trim().is_empty())
}

fn has_class(element: ElementRef, class: &str) -> bool {
    element.value().classes().any(|name| name == class)
}

/// Champion, item and ability icons come from Data Dragon.
fn is_icon(source: &str) -> bool {
    source.contains("ddragon.leagueoflegends.com")
}

#[derive(Default)]
struct Walker {
    sections: Vec<Section>,
}

impl Walker {
    fn section(&mut self) -> &mut Section {
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        self.sections.last_mut().expect("a section")
    }

    fn block(&mut self) -> &mut Block {
        let section = self.section();
        if section.blocks.is_empty() {
            section.blocks.push(Block::default());
        }
        section.blocks.last_mut().expect("a block")
    }

    fn new_block(&mut self) {
        let section = self.section();
        // An empty block is reused instead of leaving a hole.
        if section.blocks.last().is_none_or(|block| !block.title.is_empty() || !block.icon.is_empty() || !block.nodes.is_empty()) {
            section.blocks.push(Block::default());
        }
    }

    fn push(&mut self, node: Node) {
        self.block().nodes.push(node);
    }

    fn picture(&mut self, source: &str, linked: bool) {
        if is_icon(source) {
            let Some(key) = image_key(source, 128) else { return };
            let block = self.block();
            // The champion's or item's portrait above its name.
            if block.icon.is_empty() && block.title.is_empty() && block.nodes.is_empty() {
                block.icon = key;
                // Data Dragon's folders tell what the card is about, in every language.
                block.kind = if source.contains("/img/champion/") {
                    "champion".into()
                } else if source.contains("/img/item/") {
                    "item".into()
                } else {
                    String::new()
                };
            } else if linked || !block.nodes.is_empty() {
                self.push(Node::Img { src: key, small: true });
            }
        } else if let Some(key) = image_key(source, 960) {
            self.push(Node::Img { src: key, small: false });
        }
    }

    fn pictures(&mut self, element: ElementRef) {
        static IMAGE: LazyLock<scraper::Selector> = LazyLock::new(|| scraper::Selector::parse("img").expect("selector"));
        for image in element.select(&IMAGE) {
            if let Some(source) = image.value().attr("src") {
                let linked = image.parent().and_then(ElementRef::wrap).is_some_and(|parent| parent.value().name() == "a" && !has_class(parent, "reference-link"));
                self.picture(source, linked);
            }
        }
    }

    fn list(&mut self, element: ElementRef, depth: u8) {
        for item in element.children().filter_map(ElementRef::wrap) {
            match item.value().name() {
                "li" => {
                    let text = spans(item);
                    if !blank(&text) {
                        self.push(Node::Li { depth, spans: text });
                    }
                    for nested in item.children().filter_map(ElementRef::wrap).filter(|nested| matches!(nested.value().name(), "ul" | "ol")) {
                        self.list(nested, depth.saturating_add(1).min(3));
                    }
                }
                "ul" | "ol" => self.list(item, depth.saturating_add(1).min(3)),
                _ => {}
            }
        }
    }

    fn children(&mut self, element: ElementRef) {
        for child in element.children() {
            match child.value() {
                DomNode::Text(text) => {
                    let mut out = Vec::new();
                    push_text(&mut out, text, false, false, "");
                    if !blank(&out) {
                        out[0].t = out[0].t.trim().to_owned();
                        self.push(Node::P { spans: out });
                    }
                }
                DomNode::Element(_) => {
                    if let Some(child) = ElementRef::wrap(child) {
                        self.visit(child);
                    }
                }
                _ => {}
            }
        }
    }

    fn visit(&mut self, element: ElementRef) {
        match element.value().name() {
            "h1" | "h2" => {
                let title = spans(element).iter().map(|span| span.t.as_str()).collect::<String>().trim().to_owned();
                if !title.is_empty() {
                    self.sections.push(Section { title, blocks: Vec::new() });
                }
            }
            "h3" => {
                let title = spans(element).iter().map(|span| span.t.as_str()).collect::<String>().trim().to_owned();
                if title.is_empty() {
                    return;
                }
                let block = self.block();
                if !block.title.is_empty() || !block.nodes.is_empty() {
                    self.new_block();
                }
                self.block().title = title;
            }
            "h4" | "h5" | "h6" => {
                static IMAGE: LazyLock<scraper::Selector> = LazyLock::new(|| scraper::Selector::parse("img").expect("selector"));
                let icon = element.select(&IMAGE).next().and_then(|image| image.value().attr("src")).and_then(|source| image_key(source, 128)).unwrap_or_default();
                let text = spans(element);
                if !blank(&text) {
                    self.push(Node::H { icon, spans: text });
                }
            }
            "p" => {
                // "Back to top" links of the old notes.
                if element.children().filter_map(ElementRef::wrap).any(|child| has_class(child, "btt")) {
                    return;
                }
                let text = spans(element);
                if !blank(&text) {
                    self.push(Node::P { spans: text });
                }
                self.pictures(element);
            }
            "blockquote" => {
                let paragraphs: Vec<ElementRef> = element.children().filter_map(ElementRef::wrap).filter(|child| child.value().name() == "p").collect();
                if paragraphs.is_empty() {
                    let text = spans(element);
                    if !blank(&text) {
                        self.push(Node::Q { spans: text });
                    }
                } else {
                    for paragraph in paragraphs {
                        let text = spans(paragraph);
                        if !blank(&text) {
                            self.push(Node::Q { spans: text });
                        }
                    }
                }
            }
            "ul" | "ol" => self.list(element, 0),
            "hr" => {
                if self.block().nodes.last().is_some_and(|last| !matches!(last, Node::Hr)) {
                    self.push(Node::Hr);
                }
            }
            "img" => {
                if let Some(source) = element.value().attr("src") {
                    self.picture(source, false);
                }
            }
            "table" => {
                static ROW: LazyLock<scraper::Selector> = LazyLock::new(|| scraper::Selector::parse("tr").expect("selector"));
                static CELL: LazyLock<scraper::Selector> = LazyLock::new(|| scraper::Selector::parse("th, td").expect("selector"));
                for row in element.select(&ROW) {
                    let mut line: Vec<Span> = Vec::new();
                    for cell in row.select(&CELL) {
                        let text = spans(cell);
                        if blank(&text) {
                            continue;
                        }
                        if !line.is_empty() {
                            line.push(Span { t: " · ".into(), ..Default::default() });
                        }
                        line.extend(text);
                    }
                    if !line.is_empty() {
                        self.push(Node::Li { depth: 0, spans: line });
                    }
                }
            }
            "script" | "style" | "iframe" | "video" | "svg" | "br" | "noscript" => {}
            "a" | "span" | "strong" | "b" | "em" | "i" => {
                // The designers' names under the introduction are left out with their portraits.
                if has_class(element, "context-designer") || has_class(element, "btt") {
                    return;
                }
                let text = spans(element);
                if !blank(&text) {
                    self.push(Node::P { spans: text });
                }
                if element.value().name() == "a" {
                    self.pictures(element);
                }
            }
            "div" if has_class(element, "content-border") => {
                self.new_block();
                self.children(element);
            }
            "div" if has_class(element, "context-designers") => {}
            _ => self.children(element),
        }
    }

    fn finish(mut self) -> Vec<Section> {
        for section in &mut self.sections {
            for block in &mut section.blocks {
                while matches!(block.nodes.last(), Some(Node::Hr)) {
                    block.nodes.pop();
                }
                while matches!(block.nodes.first(), Some(Node::Hr)) {
                    block.nodes.remove(0);
                }
            }
            section.blocks.retain(|block| !block.title.is_empty() || !block.nodes.is_empty());
        }
        self.sections.retain(|section| !section.blocks.is_empty());
        self.sections
    }
}

fn parse(body: &str) -> Vec<Section> {
    let document = Html::parse_fragment(body);
    let mut walker = Walker::default();
    walker.children(document.root_element());
    walker.finish()
}

fn build(slug: &str, raw: &Raw) -> Notes {
    Notes {
        slug: slug.to_owned(),
        version: version_of(&raw.title, slug),
        title: raw.title.clone(),
        date: raw.date.clone(),
        summary: raw.summary.clone(),
        image: image_key(&raw.banner, 1280).unwrap_or_default(),
        url: format!("{SITE}{}/", raw.path),
        sections: parse(&raw.body),
        fetched_at: raw.fetched_at,
    }
}

fn fetch_raw(entry: &Entry) -> Result<Raw, &'static str> {
    let page = page_data(&fetch(&format!("{SITE}{}/", entry.path))?).ok_or("patch_notes_failed")?;
    let mut raw = Raw { fetched_at: now(), path: entry.path.clone(), title: entry.title.clone(), date: entry.date.clone(), summary: entry.summary.clone(), ..Default::default() };
    for blade in page["blades"].as_array().into_iter().flatten() {
        if blade["type"] == "articleMasthead" {
            if let Some(title) = blade["title"].as_str().filter(|title| !title.trim().is_empty()) {
                raw.title = title.trim().to_owned();
            }
            if let Some(summary) = blade["description"]["body"].as_str() {
                raw.summary = plain(summary);
            }
            if let Some(date) = blade["publishDate"].as_str() {
                raw.date = date.to_owned();
            }
            if let Some(banner) = blade["banner"]["url"].as_str().or_else(|| blade["banner"]["media"]["url"].as_str()) {
                raw.banner = banner.to_owned();
            }
            continue;
        }
        // The article itself: the longest text of the page (its blade's name changed over the years).
        let body = blade["richText"]["body"].as_str().or_else(|| blade["body"]["body"].as_str()).unwrap_or_default();
        if body.len() > raw.body.len() {
            raw.body = body.to_owned();
        }
    }
    if raw.body.is_empty() {
        return Err("patch_notes_failed");
    }
    Ok(raw)
}

fn read_raw(locale: &str, slug: &str) -> Option<Raw> {
    let bytes = std::fs::read(directory(locale)?.join(format!("{slug}.json"))).ok()?;
    serde_json::from_slice::<Raw>(&bytes).ok().filter(|raw| !raw.body.is_empty())
}

// ---------------------------------------------------------------- memory + search

struct BlockText {
    section: usize,
    block: usize,
    /// Words of each part, lower case without accents, between spaces (" annie ").
    title: String,
    head: String,
    body: String,
    /// The parts of the card under each small heading ("Q - Razor Shuriken"): nodes `start..end`.
    groups: Vec<Group>,
}

struct Group {
    start: usize,
    end: usize,
    head: String,
}

struct Indexed {
    locale: String,
    notes: Notes,
    blocks: Vec<BlockText>,
}

static MEMORY: LazyLock<Mutex<HashMap<String, Arc<Indexed>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn memory() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Indexed>>> {
    MEMORY.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lower case, accents dropped, anything that is not a letter or digit becomes a space.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(' ');
    for letter in text.chars().flat_map(char::to_lowercase) {
        let plain = match letter {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => "a",
            'ç' => "c",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'í' | 'ì' | 'î' | 'ï' | 'ı' => "i",
            'ñ' => "n",
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => "o",
            'ú' | 'ù' | 'û' | 'ü' => "u",
            'ş' => "s",
            'ğ' => "g",
            'ß' => "ss",
            '\u{307}' => "",
            other if other.is_alphanumeric() => {
                out.push(other);
                continue;
            }
            _ => " ",
        };
        if plain == " " {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push_str(plain);
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

fn text_of(spans: &[Span]) -> String {
    spans.iter().map(|span| span.t.as_str()).collect::<Vec<_>>().join(" ")
}

fn index(locale: &str, notes: Notes) -> Indexed {
    let mut blocks = Vec::new();
    for (section_index, section) in notes.sections.iter().enumerate() {
        for (block_index, block) in section.blocks.iter().enumerate() {
            let mut head = section.title.clone();
            let mut body = String::new();
            let mut groups: Vec<Group> = Vec::new();
            for (at, node) in block.nodes.iter().enumerate() {
                match node {
                    Node::H { spans, .. } => {
                        head.push(' ');
                        head.push_str(&text_of(spans));
                        if let Some(open) = groups.last_mut() {
                            open.end = at;
                        }
                        groups.push(Group { start: at, end: block.nodes.len(), head: normalize(&text_of(spans)) });
                    }
                    Node::P { spans } | Node::Q { spans } | Node::Li { spans, .. } => {
                        body.push(' ');
                        body.push_str(&text_of(spans));
                    }
                    Node::Img { .. } | Node::Hr => {}
                }
            }
            // A rule between two parts belongs to neither.
            for group in &mut groups {
                while group.end > group.start + 1 && matches!(block.nodes[group.end - 1], Node::Hr) {
                    group.end -= 1;
                }
            }
            blocks.push(BlockText { section: section_index, block: block_index, title: normalize(&block.title), head: normalize(&head), body: normalize(&body), groups });
        }
    }
    Indexed { locale: locale.to_owned(), notes, blocks }
}

fn remember(locale: &str, slug: &str, raw: &Raw) -> Arc<Indexed> {
    let indexed = Arc::new(index(locale, build(slug, raw)));
    memory().insert(format!("{locale}/{slug}"), indexed.clone());
    indexed
}

/// One patch's notes. From disk when they are there; downloaded (once) otherwise.
pub fn notes(locale: &str, slug: &str, refresh: bool) -> Result<Notes, &'static str> {
    let locale = valid_locale(locale)?;
    if !valid_slug(slug) {
        return Err("patch_notes_failed");
    }
    let stored = read_list(locale);
    let newest = stored.as_ref().and_then(|stored| stored.entries.first()).is_some_and(|entry| entry.slug == slug);
    let cached = read_raw(locale, slug);
    if let Some(raw) = &cached {
        let old = newest && now() - raw.fetched_at > NEWEST_AGE;
        if !refresh && !old {
            if let Some(known) = memory().get(&format!("{locale}/{slug}")) {
                return Ok(known.notes.clone());
            }
            let notes = remember(locale, slug, raw).notes.clone();
            save_images();
            return Ok(notes);
        }
    }
    let entry = stored.and_then(|stored| stored.entries.into_iter().find(|entry| entry.slug == slug));
    let fetched = match entry {
        Some(entry) => fetch_raw(&entry),
        None => Err("patch_notes_failed"),
    };
    let raw = match fetched {
        Ok(raw) => {
            if let (Some(folder), Ok(bytes)) = (directory(locale), serde_json::to_vec(&raw)) {
                write(&folder.join(format!("{slug}.json")), &bytes);
            }
            raw
        }
        Err(error) => cached.ok_or(error)?,
    };
    let notes = remember(locale, slug, &raw).notes.clone();
    save_images();
    Ok(notes)
}

/// Every patch on disk for this language, read into memory (the search reads them all).
fn load_all(locale: &str) -> Vec<Arc<Indexed>> {
    let slugs: Vec<String> = directory(locale)
        .and_then(|folder| std::fs::read_dir(folder).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|file| file.file_name().to_str().and_then(|name| name.strip_suffix(".json")).map(str::to_owned))
        .filter(|slug| slug != "list" && valid_slug(slug))
        .collect();
    let mut loaded = false;
    for slug in &slugs {
        if memory().contains_key(&format!("{locale}/{slug}")) {
            continue;
        }
        if let Some(raw) = read_raw(locale, slug) {
            remember(locale, slug, &raw);
            loaded = true;
        }
    }
    if loaded {
        save_images();
    }
    let mut all: Vec<Arc<Indexed>> = memory().values().filter(|indexed| indexed.locale == locale).cloned().collect();
    all.sort_by(|left, right| right.notes.date.cmp(&left.notes.date));
    all
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub slug: String,
    pub version: String,
    pub date: String,
    pub section: String,
    pub section_index: usize,
    pub block_index: usize,
    /// The card, or only the parts of it that were asked for ("zed q" → Zed's Q) when `focused`.
    pub block: Block,
    pub focused: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchView {
    pub hits: Vec<Hit>,
    /// Blocks that match (may be more than `hits`).
    pub total: usize,
    /// Patches that were searched (the ones on disk).
    pub patches: usize,
    /// The words that were searched, after spelling fixes (for highlighting).
    pub terms: Vec<String>,
    /// The search as corrected ("anie" → "annie"), or "" when nothing changed.
    pub corrected: String,
    /// Patches named by number in the search ("26.23").
    pub versions: Vec<String>,
    /// How many patches have a match.
    pub patch_count: usize,
}

fn distance(left: &str, right: &str, limit: usize) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > limit {
        return limit + 1;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, a) in left.iter().enumerate() {
        let mut current = vec![row + 1];
        for (column, b) in right.iter().enumerate() {
            let cost = usize::from(a != b);
            current.push((previous[column] + cost).min(previous[column + 1] + 1).min(current[column] + 1));
        }
        previous = current;
    }
    previous[right.len()]
}

fn same_version(left: &str, right: &str) -> bool {
    let parts = |version: &str| version.split('.').map(|part| part.trim_start_matches('0').to_lowercase()).collect::<Vec<_>>();
    !left.is_empty() && parts(left) == parts(right)
}

/// Search every patch on disk. All words must be in a block (its name counts
/// most, then its headings, then its text); a word nobody wrote is replaced
/// by the closest one that exists; "26.23" narrows to that patch.
///
/// `topic`: "champion" or "item" keep only those cards and want the name in
/// the card's title (newest first: the top one is the last change); "other"
/// keeps the rest (systems, modes, skins); anything else searches everything.
/// A lone Q, W, E, R or P is an ability: "zed q" gives Zed's cards cut down to
/// the Q part. Other words found in a part's heading cut the card the same way.
pub fn search(locale: &str, query: &str, topic: &str) -> Result<SearchView, &'static str> {
    static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b(\d{1,4}\.\d{1,2})\b").expect("regex"));
    let locale = valid_locale(locale)?;
    let all = load_all(locale);
    let mut view = SearchView { patches: all.len(), ..Default::default() };
    let query: String = query.chars().take(120).collect();
    let wanted: Vec<String> = VERSION.captures_iter(&query).map(|found| found[1].to_owned()).collect();
    let rest = VERSION.replace_all(&query, " ");
    let mut terms: Vec<String> = normalize(&rest).split_whitespace().map(str::to_owned).collect();
    terms.dedup();
    terms.truncate(8);
    view.versions = all.iter().filter(|indexed| wanted.iter().any(|version| same_version(version, &indexed.notes.version))).map(|indexed| indexed.notes.slug.clone()).collect();
    if terms.is_empty() {
        return Ok(view);
    }
    let scope: Vec<&Arc<Indexed>> = if wanted.is_empty() { all.iter().collect() } else { all.iter().filter(|indexed| view.versions.contains(&indexed.notes.slug)).collect() };

    // Spelling: a word that starts no word of the notes is replaced by the closest one.
    let mut corrected = false;
    for term in &mut terms {
        let ascii = term.is_ascii();
        let needle = format!(" {term}");
        let known = scope.iter().any(|indexed| indexed.blocks.iter().any(|block| if ascii { block.title.contains(&needle) || block.head.contains(&needle) || block.body.contains(&needle) } else { block.title.contains(term.as_str()) || block.head.contains(term.as_str()) || block.body.contains(term.as_str()) }));
        let length = term.chars().count();
        if known || !ascii || length < 4 {
            continue;
        }
        let limit = if length >= 7 { 2 } else { 1 };
        let mut counts: HashMap<&str, (usize, u32)> = HashMap::new();
        for indexed in &scope {
            for block in &indexed.blocks {
                for (weight, field) in [(8, &block.title), (2, &block.head), (1, &block.body)] {
                    for word in field.split_whitespace() {
                        if word.len().abs_diff(term.len()) > limit {
                            continue;
                        }
                        let entry = counts.entry(word).or_insert_with(|| (distance(term, word, limit), 0));
                        entry.1 += weight;
                    }
                }
            }
        }
        let best = counts.into_iter().filter(|(_, (far, _))| *far <= limit).min_by(|left, right| left.1 .0.cmp(&right.1 .0).then(right.1 .1.cmp(&left.1 .1)).then(left.0.cmp(right.0)));
        if let Some((word, _)) = best {
            *term = word.to_owned();
            corrected = true;
        }
    }
    if corrected {
        view.corrected = terms.join(" ");
    }

    let named = matches!(topic, "champion" | "item");
    let ability = |term: &str| matches!(term, "q" | "w" | "e" | "r" | "p");
    let in_heading = |group: &Group, term: &str| -> bool {
        if ability(term) {
            group.head.starts_with(&format!(" {term} ")) || (term == "p" && group.head.starts_with(" passive "))
        } else if term.is_ascii() {
            group.head.contains(&format!(" {term}"))
        } else {
            group.head.contains(term)
        }
    };
    let mut found: Vec<(f32, &Arc<Indexed>, &BlockText, Vec<usize>)> = Vec::new();
    for indexed in &scope {
        'blocks: for block in &indexed.blocks {
            let kind = indexed.notes.sections[block.section].blocks[block.block].kind.as_str();
            let wanted = match topic {
                "champion" | "item" => kind == topic,
                "other" => kind.is_empty(),
                _ => true,
            };
            if !wanted {
                continue;
            }
            let mut score = 0.0;
            let mut titled = false;
            // Words that are not the card's name: they may point at one part of it.
            let mut detail: Vec<&str> = Vec::new();
            for term in &terms {
                if ability(term) {
                    if !block.groups.iter().any(|group| in_heading(group, term)) {
                        continue 'blocks;
                    }
                    score += 6.0;
                    detail.push(term);
                    continue;
                }
                let part = |field: &str, exact: f32, prefix: f32| -> f32 {
                    if term.is_ascii() {
                        if field.contains(&format!(" {term} ")) {
                            exact
                        } else if field.contains(&format!(" {term}")) {
                            prefix
                        } else {
                            0.0
                        }
                    } else if field.contains(term.as_str()) {
                        exact
                    } else {
                        0.0
                    }
                };
                let title = part(&block.title, 12.0, 9.0);
                let best = title.max(part(&block.head, 5.0, 4.0)).max(part(&block.body, 2.0, 1.5));
                if best == 0.0 {
                    continue 'blocks;
                }
                if title > 0.0 {
                    titled = true;
                } else {
                    detail.push(term);
                }
                score += best;
            }
            if named && !titled {
                continue;
            }
            // The parts whose heading has every such word; all of the card when none (or all) do.
            let mut parts: Vec<usize> = Vec::new();
            if !detail.is_empty() {
                parts = (0..block.groups.len()).filter(|at| detail.iter().all(|term| in_heading(&block.groups[*at], term))).collect();
                if parts.len() == block.groups.len() && block.groups.first().is_some_and(|group| group.start == 0) {
                    parts.clear();
                }
            }
            found.push((score, indexed, block, parts));
        }
    }
    if named {
        // A champion's or item's history: newest first.
        found.sort_by(|left, right| right.1.notes.date.cmp(&left.1.notes.date).then(right.0.total_cmp(&left.0)).then(left.2.section.cmp(&right.2.section)).then(left.2.block.cmp(&right.2.block)));
    } else {
        found.sort_by(|left, right| right.0.total_cmp(&left.0).then(right.1.notes.date.cmp(&left.1.notes.date)).then(left.2.section.cmp(&right.2.section)).then(left.2.block.cmp(&right.2.block)));
    }
    view.total = found.len();
    let mut slugs: Vec<&str> = found.iter().map(|hit| hit.1.notes.slug.as_str()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    view.patch_count = slugs.len();
    view.hits = found
        .into_iter()
        .take(MAX_HITS)
        .map(|(_, indexed, block, parts)| {
            let section = &indexed.notes.sections[block.section];
            let mut card = section.blocks[block.block].clone();
            let focused = !parts.is_empty();
            if focused {
                let mut nodes = Vec::new();
                for at in parts {
                    let group = &block.groups[at];
                    if !nodes.is_empty() {
                        nodes.push(Node::Hr);
                    }
                    nodes.extend(card.nodes[group.start..group.end].iter().cloned());
                }
                card.nodes = nodes;
            }
            Hit {
                slug: indexed.notes.slug.clone(),
                version: indexed.notes.version.clone(),
                date: indexed.notes.date.clone(),
                section: section.title.clone(),
                section_index: block.section,
                block_index: block.block,
                block: card,
                focused,
            }
        })
        .collect();
    view.terms = terms;
    Ok(view)
}

// ---------------------------------------------------------------- download for the search

/// Download the notes that are not on disk yet, newest first: the `count`
/// newest patches (0 = all of them). One page at a time with a pause, so a
/// slow line stays usable. `progress(done, total)` after each page; stops when
/// `cancel` is set. Returns how many were downloaded.
pub fn download(locale: &str, count: usize, cancel: &AtomicBool, progress: impl Fn(usize, usize)) -> Result<usize, &'static str> {
    let locale = valid_locale(locale)?;
    let listed = list(locale, false)?;
    let wanted: Vec<&Entry> = listed.entries.iter().take(if count == 0 { usize::MAX } else { count }).filter(|entry| !entry.cached).collect();
    let total = wanted.len();
    let mut done = 0;
    let mut failures = 0;
    progress(0, total);
    for entry in wanted {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match notes(locale, &entry.slug, false) {
            Ok(_) => failures = 0,
            Err(_) => {
                failures += 1;
                // The line is down: no point in asking for the rest.
                if failures >= 3 {
                    break;
                }
            }
        }
        done += 1;
        progress(done, total);
        std::thread::sleep(Duration::from_millis(350));
    }
    Ok(done)
}

/// Parse an article's HTML (tests and tools).
pub fn parse_for_test(body: &str) -> Vec<Section> {
    parse(body)
}
