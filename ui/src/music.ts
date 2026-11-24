// Music player (Settings › Music, off by default): playlists of YouTube songs.
//
// The sound comes from YouTube's own embedded player, driven with its
// postMessage protocol (no remote script in this page). The player is kept
// tiny and off-screen, so YouTube sends its smallest video with the sound;
// "Show video" lays it over the cover instead. Covers and song lists come from
// Rust (`src/music.rs`); the moving sound wave from `src/music_wave.rs`, only
// while this page is shown and music plays. In the tray the webview stops
// drawing (Rust hides it), so every animation here stops by itself.

import { ApiError, api, events, musicCover, type MusicLibrary, type MusicPlaylist, type MusicTrack } from "./api";
import { $, contextMenu, esc, fragment, modal, toast } from "./dom";
import { icon } from "./icons";
import { errorText, t } from "./i18n";
import { compensate, coverTint, drawBars } from "./wave";

type Repeat = "off" | "all" | "one";

const YT = "https://www.youtube-nocookie.com";
const STATE_KEY = "musicState";

let enabled = false;
let speed = 256;
/** Download speed cap in kbit/s (Settings › Music), 0 = none. */
export const musicSpeed = (): number => speed;
let library: MusicLibrary = { playlists: [] };

/** What plays: a copy of a playlist's songs (or one search result). */
let queue: MusicTrack[] = [];
/** Playlist id the queue came from ("" = search). */
let queueSource = "";
let index = -1;
let shuffle = false;
let repeat: Repeat = "off";
/** Shuffle: songs not played yet in this round; played songs, for Previous. */
let bag: number[] = [];
let history: number[] = [];

let playing = false;
let buffering = false;
let time = 0;
let timeAt = 0;
let duration = 0;
let volume = 70;
let muted = false;
let videoShown = false;
/** Songs refused one after another (stops skipping through a whole list). */
let refused = 0;

let windowVisible = true;
let pageOpen = false;

// ---------------------------------------------------------------- helpers

const store = (key: string, value: string) => {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not kept: fine.
  }
};
const stored = (key: string): string | null => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};

const describe = (error: unknown): string => (error instanceof ApiError ? errorText(error.code, error.detail) : String(error));

export const clockText = (seconds: number): string => {
  const total = Math.max(0, Math.floor(seconds || 0));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
};

const lengthText = (seconds: number): string => {
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return t("music.minutes", { count: minutes });
  return t("music.hours", { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
};

const songsText = (count: number): string => (count === 1 ? t("music.oneSong") : t("music.songs", { count }));

const current = (): MusicTrack | null => queue[index] ?? null;

const nowTime = (): number => (playing && !buffering ? Math.min(duration || Infinity, time + (performance.now() - timeAt) / 1000) : time);

/** What plays, for the tray player (Rust keeps it; null = nothing loaded). */
function reportNow(): void {
  const track = enabled ? current() : null;
  void api
    .musicNowPlaying(
      track
        ? { id: track.id, title: track.title, artist: track.artist, playing, time: nowTime(), duration: duration || track.duration, volume: muted ? 0 : volume }
        : null,
    )
    .catch(() => undefined);
}

function saveState(): void {
  reportNow();
  const track = current();
  store(
    STATE_KEY,
    JSON.stringify({
      source: queueSource,
      track: track ?? null,
      time: Math.floor(nowTime()),
      volume,
      muted,
      shuffle,
      repeat,
    }),
  );
}

function loadState(): void {
  try {
    const saved = JSON.parse(stored(STATE_KEY) ?? "{}") as {
      source?: string;
      track?: MusicTrack | null;
      time?: number;
      volume?: number;
      muted?: boolean;
      shuffle?: boolean;
      repeat?: Repeat;
    };
    volume = typeof saved.volume === "number" ? Math.min(100, Math.max(0, saved.volume)) : 70;
    muted = Boolean(saved.muted);
    shuffle = Boolean(saved.shuffle);
    repeat = saved.repeat === "all" || saved.repeat === "one" ? saved.repeat : "off";
    if (saved.track?.id) {
      const list = library.playlists.find((playlist) => playlist.id === saved.source);
      const at = list?.tracks.findIndex((track) => track.id === saved.track!.id) ?? -1;
      if (list && at >= 0) {
        queue = [...list.tracks];
        queueSource = list.id;
        index = at;
      } else {
        queue = [saved.track];
        queueSource = "";
        index = 0;
      }
      time = saved.time ?? 0;
      duration = queue[index].duration;
    }
  } catch {
    // Start empty.
  }
}

// ---------------------------------------------------------------- YouTube player

let host: HTMLElement | null = null;
let frame: HTMLIFrameElement | null = null;
let frameReady = false;
let listenTimer = 0;
let pending: [string, unknown[]][] = [];

function post(message: Record<string, unknown>): void {
  frame?.contentWindow?.postMessage(JSON.stringify({ ...message, id: "lc", channel: "widget" }), YT);
}

function command(func: string, args: unknown[] = []): void {
  if (!frameReady) {
    // A new song replaces a song still waiting.
    if (func === "loadVideoById") pending = pending.filter(([name]) => name !== "loadVideoById");
    pending.push([func, args]);
    return;
  }
  post({ event: "command", func, args });
}

function embedUrl(id: string, start: number): string {
  const params = new URLSearchParams({
    enablejsapi: "1",
    autoplay: "1",
    controls: "0",
    disablekb: "1",
    fs: "0",
    iv_load_policy: "3",
    rel: "0",
    cc_load_policy: "0",
    playsinline: "1",
    origin: location.origin,
    widget_referrer: location.origin,
  });
  if (start > 0) params.set("start", String(Math.floor(start)));
  return `${YT}/embed/${id}?${params}`;
}

function openFrame(id: string, start: number): void {
  closeFrame();
  host = fragment(`<div class="music-host" aria-hidden="true"></div>`);
  document.body.append(host);
  frame = document.createElement("iframe");
  frame.title = "YouTube";
  frame.allow = "autoplay; encrypted-media";
  frame.referrerPolicy = "strict-origin-when-cross-origin";
  frame.src = embedUrl(id, start);
  frame.addEventListener("load", () => {
    // Ask the player to talk until it says it is ready (as YouTube's own API does).
    window.clearInterval(listenTimer);
    let tries = 0;
    listenTimer = window.setInterval(() => {
      if (frameReady || ++tries > 80) return window.clearInterval(listenTimer);
      post({ event: "listening" });
    }, 250);
    post({ event: "listening" });
  });
  host.append(frame);
  placeFrame();
}

function closeFrame(): void {
  window.clearInterval(listenTimer);
  host?.remove();
  host = null;
  frame = null;
  frameReady = false;
  pending = [];
}

/** Off-screen and tiny, or laid over the cover ("Show video"). */
function placeFrame(): void {
  if (!host) return;
  // Hidden under any dialog opened over the page.
  const covered = document.querySelectorAll("#modal-root > .overlay").length > 1;
  const slot = pageOpen && videoShown && !covered ? document.querySelector<HTMLElement>(".music-art-box") : null;
  const rect = slot?.getBoundingClientRect();
  if (rect && rect.width > 0) {
    host.classList.add("is-shown");
    Object.assign(host.style, { left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px` });
  } else {
    host.classList.remove("is-shown");
    host.removeAttribute("style");
  }
}

window.addEventListener("message", (event) => {
  if (event.origin !== YT || !frame || event.source !== frame.contentWindow) return;
  let data: { event?: string; info?: unknown };
  try {
    data = typeof event.data === "string" ? JSON.parse(event.data) : event.data;
  } catch {
    return;
  }
  if (data.event === "onReady" || (data.event === "initialDelivery" && !frameReady)) {
    frameReady = true;
    window.clearInterval(listenTimer);
    post({ event: "command", func: "setVolume", args: [volume] });
    post({ event: "command", func: muted ? "mute" : "unMute", args: [] });
    for (const [func, args] of pending) post({ event: "command", func, args });
    pending = [];
  }
  if (data.event === "infoDelivery" || data.event === "initialDelivery") {
    const info = (data.info ?? {}) as { currentTime?: number; duration?: number; playerState?: number };
    if (typeof info.currentTime === "number") {
      time = info.currentTime;
      timeAt = performance.now();
    }
    if (typeof info.duration === "number" && info.duration > 0 && Math.abs(info.duration - duration) > 0.5) {
      duration = info.duration;
      learnDuration();
    }
    if (typeof info.playerState === "number") onState(info.playerState);
    paintProgress();
  } else if (data.event === "onStateChange" && typeof data.info === "number") {
    onState(data.info);
  } else if (data.event === "onError") {
    onError(Number(data.info));
  }
});

/** -1 not started, 0 ended, 1 playing, 2 paused, 3 buffering, 5 cued. */
function onState(state: number): void {
  const wasPlaying = playing;
  if (state === 1) {
    playing = true;
    buffering = false;
    refused = 0;
  } else if (state === 3) {
    buffering = true;
  } else if (state === 2 || state === 5) {
    playing = false;
    buffering = false;
  } else if (state === 0) {
    playing = false;
    buffering = false;
    ended();
  }
  timeAt = performance.now();
  if (wasPlaying !== playing || state === 3) {
    paintControls();
    saveState();
  }
}

function onError(code: number): void {
  const track = current();
  refused++;
  // 101 / 150: the owner allows the song only on YouTube; 100: removed or private.
  toast("info", t("music.cannotPlay"), track ? track.title : "");
  if (refused >= Math.min(queue.length, 8)) {
    playing = false;
    refused = 0;
    paintControls();
    return;
  }
  void code;
  window.setTimeout(() => next(true), 900);
}

/** A song's real length: shown and saved where the list did not know it. */
function learnDuration(): void {
  const track = current();
  if (!track || !duration) return;
  const seconds = Math.round(duration);
  if (track.duration === seconds) return;
  const unknown = track.duration === 0;
  track.duration = seconds;
  if (unknown) {
    void api.musicSetDuration(track.id, seconds).catch(() => undefined);
    for (const playlist of library.playlists) for (const entry of playlist.tracks) if (entry.id === track.id && !entry.duration) entry.duration = seconds;
    if (pageOpen) renderMain();
  }
}

// ---------------------------------------------------------------- playback

function playAt(position: number, start = 0): void {
  const track = queue[position];
  if (!track) return;
  if (index >= 0 && index !== position) history.push(index);
  if (history.length > 200) history.shift();
  index = position;
  bag = bag.filter((entry) => entry !== position);
  time = start;
  timeAt = performance.now();
  duration = track.duration;
  playing = true;
  buffering = true;
  if (!frame) openFrame(track.id, start);
  else if (!frameReady) frame.src = embedUrl(track.id, start);
  else command("loadVideoById", [{ videoId: track.id, startSeconds: Math.floor(start) }]);
  saveState();
  paintAll();
}

/** Play a list (a playlist's songs or search results) from a song. */
export function playList(tracks: MusicTrack[], source: string, from: number, shuffled = false): void {
  if (!tracks.length) return;
  queue = [...tracks];
  queueSource = source;
  history = [];
  index = -1;
  if (shuffled) shuffle = true;
  refill(from);
  const start = shuffled ? takeRandom() : from;
  playAt(start ?? from);
}

function refill(except = -1): void {
  bag = queue.map((_, position) => position).filter((position) => position !== except);
}

function takeRandom(): number | undefined {
  if (!bag.length) return undefined;
  const pick = bag[Math.floor(Math.random() * bag.length)];
  bag = bag.filter((entry) => entry !== pick);
  return pick;
}

export function toggle(): void {
  const track = current();
  if (!track) return;
  if (!frame) return playAt(index, time);
  if (playing) {
    command("pauseVideo");
    playing = false;
  } else {
    command("playVideo");
    playing = true;
  }
  timeAt = performance.now();
  paintControls();
  saveState();
}

/** Next song; `auto` = the last one ended or could not play. */
export function next(auto = false): void {
  if (!queue.length) return;
  if (auto && repeat === "one" && current()) return playAt(index, 0);
  let target: number | undefined;
  if (shuffle) {
    target = takeRandom();
    if (target === undefined && repeat !== "off") {
      refill(index);
      target = takeRandom();
    }
  } else {
    target = index + 1 < queue.length ? index + 1 : repeat !== "off" || !auto ? 0 : undefined;
  }
  if (target === undefined) {
    playing = false;
    paintControls();
    return;
  }
  playAt(target);
}

export function previous(): void {
  if (!queue.length) return;
  if (nowTime() > 3) return seek(0);
  const back = history.pop();
  if (back !== undefined) {
    index = -1;
    playAt(back);
    history.pop();
    return;
  }
  playAt(index > 0 ? index - 1 : queue.length - 1);
}

function ended(): void {
  next(true);
}

function seek(seconds: number): void {
  time = Math.max(0, seconds);
  timeAt = performance.now();
  if (frame) command("seekTo", [time, true]);
  paintProgress();
  reportNow();
}

function setVolume(value: number): void {
  volume = Math.min(100, Math.max(0, Math.round(value)));
  if (volume > 0 && muted) {
    muted = false;
    command("unMute");
  }
  command("setVolume", [volume]);
  saveState();
}

function toggleMute(): void {
  muted = !muted;
  command(muted ? "mute" : "unMute");
  paintControls();
  saveState();
}

// ---------------------------------------------------------------- sound wave

let levels: number[] = new Array(48).fill(0);
let shown: number[] = new Array(48).fill(0);
let waveOn = false;
let lastWave = 0;
let frameLoop = 0;

function wantWave(): boolean {
  return pageOpen && windowVisible && document.visibilityState === "visible" && playing;
}

function syncWave(): void {
  const want = wantWave();
  if (want !== waveOn) {
    waveOn = want;
    void api.musicWave(want).catch(() => undefined);
  }
  if (!want) levels = levels.map(() => 0);
  startLoop();
}

/** One animation loop for the page (progress + wave), only while it is shown. */
function startLoop(): void {
  if (frameLoop || !pageOpen || !windowVisible) return;
  const step = () => {
    frameLoop = 0;
    if (!pageOpen || !windowVisible || document.visibilityState !== "visible") return;
    paintProgress();
    const moving = drawWave();
    if (playing || moving) frameLoop = requestAnimationFrame(step);
  };
  frameLoop = requestAnimationFrame(step);
}

function drawWave(): boolean {
  const canvas = document.querySelector<HTMLCanvasElement>(".music-wave");
  if (!canvas) return false;
  // No fresh levels for a while (no sound device): settle.
  if (performance.now() - lastWave > 400) levels = levels.map((level) => level * 0.9);
  const style = getComputedStyle(canvas);
  return drawBars(canvas, shown, levels, style.getPropertyValue("--music-tint").trim() || "10 132 255", style.getPropertyValue("--accent-rgb").trim() || "10 132 255");
}

// ---------------------------------------------------------------- cover colour

const tints = new Map<string, string>();

/** The cover's main colour, for the glow and the wave. */
function readTint(image: HTMLImageElement, id: string): void {
  const tint = tints.get(id) ?? coverTint(image);
  if (!tint) return;
  tints.set(id, tint);
  applyTint(tint);
}

function applyTint(tint: string): void {
  document.querySelectorAll<HTMLElement>(".music-overlay, .music-mini").forEach((element) => element.style.setProperty("--music-tint", tint));
}

// ---------------------------------------------------------------- page

let overlay: HTMLElement | null = null;
/** Main area: a playlist id or "search". */
let view = stored("musicView") ?? "search";
let searchText = "";
let results: MusicTrack[] = [];
let searching = false;

const playlistById = (id: string): MusicPlaylist | undefined => library.playlists.find((playlist) => playlist.id === id);

function trackRow(track: MusicTrack, position: number, source: string, editable: boolean, count: number): string {
  const isCurrent = queueSource === source && current()?.id === track.id && index === position;
  return `
    <div class="music-row${isCurrent ? " is-current" : ""}" data-row="${position}" role="button" tabindex="0">
      <span class="music-row-index">${isCurrent ? `<span class="music-eq${playing ? " is-playing" : ""}" aria-hidden="true"><i></i><i></i><i></i></span>` : position + 1}</span>
      <img class="music-row-cover" src="${esc(musicCover(track.id, "lo"))}" alt="" loading="lazy" decoding="async"/>
      <span class="music-row-text"><strong>${esc(track.title)}</strong><span>${esc(track.artist)}</span></span>
      <span class="music-row-length">${track.duration ? clockText(track.duration) : ""}</span>
      <span class="music-row-actions">
        ${
          editable
            ? `<button type="button" class="icon-btn" data-row-action="up" data-tooltip="${esc(t("music.moveUp"))}" aria-label="${esc(t("music.moveUp"))}"${position === 0 ? " disabled" : ""}>${icon("arrow-up", 14)}</button>
               <button type="button" class="icon-btn" data-row-action="down" data-tooltip="${esc(t("music.moveDown"))}" aria-label="${esc(t("music.moveDown"))}"${position === count - 1 ? " disabled" : ""}>${icon("arrow-down", 14)}</button>
               <button type="button" class="icon-btn" data-row-action="remove" data-tooltip="${esc(t("music.remove"))}" aria-label="${esc(t("music.remove"))}">${icon("trash", 14)}</button>`
            : `<button type="button" class="icon-btn" data-row-action="add" data-tooltip="${esc(t("music.addTo"))}" aria-label="${esc(t("music.addTo"))}">${icon("list-plus", 15)}</button>`
        }
      </span>
    </div>`;
}

function renderNav(): void {
  const nav = overlay?.querySelector<HTMLElement>(".music-nav");
  if (!nav) return;
  nav.innerHTML = `
    <button type="button" class="music-nav-row" data-view="search" aria-selected="${view === "search"}">${icon("search", 15)}<span>${esc(t("music.search"))}</span></button>
    <div class="music-nav-title"><span>${esc(t("music.playlists"))}</span>
      <button type="button" class="icon-btn" data-music="new" data-tooltip="${esc(t("music.newPlaylist"))}" aria-label="${esc(t("music.newPlaylist"))}">${icon("plus", 14)}</button>
    </div>
    <div class="music-nav-list">
      ${library.playlists
        .map(
          (playlist) => `
        <button type="button" class="music-nav-row" data-view="${esc(playlist.id)}" aria-selected="${view === playlist.id}">
          ${playlist.tracks[0] ? `<img src="${esc(musicCover(playlist.tracks[0].id, "lo"))}" alt="" loading="lazy"/>` : `<span class="music-nav-blank">${icon("list-music", 14)}</span>`}
          <span>${esc(playlist.name)}</span>
          ${queueSource === playlist.id && playing ? `<span class="music-eq is-playing" aria-hidden="true"><i></i><i></i><i></i></span>` : `<em>${playlist.tracks.length}</em>`}
        </button>`,
        )
        .join("")}
    </div>
    <button type="button" class="btn btn-ghost music-import" data-music="import">${icon("download", 14)}<span>${esc(t("music.import"))}</span></button>`;
}

function renderMain(): void {
  const main = overlay?.querySelector<HTMLElement>(".music-main");
  if (!main) return;
  const scroll = main.scrollTop;
  const playlist = playlistById(view);
  if (!playlist) {
    if (view !== "search") view = "search";
    main.innerHTML = `
      <div class="music-search">
        <label class="search music-search-field">
          ${icon("search", 16)}
          <input type="text" name="musicSearch" placeholder="${esc(t("music.searchPlaceholder"))}" autocomplete="off" spellcheck="false" value="${esc(searchText)}"/>
        </label>
      </div>
      <div class="music-list">
        ${
          searching
            ? `<p class="music-hint">${esc(t("music.searching"))}</p>`
            : results.length
              ? results.map((track, position) => trackRow(track, position, "", false, results.length)).join("")
              : searchText
                ? `<p class="music-hint">${esc(t("music.noResults"))}</p>`
                : library.playlists.length
                  ? `<div class="music-search-idle">${icon("audio-lines", 34)}<p>${esc(t("music.welcomeHint"))}</p></div>`
                  : `<div class="music-welcome">
                       <span class="music-welcome-icon">${icon("music", 26)}</span>
                       <h3>${esc(t("music.welcome"))}</h3>
                       <p>${esc(t("music.welcomeHint"))}</p>
                       <div class="music-welcome-actions">
                         <button type="button" class="btn btn-primary" data-music="new">${icon("plus", 14)}<span>${esc(t("music.newPlaylist"))}</span></button>
                         <button type="button" class="btn btn-ghost" data-music="import">${icon("download", 14)}<span>${esc(t("music.import"))}</span></button>
                       </div>
                     </div>`
        }
      </div>`;
    return;
  }
  const total = playlist.tracks.reduce((sum, track) => sum + track.duration, 0);
  const covers = playlist.tracks.slice(0, 4);
  main.innerHTML = `
    <header class="music-head">
      <div class="music-head-cover${covers.length >= 4 ? " is-grid" : ""}">
        ${covers.length ? (covers.length >= 4 ? covers : covers.slice(0, 1)).map((track) => `<img src="${esc(musicCover(track.id, covers.length >= 4 ? "lo" : "hi"))}" alt=""/>`).join("") : icon("list-music", 34)}
      </div>
      <div class="music-head-text">
        <span class="music-head-kind">${esc(playlist.source ? t("music.fromYoutube") : t("music.playlist"))}</span>
        <h3>${esc(playlist.name)}</h3>
        <span class="music-head-sub">${esc(songsText(playlist.tracks.length))}${total ? ` · ${esc(lengthText(total))}` : ""}</span>
        <div class="music-head-actions">
          <button type="button" class="btn btn-primary" data-music="play-all"${playlist.tracks.length ? "" : " disabled"}>${icon("play", 14)}<span>${esc(t("music.play"))}</span></button>
          <button type="button" class="btn" data-music="shuffle-all"${playlist.tracks.length ? "" : " disabled"}>${icon("shuffle", 14)}<span>${esc(t("music.shuffle"))}</span></button>
          <button type="button" class="btn btn-ghost" data-music="add">${icon("plus", 14)}<span>${esc(t("music.addSongs"))}</span></button>
          <button type="button" class="icon-btn music-more" data-music="more" aria-label="${esc(t("music.more"))}" data-tooltip="${esc(t("music.more"))}">${icon("more", 16)}</button>
        </div>
      </div>
    </header>
    <div class="music-list">
      ${
        playlist.tracks.length
          ? playlist.tracks.map((track, position) => trackRow(track, position, playlist.id, true, playlist.tracks.length)).join("")
          : `<div class="music-empty"><strong>${esc(t("music.empty"))}</strong><span>${esc(t("music.emptyHint"))}</span></div>`
      }
    </div>`;
  main.scrollTop = scroll;
}

function renderNow(): void {
  const now = overlay?.querySelector<HTMLElement>(".music-now");
  if (!now) return;
  const track = current();
  if (!track) {
    now.innerHTML = `
      <div class="music-art-box is-empty">${icon("music", 40)}</div>
      <div class="music-meta"><strong>${esc(t("music.nothing"))}</strong><span>${esc(t("music.nothingHint"))}</span></div>`;
    return;
  }
  now.innerHTML = `
    <div class="music-glow" style="background-image:url('${esc(musicCover(track.id, "hi"))}')" aria-hidden="true"></div>
    <div class="music-art-box">
      <img class="music-art" src="${esc(musicCover(track.id, "hi"))}" alt=""${"__TAURI_INTERNALS__" in window ? ' crossorigin="anonymous"' : ""} decoding="async"/>
    </div>
    <div class="music-meta"><strong title="${esc(track.title)}">${esc(track.title)}</strong><span>${esc(track.artist)}</span></div>
    <canvas class="music-wave" aria-hidden="true"></canvas>
    <div class="music-progress">
      <span class="music-time" data-time>0:00</span>
      <input type="range" class="music-seek" min="0" max="1000" step="1" value="0" aria-label="${esc(t("music.seek"))}"/>
      <span class="music-time" data-length>${clockText(duration)}</span>
    </div>
    <div class="music-controls">
      <button type="button" class="icon-btn music-ctl" data-music="shuffle" aria-pressed="${shuffle}" data-tooltip="${esc(t("music.shuffle"))}" aria-label="${esc(t("music.shuffle"))}">${icon("shuffle", 17)}</button>
      <button type="button" class="icon-btn music-ctl" data-music="previous" data-tooltip="${esc(t("music.previous"))}" aria-label="${esc(t("music.previous"))}">${icon("skip-back", 19)}</button>
      <button type="button" class="music-play" data-music="toggle" aria-label="${esc(playing ? t("music.pause") : t("music.play"))}">${icon(playing ? "pause" : "play", 22)}</button>
      <button type="button" class="icon-btn music-ctl" data-music="next" data-tooltip="${esc(t("music.next"))}" aria-label="${esc(t("music.next"))}">${icon("skip-forward", 19)}</button>
      <button type="button" class="icon-btn music-ctl" data-music="repeat" aria-pressed="${repeat !== "off"}" data-tooltip="${esc(repeatLabel())}" aria-label="${esc(repeatLabel())}">${icon(repeat === "one" ? "repeat-1" : "repeat", 17)}</button>
    </div>
    <div class="music-extra">
      <button type="button" class="icon-btn" data-music="mute" data-tooltip="${esc(muted ? t("music.unmute") : t("music.mute"))}" aria-label="${esc(muted ? t("music.unmute") : t("music.mute"))}">${icon(muted || volume === 0 ? "volume-x" : "volume", 16)}</button>
      <input type="range" class="music-volume" min="0" max="100" step="1" value="${muted ? 0 : volume}" style="--fill:${muted ? 0 : volume}%" aria-label="${esc(t("music.volume"))}"/>
      <button type="button" class="btn btn-ghost music-video-btn" data-music="video" aria-pressed="${videoShown}">${icon("monitor-play", 14)}<span>${esc(videoShown ? t("music.hideVideo") : t("music.showVideo"))}</span></button>
    </div>`;
  const art = now.querySelector<HTMLImageElement>(".music-art");
  if (art) {
    const read = () => readTint(art, track.id);
    if (art.complete && art.naturalWidth) read();
    else art.addEventListener("load", read, { once: true });
  }
  paintProgress();
  requestAnimationFrame(placeFrame);
}

const repeatLabel = (): string => (repeat === "one" ? t("music.repeatOne") : repeat === "all" ? t("music.repeatAll") : t("music.repeatOff"));

/** Seek bar and times (cheap: called every frame while shown). */
function paintProgress(): void {
  const value = nowTime();
  if (overlay) {
    const seekBar = overlay.querySelector<HTMLInputElement>(".music-seek");
    if (seekBar && !seekBar.matches(":active")) {
      seekBar.value = String(duration ? Math.round((value / duration) * 1000) : 0);
      seekBar.style.setProperty("--fill", `${duration ? (value / duration) * 100 : 0}%`);
    }
    const timeLabel = overlay.querySelector<HTMLElement>("[data-time]");
    if (timeLabel) timeLabel.textContent = clockText(value);
    const lengthLabel = overlay.querySelector<HTMLElement>("[data-length]");
    if (lengthLabel) lengthLabel.textContent = clockText(duration);
  }
  const bar = mini?.querySelector<HTMLElement>(".music-mini-bar span");
  if (bar) bar.style.width = `${duration ? Math.min(100, (value / duration) * 100) : 0}%`;
}

/** Play/pause state everywhere (page, rows, nav, mini player). */
function paintControls(): void {
  if (overlay) {
    const play = overlay.querySelector<HTMLElement>(".music-play");
    if (play) {
      play.innerHTML = icon(playing ? "pause" : "play", 22);
      play.setAttribute("aria-label", playing ? t("music.pause") : t("music.play"));
    }
    overlay.querySelectorAll(".music-eq").forEach((bars) => bars.classList.toggle("is-playing", playing));
    const mute = overlay.querySelector<HTMLElement>('[data-music="mute"]');
    if (mute) mute.innerHTML = icon(muted || volume === 0 ? "volume-x" : "volume", 16);
    const volumeBar = overlay.querySelector<HTMLInputElement>(".music-volume");
    if (volumeBar) {
      volumeBar.value = String(muted ? 0 : volume);
      volumeBar.style.setProperty("--fill", `${muted ? 0 : volume}%`);
    }
    renderNav();
  }
  paintMini();
  syncWave();
}

function paintAll(): void {
  if (overlay) {
    renderNav();
    renderMain();
    renderNow();
  }
  paintMini();
  syncWave();
}

async function refreshLibrary(next?: MusicLibrary): Promise<void> {
  library = next ?? (await api.musicLibrary());
  if (pageOpen) {
    renderNav();
    renderMain();
  }
}

async function ask(title: string, label: string, value: string, action: string, hint = "", multiline = false): Promise<string | null> {
  let text: string | null = null;
  const answer = await modal({
    title,
    body: `${hint ? `<p class="hint">${esc(hint)}</p>` : ""}
      <label class="field"><span>${esc(label)}</span>${
        multiline
          ? `<textarea name="musicAsk" rows="6" spellcheck="false" autocomplete="off">${esc(value)}</textarea>`
          : `<input name="musicAsk" value="${esc(value)}" maxlength="400" spellcheck="false" autocomplete="off"/>`
      }</label>`,
    actions: [
      { label: t("common.cancel"), value: "cancel" },
      { label: action, value: "ok", kind: "primary" },
    ],
    onOpen: (root) => {
      const field = root.querySelector<HTMLInputElement | HTMLTextAreaElement>('[name="musicAsk"]');
      field?.focus();
      field?.select();
    },
    onSubmit: (choice, root) => {
      if (choice !== "ok") return true;
      text = root.querySelector<HTMLInputElement | HTMLTextAreaElement>('[name="musicAsk"]')?.value.trim() ?? "";
      return text.length > 0;
    },
  });
  return answer === "ok" ? text : null;
}

/** Long work with a busy button: errors become a toast. */
async function busy<T>(button: HTMLElement | null, work: () => Promise<T>, failTitle: string): Promise<T | null> {
  button?.classList.add("is-busy");
  button?.setAttribute("disabled", "");
  try {
    return await work();
  } catch (error) {
    toast("error", failTitle, describe(error));
    return null;
  } finally {
    button?.classList.remove("is-busy");
    button?.removeAttribute("disabled");
  }
}

async function newPlaylist(): Promise<void> {
  const name = await ask(t("music.newPlaylist"), t("music.playlistName"), "", t("music.create"));
  if (!name) return;
  const next = await busy(null, () => api.musicCreatePlaylist(name), t("music.newPlaylist"));
  if (!next) return;
  view = next.playlists[next.playlists.length - 1]?.id ?? view;
  store("musicView", view);
  await refreshLibrary(next);
}

async function importPlaylist(): Promise<void> {
  const link = await ask(t("music.importTitle"), t("music.playlistLink"), "", t("music.importButton"), t("music.importHint"));
  if (!link) return;
  toast("info", t("music.importing"));
  const next = await busy(null, () => api.musicImportPlaylist(link), t("music.importTitle"));
  if (!next) return;
  const made = next.playlists[next.playlists.length - 1];
  view = made?.id ?? view;
  store("musicView", view);
  await refreshLibrary(next);
  if (made) toast("success", made.name, songsText(made.tracks.length));
}

async function addSongs(playlist: MusicPlaylist, button: HTMLElement | null): Promise<void> {
  const input = await ask(t("music.addTitle", { name: playlist.name }), t("music.links"), "", t("music.addButton"), t("music.addHint"), true);
  if (!input) return;
  const result = await busy(button, () => api.musicAddLinks(playlist.id, input), t("music.addSongs"));
  if (!result) return;
  await refreshLibrary(result.library);
  toast("success", t("music.added", { count: result.added }));
}

function addMenu(track: MusicTrack, x: number, y: number): void {
  contextMenu(x, y, [
    ...library.playlists.map((playlist) => ({
      icon: "list-music",
      label: playlist.name,
      run: () =>
        void busy(null, () => api.musicAddTracks(playlist.id, [track]), t("music.addTo")).then((next) => {
          if (!next) return;
          void refreshLibrary(next);
          toast("success", t("music.addedTo", { name: playlist.name }));
        }),
    })),
    {
      icon: "plus",
      label: t("music.newPlaylist"),
      run: () =>
        void (async () => {
          const name = await ask(t("music.newPlaylist"), t("music.playlistName"), "", t("music.create"));
          if (!name) return;
          const made = await busy(null, () => api.musicCreatePlaylist(name), t("music.newPlaylist"));
          const playlist = made?.playlists[made.playlists.length - 1];
          if (!playlist) return;
          const next = await busy(null, () => api.musicAddTracks(playlist.id, [track]), t("music.addTo"));
          if (next) {
            await refreshLibrary(next);
            toast("success", t("music.addedTo", { name: playlist.name }));
          }
        })(),
    },
  ]);
}

function moreMenu(playlist: MusicPlaylist, x: number, y: number): void {
  contextMenu(x, y, [
    {
      icon: "pencil",
      label: t("music.rename"),
      run: () =>
        void (async () => {
          const name = await ask(t("music.renameTitle"), t("music.playlistName"), playlist.name, t("common.save"));
          if (!name) return;
          const next = await busy(null, () => api.musicRenamePlaylist(playlist.id, name), t("music.rename"));
          if (next) await refreshLibrary(next);
        })(),
    },
    ...(playlist.source
      ? [
          {
            icon: "refresh-cw",
            label: t("music.sync"),
            run: () =>
              void busy(null, () => api.musicSyncPlaylist(playlist.id), t("music.sync")).then((next) => {
                if (!next) return;
                void refreshLibrary(next);
                toast("success", t("music.synced"));
              }),
          },
        ]
      : []),
    {
      icon: "trash",
      label: t("music.delete"),
      danger: true,
      run: () =>
        void (async () => {
          const answer = await modal({
            title: t("music.delete"),
            body: `<p class="hint">${esc(t("music.deleteText", { name: playlist.name }))}</p>`,
            actions: [
              { label: t("common.cancel"), value: "cancel" },
              { label: t("music.delete"), value: "delete", kind: "danger" },
            ],
          });
          if (answer !== "delete") return;
          const next = await busy(null, () => api.musicDeletePlaylist(playlist.id), t("music.delete"));
          if (!next) return;
          view = "search";
          store("musicView", view);
          await refreshLibrary(next);
        })(),
    },
  ]);
}

async function runSearch(): Promise<void> {
  const query = searchText.trim();
  if (!query) {
    results = [];
    return renderMain();
  }
  searching = true;
  renderMain();
  try {
    results = await api.musicSearch(query);
  } catch (error) {
    results = [];
    toast("error", t("music.search"), describe(error));
  }
  searching = false;
  renderMain();
  overlay?.querySelector<HTMLInputElement>('[name="musicSearch"]')?.focus();
}

export async function openMusic(): Promise<void> {
  if (!enabled || overlay) return;
  try {
    await refreshLibrary();
  } catch (error) {
    toast("error", t("music.title"), describe(error));
    return;
  }
  overlay = fragment(`
    <div class="overlay music-overlay">
      <div class="modal music-modal" role="dialog" aria-modal="true" aria-label="${esc(t("music.title"))}">
        <header class="modal-head"><h2>${esc(t("music.title"))}</h2></header>
        <div class="modal-close-mount">
          <button class="modal-close" data-music="close" aria-label="${esc(t("common.close"))}"><span></span></button>
        </div>
        <div class="music-layout">
          <nav class="music-nav" aria-label="${esc(t("music.playlists"))}"></nav>
          <section class="music-main"></section>
          <aside class="music-now"></aside>
        </div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  $("#modal-root").append(overlay);
  pageOpen = true;
  const tint = tints.get(current()?.id ?? "");
  if (tint) applyTint(tint);
  paintAll();
  const page = overlay;

  const close = () => {
    pageOpen = false;
    window.removeEventListener("keydown", keys, true);
    window.removeEventListener("resize", placeFrame);
    dialogs.disconnect();
    cancelAnimationFrame(frameLoop);
    frameLoop = 0;
    page.remove();
    overlay = null;
    placeFrame();
    syncWave();
    previousFocus?.focus?.();
  };
  const typing = (target: EventTarget | null) => target instanceof HTMLInputElement && target.type === "text" || target instanceof HTMLTextAreaElement;
  const keys = (event: KeyboardEvent) => {
    if (!page.isConnected || document.querySelector(".overlay:not(.music-overlay)")) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      close();
    } else if (event.key === " " && !typing(event.target) && !(event.target instanceof HTMLButtonElement)) {
      event.preventDefault();
      toggle();
    }
  };
  window.addEventListener("keydown", keys, true);
  window.addEventListener("resize", placeFrame);
  page.querySelector(".music-now")?.addEventListener("scroll", placeFrame, { passive: true });
  const dialogs = new MutationObserver(placeFrame);
  dialogs.observe($("#modal-root"), { childList: true });
  page.addEventListener("mousedown", (event) => {
    if (event.target === page) close();
  });

  page.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const navRow = target.closest<HTMLElement>("[data-view]");
    if (navRow) {
      view = navRow.dataset.view ?? "search";
      store("musicView", view);
      renderNav();
      renderMain();
      page.querySelector<HTMLElement>(".music-main")!.scrollTop = 0;
      if (view === "search") page.querySelector<HTMLInputElement>('[name="musicSearch"]')?.focus();
      return;
    }
    const playlist = playlistById(view);
    const rowAction = target.closest<HTMLElement>("[data-row-action]");
    const row = target.closest<HTMLElement>("[data-row]");
    if (rowAction && row) {
      const position = Number(row.dataset.row);
      const action = rowAction.dataset.rowAction;
      if (action === "add") {
        const rect = rowAction.getBoundingClientRect();
        const track = results[position];
        if (track) addMenu(track, rect.left, rect.bottom + 4);
      } else if (playlist && action === "remove") {
        void busy(null, () => api.musicRemoveTrack(playlist.id, position), t("music.remove")).then((next) => next && refreshLibrary(next));
      } else if (playlist && (action === "up" || action === "down")) {
        const to = action === "up" ? position - 1 : position + 1;
        void busy(null, () => api.musicMoveTrack(playlist.id, position, to), t("music.moveUp")).then((next) => next && refreshLibrary(next));
      }
      return;
    }
    if (row) {
      const position = Number(row.dataset.row);
      if (playlist) {
        if (queueSource === playlist.id && index === position && current()?.id === playlist.tracks[position]?.id) return toggle();
        playList(playlist.tracks, playlist.id, position);
      } else if (results[position]) {
        playList([results[position]], "", 0);
      }
      return;
    }
    const button = target.closest<HTMLElement>("[data-music]");
    if (!button) return;
    const action = button.dataset.music;
    if (action === "close") close();
    if (action === "new") void newPlaylist();
    if (action === "import") void importPlaylist();
    if (action === "toggle") toggle();
    if (action === "next") next();
    if (action === "previous") previous();
    if (action === "mute") toggleMute();
    if (action === "shuffle") {
      shuffle = !shuffle;
      if (shuffle) refill(index);
      button.setAttribute("aria-pressed", String(shuffle));
      saveState();
    }
    if (action === "repeat") {
      repeat = repeat === "off" ? "all" : repeat === "all" ? "one" : "off";
      saveState();
      renderNow();
      // The three states look alike at a glance: say which one is on.
      toast("info", repeatLabel(), "", 1800);
    }
    if (action === "video") {
      videoShown = !videoShown;
      renderNow();
    }
    if (playlist && action === "play-all") playList(playlist.tracks, playlist.id, 0);
    if (playlist && action === "shuffle-all") playList(playlist.tracks, playlist.id, 0, true);
    if (playlist && action === "add") void addSongs(playlist, button);
    if (playlist && action === "more") {
      const rect = button.getBoundingClientRect();
      moreMenu(playlist, rect.left, rect.bottom + 4);
    }
  });
  page.addEventListener("keydown", (event) => {
    const target = event.target as HTMLElement;
    if (event.key === "Enter" && target instanceof HTMLInputElement && target.name === "musicSearch") {
      event.preventDefault();
      searchText = target.value;
      void runSearch();
    } else if (event.key === "Enter" && target.matches(".music-row")) {
      target.click();
    }
  });
  page.addEventListener("input", (event) => {
    const target = event.target as HTMLInputElement;
    if (target.name === "musicSearch") searchText = target.value;
    if (target.classList.contains("music-seek")) {
      target.style.setProperty("--fill", `${Number(target.value) / 10}%`);
      const label = page.querySelector<HTMLElement>("[data-time]");
      if (label) label.textContent = clockText((Number(target.value) / 1000) * duration);
    }
    if (target.classList.contains("music-volume")) {
      target.style.setProperty("--fill", `${target.value}%`);
      setVolume(Number(target.value));
      const mute = page.querySelector<HTMLElement>('[data-music="mute"]');
      if (mute) mute.innerHTML = icon(volume === 0 ? "volume-x" : "volume", 16);
    }
  });
  page.addEventListener("change", (event) => {
    const target = event.target as HTMLInputElement;
    if (target.classList.contains("music-seek") && duration) seek((Number(target.value) / 1000) * duration);
  });
  if (view === "search") page.querySelector<HTMLInputElement>('[name="musicSearch"]')?.focus();
  else page.querySelector<HTMLElement>(".music-modal")?.focus();
}

// ---------------------------------------------------------------- mini player (sidebar)

let mini: HTMLElement | null = null;

function paintMini(): void {
  if (!mini) return;
  const track = current();
  mini.hidden = !enabled || !track;
  if (!track) return;
  const art = mini.querySelector<HTMLImageElement>(".music-mini-art")!;
  const src = musicCover(track.id, "lo");
  if (art.getAttribute("src") !== src) art.src = src;
  mini.querySelector<HTMLElement>(".music-mini-text strong")!.textContent = track.title;
  mini.querySelector<HTMLElement>(".music-mini-text span")!.textContent = track.artist;
  const play = mini.querySelector<HTMLElement>('[data-mini="toggle"]')!;
  play.innerHTML = icon(playing ? "pause" : "play", 15);
  play.setAttribute("aria-label", playing ? t("music.pause") : t("music.play"));
  mini.classList.toggle("is-playing", playing);
  paintProgress();
}

function mountMini(): void {
  const bottom = document.querySelector<HTMLElement>(".side-bottom");
  if (!bottom || mini) return;
  mini = fragment(`
    <div class="music-mini" hidden>
      <button type="button" class="music-mini-open" data-mini="open" aria-label="${esc(t("music.open"))}">
        <img class="music-mini-art" alt=""/>
        <span class="music-mini-text"><strong></strong><span></span></span>
      </button>
      <button type="button" class="music-mini-btn" data-mini="toggle"></button>
      <button type="button" class="music-mini-btn" data-mini="next" aria-label="${esc(t("music.next"))}">${icon("skip-forward", 15)}</button>
      <div class="music-mini-bar"><span></span></div>
    </div>`);
  bottom.prepend(mini);
  mini.addEventListener("click", (event) => {
    const action = (event.target as HTMLElement).closest<HTMLElement>("[data-mini]")?.dataset.mini;
    if (action === "open") void openMusic();
    if (action === "toggle") toggle();
    if (action === "next") next();
  });
}

// ---------------------------------------------------------------- start / settings

function showTile(): void {
  const tile = document.querySelector<HTMLElement>("#music-tile");
  if (tile) tile.hidden = !enabled;
}

export const musicOn = (): boolean => enabled;

/** At start-up: nothing loads or plays until the player opens a song. */
export async function initMusic(): Promise<void> {
  mountMini();
  void events.onWindowVisible((visible) => {
    windowVisible = visible;
    if (visible) startLoop();
    else {
      cancelAnimationFrame(frameLoop);
      frameLoop = 0;
    }
    syncWave();
  });
  document.addEventListener("visibilitychange", syncWave);
  void events.onMusicWave((next) => {
    levels = compensate(next, muted ? 0 : volume);
    lastWave = performance.now();
  });
  void events.onMusicControl((action) => {
    if (!enabled) return;
    if (action === "toggle") toggle();
    if (action === "next") next();
    if (action === "previous") previous();
    // The tray player's volume slider and mute button.
    if (action === "mute") toggleMute();
    if (action.startsWith("volume:")) {
      setVolume(Number(action.slice(7)));
      paintControls();
    }
    if (action === "mute" || action.startsWith("volume:")) reportNow();
  });
  // Saved progress every 10 s while playing (cheap, survives a crash).
  window.setInterval(() => playing && saveState(), 10_000);
  try {
    const prefs = await api.musicPrefs();
    enabled = prefs.enabled;
    speed = prefs.speed ?? 256;
  } catch {
    enabled = false;
  }
  showTile();
  if (!enabled) return;
  try {
    library = await api.musicLibrary();
  } catch {
    library = { playlists: [] };
  }
  loadState();
  paintMini();
  reportNow();
}

/** Settings › Music. Turning it off stops the music and hides everything. */
export async function setMusicEnabled(on: boolean, nextSpeed = speed): Promise<void> {
  if (on === enabled && nextSpeed === speed) return;
  const saved = await api.setMusicPrefs({ enabled: on, speed: nextSpeed });
  speed = saved.speed;
  if (saved.enabled === enabled) return;
  enabled = saved.enabled;
  showTile();
  if (!enabled) {
    closeFrame();
    playing = false;
    if (overlay) overlay.querySelector<HTMLElement>('[data-music="close"]')?.click();
  } else {
    library = await api.musicLibrary().catch(() => ({ playlists: [] }));
    loadState();
  }
  paintMini();
  syncWave();
  reportNow();
}
