// Account page (Overview / Champions / Value) and the cross-account skin finder.
import {
  ApiError,
  accountLabel,
  api,
  championIcon,
  gameImage,
  keyId,
  keyOf,
  profileIcon,
  type AccountView,
  type MatchRecord,
  type OwnedChampion,
  type Profile,
  type ProfileView,
  type RankPoint,
  type RankedQueue,
  type SeasonRank,
  type SkinOwner,
} from "./api";
import { errorText, lang, localeTag, t, tierName, type MessageKey } from "./i18n";
import { $, esc, fragment, leave, toast } from "./dom";
import { icon } from "./icons";
import { APEX, emblem, normalizeTier, palette, rankOf, type Mode } from "./ranks";
import { accountValue, pricesUpdated, skinPrice } from "./prices";
import { championIdOf, championNameOf, queueKey, statsHtml, type StatsFilters } from "./stats";
import { hideChartHover, pointsAttribute, wireChartHover, type HoverPoint } from "./charthover";

export interface ProfileContext {
  /** The latest view of the account (it may change while the page is open). */
  account: () => AccountView | undefined;
  mode: Mode;
  rpPerEuro: number;
  login: (account: AccountView) => void;
  edit: (account: AccountView) => void;
  /** Whether this account is the one signed in right now. */
  connection: (account: AccountView) => "none" | "signedIn" | "playing";
}

type Tab = "overview" | "stats" | "champions" | "value";

/** Re-render the open page's header (e.g. the connection state changed). */
let onHeadChange: (() => void) | null = null;
export const refreshProfileHead = (): void => onHeadChange?.();

let closeActive: (() => void) | null = null;

/** Refresh the open page when a new snapshot arrives for its account. */
let onSnapshot: ((id: string) => void) | null = null;
export const profileUpdated = (id: string): void => onSnapshot?.(id);
export const profileOpen = (): boolean => closeActive !== null;

const number = (value: number): string => new Intl.NumberFormat(localeTag()).format(value);
const euro = (value: number): string =>
  new Intl.NumberFormat(localeTag(), { style: "currency", currency: "EUR", maximumFractionDigits: 0 }).format(value);

function ago(at: number): string {
  const seconds = Math.round((at - Date.now()) / 1000);
  const format = new Intl.RelativeTimeFormat(localeTag(), { numeric: "auto" });
  const units: [Intl.RelativeTimeFormatUnit, number][] = [["day", 86400], ["hour", 3600], ["minute", 60]];
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) return format.format(Math.round(seconds / size), unit);
  }
  return format.format(0, "minute");
}

const describe = (error: unknown): string =>
  error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error));

// ---------------------------------------------------------------- page shell

export function openProfile(context: ProfileContext): void {
  closeActive?.();
  const initial = context.account();
  if (!initial) return;
  const id = keyId(keyOf(initial));
  let view: ProfileView = { profile: null, history: [], games: [], seasons: [] };
  const statsFilters: StatsFilters = { queue: "all", range: 20, metric: "kda" };
  let tab: Tab = "overview";
  let chartMode: Mode = context.mode;
  let selectedChampion: number | null = null;
  let championQuery = "";
  let championSort: "name" | "mastery" | "skins" = "mastery";
  let withSkins = false;
  let syncing = false;

  const overlay = fragment(`
    <div class="profile-overlay" role="dialog" aria-modal="true">
      <div class="profile-page">
        <header class="profile-head"></header>
        <nav class="profile-tabs" role="tablist">
          <button role="tab" data-tab="overview">${icon("chart-line", 15)}<span>${esc(t("profile.overview"))}</span></button>
          <button role="tab" data-tab="stats">${icon("swords", 15)}<span>${esc(t("stats.tab"))}</span></button>
          <button role="tab" data-tab="champions">${icon("users", 15)}<span>${esc(t("profile.champions"))}</span></button>
          <button role="tab" data-tab="value">${icon("gem", 15)}<span>${esc(t("profile.value"))}</span></button>
        </nav>
        <div class="profile-body"></div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  document.body.append(overlay);
  const head = $(".profile-head", overlay);
  const body = $(".profile-body", overlay);

  const account = (): AccountView => context.account() ?? initial;

  const renderHead = () => {
    const current = account();
    const profile = view.profile;
    const rank = rankOf(current, context.mode);
    const colors = palette(rank.tier);
    overlay.style.setProperty("--tier", colors.base);
    overlay.style.setProperty("--tier-light", colors.light);
    const [gameName, tag] = splitRiotId(accountLabel(current));
    const level = profile?.summonerLevel || Number(current.level) || 0;
    head.innerHTML = `
      <div class="profile-avatar">
        ${profile?.profileIconId ? `<img src="${esc(profileIcon(profile.profileIconId))}" alt="" draggable="false" />` : `<div class="avatar-emblem">${emblem(rank.tier, 72, context.mode)}</div>`}
        ${level ? `<span class="avatar-level">${esc(String(level))}</span>` : ""}
      </div>
      <div class="profile-identity">
        <h2><span class="game-name">${esc(gameName)}</span>${tag ? `<span class="tag">#${esc(tag)}</span>` : ""}</h2>
        <div class="meta">
          <span class="chip chip-region">${esc(current.regionDisplay)}</span>
          <span class="acc-id">${esc(current.accountId)}</span>
          <span class="profile-synced">${esc(profile ? t("profile.syncedAt", { time: ago(profile.syncedAt) }) : t("profile.never"))}</span>
        </div>
      </div>
      <div class="profile-actions">
        <button class="btn btn-ghost" data-profile="sync" ${syncing ? "disabled" : ""}>${icon("refresh-cw", 14)}<span>${esc(t(syncing ? "profile.syncing" : "profile.sync"))}</span></button>
        <button class="btn btn-ghost" data-profile="edit">${icon("pencil", 14)}<span>${esc(t("menu.edit"))}</span></button>
        ${loginButton(context.connection(current), "data-profile=\"login\"")}
        <button class="icon-btn profile-close" data-profile="close" aria-label="${esc(t("common.close"))}">${icon("x", 16)}</button>
      </div>`;
  };

  const renderBody = () => {
    overlay.querySelectorAll<HTMLButtonElement>("[data-tab]").forEach((button) => {
      button.setAttribute("aria-selected", String(button.dataset.tab === tab));
    });
    const champions = overlay.querySelector<HTMLElement>('[data-tab="champions"] span');
    if (champions) {
      const count = view.profile?.champions.length;
      champions.textContent = count ? `${t("profile.champions")} (${count})` : t("profile.champions");
    }
    if (tab === "overview") body.innerHTML = overviewHtml(account(), view, chartMode, context.mode);
    if (tab === "stats") body.innerHTML = statsHtml(view.games, view.profile, statsFilters);
    if (tab === "champions") body.innerHTML = championsHtml(view.profile, { selectedChampion, championQuery, championSort, withSkins });
    if (tab === "value") body.innerHTML = valueHtml(view.profile, context.rpPerEuro);
    if (tab === "champions") {
      const search = body.querySelector<HTMLInputElement>("[data-champion-search]");
      if (search && document.activeElement !== search && championQuery) {
        search.focus();
        search.setSelectionRange(championQuery.length, championQuery.length);
      }
    }
  };

  const render = () => {
    renderHead();
    renderBody();
  };

  const load = async () => {
    try {
      view = await api.getProfile(keyOf(account()));
    } catch (error) {
      toast("error", t("toast.openFailed"), describe(error), 5000);
    }
    render();
  };

  const sync = async () => {
    if (syncing) return;
    syncing = true;
    renderHead();
    try {
      view = await api.syncProfile(keyOf(account()));
      toast("success", t("profile.synced"), accountLabel(account()));
    } catch (error) {
      toast("error", t("profile.syncFailed"), describe(error), 6000);
    } finally {
      syncing = false;
      render();
    }
  };

  const close = () => {
    if (!closeActive) return;
    closeActive = null;
    onSnapshot = null;
    onHeadChange = null;
    hideChartHover();
    document.removeEventListener("keydown", onKey, true);
    void leave(overlay, "profile-out");
    previousFocus?.focus?.();
  };

  const onKey = (event: KeyboardEvent) => {
    // A dialog opened from the page (login confirm, edit) owns the keyboard.
    if (document.querySelector("#modal-root > .overlay:not(.overlay-out), .login-overlay")) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      if (selectedChampion !== null && tab === "champions") {
        selectedChampion = null;
        renderBody();
      } else {
        close();
      }
    }
  };

  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    if (target === overlay) return close();
    const action = target.closest<HTMLElement>("[data-profile]")?.dataset.profile;
    if (action === "close") return close();
    if (action === "sync") return void sync();
    if (action === "login") return context.login(account());
    if (action === "edit") return context.edit(account());
    const nextTab = target.closest<HTMLElement>("[data-tab]")?.dataset.tab as Tab | undefined;
    if (nextTab && nextTab !== tab) {
      tab = nextTab;
      body.scrollTop = 0;
      return renderBody();
    }
    const statsButton = target.closest<HTMLElement>("[data-stats-queue], [data-stats-range], [data-stats-metric]");
    if (statsButton) {
      const { statsQueue, statsRange, statsMetric } = statsButton.dataset;
      if (statsQueue) statsFilters.queue = statsQueue as StatsFilters["queue"];
      if (statsRange) statsFilters.range = Number(statsRange) as StatsFilters["range"];
      if (statsMetric) statsFilters.metric = statsMetric as StatsFilters["metric"];
      return renderBody();
    }
    if (target.closest("[data-open-stats]")) {
      tab = "stats";
      body.scrollTop = 0;
      return renderBody();
    }
    const mode = target.closest<HTMLElement>("[data-chart-mode]")?.dataset.chartMode as Mode | undefined;
    if (mode && mode !== chartMode) {
      chartMode = mode;
      return renderBody();
    }
    const sort = target.closest<HTMLElement>("[data-champion-sort]")?.dataset.championSort;
    if (sort === "name" || sort === "mastery" || sort === "skins") {
      championSort = sort;
      return renderBody();
    }
    if (target.closest("[data-champion-back]")) {
      selectedChampion = null;
      return renderBody();
    }
    const championButton = target.closest<HTMLElement>("[data-champion]");
    if (championButton) {
      selectedChampion = Number(championButton.dataset.champion);
      // A row of the value breakdown jumps to that champion's skins.
      if (championButton.dataset.tabTarget === "champions") tab = "champions";
      body.scrollTop = 0;
      return renderBody();
    }
  });
  overlay.addEventListener("input", (event) => {
    const target = event.target as HTMLInputElement;
    if (target.matches("[data-champion-search]")) {
      championQuery = target.value;
      renderBody();
    }
  });
  overlay.addEventListener("change", (event) => {
    const target = event.target as HTMLInputElement;
    if (target.matches("[data-with-skins]")) {
      withSkins = target.checked;
      renderBody();
    }
  });
  // Pictures that fail to load (offline, unknown path) fall back to a blank tile.
  overlay.addEventListener("error", (event) => {
    const image = event.target as HTMLElement;
    if (image instanceof HTMLImageElement) image.classList.add("is-missing");
  }, true);
  document.addEventListener("keydown", onKey, true);
  wireChartHover(overlay);

  closeActive = close;
  onSnapshot = (updated) => {
    if (updated === id) void load();
  };
  onHeadChange = renderHead;
  render();
  $(".profile-close", overlay).focus();
  void load();
}

export function closeProfile(): void {
  closeActive?.();
}

/** The Log in button, or the account's connection state when it is signed in. */
export function loginButton(state: "none" | "signedIn" | "playing", attributes: string): string {
  if (state === "playing") {
    return `<button class="btn btn-play is-connected" ${attributes} disabled>${icon("check", 14)}<span>${esc(t("connected.playing"))}</span></button>`;
  }
  if (state === "signedIn") {
    return `<button class="btn btn-play is-connected" ${attributes} disabled>${icon("check", 14)}<span>${esc(t("connected.button"))}</span></button>`;
  }
  return `<button class="btn btn-play" ${attributes}>${icon("login", 14)}<span>${esc(t("card.login"))}</span></button>`;
}

function splitRiotId(name: string): [string, string] {
  const index = name.lastIndexOf("#");
  return index < 0 ? [name.trim(), ""] : [name.slice(0, index).trim(), name.slice(index + 1).trim()];
}

// ---------------------------------------------------------------- overview

const QUEUE_TITLES: Record<string, MessageKey> = {
  RANKED_SOLO_5x5: "profile.q.solo",
  RANKED_FLEX_SR: "profile.q.flex",
  RANKED_TFT: "profile.q.tft",
};

function rankedCard(title: string, queue: Pick<RankedQueue, "tier" | "division" | "lp" | "wins" | "losses"> | null, mode: Mode, note = ""): string {
  const tier = normalizeTier(queue?.tier ?? "Unranked");
  const ranked = tier !== "Unranked" && tier !== "Error";
  const games = (queue?.wins ?? 0) + (queue?.losses ?? 0);
  const rate = games ? Math.round(((queue?.wins ?? 0) / games) * 100) : 0;
  const colors = palette(tier);
  const label = !ranked ? t("rank.unranked") : APEX.has(tier) || !queue?.division ? tierName(tier) : `${tierName(tier)} ${queue.division}`;
  return `
    <section class="ranked-card" style="--tier:${colors.base};--tier-light:${colors.light};--tier-dark:${colors.dark}" data-tier="${tier}">
      <h4>${esc(title)}</h4>
      <div class="ranked-card-body">
        <div class="ranked-emblem">${emblem(tier, 92, mode)}</div>
        <div>
          <div class="rank-name">${esc(label)}</div>
          ${ranked ? `<div class="lp">${esc(String(queue?.lp ?? 0))} LP</div>` : ""}
          ${games ? `
            <div class="ranked-record">${esc(t("profile.winLoss", { wins: queue?.wins ?? 0, losses: queue?.losses ?? 0 }))} · <b>${esc(t("profile.winRate", { rate }))}</b></div>
            <div class="winrate-bar"><span style="--p:${rate / 100}"></span></div>` : `<div class="ranked-record muted">${esc(t("profile.noGames"))}</div>`}
          ${note ? `<div class="ranked-note">${esc(note)}</div>` : ""}
        </div>
      </div>
    </section>`;
}

function overviewHtml(account: AccountView, view: ProfileView, chartMode: Mode, mode: Mode): string {
  const profile = view.profile;
  const queue = (name: string) => profile?.ranked.find((entry) => entry.queue === name) ?? null;
  // Without a snapshot, show what OP.GG reports for the card.
  const fallback = (rank: { tier: string; division: string; lp: string }) => ({
    tier: rank.tier, division: rank.division, lp: Number.parseInt(rank.lp, 10) || 0, wins: 0, losses: 0,
  });
  const cards = profile
    ? [
        rankedCard(t(QUEUE_TITLES.RANKED_SOLO_5x5), queue("RANKED_SOLO_5x5"), "lol"),
        rankedCard(t(QUEUE_TITLES.RANKED_FLEX_SR), queue("RANKED_FLEX_SR"), "lol"),
        rankedCard(t(QUEUE_TITLES.RANKED_TFT), queue("RANKED_TFT") ?? fallback(account.tft), "tft"),
      ]
    : [
        rankedCard(t(QUEUE_TITLES.RANKED_SOLO_5x5), fallback(account.lol), "lol", t("profile.fromOpgg")),
        rankedCard(t(QUEUE_TITLES.RANKED_TFT), fallback(account.tft), "tft", t("profile.fromOpgg")),
      ];
  return `
    ${profile ? "" : emptyNotice()}
    <div class="ranked-cards">${cards.join("")}</div>
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("profile.lpHistory"))}</h3>
        <div class="segmented">
          <button data-chart-mode="lol" aria-pressed="${chartMode === "lol"}">${esc(t("mode.lol"))}</button>
          <button data-chart-mode="tft" aria-pressed="${chartMode === "tft"}">${esc(t("mode.tft"))}</button>
        </div>
      </header>
      ${rankChart(view.history.filter((point) => point.mode === chartMode), chartMode)}
      ${historyNote(view.history.filter((point) => point.mode === chartMode))}
      ${chartMode === "lol" ? seasonHistory(view.seasons ?? []) : ""}
    </section>
    ${recentGames(view.games, profile, mode)}`;
}

/** Explain a short chart: LP history only exists from the day it was first recorded. */
function historyNote(points: RankPoint[]): string {
  if (!points.length) return "";
  const span = points.at(-1)!.at - points[0].at;
  if (span > 7 * 86_400_000) return "";
  const since = new Date(points[0].at).toLocaleDateString(localeTag(), { day: "numeric", month: "long" });
  return `<p class="chart-note">${icon("info", 13)}<span>${esc(t("profile.historyNote", { date: since }))}</span></p>`;
}

const seasonRankLabel = (tier: string, division: string, lp?: number): string => {
  const name = normalizeTier(tier);
  const base = APEX.has(name) || !division ? tierName(name) : `${tierName(name)} ${division}`;
  return lp === undefined ? base : `${base} · ${lp} LP`;
};

/** Past seasons from OP.GG: where each season ended, with its peak on hover. */
function seasonHistory(seasons: SeasonRank[]): string {
  if (!seasons.length) return "";
  return `
    <div class="season-history">
      <h4>${esc(t("profile.seasons"))}</h4>
      <ol>${seasons.map((season) => {
        const peak = season.peakTier ? t("profile.seasonPeak", { rank: seasonRankLabel(season.peakTier, season.peakDivision, season.peakLp) }) : "";
        return `
        <li style="--tier:${palette(season.tier).base}" ${peak ? `data-tooltip="${esc(peak)}"` : ""}>
          <span class="season-crest">${emblem(season.tier, 30, "lol")}</span>
          <span class="season-text"><em>${esc(season.season)}</em><b>${esc(seasonRankLabel(season.tier, season.division))}</b><span>${esc(`${season.lp} LP`)}</span></span>
        </li>`;
      }).join("")}</ol>
    </div>`;
}

function emptyNotice(): string {
  return `
    <div class="profile-empty">
      ${icon("info", 18)}
      <div><strong>${esc(t("profile.emptyTitle"))}</strong><p>${esc(t("profile.emptyText"))}</p></div>
    </div>`;
}

// ---------------------------------------------------------------- rank chart

/** Tiers from the bottom of the ladder up; four divisions of 100 LP each below Master. */
const LADDER = ["Iron", "Bronze", "Silver", "Gold", "Platinum", "Emerald", "Diamond"];
const DIVISION_STEP: Record<string, number> = { IV: 0, III: 1, II: 2, I: 3 };

function ladderScore(point: Pick<RankPoint, "tier" | "division" | "lp">): number | null {
  const tier = normalizeTier(point.tier);
  if (APEX.has(tier)) return LADDER.length * 400 + Math.max(0, point.lp);
  const index = LADDER.indexOf(tier);
  if (index < 0) return null;
  return index * 400 + (DIVISION_STEP[point.division.toUpperCase()] ?? 0) * 100 + Math.min(100, Math.max(0, point.lp));
}

function tierAt(score: number): string {
  if (score >= LADDER.length * 400) return "Master";
  return LADDER[Math.max(0, Math.floor(score / 400))];
}

function rankChart(points: RankPoint[], mode: Mode): string {
  const scored = points
    .map((point) => ({ point, score: ladderScore(point) }))
    .filter((entry): entry is { point: RankPoint; score: number } => entry.score !== null);
  if (!scored.length) {
    return `<p class="chart-empty">${icon("chart-line", 18)}<span>${esc(t("profile.lpEmpty"))}</span></p>`;
  }
  // The latest standing holds until now: draw it up to the present.
  const latest = scored.at(-1)!;
  const now = Date.now();
  const line_points = now - latest.point.at > 60_000 || scored.length === 1
    ? [...scored, { point: { ...latest.point, at: Math.max(now, latest.point.at + 60_000) }, score: latest.score }]
    : scored;
  const width = 760;
  const height = 210;
  const pad = { left: 74, right: 16, top: 14, bottom: 26 };
  const scores = scored.map((entry) => entry.score);
  let low = Math.floor((Math.min(...scores) - 40) / 100) * 100;
  let high = Math.ceil((Math.max(...scores) + 40) / 100) * 100;
  if (high - low < 200) high = low + 200;
  low = Math.max(0, low);
  const start = scored[0].point.at;
  const span = Math.max(1, line_points.at(-1)!.point.at - start);
  const x = (at: number) => pad.left + ((at - start) / span) * (width - pad.left - pad.right);
  const y = (score: number) => pad.top + (1 - (score - low) / (high - low)) * (height - pad.top - pad.bottom);
  const line = line_points.map((entry) => `${x(entry.point.at).toFixed(1)},${y(entry.score).toFixed(1)}`).join(" ");
  const area = `${pad.left},${height - pad.bottom} ${line} ${x(line_points.at(-1)!.point.at).toFixed(1)},${height - pad.bottom}`;

  // Horizontal rules at division boundaries, labelled at tier boundaries.
  const rules: string[] = [];
  const step = high - low > 1200 ? 400 : 100;
  for (let value = Math.ceil(low / step) * step; value <= high; value += step) {
    const apexStart = LADDER.length * 400;
    const tierEdge = value < apexStart ? value % 400 === 0 : value === apexStart;
    const tier = tierAt(value);
    // Above Master there are no divisions: label the LP instead.
    const label = value > apexStart
      ? t("profile.apexLp", { lp: value - apexStart })
      : tierEdge || step === 100
        ? APEX.has(tier) ? tierName(tier) : `${tierName(tier)}${tierEdge ? "" : ` ${["IV", "III", "II", "I"][(value % 400) / 100]}`}`
        : "";
    rules.push(`
      <line x1="${pad.left}" x2="${width - pad.right}" y1="${y(value).toFixed(1)}" y2="${y(value).toFixed(1)}" class="${tierEdge ? "rule-tier" : "rule"}"/>
      ${label ? `<text x="${pad.left - 8}" y="${(y(value) + 4).toFixed(1)}" text-anchor="end" style="fill:${palette(tier).base}">${esc(label)}</text>` : ""}`);
  }
  const dateFormat = new Intl.DateTimeFormat(localeTag(), { day: "numeric", month: "short" });
  const dots = scored.map((entry) => {
    const tier = normalizeTier(entry.point.tier);
    return `<circle cx="${x(entry.point.at).toFixed(1)}" cy="${y(entry.score).toFixed(1)}" r="3.2" style="fill:${palette(tier).base}"/>`;
  }).join("");
  // Hover readout: rank, LP and the change since the previous point.
  const hover: HoverPoint[] = scored.map((entry, index) => {
    const point = entry.point;
    const tier = normalizeTier(point.tier);
    const change = index ? entry.score - scored[index - 1].score : 0;
    return {
      x: Number(x(point.at).toFixed(1)),
      y: Number(y(entry.score).toFixed(1)),
      title: APEX.has(tier) ? tierName(tier) : `${tierName(tier)} ${point.division}`,
      lines: [
        `${point.lp} LP${index ? ` (${change >= 0 ? "+" : ""}${change})` : ""}`,
        new Date(point.at).toLocaleString(localeTag(), { dateStyle: "medium", timeStyle: "short" }),
      ],
      color: palette(tier).base,
    };
  });
  const last = scored.at(-1)!;
  const color = palette(last.point.tier);
  return `
    <svg class="rank-chart" viewBox="0 0 ${width} ${height}" role="img" aria-label="${esc(t("profile.lpHistory"))}" style="--line:${color.base};--line-light:${color.light}" data-mode="${mode}" data-points="${pointsAttribute(hover)}">
      <defs>
        <linearGradient id="chart-fill" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stop-color="${color.base}" stop-opacity=".28"/>
          <stop offset="1" stop-color="${color.base}" stop-opacity="0"/>
        </linearGradient>
      </defs>
      ${rules.join("")}
      <polygon points="${area}" fill="url(#chart-fill)"/>
      <polyline points="${line}" class="chart-line"/>
      ${dots}
      <g class="chart-hover" visibility="hidden"></g>
      <text x="${pad.left}" y="${height - 6}" class="axis">${esc(dateFormat.format(start))}</text>
      <text x="${width - pad.right}" y="${height - 6}" text-anchor="end" class="axis">${esc(dateFormat.format(line_points.at(-1)!.point.at))}</text>
    </svg>`;
}

// ---------------------------------------------------------------- recent games

const kda = (kills: number, deaths: number, assists: number): string =>
  deaths ? ((kills + assists) / deaths).toFixed(2) : t("profile.perfect");

function recentGames(archive: MatchRecord[], profile: Profile | null, mode: Mode): string {
  const games = archive.filter((game) => !game.remake).slice(0, 20);
  if (!games.length) {
    return `<section class="profile-panel"><h3>${esc(t("profile.recent"))}</h3><p class="muted">${esc(t("profile.recentEmpty"))}</p></section>`;
  }
  const wins = games.filter((game) => game.win).length;
  const totals = games.reduce((sum, game) => ({ k: sum.k + game.kills, d: sum.d + game.deaths, a: sum.a + game.assists }), { k: 0, d: 0, a: 0 });
  const played = new Map<string, { id: number; games: number; wins: number }>();
  games.forEach((game) => {
    const name = championNameOf(game, profile);
    const entry = played.get(name) ?? { id: championIdOf(game, profile), games: 0, wins: 0 };
    entry.games++;
    if (game.win) entry.wins++;
    played.set(name, entry);
  });
  const top = [...played.entries()].sort((a, b) => b[1].games - a[1].games).slice(0, 3);
  const rate = Math.round((wins / games.length) * 100);
  const portrait = (id: number) => id ? `<img src="${esc(championIcon(id))}" alt="" draggable="false" />` : `<span class="portrait-blank"></span>`;
  return `
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("profile.recent"))}</h3>
        <span class="muted">${esc(t("profile.recentSummary", { count: games.length, wins, losses: games.length - wins, kda: kda(totals.k, totals.d, totals.a) }))}
          · <button class="text-button" data-open-stats>${esc(t("stats.more"))}</button></span>
      </header>
      <div class="recent-summary">
        <div class="donut-wrap" data-tooltip="${esc(t("profile.recentSummary", { count: games.length, wins, losses: games.length - wins, kda: kda(totals.k, totals.d, totals.a) }))}"><div class="donut" style="--p:${rate}"></div><span>${rate}%</span></div>
        <div class="top-champions">
          ${top.map(([name, entry]) => `
            <div class="top-champion">
              ${portrait(entry.id)}
              <div><b>${esc(name)}</b>
                <span>${esc(t("profile.winLoss", { wins: entry.wins, losses: entry.games - entry.wins }))} · ${Math.round((entry.wins / entry.games) * 100)}%</span></div>
            </div>`).join("")}
        </div>
      </div>
      <ol class="match-list" data-mode="${mode}">
        ${games.map((game) => {
          const detailed = game.source === "client" && game.durationSeconds > 0;
          const kp = game.teamKills ? Math.round(((game.kills + game.assists) / game.teamKills) * 100) : null;
          return `
          <li class="match ${game.win ? "is-win" : "is-loss"}">
            <span class="match-champion">${portrait(championIdOf(game, profile))}</span>
            <div class="match-result">
              <b>${esc(t(game.win ? "profile.win" : "profile.loss"))}</b>
              <span>${esc(t(queueKey(game)))}${game.source === "opgg" ? ` · <em class="source-tag">OP.GG</em>` : ""}</span>
            </div>
            <div class="match-kda"><b>${game.kills} / <em>${game.deaths}</em> / ${game.assists}</b><span>${esc(kda(game.kills, game.deaths, game.assists))} KDA${kp !== null ? ` · ${kp}% ${esc(t("stats.kpShort"))}` : ""}</span></div>
            <div class="match-cs"><span>${esc(t("profile.cs", { cs: game.cs }))}</span><span>${detailed ? esc(t("profile.minutes", { m: Math.round(game.durationSeconds / 60) })) : game.opScoreX10 != null ? esc(`OP ${(game.opScoreX10 / 10).toFixed(1)}`) : ""}</span></div>
            <div class="match-extra">${detailed ? `<span>${esc(new Intl.NumberFormat(localeTag()).format(game.damage))} ${esc(t("stats.damageShort"))}</span><span>${esc(new Intl.NumberFormat(localeTag()).format(game.gold))} ${esc(t("stats.goldShort"))} · ${game.vision} ${esc(t("stats.visionShort"))}</span>` : ""}</div>
            <time class="match-time" datetime="${new Date(game.created).toISOString()}">${esc(ago(game.created))}</time>
          </li>`;
        }).join("")}
      </ol>
    </section>`;
}

// ---------------------------------------------------------------- champions

interface ChampionFilters {
  selectedChampion: number | null;
  championQuery: string;
  championSort: "name" | "mastery" | "skins";
  withSkins: boolean;
}

const normalize = (value: string): string =>
  value.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase().replace(/[^a-z0-9]/g, "");

function championsHtml(profile: Profile | null, filters: ChampionFilters): string {
  if (!profile) return emptyNotice();
  const selected = profile.champions.find((champion) => champion.id === filters.selectedChampion);
  if (selected) return championSkins(selected);
  const query = normalize(filters.championQuery);
  const list = profile.champions
    .filter((champion) => !filters.withSkins || champion.skins.length > 0)
    .filter((champion) => !query
      || normalize(champion.name).includes(query)
      || champion.skins.some((skin) => normalize(skin.name).includes(query)))
    .sort((a, b) => {
      if (filters.championSort === "mastery") return b.masteryPoints - a.masteryPoints || a.name.localeCompare(b.name, lang());
      if (filters.championSort === "skins") return b.skins.length - a.skins.length || a.name.localeCompare(b.name, lang());
      return a.name.localeCompare(b.name, lang());
    });
  const skinTotal = profile.champions.reduce((sum, champion) => sum + champion.skins.length, 0);
  return `
    <div class="champion-toolbar">
      <label class="search">${icon("search", 15)}
        <input data-champion-search type="text" value="${esc(filters.championQuery)}" placeholder="${esc(t("profile.search"))}" autocomplete="off" spellcheck="false" /></label>
      <div class="segmented">
        ${(["mastery", "skins", "name"] as const).map((sort) => `
          <button data-champion-sort="${sort}" aria-pressed="${filters.championSort === sort}">${esc(t(sort === "mastery" ? "profile.sortMastery" : sort === "skins" ? "profile.sortSkins" : "profile.sortName"))}</button>`).join("")}
      </div>
      <label class="check-row"><input type="checkbox" data-with-skins ${filters.withSkins ? "checked" : ""}/><span>${esc(t("profile.withSkins"))}</span></label>
      <span class="muted champion-count">${esc(t("profile.championCount", { count: profile.champions.length }))} · ${esc(t("profile.skinCount", { count: skinTotal }))}</span>
    </div>
    <div class="champion-grid">
      ${list.map((champion) => `
        <button class="champion-tile${champion.skins.length ? " has-skins" : ""}" data-champion="${champion.id}" title="${esc(`${champion.name} — ${champion.title}`)}">
          <span class="champion-portrait"><img src="${esc(championIcon(champion.id))}" alt="" loading="lazy" draggable="false" />
            ${champion.masteryLevel ? `<span class="mastery-badge">${champion.masteryLevel}</span>` : ""}</span>
          <span class="champion-name">${esc(champion.name)}</span>
          <span class="champion-skins">${champion.skins.length ? `${icon("shirt", 12)}${champion.skins.length}` : ""}</span>
        </button>`).join("")}
    </div>`;
}

function priceLabel(price: ReturnType<typeof skinPrice>): string {
  if (price.kind === "rp") return `${number(price.rp)} RP`;
  if (price.kind === "estimate") return t("profile.estimate", { rp: number(price.rp) });
  if (price.kind === "special") return t("profile.special");
  return "—";
}

function championSkins(champion: OwnedChampion): string {
  return `
    <div class="champion-detail-head">
      <button class="btn btn-ghost" data-champion-back>${icon("arrow-left", 14)}<span>${esc(t("profile.back"))}</span></button>
      <img src="${esc(championIcon(champion.id))}" alt="" draggable="false" />
      <div>
        <h3>${esc(champion.name)}</h3>
        <span class="muted">${esc(champion.title)}${champion.masteryLevel ? ` · ${esc(t("profile.mastery", { level: champion.masteryLevel }))} · ${esc(t("profile.points", { points: number(champion.masteryPoints) }))}` : ""}</span>
      </div>
    </div>
    ${champion.skins.length ? `
      <div class="skin-grid">
        ${champion.skins.map((skin) => {
          const price = skinPrice(skin);
          return `
          <figure class="skin-card rarity-${esc(skin.rarity || "none")}">
            <div class="skin-art">${skin.tile ? `<img src="${esc(gameImage(skin.tile))}" alt="" loading="lazy" draggable="false" />` : ""}</div>
            <figcaption>
              <b>${esc(skin.name)}</b>
              <span class="skin-meta">
                <span class="skin-price price-${price.kind}">${esc(priceLabel(price))}</span>
                ${skin.chromasTotal ? `<span>${esc(t("profile.chromas", { owned: skin.chromasOwned, total: skin.chromasTotal }))}</span>` : ""}
              </span>
            </figcaption>
          </figure>`;
        }).join("")}
      </div>` : `<p class="muted profile-note">${esc(t("profile.noSkins"))}</p>`}`;
}

// ---------------------------------------------------------------- value

function valueHtml(profile: Profile | null, rpPerEuro: number): string {
  if (!profile) return emptyNotice();
  const value = accountValue(profile.champions);
  const max = Math.max(1, ...value.byChampion.map((entry) => entry.rp));
  const stat = (label: string, main: string, sub = "", extra = "") => `
    <div class="value-stat ${extra}"><span>${esc(label)}</span><b>${main}</b>${sub ? `<em>${sub}</em>` : ""}</div>`;
  return `
    <div class="value-stats">
      ${stat(t("profile.valueSkins"), `${esc(number(value.skinsRp))} RP`, esc(t("profile.valueEuro", { amount: euro(value.skinsRp / rpPerEuro) })), "is-main")}
      ${stat(t("profile.skins"), esc(number(value.skinCount)), esc(t("profile.valueSpecialCount", { count: value.specialCount })))}
      ${stat(t("profile.valueChampions"), `${esc(number(value.championsRp))} RP`, esc(t("profile.championCount", { count: profile.champions.length })))}
      ${profile.wallet.rp != null || profile.wallet.blueEssence != null
        ? stat(t("profile.wallet"), `${esc(number(profile.wallet.rp ?? 0))} RP`, esc(`${number(profile.wallet.blueEssence ?? 0)} ${t("profile.be")}`))
        : ""}
    </div>
    <p class="muted profile-note">${esc(t("profile.valueRate", { rate: rpPerEuro }))} ${esc(t("profile.valueSpecialHint"))}
      ${value.estimatedCount ? esc(t("profile.valueEstimated", { count: value.estimatedCount })) : ""}
      ${esc(t("profile.pricesUpdated", { date: pricesUpdated }))}</p>
    <section class="profile-panel">
      <h3>${esc(t("profile.valueBreakdown"))}</h3>
      ${value.byChampion.length ? `
      <ol class="value-list">
        ${value.byChampion.map((entry) => `
          <li>
            <button class="value-row" data-champion="${entry.champion.id}" data-tab-target="champions">
              <img src="${esc(championIcon(entry.champion.id))}" alt="" loading="lazy" draggable="false" />
              <span class="value-name">${esc(entry.champion.name)}<em>${esc(t("profile.skinCount", { count: entry.champion.skins.length }))}${entry.special ? ` · ${esc(t("profile.valueSpecialCount", { count: entry.special }))}` : ""}</em></span>
              <span class="value-bar"><span style="--p:${entry.rp / max}"></span></span>
              <b>${esc(number(entry.rp))} RP</b>
            </button>
          </li>`).join("")}
      </ol>` : `<p class="muted">${esc(t("profile.noSkinsAtAll"))}</p>`}
    </section>`;
}

// ---------------------------------------------------------------- skin finder

export function openSkinFinder(options: {
  accounts: () => AccountView[];
  open: (account: AccountView) => void;
  login: (account: AccountView) => void;
}): void {
  closeActive?.();
  const overlay = fragment(`
    <div class="profile-overlay finder-overlay" role="dialog" aria-modal="true" aria-label="${esc(t("finder.title"))}">
      <div class="profile-page finder-page">
        <header class="finder-head">
          <h2>${icon("shirt", 18)}<span>${esc(t("finder.title"))}</span></h2>
          <label class="search">${icon("search", 15)}<input data-finder-search type="text" placeholder="${esc(t("finder.search"))}" autocomplete="off" spellcheck="false" /></label>
          <button class="icon-btn profile-close" data-profile="close" aria-label="${esc(t("common.close"))}">${icon("x", 16)}</button>
        </header>
        <div class="profile-body finder-results"><p class="muted profile-note">${esc(t("profile.syncing"))}</p></div>
      </div>
    </div>`);
  const previousFocus = document.activeElement as HTMLElement | null;
  document.body.append(overlay);
  const input = $<HTMLInputElement>("[data-finder-search]", overlay);
  const results = $(".finder-results", overlay);
  let index: SkinOwner[] = [];
  let loaded = false;

  const byKey = (owner: SkinOwner) => options.accounts().find((account) => keyId(keyOf(account)) === keyId(owner.key));

  const render = () => {
    if (!loaded) return;
    const query = normalize(input.value);
    if (!index.length) {
      results.innerHTML = `<div class="profile-empty">${icon("info", 18)}<div><p>${esc(t("finder.empty"))}</p></div></div>`;
      return;
    }
    if (!query) {
      results.innerHTML = `<p class="muted profile-note">${esc(t("finder.hint", { count: new Set(index.map((owner) => keyId(owner.key))).size }))}</p>`;
      return;
    }
    const groups = new Map<number, SkinOwner[]>();
    for (const owner of index) {
      if (!normalize(`${owner.skinName} ${owner.championName}`).includes(query) && !normalize(owner.skinName).includes(query)) continue;
      const list = groups.get(owner.skinId) ?? [];
      list.push(owner);
      groups.set(owner.skinId, list);
    }
    if (!groups.size) {
      results.innerHTML = `<p class="muted profile-note">${esc(t("finder.none", { query: input.value.trim() }))}</p>`;
      return;
    }
    results.innerHTML = `<div class="finder-list">${[...groups.values()].slice(0, 60).map((owners) => {
      const first = owners[0];
      return `
        <article class="finder-item">
          <div class="skin-art">${first.tile ? `<img src="${esc(gameImage(first.tile))}" alt="" loading="lazy" draggable="false" />` : ""}</div>
          <div class="finder-info">
            <b>${esc(first.skinName)}</b>
            <span class="muted">${esc(first.championName)} · ${esc(t("finder.owners", { count: owners.length }))}</span>
            <ul class="finder-owners">
              ${owners.map((owner) => {
                const account = byKey(owner);
                if (!account) return "";
                const id = keyId(owner.key);
                return `<li>
                  <button class="finder-account" data-finder-open="${esc(id)}">${esc(accountLabel(account))}<span class="chip chip-region">${esc(account.regionDisplay)}</span></button>
                  <button class="btn btn-play" data-finder-login="${esc(id)}">${icon("login", 13)}${esc(t("card.login"))}</button>
                </li>`;
              }).join("")}
            </ul>
          </div>
        </article>`;
    }).join("")}</div>`;
  };

  const close = () => {
    if (!closeActive) return;
    closeActive = null;
    document.removeEventListener("keydown", onKey, true);
    void leave(overlay, "profile-out");
    previousFocus?.focus?.();
  };
  const onKey = (event: KeyboardEvent) => {
    if (document.querySelector("#modal-root > .overlay:not(.overlay-out), .login-overlay")) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      close();
    }
  };
  const find = (id: string | undefined) => options.accounts().find((account) => keyId(keyOf(account)) === id);
  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    if (target === overlay || target.closest('[data-profile="close"]')) return close();
    const openId = target.closest<HTMLElement>("[data-finder-open]")?.dataset.finderOpen;
    const loginId = target.closest<HTMLElement>("[data-finder-login]")?.dataset.finderLogin;
    const account = find(openId ?? loginId);
    if (!account) return;
    if (openId) {
      close();
      options.open(account);
    } else {
      options.login(account);
    }
  });
  overlay.addEventListener("error", (event) => {
    if (event.target instanceof HTMLImageElement) event.target.classList.add("is-missing");
  }, true);
  input.addEventListener("input", render);
  document.addEventListener("keydown", onKey, true);
  closeActive = close;
  input.focus();
  void api.skinIndex().then((owners) => {
    index = owners;
    loaded = true;
    render();
  }, (error) => {
    results.innerHTML = `<p class="muted profile-note">${esc(describe(error))}</p>`;
  });
}
