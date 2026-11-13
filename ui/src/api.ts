import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Key {
  accountId: string;
  region: string;
}

export interface LolRank {
  tier: string;
  division: string;
  lp: string;
  reachedLastSeason: string;
  finishedLastSeason: string;
}

export interface TftRank {
  tier: string;
  division: string;
  lp: string;
  lastSet: string;
}

export type LoginMethod = "riot" | "previous";

export interface Settings {
  autoRefresh: boolean;
  intervalMinutes: number;
  startMinimized: boolean;
  launchAtStartup: boolean;
  loginMethod: LoginMethod;
  launchGame: boolean;
  /** Install new releases automatically; when off, only notify. */
  autoUpdate: boolean;
  /** Interface size in percent; the window zooms and resizes with it. */
  uiScale: number;
  /** Empty until the welcome dialog has been answered once. */
  uiLanguage: string;
  /** Riot Points per euro, for the account value estimate. */
  rpPerEuro: number;
  riotClientFound: boolean;
  tftInstalled: boolean;
}

// `uiLanguage` is set through its own command, not the Settings form, so it is
// not part of what the form submits.
export type SettingsInput = Omit<Settings, "riotClientFound" | "tftInstalled" | "uiLanguage">;

export interface Schedule {
  enabled: boolean;
  intervalMinutes: number;
  nextAt: number | null;
  lastAt: number | null;
  running: boolean;
}

export type LoginStep =
  | "closeLeague"
  | "openClient"
  | "signOut"
  | "signIn"
  | "verification"
  | "focus"
  | "type"
  | "launchGame"
  | "updating"
  | "done";

/** Progress Riot reports while an update keeps the game from launching. */
export interface LoginUpdate {
  /** Share of the update already applied, 0-100, when Riot reports it. */
  percent: number | null;
  /** Riot's estimate of the time left, when it reports one. */
  remainingMs: number | null;
}

export interface GameStatus {
  clientOpen: boolean;
  inGame: boolean;
  sameAccount: boolean;
}

export interface Release {
  version: string;
  name: string;
  url: string;
  notes: string;
  publishedAt: string;
}

export type UpdatePhase =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "ready"
  | "restarting"
  | "failed";

/** "available" and "failed" fall back to the download banner; "ready" is installed. */
export interface UpdateStatus {
  phase: UpdatePhase;
  current: string;
  /** The latest release when known, newer or not. */
  release: Release | null;
  received: number;
  total: number;
  /** The check the app starts with, which may hold the loading screen. */
  launch: boolean;
  /** Increases with every change; an older snapshot is ignored. */
  seq: number;
}

export type LoginResult = "signedIn" | "typed" | "alreadySignedIn";

export interface RankProvenance {
  source: string;
  fetchedAt: number | null;
  sourceUpdatedAt: number | null;
  freshness: "fresh" | "stale" | "unknown";
  /** Set by the backend from its single age threshold. */
  freshUntil: number | null;
  renewalAttempted: boolean;
  renewalSucceeded: boolean;
  error: "profile_not_found" | "rate_limited" | "provider_unavailable" | "parsing_failed" | "forbidden" | "timeout" | "renewal_failed" | null;
}

export interface AccountView {
  rankProvenance?: RankProvenance | null;
  tftProvenance?: RankProvenance | null;
  accountId: string;
  name: string;
  riotIdNotFound: boolean;
  region: string;
  regionDisplay: string;
  description: string;
  level: string;
  hasPassword: boolean;
  /** Skip Riot's profile-update flow at sign-in (VN accounts). */
  skipProfileUpdate: boolean;
  /** Product locale this account starts the game in; null = Riot's choice. */
  locale: string | null;
  lol: LolRank;
  tft: TftRank;
  /** League suspension reported at the last Riot sign-in; `until` is null when permanent. */
  ban?: Ban | null;
}

export interface Ban {
  until: number | null;
}

/** Snapshot read from the League client at the account's last sync. */
export interface Profile {
  syncedAt: number;
  summonerLevel: number;
  profileIconId: number;
  ranked: RankedQueue[];
  champions: OwnedChampion[];
  matches: MatchSummary[];
  wallet: { rp: number | null; blueEssence: number | null };
}

export interface RankedQueue {
  /** RANKED_SOLO_5x5, RANKED_FLEX_SR, RANKED_TFT, ... */
  queue: string;
  tier: string;
  division: string;
  lp: number;
  wins: number;
  losses: number;
}

export interface OwnedChampion {
  id: number;
  name: string;
  alias: string;
  title: string;
  masteryLevel: number;
  masteryPoints: number;
  purchased: number;
  skins: OwnedSkin[];
}

export interface OwnedSkin {
  id: number;
  name: string;
  /** Game-data image path (see `gameImage`). */
  tile: string;
  rarity: string;
  chromasOwned: number;
  chromasTotal: number;
}

export interface MatchSummary {
  gameId: number;
  created: number;
  durationSeconds: number;
  queueId: number;
  mode: string;
  championId: number;
  win: boolean;
  kills: number;
  deaths: number;
  assists: number;
  cs: number;
  championLevel: number;
}

export interface RankPoint {
  at: number;
  mode: "lol" | "tft";
  tier: string;
  division: string;
  lp: number;
}

/** One archived game: full detail from the League client, or a summary from OP.GG. */
export interface MatchRecord {
  source: "client" | "opgg";
  gameId: number;
  created: number;
  durationSeconds: number;
  queueId: number;
  queue: string;
  mode: string;
  championId: number;
  champion: string;
  win: boolean;
  remake: boolean;
  kills: number;
  deaths: number;
  assists: number;
  cs: number;
  championLevel: number;
  gold: number;
  damage: number;
  damageTaken: number;
  damageMitigated: number;
  healing: number;
  vision: number;
  wardsPlaced: number;
  wardsKilled: number;
  controlWards: number;
  ccSeconds: number;
  turretKills: number;
  teamKills: number;
  teamDamage: number;
  position: string;
  items: number[];
  spells: number[];
  largestMultiKill: number;
  doubleKills: number;
  tripleKills: number;
  quadraKills: number;
  pentaKills: number;
  firstBlood: boolean;
  opScoreX10: number | null;
  /** `blue`, `red` or empty. */
  side: string;
}

/** A finished season's Solo/Duo rank from OP.GG. */
export interface SeasonRank {
  season: string;
  tier: string;
  division: string;
  lp: number;
  peakTier: string;
  peakDivision: string;
  peakLp: number;
}

export interface ProfileView {
  profile: Profile | null;
  history: RankPoint[];
  /** Past seasons' Solo/Duo ranks, newest first. */
  seasons: SeasonRank[];
  /** Every archived game, newest first. */
  games: MatchRecord[];
}

/** One value in League's shared settings file. */
export interface GameSettingsChange {
  file: string;
  section: string;
  name: string;
  value: string;
}

/** League's PersistedSettings.json: `Game.cfg` and `Input.ini` sections. */
export interface GameSettingsView {
  /** null when no League install with a settings file was found. */
  path: string | null;
  locked: boolean;
  leagueOpen: boolean;
  files: { name: string; sections: { name: string; settings: { name: string; value: string }[] }[] }[];
  lastBackup: number | null;
  backups: number;
}

/** Who is signed in to the Riot Client right now. */
export interface SessionStatus {
  accountId: string | null;
  clientOpen: boolean;
  inGame: boolean;
}

/** One owned skin, for finding which account has it. */
export interface SkinOwner {
  key: Key;
  championId: number;
  championName: string;
  skinId: number;
  skinName: string;
  tile: string;
  rarity: string;
}

const GAME_DATA = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/";

/**
 * URL of a game-data picture. In the app it goes through the `lacache`
 * protocol, which downloads it once and then serves it from disk.
 */
export const gameImage = (path: string): string =>
  ("__TAURI_INTERNALS__" in window ? "http://lacache.localhost/" : GAME_DATA) + path.replace(/^\/+/, "");

export const championIcon = (id: number): string => gameImage(`v1/champion-icons/${id}.png`);
export const profileIcon = (id: number): string => gameImage(`v1/profile-icons/${id}.jpg`);

export const accountLabel = (account: AccountView): string => account.name || account.accountId;

/** Backend failure: a stable code the UI translates, plus optional detail. */
export interface ErrorPayload {
  code: string;
  detail: string | null;
}

export class ApiError extends Error {
  constructor(
    readonly code: string,
    readonly detail: string | null = null,
  ) {
    super(code);
  }
}

export interface RefreshDone {
  error: ErrorPayload | null;
  /** True for a full refresh, false for a single-account refresh. */
  exclusive: boolean;
  auto: boolean;
}

export interface RefreshStarted {
  total: number;
  auto: boolean;
}

export interface LocaleOption {
  code: string;
  name: string;
  /// False when the client home page does not render in this locale.
  homePageWorks: boolean;
  /// False when the Riot Client substitutes another locale at launch.
  launchable: boolean;
}

export interface Bootstrap {
  accounts: AccountView[];
  regions: string[];
  locales: LocaleOption[];
  version: string;
  loggingAvailable: boolean;
  loadError: ErrorPayload | null;
  /** A release that failed to start; the previous version was restored. */
  updateFailed: string | null;
  settings: Settings;
  schedule: Schedule;
}

export interface BatchResult {
  added: number;
  skipped: number;
}

export interface BulkAddResult {
  added: number;
  skippedLines: number[];
}

export interface UpdateAccount {
  name: string;
  description: string;
  locale: string | null;
  password?: string;
  skipProfileUpdate?: boolean;
}

export interface NewAccount {
  accountId: string;
  name: string;
  region: string;
  password: string;
  description: string;
  locale?: string | null;
  skipProfileUpdate?: boolean;
}

export function parseAccountLine(line: string): Pick<NewAccount, "accountId" | "name" | "password"> | undefined {
  // Mirrors `parse_account_line` in Rust: `--` separates fields, two fields
  // mean no Riot ID yet, and a three-field line needs all three filled. A
  // password that starts with `--` therefore fails to parse rather than
  // silently losing its dashes. The middle field must look like a Riot ID
  // (`Name#TAG`, no leading dash), so a password containing `--` or the old
  // `---` separator form is rejected instead of truncating the password.
  const parts = line.split("--").map((part) => part.trim());
  if (parts.length === 2 && parts[0] && parts[1]) {
    return { accountId: parts[0], name: "", password: parts[1] };
  }
  if (parts.length === 3 && parts[0] && parts[1].includes("#") && !parts[1].startsWith("-") && parts[2]) {
    return { accountId: parts[0], name: parts[1], password: parts[2] };
  }
  return undefined;
}

export const keyOf = (account: AccountView): Key => ({
  accountId: account.accountId,
  region: account.region,
});

export const keyId = (key: Key): string => `${key.region}:${key.accountId}`;

/** Tauri rejects with the command's `Err(AppError)`; normalize to ApiError. */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    if (error && typeof error === "object" && "code" in error) {
      const payload = error as ErrorPayload;
      throw new ApiError(payload.code, payload.detail);
    }
    throw new ApiError("unknown", String(error));
  }
}

export const api = {
  bootstrap: () => call<Bootstrap>("bootstrap"),
  appReady: () => call<boolean>("app_ready"),
  listAccounts: () => call<AccountView[]>("list_accounts"),
  addAccount: (input: NewAccount) => call<AccountView>("add_account", { input }),
  multiAdd: (text: string, region: string, locale: string | null) =>
    call<BulkAddResult>("multi_add", { text, region, locale }),
  /** `input.password` left undefined keeps the stored credential. */
  updateAccount: (key: Key, input: UpdateAccount) =>
    call<AccountView>("update_account", { key, input }),
  deleteAccount: (key: Key) => call<void>("delete_account", { key }),
  refreshRanks: (force = false) => call<number>("refresh_ranks", { force }),
  copyAccountId: (key: Key) => call<void>("copy_account_id", { key }),
  copyPassword: (key: Key) => call<number>("copy_password", { key }),
  /** `launchBanned` launches the game even if Riot reports a League ban. */
  login: (key: Key, tft: boolean, closeRunning: boolean, launchBanned: boolean) =>
    call<LoginResult>("login", { key, tft, closeRunning, launchBanned }),
  cancelLogin: () => call<void>("cancel_login"),
  gameStatus: (key: Key) => call<GameStatus>("game_status", { key }),
  updateStatus: () => call<UpdateStatus>("update_status"),
  checkUpdate: () => call<UpdateStatus>("check_update"),
  /** No login, rank refresh or match in progress. */
  updateIdle: () => call<boolean>("update_idle"),
  restartToUpdate: () => call<void>("restart_to_update"),
  openRelease: (url: string) => call<void>("open_release", { url }),
  getSettings: () => call<Settings>("get_settings"),
  updateSettings: (input: SettingsInput) => call<Settings>("update_settings", { input }),
  /** Durable copy of the interface language, for when the webview cache is lost. */
  setUiLanguage: (language: string) => call<void>("set_ui_language", { language }),
  exportData: (title: string) => call<string | null>("export_data", { title }),
  importData: (title: string) => call<BatchResult | null>("import_data", { title }),
  openProfile: (key: Key, tft: boolean) => call<void>("open_profile", { key, tft }),
  openLogsFolder: () => call<void>("open_logs_folder"),
  /** Opens ABC's Bilibili page, where the zh-cn-home-locale plugin lives. */
  openHomeFixAuthor: () => call<void>("open_home_fix_author"),
  /** Opens Pengu Loader's site; the plugin needs it installed. */
  openPenguLoader: () => call<void>("open_pengu_loader"),
  getProfile: (key: Key) => call<ProfileView>("get_profile", { key }),
  /** Needs the League client open and signed in as this account. */
  syncProfile: (key: Key) => call<ProfileView>("sync_profile", { key }),
  /** Reads the season's LP graph and match list from OP.GG (at most every 6 h
   * unless `force`). True when something new was stored. */
  syncOpggHistory: (key: Key, force = false) => call<boolean>("sync_opgg_history", { key, force }),
  skinIndex: () => call<SkinOwner[]>("skin_index"),
  champSelectState: () => call<ChampSelect>("champ_select_state"),
  buildPrefs: () => call<BuildPrefs>("build_prefs"),
  setBuildPrefs: (prefs: BuildPrefs) => call<BuildPrefs>("set_build_prefs", { prefs }),
  buildAssets: () => call<GameAssets>("build_assets"),
  /** OP.GG's statistics plus one build to follow against these enemies. */
  championBuild: (championId: number, mode: BuildMode, position: string, threats: Threats) =>
    call<ChampionBuild>("champion_build", { championId, mode, position, threats }),
  laneSuggestions: (position: string) => call<Suggestion[]>("lane_suggestions", { position }),
  liveGameState: () => call<LiveGame | null>("live_game_state"),
  /** Live numbers for everyone (local only); null before the match starts. */
  liveStats: () => call<LiveStats | null>("live_stats"),
  recorderPrefs: () => call<RecorderPrefsView>("recorder_prefs"),
  setRecorderPrefs: (prefs: RecorderPrefs) => call<RecorderPrefsView>("set_recorder_prefs", { prefs }),
  pickRecordingsFolder: (title: string) => call<string | null>("pick_recordings_folder", { title }),
  recorderStatus: () => call<RecorderStatus>("recorder_status"),
  microphones: () => call<{ id: string; name: string; console: boolean; communications: boolean }[]>("microphones"),
  /** Loudest level (0..1) the microphone hears in 2 s. */
  microphoneLevel: (id: string) => call<number>("microphone_level", { id }),
  recordings: () => call<RecordingsView>("recordings"),
  deleteRecording: (path: string) => call<void>("delete_recording", { path }),
  /** `folder`: a folder inside the recordings folder (e.g. "Share Lock#EUW/Clips"), when no `path`. */
  openRecordingsFolder: (path: string | null, folder: string | null = null) => call<void>("open_recordings_folder", { path, folder }),
  /** `parts`: [start, end] seconds, joined into one clip with plain cuts. */
  /** `mode`: "fast" (stream copy), "exact" (re-encoded), "discord" (re-encoded under 9.5 MB). */
  /** `sounds`: the sources to keep ("game", "pc", "mic", "discord"); undefined = all. */
  /** `gains`: level per source (1 = as recorded, up to 3). */
  saveClip: (path: string, parts: [number, number][], mode: "fast" | "exact" | "discord", name: string, sounds?: string[], gains?: Record<string, number>) =>
    call<Recording>("save_clip", { path, parts, mode, name, sounds: sounds ?? null, gains: gains ?? null }),
  /** Each sound source of a recording as its own file (split the first time). */
  /** End-of-game details saved with a game recording; read from the League client first when `fetch`. */
  recordingGame: (path: string, fetch: boolean) => call<import("./gameinfo").GameDetails | null>("recording_game", { path, fetch }),
  recordingSoundTracks: (path: string) => call<{ label: string; path: string }[]>("recording_sound_tracks", { path }),
  /** Puts the chosen parts of the build into the League client. */
  applyBuild: (request: ApplyRequest) => call<ApplyResult>("apply_build", { request }),
  sessionStatus: () => call<SessionStatus>("session_status"),
  gameSettings: () => call<GameSettingsView>("game_settings"),
  /** Writes the values, then locks the file again when `lock` is set. */
  applyGameSettings: (changes: GameSettingsChange[], lock: boolean) => call<GameSettingsView>("apply_game_settings", { changes, lock }),
  lockGameSettings: (locked: boolean) => call<GameSettingsView>("lock_game_settings", { locked }),
  restoreGameSettings: () => call<GameSettingsView>("restore_game_settings"),
  openGameSettingsFolder: () => call<void>("open_game_settings_folder"),
};

// ---------------------------------------------------------------- champ select builds

export type BuildMode = "ranked" | "aram";

export interface ChampSelect {
  active: boolean;
  championId: number;
  locked: boolean;
  /** OP.GG lane (`top`, `jungle`, `mid`, `adc`, `support`) or empty. */
  position: string;
  /** Null for modes without OP.GG builds (Arena, rotating modes). */
  mode: BuildMode | null;
  queueId: number;
  /** Enemy champions shown so far. */
  enemies: number[];
}

/** The enemy team: how many deal magic or physical damage, tank, or heal. */
export interface Threats {
  magic: number;
  physical: number;
  tanks: number;
  healers: number;
}

export interface Suggestion {
  championId: number;
  tier: number;
  rank: number;
  winRate: number;
  pickRate: number;
}

/** One build to follow: start, six items in order, swaps. */
export interface FinalBuild {
  starter: number[];
  order: number[];
  swaps: number[];
  adapted: { item: number; reason: "magicResist" | "armor" | "antiHeal" }[];
}

export interface BuildPrefs {
  experimental: boolean;
  openOnChampSelect: boolean;
  autoApply: boolean;
  autoRunes: boolean;
  autoSpells: boolean;
  autoItems: boolean;
}

export interface QueueRank {
  tier: string;
  division: string;
  lp: number;
  wins: number;
  losses: number;
}

export interface LivePlayer {
  gameName: string;
  tagLine: string;
  championId: number;
  team: number;
  isMe: boolean;
  position: string;
  solo: QueueRank | null;
  flex: QueueRank | null;
  previousTier: string;
  recentGames: number;
  recentWins: number;
  championGames: number;
  championWins: number;
  unknown: boolean;
}

/** One player's live numbers from Riot's Live Client Data API. */
export interface LiveStatsPlayer {
  gameName: string;
  tagLine: string;
  champion: string;
  /** 1 = blue side, 2 = red side. */
  team: number;
  position: string;
  level: number;
  kills: number;
  deaths: number;
  assists: number;
  cs: number;
  vision: number;
  items: number[];
  dead: boolean;
  respawn: number;
}

export interface LiveStats {
  gameTime: number;
  players: LiveStatsPlayer[];
  /** [blue, red] */
  dragons: [number, number];
  barons: [number, number];
  heralds: [number, number];
  towers: [number, number];
  inhibitors: [number, number];
}

export interface LiveGame {
  gameId: number;
  queueId: number;
  gameMode: string;
  myTeam: number;
  players: LivePlayer[];
}

export interface Rates {
  games: number;
  winRate: number;
  pickRate: number;
}

export interface RunePage extends Rates {
  primaryStyle: number;
  subStyle: number;
  perks: number[];
}

export interface IdSet extends Rates {
  ids: number[];
}

export interface ChampionBuild {
  championId: number;
  mode: BuildMode;
  position: string;
  positions: { position: string; roleRate: number; winRate: number; games: number }[];
  tier: number | null;
  rank: number | null;
  winRate: number;
  pickRate: number;
  banRate: number;
  games: number;
  runePages: RunePage[];
  spells: IdSet[];
  starterItems: IdSet[];
  coreItems: IdSet[];
  boots: IdSet[];
  situational: IdSet[];
  skills: (Rates & { order: string[]; priority: string[] }) | null;
  weakAgainst: { championId: number; games: number; winRate: number }[];
  strongAgainst: { championId: number; games: number; winRate: number }[];
  plan: FinalBuild;
}

export interface Asset {
  name: string;
  icon: string;
  /** Items only. */
  price?: number;
  /** Runes: end-of-game lines ("Total Damage Dealt: @eogvar1@"). */
  stats?: string[];
}

export interface GameAssets {
  perks: Record<string, Asset>;
  styles: Record<string, Asset & { slots: number[][] }>;
  items: Record<string, Asset>;
  spells: Record<string, Asset>;
}

export interface ApplyRequest {
  championId: number;
  mode: BuildMode;
  position: string;
  runePage: number | null;
  runeName: string;
  spells: number | null;
  itemSet: {
    title: string;
    starter: string;
    core: string;
    situational: string;
    consumables: string;
  } | null;
  threats: Threats;
}

export interface ApplyResult {
  runes: "saved" | "replacedCurrent" | null;
  spells: boolean;
  itemSet: boolean;
  errors: string[];
}

// ---------------------------------------------------------------- recorder

export type RecorderPreset = "light" | "balanced" | "high" | "custom";

export interface RecorderPrefs {
  enabled: boolean;
  /** Empty = the default folder (Videos\League Companion). */
  folder: string;
  preset: RecorderPreset;
  /** Custom quality: largest height (0 = window size), frames per second, Mbps. */
  height: number;
  fps: number;
  bitrate: number;
  encoder: "auto" | "software";
  capture: "window" | "screen";
  gameAudio: boolean;
  microphone: boolean;
  /** Discord's sound only (voice chat), as its own track. */
  discord: boolean;
  /** Windows endpoint id of the microphone; "" = Windows' default communication device. */
  micDevice: string;
  /** 0 = no limit. */
  diskLimitGb: number;
}

export interface RecorderPrefsView extends RecorderPrefs {
  /** The folder actually used. */
  root: string;
}

export interface RecorderStatus {
  enabled: boolean;
  recording: boolean;
  account: string;
  startedAt: number;
  path: string;
  error: string;
}

export interface Moment {
  kind: string;
  /** Seconds into the video. */
  time: number;
  gameTime: number;
  detail: string;
  ally: boolean | null;
  /** The player took part; missing on both teams' objectives in older recordings. */
  mine?: boolean;
}

export interface RecordingMeta {
  kind: "game" | "clip";
  account: string;
  riotId: string;
  champion: string;
  queue: string;
  /** Riot's queue id (0 when unknown). */
  queueId?: number;
  result: string;
  startedAt: number;
  duration: number;
  gameStart: number | null;
  width: number;
  height: number;
  fps: number;
  encoder: string;
  capture: string;
  audio: boolean;
  audioSource: string;
  /** Audio tracks in order: one source, or "mix" then each source; empty in older files. */
  audioTracks: string[];
  moments: Moment[];
  source: string;
  clipStart: number;
  /** Clips made small enough for Discord. */
  discord?: boolean;
}

export interface Recording {
  /** Relative to the recordings folder, with "/". */
  path: string;
  name: string;
  account: string;
  kind: "game" | "clip";
  size: number;
  modified: number;
  meta: RecordingMeta | null;
}

export interface RecordingsView {
  root: string;
  totalBytes: number;
  limitBytes: number;
  entries: Recording[];
}

export const events = {
  onLoginStep: (handler: (step: LoginStep) => void): Promise<UnlistenFn> =>
    listen<LoginStep>("login-step", (event) => handler(event.payload)),
  onLoginUpdate: (handler: (update: LoginUpdate | null) => void): Promise<UnlistenFn> =>
    listen<LoginUpdate | null>("login-update", (event) => handler(event.payload)),
  onUpdateStatus: (handler: (status: UpdateStatus) => void): Promise<UnlistenFn> =>
    listen<UpdateStatus>("update-status", (event) => handler(event.payload)),
  onRefreshStarted: (handler: (started: RefreshStarted) => void): Promise<UnlistenFn> =>
    listen<RefreshStarted>("refresh-started", (event) => handler(event.payload)),
  onSchedule: (handler: (schedule: Schedule) => void): Promise<UnlistenFn> =>
    listen<Schedule>("schedule", (event) => handler(event.payload)),
  onRankPending: (handler: (keys: Key[]) => void): Promise<UnlistenFn> =>
    listen<Key[]>("rank-pending", (event) => handler(event.payload)),
  onRankUpdate: (handler: (account: AccountView) => void): Promise<UnlistenFn> =>
    listen<AccountView>("rank-update", (event) => handler(event.payload)),
  onRefreshDone: (handler: (done: RefreshDone) => void): Promise<UnlistenFn> =>
    listen<RefreshDone>("refresh-done", (event) => handler(event.payload)),
  onAccountUpdate: (handler: (account: AccountView) => void): Promise<UnlistenFn> =>
    listen<AccountView>("account-update", (event) => handler(event.payload)),
  /** A new profile snapshot was stored for this account. */
  onProfileUpdate: (handler: (key: Key) => void): Promise<UnlistenFn> =>
    listen<Key>("profile-update", (event) => handler(event.payload)),
  /** Champ select started, ended, or the local player's champion or lane changed. */
  onChampSelect: (handler: (state: ChampSelect) => void): Promise<UnlistenFn> =>
    listen<ChampSelect>("champ-select", (event) => handler(event.payload)),
  /** A game started (everyone's ranks are in) or ended (null). */
  onLiveGame: (handler: (game: LiveGame | null) => void): Promise<UnlistenFn> =>
    listen<LiveGame | null>("live-game", (event) => handler(event.payload)),
  /** The recorder started or stopped recording, or its settings changed. */
  onRecorderStatus: (handler: (status: RecorderStatus) => void): Promise<UnlistenFn> =>
    listen<RecorderStatus>("recorder-status", (event) => handler(event.payload)),
  /** A recording or clip was saved or deleted. */
  onRecordingsChanged: (handler: () => void): Promise<UnlistenFn> => listen("recordings-changed", () => handler()),
  /** A game recording's end-of-game details were saved (its path). */
  onRecordingGame: (handler: (path: string) => void): Promise<UnlistenFn> => listen<string>("recording-game", (event) => handler(event.payload)),
};
