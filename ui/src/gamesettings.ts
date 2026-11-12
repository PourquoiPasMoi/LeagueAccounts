// Game settings page: League's shared settings file (PersistedSettings.json),
// edited visually, then written and locked again so every account keeps it.
import { ApiError, api, type GameSettingsChange, type GameSettingsView } from "./api";
import { errorText, localeTag, t, type MessageKey } from "./i18n";
import { $, esc, fragment, leave, modal, toast } from "./dom";
import { icon } from "./icons";

type Category = "hotkeys" | "sound" | "interface" | "game" | "accessibility" | "advanced";

interface Slider { kind: "slider"; file: string; section: string; name: string; label: string; min: number; max: number; step: number; percent?: boolean }
interface Toggle { kind: "toggle"; file: string; section: string; name: string; label: string }
type Control = Slider | Toggle;

// Setting names follow the English game client, which is how players know them.
const G = "Game.cfg";
const I = "Input.ini";
const toggle = (section: string, name: string, label: string): Toggle => ({ kind: "toggle", file: G, section, name, label });
const slider = (section: string, name: string, label: string, min = 0, max = 1, step = 0.01, percent = true): Slider =>
  ({ kind: "slider", file: G, section, name, label, min, max, step, percent });

const SOUND_CHANNELS: [string, string][] = [
  ["Master", "Master volume"], ["Music", "Music"], ["Announcer", "Announcer"], ["Voice", "Voice"],
  ["Sfx", "Sound effects"], ["Ambience", "Ambience"], ["Pings", "Pings"], ["Notifications", "Notifications"],
];

const PANELS: Record<Exclude<Category, "hotkeys" | "sound" | "advanced">, [string, Control[]][]> = {
  interface: [
    ["HUD", [
      slider("HUD", "GlobalScale", "HUD scale"),
      slider("HUD", "ChatScale", "Chat scale", 0, 100, 1, false),
      slider("HUD", "ShopScale", "Shop scale"),
      toggle("HUD", "FlipMiniMap", "Flip minimap"),
      toggle("HUD", "MinimapMoveSelf", "Show minimap on the left side"),
      toggle("HUD", "ShowTeamFramesOnLeft", "Show team frames on the left"),
      toggle("HUD", "MirroredScoreboard", "Mirrored scoreboard"),
      toggle("HUD", "ShowFPSAndLatency", "Show FPS and latency"),
    ]],
    ["Chat", [
      toggle("HUD", "ShowTimestamps", "Show timestamps"),
      toggle("HUD", "ShowAlliedChat", "Show allied chat"),
      toggle("HUD", "ShowAllChannelChat", "Show all chat"),
      toggle("Chat", "EnableChatFilter", "Chat filter"),
    ]],
    ["Display", [
      toggle("HUD", "ShowSummonerNames", "Show summoner names"),
      toggle("HUD", "ShowSpellCosts", "Show spell costs"),
      toggle("HUD", "DrawHealthBars", "Show health bars"),
      toggle("HUD", "ShowHealthBarShake", "Health bar shake"),
      toggle("HUD", "ShowAttackRadius", "Show attack range"),
      toggle("HUD", "ShowNeutralCamps", "Show neutral camps"),
      toggle("HUD", "ShowOffScreenPointsOfInterest", "Show off-screen points of interest"),
      toggle("HUD", "ShowPlayerPerks", "Show player runes"),
      toggle("HUD", "FlashScreenWhenDamaged", "Flash screen when damaged"),
      toggle("HUD", "FlashScreenWhenStunned", "Flash screen when stunned"),
      toggle("LossOfControl", "LossOfControlEnabled", "Loss of control indicator"),
      toggle("LossOfControl", "ShowSlows", "Show slows in loss of control"),
    ]],
  ],
  game: [
    ["Controls", [
      slider("General", "CursorScale", "Cursor scale"),
      slider("HUD", "MapScrollSpeed", "Camera move speed"),
      toggle("General", "AutoAcquireTarget", "Auto attack"),
      toggle("General", "EnableTargetedAttackMove", "Attack move on cursor"),
      toggle("General", "EnableLeftMouseButtonAttackMove", "Left click attack move"),
      toggle("General", "PredictMovement", "Predict movement"),
      toggle("General", "TargetChampionsOnlyAsToggle", "Target champions only as a toggle"),
    ]],
    ["Camera and map", [
      toggle("General", "SnapCameraOnRespawn", "Snap camera on respawn"),
      toggle("General", "EnableScreenShake", "Screen shake"),
      toggle("General", "ShowTurretRangeIndicators", "Show turret range indicators"),
      toggle("General", "RecommendJunglePaths", "Recommend jungle paths"),
      toggle("General", "RelativeTeamColors", "Relative team colors"),
    ]],
  ],
  accessibility: [
    ["Color", [
      slider("Accessibility", "ColorBrightness", "Brightness"),
      slider("Accessibility", "ColorContrast", "Contrast"),
      slider("Accessibility", "ColorGamma", "Gamma"),
      slider("Accessibility", "ColorLevel", "Level"),
    ]],
  ],
};

const HOTKEYS: [string, [string, string][]][] = [
  ["Abilities", [
    ["evtCastSpell1", "Ability 1"], ["evtCastSpell2", "Ability 2"], ["evtCastSpell3", "Ability 3"],
    ["evtCastSpell4", "Ability 4 (ultimate)"], ["evtCastAvatarSpell1", "Summoner spell 1"], ["evtCastAvatarSpell2", "Summoner spell 2"],
    ["evtLevelSpell1", "Level up ability 1"], ["evtLevelSpell2", "Level up ability 2"], ["evtLevelSpell3", "Level up ability 3"],
    ["evtLevelSpell4", "Level up ability 4"],
  ]],
  ["Items", [
    ["evtUseItem1", "Item 1"], ["evtUseItem2", "Item 2"], ["evtUseItem3", "Item 3"], ["evtUseItem4", "Item 4"],
    ["evtUseItem5", "Item 5"], ["evtUseItem6", "Item 6"], ["evtUseVisionItem", "Trinket"], ["evtUseItem7", "Recall"],
  ]],
  ["Movement and targeting", [
    ["evtPlayerMoveClick", "Move"], ["evtPlayerAttackMoveClick", "Attack move click"], ["evtPlayerAttackMove", "Attack move"],
    ["evtPlayerAttackOnlyClick", "Attack only"], ["evtPlayerStopPosition", "Stop"], ["evtPlayerHoldPosition", "Hold position"],
    ["evtChampionOnly", "Target champions only"], ["evtPetMoveClick", "Pet move"],
  ]],
  ["Camera", [
    ["evtCameraLockToggle", "Toggle camera lock"], ["evtCameraSnap", "Center camera on champion"], ["evtSelectSelf", "Select self"],
    ["evtSelectAlly1", "Select ally 1"], ["evtSelectAlly2", "Select ally 2"], ["evtSelectAlly3", "Select ally 3"],
    ["evtSelectAlly4", "Select ally 4"], ["evtOnUIMouse4Pan", "Drag camera"],
  ]],
  ["Camera scrolling", [
    ["evtScrollUp", "Scroll camera up"], ["evtScrollDown", "Scroll camera down"], ["evtScrollLeft", "Scroll camera left"],
    ["evtScrollRight", "Scroll camera right"], ["evtDragScrollLock", "Drag scroll lock"], ["evtToggleMouseClip", "Lock mouse to window"],
  ]],
  ["Pings", [
    ["evntPlayerPing", "Ping"], ["evntPlayerPingCursor", "Ping at cursor"], ["evntPlayerPingDanger", "Caution ping"],
    ["evntPlayerPingCursorDanger", "Caution ping at cursor"], ["evtPlayerPingRadialDanger", "Retreat ping"],
    ["evtPlayerPingMIA", "Enemy missing ping"], ["evtPlayerPingOMW", "On my way ping"], ["evtPlayerPingComeHere", "Assist me ping"],
    ["evtPlayerPingPush", "Push ping"], ["evtPlayerPingAllIn", "All in ping"], ["evtPlayerPingVisionNeeded", "Need vision ping"],
    ["evtPlayerPingAreaIsWarded", "Area warded ping"],
  ]],
  ["Emotes", [
    ["evtEmoteJoke", "Joke"], ["evtEmoteTaunt", "Taunt"], ["evtEmoteDance", "Dance"], ["evtEmoteLaugh", "Laugh"],
    ["evtEmoteToggle", "Toggle"], ["evtChampMasteryDisplay", "Mastery emote"], ["evtReciprocityTrigger", "Fist bump"],
    ["evtReciprocityMyBadTrigger", "My bad"], ["evtRadialEmoteOpen", "Open emote wheel"], ["evtRadialEmoteInstantOpen", "Open emote wheel (instant)"],
    ["evtRadialEmotePlaySlot0", "Emote wheel: centre"], ["evtRadialEmotePlaySlot1", "Emote wheel: slot 1"], ["evtRadialEmotePlaySlot2", "Emote wheel: slot 2"],
    ["evtRadialEmotePlaySlot3", "Emote wheel: slot 3"], ["evtRadialEmotePlaySlot4", "Emote wheel: slot 4"], ["evtRadialEmotePlaySlot5", "Emote wheel: slot 5"],
    ["evtRadialEmotePlaySlot6", "Emote wheel: slot 6"], ["evtRadialEmotePlaySlot7", "Emote wheel: slot 7"], ["evtRadialEmotePlaySlot8", "Emote wheel: slot 8"],
  ]],
  ["Communication and shop", [
    ["evtOpenShop", "Open shop"], ["evtShopFocusSearch", "Shop: search"], ["evtShopSwitchTabs", "Shop: switch tabs"],
    ["evtChatHistory", "Chat history"], ["evtPushToTalk", "Push to talk"], ["evtPushToTalkTeam", "Push to talk (team)"],
    ["evtShowVoicePanel", "Voice panel"],
  ]],
  ["Display", [
    ["evtShowScoreBoard", "Scoreboard (toggle)"], ["evtHoldShowScoreBoard", "Scoreboard (hold)"], ["evtTogglePlayerStats", "Player stats"],
    ["evtToggleFPSAndLatency", "FPS and latency"], ["evtShowHealthBars", "Health bars"], ["evtToggleMinionHealthBars", "Minion health bars"],
    ["evtShowSummonerNames", "Summoner names"], ["evtDrawHud", "Show or hide HUD"], ["evtToggleDeathRecapShowcase", "Death recap"],
    ["evtShowCharacterMenu", "Character menu"], ["evtSysMenu", "Game menu"],
  ]],
];

/** Quick cast toggles shown next to their ability. */
const QUICK_CAST: Record<string, string> = {
  evtCastSpell1: "evtCastSpell1smart", evtCastSpell2: "evtCastSpell2smart", evtCastSpell3: "evtCastSpell3smart",
  evtCastSpell4: "evtCastSpell4smart", evtCastAvatarSpell1: "evtCastAvatarSpell1smart", evtCastAvatarSpell2: "evtCastAvatarSpell2smart",
  evtUseItem1: "evtUseItem1smart", evtUseItem2: "evtUseItem2smart", evtUseItem3: "evtUseItem3smart",
  evtUseItem4: "evtUseItem4smart", evtUseItem5: "evtUseItem5smart", evtUseItem6: "evtUseItem6smart", evtUseVisionItem: "evtUseVisionItemsmart",
};

const CATEGORIES: [Category, MessageKey, string][] = [
  ["hotkeys", "gs.cat.hotkeys", "keyboard"], ["sound", "gs.cat.sound", "volume"], ["interface", "gs.cat.interface", "monitor"],
  ["game", "gs.cat.game", "gamepad"], ["accessibility", "gs.cat.accessibility", "eye"], ["advanced", "gs.cat.advanced", "sliders"],
];

const key = (file: string, section: string, name: string) => `${file}\u0000${section}\u0000${name}`;

// ---------------------------------------------------------------- smart search

/** Extra words people type for a setting (by setting name), so "ult" finds Ability 4. */
const TAGS: Record<string, string> = {
  evtCastSpell1: "q skill spell", evtCastSpell2: "w skill spell", evtCastSpell3: "e skill spell", evtCastSpell4: "r ult ulti ultimate skill spell",
  evtCastAvatarSpell1: "d flash ignite teleport summoners", evtCastAvatarSpell2: "f flash ignite teleport summoners",
  evtLevelSpell1: "skill up rank up points", evtLevelSpell2: "skill up rank up points", evtLevelSpell3: "skill up rank up points", evtLevelSpell4: "skill up rank up points ult",
  evtUseVisionItem: "ward vision trinket 4", evtUseItem7: "recall back home b base",
  evtUseItem1: "inventory slot active", evtUseItem2: "inventory slot active", evtUseItem3: "inventory slot active",
  evtUseItem4: "inventory slot active", evtUseItem5: "inventory slot active", evtUseItem6: "inventory slot active",
  evtPlayerAttackMoveClick: "am a move amove attackmove", evtPlayerAttackMove: "am a move amove attackmove",
  evtPlayerStopPosition: "s halt", evtPlayerHoldPosition: "stand still", evtPlayerMoveClick: "walk right click",
  evtCameraLockToggle: "y lock unlock", evtCameraSnap: "space center focus", evtSelectSelf: "f1 center camera",
  evtOpenShop: "p store buy", evtChatHistory: "messages log", evtPushToTalk: "voice mic microphone",
  evntPlayerPing: "ping alert", evntPlayerPingCursor: "ping alert", evntPlayerPingDanger: "ping danger retreat",
  evtPlayerPingMIA: "ss missing ping", evtPlayerPingOMW: "coming ping", evtPlayerPingAreaIsWarded: "ward ping vision",
  evtRadialEmoteInstantOpen: "emote wheel radial", evtEmoteDance: "emote", evtEmoteJoke: "emote", evtEmoteLaugh: "emote",
  evtEmoteTaunt: "emote", evtEmoteToggle: "emote mastery badge",
  evtChampMasteryDisplay: "emote mastery badge", evtReciprocityTrigger: "emote fist bump reciprocity thanks gg",
  evtReciprocityMyBadTrigger: "emote my bad sorry apology reciprocity", evtRadialEmoteOpen: "emote wheel radial",
  evtPlayerPingRadialDanger: "ping retreat danger back", evtPlayerPingComeHere: "ping assist help come", evtPlayerPingPush: "ping push",
  evtPlayerPingAllIn: "ping all in fight engage", evtPlayerPingVisionNeeded: "ping vision ward need", evntPlayerPingCursorDanger: "ping danger caution",
  evtShowScoreBoard: "tab scoreboard score", evtHoldShowScoreBoard: "tab scoreboard score", evtTogglePlayerStats: "stats",
  evtToggleFPSAndLatency: "fps ping latency frames", evtShowHealthBars: "hp health bar", evtToggleMinionHealthBars: "hp health bar minions creeps",
  evtShowSummonerNames: "names nicknames", evtDrawHud: "hud interface ui hide", evtToggleDeathRecapShowcase: "death recap",
  evtShopFocusSearch: "shop store search find", evtShopSwitchTabs: "shop store tabs", evtPushToTalkTeam: "voice mic microphone team",
  evtShowVoicePanel: "voice mic microphone", evtScrollUp: "camera scroll pan", evtScrollDown: "camera scroll pan",
  evtScrollLeft: "camera scroll pan", evtScrollRight: "camera scroll pan", evtToggleMouseClip: "mouse cursor lock confine",
  evtSysMenu: "menu escape options", evtShowCharacterMenu: "menu character",
  Master: "volume sound audio overall", Music: "volume sound audio songs", Announcer: "volume sound audio voice over",
  Voice: "volume sound audio champion lines vo", Sfx: "volume sound audio effects sfx", Ambience: "volume sound audio background",
  Pings: "volume sound audio alerts", Notifications: "volume sound audio alerts",
  GlobalScale: "hud ui size interface", ChatScale: "chat size text", ShopScale: "shop store size",
  FlipMiniMap: "minimap map side", MinimapMoveSelf: "minimap map side", ShowTeamFramesOnLeft: "team portraits",
  MirroredScoreboard: "tab scoreboard", ShowFPSAndLatency: "fps ping latency frames framerate network",
  ShowTimestamps: "chat time", ShowAlliedChat: "chat team", ShowAllChannelChat: "chat all enemies", EnableChatFilter: "chat profanity swear",
  ShowSummonerNames: "names nicknames", ShowSpellCosts: "mana cost", DrawHealthBars: "hp health bar", ShowHealthBarShake: "hp health bar",
  ShowAttackRadius: "range attack circle", ShowNeutralCamps: "jungle camps", ShowOffScreenPointsOfInterest: "arrows offscreen",
  ShowPlayerPerks: "runes perks", FlashScreenWhenDamaged: "damage red flash", FlashScreenWhenStunned: "cc stun flash",
  LossOfControlEnabled: "cc stun", ShowSlows: "cc slow",
  CursorScale: "mouse pointer cursor size", MapScrollSpeed: "camera speed scroll", AutoAcquireTarget: "auto attack aa",
  EnableTargetedAttackMove: "attack move cursor am", EnableLeftMouseButtonAttackMove: "attack move left click am",
  PredictMovement: "prediction latency", TargetChampionsOnlyAsToggle: "champions only", SnapCameraOnRespawn: "camera respawn",
  EnableScreenShake: "shake camera", ShowTurretRangeIndicators: "tower turret range", RecommendJunglePaths: "jungle path",
  RelativeTeamColors: "colors team red blue",
  ColorBrightness: "color brightness light", ColorContrast: "color contrast", ColorGamma: "color gamma", ColorLevel: "color level",
};

/** Lower-case words, splitting camelCase setting names ("ShowFPSAndLatency" → show fps and latency). */
function words(text: string): string[] {
  return text
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .normalize("NFD").replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter(Boolean);
}

/** Edit distance, giving up once it exceeds `limit`. */
function distance(a: string, b: string, limit: number): number {
  if (Math.abs(a.length - b.length) > limit) return limit + 1;
  let previous = Array.from({ length: b.length + 1 }, (_, index) => index);
  for (let i = 1; i <= a.length; i++) {
    const row = [i];
    let best = i;
    for (let j = 1; j <= b.length; j++) {
      row[j] = Math.min(previous[j] + 1, row[j - 1] + 1, previous[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
      best = Math.min(best, row[j]);
    }
    if (best > limit) return limit + 1;
    previous = row;
  }
  return previous[b.length];
}

/** How well one typed word matches a setting's words (0 = not at all). */
function tokenScore(token: string, haystack: string[], joined: string): number {
  const typos = token.length >= 7 ? 2 : token.length >= 4 ? 1 : 0;
  let best = 0;
  for (const word of haystack) {
    if (word === token) return 4;
    if (word.startsWith(token)) best = Math.max(best, 3);
    else if (token.length >= 3 && word.includes(token)) best = Math.max(best, 2);
    else if (typos && distance(token, word, typos) <= typos) best = Math.max(best, 2.5);
    else if (typos && word.length > token.length && distance(token, word.slice(0, token.length), typos) <= typos) best = Math.max(best, 1.5);
  }
  // Words typed together ("quickcast", "minimap").
  if (!best && token.length >= 4 && joined.includes(token)) best = 2;
  return best;
}

/** Score a setting for the query; 0 when any typed word is missing. */
function matchScore(query: string[], haystack: string[]): number {
  const joined = haystack.join("");
  let total = 0;
  for (const token of query) {
    const score = tokenScore(token, haystack, joined);
    if (!score) return 0;
    total += score;
  }
  return total;
}

// ---------------------------------------------------------------- key bindings

const UNBOUND = "[<Unbound>]";

/** `[Ctrl][1],[a]` → [["Ctrl", "1"], ["a"]]; unbound slots are empty arrays. */
function parseBinding(value: string): string[][] {
  if (!value.trim()) return [];
  return value.split(/\],\s*\[/).map((slot) => {
    const tokens = [...`[${slot.replace(/^\[|\]$/g, "")}]`.matchAll(/\[([^\]]*)\]/g)].map((match) => match[1]);
    return tokens.filter((token) => token && token !== "<Unbound>");
  });
}

function formatBinding(slots: string[][]): string {
  const filled = slots.map((slot) => (slot.length ? slot.map((token) => `[${token}]`).join("") : UNBOUND));
  return filled.join(",");
}

const keyLabel = (token: string): string => (token.length === 1 ? token.toUpperCase() : token.replace(/^Button (\d)$/, "Mouse $1"));

/** A browser key event as League's token, or null when it can't be expressed safely. */
function tokenFromKey(event: KeyboardEvent): string | null {
  const name = event.key;
  if (/^F([1-9]|1[0-2])$/.test(name)) return name;
  if (name === " ") return "Space";
  if (name === "Tab") return "Tab";
  if (name.length === 1 && /[a-z0-9`\-=\[\];',./\\]/i.test(name)) return name.toLowerCase();
  // With Shift held the character changes (1 → !); use the physical key instead.
  const digit = /^Digit(\d)$/.exec(event.code);
  if (digit) return digit[1];
  const letter = /^Key([A-Z])$/.exec(event.code);
  if (letter) return letter[1].toLowerCase();
  return null;
}

// ---------------------------------------------------------------- page

let open = false;

export async function openGameSettings(): Promise<void> {
  if (open) return;
  open = true;
  let view: GameSettingsView | null = null;
  let category: Category = "hotkeys";
  let lockAfter = true;
  let query = "";
  /** The smart search box above the categories; results replace the category view. */
  let find = "";
  let busy = false;
  /** Edited values by setting key; compared with the file to find changes. */
  const edits = new Map<string, string>();
  let capturing: { name: string; slot: number } | null = null;

  // The same framed dialog as Settings: title and divider, corner close
  // button, and the action buttons sitting on the bottom frame.
  const overlay = fragment(`
    <div class="overlay gs-overlay">
      <div class="modal gs-modal" role="dialog" aria-modal="true" aria-label="${esc(t("gs.title"))}">
        <header class="modal-head"><h2>${esc(t("gs.title"))}</h2></header>
        <div class="modal-close-mount">
          <button class="modal-close" data-gs="close" aria-label="${esc(t("common.close"))}"><span></span></button>
        </div>
        <div class="gs-head"></div>
        <div class="gs-layout">
          <nav class="gs-nav" role="tablist"></nav>
          <div class="gs-content"></div>
        </div>
        <footer class="modal-foot gs-foot"></footer>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  $("#modal-root").append(overlay);
  const head = $(".gs-head", overlay);
  const nav = $(".gs-nav", overlay);
  const content = $(".gs-content", overlay);
  const foot = $(".gs-foot", overlay);

  const original = (file: string, section: string, name: string): string | undefined =>
    view?.files.find((entry) => entry.name === file)?.sections.find((entry) => entry.name === section)
      ?.settings.find((entry) => entry.name === name)?.value;
  const value = (file: string, section: string, name: string): string | undefined =>
    edits.get(key(file, section, name)) ?? original(file, section, name);
  const set = (file: string, section: string, name: string, next: string) => {
    const id = key(file, section, name);
    if (original(file, section, name) === next) edits.delete(id);
    else edits.set(id, next);
  };
  const changes = (): GameSettingsChange[] =>
    [...edits.entries()].map(([id, next]) => {
      const [file, section, name] = id.split("\u0000");
      return { file, section, name, value: next };
    });

  // ------------------------------------------------------------ rendering

  const renderHead = () => {
    if (!view?.path) {
      head.innerHTML = `<p class="gs-subtitle">${esc(t("gs.subtitle"))}</p>`;
      return;
    }
    const locked = view.locked;
    const count = edits.size;
    head.innerHTML = `
      <div class="gs-status ${locked ? "is-locked" : "is-unlocked"}" data-tooltip="${esc(t(locked ? "gs.lockedHint" : "gs.unlockedHint"))}">
        ${icon(locked ? "lock" : "lock-open", 15)}<b>${esc(t(locked ? "gs.locked" : "gs.unlocked"))}</b>
        <button class="text-button" data-gs="toggle-lock" ${busy ? "disabled" : ""}>${esc(t(locked ? "gs.unlock" : "gs.lock"))}</button>
      </div>
      <span class="gs-changes${count ? " has-changes" : ""}">${esc(count ? t("gs.changes", { count }) : t("gs.noChanges"))}</span>
      <label class="toggle-row gs-lock-after"><span class="toggle-text"><strong>${esc(t("gs.keepLocked"))}</strong></span>
        <input type="checkbox" data-gs-lock-after ${lockAfter ? "checked" : ""}/><span class="switch"></span></label>
      ${view.backups ? `<button class="btn btn-ghost" data-gs="undo" ${busy ? "disabled" : ""} data-tooltip="${esc(view.lastBackup ? new Date(view.lastBackup).toLocaleString(localeTag()) : "")}">${icon("undo", 14)}<span>${esc(t("gs.undo"))}</span></button>` : ""}
      <button class="btn btn-ghost" data-gs="folder">${icon("folder-open", 14)}<span>${esc(t("gs.openFolder"))}</span></button>`;
  };

  const renderNav = () => {
    nav.innerHTML = `
      <label class="search gs-find">${icon("search", 14)}<input data-gs-find type="text" value="${esc(find)}" placeholder="${esc(t("gs.find"))}" autocomplete="off" spellcheck="false"/></label>
      ${CATEGORIES.map(([id, label, glyph]) => `
      <button role="tab" data-gs-cat="${id}" aria-selected="${!find.trim() && id === category}">${icon(glyph, 16)}<span>${esc(t(label))}</span></button>`).join("")}`;
  };

  const renderFoot = () => {
    const count = edits.size;
    foot.innerHTML = view?.path ? `
      <button class="btn btn-ghost" data-gs="revert" ${!count || busy ? "disabled" : ""}>${esc(t("gs.revert"))}</button>
      <button class="btn btn-primary" data-gs="apply" ${!count || busy ? "disabled" : ""}>${esc(t(lockAfter ? "gs.applyLock" : "gs.apply"))}</button>`
      : `<button class="btn btn-ghost" data-gs="close">${esc(t("common.close"))}</button>`;
  };

  const controlHtml = (control: Control): string => {
    const current = value(control.file, control.section, control.name);
    if (current === undefined) return "";
    const changed = edits.has(key(control.file, control.section, control.name)) ? " is-changed" : "";
    const attrs = `data-file="${esc(control.file)}" data-section="${esc(control.section)}" data-name="${esc(control.name)}"`;
    if (control.kind === "toggle") {
      return `<label class="toggle-row gs-row${changed}"><span class="toggle-text"><strong>${esc(control.label)}</strong></span>
        <input type="checkbox" data-gs-toggle ${attrs} ${current.trim() === "1" ? "checked" : ""}/><span class="switch"></span></label>`;
    }
    const number = Number(current) || 0;
    const shown = control.percent ? `${Math.round(((number - control.min) / (control.max - control.min)) * 100)}` : String(Math.round(number));
    return `<label class="gs-row gs-slider${changed}"><span>${esc(control.label)}</span>
      <input type="range" data-gs-slider ${attrs} min="${control.min}" max="${control.max}" step="${control.step}" value="${number}" style="--fill:${((number - control.min) / (control.max - control.min)) * 100}%"/>
      <output>${esc(shown)}</output></label>`;
  };

  const panel = (title: string, body: string) => (body.trim() ? `<section class="settings-section gs-panel"><h3>${esc(title)}</h3>${body}</section>` : "");

  const soundRow = (channel: string, label: string): string => {
    const volume = value(G, "Volume", `${channel}Volume`);
    if (volume === undefined) return "";
    const muted = value(G, "Volume", `${channel}Mute`) === "1";
    const changed = edits.has(key(G, "Volume", `${channel}Volume`)) || edits.has(key(G, "Volume", `${channel}Mute`)) ? " is-changed" : "";
    const number = Number(volume) || 0;
    return `<div class="gs-row gs-slider gs-volume${changed}${muted ? " is-muted" : ""}">
      <span>${esc(label)}</span>
      <input type="range" data-gs-slider data-file="${G}" data-section="Volume" data-name="${channel}Volume" min="0" max="1" step="0.01" value="${number}" style="--fill:${number * 100}%" aria-label="${esc(label)}"/>
      <output>${Math.round(number * 100)}</output>
      <button class="icon-btn gs-mute" data-gs-mute="${channel}" aria-pressed="${muted}" data-tooltip="${esc(t("gs.mute"))}">${icon(muted ? "volume-x" : "volume", 15)}</button>
    </div>`;
  };
  const soundHtml = () => panel(t("gs.cat.sound"), SOUND_CHANNELS.map(([channel, label]) => soundRow(channel, label)).join(""));

  const bindingCounts = (): Map<string, string[]> => {
    const owners = new Map<string, string[]>();
    for (const [, rows] of HOTKEYS) {
      for (const [name, label] of rows) {
        const binding = value(I, "GameEvents", name);
        if (binding === undefined) continue;
        for (const slot of parseBinding(binding)) {
          if (!slot.length) continue;
          const id = slot.join("+").toLowerCase();
          owners.set(id, [...(owners.get(id) ?? []), label]);
        }
      }
    }
    return owners;
  };

  const hotkeyRow = (name: string, label: string, owners: Map<string, string[]>): string => {
      // A hotkey the game has not written yet (never bound) shows as unbound.
      const binding = value(I, "GameEvents", name) ?? "";
      const slots = parseBinding(binding);
      while (slots.length < 2) slots.push([]);
      const changed = edits.has(key(I, "GameEvents", name)) ? " is-changed" : "";
      const quick = QUICK_CAST[name];
      const quickValue = quick ? value(I, "Quickbinds", quick) : undefined;
      return `<div class="gs-row gs-key${changed}">
        <span>${esc(label)}</span>
        <div class="gs-slots">${slots.slice(0, 2).map((slot, index) => {
          const active = capturing?.name === name && capturing.slot === index;
          const id = slot.join("+").toLowerCase();
          const others = slot.length ? (owners.get(id) ?? []).filter((owner) => owner !== label) : [];
          return `<button class="gs-slot${active ? " is-capturing" : ""}${slot.length ? "" : " is-empty"}${others.length ? " is-conflict" : ""}" data-gs-bind="${esc(name)}" data-slot="${index}"
            ${others.length ? `data-tooltip="${esc(t("gs.conflict", { names: others.join(", ") }))}"` : ""}>
            ${active ? esc(t("gs.pressKey")) : slot.length ? esc(slot.map(keyLabel).join(" + ")) : esc(t("gs.unbound"))}
          </button>`;
        }).join("")}</div>
        ${quickValue !== undefined ? `<label class="gs-quick"><input type="checkbox" data-gs-toggle data-file="${I}" data-section="Quickbinds" data-name="${esc(quick)}" ${quickValue === "1" ? "checked" : ""}/><span>${esc(t("gs.quickCast"))}</span></label>` : `<span></span>`}
      </div>`;
  };
  const hotkeysHtml = () => {
    const owners = bindingCounts();
    return `<p class="gs-hint">${esc(t("gs.keyHint"))}</p>` + HOTKEYS.map(([group, rows]) => panel(group, rows.map(([name, label]) => hotkeyRow(name, label, owners)).join(""))).join("");
  };

  const rawRow = (file: string, section: string, name: string): string => {
    const current = value(file, section, name) ?? "";
    const changed = edits.has(key(file, section, name)) ? " is-changed" : "";
    return `<label class="gs-row gs-raw${changed}"><span>${esc(name)}</span>
      <input type="text" data-gs-raw data-file="${esc(file)}" data-section="${esc(section)}" data-name="${esc(name)}" value="${esc(current)}" spellcheck="false" autocomplete="off"/></label>`;
  };

  /** Every setting the search can find, as (where it lives, words to match, how to draw it). */
  const searchEntries = (): { place: string; words: string[]; html: () => string }[] => {
    const owners = bindingCounts();
    const entries: { place: string; words: string[]; html: () => string }[] = [];
    const add = (place: string, label: string, name: string, extra: string, html: () => string) =>
      entries.push({ place, words: words(`${label} ${name} ${TAGS[name] ?? ""} ${extra}`), html });
    const hotkeys = t("gs.cat.hotkeys");
    for (const [group, rows] of HOTKEYS) {
      for (const [name, label] of rows) {
        const binding = value(I, "GameEvents", name);
        if (binding === undefined) continue;
        // The current key counts too: searching "space" finds what Space does.
        const keys = parseBinding(binding).flat().join(" ");
        const quick = QUICK_CAST[name] ? "quick cast smart cast self cast" : "";
        add(`${hotkeys} · ${group}`, label, name, `${group} hotkey keybind key bind shortcut ${keys} ${quick}`, () => hotkeyRow(name, label, owners));
      }
    }
    const sound = t("gs.cat.sound");
    for (const [channel, label] of SOUND_CHANNELS) {
      if (value(G, "Volume", `${channel}Volume`) === undefined) continue;
      add(sound, label, channel, "volume mute", () => soundRow(channel, label));
    }
    for (const [cat, labelKey] of [["interface", "gs.cat.interface"], ["game", "gs.cat.game"], ["accessibility", "gs.cat.accessibility"]] as const) {
      for (const [title, controls] of PANELS[cat]) {
        for (const control of controls) {
          if (value(control.file, control.section, control.name) === undefined) continue;
          add(`${t(labelKey)} · ${title}`, control.label, control.name, `${title} ${control.section}`, () => controlHtml(control));
        }
      }
    }
    // The raw list last, so curated controls rank first for the same words.
    const all = t("gs.cat.advanced");
    for (const file of view?.files ?? []) {
      for (const section of file.sections) {
        for (const setting of section.settings) {
          entries.push({
            place: `${all} · ${file.name} · ${section.name}`,
            words: words(`${setting.name} ${section.name} ${TAGS[setting.name] ?? ""} raw`),
            html: () => rawRow(file.name, section.name, setting.name),
          });
        }
      }
    }
    return entries;
  };

  const searchHtml = (): string => {
    const tokens = words(find);
    const results = searchEntries()
      .map((entry, order) => ({ entry, order, score: matchScore(tokens, entry.words) * (entry.place.startsWith(t("gs.cat.advanced")) ? 0.7 : 1) }))
      .filter((result) => result.score > 0)
      .sort((a, b) => b.score - a.score || a.order - b.order)
      .slice(0, 60);
    if (!results.length) return `<p class="muted profile-note">${esc(t("gs.findNone", { query: find.trim() }))}</p>`;
    // Group by where each setting lives, keeping the best matches' groups first.
    const groups = new Map<string, string[]>();
    for (const { entry } of results) groups.set(entry.place, [...(groups.get(entry.place) ?? []), entry.html()]);
    return `<p class="gs-hint">${esc(t("gs.findCount", { count: results.length }))}</p>` +
      [...groups].map(([place, rows]) => panel(place, rows.join(""))).join("");
  };

  const advancedHtml = () => {
    const needle = query.trim().toLowerCase();
    const sections = (view?.files ?? []).flatMap((file) => file.sections.map((section) => {
      const rows = section.settings.filter((setting) => !needle || `${section.name} ${setting.name} ${value(file.name, section.name, setting.name)}`.toLowerCase().includes(needle));
      if (!rows.length) return "";
      return panel(`${file.name} · ${section.name}`, rows.map((setting) => rawRow(file.name, section.name, setting.name)).join(""));
    }));
    return `<label class="search gs-search">${icon("search", 15)}<input data-gs-search type="text" value="${esc(query)}" placeholder="${esc(t("gs.search"))}" autocomplete="off" spellcheck="false"/></label>${sections.join("")}`;
  };

  const renderContent = () => {
    if (!view) {
      content.innerHTML = `<p class="muted profile-note">${esc(t("profile.syncing"))}</p>`;
      return;
    }
    if (!view.path) {
      content.innerHTML = `<div class="profile-empty">${icon("alert", 18)}<div><p>${esc(t("gs.missing"))}</p></div></div>`;
      return;
    }
    const notice = view.leagueOpen ? `<div class="profile-empty gs-warning">${icon("alert", 18)}<div><p>${esc(t("gs.leagueOpen"))}</p></div></div>` : "";
    let body = "";
    if (find.trim()) body = searchHtml();
    else if (category === "hotkeys") body = hotkeysHtml();
    else if (category === "sound") body = soundHtml();
    else if (category === "advanced") body = advancedHtml();
    else body = PANELS[category].map(([title, controls]) => panel(title, controls.map(controlHtml).join(""))).join("");
    content.innerHTML = notice + body;
  };

  const render = () => {
    renderHead();
    renderNav();
    renderContent();
    renderFoot();
  };

  const refreshAfterEdit = (keepFocus?: HTMLInputElement) => {
    if (keepFocus) {
      // Typing in the raw list: keep the field and caret, update only the markers.
      keepFocus.closest(".gs-row")?.classList.toggle("is-changed", edits.has(key(keepFocus.dataset.file!, keepFocus.dataset.section!, keepFocus.dataset.name!)));
    } else {
      renderContent();
    }
    renderHead();
    renderFoot();
  };

  // ------------------------------------------------------------ actions

  const describe = (error: unknown) => (error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error)));

  const run = async (work: () => Promise<GameSettingsView>, success?: MessageKey) => {
    if (busy) return;
    busy = true;
    renderHead();
    renderFoot();
    try {
      view = await work();
      if (success) toast("success", t(success));
      return true;
    } catch (error) {
      toast("error", t("gs.failed"), describe(error), 6000);
      return false;
    } finally {
      busy = false;
      render();
    }
  };

  const apply = async () => {
    const pending = changes();
    if (!pending.length) return;
    const locking = lockAfter;
    if (await run(() => api.applyGameSettings(pending, locking), locking ? "gs.savedLocked" : "gs.saved")) edits.clear();
    render();
  };

  const close = async () => {
    if (edits.size) {
      const answer = await modal({
        title: t("gs.discardTitle"),
        body: `<p class="hint">${esc(t("gs.discardText"))}</p>`,
        actions: [{ label: t("common.cancel"), value: "cancel" }, { label: t("gs.discard"), value: "discard", kind: "danger" }],
      });
      if (answer !== "discard") return;
    }
    open = false;
    document.removeEventListener("keydown", onKey, true);
    document.removeEventListener("mousedown", onMouse, true);
    void leave(overlay, "overlay-out");
    previousFocus?.focus?.();
  };

  const finishCapture = (token: string[] | null) => {
    if (!capturing) return;
    const { name, slot } = capturing;
    capturing = null;
    if (token) {
      const slots = parseBinding(value(I, "GameEvents", name) ?? "");
      while (slots.length <= slot) slots.push([]);
      slots[slot] = token;
      // Keep the file's shape: one slot stays one slot when the second is empty.
      while (slots.length > 1 && !slots.at(-1)!.length) slots.pop();
      set(I, "GameEvents", name, formatBinding(slots));
    }
    renderContent();
    renderHead();
    renderFoot();
  };

  const onKey = (event: KeyboardEvent) => {
    // A dialog opened on top (discard confirmation) owns the keyboard.
    if ([...document.querySelectorAll("#modal-root > .overlay:not(.overlay-out)")].at(-1) !== overlay) return;
    if (capturing) {
      event.preventDefault();
      event.stopImmediatePropagation();
      if (["Control", "Shift", "Alt", "Meta"].includes(event.key)) return;
      if (event.key === "Escape") return finishCapture(null);
      if (event.key === "Backspace" || event.key === "Delete") return finishCapture([]);
      const token = tokenFromKey(event);
      if (!token) {
        toast("info", t("gs.unsupportedKey"));
        return finishCapture(null);
      }
      const modifiers = [event.ctrlKey && "Ctrl", event.shiftKey && "Shift", event.altKey && "Alt"].filter(Boolean) as string[];
      return finishCapture([...modifiers, token]);
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      if (find) {
        find = "";
        renderNav();
        renderContent();
        return;
      }
      void close();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f") {
      event.preventDefault();
      event.stopImmediatePropagation();
      nav.querySelector<HTMLInputElement>("[data-gs-find]")?.select();
    }
  };

  // Mouse buttons 3 to 5 (middle and side buttons) can be bound while capturing.
  const onMouse = (event: MouseEvent) => {
    if (!capturing) return;
    const button = ({ 1: "Button 3", 3: "Button 4", 4: "Button 5" } as Record<number, string>)[event.button];
    if (!button) {
      if (!(event.target as HTMLElement).closest("[data-gs-bind]")) finishCapture(null);
      return;
    }
    event.preventDefault();
    event.stopImmediatePropagation();
    const modifiers = [event.ctrlKey && "Ctrl", event.shiftKey && "Shift", event.altKey && "Alt"].filter(Boolean) as string[];
    finishCapture([...modifiers, button]);
  };

  overlay.addEventListener("mousedown", (event) => {
    // Clicking the dimmed backdrop closes, like the other dialogs.
    if (event.target === overlay) void close();
  });
  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const action = target.closest<HTMLElement>("[data-gs]")?.dataset.gs;
    if (action === "close") return void close();
    if (action === "apply") return void apply();
    if (action === "revert") {
      edits.clear();
      return render();
    }
    if (action === "toggle-lock" && view) return void run(() => api.lockGameSettings(!view!.locked), view.locked ? "gs.unlockedDone" : "gs.lockedDone");
    if (action === "undo") return void run(() => api.restoreGameSettings(), "gs.restored").then(() => { edits.clear(); render(); });
    if (action === "folder") return void api.openGameSettingsFolder().catch((error) => toast("error", t("toast.openFailed"), describe(error)));
    const cat = target.closest<HTMLElement>("[data-gs-cat]")?.dataset.gsCat as Category | undefined;
    if (cat && (cat !== category || find)) {
      category = cat;
      find = "";
      capturing = null;
      content.scrollTop = 0;
      renderNav();
      return renderContent();
    }
    const bind = target.closest<HTMLElement>("[data-gs-bind]");
    if (bind) {
      event.preventDefault();
      capturing = { name: bind.dataset.gsBind!, slot: Number(bind.dataset.slot) };
      return renderContent();
    }
    const mute = target.closest<HTMLElement>("[data-gs-mute]")?.dataset.gsMute;
    if (mute) {
      set(G, "Volume", `${mute}Mute`, value(G, "Volume", `${mute}Mute`) === "1" ? "0" : "1");
      return refreshAfterEdit();
    }
  });

  overlay.addEventListener("input", (event) => {
    const input = event.target as HTMLInputElement;
    const { file, section, name } = input.dataset;
    if (input.matches("[data-gs-find]")) {
      find = input.value;
      nav.querySelectorAll<HTMLButtonElement>("[data-gs-cat]").forEach((button) => {
        button.setAttribute("aria-selected", String(!find.trim() && button.dataset.gsCat === category));
      });
      content.scrollTop = 0;
      return renderContent();
    }
    if (input.matches("[data-gs-search]")) {
      query = input.value;
      renderContent();
      const search = content.querySelector<HTMLInputElement>("[data-gs-search]");
      search?.focus();
      search?.setSelectionRange(query.length, query.length);
      return;
    }
    if (!file || !section || !name) return;
    if (input.matches("[data-gs-slider]")) {
      const current = original(file, section, name) ?? "";
      const decimals = current.includes(".") ? current.split(".")[1].length : 0;
      set(file, section, name, Number(input.value).toFixed(decimals));
      // Update the slider in place so dragging stays smooth.
      const min = Number(input.min);
      const max = Number(input.max);
      input.style.setProperty("--fill", `${((Number(input.value) - min) / (max - min)) * 100}%`);
      const output = input.parentElement?.querySelector("output");
      if (output) output.textContent = input.closest(".gs-volume") || max === 1 ? String(Math.round(((Number(input.value) - min) / (max - min)) * 100)) : String(Math.round(Number(input.value)));
      input.closest(".gs-row")?.classList.toggle("is-changed", edits.has(key(file, section, name)) || Boolean(input.closest(".gs-volume") && edits.has(key(file, section, name.replace("Volume", "Mute")))));
      renderHead();
      return renderFoot();
    }
    if (input.matches("[data-gs-raw]")) {
      set(file, section, name, input.value);
      return refreshAfterEdit(input);
    }
  });

  overlay.addEventListener("change", (event) => {
    const input = event.target as HTMLInputElement;
    if (input.matches("[data-gs-lock-after]")) {
      lockAfter = input.checked;
      return renderFoot();
    }
    const { file, section, name } = input.dataset;
    if (input.matches("[data-gs-toggle]") && file && section && name) {
      set(file, section, name, input.checked ? "1" : "0");
      return refreshAfterEdit();
    }
  });

  document.addEventListener("keydown", onKey, true);
  document.addEventListener("mousedown", onMouse, true);
  render();
  try {
    view = await api.gameSettings();
    lockAfter = view.locked || !view.path ? true : lockAfter;
  } catch (error) {
    toast("error", t("gs.failed"), describe(error), 6000);
    view = { path: null, locked: false, leagueOpen: false, files: [], lastBackup: null, backups: 0 };
  }
  render();
}
