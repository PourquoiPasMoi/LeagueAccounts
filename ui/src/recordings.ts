// Recordings page: full games and clips per account, a player whose timeline
// shows the key moments (click one to jump there), and a start/end range that
// is saved as a clip in the account's Clips folder.
import { ApiError, api, events, type Moment, type RecorderStatus, type Recording, type RecordingsView } from "./api";
import { errorText, localeTag, t, type MessageKey } from "./i18n";
import { $, esc, fragment, leave, modal, toast } from "./dom";
import { icon } from "./icons";
import { momentIcon } from "./momenticons";
import { renderGameInfo, type GameDetails } from "./gameinfo";

const UNKNOWN = "Unknown";
const OBJECTIVES = new Set(["dragon", "baron", "herald", "grubs", "tower", "inhibitor", "ace"]);

type ClipMode = "fast" | "exact" | "discord";
/** Moment filters of the viewer (side list and timeline). */
type Filter = "all" | "kills" | "deaths" | "assists" | "objectives";
const FILTERS: [Filter, MessageKey][] = [
  ["all", "rec.filterAll"],
  ["kills", "rec.filterKills"],
  ["deaths", "rec.filterDeaths"],
  ["assists", "rec.filterAssists"],
  ["objectives", "rec.filterObjectives"],
];
const KILLS = new Set(["kill", "multikill", "firstBlood"]);
function matches(moment: Moment, filter: Filter): boolean {
  if (filter === "kills") return KILLS.has(moment.kind);
  if (filter === "deaths") return moment.kind === "death";
  if (filter === "assists") return moment.kind === "assist";
  if (filter === "objectives") return OBJECTIVES.has(moment.kind);
  return true;
}
/** Kills / deaths / assists from the player's moments ("" without moments). */
function kda(entry: Recording): string {
  const moments = cleanMoments(entry.meta?.moments ?? []);
  if (!moments.length) return "";
  const count = (test: (moment: Moment) => boolean) => moments.filter(test).length;
  return `${count((moment) => KILLS.has(moment.kind))} / ${count((moment) => moment.kind === "death")} / ${count((moment) => moment.kind === "assist")}`;
}
const MODE_HINTS: Record<ClipMode, MessageKey> = { fast: "rec.fastHint", exact: "rec.exactHint", discord: "rec.discordHint" };
/** Seconds shown before a moment when jumping to it. */
const LEAD = 3;

let open = false;

/** Player volume per sound source and for everything, in percent (kept between sessions). */
const LEVELS_KEY = "recSoundLevels";
const MAX_LEVEL = 300;
const MAX_MASTER = 200;
let levels: Record<string, number> = (() => {
  try {
    const value = JSON.parse(localStorage.getItem(LEVELS_KEY) ?? "{}");
    return value && typeof value === "object" ? value : {};
  } catch {
    return {};
  }
})();
function level(name: string): number {
  const value = Number(levels[name]);
  return Number.isFinite(value) ? Math.min(name === "master" ? MAX_MASTER : MAX_LEVEL, Math.max(0, value)) : 100;
}
function setLevel(name: string, value: number): void {
  levels = { ...levels, [name]: value };
  try {
    localStorage.setItem(LEVELS_KEY, JSON.stringify(levels));
  } catch {
    // Not kept: fine.
  }
}

const describe = (error: unknown) => (error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error)));

export function clock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds || 0));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

function bytes(value: number): string {
  const format = (amount: number, unit: string) => `${new Intl.NumberFormat(localeTag(), { maximumFractionDigits: amount < 10 ? 1 : 0 }).format(amount)} ${unit}`;
  if (value >= 1e9) return format(value / 1e9, "GB");
  if (value >= 1e6) return format(value / 1e6, "MB");
  return format(value / 1e3, "KB");
}

const accountLabel = (account: string) => (account === UNKNOWN ? t("rec.unknown") : account);

const MOMENT_KEYS: Record<string, MessageKey> = {
  kill: "moment.kill",
  death: "moment.death",
  assist: "moment.assist",
  firstBlood: "moment.firstBlood",
  dragon: "moment.dragon",
  baron: "moment.baron",
  herald: "moment.herald",
  grubs: "moment.grubs",
  tower: "moment.tower",
  inhibitor: "moment.inhibitor",
  ace: "moment.ace",
  gameEnd: "moment.gameEnd",
};

/** A first blood or multikill is the same moment as the kill it comes with:
 * keep one marker (recordings made before this was merged have both). */
export function cleanMoments(all: Moment[]): Moment[] {
  // Only the player's own moments: older recordings also hold both teams' objectives.
  const moments = all.filter((moment) => !OBJECTIVES.has(moment.kind) || moment.mine === true);
  const special = moments.filter((moment) => moment.kind === "firstBlood" || moment.kind === "multikill");
  const merged = moments.filter((moment) => moment.kind !== "kill" || !special.some((other) => Math.abs(other.gameTime - moment.gameTime) < 1.5));
  // A first blood that is also a multikill (rare) shows as first blood.
  return merged.filter((moment) => moment.kind !== "multikill" || !merged.some((other) => other.kind === "firstBlood" && Math.abs(other.gameTime - moment.gameTime) < 1.5));
}

function momentLabel(moment: Moment): string {
  if (moment.kind === "multikill") {
    const size = Math.min(5, Math.max(2, Number(moment.detail) || 2));
    return t(`moment.multi${size}` as MessageKey);
  }
  if (moment.kind === "gameEnd") return moment.detail === "Win" ? t("rec.win") : moment.detail === "Loss" ? t("rec.loss") : t("moment.gameEnd");
  return t(MOMENT_KEYS[moment.kind] ?? "moment.gameEnd");
}

/** Colour family of a marker. */
function momentTone(moment: Moment): string {
  if (moment.kind === "death") return "bad";
  if (moment.kind === "gameEnd") return moment.detail === "Win" ? "good" : "bad";
  if (moment.ally === false) return "bad";
  if (["kill", "multikill", "firstBlood"].includes(moment.kind)) return "mine";
  if (moment.kind === "assist") return "assist";
  return "good";
}

function momentTooltip(moment: Moment): string {
  const parts = [momentLabel(moment), clock(moment.gameTime)];
  if (moment.ally === true) parts.push(t("rec.allyTeam"));
  if (moment.ally === false) parts.push(t("rec.enemyTeam"));
  if (moment.detail && !["multikill", "gameEnd"].includes(moment.kind)) parts.push(moment.detail);
  return parts.join(" · ");
}

function title(entry: Recording): string {
  const meta = entry.meta;
  // Clips are named by the player: show that name.
  if (entry.kind === "clip" || !meta || !meta.champion) return entry.name;
  return [meta.champion, meta.queue].filter(Boolean).join(" · ");
}

export async function openRecordings(): Promise<void> {
  if (open) return;
  open = true;
  let view: RecordingsView = { root: "", totalBytes: 0, limitBytes: 0, entries: [] };
  let status: RecorderStatus = { enabled: false, recording: false, account: "", startedAt: 0, path: "", error: "" };
  let account = "";
  let kind: "game" | "clip" = "game";
  let selected: Recording | null = null;
  let range: { start: number; end: number } | null = null;
  /** Parts added for a multi-part clip, in time order, never overlapping. */
  let parts: { start: number; end: number }[] = [];
  /** Timeline zoom: 1 = the whole recording; the view starts at `viewStart` seconds. */
  let zoom = 1;
  let viewStart = 0;
  let filter: Filter = "all";
  /** fast: stream copy · exact: re-encoded · discord: re-encoded under 9.5 MB. */
  let mode: ClipMode = "fast";
  let saving = false;
  /** Sound: everything goes through Web Audio so levels can pass 100 %. The
   * video plays the recording's mixed track until each source is ready as
   * its own file; then the video is silent and the sources play next to it. */
  let audio: AudioContext | null = null;
  let masterGain: GainNode | null = null;
  let videoSound: { source: MediaElementAudioSourceNode; gain: GainNode } | null = null;
  let tracks: { label: string; element: HTMLAudioElement; source: MediaElementAudioSourceNode; gain: GainNode }[] = [];
  let trackState: "none" | "loading" | "ready" | "failed" = "none";
  let mixerOpen = false;

  const overlay = fragment(`
    <div class="overlay rec-overlay">
      <div class="modal rec-modal" role="dialog" aria-modal="true" aria-label="${esc(t("rec.title"))}">
        <header class="modal-head"><h2>${esc(t("rec.title"))}</h2></header>
        <div class="modal-close-mount">
          <button class="modal-close" data-rec="close" aria-label="${esc(t("common.close"))}"><span></span></button>
        </div>
        <div class="rec-head"></div>
        <div class="rec-layout">
          <nav class="rec-nav" aria-label="${esc(t("rec.accounts"))}"></nav>
          <section class="rec-list"></section>
        </div>
      </div>
      <div class="rec-viewer" hidden role="dialog" aria-modal="true">
        <header class="rec-viewer-head"></header>
        <div class="rec-viewer-scroll">
          <div class="rec-viewer-body">
            <section class="rec-player"></section>
            <aside class="rec-side"></aside>
          </div>
          <section class="rec-game" hidden></section>
        </div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  $("#modal-root").append(overlay);
  const head = $(".rec-head", overlay);
  const nav = $(".rec-nav", overlay);
  const list = $(".rec-list", overlay);
  const viewer = $(".rec-viewer", overlay);
  const viewerHead = $(".rec-viewer-head", overlay);
  const player = $(".rec-player", overlay);
  const side = $(".rec-side", overlay);
  const gamePanel = $(".rec-game", overlay);
  const scroller = $(".rec-viewer-scroll", overlay);
  /** End-of-game details of the open game: shown under the player unless hidden. */
  let gameDetails: GameDetails | null = null;
  let gameState: "none" | "loading" | "ready" | "error" = "none";
  let gameError = "";
  const gameShown = () => {
    try {
      return localStorage.getItem("recGameInfo") !== "hidden";
    } catch {
      return true;
    }
  };
  const setGameShown = (shown: boolean) => {
    try {
      localStorage.setItem("recGameInfo", shown ? "shown" : "hidden");
    } catch {
      // Not kept: fine.
    }
  };

  /** The moments shown: the player's own, narrowed by the viewer's filter. */
  const currentMoments = () => cleanMoments(selected?.meta?.moments ?? []).filter((moment) => matches(moment, filter));
  const isRecording = (entry: Recording) => status.recording && status.path === entry.path;
  const accounts = () => [...new Set(view.entries.map((entry) => entry.account))].sort((a, b) => (a === UNKNOWN ? 1 : b === UNKNOWN ? -1 : a.localeCompare(b)));
  const shown = () => view.entries.filter((entry) => entry.kind === kind && (!account || entry.account === account));

  // ------------------------------------------------------------ rendering

  const renderHead = () => {
    const state = status.recording
      ? `<span class="rec-state is-live">${icon("video", 14)}<b>${esc(t("rec.recordingNow", { account: accountLabel(status.account) }))}</b></span>`
      : status.enabled
        ? `<span class="rec-state is-on">${icon("video", 14)}<b>${esc(t("rec.ready"))}</b></span>`
        : `<span class="rec-state is-off">${icon("video", 14)}<b>${esc(t("rec.off"))}</b><span>${esc(t("rec.turnOn"))}</span></span>`;
    const failed = status.error && !status.recording && status.error !== "recorder_stopped" ? `<span class="rec-error">${esc(t("rec.lastFailed", { reason: errorText(status.error) }))}</span>` : "";
    const usage = view.limitBytes ? t("rec.usedOf", { used: bytes(view.totalBytes), limit: bytes(view.limitBytes) }) : t("rec.used", { used: bytes(view.totalBytes) });
    head.innerHTML = `${state}${failed}
      <span class="rec-usage">${esc(usage)}</span>
      <button class="btn btn-ghost" data-rec="folder" data-tooltip="${esc(view.root)}">${icon("folder-open", 14)}<span>${esc(t("rec.openFolder"))}</span></button>`;
  };

  const renderNav = () => {
    const count = (name: string) => view.entries.filter((entry) => !name || entry.account === name).length;
    nav.innerHTML = [["", t("rec.all")], ...accounts().map((name) => [name, accountLabel(name)])]
      .map(([name, label]) => `<button role="tab" data-rec-account="${esc(name)}" aria-selected="${name === account}"><span>${esc(label)}</span><em>${count(name)}</em></button>`)
      .join("");
  };

  const renderList = () => {
    const entries = shown();
    const rows = entries.map((entry) => {
      const meta = entry.meta;
      const date = new Date(meta?.startedAt || entry.modified).toLocaleString(localeTag(), { dateStyle: "medium", timeStyle: "short" });
      const result = meta?.result === "Win" ? `<span class="rec-result is-win">${esc(t("rec.win"))}</span>` : meta?.result === "Loss" ? `<span class="rec-result is-loss">${esc(t("rec.loss"))}</span>` : "";
      const live = isRecording(entry) ? `<span class="rec-result is-live">${esc(t("rec.inProgress"))}</span>` : "";
      const details = [date, meta?.duration ? clock(meta.duration) : "", bytes(entry.size), account ? "" : accountLabel(entry.account)].filter(Boolean).join(" · ");
      const score = entry.kind === "game" ? kda(entry) : "";
      return `
        <div class="rec-row${selected?.path === entry.path ? " is-selected" : ""}" role="button" tabindex="0" data-rec-path="${esc(entry.path)}">
          <div class="rec-row-main"><strong>${esc(title(entry))}</strong>${score ? `<span class="rec-kda">${esc(score)}</span>` : ""}${result}${live}</div>
          <div class="rec-row-sub">${esc(details)}</div>
          <button class="icon-btn rec-row-delete" data-rec="delete" data-tooltip="${esc(t("menu.delete"))}" aria-label="${esc(t("menu.delete"))}">${icon("trash", 14)}</button>
        </div>`;
    });
    list.innerHTML = `
      <div class="segmented rec-kind" role="tablist">
        <button data-rec-kind="game" aria-pressed="${kind === "game"}">${icon("film", 13)}<span>${esc(t("rec.games"))}</span></button>
        <button data-rec-kind="clip" aria-pressed="${kind === "clip"}">${icon("scissors", 13)}<span>${esc(t("rec.clips"))}</span></button>
      </div>
      <div class="rec-rows">${rows.join("") || `<p class="rec-empty">${esc(t(kind === "game" ? "rec.empty" : "rec.emptyClips"))}</p>`}</div>`;
  };

  // The player is rebuilt only when the selection changes; timeline updates are in place.
  let video: HTMLVideoElement | null = null;

  const markerHtml = (moment: Moment, index: number) =>
    `<button class="rec-marker tone-${momentTone(moment)}" data-rec-moment="${index}" data-tooltip="${esc(momentTooltip(moment))}" aria-label="${esc(momentTooltip(moment))}">${momentIcon(moment.kind)}${moment.kind === "multikill" ? `<b>${esc(String(Math.min(5, Math.max(2, Number(moment.detail) || 2))))}</b>` : ""}</button>`;

  /** Markers on the timeline and the side list, for the current filter. */
  const renderMoments = () => {
    const track = player.querySelector<HTMLElement>("[data-rec-track]");
    if (!track || !selected) return;
    const moments = currentMoments();
    track.querySelectorAll(".rec-marker").forEach((marker) => marker.remove());
    track.querySelector("[data-rec-handle]")?.insertAdjacentHTML("beforebegin", moments.map(markerHtml).join(""));
    const all = cleanMoments(selected.meta?.moments ?? []);
    side.querySelectorAll<HTMLElement>("[data-rec-filter]").forEach((button) => {
      button.setAttribute("aria-pressed", String(button.dataset.recFilter === filter));
      const count = all.filter((moment) => matches(moment, button.dataset.recFilter as Filter)).length;
      const badge = button.querySelector("em");
      if (badge) badge.textContent = String(count);
    });
    const rows = moments
      .map((moment, index) => {
        const detail = moment.detail && !["multikill", "gameEnd"].includes(moment.kind) ? moment.detail : moment.ally === true ? t("rec.allyTeam") : moment.ally === false ? t("rec.enemyTeam") : "";
        return `<button class="rec-side-row tone-${momentTone(moment)}" data-rec-moment="${index}">
          ${momentIcon(moment.kind, 15)}<b>${clock(moment.time)}</b><span>${esc(momentLabel(moment))}</span>${detail ? `<em>${esc(detail)}</em>` : ""}</button>`;
      })
      .join("");
    $(".rec-side-list", side).innerHTML = rows || `<p class="rec-empty">${esc(t("rec.noMoments"))}</p>`;
    updateTimeline();
  };

  const closeViewer = () => {
    stopSound();
    mixerOpen = false;
    video?.pause();
    video?.removeAttribute("src");
    video?.load();
    video = null;
    selected = null;
    range = null;
    parts = [];
    viewer.hidden = true;
    player.innerHTML = "";
    side.innerHTML = "";
    renderList();
  };

  const renderPlayer = () => {
    stopSound();
    gameDetails = null;
    gameState = "none";
    gamePanel.hidden = true;
    gamePanel.innerHTML = "";
    scroller.scrollTop = 0;
    video = null;
    zoom = 1;
    viewStart = 0;
    if (!selected) {
      viewer.hidden = true;
      return;
    }
    const entry = selected;
    const meta = entry.meta;
    viewer.hidden = false;
    const date = new Date(meta?.startedAt || entry.modified).toLocaleString(localeTag(), { dateStyle: "medium", timeStyle: "short" });
    const result = meta?.result === "Win" ? `<span class="rec-result is-win">${esc(t("rec.win"))}</span>` : meta?.result === "Loss" ? `<span class="rec-result is-loss">${esc(t("rec.loss"))}</span>` : "";
    const score = entry.kind === "game" ? kda(entry) : "";
    viewerHead.innerHTML = `
      <button class="btn btn-ghost rec-back" data-rec="back">${icon("arrow-left", 14)}<span>${esc(t("rec.title"))}</span></button>
      <div class="rec-viewer-title">
        <h2>${esc(title(entry))}</h2>${result}${score ? `<span class="rec-kda">${esc(score)}</span>` : ""}
        <span class="rec-viewer-sub">${esc([date, meta?.duration ? clock(meta.duration) : "", accountLabel(entry.account), bytes(entry.size)].filter(Boolean).join(" · "))}</span>
      </div>
      ${entry.kind === "game" ? `<button class="btn btn-ghost" data-rec="game-info" aria-pressed="${gameShown()}">${icon("chart-line", 14)}<span>${esc(t("gi.title"))}</span></button>` : ""}
      <button class="btn btn-ghost" data-rec="reveal">${icon("folder-open", 14)}<span>${esc(t("rec.showFile"))}</span></button>
      <button class="btn btn-ghost" data-rec="delete-current">${icon("trash", 14)}<span>${esc(t("menu.delete"))}</span></button>`;
    if (isRecording(entry)) {
      player.innerHTML = `<p class="rec-empty rec-pick">${esc(t("rec.inProgress"))}</p>`;
      side.innerHTML = "";
      return;
    }
    const facts = meta
      ? [meta.width && `${meta.width}×${meta.height}`, meta.fps && `${meta.fps} fps`, meta.encoder, meta.audioSource && meta.audioSource.split("+").map((source) => t(`rec.audio.${source}` as MessageKey)).join(" + ")].filter(Boolean)
      : [];
    player.innerHTML = `
      <div class="rec-screen"><video preload="metadata" crossorigin="anonymous" src="http://larec.localhost/${encodeURIComponent(entry.path)}?v=${entry.modified}"></video></div>
      <div class="rec-timeline">
        <div class="rec-track" data-rec-track>
          <div class="rec-parts-layer"></div>
          <div class="rec-range" hidden></div>
          <div class="rec-progress"></div>
          <div class="rec-handle" data-rec-handle="start" hidden></div>
          <div class="rec-handle" data-rec-handle="end" hidden></div>
          <div class="rec-head-line"></div>
        </div>
        <div class="rec-overview" data-rec-overview hidden><div class="rec-overview-window"></div><div class="rec-overview-head"></div></div>
      </div>
      <div class="rec-controls">
        <button class="icon-btn rec-play" data-rec="play" aria-label="${esc(t("rec.play"))}"><span class="rec-play-glyph"></span></button>
        <span class="rec-time"><b data-rec-now>0:00</b> / <span data-rec-total>${clock(meta?.duration ?? 0)}</span></span>
        <span class="rec-spacer"></span>
        <div class="rec-zoom" data-tooltip="${esc(t("rec.zoomHint"))}">
          <button class="icon-btn" data-rec="zoom-out" aria-label="${esc(t("rec.zoomOut"))}">${icon("minus", 13)}</button>
          <span data-rec-zoom>×1</span>
          <button class="icon-btn" data-rec="zoom-in" aria-label="${esc(t("rec.zoomIn"))}">${icon("plus", 13)}</button>
        </div>
        <div class="rec-mixer-wrap">
          <button class="icon-btn" data-rec="mixer" aria-label="${esc(t("rec.volume"))}" data-tooltip="${esc(t("rec.volume"))}">${icon(level("master") === 0 ? "volume-x" : "volume", 15)}</button>
          <div class="rec-mixer" data-rec-mixer hidden></div>
        </div>
        <button class="icon-btn" data-rec="fullscreen" aria-label="${esc(t("rec.fullscreen"))}">${icon("monitor", 15)}</button>
      </div>
      <div class="rec-tools">
        <button class="btn btn-ghost" data-rec="set-start">${esc(t("rec.setStart"))}</button>
        <button class="btn btn-ghost" data-rec="set-end">${esc(t("rec.setEnd"))}</button>
        <button class="btn btn-ghost" data-rec="add-part" disabled>${esc(t("rec.addPart"))}</button>
        <span class="rec-range-text" data-rec-range></span>
        <div class="segmented rec-mode" data-tooltip="${esc(t(MODE_HINTS[mode]))}">
          <button data-rec-mode="fast" aria-pressed="${mode === "fast"}">${esc(t("rec.fast"))}</button>
          <button data-rec-mode="exact" aria-pressed="${mode === "exact"}">${esc(t("rec.exact"))}</button>
          <button data-rec-mode="discord" aria-pressed="${mode === "discord"}">${esc(t("rec.discord"))}</button>
        </div>
        <button class="btn btn-primary" data-rec="save-clip" disabled>${icon("scissors", 14)}<span>${esc(t("rec.saveClip"))}</span></button>
      </div>
      <div class="rec-parts" data-rec-parts hidden></div>`;
    side.innerHTML = `
      <h3 class="rec-side-title">${esc(t("rec.moments"))}</h3>
      <div class="rec-filters">${FILTERS.map(([id, key]) => `<button data-rec-filter="${id}" aria-pressed="${id === filter}"><span>${esc(t(key))}</span><em></em></button>`).join("")}</div>
      <div class="rec-side-list"></div>
      <div class="rec-side-facts">${[entry.name, ...facts].map((fact) => `<span>${esc(String(fact))}</span>`).join("")}</div>`;
    video = $<HTMLVideoElement>("video", player);
    video.addEventListener("loadedmetadata", updateTimeline);
    video.addEventListener("timeupdate", updateTimeline);
    video.addEventListener("play", updatePlayState);
    video.addEventListener("pause", updatePlayState);
    video.addEventListener("click", () => togglePlay());
    video.addEventListener("error", () => toast("error", t("rec.playFailed")));
    video.addEventListener("play", () => void audio?.resume().catch(() => undefined));
    for (const name of ["play", "pause", "seeked", "ratechange", "playing", "timeupdate"]) video.addEventListener(name, () => syncTracks(name === "seeked" || name === "playing"));
    video.addEventListener("waiting", () => tracks.forEach(({ element }) => element.pause()));
    connectVideo(video);
    void loadTracks(entry);
    void loadGame(entry, true);
    renderMoments();
    updatePlayState();
    renderMixer();
  };

  // ------------------------------------------------------------ game details

  const renderGame = () => {
    const entry = selected;
    const shown = Boolean(entry && entry.kind === "game" && gameShown() && gameState !== "none");
    gamePanel.hidden = !shown;
    viewerHead.querySelector('[data-rec="game-info"]')?.setAttribute("aria-pressed", String(gameShown()));
    if (!shown) return;
    const bar = `<div class="rec-game-bar"><h3>${esc(t("gi.title"))}</h3><button class="btn btn-ghost" data-rec="game-hide">${icon("eye-off", 14)}<span>${esc(t("gi.hide"))}</span></button></div>`;
    if (gameState === "ready" && gameDetails) {
      if (!gamePanel.querySelector(".rec-game-bar")) gamePanel.innerHTML = `${bar}<div class="rec-game-content"></div>`;
      renderGameInfo($(".rec-game-content", gamePanel), gameDetails);
      return;
    }
    const message = gameState === "loading" ? t("gi.loading") : errorText(gameError);
    gamePanel.innerHTML = `${bar}<div class="rec-game-missing"><p>${esc(message)}</p>${gameState === "error" ? `<button class="btn btn-ghost" data-rec="game-load">${esc(t("gi.load"))}</button>` : ""}</div>`;
  };

  // A time picked in the game stats (game clock seconds): watch it in the video.
  gamePanel.addEventListener("gi-seek", (event) => {
    const gameSeconds = Number((event as CustomEvent<number>).detail);
    const meta = selected?.meta;
    const moment = meta?.moments.find((entry) => Number.isFinite(entry.gameTime));
    const offset = meta?.gameStart ?? (moment ? moment.time - moment.gameTime : null);
    if (!video || offset == null || !Number.isFinite(gameSeconds)) return;
    seek(gameSeconds + offset - LEAD);
    scroller.scrollTo({ top: 0, behavior: "smooth" });
    if (video.paused) void video.play().catch(() => undefined);
  });

  const loadGame = async (entry: Recording, fetch: boolean) => {
    if (entry.kind !== "game") return;
    gameState = "loading";
    renderGame();
    try {
      const details = await api.recordingGame(entry.path, fetch);
      if (selected !== entry) return;
      gameDetails = details;
      gameState = details ? "ready" : "error";
      gameError = details ? "" : "game_not_found";
    } catch (error) {
      if (selected !== entry) return;
      gameState = "error";
      gameError = error instanceof ApiError ? error.code : "game_not_found";
    }
    gamePanel.innerHTML = "";
    renderGame();
  };

  // ------------------------------------------------------------ sound

  const context = (): AudioContext | null => {
    if (!audio) {
      try {
        audio = new AudioContext();
        masterGain = audio.createGain();
        masterGain.connect(audio.destination);
      } catch {
        audio = null;
      }
    }
    return audio;
  };

  const applyLevels = () => {
    if (masterGain) masterGain.gain.value = level("master") / 100;
    else if (video) video.volume = Math.min(1, level("master") / 100);
    if (videoSound) videoSound.gain.gain.value = trackState === "ready" ? 0 : 1;
    tracks.forEach(({ label, gain }) => (gain.gain.value = level(label) / 100));
    const button = player.querySelector<HTMLElement>('[data-rec="mixer"]');
    if (button) button.innerHTML = icon(level("master") === 0 ? "volume-x" : "volume", 15);
  };

  const connectVideo = (element: HTMLVideoElement) => {
    const context_ = context();
    if (!context_ || !masterGain) return applyLevels();
    try {
      const source = context_.createMediaElementSource(element);
      const gain = context_.createGain();
      source.connect(gain).connect(masterGain);
      videoSound = { source, gain };
    } catch {
      videoSound = null;
    }
    applyLevels();
  };

  const stopSound = () => {
    tracks.forEach(({ element, source, gain }) => {
      element.pause();
      element.removeAttribute("src");
      element.load();
      source.disconnect();
      gain.disconnect();
    });
    tracks = [];
    trackState = "none";
    videoSound?.source.disconnect();
    videoSound?.gain.disconnect();
    videoSound = null;
  };

  /** The sources follow the video: same position, rate and play state. */
  const syncTracks = (force = false) => {
    if (!video || trackState !== "ready") return;
    for (const { element } of tracks) {
      if (element.playbackRate !== video.playbackRate) element.playbackRate = video.playbackRate;
      if (force || Math.abs(element.currentTime - video.currentTime) > 0.15) element.currentTime = video.currentTime;
      if (video.paused || video.seeking) {
        if (!element.paused) element.pause();
      } else if (element.paused) {
        void element.play().catch(() => undefined);
      }
    }
  };

  /** Recordings with game, mic and Discord on their own tracks: each source as
   * its own file (split once, a few seconds for a long game). */
  const loadTracks = async (entry: Recording) => {
    const sources = (entry.meta?.audioTracks ?? []).filter((track) => track !== "mix");
    if ((entry.meta?.audioTracks.length ?? 0) < 2 || !sources.length || !audio || !masterGain) return;
    trackState = "loading";
    renderMixer();
    try {
      const list = await api.recordingSoundTracks(entry.path);
      if (selected !== entry || !audio || !masterGain) return;
      const context_ = audio;
      const master = masterGain;
      const made = list.map(({ label, path }) => {
        const element = new Audio();
        element.crossOrigin = "anonymous";
        element.preload = "auto";
        element.src = `http://larec.localhost/${encodeURIComponent(path)}?v=${entry.modified}`;
        const source = context_.createMediaElementSource(element);
        const gain = context_.createGain();
        gain.gain.value = level(label) / 100;
        source.connect(gain).connect(master);
        return { label, element, source, gain };
      });
      // Switch from the mixed track only once every source can play.
      await Promise.all(
        made.map(
          ({ element }) =>
            new Promise<void>((resolve, reject) => {
              element.addEventListener("canplay", () => resolve(), { once: true });
              element.addEventListener("error", () => reject(new Error("track")), { once: true });
              setTimeout(() => reject(new Error("timeout")), 15000);
            }),
        ),
      ).catch((error) => {
        made.forEach(({ element, source, gain }) => {
          element.removeAttribute("src");
          source.disconnect();
          gain.disconnect();
        });
        throw error;
      });
      if (selected !== entry) {
        made.forEach(({ element, source, gain }) => {
          element.removeAttribute("src");
          source.disconnect();
          gain.disconnect();
        });
        return;
      }
      tracks = made;
      trackState = made.length ? "ready" : "failed";
    } catch {
      if (selected === entry) trackState = "failed";
    }
    applyLevels();
    syncTracks(true);
    renderMixer();
  };

  const renderMixer = () => {
    const panel = player.querySelector<HTMLElement>("[data-rec-mixer]");
    if (!panel || !selected) return;
    panel.hidden = !mixerOpen;
    if (!mixerOpen) return;
    const sources = (selected.meta?.audioTracks ?? []).filter((track) => track !== "mix");
    const separate = sources.length > 0 && (selected.meta?.audioTracks.length ?? 0) > 1;
    const row = (name: string, label: string, max: number, disabled = false) => `
      <label class="rec-mix-row${disabled ? " is-disabled" : ""}"><span>${esc(label)}</span>
        <input type="range" min="0" max="${max}" step="5" value="${level(name)}" data-rec-level="${esc(name)}"${disabled ? " disabled" : ""}/>
        <b data-rec-level-value="${esc(name)}">${level(name)}%</b></label>`;
    const note = !separate ? "" : trackState === "loading" ? t("rec.preparingTracks") : trackState === "failed" ? t("rec.tracksFailed") : t("rec.levelsHint");
    panel.innerHTML = `<h4>${esc(t("rec.volume"))}</h4>
      ${row("master", t("rec.master"), MAX_MASTER)}
      ${separate ? sources.map((source) => row(source, t(`rec.audio.${source}` as MessageKey), MAX_LEVEL, trackState !== "ready")).join("") : ""}
      ${note ? `<p class="rec-mix-note">${esc(note)}</p>` : ""}`;
  };

  const duration = () => (video && Number.isFinite(video.duration) && video.duration > 0 ? video.duration : selected?.meta?.duration || 0);
  /** The visible part of the timeline, in seconds. */
  const viewLength = () => (duration() || 1) / zoom;
  const clampView = () => {
    viewStart = Math.min(Math.max(0, viewStart), Math.max(0, (duration() || 0) - viewLength()));
  };
  const inView = (time: number) => time >= viewStart && time <= viewStart + viewLength();
  const percent = (time: number) => `${Math.min(100, Math.max(0, ((time - viewStart) / viewLength()) * 100))}%`;
  /** Zoom by `factor` keeping `anchor` (seconds) at the same place on screen. */
  const setZoom = (next: number, anchor = video?.currentTime ?? 0) => {
    const level = Math.min(64, Math.max(1, next));
    const fraction = viewLength() ? (anchor - viewStart) / viewLength() : 0.5;
    zoom = level;
    viewStart = anchor - Math.min(1, Math.max(0, fraction)) * viewLength();
    clampView();
    updateTimeline();
  };

  const updateTimeline = () => {
    if (!video) return;
    const total = duration();
    const track = player.querySelector<HTMLElement>("[data-rec-track]");
    if (!track) return;
    // Zoomed in and playing: the view follows the playhead.
    if (zoom > 1 && !video.paused && !inView(video.currentTime)) {
      viewStart = video.currentTime - viewLength() * 0.1;
    }
    clampView();
    const zoomLabel = player.querySelector<HTMLElement>("[data-rec-zoom]");
    if (zoomLabel) zoomLabel.textContent = zoom > 1 ? `×${zoom}` : "×1";
    track.classList.toggle("is-zoomed", zoom > 1);
    $("[data-rec-now]", player).textContent = clock(video.currentTime);
    $("[data-rec-total]", player).textContent = clock(total);
    $(".rec-progress", track).style.width = percent(video.currentTime);
    $(".rec-head-line", track).style.left = percent(video.currentTime);
    // Icons that would overlap go to the row above (up to three rows).
    const moments = currentMoments();
    const width = track.clientWidth || 1;
    const lanes: number[] = [];
    [...track.querySelectorAll<HTMLElement>(".rec-marker")]
      .map((marker) => ({ marker, time: moments[Number(marker.dataset.recMoment)]?.time ?? 0 }))
      .sort((a, b) => a.time - b.time)
      .forEach(({ marker, time }) => {
        marker.hidden = !inView(time);
        if (marker.hidden) return;
        const x = ((time - viewStart) / viewLength()) * width;
        let lane = lanes.findIndex((last) => x - last >= 17);
        if (lane < 0) lane = lanes.length < 3 ? lanes.length : 0;
        lanes[lane] = x;
        marker.style.left = percent(time);
        marker.style.setProperty("--lane", String(lane));
      });
    track.style.setProperty("--lanes", String(Math.max(1, lanes.length)));
    const area = $(".rec-range", track);
    const handles = track.querySelectorAll<HTMLElement>("[data-rec-handle]");
    area.hidden = !range;
    handles.forEach((handle) => (handle.hidden = !range));
    const text = $("[data-rec-range]", player);
    const save = $<HTMLButtonElement>('[data-rec="save-clip"]', player);
    if (range) {
      area.style.left = percent(range.start);
      area.style.width = `calc(${percent(range.end)} - ${percent(range.start)})`;
      handles[0].style.left = percent(range.start);
      handles[1].style.left = percent(range.end);
      text.textContent = t("rec.clipRange", { start: clock(range.start), end: clock(range.end), length: clock(range.end - range.start) });
    } else {
      text.textContent = t("rec.clipHint");
    }
    const overview = player.querySelector<HTMLElement>("[data-rec-overview]");
    if (overview) {
      overview.hidden = zoom <= 1;
      const whole = total || 1;
      $(".rec-overview-window", overview).style.cssText = `left:${(viewStart / whole) * 100}%;width:${(viewLength() / whole) * 100}%`;
      $(".rec-overview-head", overview).style.left = `${Math.min(100, (video.currentTime / whole) * 100)}%`;
    }
    // The side list shows the last moment before the playhead.
    let current = -1;
    moments.forEach((moment, index) => {
      if (moment.time <= video!.currentTime + 0.5) current = index;
    });
    side.querySelectorAll<HTMLElement>(".rec-side-row").forEach((row) => row.classList.toggle("is-current", Number(row.dataset.recMoment) === current));
    const layer = $(".rec-parts-layer", track);
    layer.innerHTML = parts.map((part) => `<div class="rec-part" style="left:${percent(part.start)};width:calc(${percent(part.end)} - ${percent(part.start)})"></div>`).join("");
    const list = $("[data-rec-parts]", player);
    list.hidden = parts.length === 0;
    const partsLength = parts.reduce((sum, part) => sum + part.end - part.start, 0);
    list.innerHTML = parts.length
      ? `<span class="rec-parts-total">${esc(t("rec.parts", { count: parts.length, length: clock(partsLength) }))}</span>` +
        parts
          .map((part, index) => `<span class="rec-part-chip"><button class="text-button" data-rec-part-seek="${index}">${clock(part.start)} – ${clock(part.end)}</button><button class="rec-part-remove" data-rec-part-remove="${index}" aria-label="${esc(t("rec.removePart"))}" data-tooltip="${esc(t("rec.removePart"))}">${icon("x", 10)}</button></span>`)
          .join("")
      : "";
    $<HTMLButtonElement>('[data-rec="add-part"]', player).disabled = !range || range.end - range.start < 1;
    save.disabled = saving || clipParts().length === 0;
    save.classList.toggle("is-busy", saving);
  };

  /** Merge a part into the list (time order, overlaps joined). */
  const merge = (list: { start: number; end: number }[], part: { start: number; end: number }) => {
    const sorted = [...list, part].sort((a, b) => a.start - b.start);
    const joined: { start: number; end: number }[] = [];
    for (const item of sorted) {
      const last = joined.at(-1);
      if (last && item.start <= last.end) last.end = Math.max(last.end, item.end);
      else joined.push({ ...item });
    }
    return joined;
  };
  /** What Save clip saves: the added parts plus the current selection. */
  const clipParts = () => {
    const current = range && range.end - range.start >= 1 ? range : null;
    return current ? merge(parts, current) : parts;
  };

  const updatePlayState = () => {
    const button = player.querySelector<HTMLElement>(".rec-play");
    button?.classList.toggle("is-playing", Boolean(video && !video.paused));
  };

  const togglePlay = () => {
    if (!video) return;
    if (video.paused) void video.play().catch(() => undefined);
    else video.pause();
  };

  const seek = (time: number) => {
    if (!video) return;
    video.currentTime = Math.min(Math.max(0, time), duration());
    if (zoom > 1 && !inView(video.currentTime)) viewStart = video.currentTime - viewLength() / 2;
    updateTimeline();
  };

  const render = () => {
    renderHead();
    renderNav();
    renderList();
  };

  const reload = async (keepSelection = true) => {
    try {
      [view, status] = await Promise.all([api.recordings(), api.recorderStatus()]);
    } catch (error) {
      toast("error", t("rec.loadFailed"), describe(error), 6000);
    }
    const previous = selected?.path;
    const previousVersion = selected ? `${selected.size}:${selected.modified}` : "";
    selected = keepSelection ? (view.entries.find((entry) => entry.path === previous) ?? null) : null;
    // The same recording rewritten on disk (e.g. made seekable): load the new file.
    const changed = selected !== null && `${selected.size}:${selected.modified}` !== previousVersion;
    if (account && !view.entries.some((entry) => entry.account === account)) account = "";
    renderHead();
    renderNav();
    renderList();
    if (previous && !selected) {
      // The open recording is gone (deleted, or replaced by the disk limit).
      closeViewer();
    } else if (changed && video) {
      const time = video.currentTime;
      renderPlayer();
      video?.addEventListener("loadedmetadata", () => seek(time), { once: true });
    }
  };

  /** Open a recording in the full-window viewer. */
  const select = (path: string) => {
    selected = view.entries.find((entry) => entry.path === path) ?? null;
    range = null;
    parts = [];
    renderList();
    renderPlayer();
    viewer.querySelector<HTMLElement>("[data-rec='back']")?.focus();
  };

  const remove = async (path: string) => {
    const entry = view.entries.find((item) => item.path === path);
    if (!entry) return;
    const answer = await modal({
      title: t("rec.deleteTitle"),
      body: `<p class="hint">${esc(t("rec.deleteText", { name: entry.name }))}</p>`,
      actions: [{ label: t("common.cancel"), value: "cancel" }, { label: t("menu.delete"), value: "delete", kind: "danger" }],
    });
    if (answer !== "delete") return;
    if (selected?.path === path) closeViewer();
    try {
      await api.deleteRecording(path);
      toast("success", t("rec.deleted"));
    } catch (error) {
      toast("error", t("rec.deleteFailed"), describe(error));
    }
    await reload();
  };

  const saveClip = async () => {
    if (!selected || saving) return;
    const source = selected;
    const chosen = clipParts();
    if (!chosen.length) return;
    const suggested = `${title(source).replace(/ · /g, " ")} ${clock(chosen[0].start).replace(/:/g, "-")}`;
    let name = "";
    // Sources recorded on their own tracks can be left out of the clip.
    const meta = source.meta;
    const sources = (meta?.audioTracks ?? []).filter((track) => track !== "mix");
    const choices = sources.length ? sources : meta?.audio ? ["all"] : [];
    let sounds: string[] | undefined;
    const soundLabel = (sound: string) => (sound === "all" ? t("rec.sound") : `${t(`rec.audio.${sound}` as MessageKey)} ${level(sound)}%`);
    // The player's volume per source goes into the clip.
    const gains = sources.length ? Object.fromEntries(sources.map((sound) => [sound, level(sound) / 100])) : undefined;
    const answer = await modal({
      title: t("rec.saveClip"),
      body: `<label class="field"><span>${esc(t("rec.clipName"))}</span>
        <input name="clipName" maxlength="120" autocomplete="off" spellcheck="false" value="${esc(suggested)}"/></label>
        ${choices.length ? `<div class="rec-sounds"><span>${esc(t("rec.sound"))}${sources.length ? ` <em>${esc(t("rec.clipLevels"))}</em>` : ""}</span>${choices
          .map((sound) => `<label class="rec-sound"><input type="checkbox" name="clipSound" value="${esc(sound)}"${sound === "all" || level(sound) > 0 ? " checked" : ""}/><span>${esc(soundLabel(sound))}</span></label>`)
          .join("")}</div>` : ""}
        <p class="hint">${esc(t(MODE_HINTS[mode]))}</p>`,
      actions: [{ label: t("common.cancel"), value: "cancel" }, { label: t("rec.saveClip"), value: "save", kind: "primary" }],
      onOpen: (root) => $<HTMLInputElement>('[name="clipName"]', root).select(),
      onSubmit: (_value, root) => {
        name = $<HTMLInputElement>('[name="clipName"]', root).value.trim();
        const checked = [...root.querySelectorAll<HTMLInputElement>('[name="clipSound"]:checked')].map((input) => input.value);
        // Everything kept: undefined (the recording's mix as it is).
        sounds = checked.length === choices.length ? undefined : checked.filter((sound) => sound !== "all");
        return true;
      },
    });
    if (answer !== "save" || selected !== source) return;
    saving = true;
    updateTimeline();
    toast("info", t("rec.saving"));
    try {
      const clip = await api.saveClip(source.path, chosen.map((part) => [part.start, part.end] as [number, number]), mode, name, sounds, gains);
      parts = [];
      range = null;
      toast("success", t("rec.clipSaved"), clip.name);
    } catch (error) {
      toast("error", t("rec.clipFailed"), describe(error), 6000);
    }
    saving = false;
    await reload();
    updateTimeline();
  };

  // ------------------------------------------------------------ events

  const close = () => {
    open = false;
    video?.pause();
    stopSound();
    void audio?.close().catch(() => undefined);
    audio = null;
    masterGain = null;
    document.removeEventListener("keydown", onKey, true);
    resized.disconnect();
    unlisten.forEach((stop) => void stop.then((fn) => fn()));
    void leave(overlay, "overlay-out");
    previousFocus?.focus?.();
  };

  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) close();
  });

  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const action = target.closest<HTMLElement>("[data-rec]")?.dataset.rec;
    const row = target.closest<HTMLElement>("[data-rec-path]");
    if (action === "close") return close();
    if (action === "back") return closeViewer();
    if (action === "delete-current" && selected) return void remove(selected.path);
    const chosenFilter = target.closest<HTMLElement>("[data-rec-filter]")?.dataset.recFilter as Filter | undefined;
    if (chosenFilter) {
      filter = chosenFilter;
      return renderMoments();
    }
    if (action === "folder") return void api.openRecordingsFolder(null).catch((error) => toast("error", t("toast.openFailed"), describe(error)));
    if (action === "reveal" && selected) return void api.openRecordingsFolder(selected.path).catch((error) => toast("error", t("toast.openFailed"), describe(error)));
    if (action === "delete" && row) return void remove(row.dataset.recPath!);
    if (action === "play") return togglePlay();
    if (action === "game-info") {
      setGameShown(true);
      renderGame();
      if (gameState === "none" && selected) void loadGame(selected, true);
      return void requestAnimationFrame(() => gamePanel.scrollIntoView({ behavior: "smooth", block: "start" }));
    }
    if (action === "game-hide") {
      setGameShown(false);
      scroller.scrollTo({ top: 0, behavior: "smooth" });
      return renderGame();
    }
    if (action === "game-load" && selected) return void loadGame(selected, true);
    if (action === "mixer") {
      mixerOpen = !mixerOpen;
      return renderMixer();
    }
    if (target.closest(".rec-mixer")) return;
    if (mixerOpen) {
      mixerOpen = false;
      renderMixer();
    }
    if (action === "fullscreen") return void player.querySelector(".rec-screen")?.requestFullscreen?.().catch(() => undefined);
    if ((action === "set-start" || action === "set-end") && video) {
      const now = video.currentTime;
      const total = duration();
      if (action === "set-start") range = { start: now, end: range && range.end > now + 1 ? range.end : Math.min(total, now + 30) };
      else range = { start: range && range.start < now - 1 ? range.start : Math.max(0, now - 30), end: now };
      return updateTimeline();
    }
    if (action === "save-clip") return void saveClip();
    if (action === "zoom-in") return setZoom(zoom * 2);
    if (action === "zoom-out") return setZoom(zoom / 2);
    if (action === "add-part" && range && range.end - range.start >= 1) {
      parts = merge(parts, range);
      range = null;
      return updateTimeline();
    }
    const removePart = target.closest<HTMLElement>("[data-rec-part-remove]")?.dataset.recPartRemove;
    if (removePart !== undefined) {
      parts.splice(Number(removePart), 1);
      return updateTimeline();
    }
    const seekPart = target.closest<HTMLElement>("[data-rec-part-seek]")?.dataset.recPartSeek;
    if (seekPart !== undefined) return seek(parts[Number(seekPart)]?.start ?? 0);
    const chosenMode = target.closest<HTMLElement>("[data-rec-mode]")?.dataset.recMode;
    if (chosenMode) {
      mode = chosenMode as ClipMode;
      player.querySelectorAll<HTMLElement>("[data-rec-mode]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.recMode === chosenMode)));
      player.querySelector<HTMLElement>(".rec-mode")!.dataset.tooltip = t(MODE_HINTS[mode]);
      return;
    }
    const moment = target.closest<HTMLElement>("[data-rec-moment]")?.dataset.recMoment;
    if (moment !== undefined) {
      const time = currentMoments()[Number(moment)]?.time ?? 0;
      seek(time - LEAD);
      if (video?.paused) void video.play().catch(() => undefined);
      return;
    }
    const accountButton = target.closest<HTMLElement>("[data-rec-account]");
    if (accountButton) {
      account = accountButton.dataset.recAccount ?? "";
      renderNav();
      return renderList();
    }
    const kindButton = target.closest<HTMLElement>("[data-rec-kind]")?.dataset.recKind as "game" | "clip" | undefined;
    if (kindButton && kindButton !== kind) {
      kind = kindButton;
      return renderList();
    }
    if (row) return select(row.dataset.recPath!);
  });

  // Track: click to seek; drag the clip handles.
  overlay.addEventListener("pointerdown", (event) => {
    const target = event.target as HTMLElement;
    const overview = target.closest<HTMLElement>("[data-rec-overview]");
    if (overview && video) {
      // The overview strip: move the zoomed view there.
      const box = overview.getBoundingClientRect();
      viewStart = Math.min(1, Math.max(0, (event.clientX - box.left) / box.width)) * duration() - viewLength() / 2;
      clampView();
      return updateTimeline();
    }
    const track = target.closest<HTMLElement>("[data-rec-track]");
    if (!track || !video || target.closest(".rec-marker")) return;
    const handle = target.closest<HTMLElement>("[data-rec-handle]")?.dataset.recHandle as "start" | "end" | undefined;
    const timeAt = (x: number) => {
      const box = track.getBoundingClientRect();
      return viewStart + Math.min(1, Math.max(0, (x - box.left) / box.width)) * viewLength();
    };
    const move = (x: number) => {
      const time = timeAt(x);
      if (handle && range) {
        if (handle === "start") range.start = Math.min(time, range.end - 1);
        else range.end = Math.max(time, range.start + 1);
        range.start = Math.max(0, range.start);
        range.end = Math.min(duration(), range.end);
        seek(handle === "start" ? range.start : range.end);
      } else {
        seek(time);
      }
    };
    event.preventDefault();
    track.setPointerCapture(event.pointerId);
    move(event.clientX);
    const onMove = (moveEvent: PointerEvent) => move(moveEvent.clientX);
    const onUp = () => {
      track.removeEventListener("pointermove", onMove);
      track.removeEventListener("pointerup", onUp);
      track.removeEventListener("pointercancel", onUp);
    };
    track.addEventListener("pointermove", onMove);
    track.addEventListener("pointerup", onUp);
    track.addEventListener("pointercancel", onUp);
  });

  // Timeline wheel: Ctrl + wheel zooms around the cursor; the wheel alone moves along when zoomed in.
  overlay.addEventListener(
    "wheel",
    (event) => {
      // Ctrl + wheel is also the web view's page zoom: never let it zoom the app here.
      if (event.ctrlKey) event.preventDefault();
      const area = (event.target as HTMLElement).closest<HTMLElement>(".rec-timeline");
      const track = area?.querySelector<HTMLElement>("[data-rec-track]");
      if (!track || !video) return;
      const box = track.getBoundingClientRect();
      const anchor = viewStart + Math.min(1, Math.max(0, (event.clientX - box.left) / box.width)) * viewLength();
      if (event.ctrlKey) {
        setZoom(event.deltaY < 0 ? zoom * 2 : zoom / 2, anchor);
      } else if (zoom > 1) {
        event.preventDefault();
        const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
        viewStart += Math.sign(delta) * viewLength() * 0.15;
        clampView();
        updateTimeline();
      }
    },
    { passive: false },
  );

  // Volume sliders: live while dragging; double-click sets 100 %.
  overlay.addEventListener("input", (event) => {
    const slider = (event.target as HTMLElement).closest<HTMLInputElement>("[data-rec-level]");
    if (!slider) return;
    const name = slider.dataset.recLevel!;
    setLevel(name, Number(slider.value));
    const value = player.querySelector(`[data-rec-level-value="${CSS.escape(name)}"]`);
    if (value) value.textContent = `${level(name)}%`;
    applyLevels();
  });
  overlay.addEventListener("dblclick", (event) => {
    const slider = (event.target as HTMLElement).closest<HTMLInputElement>("[data-rec-level]");
    if (!slider || slider.disabled) return;
    slider.value = "100";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });

  overlay.addEventListener("keydown", (event) => {
    const row = (event.target as HTMLElement).closest<HTMLElement>("[data-rec-path]");
    if (row && (event.key === "Enter" || event.key === " ") && event.target === row) {
      event.preventDefault();
      select(row.dataset.recPath!);
    }
  });

  const onKey = (event: KeyboardEvent) => {
    if ([...document.querySelectorAll("#modal-root > .overlay:not(.overlay-out)")].at(-1) !== overlay) return;
    if ((event.target as HTMLElement).matches?.("input, textarea, select")) return;
    if (event.key === "Escape") {
      if (document.fullscreenElement) return;
      event.preventDefault();
      if (mixerOpen) {
        mixerOpen = false;
        return renderMixer();
      }
      return viewer.hidden ? close() : closeViewer();
    }
    if (!video || viewer.hidden) return;
    if (event.key === " " && !(event.target as HTMLElement).closest?.("[data-rec-path], button")) {
      event.preventDefault();
      togglePlay();
    }
    if (event.key === "ArrowLeft") seek(video.currentTime - 5);
    if (event.key === "ArrowRight") seek(video.currentTime + 5);
  };
  document.addEventListener("keydown", onKey, true);
  const resized = new ResizeObserver(() => updateTimeline());
  resized.observe(player);

  const unlisten = [
    events.onRecordingsChanged(() => void reload()),
    events.onRecordingGame((path) => {
      if (selected?.path === path && gameState !== "ready") void loadGame(selected, false);
    }),
    events.onRecorderStatus((next) => {
      status = next;
      renderHead();
      renderList();
    }),
  ];

  render();
  await reload(false);
}
