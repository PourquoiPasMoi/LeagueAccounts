// Patch notes page: League's patch notes read from Riot's site (kept on disk
// once read), drawn with the app's own design, with a search across every
// patch on disk. A second tab lists the app's own changes.
import { ApiError, api, events, gameImage, type PatchBlock, type PatchEntry, type PatchHit, type PatchNode, type PatchNotes, type PatchProgress, type PatchSearch, type PatchSpan } from "./api";
import { appChanges } from "./changelog";
import { $, esc, fragment, leave, reducedMotion } from "./dom";
import { icon } from "./icons";
import { lang, localeTag, t, type Lang } from "./i18n";
import { openReport } from "./report";

/** Riot's site language for each language of the app (it has no Simplified Chinese). */
const LOCALES: Record<Lang, string> = { en: "en-us", tr: "tr-tr", de: "de-de", es: "es-es", fr: "fr-fr", pt: "pt-br", zh: "zh-tw", zhTw: "zh-tw" };
/// Rough size of one patch's page on the wire, for the download note.
const PAGE_MB = 0.07;
/// The newest patches are fetched without asking (small), so the search has something to read.
const AUTO_PATCHES = 6;

type Tab = "league" | "app";
/** What the search looks at. */
type Topic = "all" | "champion" | "item" | "other";
const TOPICS: Topic[] = ["all", "champion", "item", "other"];
const TOPIC_KEY = "pnTopic";

function savedTopic(): Topic {
  try {
    const saved = localStorage.getItem(TOPIC_KEY) as Topic | null;
    return saved && TOPICS.includes(saved) ? saved : "all";
  } catch {
    return "all";
  }
}

const dateText = (iso: string, long = true): string => {
  const parsed = new Date(iso.length <= 10 ? `${iso}T12:00:00` : iso);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleDateString(localeTag(), long ? { day: "numeric", month: "long", year: "numeric" } : { day: "numeric", month: "short" });
};

const yearOf = (iso: string): string => iso.slice(0, 4);

/** Text with the searched words marked. */
function marked(text: string, terms: RegExp | null): string {
  if (!terms) return esc(text);
  let out = "";
  let last = 0;
  for (const match of text.matchAll(terms)) {
    const at = match.index ?? 0;
    out += `${esc(text.slice(last, at))}<mark>${esc(match[0])}</mark>`;
    last = at + match[0].length;
  }
  return out + esc(text.slice(last));
}

function termsPattern(terms: string[]): RegExp | null {
  const words = terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).filter(Boolean);
  if (!words.length) return null;
  try {
    return new RegExp(`(?<![\\p{L}\\p{N}])(?:${words.join("|")})[\\p{L}\\p{N}]*`, "giu");
  } catch {
    return null;
  }
}

function spanHtml(span: PatchSpan, terms: RegExp | null, extra = ""): string {
  const classes = [span.b ? "pn-b" : "", span.i ? "pn-i" : "", span.href ? "pn-link" : "", extra].filter(Boolean).join(" ");
  const text = marked(span.t, terms).replace(/\n/g, "<br>");
  return classes ? `<span class="${classes}">${text}</span>` : text;
}

const ARROW = /\s*(?:⇒|→|=>)\s*/;

/** A change line: "Damage: 10 ⇒ 20" gets its old value dimmed and the new one bright. */
function changeHtml(spans: PatchSpan[], terms: RegExp | null): string {
  if (!spans.some((span) => ARROW.test(span.t))) return spans.map((span) => spanHtml(span, terms)).join("");
  let after = false;
  let out = "";
  for (const span of spans) {
    const parts = span.t.split(ARROW);
    parts.forEach((part, index) => {
      if (index > 0) {
        after = true;
        out += `<i class="pn-arrow" aria-hidden="true"></i>`;
      }
      if (!part) return;
      // Before the arrow: the bold part is the stat's name, the rest is the old value.
      const role = after ? "pn-new" : span.b ? "pn-label" : "pn-old";
      out += spanHtml({ ...span, t: part, b: false }, terms, role);
    });
  }
  return out;
}

const image = (path: string): string => gameImage(path);

function nodesHtml(nodes: PatchNode[], terms: RegExp | null): string {
  let out = "";
  let list = false;
  const close = () => {
    if (list) out += "</ul>";
    list = false;
  };
  for (const node of nodes) {
    if (node.k === "li") {
      if (!list) out += `<ul class="pn-changes">`;
      list = true;
      out += `<li style="--depth:${Math.min(node.depth, 3)}">${changeHtml(node.spans, terms)}</li>`;
      continue;
    }
    close();
    if (node.k === "h") out += `<h4 class="pn-h">${node.icon ? `<img src="${esc(image(node.icon))}" alt="" loading="lazy" draggable="false"/>` : ""}<span>${node.spans.map((span) => spanHtml({ ...span, b: false }, terms)).join("")}</span></h4>`;
    else if (node.k === "p") out += `<p>${node.spans.map((span) => spanHtml(span, terms)).join("")}</p>`;
    else if (node.k === "q") out += `<blockquote class="pn-q">${node.spans.map((span) => spanHtml(span, terms)).join("")}</blockquote>`;
    else if (node.k === "img") out += `<img class="${node.small ? "pn-img-small" : "pn-img"}" src="${esc(image(node.src))}" alt="" loading="lazy" draggable="false"/>`;
    else out += `<hr/>`;
  }
  close();
  return out;
}

function blockHtml(block: PatchBlock, terms: RegExp | null, id = "", meta = ""): string {
  const head = block.title || block.icon ? `<header class="pn-card-head">${block.icon ? `<img src="${esc(image(block.icon))}" alt="" loading="lazy" draggable="false"/>` : ""}${block.title ? `<h3>${marked(block.title, terms)}</h3>` : ""}</header>` : "";
  return `<article class="pn-card" data-reveal${id ? ` id="${id}"` : ""}>${meta}${head}<div class="pn-card-body">${nodesHtml(block.nodes, terms)}</div></article>`;
}

/// Words in a section's name (any of the app's languages) and the icon that stands for it.
const SECTION_ICONS: [RegExp, string][] = [
  [/aram/, "snowflake"],
  [/arena|arène|竞技场|競技場/, "trophy"],
  [/skin|chroma|aspect|kostüm|造型|皮肤|炫彩/, "shirt"],
  [/bug|hata|fehler|error|erro|修复|修正|錯誤/, "bug"],
  [/highlight|öne çıkan|höhepunkt|destac|temps forts|亮点|焦點/, "sparkles"],
  [/voice|chat|ses|sprach|voz|vocal|语音|語音/, "mic"],
  [/map|harita|karte|mapa|carte|地图|地圖/, "map"],
  [/music|müzik|musik|música|musique|音乐|音樂/, "music"],
  [/replay|download|indir|descarg|télécharg|下载|下載/, "download"],
  [/champion|şampiyon|campe|英雄/, "swords"],
  [/item|eşya|gegenst|objet|iten|装备|裝備/, "gem"],
  [/classic|ranked|rift|klasik|klassisch|clásic|classique|clássic|排位|經典|经典/, "gamepad"],
  [/system|sistem|système|系统|系統/, "layers"],
];

/** The icon of a section's pill: by what its cards are about, else by its name, else its first letter. */
function sectionMark(section: PatchNotes["sections"][number]): string {
  const kinds = section.blocks.map((block) => block.kind ?? "");
  const most = (kind: string) => kinds.filter((value) => value === kind).length * 2 > kinds.length;
  const name = section.title.toLowerCase();
  const byName = SECTION_ICONS.find(([words]) => words.test(name))?.[1];
  const chosen = byName ?? (most("champion") ? "swords" : most("item") ? "gem" : "");
  if (chosen) return icon(chosen, 15);
  const letter = [...section.title.trim()][0]?.toUpperCase() ?? "";
  return `<span class="pn-chip-letter" aria-hidden="true">${esc(letter)}</span>`;
}

function notesHtml(notes: PatchNotes): string {
  const named = notes.sections.filter((section) => section.title);
  const chips = named.length > 1 ? `<nav class="pn-chips">${notes.sections.map((section, index) => (section.title ? `<button type="button" class="pn-chip" data-pn-section="${index}" data-tooltip="${esc(section.title)}" aria-label="${esc(section.title)}">${sectionMark(section)}<span class="pn-chip-name">${esc(section.title)}</span></button>` : "")).join("")}</nav>` : "";
  const sections = notes.sections
    .map(
      (section, index) => `
      <section class="pn-section" id="pn-s-${index}" data-pn-anchor="${index}">
        ${section.title ? `<h2 data-reveal>${esc(section.title)}</h2>` : ""}
        <div class="pn-cards">${section.blocks.map((block, at) => blockHtml(block, null, `pn-b-${index}-${at}`)).join("")}</div>
      </section>`,
    )
    .join("");
  return `
    <header class="pn-hero${notes.image ? " has-image" : ""}">
      ${notes.image ? `<img class="pn-hero-image" src="${esc(image(notes.image))}" alt="" draggable="false"/>` : ""}
      <div class="pn-hero-text">
        <p class="pn-hero-date">${esc(dateText(notes.date))}</p>
        <h1>${esc(notes.title)}</h1>
        ${notes.summary ? `<p class="pn-hero-summary">${esc(notes.summary)}</p>` : ""}
        <div class="pn-hero-actions">
          <button type="button" class="btn btn-primary btn-small" data-pn="report">${icon("download", 12)}<span>${esc(t("pn.createImage"))}</span></button>
          <button type="button" class="btn btn-ghost btn-small" data-pn="site">${icon("external-link", 12)}<span>${esc(t("pn.openSite"))}</span></button>
        </div>
      </div>
    </header>
    ${chips}${sections}`;
}

const skeleton = (): string => `
  <div class="pn-skeleton" aria-busy="true" aria-label="${esc(t("pn.loading"))}">
    <div class="pn-skel pn-skel-hero"></div>
    ${[0, 1, 2, 3].map((index) => `<div class="pn-skel pn-skel-card" style="--k:${index}"></div>`).join("")}
  </div>`;

const message = (title: string, text = "", retry = false): string => `
  <div class="pn-empty">
    <h3>${esc(title)}</h3>
    ${text ? `<p>${esc(text)}</p>` : ""}
    ${retry ? `<button type="button" class="btn btn-ghost btn-small" data-pn="retry">${esc(t("pn.retry"))}</button>` : ""}
  </div>`;

let opened = false;

export async function openPatchNotes(appVersion: string): Promise<void> {
  if (opened) return;
  opened = true;
  const locale = LOCALES[lang()] ?? "en-us";
  const own = appChanges(appVersion);
  let tab: Tab = "league";
  let entries: PatchEntry[] = [];
  let offline = false;
  let failed = false;
  let selected = "";
  let query = "";
  let topic: Topic = savedTopic();
  let progress: PatchProgress | null = null;
  /// Each load/search has a number: a slower, older answer is dropped.
  let turn = 0;
  const known = new Map<string, PatchNotes>();
  /// The results on screen, for an image of them.
  let found: { view: PatchSearch; asked: string } | null = null;

  const overlay = fragment(`
    <div class="overlay pn-overlay">
      <div class="modal pn-modal" role="dialog" aria-modal="true" aria-label="${esc(t("pn.title"))}">
        <div class="modal-close-mount">
          <button class="modal-close" data-pn="close" aria-label="${esc(t("common.close"))}"><span></span></button>
        </div>
        <header class="pn-head">
          <h2>${esc(t("pn.title"))}</h2>
          <div class="segmented pn-tabs">
            <button type="button" data-pn-tab="league" aria-pressed="true">${esc(t("pn.tabLeague"))}</button>
            <button type="button" data-pn-tab="app" aria-pressed="false">${esc(t("pn.tabApp"))}</button>
          </div>
          <label class="pn-search">${icon("search", 14)}<input type="search" data-pn-search placeholder="${esc(t("pn.search"))}" aria-label="${esc(t("pn.search"))}" autocomplete="off" spellcheck="false"/><kbd>/</kbd></label>
        </header>
        <div class="pn-layout">
          <nav class="pn-list" aria-label="${esc(t("pn.title"))}">
            <div class="pn-list-scroll"></div>
            <footer class="pn-coverage"></footer>
          </nav>
          <section class="pn-reader"><div class="pn-reader-inner"></div></section>
        </div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  $("#modal-root").append(overlay);
  const list = $(".pn-list-scroll", overlay);
  const coverage = $(".pn-coverage", overlay);
  const reader = $(".pn-reader", overlay);
  const inner = $(".pn-reader-inner", overlay);
  const field = $<HTMLInputElement>("[data-pn-search]", overlay);

  // A picture that cannot load is left out rather than shown broken.
  overlay.addEventListener("error", (event) => (event.target as HTMLElement).tagName === "IMG" && (event.target as HTMLElement).remove(), true);

  // Cards rise into place as they scroll into view.
  const reveal = new IntersectionObserver(
    (seen) => {
      for (const entry of seen) {
        if (!entry.isIntersecting) continue;
        entry.target.classList.add("is-in");
        reveal.unobserve(entry.target);
      }
    },
    { root: reader, rootMargin: "0px 0px -6% 0px" },
  );
  const watch = () => {
    let order = 0;
    inner.querySelectorAll<HTMLElement>("[data-reveal]:not(.is-in)").forEach((element) => {
      // The first ones on screen come in one after the other.
      element.style.setProperty("--d", `${Math.min(order++, 7) * 45}ms`);
      reveal.observe(element);
    });
  };

  /** Swap what the reader shows, with a short cross-fade. */
  const show = async (html: string, mine: number, keepScroll = false) => {
    if (!reducedMotion() && inner.childElementCount) {
      inner.classList.add("is-out");
      await new Promise((resolve) => setTimeout(resolve, 110));
      if (mine !== turn) return;
    }
    inner.innerHTML = html;
    inner.classList.remove("is-out");
    if (!keepScroll) reader.scrollTop = 0;
    watch();
    followChips();
  };

  const rows = (): string => {
    if (tab === "app") {
      return own.map((entry, index) => `<button type="button" class="pn-row" data-pn-version="${esc(entry.version)}" style="--k:${Math.min(index, 14)}"><b>${esc(entry.version)}</b><span>${esc(dateText(entry.date, false))}</span></button>`).join("");
    }
    let year = "";
    return entries
      .map((entry, index) => {
        const head = yearOf(entry.date) !== year ? `<h4 class="pn-year">${esc((year = yearOf(entry.date)))}</h4>` : "";
        return `${head}<button type="button" class="pn-row${entry.slug === selected && !query ? " is-current" : ""}" data-pn-slug="${esc(entry.slug)}" style="--k:${Math.min(index, 14)}" data-tooltip="${esc(entry.summary)}"><b>${esc(entry.version || entry.title)}</b><span>${esc(dateText(entry.date, false))}</span></button>`;
      })
      .join("");
  };

  const renderList = () => {
    list.innerHTML = rows();
  };

  const markCurrent = () => {
    list.querySelectorAll<HTMLElement>(".pn-row").forEach((row) => row.classList.toggle("is-current", !query && row.dataset.pnSlug === selected));
    list.querySelector<HTMLElement>(".pn-row.is-current")?.scrollIntoView({ block: "nearest" });
  };

  const renderCoverage = () => {
    if (tab === "app" || !entries.length) {
      coverage.innerHTML = "";
      return;
    }
    const have = entries.filter((entry) => entry.cached).length;
    const missing = entries.length - have;
    const note = `<p>${esc(t(offline ? "pn.offline" : "pn.coverage", { have, total: entries.length }))}</p>`;
    if (progress?.running && progress.total > 0) {
      coverage.innerHTML = `<p>${esc(t("pn.downloading", { done: progress.done, total: progress.total }))}</p><div class="pn-progress"><span style="width:${Math.round((progress.done / progress.total) * 100)}%"></span></div><button type="button" class="btn btn-ghost btn-small" data-pn="cancel">${esc(t("common.cancel"))}</button>`;
    } else if (missing > 0 && !offline) {
      coverage.innerHTML = `${note}<button type="button" class="btn btn-ghost btn-small" data-pn="download">${icon("download", 12)}<span>${esc(t("pn.downloadAll", { size: Math.max(1, Math.round(missing * PAGE_MB)) }))}</span></button>`;
    } else {
      coverage.innerHTML = note;
    }
  };

  // -------------------------------------------------------------- the reader

  /// A clicked pill stays the open one while the page scrolls to its section.
  let held: { section: string; until: number } | null = null;

  /** The pill of the section on screen is the open one (icon and name); the others are icons. */
  const followChips = () => {
    const chips = [...inner.querySelectorAll<HTMLElement>("[data-pn-section]")];
    if (!chips.length) return;
    const top = reader.getBoundingClientRect().top + 96;
    let current = chips[0].dataset.pnSection;
    for (const section of inner.querySelectorAll<HTMLElement>("[data-pn-anchor]")) {
      if (section.getBoundingClientRect().top <= top && inner.querySelector(`[data-pn-section="${section.dataset.pnAnchor}"]`)) current = section.dataset.pnAnchor;
    }
    if (held && performance.now() < held.until) current = held.section;
    else held = null;
    for (const chip of chips) {
      const on = chip.dataset.pnSection === current;
      // The row of pills follows sideways only (scrollIntoView would also move the page and stop a smooth scroll).
      if (on && !chip.classList.contains("is-on")) {
        const row = chip.parentElement!;
        const left = chip.offsetLeft - row.offsetLeft;
        if (left < row.scrollLeft + 30 || left + chip.offsetWidth > row.scrollLeft + row.clientWidth - 30) row.scrollTo({ left: Math.max(0, left - row.clientWidth / 2 + chip.offsetWidth / 2), behavior: reducedMotion() ? "auto" : "smooth" });
      }
      chip.classList.toggle("is-on", on);
    }
  };
  let ticking = false;
  reader.addEventListener("scroll", () => {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(() => {
      ticking = false;
      followChips();
      reader.classList.toggle("is-scrolled", reader.scrollTop > 8);
    });
  });

  const errorHtml = (error: unknown): string => (error instanceof ApiError && error.code === "patch_notes_offline" ? message(t("pn.offlineTitle"), t("pn.offlineText"), true) : message(t("pn.failed"), "", true));

  const openPatch = async (slug: string, at = "") => {
    const mine = ++turn;
    selected = slug;
    if (query) {
      query = "";
      field.value = "";
      overlay.classList.remove("is-searching");
    }
    markCurrent();
    let notes = known.get(slug);
    if (!notes) {
      await show(skeleton(), mine);
      try {
        notes = await api.patchNotes(locale, slug);
      } catch (error) {
        if (mine === turn) await show(errorHtml(error), mine);
        return;
      }
      known.set(slug, notes);
      const entry = entries.find((item) => item.slug === slug);
      if (entry && !entry.cached) {
        entry.cached = true;
        renderCoverage();
      }
    }
    if (mine !== turn) return;
    await show(notesHtml(notes), mine);
    if (mine !== turn || !at) return;
    const target = inner.querySelector<HTMLElement>(`#pn-b-${at}`);
    if (!target) return;
    target.classList.add("is-in");
    target.scrollIntoView({ block: "center", behavior: reducedMotion() ? "auto" : "smooth" });
    target.classList.add("is-found");
    setTimeout(() => target.classList.remove("is-found"), 2200);
  };

  // -------------------------------------------------------------- search

  const hitHtml = (hit: PatchHit, terms: RegExp | null): string => {
    const meta = `<button type="button" class="pn-hit-meta" data-pn-open="${esc(hit.slug)}" data-pn-at="${hit.sectionIndex}-${hit.blockIndex}"><b>${esc(hit.version || hit.slug)}</b><span>${esc(dateText(hit.date))}</span>${hit.section ? `<em>${esc(hit.section)}</em>` : ""}</button>`;
    return blockHtml(hit.block, terms, "", meta);
  };

  const topicsHtml = (): string => `
    <div class="pn-topics" role="group" aria-label="${esc(t("pn.topics"))}">
      ${TOPICS.map((name) => `<button type="button" data-pn-topic="${name}" aria-pressed="${name === topic}">${esc(t(`pn.topic.${name}`))}</button>`).join("")}
    </div>`;

  const searchHtml = (view: PatchSearch, asked: string): string => {
    const terms = termsPattern(view.terms);
    const versions = view.versions
      .map((slug) => entries.find((entry) => entry.slug === slug))
      .filter((entry): entry is PatchEntry => Boolean(entry))
      .map((entry) => `<button type="button" class="pn-jump" data-pn-open="${esc(entry.slug)}" data-reveal><b>${esc(t("pn.openPatch", { version: entry.version }))}</b><span>${esc(dateText(entry.date))}</span><p>${esc(entry.summary)}</p></button>`)
      .join("");
    const named = topic === "champion" || topic === "item";
    const tip = topic === "champion" ? `<p class="pn-tip">${esc(t("pn.abilityTip"))}</p>` : "";
    if (!view.hits.length && !versions) {
      return `<header class="pn-results-head" data-reveal>${topicsHtml()}${tip}</header>${message(t("pn.noResults", { query: asked }), t(topic === "all" ? "pn.noResultsHint" : "pn.noResultsTopic", { have: view.patches, total: entries.length }))}`;
    }
    const count = view.total > view.hits.length ? t("pn.resultsMore", { shown: view.hits.length, count: view.total }) : t(view.total === 1 ? "pn.result" : "pn.results", { count: view.total });
    // A champion's or item's history is newest first: the first card is its last change.
    const newest = named ? view.hits[0] : undefined;
    const last = newest ? `<p class="pn-last"><b>${esc(newest.block.title)}</b><span>${esc(t("pn.lastChanged", { version: newest.version, date: dateText(newest.date), count: view.patchCount }))}</span></p>` : "";
    return `
      <header class="pn-results-head" data-reveal>
        ${topicsHtml()}
        ${view.hits.length ? `<div class="pn-results-title"><h2>${esc(count)}</h2><button type="button" class="btn btn-ghost btn-small" data-pn="report-search">${icon("download", 12)}<span>${esc(t("pn.createImage"))}</span></button></div>` : ""}
        ${view.corrected ? `<p>${esc(t("pn.corrected", { query: view.corrected }))}</p>` : ""}
        ${last}${tip}
      </header>
      <div class="pn-cards">${versions}${view.hits.map((hit) => hitHtml(hit, terms)).join("")}</div>`;
  };

  const appHtml = (): string => {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const terms = termsPattern(words);
    const shown = own
      .map((entry) => ({ ...entry, items: words.length ? entry.items.filter((item) => words.every((word) => `${entry.version} ${item}`.toLowerCase().includes(word))) : entry.items }))
      .filter((entry) => entry.items.length);
    if (!shown.length) return message(t("pn.noResults", { query }));
    return `
      <header class="pn-results-head" data-reveal><h2>${esc(t("pn.tabApp"))}</h2><p>${esc(t("pn.appIntro"))}</p></header>
      <div class="pn-cards">${shown
        .map(
          (entry) => `
          <article class="pn-card pn-own" data-reveal id="pn-v-${esc(entry.version.replace(/\./g, "-"))}">
            <header class="pn-card-head"><h3>${esc(entry.version)}</h3><span>${esc(dateText(entry.date))}</span></header>
            <div class="pn-card-body"><ul class="pn-changes">${entry.items.map((item) => `<li>${marked(item, terms)}</li>`).join("")}</ul></div>
          </article>`,
        )
        .join("")}</div>`;
  };

  const runSearch = async () => {
    const mine = ++turn;
    overlay.classList.toggle("is-searching", Boolean(query));
    if (tab === "app") return show(appHtml(), mine);
    markCurrent();
    if (!query) return selected ? openPatch(selected) : undefined;
    try {
      const asked = query;
      const view = await api.patchNotesSearch(locale, asked, topic);
      if (mine === turn) {
        found = { view, asked };
        await show(searchHtml(view, asked), mine);
      }
    } catch (error) {
      if (mine === turn) await show(errorHtml(error), mine);
    }
  };

  let typing = 0;
  field.addEventListener("input", () => {
    query = field.value.trim();
    window.clearTimeout(typing);
    typing = window.setTimeout(() => void runSearch(), query ? 170 : 0);
  });

  // -------------------------------------------------------------- loading

  const loadList = async (refresh = false) => {
    const mine = ++turn;
    failed = false;
    if (!entries.length) await show(skeleton(), mine);
    try {
      const view = await api.patchNotesList(locale, refresh);
      entries = view.entries;
      offline = view.offline;
    } catch (error) {
      failed = true;
      if (mine === turn && tab === "league") await show(errorHtml(error), mine);
      return;
    }
    if (tab !== "league") return;
    renderList();
    renderCoverage();
    if (mine !== turn) return;
    if (!selected) selected = entries[0]?.slug ?? "";
    if (selected) await openPatch(selected);
    // The newest patches, quietly: they are small and make the search useful at once.
    if (!offline && entries.slice(0, AUTO_PATCHES).some((entry) => !entry.cached)) void api.patchNotesDownload(locale, AUTO_PATCHES).catch(() => false);
  };

  const refreshCached = async () => {
    try {
      const view = await api.patchNotesList(locale, false);
      entries = view.entries;
      offline = view.offline;
    } catch {
      // The list on screen stays.
    }
    renderCoverage();
  };

  const unlisten = events.onPatchNotesProgress((update) => {
    progress = update;
    if (!update.running) {
      progress = null;
      void refreshCached().then(() => (query && tab === "league" ? runSearch() : undefined));
    } else {
      renderCoverage();
    }
  });

  const setTab = (next: Tab) => {
    if (tab === next) return;
    tab = next;
    overlay.querySelectorAll<HTMLElement>("[data-pn-tab]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.pnTab === tab)));
    overlay.classList.toggle("is-app", tab === "app");
    field.value = "";
    query = "";
    overlay.classList.remove("is-searching");
    field.placeholder = t(tab === "app" ? "pn.searchApp" : "pn.search");
    renderList();
    renderCoverage();
    if (tab === "app") {
      void show(appHtml(), ++turn);
    } else if (failed || !entries.length) {
      void loadList();
    } else if (selected) {
      void openPatch(selected);
    }
  };

  // -------------------------------------------------------------- input

  const close = () => {
    if (!opened) return;
    opened = false;
    turn += 1;
    window.clearTimeout(typing);
    reveal.disconnect();
    document.removeEventListener("keydown", onKey, true);
    void unlisten.then((stop) => stop());
    void leave(overlay, "overlay-out");
    previousFocus?.focus?.();
  };

  const onKey = (event: KeyboardEvent) => {
    if ([...document.querySelectorAll("#modal-root > .overlay:not(.overlay-out)")].at(-1) !== overlay) return;
    const typingNow = (event.target as HTMLElement).matches?.("input, textarea, select");
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (field.value) {
        field.value = "";
        query = "";
        void runSearch();
        return;
      }
      close();
    } else if ((event.key === "/" && !typingNow) || (event.key.toLowerCase() === "f" && event.ctrlKey)) {
      event.preventDefault();
      field.focus();
      field.select();
    }
  };
  document.addEventListener("keydown", onKey, true);

  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) close();
  });

  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const action = target.closest<HTMLElement>("[data-pn]")?.dataset.pn;
    if (action === "close") return close();
    if (action === "retry") return void loadList(true);
    if (action === "site") return void api.openPatchNotes(locale, selected).catch(() => undefined);
    if (action === "report") {
      const notes = known.get(selected);
      if (notes) openReport({ title: notes.title, subtitle: [dateText(notes.date), notes.summary].filter(Boolean).join(" · "), sections: notes.sections });
      return;
    }
    if (action === "report-search" && found) {
      const { view, asked } = found;
      const named = (topic === "champion" || topic === "item") && view.hits[0];
      openReport({
        title: named ? view.hits[0].block.title : t("report.searchTitle", { query: view.corrected || asked }),
        subtitle: named ? t("pn.lastChanged", { version: view.hits[0].version, date: dateText(view.hits[0].date), count: view.patchCount }) : t(view.total === 1 ? "pn.result" : "pn.results", { count: view.total }),
        // One card per result, named by its patch.
        sections: [{ title: "", blocks: view.hits.map((hit) => ({ ...hit.block, title: [hit.block.title || hit.section, hit.version].filter(Boolean).join(" · ") })) }],
        whole: true,
      });
      return;
    }
    if (action === "cancel") return void api.patchNotesCancel().catch(() => undefined);
    if (action === "download") {
      progress = { done: 0, total: entries.filter((entry) => !entry.cached).length, running: true };
      renderCoverage();
      return void api.patchNotesDownload(locale, 0).catch(() => false);
    }
    const topicButton = target.closest<HTMLElement>("[data-pn-topic]");
    if (topicButton) {
      topic = topicButton.dataset.pnTopic as Topic;
      try {
        localStorage.setItem(TOPIC_KEY, topic);
      } catch {
        // Not kept: the next search starts on "All".
      }
      return void runSearch();
    }
    const tabButton = target.closest<HTMLElement>("[data-pn-tab]");
    if (tabButton) return setTab(tabButton.dataset.pnTab as Tab);
    const row = target.closest<HTMLElement>("[data-pn-slug]");
    if (row) return void openPatch(row.dataset.pnSlug!);
    const version = target.closest<HTMLElement>("[data-pn-version]");
    if (version) {
      const card = inner.querySelector<HTMLElement>(`#pn-v-${version.dataset.pnVersion!.replace(/\./g, "-")}`);
      card?.classList.add("is-in");
      card?.scrollIntoView({ block: "start", behavior: reducedMotion() ? "auto" : "smooth" });
      return;
    }
    const jump = target.closest<HTMLElement>("[data-pn-open]");
    if (jump) return void openPatch(jump.dataset.pnOpen!, jump.dataset.pnAt ?? "");
    const chip = target.closest<HTMLElement>("[data-pn-section]");
    if (chip) {
      const section = inner.querySelector<HTMLElement>(`#pn-s-${chip.dataset.pnSection}`);
      if (section) {
        held = { section: chip.dataset.pnSection!, until: performance.now() + 900 };
        followChips();
        reader.scrollTo({ top: section.offsetTop - 64, behavior: reducedMotion() ? "auto" : "smooth" });
        // Once the scroll has settled the pills follow the page again.
        window.setTimeout(followChips, 950);
      }
    }
  });

  await loadList();
}
