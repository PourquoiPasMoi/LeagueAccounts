// Tray player: the small window Rust opens on a right-click of the tray icon
// while a song is loaded (src-tauri/src/music.rs `open_tray_player`). It is
// created on that click and destroyed when it loses focus, so its frame loop
// and the wave meter only run while it is on screen.

// Its own copy of the design tokens: importing league.css here made Vite move it
// into a chunk shared with the main window, loaded after polish.css (old look came back).
import "./tray-base.css";
import "./tray.css";
import { api, events, musicCover, type MusicNow } from "./api";
import { icon } from "./icons";
import { t } from "./i18n";
import { compensate, coverTint, drawBars } from "./wave";

const $ = <T extends HTMLElement>(selector: string) => document.querySelector<T>(selector)!;

let now: MusicNow | null = null;
let levels: number[] = new Array(48).fill(0);
const shown: number[] = new Array(48).fill(0);
let lastWave = 0;
let tint = "10 132 255";
const accent = getComputedStyle(document.documentElement).getPropertyValue("--accent-rgb").trim() || "10 132 255";

const clockText = (seconds: number): string => {
  const total = Math.max(0, Math.floor(seconds || 0));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
};

const position = (): number => {
  if (!now) return 0;
  const passed = now.playing && now.at ? (Date.now() - now.at) / 1000 : 0;
  return Math.min(now.duration || Infinity, now.time + passed);
};

function label(button: HTMLElement, name: string, text: string, size: number): void {
  button.innerHTML = icon(name, size);
  button.setAttribute("aria-label", text);
  button.title = text;
}

function paint(): void {
  if (!now) {
    void api.musicTray("close");
    return;
  }
  const cover = $<HTMLImageElement>(".tp-cover");
  const src = musicCover(now.id, "hi");
  if (cover.getAttribute("src") !== src) {
    cover.crossOrigin = "anonymous";
    cover.src = src;
    $(".tp-glow").style.backgroundImage = `url('${src}')`;
  }
  $(".tp-text strong").textContent = now.title;
  $(".tp-text strong").title = now.title;
  $(".tp-text span").textContent = now.artist;
  label($('[data-tray="toggle"]'), now.playing ? "pause" : "play", now.playing ? t("music.pause") : t("music.play"), 20);
  document.body.classList.toggle("is-playing", now.playing);
}

$<HTMLImageElement>(".tp-cover").addEventListener("load", (event) => {
  const read = coverTint(event.target as HTMLImageElement);
  if (read) {
    tint = read;
    document.body.style.setProperty("--music-tint", tint);
  }
});

function frame(): void {
  const value = position();
  const length = now?.duration ?? 0;
  $("[data-time]").textContent = clockText(value);
  $("[data-length]").textContent = clockText(length);
  $(".tp-bar i").style.width = `${length ? Math.min(100, (value / length) * 100) : 0}%`;
  if (performance.now() - lastWave > 400) levels = levels.map((level) => level * 0.9);
  drawBars($<HTMLCanvasElement>(".tp-wave"), shown, levels, tint, accent);
  requestAnimationFrame(frame);
}

label($('[data-tray="previous"]'), "skip-back", t("music.previous"), 18);
label($('[data-tray="next"]'), "skip-forward", t("music.next"), 18);
label($('[data-tray="open"]'), "maximize", t("music.openApp"), 14);
label($('[data-tray="quit"]'), "x", t("music.quitApp"), 14);

document.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-tray]")?.dataset.tray;
  if (action) void api.musicTray(action as "toggle" | "next" | "previous" | "open" | "quit");
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") void api.musicTray("close");
  if (event.key === " ") {
    event.preventDefault();
    void api.musicTray("toggle");
  }
});
document.addEventListener("contextmenu", (event) => event.preventDefault());

void events.onMusicNow((next) => {
  now = next;
  paint();
});
void events.onMusicWave((next) => {
  levels = compensate(next, now?.volume ?? 100);
  lastWave = performance.now();
});
void api.musicNow().then((next) => {
  now = next;
  paint();
  requestAnimationFrame(frame);
});
void api.musicWave(true);
