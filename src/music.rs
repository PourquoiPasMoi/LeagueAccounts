//! Music player (off by default, Settings › Music): playlists of YouTube songs.
//!
//! Sound comes from YouTube's own embedded player in the page (kept tiny and
//! hidden, so YouTube sends its smallest video with the sound). YouTube blocks
//! audio-only downloads by other apps (stream answers 403 after the first
//! piece, then "confirm you're not a bot"), and getting around that is not
//! something this app does.
//!
//! This module keeps the playlists (`music-library.json`), reads song names
//! and playlists (YouTube's app API with the Android VR client: small answers,
//! a playlist page ~6 KB, a search ~9 KB; single songs through oEmbed, ~1 KB)
//! and caches cover pictures on disk.

use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

// ---------------------------------------------------------------- data

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub artist: String,
    /// Seconds (0 = not known yet).
    #[serde(default)]
    pub duration: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    /// The YouTube playlist it was imported from ("" = made in the app).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    #[serde(default)]
    pub playlists: Vec<Playlist>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    /// The whole feature: sidebar row, page, mini player.
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheInfo {
    pub bytes: u64,
    pub pictures: u32,
}

fn prefs_file() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("music.json"))
}

fn library_file() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("music-library.json"))
}

fn cache_dir(kind: &str) -> Option<PathBuf> {
    let directory = app_data_dir().ok()?.join("cache").join("music").join(kind);
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

static PREFS: LazyLock<Mutex<Option<Prefs>>> = LazyLock::new(|| Mutex::new(None));

pub fn prefs() -> Prefs {
    let mut cached = PREFS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(prefs) = cached.as_ref() {
        return prefs.clone();
    }
    let prefs: Prefs = prefs_file()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    *cached = Some(prefs.clone());
    prefs
}

pub fn set_prefs(prefs: Prefs) -> Result<Prefs, &'static str> {
    let path = prefs_file().ok_or("file_write")?;
    let body = serde_json::to_vec_pretty(&prefs).map_err(|_| "file_write")?;
    std::fs::write(path, body).map_err(|_| "file_write")?;
    *PREFS.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(prefs.clone());
    Ok(prefs)
}

/// One writer at a time for the library file.
static LIBRARY: Mutex<()> = Mutex::new(());

pub fn library() -> Library {
    library_file()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_library(library: &Library) -> Result<(), &'static str> {
    let path = library_file().ok_or("file_write")?;
    let body = serde_json::to_vec(library).map_err(|_| "file_write")?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, body).map_err(|_| "file_write")?;
    std::fs::rename(&temp, &path).map_err(|_| "file_write")
}

/// Change the library under the lock and save it.
fn edit<T>(change: impl FnOnce(&mut Library) -> Result<T, &'static str>) -> Result<(Library, T), &'static str> {
    let _guard = LIBRARY.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut library = library();
    let value = change(&mut library)?;
    save_library(&library)?;
    Ok((library, value))
}

fn new_id() -> String {
    format!("{:x}", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |time| time.as_nanos()))
}

fn clean_name(name: &str) -> Result<String, &'static str> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    if name.is_empty() {
        Err("music_name_empty")
    } else {
        Ok(name)
    }
}

fn playlist_mut<'a>(library: &'a mut Library, id: &str) -> Result<&'a mut Playlist, &'static str> {
    library.playlists.iter_mut().find(|playlist| playlist.id == id).ok_or("music_playlist_missing")
}

pub fn create_playlist(name: &str) -> Result<Library, &'static str> {
    let name = clean_name(name)?;
    edit(|library| {
        library.playlists.push(Playlist { id: new_id(), name, source: String::new(), tracks: Vec::new() });
        Ok(())
    })
    .map(|(library, _)| library)
}

pub fn rename_playlist(id: &str, name: &str) -> Result<Library, &'static str> {
    let name = clean_name(name)?;
    edit(|library| {
        playlist_mut(library, id)?.name = name;
        Ok(())
    })
    .map(|(library, _)| library)
}

pub fn delete_playlist(id: &str) -> Result<Library, &'static str> {
    edit(|library| {
        library.playlists.retain(|playlist| playlist.id != id);
        Ok(())
    })
    .map(|(library, _)| library)
}

pub fn remove_track(id: &str, index: usize) -> Result<Library, &'static str> {
    edit(|library| {
        let playlist = playlist_mut(library, id)?;
        if index < playlist.tracks.len() {
            playlist.tracks.remove(index);
        }
        Ok(())
    })
    .map(|(library, _)| library)
}

pub fn move_track(id: &str, from: usize, to: usize) -> Result<Library, &'static str> {
    edit(|library| {
        let playlist = playlist_mut(library, id)?;
        if from < playlist.tracks.len() {
            let track = playlist.tracks.remove(from);
            let to = to.min(playlist.tracks.len());
            playlist.tracks.insert(to, track);
        }
        Ok(())
    })
    .map(|(library, _)| library)
}

/// Songs already known (search results) added without asking YouTube again.
pub fn add_tracks(id: &str, tracks: Vec<Track>) -> Result<Library, &'static str> {
    let tracks: Vec<Track> = tracks.into_iter().filter(|track| valid_id(&track.id)).collect();
    edit(|library| {
        playlist_mut(library, id)?.tracks.extend(tracks);
        Ok(())
    })
    .map(|(library, _)| library)
}

/// Links pasted by the player (one or many, any separator): songs are added,
/// a playlist link adds all its songs. Returns how many songs were added.
pub fn add_links(id: &str, input: &str) -> Result<(Library, usize), &'static str> {
    // The playlist must exist before anything is fetched.
    if !library().playlists.iter().any(|playlist| playlist.id == id) {
        return Err("music_playlist_missing");
    }
    let links: Vec<Link> = input.split(|c: char| c.is_whitespace() || c == ',').filter_map(parse_link).collect();
    if links.is_empty() {
        return Err("music_link_invalid");
    }
    let mut found = Vec::new();
    let mut last_error = None;
    for link in links {
        match link {
            Link::Video(video) => match song(&video) {
                Ok(track) => found.push(track),
                Err(error) => last_error = Some(error),
            },
            Link::Playlist(list) => match playlist(&list) {
                Ok((_, tracks)) => found.extend(tracks),
                Err(error) => last_error = Some(error),
            },
        }
    }
    if found.is_empty() {
        return Err(last_error.unwrap_or("music_unavailable"));
    }
    let count = found.len();
    edit(|library| {
        playlist_mut(library, id)?.tracks.extend(found);
        Ok(count)
    })
}

/// A YouTube playlist becomes a new playlist with its name.
pub fn import_playlist(input: &str) -> Result<Library, &'static str> {
    let list = input
        .split_whitespace()
        .find_map(|word| match parse_link_list_first(word) {
            Some(Link::Playlist(list)) => Some(list),
            _ => None,
        })
        .ok_or("music_playlist_link")?;
    let (title, tracks) = playlist(&list)?;
    edit(|library| {
        library.playlists.push(Playlist {
            id: new_id(),
            name: if title.trim().is_empty() { "YouTube".to_owned() } else { title.chars().take(80).collect() },
            source: list,
            tracks,
        });
        Ok(())
    })
    .map(|(library, _)| library)
}

/// Re-read an imported playlist from YouTube (its songs replace the old list).
pub fn sync_playlist(id: &str) -> Result<Library, &'static str> {
    let source = library()
        .playlists
        .iter()
        .find(|playlist| playlist.id == id)
        .map(|playlist| playlist.source.clone())
        .ok_or("music_playlist_missing")?;
    if source.is_empty() {
        return Err("music_playlist_link");
    }
    let (_, tracks) = playlist(&source)?;
    edit(|library| {
        playlist_mut(library, id)?.tracks = tracks;
        Ok(())
    })
    .map(|(library, _)| library)
}

/// The length of a song learned while playing it, saved where it was unknown.
pub fn remember_duration(video: &str, seconds: u32) {
    if seconds == 0 || !valid_id(video) {
        return;
    }
    let _ = edit(|library| {
        let mut changed = false;
        for track in library.playlists.iter_mut().flat_map(|playlist| playlist.tracks.iter_mut()) {
            if track.id == video && track.duration == 0 {
                track.duration = seconds;
                changed = true;
            }
        }
        if changed {
            Ok(())
        } else {
            Err("unchanged")
        }
    });
}

// ---------------------------------------------------------------- links

#[derive(Debug, PartialEq, Eq)]
pub enum Link {
    Video(String),
    Playlist(String),
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_list(id: &str) -> bool {
    (12..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| pair.split_once('=').filter(|(key, _)| *key == name).map(|(_, value)| value))
}

/// Split a link into (host, path, query) without a URL crate.
fn split_url(input: &str) -> (String, String, String) {
    let rest = input.split_once("://").map_or(input, |(_, rest)| rest);
    let rest = rest.split('#').next().unwrap_or(rest);
    let (place, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = place.split_once('/').unwrap_or((place, ""));
    (host.to_ascii_lowercase(), path.to_owned(), query.to_owned())
}

/// Song first: a song opened from a playlist adds that song.
pub fn parse_link(input: &str) -> Option<Link> {
    parse(input, false)
}

/// Playlist first: for "Import a playlist".
fn parse_link_list_first(input: &str) -> Option<Link> {
    parse(input, true)
}

fn parse(input: &str, list_first: bool) -> Option<Link> {
    let input = input.trim().trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>');
    if input.is_empty() {
        return None;
    }
    if !input.contains('/') && !input.contains('.') {
        if valid_id(input) {
            return Some(Link::Video(input.to_owned()));
        }
        if valid_list(input) {
            return Some(Link::Playlist(input.to_owned()));
        }
        return None;
    }
    let (host, path, query) = split_url(input);
    let host = host.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    let first = segments.next().unwrap_or("");
    let video = if host == "youtu.be" {
        Some(first.to_owned())
    } else if host == "youtube.com" || host == "youtube-nocookie.com" {
        match first {
            "watch" => query_value(&query, "v").map(str::to_owned),
            "shorts" | "embed" | "live" | "v" => segments.next().map(str::to_owned),
            _ => None,
        }
    } else {
        return None;
    }
    .filter(|id| valid_id(id));
    // Mixes ("RD…") are made for one viewer on the fly and cannot be read as a list.
    let list = query_value(&query, "list").filter(|list| valid_list(list) && !list.starts_with("RD")).map(str::to_owned);
    match (video, list) {
        (Some(_), Some(list)) if list_first => Some(Link::Playlist(list)),
        (Some(video), _) => Some(Link::Video(video)),
        (None, Some(list)) => Some(Link::Playlist(list)),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------- YouTube

const VR_VERSION: &str = "1.65.10";

fn vr_agent() -> String {
    format!("com.google.android.apps.youtube.vr.oculus/{VR_VERSION} (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip")
}

fn vr_context() -> Value {
    json!({
        "client": {
            "clientName": "ANDROID_VR",
            "clientVersion": VR_VERSION,
            "deviceMake": "Oculus",
            "deviceModel": "Quest 3",
            "androidSdkVersion": 32,
            "osName": "Android",
            "osVersion": "12L",
            "hl": "en",
            "gl": "US"
        }
    })
}

static API: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(12))
        .user_agent(vr_agent())
        .build()
        .ok()
});

/// Cover pictures: a slow line may need a while for a 130 KB picture.
static FILES: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(12))
        .user_agent(vr_agent())
        .build()
        .ok()
});

fn innertube(endpoint: &str, mut body: Value) -> Result<Value, &'static str> {
    let client = API.as_ref().ok_or("network")?;
    body["context"] = vr_context();
    let response = client
        .post(format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"))
        .header("X-YouTube-Client-Name", "28")
        .header("X-YouTube-Client-Version", VR_VERSION)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .map_err(|_| "network")?;
    if !response.status().is_success() {
        return Err("music_unavailable");
    }
    let bytes = response.bytes().map_err(|_| "network")?;
    serde_json::from_slice(&bytes).map_err(|_| "music_unavailable")
}

/// Text of a YouTube text object (`simpleText` or `runs`).
fn text(value: &Value) -> String {
    if let Some(simple) = value.get("simpleText").and_then(Value::as_str) {
        return simple.to_owned();
    }
    value
        .get("runs")
        .and_then(Value::as_array)
        .map(|runs| runs.iter().filter_map(|run| run.get("text").and_then(Value::as_str)).collect())
        .unwrap_or_default()
}

/// Every value stored under `key`, anywhere in the answer.
fn collect<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    out.push(child);
                }
                collect(child, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|child| collect(child, key, out)),
        _ => {}
    }
}

/// "4:09" or "1:02:03" in seconds.
fn clock(text: &str) -> u32 {
    text.split(':').try_fold(0u32, |total, part| part.trim().parse::<u32>().ok().map(|value| total * 60 + value)).unwrap_or(0)
}

/// Title and channel of one song (YouTube's oEmbed, ~1 KB). The length is
/// learned when it plays.
pub fn song(video: &str) -> Result<Track, &'static str> {
    if !valid_id(video) {
        return Err("music_link_invalid");
    }
    let client = API.as_ref().ok_or("network")?;
    let response = client
        .get(format!("https://www.youtube.com/oembed?format=json&url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D{video}"))
        .send()
        .map_err(|_| "network")?;
    if !response.status().is_success() {
        // 401: embedding turned off by the owner; 404: private or removed.
        return Err(if response.status().as_u16() == 401 { "music_no_embed" } else { "music_unavailable" });
    }
    let answer: Value = serde_json::from_slice(&response.bytes().map_err(|_| "network")?).map_err(|_| "music_unavailable")?;
    Ok(Track {
        id: video.to_owned(),
        title: answer.get("title").and_then(Value::as_str).unwrap_or(video).to_owned(),
        artist: answer.get("author_name").and_then(Value::as_str).unwrap_or_default().trim_end_matches(" - Topic").to_owned(),
        duration: 0,
    })
}

/// Name and songs of a public or unlisted playlist (up to 2000 songs).
pub fn playlist(list: &str) -> Result<(String, Vec<Track>), &'static str> {
    if !valid_list(list) {
        return Err("music_playlist_link");
    }
    let mut answer = innertube("browse", json!({ "browseId": format!("VL{list}") }))?;
    let mut headers = Vec::new();
    collect(&answer, "playlistHeaderRenderer", &mut headers);
    let title = headers.first().map(|header| text(&header["title"])).unwrap_or_default();
    let mut tracks = Vec::new();
    for _ in 0..100 {
        let page = answer.get("continuationContents").unwrap_or(&answer);
        let mut items = Vec::new();
        collect(page, "playlistVideoRenderer", &mut items);
        for item in items {
            let Some(id) = item.get("videoId").and_then(Value::as_str).filter(|id| valid_id(id)) else { continue };
            // Deleted and private videos stay in a playlist without a length.
            if item.get("isPlayable").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            tracks.push(Track {
                id: id.to_owned(),
                title: text(&item["title"]),
                artist: text(&item["shortBylineText"]).trim_end_matches(" - Topic").to_owned(),
                duration: item
                    .get("lengthSeconds")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_else(|| clock(&text(&item["lengthText"]))),
            });
        }
        let mut next = Vec::new();
        collect(page, "nextContinuationData", &mut next);
        let Some(token) = next.first().and_then(|data| data.get("continuation")).and_then(Value::as_str).map(str::to_owned) else { break };
        if tracks.len() >= 2000 {
            break;
        }
        answer = innertube("browse", json!({ "continuation": token }))?;
    }
    if tracks.is_empty() {
        return Err("music_playlist_empty");
    }
    Ok((title, tracks))
}

/// Songs matching words typed by the player (videos only, no live streams).
pub fn search(query: &str) -> Result<Vec<Track>, &'static str> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(Link::Video(video)) = parse_link(query) {
        return song(&video).map(|track| vec![track]);
    }
    // params = the "Videos" filter.
    let answer = innertube("search", json!({ "query": query.chars().take(200).collect::<String>(), "params": "EgIQAQ%3D%3D" }))?;
    let mut items = Vec::new();
    collect(&answer, "compactVideoRenderer", &mut items);
    collect(&answer, "videoRenderer", &mut items);
    let mut tracks: Vec<Track> = Vec::new();
    for item in items {
        let Some(id) = item.get("videoId").and_then(Value::as_str).filter(|id| valid_id(id)) else { continue };
        let duration = clock(&text(&item["lengthText"]));
        if duration == 0 || tracks.iter().any(|track| track.id == id) {
            continue;
        }
        let mut artist = text(&item["longBylineText"]);
        if artist.is_empty() {
            artist = text(&item["shortBylineText"]);
        }
        tracks.push(Track { id: id.to_owned(), title: text(&item["title"]), artist: artist.trim_end_matches(" - Topic").to_owned(), duration });
    }
    Ok(tracks)
}

// ---------------------------------------------------------------- pictures

/// Cover picture of a song, kept on disk: "hi" = the largest YouTube has
/// (1280×720 when it exists), "lo" = 320×180 for lists (~10 KB).
pub fn thumb(video: &str, size: &str) -> Option<Vec<u8>> {
    if !valid_id(video) {
        return None;
    }
    let size = if size == "hi" { "hi" } else { "lo" };
    let path = cache_dir("thumbs")?.join(format!("{video}.{size}.jpg"));
    if let Ok(bytes) = std::fs::read(&path) {
        return Some(bytes);
    }
    let names: &[&str] = if size == "hi" { &["maxresdefault", "sddefault", "hqdefault"] } else { &["mqdefault"] };
    let client = FILES.as_ref()?;
    for name in names {
        let Ok(response) = client.get(format!("https://i.ytimg.com/vi/{video}/{name}.jpg")).send() else { continue };
        // A missing size answers 404 with a grey placeholder picture.
        if !response.status().is_success() {
            continue;
        }
        let Ok(bytes) = response.bytes() else { continue };
        if bytes.len() < 1500 {
            continue;
        }
        let _ = std::fs::write(&path, &bytes);
        return Some(bytes.to_vec());
    }
    None
}

// ---------------------------------------------------------------- cache

fn picture_files() -> Vec<(PathBuf, u64)> {
    let Some(directory) = cache_dir("thumbs") else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(directory) else { return Vec::new() };
    entries.flatten().filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.len()))).collect()
}

pub fn cache_info() -> CacheInfo {
    let files = picture_files();
    CacheInfo { bytes: files.iter().map(|(_, size)| size).sum(), pictures: files.len() as u32 }
}

/// Delete the saved cover pictures (they download again when shown).
pub fn clear_cache() -> CacheInfo {
    for (path, _) in picture_files() {
        let _ = std::fs::remove_file(path);
    }
    cache_info()
}
