// Recordings page: full games and clips per account, a player whose timeline
// shows the key moments (click one to jump there), and a start/end range that
// is saved as a clip in the account's Clips folder.
import { ApiError, api, events, type Moment, type RecorderStatus, type Recording, type RecordingsView } from "./api";
import { errorText, localeTag, t, type MessageKey } from "./i18n";
import { $, esc, fragment, leave, modal, toast } from "./dom";
import { icon } from "./icons";
import { momentIcon } from "./momenticons";

const UNKNOWN = "Unknown";
const OBJECTIVES = new Set(["dragon", "baron", "herald", "grubs", "tower", "inhibitor", "ace"]);

type ClipMode = "fast" | "exact" | "discord";
const MODE_HINTS: Record<ClipMode, MessageKey> = { fast: "rec.fastHint", exact: "rec.exactHint", discord: "rec.discordHint" };
/** Seconds shown before a moment when jumping to it. */
const LEAD = 3;

let open = false;

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
  /** The account and game columns folded away for a bigger player (remembered). */
  let wide = false;
  try {
    wide = localStorage.getItem("recListHidden") === "1";
  } catch {
    // Non-essential.
  }
  /** fast: stream copy · exact: re-encoded · discord: re-encoded under 9.5 MB. */
  let mode: ClipMode = "fast";
  let saving = false;

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
          <section class="rec-player"></section>
        </div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  overlay.classList.toggle("is-wide", wide);
  $("#modal-root").append(overlay);
  const head = $(".rec-head", overlay);
  const nav = $(".rec-nav", overlay);
  const list = $(".rec-list", overlay);
  const player = $(".rec-player", overlay);

  const currentMoments = () => cleanMoments(selected?.meta?.moments ?? []);
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
      <button class="btn btn-ghost" data-rec="toggle-list">${icon("list", 14)}<span>${esc(t(wide ? "rec.showList" : "rec.hideList"))}</span></button>
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
      return `
        <div class="rec-row${selected?.path === entry.path ? " is-selected" : ""}" role="button" tabindex="0" data-rec-path="${esc(entry.path)}">
          <div class="rec-row-main"><strong>${esc(title(entry))}</strong>${result}${live}</div>
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

  const renderPlayer = () => {
    video = null;
    if (!selected) {
      player.innerHTML = `<p class="rec-empty rec-pick">${esc(t("rec.pick"))}</p>`;
      return;
    }
    const entry = selected;
    const meta = entry.meta;
    if (isRecording(entry)) {
      player.innerHTML = `<p class="rec-empty rec-pick">${esc(t("rec.inProgress"))}</p>`;
      return;
    }
    const moments = currentMoments();
    const facts = meta
      ? [meta.width && `${meta.width}×${meta.height}`, meta.fps && `${meta.fps} fps`, meta.encoder, meta.audioSource && t(`rec.audio.${meta.audioSource.split("+")[0]}` as MessageKey)].filter(Boolean).join(" · ")
      : "";
    player.innerHTML = `
      <div class="rec-screen"><video preload="metadata" src="http://larec.localhost/${encodeURIComponent(entry.path)}?v=${entry.modified}"></video></div>
      <div class="rec-controls">
        <button class="icon-btn rec-play" data-rec="play" aria-label="${esc(t("rec.play"))}"><span class="rec-play-glyph"></span></button>
        <span class="rec-time"><b data-rec-now>0:00</b> / <span data-rec-total>${clock(meta?.duration ?? 0)}</span></span>
        <div class="rec-track" data-rec-track>
          <div class="rec-parts-layer"></div>
          <div class="rec-range" hidden></div>
          <div class="rec-progress"></div>
          ${moments.map((moment, index) => `<button class="rec-marker tone-${momentTone(moment)}" data-rec-moment="${index}" data-tooltip="${esc(momentTooltip(moment))}" aria-label="${esc(momentTooltip(moment))}">${momentIcon(moment.kind)}${moment.kind === "multikill" ? `<b>${esc(String(Math.min(5, Math.max(2, Number(moment.detail) || 2))))}</b>` : ""}</button>`).join("")}
          <div class="rec-handle" data-rec-handle="start" hidden></div>
          <div class="rec-handle" data-rec-handle="end" hidden></div>
          <div class="rec-head-line"></div>
        </div>
        <button class="icon-btn" data-rec="mute" aria-label="${esc(t("rec.mute"))}">${icon("volume", 15)}</button>
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
      <div class="rec-parts" data-rec-parts hidden></div>
      <div class="rec-moments">${moments
        .map((moment, index) => `<button class="rec-chip tone-${momentTone(moment)}" data-rec-moment="${index}">${momentIcon(moment.kind, 13)}<b>${clock(moment.time)}</b>${esc(momentLabel(moment))}</button>`)
        .join("")}</div>
      <div class="rec-facts"><span>${esc(entry.name)}</span>${facts ? `<span>${esc(facts)}</span>` : ""}
        <button class="text-button" data-rec="reveal">${esc(t("rec.showFile"))}</button></div>`;
    video = $<HTMLVideoElement>("video", player);
    video.addEventListener("loadedmetadata", updateTimeline);
    video.addEventListener("timeupdate", updateTimeline);
    video.addEventListener("play", updatePlayState);
    video.addEventListener("pause", updatePlayState);
    video.addEventListener("click", () => togglePlay());
    video.addEventListener("error", () => toast("error", t("rec.playFailed")));
    updateTimeline();
    updatePlayState();
  };

  const duration = () => (video && Number.isFinite(video.duration) && video.duration > 0 ? video.duration : selected?.meta?.duration || 0);
  const percent = (time: number) => `${Math.min(100, Math.max(0, (time / (duration() || 1)) * 100))}%`;

  const updateTimeline = () => {
    if (!video) return;
    const total = duration();
    const track = player.querySelector<HTMLElement>("[data-rec-track]");
    if (!track) return;
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
        const x = (time / (total || 1)) * width;
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
    updateTimeline();
  };

  const render = () => {
    renderHead();
    renderNav();
    renderList();
    renderPlayer();
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
    if (selected?.path !== previous || !video) {
      range = null;
      parts = [];
      renderPlayer();
    } else if (changed && video) {
      const time = video.currentTime;
      renderPlayer();
      video?.addEventListener("loadedmetadata", () => seek(time), { once: true });
    }
  };

  const select = (path: string) => {
    if (selected?.path === path) return;
    selected = view.entries.find((entry) => entry.path === path) ?? null;
    range = null;
    parts = [];
    renderList();
    renderPlayer();
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
    if (selected?.path === path) {
      video?.pause();
      video?.removeAttribute("src");
      video?.load();
      selected = null;
      renderPlayer();
    }
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
    const answer = await modal({
      title: t("rec.saveClip"),
      body: `<label class="field"><span>${esc(t("rec.clipName"))}</span>
        <input name="clipName" maxlength="120" autocomplete="off" spellcheck="false" value="${esc(suggested)}"/></label>
        <p class="hint">${esc(t(MODE_HINTS[mode]))}</p>`,
      actions: [{ label: t("common.cancel"), value: "cancel" }, { label: t("rec.saveClip"), value: "save", kind: "primary" }],
      onOpen: (root) => $<HTMLInputElement>('[name="clipName"]', root).select(),
      onSubmit: (_value, root) => {
        name = $<HTMLInputElement>('[name="clipName"]', root).value.trim();
        return true;
      },
    });
    if (answer !== "save" || selected !== source) return;
    saving = true;
    updateTimeline();
    toast("info", t("rec.saving"));
    try {
      const clip = await api.saveClip(source.path, chosen.map((part) => [part.start, part.end] as [number, number]), mode, name);
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
    if (action === "folder") return void api.openRecordingsFolder(null).catch((error) => toast("error", t("toast.openFailed"), describe(error)));
    if (action === "reveal" && selected) return void api.openRecordingsFolder(selected.path).catch((error) => toast("error", t("toast.openFailed"), describe(error)));
    if (action === "delete" && row) return void remove(row.dataset.recPath!);
    if (action === "play") return togglePlay();
    if (action === "mute" && video) {
      video.muted = !video.muted;
      target.closest<HTMLElement>("[data-rec]")!.innerHTML = icon(video.muted ? "volume-x" : "volume", 15);
      return;
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
    if (action === "add-part" && range && range.end - range.start >= 1) {
      parts = merge(parts, range);
      range = null;
      return updateTimeline();
    }
    if (action === "toggle-list") {
      wide = !wide;
      try {
        localStorage.setItem("recListHidden", wide ? "1" : "0");
      } catch {
        // Non-essential.
      }
      overlay.classList.toggle("is-wide", wide);
      renderHead();
      return requestAnimationFrame(updateTimeline);
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
    const track = target.closest<HTMLElement>("[data-rec-track]");
    if (!track || !video || target.closest(".rec-marker")) return;
    const handle = target.closest<HTMLElement>("[data-rec-handle]")?.dataset.recHandle as "start" | "end" | undefined;
    const timeAt = (x: number) => {
      const box = track.getBoundingClientRect();
      return Math.min(1, Math.max(0, (x - box.left) / box.width)) * duration();
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
      return close();
    }
    if (!video) return;
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
    events.onRecorderStatus((next) => {
      status = next;
      renderHead();
      renderList();
    }),
  ];

  render();
  await reload(false);
}
