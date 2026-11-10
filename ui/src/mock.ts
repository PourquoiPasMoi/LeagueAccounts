// Development-only stand-in for the Tauri backend, used when the UI is opened
// in a plain browser (`npm run dev`).
// Never bundled into release builds. Everything lives only in memory.
import { ApiError, api, events, keyId, parseAccountLine, type AccountView, type BulkAddResult, type Key, type LoginStep, type LoginUpdate, type MatchRecord, type MatchSummary, type NewAccount, type OwnedChampion, type ProfileView, type RankPoint, type RefreshDone, type Settings, type SkinOwner, type UpdateAccount, type UpdateStatus } from "./api";
import demoChampions from "./data/mock-champions.json";

const lol = (tier: string, division: string, lp: string, finished = "Gold II", reached = finished) => ({
  tier,
  division,
  lp,
  reachedLastSeason: reached,
  finishedLastSeason: finished,
});
const tft = (tier: string, division: string, lp: string, lastSet = "Unranked") => ({ tier, division, lp, lastSet });

const sample: AccountView[] = [
  ["mainacc01", "Hextech Fox#EUW", "EUW", "412", "main", lol("Diamond", "II", "57", "Emerald I", "Diamond IV 12LP"), tft("Master", "", "142", "Diamond I")],
  ["smurfy_tr", "Kebab Enjoyer#TR1", "TR", "187", "smurf", lol("Emerald", "IV", "23", "Platinum I"), tft("Gold", "III", "64", "Gold IV")],
  ["duoqueen", "Piltover Queen#NA1", "NA", "301", "", lol("Challenger", "", "1124", "Grandmaster 640LP", "Challenger 1302LP"), tft("Unranked", "", "")],
  ["aramonly", "Snowball King#EUW", "EUW", "95", "ARAM", lol("Unranked", "", "", "Unranked"), tft("Platinum", "I", "88", "Silver I")],
  ["ironwill", "Iron Will#EUNE", "EUNE", "38", "", lol("Silver", "I", "12", "Bronze II"), tft("Iron", "II", "40")],
  ["goldenboy", "Golden Boy#KR1", "KR", "256", "Lee Sin OTP", lol("Gold", "III", "76", "Gold I", "Platinum IV"), tft("Emerald", "II", "5", "Platinum II")],
  ["gm_lux", "Lux Main#EUW", "EUW", "520", "", lol("Grandmaster", "", "512", "Master 210LP"), tft("Diamond", "IV", "0", "Emerald I")],
  ["bronzie", "Bronze Spirit#TR1", "TR", "61", "", lol("Bronze", "I", "99", "Iron I"), tft("Bronze", "IV", "17")],
  ["scholarly", "Arcane Scholar#EUNE", "EUNE", "380", "TFT main", lol("Master", "", "210", "Diamond I"), tft("Challenger", "", "1040", "Grandmaster")],
  ["tidalwave", "Tidal Wave#BR1", "BR", "143", "duo with Leo", lol("Platinum", "II", "48", "Gold I"), tft("Grandmaster", "", "520", "Master")],
  ["firstrecall", "First Recall#KR1", "KR", "30", "fresh account", lol("Iron", "IV", "7", "Unranked"), tft("Silver", "II", "36", "Bronze I")],
].map(([accountId, name, regionDisplay, level, description, lolRank, tftRank]) => ({
  accountId: accountId as string,
  name: name as string,
  riotIdNotFound: false,
  region: (regionDisplay as string).toLowerCase(),
  regionDisplay: regionDisplay as string,
  level: level as string,
  description: description as string,
  hasPassword: true,
  // Demo data: half the accounts pin a language, the rest follow the client.
  locale: (regionDisplay as string) === "TW" ? "zh_MY" : null,
  // Demo data: the Vietnamese-region account shows the skip flag.
  skipProfileUpdate: (regionDisplay as string) === "VN2",
  lol: lolRank as AccountView["lol"],
  tft: tftRank as AccountView["tft"],
  ban: accountId === "smurfy_tr" ? { until: Date.now() + (6 * 24 + 13) * 3_600_000 } : null,
}));

type Handler<T> = (payload: T) => void;
const listeners = {
  step: [] as Handler<LoginStep>[],
  progress: [] as Handler<LoginUpdate | null>[],
  pending: [] as Handler<Key[]>[],
  update: [] as Handler<AccountView>[],
  done: [] as Handler<RefreshDone>[],
  status: [] as Handler<UpdateStatus>[],
};
const unlisten = () => () => {};
const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const settings: Settings = {
  autoRefresh: true,
  intervalMinutes: 30,
  startMinimized: false,
  launchAtStartup: false,
  loginMethod: "riot",
  launchGame: true,
  autoUpdate: true,
  uiScale: 100,
  uiLanguage: "en",
  rpPerEuro: 125,
  riotClientFound: true,
  tftInstalled: true,
};

// Updates: `?scene=updating` finds one at launch (the update screen),
// `?scene=restart` installs one in the background (the restart notice).
const release = { version: "3.1.0", name: "v3.1.0", url: "https://github.com/", notes: "", publishedAt: "" };
let update: UpdateStatus = { phase: "checking", current: "3.0.3-dev", release: null, received: 0, total: 0, launch: true, seq: 1 };
const setUpdate = (change: Partial<UpdateStatus>) => {
  update = { ...update, ...change, seq: update.seq + 1 };
  listeners.status.forEach((handler) => handler(update));
};
/** About six seconds of simulated download, then installed. */
function simulateInstall(launch: boolean): void {
  const total = 14_680_064;
  setUpdate({ phase: "downloading", release, received: 0, total, launch });
  // At launch, start progressing once the update screen or the app is showing: a slow cold
  // start must not reach the launch check after the install already finished (and restarted).
  let shown = !launch;
  const timer = setInterval(() => {
    shown ||= document.querySelector("#startup-loader .update-screen") !== null || document.body.classList.contains("ready");
    if (!shown) return;
    const received = Math.min(total, update.received + total / 30);
    setUpdate({ received });
    if (received < total) return;
    clearInterval(timer);
    setUpdate({ phase: "ready" });
  }, 200);
}

/** Small seeded generator so every reload shows the same demo profile. */
function seeded(text: string): () => number {
  let seed = [...text].reduce((hash, char) => (hash * 31 + char.charCodeAt(0)) >>> 0, 7);
  return () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 2 ** 32;
  };
}

const LADDER = ["Iron", "Bronze", "Silver", "Gold", "Platinum", "Emerald", "Diamond"];
const DIVISIONS = ["IV", "III", "II", "I"];

/** Demo snapshot: a share of the demo champions, games and a month of rank history. */
function demoProfile(account: AccountView): ProfileView {
  // Two accounts were never synced, to show the empty state.
  if (["aramonly", "firstrecall"].includes(account.accountId)) {
    // Never synced, but OP.GG still lists its recent games.
    return { profile: null, history: demoHistory(account), games: demoGames(account, null) };
  }
  const random = seeded(account.accountId);
  const champions = (demoChampions as OwnedChampion[]).filter(() => random() < 0.7).map((champion) => ({
    ...champion,
    skins: champion.skins.filter(() => random() < 0.75),
  }));
  const now = Date.now();
  const matches: MatchSummary[] = Array.from({ length: 20 }, (_, index) => {
    const champion = champions[Math.floor(random() * Math.min(8, champions.length))];
    const win = random() < 0.55;
    return {
      gameId: index + 1,
      created: now - index * 5.3 * 3_600_000 - random() * 3_600_000,
      durationSeconds: 1200 + Math.floor(random() * 900),
      queueId: [420, 420, 420, 440, 450, 400, 1700][Math.floor(random() * 7)],
      mode: "CLASSIC",
      championId: champion?.id ?? 103,
      win,
      kills: Math.floor(random() * 14),
      deaths: Math.floor(random() * 9),
      assists: Math.floor(random() * 16),
      cs: 90 + Math.floor(random() * 170),
      championLevel: 12 + Math.floor(random() * 6),
    };
  });
  const record = (rank: { tier: string; division: string; lp: string }) => ({
    tier: rank.tier, division: rank.division, lp: Number.parseInt(rank.lp, 10) || 0,
    wins: 20 + Math.floor(random() * 80), losses: 20 + Math.floor(random() * 75),
  });
  return {
    profile: {
      syncedAt: now - 2 * 3_600_000,
      summonerLevel: Number(account.level) || 30,
      profileIconId: 4800 + Math.floor(random() * 60),
      ranked: [
        { queue: "RANKED_SOLO_5x5", ...record(account.lol) },
        { queue: "RANKED_FLEX_SR", ...record({ tier: "Gold", division: "II", lp: "40" }) },
        { queue: "RANKED_TFT", ...record(account.tft) },
      ],
      champions,
      matches,
      wallet: { rp: Math.floor(random() * 3000), blueEssence: Math.floor(random() * 60000) },
    },
    history: demoHistory(account),
    games: demoGames(account, champions),
  };
}

/** A season of games: detailed client games, plus older OP.GG summaries. */
function demoGames(account: AccountView, champions: OwnedChampion[] | null): MatchRecord[] {
  const random = seeded(`${account.accountId}:games`);
  const pool = (champions?.length ? champions : (demoChampions as OwnedChampion[])).slice(0, 9);
  const positions = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"];
  const main = positions[Math.floor(random() * 5)];
  const count = champions ? 140 : 10;
  return Array.from({ length: count }, (_, index): MatchRecord => {
    const champion = pool[Math.floor(random() * random() * pool.length)];
    const client = champions !== null && index < 60;
    const win = random() < 0.53;
    const minutes = 20 + Math.floor(random() * 16);
    const kills = Math.floor(random() * (win ? 13 : 8));
    const deaths = Math.floor(random() * (win ? 6 : 10));
    const assists = Math.floor(random() * 15);
    const queueId = [420, 420, 420, 440, 450, 400, 1700][Math.floor(random() * 7)];
    const teamKills = kills + assists + Math.floor(random() * 15) + 5;
    const damage = 9000 + Math.floor(random() * 30000);
    return {
      source: client ? "client" : "opgg",
      gameId: client ? 7_000_000 + count - index : 0,
      created: Date.now() - index * 7.5 * 3_600_000 - random() * 3_600_000,
      durationSeconds: client ? minutes * 60 : 0,
      queueId,
      queue: "",
      mode: queueId === 450 ? "ARAM" : "CLASSIC",
      championId: client ? champion.id : 0,
      champion: champion.name,
      win,
      remake: false,
      kills,
      deaths,
      assists,
      cs: queueId === 450 ? 40 + Math.floor(random() * 40) : 120 + Math.floor(random() * 140),
      championLevel: 12 + Math.floor(random() * 6),
      gold: client ? minutes * (330 + Math.floor(random() * 150)) : 0,
      damage: client ? damage : 0,
      damageTaken: client ? 12000 + Math.floor(random() * 25000) : 0,
      damageMitigated: 0,
      healing: 0,
      vision: client ? Math.floor(minutes * (0.4 + random() * 1.4)) : 0,
      wardsPlaced: 0,
      wardsKilled: 0,
      controlWards: 0,
      ccSeconds: 0,
      turretKills: 0,
      teamKills: client ? teamKills : 0,
      teamDamage: client ? damage * (3.2 + random() * 1.8) : 0,
      position: client && queueId !== 450 && queueId !== 1700 ? (random() < 0.6 ? main : positions[Math.floor(random() * 5)]) : "",
      items: [],
      spells: [],
      largestMultiKill: 1,
      doubleKills: random() < 0.25 ? 1 : 0,
      tripleKills: random() < 0.06 ? 1 : 0,
      quadraKills: random() < 0.015 ? 1 : 0,
      pentaKills: random() < 0.004 ? 1 : 0,
      firstBlood: random() < 0.15,
      opScoreX10: client ? null : 30 + Math.floor(random() * 60),
    };
  });
}

/** A month of LP walking up to the account's current rank, in both modes. */
function demoHistory(account: AccountView): RankPoint[] {
  const random = seeded(`${account.accountId}:history`);
  const points: RankPoint[] = [];
  for (const mode of ["lol", "tft"] as const) {
    const rank = mode === "lol" ? account.lol : account.tft;
    const tierIndex = LADDER.indexOf(rank.tier);
    const apex = ["Master", "Grandmaster", "Challenger"].includes(rank.tier);
    if (tierIndex < 0 && !apex) continue;
    let score = apex ? 2800 + (Number.parseInt(rank.lp, 10) || 0) : tierIndex * 400 + DIVISIONS.indexOf(rank.division) * 100 + (Number.parseInt(rank.lp, 10) || 0);
    const walk: number[] = [score];
    for (let index = 0; index < 34; index++) {
      score = Math.max(0, score - (random() < 0.58 ? 18 + Math.floor(random() * 8) : -(16 + Math.floor(random() * 8))));
      walk.unshift(score);
    }
    walk.forEach((value, index) => {
      const at = Date.now() - (walk.length - 1 - index) * 21 * 3_600_000;
      if (value >= 2800) {
        points.push({ at, mode, tier: rank.tier === "Master" || value < 3300 ? "Master" : rank.tier, division: "", lp: value - 2800 });
      } else {
        const tier = LADDER[Math.floor(value / 400)];
        points.push({ at, mode, tier, division: DIVISIONS[Math.floor((value % 400) / 100)], lp: value % 100 });
      }
    });
  }
  return points.sort((a, b) => a.at - b.at);
}

export function installMock(): void {
  const scene = new URLSearchParams(location.search).get("scene");
  const slowStart = scene === "slow-update" || scene === "late-update";
  const checkDelay = scene === "late-update" ? 7000 : scene === "slow-update" ? 3800 : scene === "restart" ? 1500 : 300;
  setTimeout(() => {
    if (scene === "updating" || slowStart) simulateInstall(true);
    else if (scene === "restart") setUpdate({ phase: "ready", release, launch: false });
    else setUpdate({ phase: "upToDate", release: { ...release, version: "3.0.3" } });
  }, checkDelay);
  let accounts = structuredClone(sample);
  const find = (key: Key) => accounts.find((account) => keyId(account) === keyId(key));
  const fakeFetch = (targets: AccountView[], exclusive = false) => {
    listeners.pending.forEach((handler) => handler(targets.map((account) => ({ accountId: account.accountId, region: account.region }))));
    targets.forEach((account, index) => {
      setTimeout(() => {
        const stored = find(account);
        if (stored?.name === account.name) {
          Object.assign(stored, account);
          listeners.update.forEach((handler) => handler(stored));
        }
        if (index === targets.length - 1) listeners.done.forEach((handler) => handler({ error: null, exclusive, auto: false }));
      }, 500 + index * 260);
    });
  };

  Object.assign(api, {
    bootstrap: async () => {
      if (slowStart) await wait(3200);
      return {
      accounts,
      regions: ["EUW", "EUNE", "NA", "KR", "TR", "BR", "JP", "OCE", "RU", "LAN", "LAS", "ME", "SEA", "TW", "VN"],
      locales: [
        { code: "en_US", name: "English (United States)", homePageWorks: true, launchable: true },
        { code: "zh_CN", name: "简体中文（中国大陆）", homePageWorks: false, launchable: false },
        { code: "zh_MY", name: "简体中文（马来西亚）", homePageWorks: true, launchable: true },
        { code: "zh_TW", name: "繁體中文（中國台灣）", homePageWorks: true, launchable: true },
        { code: "ja_JP", name: "日本語", homePageWorks: true, launchable: true },
        { code: "ko_KR", name: "한국어", homePageWorks: true, launchable: true },
      ],
      version: "3.0.3-dev",
      loggingAvailable: true,
      loadError: null,
      settings,
      schedule: { enabled: true, intervalMinutes: 30, nextAt: Date.now() + 23 * 60_000, lastAt: Date.now() - 7 * 60_000, running: false },
      };
    },
    appReady: async () => false,
    getSettings: async () => settings,
    updateSettings: async (input: object) => Object.assign(settings, input),
    login: async (key: Key, _tft: boolean, _closeRunning: boolean, launchBanned: boolean) => {
      const ban = accounts.find((account) => keyId(account) === keyId(key))?.ban;
      const steps: LoginStep[] = ["openClient", "signOut", "signIn", "launchGame", "updating", "launchGame", "done"];
      for (const step of steps) {
        if (ban && settings.launchGame && !launchBanned && step === "launchGame") throw new ApiError("account_banned", ban.until == null ? "" : String(ban.until));
        listeners.step.forEach((handler) => handler(step));
        if (step === "updating") {
          for (const percent of [18, 42, 67, 91]) {
            listeners.progress.forEach((handler) => handler({ percent, remainingMs: (100 - percent) * 1200 }));
            await wait(160);
          }
        }
        await wait(step === "signIn" ? 1400 : 700);
      }
      return "signedIn";
    },
    cancelLogin: async () => {},
    gameStatus: async () => ({ clientOpen: false, inGame: false, sameAccount: false }),
    updateStatus: async () => update,
    checkUpdate: async () => {
      await wait(400);
      if (update.phase === "ready" || update.phase === "downloading") return update;
      if (settings.autoUpdate) simulateInstall(false);
      else setUpdate({ phase: "available", release, launch: false });
      return update;
    },
    updateIdle: async () => true,
    restartToUpdate: async () => {
      if (update.phase !== "ready") throw new ApiError("update_not_ready");
      setUpdate({ phase: "restarting" });
      // The new version starts like a fresh launch, without the demo scene.
      setTimeout(() => location.replace(location.pathname), 800);
    },
    openRelease: async () => {},
    listAccounts: async () => accounts,
    addAccount: async (input: NewAccount) => {
      const account: AccountView = {
        accountId: input.accountId,
        name: input.name.trim(),
        riotIdNotFound: false,
        region: input.region.toLowerCase(),
        regionDisplay: input.region,
        description: input.description,
        level: "",
        hasPassword: true,
        locale: input.locale ?? null,
        skipProfileUpdate: input.skipProfileUpdate ?? false,
        lol: lol("Unranked", "", "", "N/A"),
        tft: tft("Unranked", "", "", "N/A"),
      };
      accounts.push(account);
      if (account.name) setTimeout(() => fakeFetch([{ ...account, level: "30", lol: lol("Silver", "II", "45", "Bronze I") }]), 50);
      return account;
    },
    multiAdd: async (text: string, region: string, locale: string | null): Promise<BulkAddResult> => {
      let added = 0;
      const skippedLines: number[] = [];
      for (const [index, line] of text.split("\n").entries()) {
        if (!line.trim()) continue;
        const input = parseAccountLine(line);
        if (!input || accounts.some((account) => account.accountId.toLowerCase() === input.accountId.toLowerCase() && account.region === region.toLowerCase())) {
          skippedLines.push(index);
          continue;
        }
        await api.addAccount({ ...input, region, description: "", locale });
        added++;
      }
      return { added, skippedLines };
    },
    updateAccount: async (key: Key, input: UpdateAccount) => {
      const account = find(key)!;
      const name = input.name.trim();
      const renamed = account.name !== name;
      Object.assign(account, {
        name,
        description: input.description,
        locale: input.locale,
        skipProfileUpdate: input.skipProfileUpdate ?? false,
      });
      if (renamed) {
        Object.assign(account, { riotIdNotFound: false, level: "", lol: lol("Unranked", "", "", "N/A"), tft: tft("Unranked", "", "", "N/A") });
        const updated = name ? { ...account, level: "30", lol: lol("Silver", "II", "45", "Bronze I") } : { ...account };
        setTimeout(() => fakeFetch([updated]), 50);
      }
      return account;
    },
    deleteAccount: async (key: Key) => {
      accounts = accounts.filter((account) => keyId(account) !== keyId(key));
    },
    refreshRanks: async () => {
      setTimeout(() => fakeFetch(accounts, true), 50);
      return accounts.length;
    },
    copyAccountId: async () => {},
    copyPassword: async () => 30,
    exportData: async () => "C:\\Users\\demo\\credentials.json",
    importData: async () => null,
    openProfile: async () => {},
    openLogsFolder: async () => {},
    openHomeFixAuthor: async () => {},
    setUiLanguage: async () => {},
    openPenguLoader: async () => {},
    getProfile: async (key: Key) => demoProfile(find(key)!),
    syncProfile: async (key: Key) => {
      await wait(900);
      const view = demoProfile(find(key)!);
      if (!view.profile) throw new ApiError("profile_not_signed_in");
      return { ...view, profile: { ...view.profile, syncedAt: Date.now() } };
    },
    // The demo's first account is signed in, with League open.
    sessionStatus: async () => ({ accountId: "mainacc01", clientOpen: true, inGame: false }),
    skinIndex: async () => accounts.flatMap((account): SkinOwner[] =>
      (demoProfile(account).profile?.champions ?? []).flatMap((champion) => champion.skins.map((skin) => ({
        key: { accountId: account.accountId, region: account.region },
        championId: champion.id,
        championName: champion.name,
        skinId: skin.id,
        skinName: skin.name,
        tile: skin.tile,
        rarity: skin.rarity,
      })))),
  } satisfies typeof api);
  Object.assign(events, {
    onLoginStep: async (handler: Handler<LoginStep>) => (listeners.step.push(handler), unlisten()),
    onLoginUpdate: async (handler: Handler<LoginUpdate | null>) => (listeners.progress.push(handler), unlisten()),
    onUpdateStatus: async (handler: Handler<UpdateStatus>) => (listeners.status.push(handler), unlisten()),
    onRefreshStarted: async () => unlisten(),
    onSchedule: async () => unlisten(),
    onRankPending: async (handler: Handler<Key[]>) => (listeners.pending.push(handler), unlisten()),
    onRankUpdate: async (handler: Handler<AccountView>) => (listeners.update.push(handler), unlisten()),
    onRefreshDone: async (handler: Handler<RefreshDone>) => (listeners.done.push(handler), unlisten()),
    onAccountUpdate: async () => unlisten(),
    onProfileUpdate: async () => unlisten(),
  } satisfies typeof events);
}
