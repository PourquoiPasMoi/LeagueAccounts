// Stats computed from an account's game archive (League client + OP.GG games).
import { championIcon, type MatchRecord, type Profile } from "./api";
import { localeTag, t, type MessageKey } from "./i18n";
import { esc } from "./dom";
import { icon } from "./icons";
import championIds from "./data/champion-ids.json";
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
  const toolbar = `
    <div class="stats-toolbar">
      ${segmented("queue", filters.queue, [
        ["all", t("stats.allQueues")], ["solo", t("profile.q.solo")], ["flex", t("profile.q.flex")],
        ["normal", t("profile.q.normal")], ["aram", t("profile.q.aram")], ["arena", t("profile.q.arena")],
      ])}
      ${segmented("range", filters.range, [[20, t("stats.last", { count: 20 })], [50, t("stats.last", { count: 50 })], [0, t("stats.allGames")]])}
    </div>`;
  const games = filterGames(all, filters);
  if (!all.length) {
    return `${toolbar}<div class="profile-empty">${icon("info", 18)}<div><strong>${esc(t("stats.emptyTitle"))}</strong><p>${esc(t("stats.emptyText"))}</p></div></div>`;
  }
  if (!games.length) return `${toolbar}<p class="muted profile-note">${esc(t("stats.noneForFilter"))}</p>`;

  const wins = games.filter((game) => game.win).length;
  const k = games.reduce((sum, game) => sum + game.kills, 0);
  const d = games.reduce((sum, game) => sum + game.deaths, 0);
  const a = games.reduce((sum, game) => sum + game.assists, 0);
  const rich = games.filter(detailed);
  const kp = mean(rich.filter((game) => game.teamKills > 0), (game) => (game.kills + game.assists) / game.teamKills);
  const share = mean(rich.filter((game) => game.teamDamage > 0), (game) => game.damage / game.teamDamage);
  const csMin = mean(rich, (game) => game.cs / minutes(game));
  const goldMin = mean(rich, (game) => game.gold / minutes(game));
  const damageMin = mean(rich, (game) => game.damage / minutes(game));
  const visionMin = mean(rich, (game) => game.vision / minutes(game));
  const length = mean(rich, (game) => game.durationSeconds / 60);
  const opScore = mean(games, (game) => (game.opScoreX10 == null ? null : game.opScoreX10 / 10));
  const n = games.length;
  const tile = (label: string, main: string, sub = "", tone = "") =>
    `<div class="stat-tile${tone ? ` ${tone}` : ""}"><span>${esc(label)}</span><b>${main}</b>${sub ? `<em>${sub}</em>` : ""}</div>`;
  const dash = "—";
  const rate = wins / n;

  const tiles = [
    tile(t("stats.games"), esc(number(n)), esc(t("profile.winLoss", { wins, losses: n - wins }))),
    tile(t("stats.winRate"), esc(percent(rate)), "", rate >= 0.5 ? "is-good" : "is-bad"),
    tile(t("stats.kda"), esc(number(kdaOf(k, d, a), 2)), esc(`${number(k / n, 1)} / ${number(d / n, 1)} / ${number(a / n, 1)}`)),
    tile(t("stats.kp"), kp ? esc(percent(kp.value)) : dash),
    tile(t("stats.csMin"), csMin ? esc(number(csMin.value, 1)) : dash),
    tile(t("stats.goldMin"), goldMin ? esc(number(goldMin.value)) : dash),
    tile(t("stats.damageMin"), damageMin ? esc(number(damageMin.value)) : dash, share ? esc(t("stats.damageShare", { share: percent(share.value) })) : ""),
    tile(t("stats.visionMin"), visionMin ? esc(number(visionMin.value, 2)) : dash),
    tile(t("stats.length"), length ? esc(t("profile.minutes", { m: Math.round(length.value) })) : dash),
    ...(opScore ? [tile(t("stats.opScore"), esc(number(opScore.value, 1)))] : []),
  ];

  const note = rich.length < n
    ? `<p class="muted profile-note">${esc(t("stats.detailNote", { detailed: rich.length, total: n }))}</p>`
    : "";

  return `
    ${toolbar}
    <div class="stat-tiles">${tiles.join("")}</div>
    ${note}
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
        ${playStylePanel(games)}
        ${championTable(games, profile)}
        ${formPanel(games, profile)}
        ${recordsPanel(games, profile)}
      </div>
      <div class="stats-side">
        ${rolesPanel(games)}
        ${sidesPanel(games)}
        ${streaksPanel(games)}
        ${weekdayPanel(games)}
        ${hoursPanel(games)}
      </div>
    </div>`;
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
  const bars = points.map((point) => {
    const barWidth = Math.max(3, (width - pad.left - pad.right) / games.length - 3);
    return `<rect x="${(x(point.index) - barWidth / 2).toFixed(1)}" y="${y(point.value).toFixed(1)}" width="${barWidth.toFixed(1)}" height="${(height - pad.bottom - y(point.value)).toFixed(1)}" class="${point.game.win ? "bar-win" : "bar-loss"}"/>`;
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
      <h3>${esc(t("stats.champions"))}</h3>
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
    </section>`;
}

// ---------------------------------------------------------------- roles, streaks, weekdays

const ROLES: [string, MessageKey][] = [
  ["TOP", "stats.role.top"], ["JUNGLE", "stats.role.jungle"], ["MIDDLE", "stats.role.mid"],
  ["BOTTOM", "stats.role.bot"], ["UTILITY", "stats.role.support"],
];

function rolesPanel(games: MatchRecord[]): string {
  const withRole = games.filter((game) => game.position);
  if (!withRole.length) return "";
  const max = Math.max(...ROLES.map(([role]) => withRole.filter((game) => game.position === role).length), 1);
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.roles"))}</h3>
      <ul class="bar-list">${ROLES.map(([role, label]) => {
        const list = withRole.filter((game) => game.position === role);
        const wins = list.filter((game) => game.win).length;
        return `<li data-tooltip="${esc(`${t(label)}: ${t("profile.winLoss", { wins, losses: list.length - wins })}`)}"><span>${esc(t(label))}</span><span class="bar"><span style="--p:${list.length / max}"></span></span>
          <b>${list.length}</b><em>${list.length ? esc(percent(wins / list.length)) : ""}</em></li>`;
      }).join("")}</ul>
    </section>`;
}

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

function weekdayPanel(games: MatchRecord[]): string {
  const format = new Intl.DateTimeFormat(localeTag(), { weekday: "short" });
  // Monday first; 2024-01-01 was a Monday.
  const days = Array.from({ length: 7 }, (_, index) => ({ label: format.format(new Date(2024, 0, 1 + index)), games: 0, wins: 0 }));
  games.forEach((game) => {
    const day = (new Date(game.created).getDay() + 6) % 7;
    days[day].games++;
    if (game.win) days[day].wins++;
  });
  const max = Math.max(1, ...days.map((day) => day.games));
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.weekdays"))}</h3>
      <div class="weekday-chart">${days.map((day) => `
        <div class="weekday" data-tooltip="${esc(`${day.label}: ${t("stats.gamesCount", { count: day.games })}${day.games ? ` · ${t("profile.winLoss", { wins: day.wins, losses: day.games - day.wins })}` : ""}`)}">
          <span class="weekday-bar"><span style="--p:${day.games / max}"></span></span>
          <b class="${day.games ? (day.wins / day.games >= 0.5 ? "is-good" : "is-bad") : ""}">${day.games ? esc(percent(day.wins / day.games)) : ""}</b>
          <em>${esc(day.label)}</em>
        </div>`).join("")}
      </div>
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
          ${championIdOf(game, profile) ? `<img src="${esc(championIcon(championIdOf(game, profile)))}" alt="" loading="lazy" draggable="false" />` : ""}
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

// ---------------------------------------------------------------- play style, sides, hours

/** Like OP.GG's season play style: totals, champion pool and recent form against the whole list. */
function playStylePanel(games: MatchRecord[]): string {
  const rich = games.filter(detailed);
  const seconds = rich.reduce((sum, game) => sum + game.durationSeconds, 0);
  const hours = Math.floor(seconds / 3600);
  const rest = Math.round((seconds % 3600) / 60);
  const pool = new Set(games.map((game) => game.champion || String(game.championId))).size;
  const total = Object.values(NAMES).filter((id) => id > 0 && id < 1000).length;
  const summary = (list: MatchRecord[]) => {
    const n = Math.max(1, list.length);
    const k = list.reduce((sum, game) => sum + game.kills, 0);
    const d = list.reduce((sum, game) => sum + game.deaths, 0);
    const a = list.reduce((sum, game) => sum + game.assists, 0);
    const detail = list.filter(detailed);
    return {
      rate: list.filter((game) => game.win).length / n,
      kda: kdaOf(k, d, a),
      kp: mean(detail.filter((game) => game.teamKills > 0), (game) => (game.kills + game.assists) / game.teamKills)?.value ?? null,
      cs: mean(detail, (game) => game.cs / minutes(game))?.value ?? null,
      damage: mean(detail, (game) => game.damage / minutes(game))?.value ?? null,
      vision: mean(detail, (game) => game.vision / minutes(game))?.value ?? null,
    };
  };
  const recent = summary(games.slice(0, 20));
  const all = summary(games);
  const rows: [string, number | null, number | null, (value: number) => string][] = [
    [t("stats.winRate"), recent.rate, all.rate, percent],
    [t("stats.kda"), recent.kda, all.kda, (value) => number(value, 2)],
    [t("stats.kp"), recent.kp, all.kp, percent],
    [t("stats.csMin"), recent.cs, all.cs, (value) => number(value, 1)],
    [t("stats.damageMin"), recent.damage, all.damage, (value) => number(value)],
    [t("stats.visionMin"), recent.vision, all.vision, (value) => number(value, 2)],
  ];
  const showCompare = games.length > 20;
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.playStyle"))}</h3>
      <div class="style-grid">
        <div><span>${esc(t("stats.playTime"))}</span><b>${esc(seconds ? `${hours}h ${rest}m` : "—")}</b><em>${esc(t("stats.gamesCount", { count: rich.length }))}</em></div>
        <div><span>${esc(t("stats.championPool"))}</span><b>${esc(`${pool} / ${total}`)}</b><em>${esc(percent(pool / Math.max(1, total)))}</em></div>
      </div>
      ${showCompare ? `
      <table class="compare-table">
        <caption>${esc(t("stats.recentVsAll"))}</caption>
        <thead><tr><th></th><th>${esc(t("stats.recent"))}</th><th>${esc(t("stats.overall"))}</th></tr></thead>
        <tbody>${rows.filter(([, left, right]) => left !== null && right !== null).map(([label, left, right, format]) => `
          <tr><td>${esc(label)}</td><td class="${left! > right! * 1.02 ? "is-good" : left! < right! * 0.98 ? "is-bad" : ""}">${esc(format(left!))}</td><td>${esc(format(right!))}</td></tr>`).join("")}
        </tbody>
      </table>` : ""}
    </section>`;
}

function sidesPanel(games: MatchRecord[]): string {
  const sides = (["blue", "red"] as const).map((side) => {
    const list = games.filter((game) => game.side === side);
    const wins = list.filter((game) => game.win).length;
    return { side, games: list.length, wins };
  });
  if (!sides.some((side) => side.games)) return "";
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.sides"))}</h3>
      <div class="side-split">${sides.map((side) => `
        <div class="is-${side.side}">
          <span>${esc(t(side.side === "blue" ? "stats.blueSide" : "stats.redSide"))}</span>
          <b class="${side.games ? (side.wins / side.games >= 0.5 ? "is-good" : "is-bad") : ""}">${side.games ? esc(percent(side.wins / side.games)) : "—"}</b>
          <em>${esc(t("profile.winLoss", { wins: side.wins, losses: side.games - side.wins }))}</em>
        </div>`).join("")}
      </div>
    </section>`;
}

function hoursPanel(games: MatchRecord[]): string {
  if (!games.length) return "";
  const hours = Array.from({ length: 24 }, () => ({ games: 0, wins: 0 }));
  games.forEach((game) => {
    const hour = new Date(game.created).getHours();
    hours[hour].games++;
    if (game.win) hours[hour].wins++;
  });
  const max = Math.max(1, ...hours.map((hour) => hour.games));
  return `
    <section class="profile-panel">
      <h3>${esc(t("stats.hours"))}</h3>
      <div class="hour-chart">${hours.map((hour, index) => `
        <div class="hour" data-tooltip="${esc(`${t("stats.hoursHint", { hour: index })}: ${t("stats.gamesCount", { count: hour.games })}${hour.games ? ` · ${t("profile.winLoss", { wins: hour.wins, losses: hour.games - hour.wins })}` : ""}`)}">
          <span class="hour-bar"><span style="--p:${hour.games / max}"></span></span>
          <em>${index % 6 === 0 ? index : ""}</em>
        </div>`).join("")}
      </div>
    </section>`;
}
