// Game settings page: League's shared settings file (PersistedSettings.json),
// edited visually, then written and locked again so every account keeps it.
import { ApiError, api, type GameSettingsChange, type GameSettingsView } from "./api";
import { errorText, lang, localeTag, t, type MessageKey } from "./i18n";
import { $, esc, fragment, leave, modal, toast } from "./dom";
import { icon } from "./icons";
import GAME_STRINGS from "./data/game-strings.json";

/** A game setting label in the app's language, as League itself writes it
 *  (from League's string tables; English is the label itself). */
const STRINGS = GAME_STRINGS as Record<string, Record<string, string>>;
const gl = (label: string): string => STRINGS[lang()]?.[label] ?? label.replace(/ \(hotkey\)$/, "");

type Category = "hotkeys" | "sound" | "video" | "interface" | "game" | "accessibility" | "advanced";

interface Slider { kind: "slider"; file: string; section: string; name: string; label: string; min: number; max: number; step: number; percent?: boolean }
/** `invert`: the file stores the opposite ("Disable…" keys), so on = "0". */
interface Toggle { kind: "toggle"; file: string; section: string; name: string; label: string; invert?: boolean }
/** A few fixed values shown as one segmented control. */
interface Choice { kind: "choice"; file: string; section: string; name: string; label: string; options: [string, string][] }
/** Width and Height together, from the display's modes. */
interface Resolution { kind: "resolution"; file: string; section: string; name: string; label: string }
type Control = Slider | Toggle | Choice | Resolution;

// Setting names follow the English game client, which is how players know them.
const G = "Game.cfg";
const I = "Input.ini";
const toggle = (section: string, name: string, label: string, invert = false): Toggle => ({ kind: "toggle", file: G, section, name, label, invert });
const choice = (section: string, name: string, label: string, options: [string, string][]): Choice => ({ kind: "choice", file: G, section, name, label, options });
const slider = (section: string, name: string, label: string, min = 0, max = 1, step = 0.01, percent = true): Slider =>
  ({ kind: "slider", file: G, section, name, label, min, max, step, percent });

// Display settings League keeps only in this PC's game.cfg (not in the shared file).
const L = "game.cfg";
const localToggle = (section: string, name: string, label: string, invert = false): Toggle => ({ kind: "toggle", file: L, section, name, label, invert });
const localSlider = (section: string, name: string, label: string): Slider => ({ kind: "slider", file: L, section, name, label, min: 0, max: 1, step: 0.01, percent: true });
const QUALITY: [string, string][] = [["0", "Very low"], ["1", "Low"], ["2", "Medium"], ["3", "High"], ["4", "Very high"]];
const quality = (name: string, label: string, options = QUALITY): Choice => ({ kind: "choice", file: L, section: "Performance", name, label, options });

const SOUND_CHANNELS: [string, string][] = [
  ["Master", "Main volume"], ["Music", "Music volume"], ["Announcer", "Announcer volume"], ["Voice", "Voice volume"],
  ["Sfx", "Sound FX volume"], ["Ambience", "Ambience volume"], ["Pings", "Pings volume"], ["Notifications", "Notifications & UI"],
];

const PANELS: Record<Exclude<Category, "hotkeys" | "advanced">, [string, Control[]][]> = {
  sound: [
    ["General", [
      toggle("General", "EnableAudio", "Disable all sound", true),
      localToggle("General", "EnableCustomAnnouncer", "Disable custom announcer", true),
    ]],
    ["Voice", [
      localSlider("Voice", "InputVolume", "Input volume (gain)"),
      localSlider("Voice", "ActivationSensitivity", "Activation threshold (voice activity only)"),
      localToggle("Voice", "MuteOnConnect", "Mute my microphone when joining"),
      localToggle("Voice", "AutoJoinParty", "Join party voice automatically"),
      localToggle("Voice", "AutoJoinTeam", "Join team voice automatically"),
      toggle("Voice", "ShowVoiceChatHalos", "Show speaking indicators on HUD"),
      toggle("Voice", "ShowVoicePanelWithScoreboard", "Show voice panel with scoreboard"),
    ]],
  ],
  video: [
    ["Display", [
      { kind: "resolution", file: L, section: "General", name: "Width", label: "Resolution" },
      { kind: "choice", file: L, section: "General", name: "WindowMode", label: "Window mode", options: [["0", "Fullscreen"], ["2", "Borderless"], ["1", "Windowed"]] },
      localToggle("General", "WaitForVerticalSync", "Wait for vertical sync"),
    ]],
    ["Graphics", [
      quality("CharacterQuality", "Character quality"),
      quality("EnvironmentQuality", "Environment quality"),
      quality("EffectsQuality", "Effects quality"),
      quality("ShadowQuality", "Shadow quality", [["0", "Off"], ["1", "Low"], ["2", "Medium"], ["3", "High"], ["4", "Very high"]]),
      localToggle("Performance", "EnableFXAA", "Anti-aliasing"),
      localToggle("General", "HideEyeCandy", "Hide eye candy"),
      localToggle("General", "ShowGodray", "God rays"),
      localToggle("Performance", "AutoPerformanceSettings", "Automatic performance settings"),
    ]],
    ["Map skins", [
      localToggle("MapSkinOptions", "MapSkinOptionDisableSnowdown", "Disable Snowdown map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableLunarRevel", "Disable Lunar Revel map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisablePoolParty", "Disable Pool Party map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableStarGuardian", "Disable Star Guardian map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableOdyssey", "Disable Odyssey map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableProject", "Disable Project map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisablePopstar", "Disable Pop Star map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableArcade", "Disable Arcade map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableURF", "Disable URF map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableWorlds", "Disable Worlds map accents"),
      localToggle("MapSkinOptions", "MapSkinOptionDisableMSI", "Disable MSI map accents"),
    ]],
  ],
  interface: [
    ["Interface size", [
      slider("HUD", "GlobalScale", "HUD scale"),
      slider("General", "CursorScale", "Cursor scale"),
      slider("HUD", "ShopScale", "Shop scale"),
      slider("HUD", "ChatScale", "Chat scale", 0, 100, 1, false),
      slider("HUD", "MinimapScale", "Minimap scale", 0, 3),
      slider("HUD", "ObjectiveVoteScale", "Objective planning scale"),
      slider("HUD", "DeathRecapScale", "Death recap scale"),
      slider("HUD", "PracticeToolScale", "Practice Tool scale"),
    ]],
    ["Health and resource bars", [
      toggle("HUD", "DrawHealthBars", "Show health bars"),
      toggle("Performance", "EnableHUDAnimations", "Enable HUD animations"),
      toggle("LossOfControl", "LossOfControlEnabled", "Show loss of control UI"),
      toggle("LossOfControl", "ShowSlows", "Show slows in loss of control"),
      toggle("HUD", "ShowHealthBarShake", "Show health bar animations"),
      choice("HUD", "NameTagDisplay", "Show names above health bar", [["0", "None"], ["1", "Summoner name"], ["2", "Champion name"]]),
      toggle("HUD", "HidePlayerNames", "Enable streamer mode"),
      toggle("HUD", "ShowLastHitAssist", "Show last hit assist"),
    ]],
    ["Notifications", [
      toggle("HUD", "FlashScreenWhenDamaged", "Screen flash on damage"),
      toggle("HUD", "FlashScreenWhenStunned", "Screen flash on loss of control"),
      toggle("HUD", "ShowOffScreenPointsOfInterest", "Show off-screen pings"),
      localToggle("General", "EnableGlobalSpellCastNotifications", "Display champion sound notification"),
    ]],
    ["Ability and attack display", [
      toggle("HUD", "AutoDisplayTarget", "Show target frame on attack"),
      toggle("HUD", "ShowAttackRadius", "Show attack move range"),
      toggle("HUD", "ShowAutoAttackRangeIndicatorLine", "WASD attack range indicator"),
      toggle("HUD", "ShowSpellCosts", "Show spell costs"),
      toggle("HUD", "EnableLineMissileVis", "Enable line missile display"),
      toggle("HUD", "DisableHudSpellClick", "Disable HUD spell click"),
      toggle("HUD", "ShowSpellRecommendations", "Show spell recommendation"),
      choice("HUD", "NumericCooldownFormat", "Ability cooldown display", [["0", "None"], ["1", "Seconds"], ["2", "Minutes + seconds"], ["3", "Minutes"]]),
      localToggle("General", "AlwaysShowExtendedTooltip", "Always show extended tooltip info"),
    ]],
    ["Minimap", [
      toggle("HUD", "FlipMiniMap", "Show minimap on left"),
      toggle("HUD", "MinimapMoveSelf", "Allow minimap movement"),
      toggle("HUD", "ShowNeutralCamps", "Show neutral camps"),
      toggle("HUD", "MinimapEnableAllTimers", "Enable all minimap timers"),
    ]],
    ["Scoreboard and team frames", [
      toggle("HUD", "MirroredScoreboard", "Mirror scoreboard layout"),
      toggle("HUD", "ShowSummonerNamesInScoreboard", "Show summoner names in the scoreboard"),
      toggle("HUD", "ShowSummonerNames", "Show summoner names"),
      toggle("HUD", "ShowPlayerPerks", "Show player runes"),
      toggle("HUD", "ShowTeamFramesOnLeft", "Show team frames on left"),
      toggle("HUD", "ShowFPSAndLatency", "Show FPS and latency"),
      localToggle("HUD", "ShowLoadingScreenBackground", "Show loading screen background"),
    ]],
    ["Chat", [
      toggle("HUD", "ShowTimestamps", "Show timestamps"),
      toggle("HUD", "ShowAlliedChat", "Show [Allied] chat"),
      toggle("HUD", "ShowAllChannelChat", "Show [All] chat (matched games)"),
      toggle("Chat", "EnableChatFilter", "Chat filter"),
      toggle("TFTHUD", "EnableChat", "Chat in Teamfight Tactics"),
    ]],
    ["Emotes", [
      choice("HUD", "EmoteSize", "Emote size", [["0", "Normal"], ["1", "Small"]]),
      toggle("HUD", "HideEnemySummonerEmotes", "Mute enemy emotes"),
      toggle("HUD", "HideReciprocityFist", "Mute reaction (fist bumps and my bad)"),
    ]],
    ["Combat text", [
      toggle("FloatingText", "Damage_Enabled", "Damage"),
      toggle("FloatingText", "EnemyDamage_Enabled", "Enemy damage"),
      toggle("FloatingText", "Heal_Enabled", "Heal"),
      toggle("FloatingText", "ManaDamage_Enabled", "Mana"),
      toggle("FloatingText", "Gold_Enabled", "Gold"),
      toggle("FloatingText", "Experience_Enabled", "Experience"),
      toggle("FloatingText", "Level_Enabled", "Level"),
      toggle("FloatingText", "QuestReceived_Enabled", "Quest"),
      toggle("FloatingText", "Invulnerable_Enabled", "Status"),
      toggle("FloatingText", "Dodge_Enabled", "Dodge"),
      toggle("FloatingText", "Special_Enabled", "Special"),
      toggle("FloatingText", "Score_Enabled", "Score"),
    ]],
  ],
  game: [
    ["Gameplay", [
      choice("HUD", "Movement Mode", "Movement mode", [["0", "Mouse (point and click)"], ["1", "Keyboard (WASD)"]]),
      toggle("General", "AutoAcquireTarget", "Auto attack"),
      toggle("General", "EnableTargetedAttackMove", "Attack move on cursor"),
      toggle("General", "EnableLeftMouseButtonAttackMove", "Bind auto attack move to left click"),
      toggle("General", "PredictMovement", "Use movement prediction"),
      toggle("General", "TargetChampionsOnlyAsToggle", "Treat 'Target Champions Only' as a toggle"),
      toggle("General", "ShowTurretRangeIndicators", "Show turret range indicators (Co-op vs AI)"),
      toggle("General", "RecommendJunglePaths", "Display recommended jungle path"),
      toggle("HUD", "EnableItemComponentPurchasing", "Enable item component purchasing"),
      localToggle("General", "ClampCastTargetLocationWithinMaxRange", "Clamp cast target location within max range"),
      localToggle("General", "EnableChampionSpellPreview", "Enable champion spell previews"),
      localToggle("General", "LearnSpellOnCast", "Learn spell on cast"),
    ]],
    ["Casting", [
      toggle("HUD", "SmartCastOnKeyRelease", "Cast on key release"),
      toggle("HUD", "SmartCastWithIndicator_CastWhenNewSpellSelected", "Cast the pressed spell upon pressing another spell"),
    ]],
    ["Camera", [
      choice("HUD", "CameraLockMode", "Camera lock mode", [["0", "Per-side offset"], ["1", "Fixed offset"], ["2", "Semi-locked"]]),
      toggle("General", "CameraZoomOnMouseScroll", "Zoom camera on mouse wheel"),
      slider("HUD", "MapScrollSpeed", "Camera move speed (mouse)"),
      slider("HUD", "KeyboardScrollSpeed", "Camera move speed (keyboard)"),
      toggle("HUD", "ScrollSmoothingEnabled", "Enable smooth camera"),
      toggle("HUD", "MiddleClickDragScrollEnabled", "Mouse button drag scroll"),
      slider("HUD", "MiddleMouseScrollSpeed", "Drag scroll speed"),
      toggle("General", "SnapCameraOnRespawn", "Move camera on revive"),
      toggle("General", "EnableScreenShake", "Enable screen shake"),
      toggle("General", "RelativeTeamColors", "Use relative team colors"),
    ]],
  ],
  accessibility: [
    ["Color", [
      toggle("ColorPalette", "ColorPalette", "Color blind mode"),
      slider("Accessibility", "ColorBrightness", "Color brightness"),
      slider("Accessibility", "ColorContrast", "Color contrast"),
      slider("Accessibility", "ColorGamma", "Color gamma"),
      slider("Accessibility", "ColorLevel", "Color level"),
    ]],
    ["Motion and cursor", [
      localToggle("General", "MinimizeCameraMotion", "Minimize camera motion"),
      localToggle("General", "ShowCursorLocator", "Show cursor locator"),
    ]],
  ],
};

const HOTKEYS: [string, [string, string][]][] = [
  ["Abilities", [
    ["evtCastSpell1", "Spell 1"], ["evtCastSpell2", "Spell 2"], ["evtCastSpell3", "Spell 3"],
    ["evtCastSpell4", "Spell 4 (ultimate)"], ["evtCastAvatarSpell1", "Summoner spell 1"], ["evtCastAvatarSpell2", "Summoner spell 2"],
    ["evtLevelSpell1", "Level up spell 1"], ["evtLevelSpell2", "Level up spell 2"], ["evtLevelSpell3", "Level up spell 3"],
    ["evtLevelSpell4", "Level up spell 4"],
  ]],
  ["Items", [
    ["evtUseItem1", "Item 1"], ["evtUseItem2", "Item 2"], ["evtUseItem3", "Item 3"], ["evtUseItem4", "Item 4"],
    ["evtUseItem5", "Item 5"], ["evtUseItem6", "Item 6"], ["evtUseVisionItem", "Trinket"], ["evtUseItem7", "Recall"],
  ]],
  ["Player movement", [
    ["evtPlayerMoveClick", "Player move click"], ["evtPlayerAttackMoveClick", "Player attack move click"], ["evtPlayerAttackMove", "Player attack move"],
    ["evtPlayerAttackOnlyClick", "Player attack only click"], ["evtPlayerStopPosition", "Player stop position"], ["evtPlayerHoldPosition", "Player hold position"],
    ["evtChampionOnly", "Target champions only"], ["evtPetMoveClick", "Move pet click"],
  ]],
  ["Camera control", [
    ["evtCameraLockToggle", "Toggle camera lock"], ["evtCameraSnap", "Center camera on champion"], ["evtSelectSelf", "Select self"],
    ["evtSelectAlly1", "Select ally 1"], ["evtSelectAlly2", "Select ally 2"], ["evtSelectAlly3", "Select ally 3"],
    ["evtSelectAlly4", "Select ally 4"], ["evtOnUIMouse4Pan", "Drag scroll"],
  ]],
  ["Camera scrolling", [
    ["evtScrollUp", "Scroll up"], ["evtScrollDown", "Scroll down"], ["evtScrollLeft", "Scroll left"],
    ["evtScrollRight", "Scroll right"], ["evtDragScrollLock", "Drag scroll lock"], ["evtToggleMouseClip", "Toggle mouse screen lock"],
  ]],
  ["Pings", [
    ["evntPlayerPing", "Alert ping"], ["evntPlayerPingCursor", "Quick alert ping"], ["evntPlayerPingDanger", "Caution ping"],
    ["evntPlayerPingCursorDanger", "Quick caution ping"], ["evtPlayerPingRadialDanger", "Retreat ping"],
    ["evtPlayerPingMIA", "Enemy missing ping"], ["evtPlayerPingOMW", "On my way ping"], ["evtPlayerPingComeHere", "Assist me ping"],
    ["evtPlayerPingPush", "Push ping"], ["evtPlayerPingAllIn", "All in ping"], ["evtPlayerPingVisionNeeded", "Need vision ping"],
    ["evtPlayerPingAreaIsWarded", "Enemy vision ping"],
  ]],
  ["Emotes", [
    ["evtEmoteJoke", "Joke"], ["evtEmoteTaunt", "Taunt"], ["evtEmoteDance", "Dance"], ["evtEmoteLaugh", "Laugh"],
    ["evtEmoteToggle", "Toggle"], ["evtChampMasteryDisplay", "Mastery emote"], ["evtReciprocityTrigger", "Fist bump"],
    ["evtReciprocityMyBadTrigger", "My bad"], ["evtRadialEmoteOpen", "Open emote wheel"], ["evtRadialEmoteInstantOpen", "Quick open emote wheel"],
    ["evtRadialEmotePlaySlot0", "Emote center"], ["evtRadialEmotePlaySlot1", "Emote wheel slot 1"], ["evtRadialEmotePlaySlot2", "Emote wheel slot 2"],
    ["evtRadialEmotePlaySlot3", "Emote wheel slot 3"], ["evtRadialEmotePlaySlot4", "Emote wheel slot 4"], ["evtRadialEmotePlaySlot5", "Emote wheel slot 5"],
    ["evtRadialEmotePlaySlot6", "Emote wheel slot 6"], ["evtRadialEmotePlaySlot7", "Emote wheel slot 7"], ["evtRadialEmotePlaySlot8", "Emote wheel slot 8"],
  ]],
  ["Communication", [
    ["evtOpenShop", "Open shop"], ["evtShopFocusSearch", "Focus shop search bar"], ["evtShopSwitchTabs", "Switch shop tabs"],
    ["evtChatHistory", "Chat history"], ["evtPushToTalk", "Push to talk (party)"], ["evtPushToTalkTeam", "Push to talk (team)"],
    ["evtShowVoicePanel", "Show/hide voice chat panel"],
  ]],
  ["Display", [
    ["evtShowScoreBoard", "Show scoreboard"], ["evtHoldShowScoreBoard", "Show scoreboard (hold)"], ["evtTogglePlayerStats", "Toggle basic player stats"],
    ["evtToggleFPSAndLatency", "Toggle FPS display"], ["evtShowHealthBars", "Show health bars (hotkey)"], ["evtToggleMinionHealthBars", "Toggle minion health bars"],
    ["evtShowSummonerNames", "Show summoner names (hotkey)"], ["evtDrawHud", "Show/hide HUD"], ["evtToggleDeathRecapShowcase", "Toggle death recap showcase"],
    ["evtShowCharacterMenu", "Show character menu"], ["evtSysMenu", "System menu"],
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
  ["hotkeys", "gs.cat.hotkeys", "keyboard"], ["sound", "gs.cat.sound", "volume"], ["video", "gs.cat.video", "monitor"], ["interface", "gs.cat.interface", "layout-grid"],
  ["game", "gs.cat.game", "gamepad"], ["accessibility", "gs.cat.accessibility", "eye"], ["advanced", "gs.cat.advanced", "sliders"],
];

const key = (file: string, section: string, name: string) => `${file}\u0000${section}\u0000${name}`;

/** Every setting that has a proper control (the search shows it there, not as a raw value). */
const CURATED = new Set<string>([
  ...Object.values(PANELS).flatMap((panels) => panels.flatMap(([, controls]) => controls.flatMap((control) =>
    control.kind === "resolution" ? [key(control.file, control.section, "Width"), key(control.file, control.section, "Height")] : [key(control.file, control.section, control.name)]))),
  ...SOUND_CHANNELS.flatMap(([channel]) => [key(G, "Volume", `${channel}Volume`), key(G, "Volume", `${channel}Mute`)]),
  ...HOTKEYS.flatMap(([, rows]) => rows.map(([name]) => key(I, "GameEvents", name))),
  ...Object.values(QUICK_CAST).map((name) => key(I, "Quickbinds", name)),
]);

// ---------------------------------------------------------------- smart search

/** Extra words people type for a setting (by setting name), so "ult" finds Ability 4. */
const TAGS: Record<string, string> = {
  Width: "resolution screen size display", WindowMode: "fullscreen borderless windowed display", WaitForVerticalSync: "vsync tearing",
  CharacterQuality: "graphics", EnvironmentQuality: "graphics", EffectsQuality: "graphics", ShadowQuality: "graphics shadows",
  EnableFXAA: "antialiasing aa graphics", AutoPerformanceSettings: "graphics auto", FrameCapType: "fps frame rate cap limit",
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
  EnableAudio: "sound audio disable all mute", EnableCustomAnnouncer: "announcer voice pack",
  InputVolume: "mic microphone voice input volume", ActivationSensitivity: "mic microphone voice open mic threshold",
  MuteOnConnect: "mic microphone voice mute", AutoJoinParty: "voice chat party join", AutoJoinTeam: "voice chat team join",
  ShowVoiceChatHalos: "voice speaking halo", ShowVoicePanelWithScoreboard: "voice tab scoreboard",
  MinimapScale: "minimap map size", ObjectiveVoteScale: "objective vote voting planning size", DeathRecapScale: "death recap size",
  PracticeToolScale: "practice tool size", EnableHUDAnimations: "hud animations", NameTagDisplay: "names nicknames health bar champion summoner",
  HidePlayerNames: "streamer mode hide names", ShowLastHitAssist: "last hit cs farming minions assist",
  EnableGlobalSpellCastNotifications: "global ultimate spell cast notification",
  AutoDisplayTarget: "target frame attack", ShowAutoAttackRangeIndicatorLine: "attack range line indicator",
  EnableLineMissileVis: "skillshot line missile", DisableHudSpellClick: "hud spell click",
  ShowSpellRecommendations: "spell recommendation level up", NumericCooldownFormat: "cooldown timer numbers seconds minutes",
  AlwaysShowExtendedTooltip: "tooltip extended details", MinimapEnableAllTimers: "minimap jungle camp timers respawn",
  ShowSummonerNamesInScoreboard: "names tab scoreboard", ShowLoadingScreenBackground: "loading screen background",
  EnableChat: "tft chat teamfight tactics", EmoteSize: "emote size small", HideEnemySummonerEmotes: "emote enemy mute hide",
  HideReciprocityFist: "fist bump reciprocity reaction mute hide",
  Damage_Enabled: "combat floating text numbers damage", EnemyDamage_Enabled: "combat floating text numbers damage enemy",
  Heal_Enabled: "combat floating text numbers heal", ManaDamage_Enabled: "combat floating text numbers mana",
  Gold_Enabled: "combat floating text numbers gold", Experience_Enabled: "combat floating text numbers xp experience",
  Level_Enabled: "combat floating text level up", QuestReceived_Enabled: "combat floating text quest",
  Invulnerable_Enabled: "combat floating text status invulnerable", Dodge_Enabled: "combat floating text dodge miss",
  Special_Enabled: "combat floating text special", Score_Enabled: "combat floating text score",
  "Movement Mode": "wasd movement keyboard point click", EnableItemComponentPurchasing: "shop items components buy",
  ClampCastTargetLocationWithinMaxRange: "cast range clamp skillshot", EnableChampionSpellPreview: "spell preview champion",
  LearnSpellOnCast: "level up learn spell skill", SmartCastOnKeyRelease: "quick cast smart cast release",
  SmartCastWithIndicator_CastWhenNewSpellSelected: "quick cast indicator smart cast",
  CameraLockMode: "camera lock offset semi locked", CameraZoomOnMouseScroll: "camera zoom mouse wheel scroll",
  KeyboardScrollSpeed: "camera speed keyboard arrows", ScrollSmoothingEnabled: "camera smooth", MiddleClickDragScrollEnabled: "camera drag middle mouse",
  MiddleMouseScrollSpeed: "camera drag speed middle mouse", ColorPalette: "colorblind color blind deuteranopia",
  MinimizeCameraMotion: "camera motion sickness reduce", ShowCursorLocator: "cursor mouse find locate",
  MapSkinOptionDisableSnowdown: "map skin winter snow", MapSkinOptionDisableLunarRevel: "map skin lunar new year",
  MapSkinOptionDisablePoolParty: "map skin summer", MapSkinOptionDisableStarGuardian: "map skin", MapSkinOptionDisableOdyssey: "map skin",
  MapSkinOptionDisableProject: "map skin", MapSkinOptionDisablePopstar: "map skin kda", MapSkinOptionDisableArcade: "map skin",
  MapSkinOptionDisableURF: "map skin urf", MapSkinOptionDisableWorlds: "map skin worlds", MapSkinOptionDisableMSI: "map skin msi",
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
    if (control.kind === "choice") {
      return `<div class="gs-row gs-choice${changed}"><span>${esc(gl(control.label))}</span>
        <div class="segmented" role="group" aria-label="${esc(gl(control.label))}">${control.options
          .map(([option, label]) => `<button type="button" data-gs-choice="${esc(option)}" ${attrs} aria-pressed="${current.trim() === option}">${esc(gl(label))}</button>`)
          .join("")}</div></div>`;
    }
    if (control.kind === "resolution") {
      const width = Number(current) || 0;
      const height = Number(value(control.file, control.section, "Height")) || 0;
      const sizes = [...(view?.resolutions ?? [])];
      if (width && height && !sizes.some(([w, h]) => w === width && h === height)) sizes.unshift([width, height]);
      const resized = edits.has(key(control.file, control.section, "Width")) || edits.has(key(control.file, control.section, "Height")) ? " is-changed" : "";
      return `<label class="gs-row gs-choice${resized}"><span>${esc(gl(control.label))}</span>
        <div class="select-wrap"><select data-gs-resolution data-file="${esc(control.file)}" data-section="${esc(control.section)}">${sizes
          .map(([w, h]) => `<option value="${w}x${h}"${w === width && h === height ? " selected" : ""}>${w} × ${h}</option>`)
          .join("")}</select></div></label>`;
    }
    if (control.kind === "toggle") {
      return `<label class="toggle-row gs-row${changed}"><span class="toggle-text"><strong>${esc(gl(control.label))}</strong></span>
        <input type="checkbox" data-gs-toggle ${attrs}${control.invert ? " data-invert" : ""} ${(current.trim() === "1") !== Boolean(control.invert) ? "checked" : ""}/><span class="switch"></span></label>`;
    }
    const number = Number(current) || 0;
    const shown = control.percent ? `${Math.round(((number - control.min) / (control.max - control.min)) * 100)}` : String(Math.round(number));
    return `<label class="gs-row gs-slider${changed}"><span>${esc(gl(control.label))}</span>
      <input type="range" data-gs-slider ${attrs}${control.percent ? " data-percent" : ""} min="${control.min}" max="${control.max}" step="${control.step}" value="${number}" style="--fill:${((number - control.min) / (control.max - control.min)) * 100}%"/>
      <output>${esc(shown)}</output></label>`;
  };

  const panel = (title: string, body: string) => (body.trim() ? `<section class="settings-section gs-panel"><h3>${esc(gl(title))}</h3>${body}</section>` : "");

  const soundRow = (channel: string, label: string): string => {
    const volume = value(G, "Volume", `${channel}Volume`);
    if (volume === undefined) return "";
    const muted = value(G, "Volume", `${channel}Mute`) === "1";
    const changed = edits.has(key(G, "Volume", `${channel}Volume`)) || edits.has(key(G, "Volume", `${channel}Mute`)) ? " is-changed" : "";
    const number = Number(volume) || 0;
    return `<div class="gs-row gs-slider gs-volume${changed}${muted ? " is-muted" : ""}">
      <span>${esc(gl(label))}</span>
      <input type="range" data-gs-slider data-file="${G}" data-section="Volume" data-name="${channel}Volume" min="0" max="1" step="0.01" value="${number}" style="--fill:${number * 100}%" aria-label="${esc(gl(label))}"/>
      <output>${Math.round(number * 100)}</output>
      <button class="icon-btn gs-mute" data-gs-mute="${channel}" aria-pressed="${muted}" data-tooltip="${esc(t("gs.mute"))}">${icon(muted ? "volume-x" : "volume", 15)}</button>
    </div>`;
  };
  const soundHtml = () => panel(t("gs.cat.sound"), SOUND_CHANNELS.map(([channel, label]) => soundRow(channel, label)).join("")) +
    PANELS.sound.map(([title, controls]) => panel(title, controls.map(controlHtml).join(""))).join("");

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
        <span>${esc(gl(label))}</span>
        <div class="gs-slots">${slots.slice(0, 2).map((slot, index) => {
          const active = capturing?.name === name && capturing.slot === index;
          const id = slot.join("+").toLowerCase();
          const others = slot.length ? (owners.get(id) ?? []).filter((owner) => owner !== label) : [];
          return `<button class="gs-slot${active ? " is-capturing" : ""}${slot.length ? "" : " is-empty"}${others.length ? " is-conflict" : ""}" data-gs-bind="${esc(name)}" data-slot="${index}"
            ${others.length ? `data-tooltip="${esc(t("gs.conflict", { names: others.map(gl).join(", ") }))}"` : ""}>
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
      entries.push({ place, words: words(`${label} ${gl(label)} ${name} ${TAGS[name] ?? ""} ${extra}`), html });
    const hotkeys = t("gs.cat.hotkeys");
    for (const [group, rows] of HOTKEYS) {
      for (const [name, label] of rows) {
        const binding = value(I, "GameEvents", name);
        if (binding === undefined) continue;
        // The current key counts too: searching "space" finds what Space does.
        const keys = parseBinding(binding).flat().join(" ");
        const quick = QUICK_CAST[name] ? "quick cast smart cast self cast" : "";
        add(`${hotkeys} · ${gl(group)}`, label, name, `${group} ${gl(group)} hotkey keybind key bind shortcut ${keys} ${quick}`, () => hotkeyRow(name, label, owners));
      }
    }
    const sound = t("gs.cat.sound");
    for (const [channel, label] of SOUND_CHANNELS) {
      if (value(G, "Volume", `${channel}Volume`) === undefined) continue;
      add(sound, label, channel, "volume mute", () => soundRow(channel, label));
    }
    for (const [cat, labelKey] of [["sound", "gs.cat.sound"], ["video", "gs.cat.video"], ["interface", "gs.cat.interface"], ["game", "gs.cat.game"], ["accessibility", "gs.cat.accessibility"]] as const) {
      for (const [title, controls] of PANELS[cat]) {
        for (const control of controls) {
          if (value(control.file, control.section, control.name) === undefined) continue;
          add(`${t(labelKey)} · ${gl(title)}`, control.label, control.name, `${title} ${gl(title)} ${control.section}`, () => controlHtml(control));
        }
      }
    }
    // The raw list last, and only for settings without a proper control above.
    const all = t("gs.cat.advanced");
    for (const file of view?.files ?? []) {
      for (const section of file.sections) {
        for (const setting of section.settings) {
          if (CURATED.has(key(file.name, section.name, setting.name))) continue;
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

  /** Video: display settings of this PC (game.cfg), with their own lock. */
  const videoHtml = () => {
    const panels = PANELS.video.map(([title, controls]) => panel(title, controls.map(controlHtml).join(""))).join("");
    if (!panels.trim()) return `<div class="profile-empty">${icon("alert", 18)}<div><p>${esc(t("gs.videoMissing"))}</p></div></div>`;
    const locked = view?.localLocked ?? false;
    return `<div class="gs-local-lock ${locked ? "is-locked" : "is-unlocked"}">
        <div class="gs-status ${locked ? "is-locked" : "is-unlocked"}">${icon(locked ? "lock" : "lock-open", 15)}<b>${esc(t(locked ? "gs.locked" : "gs.unlocked"))}</b>
          <button class="text-button" data-gs="toggle-local-lock" ${busy ? "disabled" : ""}>${esc(t(locked ? "gs.unlock" : "gs.lock"))}</button></div>
        <p>${esc(t(locked ? "gs.videoLockedHint" : "gs.videoUnlockedHint"))}</p>
      </div>
      <p class="gs-hint">${esc(t("gs.videoNote"))}</p>${panels}`;
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
    else if (category === "video") body = videoHtml();
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
    if (action === "toggle-local-lock" && view) return void run(() => api.lockLocalGameSettings(!view!.localLocked), view.localLocked ? "gs.videoUnlockedDone" : "gs.videoLockedDone");
    const choice = target.closest<HTMLElement>("[data-gs-choice]");
    if (choice?.dataset.file && choice.dataset.section && choice.dataset.name) {
      set(choice.dataset.file, choice.dataset.section, choice.dataset.name, choice.dataset.gsChoice!);
      return refreshAfterEdit();
    }
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
      if (output) output.textContent = input.closest(".gs-volume") || input.hasAttribute("data-percent") ? String(Math.round(((Number(input.value) - min) / (max - min)) * 100)) : String(Math.round(Number(input.value)));
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
    if (input.matches("[data-gs-resolution]") && file && section) {
      const [width, height] = (input as unknown as HTMLSelectElement).value.split("x");
      set(file, section, "Width", width);
      set(file, section, "Height", height);
      return refreshAfterEdit();
    }
    if (input.matches("[data-gs-toggle]") && file && section && name) {
      set(file, section, name, input.checked !== input.hasAttribute("data-invert") ? "1" : "0");
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
    view = { path: null, locked: false, leagueOpen: false, files: [], lastBackup: null, backups: 0, localLocked: false, resolutions: [] };
  }
  render();
}
