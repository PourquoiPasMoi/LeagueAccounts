// Stats computed from an account's game archive (League client + OP.GG games).
import { championIcon, type MatchRecord, type Profile } from "./api";
import { localeTag, t, type MessageKey } from "./i18n";
import { esc } from "./dom";
import { icon } from "./icons";
import championIds from "./data/champion-ids.json";
import championMeta from "./data/champion-meta.json";
import { pointsAttribute, type HoverPoint } from "./charthover";

export type QueueFilter = "all" | "solo" | "flex" | "normal" | "aram" | "arena";
export type RangeFilter = 20 | 50 | 0;
export type TrendMetric = "kda" | "cs" | "damage" | "vision" | "gold";

export interface StatsFilters {
  queue: QueueFilter;
  range: RangeFilter;
  metric: TrendMetric;
}

const QUEUES: Record<Exclude<QueueFilter, "all">, number[]> = {
  solo: [420],
  flex: [440],
  normal: [400, 430, 490],
  aram: [450, 2400, 100],
  arena: [1700, 1710],
};

const NAMES = championIds as Record<string, number>;

/** Champion id for a game, from the record or (OP.GG games) its name. */
export function championIdOf(game: MatchRecord, profile: Profile | null): number {
  if (game.championId > 0) return game.championId;
  const byName = NAMES[game.champion];
  if (byName) return byName;
  const owned = profile?.champions.find((champion) => champion.name.toLowerCase() === game.champion.toLowerCase());
  return owned?.id ?? 0;
}

export function championNameOf(game: MatchRecord, profile: Profile | null): string {
  if (game.champion) return game.champion;
  const id = game.championId;
  return profile?.champions.find((champion) => champion.id === id)?.name
    ?? Object.entries(NAMES).find(([, value]) => value === id)?.[0]
    ?? `#${id}`;
}

export function queueKey(game: MatchRecord): MessageKey {
  const id = game.queueId;
  if (id === 420) return "profile.q.solo";
  if (id === 440) return "profile.q.flex";
  if ([400, 430, 490].includes(id)) return "profile.q.normal";
  if ([450, 2400, 100].includes(id)) return "profile.q.aram";
  if ([1700, 1710].includes(id)) return "profile.q.arena";
  if (id >= 800 && id < 900) return "profile.q.bots";
  return "profile.q.other";
}

export function filterGames(games: MatchRecord[], filters: Pick<StatsFilters, "queue" | "range">): MatchRecord[] {
  const list = games
    .filter((game) => !game.remake)
    .filter((game) => filters.queue === "all" || QUEUES[filters.queue].includes(game.queueId));
  return filters.range ? list.slice(0, filters.range) : list;
}

const number = (value: number, digits = 0): string =>
  new Intl.NumberFormat(localeTag(), { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(value);
const percent = (value: number): string => `${Math.round(value * 100)}%`;
const minutes = (game: MatchRecord): number => Math.max(1, game.durationSeconds / 60);
const kdaOf = (k: number, d: number, a: number): number => (d ? (k + a) / d : k + a);

/** Mean of a per-game value over the games that have it. */
function mean(games: MatchRecord[], value: (game: MatchRecord) => number | null): { value: number; count: number } | null {
  const values = games.map(value).filter((entry): entry is number => entry !== null && Number.isFinite(entry));
  if (!values.length) return null;
  return { value: values.reduce((sum, entry) => sum + entry, 0) / values.length, count: values.length };
}

/** Client games and OP.GG match-list games have duration and full stats; games
 * from an OP.GG page summary only K/D/A and CS. */
const detailed = (game: MatchRecord): boolean => game.durationSeconds > 0;

function metricValue(game: MatchRecord, metric: TrendMetric): number | null {
  if (metric === "kda") return kdaOf(game.kills, game.deaths, game.assists);
  if (!detailed(game)) return null;
  if (metric === "cs") return game.cs / minutes(game);
  if (metric === "damage") return game.damage / minutes(game);
  if (metric === "vision") return game.vision / minutes(game);
  return game.gold / minutes(game);
}

// ---------------------------------------------------------------- html

const METRIC_KEYS: Record<TrendMetric, MessageKey> = {
  kda: "stats.kda", cs: "stats.csMin", damage: "stats.damageMin", vision: "stats.visionMin", gold: "stats.goldMin",
};

export function statsHtml(all: MatchRecord[], profile: Profile | null, filters: StatsFilters): string {
  const segmented = <T extends string | number>(name: string, value: T, options: [T, string][]) => `
    <div class="segmented">${options.map(([option, label]) => `
      <button data-stats-${name}="${option}" aria-pressed="${option === value}">${esc(label)}</button>`).join("")}</div>`;
  const queues: [QueueFilter, string][] = [
    ["all", t("stats.allQueues")], ["solo", t("profile.q.solo")], ["flex", t("profile.q.flex")],
    ["normal", t("profile.q.normal")], ["aram", t("profile.q.aram")], ["arena", t("profile.q.arena")],
  ];
  const toolbar = `
    <div class="stats-toolbar">
      ${segmented("queue", filters.queue, queues)}
      ${segmented("range", filters.range, [[0, t("stats.allGames")], [50, t("stats.last", { count: 50 })], [20, t("stats.last", { count: 20 })]])}
    </div>`;
  const games = filterGames(all, filters);
  if (!all.length) {
    return `${toolbar}<div class="profile-empty">${icon("info", 18)}<div><strong>${esc(t("stats.emptyTitle"))}</strong><p>${esc(t("stats.emptyText"))}</p></div></div>`;
  }
  if (!games.length) return `${toolbar}<p class="muted profile-note">${esc(t("stats.noneForFilter"))}</p>`;

  const rich = games.filter(detailed);
  const note = rich.length < games.length
    ? `<p class="muted profile-note">${esc(t("stats.detailNote", { detailed: rich.length, total: games.length }))}</p>`
    : "";
  const scope = queues.find(([option]) => option === filters.queue)?.[1] ?? "";

  return `
    ${toolbar}
    ${playStylePanel(games, profile, scope)}
    ${note}
    <div class="opgg-grid">
      <div class="opgg-col">
        ${performancePanel(games)}
        ${roleSharePanel(games)}
        ${classesPanel(games, profile)}
        ${poolPanel(games, profile)}
      </div>
      <div class="opgg-col">
        ${sidesPanel(games)}
        ${activityPanel(games)}
        ${metricsPanel(games)}
      </div>
    </div>
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("stats.trend"))}</h3>
        ${segmented("metric", filters.metric, [
          ["kda", t("stats.kda")], ["cs", t("stats.csMin")], ["damage", t("stats.damageMin")],
          ["gold", t("stats.goldMin")], ["vision", t("stats.visionMin")],
        ])}
      </header>
      ${trendChart(games, filters.metric, profile)}
    </section>
    <div class="stats-columns">
      <div class="stats-main">
        ${championTable(games, profile)}
      </div>
      <div class="stats-side">
        ${streaksPanel(games)}
        ${recordsPanel(games, profile)}
      </div>
    </div>
    ${formPanel(games, profile)}`;
}

// ---------------------------------------------------------------- trend chart

function trendChart(newestFirst: MatchRecord[], metric: TrendMetric, profile: Profile | null): string {
  const games = [...newestFirst].reverse();
  const points = games
    .map((game, index) => ({ game, index, value: metricValue(game, metric) }))
    .filter((point): point is { game: MatchRecord; index: number; value: number } => point.value !== null);
  if (points.length < 2) return `<p class="chart-empty">${icon("chart-line", 18)}<span>${esc(t("stats.trendEmpty"))}</span></p>`;
  const width = 760;
  const height = 190;
  const pad = { left: 44, right: 14, top: 12, bottom: 22 };
  const values = points.map((point) => point.value);
  const high = Math.max(...values) * 1.08 || 1;
  const x = (index: number) => pad.left + (index / Math.max(1, games.length - 1)) * (width - pad.left - pad.right);
  const y = (value: number) => pad.top + (1 - value / high) * (height - pad.top - pad.bottom);
  // Rolling average over five games smooths single outliers.
  const rolling = points.map((point, index) => {
    const window = points.slice(Math.max(0, index - 4), index + 1);
    return { index: point.index, value: window.reduce((sum, entry) => sum + entry.value, 0) / window.length };
  });
  const average = values.reduce((sum, value) => sum + value, 0) / values.length;
  const ticks = [0, high / 2, high].map((value) => `
    <line x1="${pad.left}" x2="${width - pad.right}" y1="${y(value).toFixed(1)}" y2="${y(value).toFixed(1)}" class="rule"/>
    <text x="${pad.left - 6}" y="${(y(value) + 4).toFixed(1)}" text-anchor="end" class="axis">${esc(number(value, value < 10 ? 1 : 0))}</text>`).join("");
  const bars = points.map((point, order) => {
    const barWidth = Math.max(3, (width - pad.left - pad.right) / games.length - 3);
    return `<rect x="${(x(point.index) - barWidth / 2).toFixed(1)}" y="${y(point.value).toFixed(1)}" width="${barWidth.toFixed(1)}" height="${(height - pad.bottom - y(point.value)).toFixed(1)}" data-i="${order}" class="${point.game.win ? "bar-win" : "bar-loss"}"/>`;
  }).join("");
  // Hover readout: the game's result, the metric, K/D/A and when it was played.
  const hover: HoverPoint[] = points.map((point, order) => {
    const game = point.game;
    return {
      x: Number(x(point.index).toFixed(1)),
      y: Number(y(rolling[order].value).toFixed(1)),
      title: `${championNameOf(game, profile)} · ${t(game.win ? "profile.win" : "profile.loss")}`,
      lines: [
        `${t(METRIC_KEYS[metric])}: ${number(point.value, point.value < 10 ? 2 : 0)}`,
        `${t("stats.rolling")}: ${number(rolling[order].value, rolling[order].value < 10 ? 2 : 0)}`,
        `${game.kills} / ${game.deaths} / ${game.assists} · ${t(queueKey(game))}`,
        new Date(game.created).toLocaleString(localeTag(), { dateStyle: "medium", timeStyle: "short" }),
      ],
      color: game.win ? "var(--win)" : "var(--loss)",
    };
  });
  const line = rolling.map((point) => `${x(point.index).toFixed(1)},${y(point.value).toFixed(1)}`).join(" ");
  return `
    <svg class="trend-chart" viewBox="0 0 ${width} ${height}" role="img" aria-label="${esc(t("stats.trend"))}" data-points="${pointsAttribute(hover)}">
      ${ticks}
      ${bars}
      <line x1="${pad.left}" x2="${width - pad.right}" y1="${y(average).toFixed(1)}" y2="${y(average).toFixed(1)}" class="avg-line"/>
      <polyline points="${line}" class="trend-line"/>
      <g class="chart-hover" visibility="hidden"></g>
      <text x="${pad.left}" y="${height - 5}" class="axis">${esc(t("stats.older"))}</text>
      <text x="${width - pad.right}" y="${height - 5}" text-anchor="end" class="axis">${esc(t("stats.newer"))}</text>
    </svg>
    <div class="chart-legend"><span class="legend-win"></span>${esc(t("profile.win"))}<span class="legend-loss"></span>${esc(t("profile.loss"))}<span class="legend-line"></span>${esc(t("stats.rolling"))}<span class="legend-avg"></span>${esc(t("stats.average", { value: number(average, 2) }))}</div>`;
}

// ---------------------------------------------------------------- champions

function championTable(games: MatchRecord[], profile: Profile | null): string {
  const groups = new Map<string, MatchRecord[]>();
  games.forEach((game) => {
    const name = championNameOf(game, profile);
    groups.set(name, [...(groups.get(name) ?? []), game]);
  });
  const rows = [...groups.entries()]
    .map(([name, list]) => {
      const wins = list.filter((game) => game.win).length;
      const k = list.reduce((sum, game) => sum + game.kills, 0);
      const d = list.reduce((sum, game) => sum + game.deaths, 0);
      const a = list.reduce((sum, game) => sum + game.assists, 0);
      const rich = list.filter(detailed);
      const cs = mean(rich, (game) => game.cs / minutes(game));
      const damage = mean(rich, (game) => game.damage / minutes(game));
      return { name, id: championIdOf(list[0], profile), games: list.length, wins, kda: kdaOf(k, d, a), cs, damage, k, d, a };
    })
    .sort((left, right) => right.games - left.games || right.wins / right.games - left.wins / left.games);
  return `
    <section class="profile-panel">
      <header class="panel-head"><h3>${esc(t("stats.champions"))}</h3><span class="muted">${esc(t("stats.championsCount", { count: rows.length }))}</span></header>
      <div class="table-scroll">
      <table class="champion-stats">
        <thead><tr>
          <th>${esc(t("profile.champions"))}</th><th>${esc(t("stats.games"))}</th><th>${esc(t("stats.winRate"))}</th>
          <th>${esc(t("stats.kda"))}</th><th>${esc(t("stats.csMin"))}</th><th>${esc(t("stats.damageMin"))}</th>
        </tr></thead>
        <tbody>${rows.map((row) => `
          <tr data-tooltip="${esc(`${row.name}: ${t("profile.winLoss", { wins: row.wins, losses: row.games - row.wins })} · ${number(row.k / row.games, 1)} / ${number(row.d / row.games, 1)} / ${number(row.a / row.games, 1)}`)}">
            <td><span class="champion-cell">${row.id ? `<img src="${esc(championIcon(row.id))}" alt="" loading="lazy" draggable="false" />` : ""}${esc(row.name)}</span></td>
            <td>${row.games}</td>
            <td><span class="rate-cell"><span class="rate-bar"><span style="--p:${row.wins / row.games}"></span></span>${esc(percent(row.wins / row.games))}</span></td>
            <td class="${row.kda >= 3 ? "is-good" : row.kda < 2 ? "is-bad" : ""}">${esc(number(row.kda, 2))}</td>
            <td>${row.cs ? esc(number(row.cs.value, 1)) : "—"}</td>
            <td>${row.damage ? esc(number(row.damage.value)) : "—"}</td>
          </tr>`).join("")}
        </tbody>
      </table>
      </div>
    </section>`;
}

// ---------------------------------------------------------------- roles, streaks, weekdays

const ROLES: [string, MessageKey][] = [
  ["TOP", "stats.role.top"], ["JUNGLE", "stats.role.jungle"], ["MIDDLE", "stats.role.mid"],
  ["BOTTOM", "stats.role.bot"], ["UTILITY", "stats.role.support"],
];

function streaksPanel(games: MatchRecord[]): string {
  const chronological = [...games].reverse();
  let best = 0;
  let worst = 0;
  let run = 0;
  chronological.forEach((game) => {
    run = game.win ? Math.max(1, run + 1) : Math.min(-1, run - 1);
    best = Math.max(best, run);
    worst = Math.min(worst, run);
  });
  const current = games.length ? (games[0].win ? t("stats.winStreak", { count: Math.max(run, 0) }) : t("stats.lossStreak", { count: Math.abs(Math.min(run, 0)) })) : "";
  const sum = (field: keyof MatchRecord) => games.reduce((total, game) => total + Number(game[field] || 0), 0);
  const multis: [string, number][] = [
    [t("stats.double"), sum("doubleKills")], [t("stats.triple"), sum("tripleKills")],
    [t("stats.quadra"), sum("quadraKills")], [t("stats.penta"), sum("pentaKills")],
    [t("stats.firstBlood"), games.filter((game) => game.firstBlood).length],
  ];
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.streaks"))}</h3>
      <div class="streak-row">
        <div><span>${esc(t("stats.current"))}</span><b class="${games[0]?.win ? "is-good" : "is-bad"}">${esc(current)}</b></div>
        <div><span>${esc(t("stats.bestStreak"))}</span><b class="is-good">${best}</b></div>
        <div><span>${esc(t("stats.worstStreak"))}</span><b class="is-bad">${Math.abs(worst)}</b></div>
      </div>
      <ul class="multi-list">${multis.map(([label, count]) => `<li><b>${count}</b><span>${esc(label)}</span></li>`).join("")}</ul>
    </section>`;
}

// ---------------------------------------------------------------- form and records

/** The last games as a strip of results, newest on the right. */
function formPanel(games: MatchRecord[], profile: Profile | null): string {
  const recent = games.slice(0, 30).reverse();
  const wins = recent.filter((game) => game.win).length;
  return `
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("stats.form"))}</h3>
        <span class="muted">${esc(t("profile.winLoss", { wins, losses: recent.length - wins }))}</span>
      </header>
      <div class="form-strip" style="--n:${Math.max(recent.length, 10)}">${recent.map((game) => `
        <span class="form-game ${game.win ? "is-win" : "is-loss"}" data-tooltip="${esc(`${championNameOf(game, profile)} · ${game.kills}/${game.deaths}/${game.assists} · ${t(queueKey(game))} · ${new Date(game.created).toLocaleDateString(localeTag())}`)}">
          <span class="form-icon">${championIdOf(game, profile) ? `<img src="${esc(championIcon(championIdOf(game, profile)))}" alt="" loading="lazy" draggable="false" />` : ""}</span>
          <em>${esc(t(game.win ? "stats.winShort" : "stats.lossShort"))}</em>
        </span>`).join("")}
      </div>
    </section>`;
}

/** Best single games: most kills, best KDA, most damage, ... */
function recordsPanel(games: MatchRecord[], profile: Profile | null): string {
  type Record = { label: string; game: MatchRecord; value: string };
  const best = (label: string, score: (game: MatchRecord) => number | null, format: (value: number, game: MatchRecord) => string): Record | null => {
    let top: { game: MatchRecord; value: number } | null = null;
    for (const game of games) {
      const value = score(game);
      if (value === null || !Number.isFinite(value) || value <= 0) continue;
      if (!top || value > top.value) top = { game, value };
    }
    return top ? { label, game: top.game, value: format(top.value, top.game) } : null;
  };
  const records = [
    best(t("stats.recordKills"), (game) => game.kills, (value) => number(value)),
    best(t("stats.recordKda"), (game) => kdaOf(game.kills, game.deaths, game.assists), (value, game) => `${number(value, 2)} (${game.kills}/${game.deaths}/${game.assists})`),
    best(t("stats.recordDamage"), (game) => (detailed(game) ? game.damage : null), (value) => number(value)),
    best(t("stats.recordCs"), (game) => (detailed(game) ? game.cs / minutes(game) : null), (value, game) => `${number(value, 1)} (${game.cs})`),
    best(t("stats.recordGold"), (game) => (detailed(game) ? game.gold : null), (value) => number(value)),
    best(t("stats.recordVision"), (game) => (detailed(game) ? game.vision : null), (value) => number(value)),
    best(t("stats.recordLongest"), (game) => (detailed(game) ? game.durationSeconds : null), (value) => t("profile.minutes", { m: Math.round(value / 60) })),
  ].filter((record): record is Record => record !== null);
  if (!records.length) return "";
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.records"))}</h3>
      <ul class="records">${records.map((record) => {
        const id = championIdOf(record.game, profile);
        return `
        <li data-tooltip="${esc(`${championNameOf(record.game, profile)} · ${t(record.game.win ? "profile.win" : "profile.loss")} · ${new Date(record.game.created).toLocaleDateString(localeTag())}`)}">
          ${id ? `<img src="${esc(championIcon(id))}" alt="" loading="lazy" draggable="false" />` : `<span class="portrait-blank"></span>`}
          <span class="record-label">${esc(record.label)}</span>
          <b>${esc(record.value)}</b>
        </li>`;
      }).join("")}</ul>
    </section>`;
}

// ---------------------------------------------------------------- OP.GG-style play style panels

const META = championMeta as Record<string, { n: string; c: string; d: string; p: string[] }>;
const TOTAL_CHAMPIONS = Object.keys(META).length;
const CLASSES = ["Mage", "Fighter", "Tank", "Assassin", "Marksman", "Support"] as const;
const CLASS_KEYS: Record<(typeof CLASSES)[number], MessageKey> = {
  Mage: "stats.class.mage", Fighter: "stats.class.fighter", Tank: "stats.class.tank",
  Assassin: "stats.class.assassin", Marksman: "stats.class.marksman", Support: "stats.class.support",
};
const ROLE_COLORS: Record<string, string> = {
  TOP: "#c89b3c", JUNGLE: "#0ac8b9", MIDDLE: "#cdbe91", BOTTOM: "#0397ab", UTILITY: "#a0784a",
};

/** Lane glyphs drawn in the client's thin-line style (24px box). */
const ROLE_GLYPHS: Record<string, string> = {
  TOP: '<path d="M4 4h13l-3 3H7v7l-3 3z"/><rect x="10" y="10" width="4" height="4"/><path d="M20 7v13H7l3-3h7V10z" opacity=".35"/>',
  JUNGLE: '<path d="M7 3c1 5 3 8 5 10-1-4-1-7 0-10 1 3 1 6 0 10 2-2 4-5 5-10 0 6-2 11-5 18C9 14 7 9 7 3z"/>',
  MIDDLE: '<path d="M4 4h6L4 10zM20 20h-6l6-6z" opacity=".35"/><path d="M17 4h3v3L7 20H4v-3z"/>',
  BOTTOM: '<path d="M20 20H7l3-3h7v-7l3-3z"/><rect x="10" y="10" width="4" height="4"/><path d="M4 17V4h13l-3 3H7v7z" opacity=".35"/>',
  UTILITY: '<path d="M12 7l2.5 3-2.5 9-2.5-9zM3 7h6l1.5 2-2.5 3-2-1zM21 7h-6l-1.5 2 2.5 3 2-1zM10 4h4l-2 2z"/>',
};
const roleGlyph = (role: string, size = 22): string =>
  `<svg class="role-glyph" viewBox="0 0 24 24" width="${size}" height="${size}" aria-hidden="true" fill="currentColor">${ROLE_GLYPHS[role] ?? ""}</svg>`;

const clamp = (value: number): number => Math.max(0, Math.min(1, value));
const clock = (seconds: number): string => {
  const total = Math.round(seconds);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
};

interface Summary {
  games: number;
  wins: number;
  rate: number;
  kda: number;
  kp: number | null;
  share: number | null;
  dpm: number | null;
  csm: number | null;
  gpm: number | null;
  vspm: number | null;
  length: number | null;
}

function summarize(list: MatchRecord[]): Summary {
  const n = Math.max(1, list.length);
  const k = list.reduce((sum, game) => sum + game.kills, 0);
  const d = list.reduce((sum, game) => sum + game.deaths, 0);
  const a = list.reduce((sum, game) => sum + game.assists, 0);
  const rich = list.filter(detailed);
  const wins = list.filter((game) => game.win).length;
  return {
    games: list.length,
    wins,
    rate: wins / n,
    kda: kdaOf(k, d, a),
    kp: mean(rich.filter((game) => game.teamKills > 0), (game) => (game.kills + game.assists) / game.teamKills)?.value ?? null,
    share: mean(rich.filter((game) => game.teamDamage > 0), (game) => game.damage / game.teamDamage)?.value ?? null,
    dpm: mean(rich, (game) => game.damage / minutes(game))?.value ?? null,
    csm: mean(rich, (game) => game.cs / minutes(game))?.value ?? null,
    gpm: mean(rich, (game) => game.gold / minutes(game))?.value ?? null,
    vspm: mean(rich, (game) => game.vision / minutes(game))?.value ?? null,
    length: mean(rich, (game) => game.durationSeconds)?.value ?? null,
  };
}

/** Games per champion, most played first. */
function championCounts(games: MatchRecord[], profile: Profile | null): { id: number; name: string; games: MatchRecord[] }[] {
  const groups = new Map<string, { id: number; name: string; games: MatchRecord[] }>();
  games.forEach((game) => {
    const name = championNameOf(game, profile);
    const group = groups.get(name) ?? { id: championIdOf(game, profile), name, games: [] };
    group.games.push(game);
    groups.set(name, group);
  });
  return [...groups.values()].sort((left, right) => right.games.length - left.games.length);
}

const focusShare = (counts: { games: MatchRecord[] }[], top: number, total: number): number =>
  counts.slice(0, top).reduce((sum, entry) => sum + entry.games.length, 0) / Math.max(1, total);

const panel = (title: string, body: string, aside = ""): string => `
  <section class="profile-panel">
    <header class="panel-head"><h3>${esc(title)}</h3>${aside ? `<span class="muted">${esc(aside)}</span>` : ""}</header>
    ${body}
  </section>`;

const chip = (text: string, tone: string): string => `<em class="delta-chip ${tone}">${esc(text)}</em>`;

function playStylePanel(games: MatchRecord[], profile: Profile | null, scope: string): string {
  const summary = summarize(games);
  const rich = games.filter(detailed);
  const seconds = rich.reduce((sum, game) => sum + game.durationSeconds, 0);
  const counts = championCounts(games, profile);
  const tile = (label: string, main: string, sub = "", tone = "") =>
    `<div class="style-tile"><span>${esc(label)}</span><b class="${tone}">${esc(main)}</b>${sub ? `<em>${esc(sub)}</em>` : ""}</div>`;
  const body = `
    <div class="style-tiles">
      ${tile(t("stats.games"), number(summary.games), t("profile.winLoss", { wins: summary.wins, losses: summary.games - summary.wins }))}
      ${tile(t("stats.winRate"), percent(summary.rate), "", summary.rate >= 0.5 ? "is-good" : "is-bad")}
      ${tile(t("stats.kda"), `${number(summary.kda, 2)}:1`)}
      ${tile(t("stats.playTime"), seconds ? `${Math.floor(seconds / 3600)}h ${Math.round((seconds % 3600) / 60)}m` : "—", summary.length ? t("stats.avgLength", { time: clock(summary.length) }) : "")}
      ${tile(t("stats.championPool"), `${counts.length} / ${TOTAL_CHAMPIONS}`, t("stats.top3Share", { share: percent(focusShare(counts, 3, games.length)) }))}
    </div>`;
  return panel(t("stats.playStyle"), body, `${scope} · ${t("stats.gamesCount", { count: games.length })}`);
}

/** Last 20 games against the whole list, as OP.GG's Performance box. */
function performancePanel(games: MatchRecord[]): string {
  if (games.length <= 20) return "";
  const recent = summarize(games.slice(0, 20));
  const all = summarize(games);
  const item = (label: string, value: number | null, base: number | null, scale: number, format: (value: number) => string, delta: (diff: number) => string, higherIsBetter: boolean | null) => {
    if (value === null || base === null) return "";
    const diff = value - base;
    const tone = higherIsBetter === null || Math.abs(diff) < 1e-9 ? "" : (diff > 0) === higherIsBetter ? "is-good" : "is-bad";
    return `
      <div class="perf-item">
        <span>${esc(label)}</span>
        <div class="perf-value"><b>${esc(format(value))}</b>${chip(delta(diff), tone || "is-neutral")}</div>
        <div class="perf-bar ${tone}"><span style="--p:${clamp(value / scale)}"></span><i style="--m:${clamp(base / scale)}"></i></div>
        <small>${esc(t("stats.allValue", { value: format(base) }))}</small>
      </div>`;
  };
  const signed = (value: number, text: string) => `${value > 0 ? "+" : value < 0 ? "−" : "±"}${text}`;
  const points = (diff: number) => signed(diff, `${Math.abs(Math.round(diff * 100))}%p`);
  const body = `
    <div class="perf-grid">
      ${item(t("stats.winRate"), recent.rate, all.rate, 1, percent, points, true)}
      ${item(t("stats.avgKda"), recent.kda, all.kda, Math.max(5, recent.kda, all.kda) * 1.1, (value) => number(value, 2), (diff) => signed(diff, number(Math.abs(diff), 2)), true)}
      ${item(t("stats.kp"), recent.kp, all.kp, 1, percent, points, true)}
      ${item(t("stats.length"), recent.length, all.length, Math.max(2400, recent.length ?? 0, all.length ?? 0) * 1.05, clock, (diff) => signed(diff, clock(Math.abs(diff))), null)}
    </div>`;
  return panel(t("stats.performance"), body, t("stats.performanceHint", { count: games.length }));
}

function sidesPanel(games: MatchRecord[]): string {
  const blue = summarize(games.filter((game) => game.side === "blue"));
  const red = summarize(games.filter((game) => game.side === "red"));
  const total = blue.games + red.games;
  if (!total) return "";
  const lead = blue.rate - red.rate;
  const leader = lead >= 0 ? t("stats.blue") : t("stats.red");
  const split = blue.games && red.games ? blue.rate / Math.max(1e-9, blue.rate + red.rate) : blue.games ? 1 : 0;
  const body = `
    <div class="side-head">
      <div class="is-blue"><span>${esc(t("stats.blue"))}</span><b>${blue.games ? esc(percent(blue.rate)) : "—"}</b><em>${esc(t("profile.winLoss", { wins: blue.wins, losses: blue.games - blue.wins }))}</em></div>
      <div class="side-mid"><span>${esc(t("stats.winRate"))}</span>${blue.games && red.games ? chip(`${leader} +${Math.abs(Math.round(lead * 1000) / 10)}%p`, lead >= 0 ? "is-blue" : "is-red") : ""}</div>
      <div class="is-red"><span>${esc(t("stats.red"))}</span><b>${red.games ? esc(percent(red.rate)) : "—"}</b><em>${esc(t("profile.winLoss", { wins: red.wins, losses: red.games - red.wins }))}</em></div>
    </div>
    <div class="side-bar"><span class="is-blue" style="--p:${split}"></span><span class="is-red"></span></div>
    <h4 class="sub-head">${esc(t("stats.playShare"))}</h4>
    <ul class="share-list">
      <li><span>${esc(t("stats.blue"))}</span><span class="share-bar is-blue"><span style="--p:${blue.games / total}"></span></span><b>${esc(percent(blue.games / total))}</b></li>
      <li><span>${esc(t("stats.red"))}</span><span class="share-bar is-red"><span style="--p:${red.games / total}"></span></span><b>${esc(percent(red.games / total))}</b></li>
    </ul>`;
  return panel(t("stats.sideStats"), body);
}

function roleSharePanel(games: MatchRecord[]): string {
  const withRole = games.filter((game) => game.position);
  if (!withRole.length) return "";
  const roles = ROLES.map(([role, label]) => ({ role, label, games: withRole.filter((game) => game.position === role) }));
  const main = roles.reduce((best, entry) => (entry.games.length > best.games.length ? entry : best), roles[0]);
  const body = `
    <div class="role-bar">${roles.filter((entry) => entry.games.length).map((entry) =>
      `<span style="--p:${entry.games.length / withRole.length};--c:${ROLE_COLORS[entry.role]}" data-tooltip="${esc(`${t(entry.label)}: ${percent(entry.games.length / withRole.length)}`)}"></span>`).join("")}</div>
    <div class="role-boxes">${roles.map((entry) => {
      const wins = entry.games.filter((game) => game.win).length;
      return `
      <div class="role-box${entry === main ? " is-main" : ""}" style="--c:${ROLE_COLORS[entry.role]}" data-tooltip="${esc(`${t(entry.label)}: ${t("profile.winLoss", { wins, losses: entry.games.length - wins })}${entry.games.length ? ` · ${percent(wins / entry.games.length)}` : ""}`)}">
        ${roleGlyph(entry.role)}
        <b>${esc(percent(entry.games.length / withRole.length))}</b>
        <em>${esc(t("stats.gamesShort", { count: entry.games.length }))}</em>
      </div>`;
    }).join("")}</div>`;
  return panel(t("stats.roleShare"), body);
}

function classesPanel(games: MatchRecord[], profile: Profile | null): string {
  const counts = new Map<string, number>();
  let known = 0;
  games.forEach((game) => {
    const kind = META[String(championIdOf(game, profile))]?.c;
    if (!kind) return;
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
    known++;
  });
  if (!known) return "";
  const rows = CLASSES.map((kind) => ({ kind, count: counts.get(kind) ?? 0 }))
    .filter((row) => row.count)
    .sort((left, right) => right.count - left.count);
  const body = `
    <ul class="class-list">${rows.map((row) => `
      <li><span>${esc(t(CLASS_KEYS[row.kind]))}</span><span class="share-bar"><span style="--p:${row.count / known}"></span></span><b>${esc(percent(row.count / known))}</b></li>`).join("")}
    </ul>`;
  return panel(t("stats.classes"), body);
}

function poolPanel(games: MatchRecord[], profile: Profile | null): string {
  const counts = championCounts(games, profile);
  const total = games.length;
  const focus = (label: string, share: number) => `
    <li><span>${esc(label)}</span><span class="share-bar"><span style="--p:${share}"></span></span><b>${esc(percent(share))}</b></li>`;
  const mastery = new Map((profile?.champions ?? []).map((champion) => [champion.id, champion]));
  const coverage = ROLES.map(([role, label]) => {
    const pool = Object.entries(META).filter(([, meta]) => meta.p.includes(role)).length;
    const played = new Set(games.filter((game) => game.position === role).map((game) => championIdOf(game, profile)).filter((id) => id > 0));
    const owned = [...played].map((id) => mastery.get(id)).filter((champion) => champion !== undefined);
    const level = owned.length ? owned.reduce((sum, champion) => sum + champion.masteryLevel, 0) / owned.length : null;
    const points = owned.length ? owned.reduce((sum, champion) => sum + champion.masteryPoints, 0) / owned.length : null;
    return { role, label, pool, played: played.size, level, points };
  });
  const body = `
    <div class="pool-tiles">
      <div class="style-tile"><span>${esc(t("stats.championsPlayed"))}</span><b>${esc(`${counts.length} / ${TOTAL_CHAMPIONS}`)}</b></div>
      <div class="style-tile"><span>${esc(t("stats.championFocus"))}</span><b>${esc(percent(focusShare(counts, 3, total)))}</b></div>
    </div>
    <ul class="class-list">
      ${focus(t("stats.mainFocus"), focusShare(counts, 1, total))}
      ${focus(t("stats.top3Focus"), focusShare(counts, 3, total))}
      ${focus(t("stats.top5Focus"), focusShare(counts, 5, total))}
    </ul>
    <h4 class="sub-head"><span>${esc(t("stats.coverage"))}</span>${profile ? `<span>${esc(t("stats.avgMastery"))}</span>` : ""}</h4>
    <ul class="coverage-list">${coverage.map((row) => `
      <li style="--c:${ROLE_COLORS[row.role]}">
        ${roleGlyph(row.role, 18)}
        <span class="coverage-name"><b>${esc(t(row.label))}</b><em>${esc(`${row.played} / ${row.pool}`)}</em></span>
        <span class="share-bar"><span style="--p:${row.pool ? row.played / row.pool : 0}"></span></span>
        <b class="coverage-rate">${esc(percent(row.pool ? row.played / row.pool : 0))}</b>
        ${profile ? `<span class="coverage-mastery">${row.level === null ? "—" : `<b>${esc(t("stats.masteryLevel", { level: number(row.level, 1) }))}</b><em>${esc(number(row.points ?? 0))}</em>`}</span>` : ""}
      </li>`).join("")}
    </ul>`;
  return panel(t("stats.championPool"), body);
}

function activityPanel(games: MatchRecord[]): string {
  const format = new Intl.DateTimeFormat(localeTag(), { weekday: "short" });
  const days = Array.from({ length: 7 }, (_, index) => ({ label: format.format(new Date(2024, 0, 1 + index)), games: 0, wins: 0 }));
  const hours = Array.from({ length: 24 }, (_, hour) => ({ hour, games: 0, wins: 0 }));
  games.forEach((game) => {
    const date = new Date(game.created);
    const day = days[(date.getDay() + 6) % 7];
    const hour = hours[date.getHours()];
    day.games++;
    hour.games++;
    if (game.win) {
      day.wins++;
      hour.wins++;
    }
  });
  const dayMax = Math.max(1, ...days.map((day) => day.games));
  const active = hours.filter((hour) => hour.games);
  const hourMax = Math.max(1, ...active.map((hour) => hour.games));
  const tone = (wins: number, count: number) => (wins / count >= 0.5 ? "is-good" : "is-bad");
  const body = `
    <h4 class="sub-head"><span>${esc(t("stats.weekday"))}</span></h4>
    <div class="weekday-chart">${days.map((day) => `
      <div class="weekday" data-tooltip="${esc(`${day.label}: ${t("stats.gamesCount", { count: day.games })}${day.games ? ` · ${t("profile.winLoss", { wins: day.wins, losses: day.games - day.wins })} · ${percent(day.wins / day.games)}` : ""}`)}">
        <span class="weekday-bar"><span class="${day.games ? tone(day.wins, day.games) : ""}" style="--p:${day.games / dayMax}"></span></span>
        <b>${day.games || ""}</b>
        <em>${esc(day.label)}</em>
      </div>`).join("")}
    </div>
    <h4 class="sub-head"><span>${esc(t("stats.hourly"))}</span><span>${esc(t("stats.winRate"))}</span></h4>
    <ul class="hour-list">${active.map((hour) => `
      <li>
        <span>${esc(`${String(hour.hour).padStart(2, "0")}:00`)}</span>
        <span class="hour-bar ${tone(hour.wins, hour.games)}"><span style="--p:${hour.games / hourMax}"><em>${esc(t("stats.gamesCount", { count: hour.games }))}</em></span></span>
        <b class="${tone(hour.wins, hour.games)}">${esc(percent(hour.wins / hour.games))}</b>
      </li>`).join("")}
    </ul>`;
  return panel(t("stats.activity"), body);
}

/** OP.GG's Performance metrics: a radar against strong reference values, and the raw numbers. */
function metricsPanel(games: MatchRecord[]): string {
  const summary = summarize(games);
  const metrics: { label: string; short: string; value: number | null; reference: number; format: (value: number) => string }[] = [
    { label: t("stats.kda"), short: t("stats.kda"), value: summary.kda, reference: 5, format: (value) => number(value, 2) },
    { label: t("stats.kp"), short: t("stats.kpShort"), value: summary.kp, reference: 0.7, format: percent },
    { label: t("stats.teamShare"), short: t("stats.teamShareShort"), value: summary.share, reference: 0.35, format: percent },
    { label: t("stats.dpm"), short: "DPM", value: summary.dpm, reference: 1000, format: (value) => number(value) },
    { label: t("stats.csm"), short: "CSM", value: summary.csm, reference: 9, format: (value) => number(value, 1) },
    { label: t("stats.gpm"), short: "GPM", value: summary.gpm, reference: 500, format: (value) => number(value) },
    { label: t("stats.vspm"), short: "VSPM", value: summary.vspm, reference: 2.5, format: (value) => number(value, 1) },
  ];
  const known = metrics.filter((metric) => metric.value !== null);
  if (known.length < 3) return "";
  const size = 240;
  const center = size / 2;
  const radius = 78;
  const angle = (index: number) => -Math.PI / 2 + (index / known.length) * Math.PI * 2;
  const point = (index: number, scale: number) =>
    `${(center + Math.cos(angle(index)) * radius * scale).toFixed(1)},${(center + Math.sin(angle(index)) * radius * scale).toFixed(1)}`;
  const rings = [0.25, 0.5, 0.75, 1].map((scale) =>
    `<polygon points="${known.map((_, index) => point(index, scale)).join(" ")}" class="radar-ring"/>`).join("");
  const spokes = known.map((_, index) => `<line x1="${center}" y1="${center}" x2="${point(index, 1).split(",")[0]}" y2="${point(index, 1).split(",")[1]}" class="radar-ring"/>`).join("");
  const shape = known.map((metric, index) => point(index, Math.max(0.06, clamp(metric.value! / metric.reference)))).join(" ");
  const dots = known.map((metric, index) => {
    const [x, y] = point(index, Math.max(0.06, clamp(metric.value! / metric.reference))).split(",");
    return `<circle cx="${x}" cy="${y}" r="3" class="radar-dot" data-metric="${index}"/><circle cx="${x}" cy="${y}" r="11" class="radar-hit" data-metric="${index}" data-tooltip="${esc(`${metric.label}: ${metric.format(metric.value!)}`)}"/>`;
  }).join("");
  const labels = known.map((metric, index) => {
    const x = center + Math.cos(angle(index)) * (radius + 20);
    const y = center + Math.sin(angle(index)) * (radius + 16) + 4;
    const anchor = Math.abs(Math.cos(angle(index))) < 0.2 ? "middle" : Math.cos(angle(index)) > 0 ? "start" : "end";
    return `<text x="${x.toFixed(1)}" y="${y.toFixed(1)}" text-anchor="${anchor}" class="radar-label" data-metric="${index}">${esc(metric.short)}</text>`;
  }).join("");
  const body = `
    <div class="metrics">
      <svg class="radar" viewBox="-44 -4 ${size + 88} ${size + 8}" role="img" aria-label="${esc(t("stats.metrics"))}">
        ${rings}${spokes}
        <polygon points="${shape}" class="radar-shape"/>
        ${dots}${labels}
      </svg>
      <ul class="metric-list">${known.map((metric, index) => `
        <li data-metric="${index}">
          <span>${esc(metric.label)}</span><b>${esc(metric.format(metric.value!))}</b>
          <span class="share-bar"><span style="--p:${clamp(metric.value! / metric.reference)}"></span></span>
        </li>`).join("")}
      </ul>
    </div>`;
  return panel(t("stats.metrics"), body);
}
