// End-of-game details of a recorded game, laid out like the League client's
// post-game screen: Scoreboard, Overview (gold advantage, kills over time,
// kill map), Stats, Graphs and Runes. The data is the client's match-history
// game (saved next to the recording), so field names follow the client.
import { api, championIcon, gameImage, type GameAssets } from "./api";
import { esc } from "./dom";
import { localeTag, t, type MessageKey } from "./i18n";
import championMeta from "./data/champion-meta.json";
import { pointsAttribute, wireChartHover, type HoverPoint } from "./charthover";

/* eslint-disable @typescript-eslint/no-explicit-any */
type Json = Record<string, any>;

export interface GameDetails {
  puuid: string;
  game: Json;
  timeline: { frames: { t: number; gold: number[] }[]; events: Json[] } | null;
}

const META = championMeta as Record<string, { n: string }>;
const championName = (id: number) => META[String(id)]?.n ?? `#${id}`;

let assets: GameAssets | null = null;
let assetsLoading: Promise<void> | null = null;
/** Rune, item and spell names and icons (resolves once they are loaded). */
export const loadGameAssets = (): Promise<void> =>
  (assetsLoading ??= api
    .buildAssets()
    .then((value) => void (assets = value))
    .catch(() => void (assetsLoading = null)));

type Tab = "scoreboard" | "overview" | "stats" | "graphs" | "runes";
const TABS: [Tab, MessageKey][] = [
  ["scoreboard", "gi.scoreboard"],
  ["overview", "gi.overview"],
  ["stats", "gi.stats"],
  ["graphs", "gi.graphs"],
  ["runes", "gi.runes"],
];

interface Player {
  id: number;
  team: number;
  champion: number;
  name: string;
  spells: number[];
  s: Json;
  me: boolean;
}

/** Stat rows of the Stats tab (and the bars of the Graphs tab when numeric). */
type Stat = { key: string; label: MessageKey; value: (s: Json) => number | string | boolean };
const num = (key: string) => (s: Json) => Number(s[key]) || 0;
const GROUPS: { id: string; label: MessageKey; stats: Stat[] }[] = [
  {
    id: "combat",
    label: "gi.combat",
    stats: [
      { key: "kda", label: "gi.kda", value: (s) => `${s.kills}/${s.deaths}/${s.assists}` },
      { key: "largestKillingSpree", label: "gi.largestKillingSpree", value: num("largestKillingSpree") },
      { key: "largestMultiKill", label: "gi.largestMultiKill", value: num("largestMultiKill") },
      { key: "timeCCingOthers", label: "gi.ccScore", value: num("timeCCingOthers") },
      { key: "firstBloodKill", label: "gi.firstBlood", value: (s) => Boolean(s.firstBloodKill) },
    ],
  },
  {
    id: "dealt",
    label: "gi.damageDealt",
    stats: [
      { key: "totalDamageDealtToChampions", label: "gi.totalDamageToChampions", value: num("totalDamageDealtToChampions") },
      { key: "physicalDamageDealtToChampions", label: "gi.physicalDamageToChampions", value: num("physicalDamageDealtToChampions") },
      { key: "magicDamageDealtToChampions", label: "gi.magicDamageToChampions", value: num("magicDamageDealtToChampions") },
      { key: "trueDamageDealtToChampions", label: "gi.trueDamageToChampions", value: num("trueDamageDealtToChampions") },
      { key: "totalDamageDealt", label: "gi.totalDamageDealt", value: num("totalDamageDealt") },
      { key: "physicalDamageDealt", label: "gi.physicalDamageDealt", value: num("physicalDamageDealt") },
      { key: "magicDamageDealt", label: "gi.magicDamageDealt", value: num("magicDamageDealt") },
      { key: "trueDamageDealt", label: "gi.trueDamageDealt", value: num("trueDamageDealt") },
      { key: "largestCriticalStrike", label: "gi.largestCriticalStrike", value: num("largestCriticalStrike") },
      { key: "damageDealtToTurrets", label: "gi.damageToTurrets", value: num("damageDealtToTurrets") },
      { key: "damageDealtToObjectives", label: "gi.damageToObjectives", value: num("damageDealtToObjectives") },
    ],
  },
  {
    id: "taken",
    label: "gi.damageTaken",
    stats: [
      { key: "totalHeal", label: "gi.healingDone", value: num("totalHeal") },
      { key: "totalDamageTaken", label: "gi.totalDamageTaken", value: num("totalDamageTaken") },
      { key: "physicalDamageTaken", label: "gi.physicalDamageTaken", value: num("physicalDamageTaken") },
      { key: "magicalDamageTaken", label: "gi.magicDamageTaken", value: num("magicalDamageTaken") },
      { key: "trueDamageTaken", label: "gi.trueDamageTaken", value: num("trueDamageTaken") },
      { key: "damageSelfMitigated", label: "gi.selfMitigated", value: num("damageSelfMitigated") },
    ],
  },
  {
    id: "vision",
    label: "gi.vision",
    stats: [
      { key: "visionScore", label: "gi.visionScore", value: num("visionScore") },
      { key: "wardsPlaced", label: "gi.wardsPlaced", value: num("wardsPlaced") },
      { key: "wardsKilled", label: "gi.wardsKilled", value: num("wardsKilled") },
      { key: "visionWardsBoughtInGame", label: "gi.controlWards", value: num("visionWardsBoughtInGame") },
    ],
  },
  {
    id: "income",
    label: "gi.income",
    stats: [
      { key: "goldEarned", label: "gi.goldEarned", value: num("goldEarned") },
      { key: "goldSpent", label: "gi.goldSpent", value: num("goldSpent") },
      { key: "totalMinionsKilled", label: "gi.minions", value: num("totalMinionsKilled") },
      { key: "neutralMinionsKilled", label: "gi.neutralMinions", value: num("neutralMinionsKilled") },
    ],
  },
  {
    id: "misc",
    label: "gi.misc",
    stats: [
      { key: "turretKills", label: "gi.towersDestroyed", value: num("turretKills") },
      { key: "inhibitorKills", label: "gi.inhibitorsDestroyed", value: num("inhibitorKills") },
    ],
  },
];

// The view's choices, kept while the app runs.
let tab: Tab = "scoreboard";
/** Stats shown as bars in the Graphs tab (several at once, like the client). */
const graphStats = new Set<string>(["totalDamageDealtToChampions"]);
let runePlayer = 0;
let shownGame = 0;
/** The player's team: listed first and teal, as in the client (blue side when unknown). */
let ally = 100;
let mapPlayer = 0;
const collapsed = new Set<string>();

const count = (value: number) => new Intl.NumberFormat(localeTag()).format(Math.round(value));
const clock = (ms: number) => {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
};

const image = (path: string | undefined, label: string, size: number, className = "") =>
  path
    ? `<img class="${className}" src="${esc(gameImage(path))}" alt="" width="${size}" height="${size}" loading="lazy" draggable="false" data-tooltip="${esc(label)}"/>`
    : `<span class="gi-blank ${className}" style="width:${size}px;height:${size}px"></span>`;
const itemImage = (id: number, size = 28) => (id ? image(assets?.items[id]?.icon, assets?.items[id]?.name ?? "", size) : `<span class="gi-blank" style="width:${size}px;height:${size}px"></span>`);
const spellImage = (id: number, size = 18) => image(assets?.spells[id]?.icon, assets?.spells[id]?.name ?? "", size);
const perkImage = (id: number, size = 18, className = "") => image(assets?.perks[id]?.icon, assets?.perks[id]?.name ?? "", size, className);
const styleImage = (id: number, size = 14) => image(assets?.styles[id]?.icon, assets?.styles[id]?.name ?? "", size);
const champImage = (player: Player, size = 32) =>
  `<img class="gi-portrait" src="${esc(championIcon(player.champion))}" alt="" width="${size}" height="${size}" loading="lazy" draggable="false" data-tooltip="${esc(championName(player.champion))}"/>`;

function players(details: GameDetails): Player[] {
  const game = details.game;
  const identities = new Map<number, Json>((game.participantIdentities ?? []).map((identity: Json) => [identity.participantId, identity.player ?? {}]));
  const list: Player[] = (game.participants ?? [])
    .map((participant: Json) => {
      const identity = identities.get(participant.participantId) ?? {};
      return {
        id: participant.participantId,
        team: participant.teamId,
        champion: participant.championId,
        name: identity.gameName || identity.summonerName || championName(participant.championId),
        spells: [participant.spell1Id, participant.spell2Id],
        s: participant.stats ?? {},
        me: Boolean(details.puuid) && identity.puuid === details.puuid,
      } as Player;
    })
    .sort((a: Player, b: Player) => a.team - b.team || a.id - b.id);
  ally = list.find((player: Player) => player.me)?.team ?? 100;
  return list.sort((a: Player, b: Player) => Number(b.team === ally) - Number(a.team === ally) || a.id - b.id);
}

const cs = (s: Json) => (Number(s.totalMinionsKilled) || 0) + (Number(s.neutralMinionsKilled) || 0);
const teamClass = (team: number) => (team === ally ? "is-blue" : "is-red");
const sides = () => [ally, ally === 100 ? 200 : 100];

// ---------------------------------------------------------------- tabs

function scoreboard(details: GameDetails, all: Player[]): string {
  const teams: Json[] = details.game.teams ?? [];
  return sides()
    .map((teamId, index) => {
      const members = all.filter((player) => player.team === teamId);
      const team = teams.find((entry) => entry.teamId === teamId) ?? {};
      const sum = (key: string) => members.reduce((total, player) => total + (Number(player.s[key]) || 0), 0);
      const rows = members
        .map(
          (player) => `
        <tr class="${player.me ? "is-me" : ""}">
          <td class="gi-champ">${champImage(player, 34)}<em>${player.s.champLevel ?? ""}</em></td>
          <td class="gi-pair">${spellImage(player.spells[0])}${spellImage(player.spells[1])}</td>
          <td class="gi-pair">${perkImage(player.s.perk0, 18)}${styleImage(player.s.perkSubStyle, 16)}</td>
          <td class="gi-name">${esc(player.name)}</td>
          <td class="gi-items">${[0, 1, 2, 3, 4, 5, 6].map((slot) => itemImage(Number(player.s[`item${slot}`]) || 0, 26)).join("")}</td>
          <td class="gi-kda">${player.s.kills} / ${player.s.deaths} / ${player.s.assists}</td>
          <td class="gi-num">${count(cs(player.s))}</td>
          <td class="gi-num">${count(Number(player.s.goldEarned) || 0)}</td>
        </tr>`,
        )
        .join("");
      const bans = (team.bans ?? []).map((ban: Json) => (ban.championId > 0 ? `<img class="gi-ban" src="${esc(championIcon(ban.championId))}" alt="" width="30" height="30" loading="lazy" draggable="false" data-tooltip="${esc(championName(ban.championId))}"/>` : "")).join("");
      const objectives: [MessageKey, number][] = [
        ["gi.towers", team.towerKills],
        ["gi.inhibitors", team.inhibitorKills],
        ["gi.barons", team.baronKills],
        ["gi.dragons", team.dragonKills],
        ["gi.heralds", team.riftHeraldKills],
        ["gi.grubs", team.hordeKills],
      ];
      return `
      <div class="gi-team ${teamClass(teamId)}">
        <table class="gi-board">
          <thead><tr>
            <th colspan="4"><span class="gi-team-name">${esc(t(index === 0 ? "gi.team1" : "gi.team2"))}</span><span class="gi-team-result">${esc(team.win === "Win" ? t("gi.victory") : t("gi.defeat"))}</span></th>
            <th class="gi-items-head">${count(sum("goldEarned"))} ${esc(t("gi.goldShort"))}</th>
            <th class="gi-kda">${sum("kills")} / ${sum("deaths")} / ${sum("assists")}</th>
            <th class="gi-num">${esc(t("gi.cs"))}</th>
            <th class="gi-num">${esc(t("gi.gold"))}</th>
          </tr></thead>
          <tbody>${rows}</tbody>
        </table>
        <aside class="gi-objectives">
          <h4>${esc(t("gi.bansObjectives"))}</h4>
          <div class="gi-bans">${bans || "<span></span>"}</div>
          <dl>${objectives.map(([label, value]) => `<div><dt>${esc(t(label))}</dt><dd>${Number(value) || 0}</dd></div>`).join("")}</dl>
        </aside>
      </div>`;
    })
    .join("");
}

/** Gold advantage of the blue team over time, as the client's Overview graph. */
function goldGraph(details: GameDetails, all: Player[]): string {
  const frames = details.timeline?.frames ?? [];
  if (frames.length < 2) return `<p class="gi-empty">${esc(t("gi.noTimeline"))}</p>`;
  const blue = new Set(all.filter((player) => player.team === ally).map((player) => player.id));
  const diffs = frames.map((frame) => frame.gold.reduce((total, gold, index) => total + (blue.has(index + 1) ? gold : -gold), 0));
  const limit = Math.max(1000, Math.ceil(Math.max(...diffs.map(Math.abs)) / 1000) * 1000);
  const [width, height, left, top, bottom] = [760, 260, 44, 10, 24];
  const end = frames.at(-1)!.t || 1;
  const x = (time: number) => left + (time / end) * (width - left - 8);
  const y = (value: number) => top + ((limit - value) / (2 * limit)) * (height - top - bottom);
  const points = frames.map((frame, index) => [x(frame.t), y(diffs[index])]);
  const line = points.map(([px, py], index) => `${index ? "L" : "M"}${px.toFixed(1)},${py.toFixed(1)}`).join("");
  const area = `${line}L${points.at(-1)![0].toFixed(1)},${y(0)}L${points[0][0].toFixed(1)},${y(0)}Z`;
  const steps = limit / 1000 <= 6 ? 1000 : Math.ceil(limit / 6000) * 1000;
  const grid: string[] = [];
  for (let value = -limit; value <= limit; value += steps) {
    grid.push(`<line x1="${left}" x2="${width - 8}" y1="${y(value)}" y2="${y(value)}" class="gi-grid"/><text x="${left - 6}" y="${y(value) + 4}" text-anchor="end">${value ? `${Math.abs(value) / 1000}k` : "0"}</text>`);
  }
  const ticks = [0, 1, 2, 3].map((part) => (end * part) / 3).map((time) => `<text x="${x(time)}" y="${height - 6}" text-anchor="middle">${clock(time)}</text>`);
  const dots = points.map(([px, py], index) => `<circle cx="${px.toFixed(1)}" cy="${py.toFixed(1)}" r="3" data-i="${index}" class="${diffs[index] >= 0 ? "is-blue" : "is-red"}"/>`);
  const hover: HoverPoint[] = points.map(([px, py], index) => {
    const ours = frames[index].gold.reduce((total, gold, slot) => total + (blue.has(slot + 1) ? gold : 0), 0);
    const theirs = frames[index].gold.reduce((total, gold) => total + gold, 0) - ours;
    const diff = diffs[index];
    return {
      x: px,
      y: py,
      title: clock(frames[index].t),
      lines: [
        diff === 0 ? t("gi.even") : t(diff > 0 ? "gi.yourLead" : "gi.enemyLead", { gold: count(Math.abs(diff)) }),
        `${t("gi.team1")}: ${count(ours)} ${t("gi.goldShort")}`,
        `${t("gi.team2")}: ${count(theirs)} ${t("gi.goldShort")}`,
        t("gi.clickToWatch"),
      ],
      color: diff >= 0 ? "#0ac8b9" : "#e06d62",
    };
  });
  return `
    <svg class="gi-gold" viewBox="0 0 ${width} ${height}" role="img" aria-label="${esc(t("gi.goldAdvantage"))}" data-points="${pointsAttribute(hover)}" data-times="${esc(JSON.stringify(frames.map((frame) => Math.round(frame.t / 1000))))}">
      <defs>
        <clipPath id="gi-above"><rect x="0" y="0" width="${width}" height="${y(0)}"/></clipPath>
        <clipPath id="gi-below"><rect x="0" y="${y(0)}" width="${width}" height="${height}"/></clipPath>
      </defs>
      ${grid.join("")}${ticks.join("")}
      <path d="${area}" class="gi-area is-blue" clip-path="url(#gi-above)"/>
      <path d="${area}" class="gi-area is-red" clip-path="url(#gi-below)"/>
      <path d="${line}" class="gi-line is-blue" clip-path="url(#gi-above)"/>
      <path d="${line}" class="gi-line is-red" clip-path="url(#gi-below)"/>
      ${dots.join("")}
      <g class="chart-hover" visibility="hidden"></g>
    </svg>`;
}

/** Kills, monsters and buildings over time, one row per team. */
function eventStrip(details: GameDetails, all: Player[]): string {
  const events = details.timeline?.events ?? [];
  const end = details.timeline?.frames.at(-1)?.t || (Number(details.game.gameDuration) || 1) * 1000;
  const teamOf = new Map(all.map((player) => [player.id, player.team]));
  const credited = (event: Json): number => (event.type === "BUILDING_KILL" ? (event.team === 100 ? 200 : 100) : (teamOf.get(event.killer) ?? 0));
  const order = sides();
  const label = (event: Json): string => {
    if (event.type === "CHAMPION_KILL") {
      const killer = all.find((player) => player.id === event.killer);
      const victim = all.find((player) => player.id === event.victim);
      return `${killer ? championName(killer.champion) : "?"} > ${victim ? championName(victim.champion) : "?"}`;
    }
    if (event.type === "BUILDING_KILL") return event.building === "INHIBITOR_BUILDING" ? t("gi.inhibitors") : t("gi.towers");
    const monster: Record<string, MessageKey> = { DRAGON: "gi.dragons", BARON_NASHOR: "gi.barons", RIFTHERALD: "gi.heralds", HORDE: "gi.grubs" };
    return t(monster[event.monster] ?? "gi.objective");
  };
  return order
    .map(
      (team) => `
      <div class="gi-strip ${teamClass(team)}"><span>${esc(t("gi.objectivesKills"))}</span><div>${events
        .filter((event) => credited(event) === team)
        .map((event) => `<i class="${event.type === "CHAMPION_KILL" ? "" : "is-big"}" style="left:${((Number(event.t) / end) * 100).toFixed(2)}%" data-gi-time="${Math.round(Number(event.t) / 1000)}" data-tooltip="${esc(`${clock(Number(event.t))}  ${label(event)}
${t("gi.clickToWatch")}`)}"></i>`)
        .join("")}</div></div>`,
    )
    .join("");
}

/** Where champions died, on a drawn Summoner's Rift (no picture needed). */
function killMap(details: GameDetails, all: Player[]): string {
  const teamOf = new Map(all.map((player) => [player.id, player.team]));
  const kills = (details.timeline?.events ?? []).filter((event) => event.type === "CHAMPION_KILL" && (!mapPlayer || event.killer === mapPlayer || event.victim === mapPlayer || (event.assists ?? []).includes(mapPlayer)));
  const size = 300;
  const at = (value: number) => (Math.min(15000, Math.max(0, Number(value) || 0)) / 15000) * size;
  const dots = kills
    .map((event) => {
      const killer = all.find((player) => player.id === event.killer);
      const victim = all.find((player) => player.id === event.victim);
      const tip = `${clock(Number(event.t))}  ${killer ? championName(killer.champion) : "?"} > ${victim ? championName(victim.champion) : "?"}`;
      return `<circle cx="${at(event.x).toFixed(1)}" cy="${(size - at(event.y)).toFixed(1)}" r="5" class="gi-kill ${teamOf.get(event.killer) === ally ? "is-blue" : "is-red"}" data-gi-time="${Math.round(Number(event.t) / 1000)}" data-tooltip="${esc(`${tip}
${t("gi.clickToWatch")}`)}"/>`;
    })
    .join("");
  const portraits = sides()
    .map((team) => `<div class="gi-map-team ${teamClass(team)}">${all.filter((player) => player.team === team).map((player) => `<button data-gi-map="${player.id}" aria-pressed="${mapPlayer === player.id}">${champImage(player, 30)}</button>`).join("")}</div>`)
    .join("");
  return `
    <div class="gi-map">
      <svg viewBox="0 0 ${size} ${size}" role="img" aria-label="${esc(t("gi.killMap"))}">
        <rect width="${size}" height="${size}" class="gi-map-ground"/>
        <path d="M0,${size} L${size},0" class="gi-map-river"/>
        <path d="M22,${size - 22} L22,22 L${size - 22},22 M22,${size - 22} L${size - 22},${size - 22} L${size - 22},22 M22,${size - 22} L${size - 22},22" class="gi-map-lane"/>
        <circle cx="22" cy="${size - 22}" r="10" class="gi-map-base ${teamClass(100)}"/><circle cx="${size - 22}" cy="22" r="10" class="gi-map-base ${teamClass(200)}"/>
        ${dots}
      </svg>
      ${portraits}
    </div>`;
}

function statsTable(all: Player[]): string {
  const head = all.map((player) => `<th class="${teamClass(player.team)}${player.me ? " is-me" : ""}">${champImage(player, 30)}</th>`).join("");
  const body = GROUPS.map((group) => {
    const open = !collapsed.has(group.id);
    const rows = open
      ? group.stats
          .map((stat) => `<tr><th>${esc(t(stat.label))}</th>${all
            .map((player) => {
              const value = stat.value(player.s);
              const shown = typeof value === "boolean" ? `<span class="gi-flag${value ? " is-on" : ""}"></span>` : typeof value === "number" ? count(value) : esc(value);
              const tip = `${player.name} (${championName(player.champion)}): ${t(stat.label)} ${typeof value === "number" ? count(value) : typeof value === "boolean" ? (value ? t("gi.yes") : t("gi.no")) : value}`;
              return `<td class="${player.me ? "is-me" : ""}" data-tooltip="${esc(tip)}">${shown}</td>`;
            })
            .join("")}</tr>`)
          .join("")
      : "";
    return `<tbody><tr class="gi-group"><th colspan="${all.length + 1}"><button data-gi-group="${group.id}" aria-expanded="${open}"><span class="gi-caret"></span>${esc(t(group.label))}</button></th></tr>${rows}</tbody>`;
  }).join("");
  return `<div class="gi-table-wrap"><table class="gi-stats"><thead><tr><th></th>${head}</tr></thead>${body}</table></div>`;
}

/** A round axis step for values up to `max` (about 6 lines). */
function axisStep(max: number): number {
  const rough = max / 6;
  const power = 10 ** Math.floor(Math.log10(Math.max(1, rough)));
  return [1, 2, 2.5, 5, 10].map((factor) => factor * power).find((step) => step >= rough) ?? power * 10;
}

function graphs(all: Player[]): string {
  const numeric = GROUPS.map((group) => ({ ...group, stats: group.stats.filter((stat) => typeof stat.value({}) === "number") })).filter((group) => group.stats.length);
  const chosen = numeric.flatMap((group) => group.stats).filter((stat) => graphStats.has(stat.key));
  const box = (attributes: string, checked: boolean, label: string, className = "") =>
    `<label class="gi-check ${className}"><input type="checkbox" ${attributes}${checked ? " checked" : ""}/><span>${esc(label)}</span></label>`;
  const list = numeric
    .map((group) => {
      const all = group.stats.every((stat) => graphStats.has(stat.key));
      return `<div class="gi-graph-group">${box(`data-gi-graph-group="${group.id}"`, all, t(group.label), "is-group")}${group.stats.map((stat) => box(`data-gi-graph="${stat.key}"`, graphStats.has(stat.key), t(stat.label))).join("")}</div>`;
    })
    .join("");
  if (!chosen.length) return `<div class="gi-graphs"><div class="gi-graph-list">${list}</div><p class="gi-empty">${esc(t("gi.pickStat"))}</p></div>`;
  const max = Math.max(1, ...all.flatMap((player) => chosen.map((stat) => Number(stat.value(player.s)) || 0)));
  const step = axisStep(max);
  const top = Math.ceil(max / step) * step;
  const ticks = Array.from({ length: Math.round(top / step) + 1 }, (_, index) => index * step);
  const lines = ticks.map((value) => `<i style="left:${((value / top) * 100).toFixed(2)}%"></i>`).join("");
  const labels = ticks.map((value) => `<span style="left:${((value / top) * 100).toFixed(2)}%">${count(value)}</span>`).join("");
  const rows = all
    .map((player, index) => {
      const bars = chosen
        .map((stat, order) => {
          const value = Number(stat.value(player.s)) || 0;
          const tip = `${player.name} (${championName(player.champion)}): ${t(stat.label)} ${count(value)}`;
          return `<span class="gi-bar-row" data-tooltip="${esc(tip)}"><span class="gi-bar-fill shade-${Math.min(order, 3)}" style="width:${((value / top) * 100).toFixed(2)}%"></span><em class="gi-bar-value">${count(value)}</em></span>`;
        })
        .join("");
      return `${index === 5 ? `<div class="gi-bar-gap"></div>` : ""}<div class="gi-bar ${teamClass(player.team)}${player.me ? " is-me" : ""}">${champImage(player, 34)}<div class="gi-bar-set" style="--bars:${chosen.length}">${bars}</div></div>`;
    })
    .join("");
  return `<div class="gi-graphs"><div class="gi-graph-list">${list}</div>
    <div class="gi-chart"><div class="gi-bars"><div class="gi-axis-lines">${lines}</div>${rows}</div><div class="gi-axis">${labels}</div></div></div>`;
}

/** "Burn Damage Dealt: @eogvar1@" with the player's numbers. */
function runeStat(template: string, s: Json, slot: number): string {
  return template.replace(/@eogvar(\d)@/g, (_match, index) => count(Number(s[`perk${slot}Var${index}`]) || 0));
}

function runes(all: Player[]): string {
  const me = all.find((player) => player.me) ?? all[0];
  const player = all.find((entry) => entry.id === runePlayer) ?? me;
  const s = player.s;
  const styles = [s.perkPrimaryStyle, s.perkSubStyle].map((id: number) => assets?.styles[id]?.name ?? "").filter(Boolean).join(" / ");
  const portraits = all.map((entry) => `<button class="${teamClass(entry.team)}" data-gi-rune="${entry.id}" aria-pressed="${entry.id === player.id}">${champImage(entry, 30)}</button>`).join("");
  const rows = [0, 1, 2, 3, 4, 5]
    .map((slot) => {
      const id = Number(s[`perk${slot}`]) || 0;
      if (!id) return "";
      const perk = assets?.perks[id];
      const lines = (perk?.stats ?? []).map((template) => `<span>${esc(runeStat(template, s, slot))}</span>`).join("");
      return `<div class="gi-rune">${perkImage(id, slot === 0 ? 48 : 36, slot === 0 ? "is-keystone" : "")}<strong>${esc(perk?.name ?? String(id))}</strong><div>${lines}</div></div>`;
    })
    .join("");
  return `
    <div class="gi-rune-pick">${portraits}</div>
    <div class="gi-rune-head ${teamClass(player.team)}">
      ${champImage(player, 44)}
      <div><strong>${esc(player.name)}</strong><span>${esc(styles)}</span></div>
      <span class="gi-items">${[0, 1, 2, 3, 4, 5, 6].map((slot) => itemImage(Number(s[`item${slot}`]) || 0, 28)).join("")}</span>
      <b>${s.kills} / ${s.deaths} / ${s.assists}</b><b>${count(cs(s))} ${esc(t("gi.cs"))}</b><b>${count(Number(s.goldEarned) || 0)} ${esc(t("gi.goldShort"))}</b>
    </div>
    <div class="gi-runes">${rows}</div>`;
}

// ---------------------------------------------------------------- rendering

/** Fill `root` with the game's details (tabs, then the chosen tab). */
export function renderGameInfo(root: HTMLElement, details: GameDetails): void {
  const all = players(details);
  const gameId = Number(details.game.gameId) || 0;
  if (gameId !== shownGame) {
    shownGame = gameId;
    runePlayer = 0;
    mapPlayer = 0;
  }
  if (!runePlayer || !all.some((player) => player.id === runePlayer)) runePlayer = all.find((player) => player.me)?.id ?? all[0]?.id ?? 0;
  if (mapPlayer && !all.some((player) => player.id === mapPlayer)) mapPlayer = 0;
  const game = details.game;
  const mine = all.find((player) => player.me);
  const win = mine ? Boolean(mine.s.win) : undefined;
  const date = game.gameCreation ? new Date(Number(game.gameCreation)).toLocaleDateString(localeTag(), { dateStyle: "medium" }) : "";
  const length = clock((Number(game.gameDuration) || 0) * 1000);
  const body =
    tab === "scoreboard"
      ? scoreboard(details, all)
      : tab === "overview"
        ? `<div class="gi-overview"><div><h4>${esc(t("gi.goldAdvantage"))}</h4>${goldGraph(details, all)}${eventStrip(details, all)}</div><div><h4>${esc(t("gi.killMap"))}</h4>${killMap(details, all)}</div></div>`
        : tab === "stats"
          ? statsTable(all)
          : tab === "graphs"
            ? graphs(all)
            : runes(all);
  root.querySelector(".gi-body")?.remove();
  const head = root.querySelector(".gi-head");
  const headHtml = `
    <div class="gi-head">
      <div class="gi-title">
        ${win === undefined ? "" : `<strong class="${win ? "is-win" : "is-loss"}">${esc(t(win ? "gi.victory" : "gi.defeat"))}</strong>`}
        <span>${esc([t("gi.rift"), date, length].filter(Boolean).join("  ·  "))}</span>
      </div>
      <nav class="gi-tabs" role="tablist">${TABS.map(([id, key]) => `<button role="tab" data-gi-tab="${id}" aria-selected="${id === tab}">${esc(t(key))}</button>`).join("")}</nav>
    </div>`;
  if (head) head.outerHTML = headHtml;
  else root.insertAdjacentHTML("beforeend", headHtml);
  root.insertAdjacentHTML("beforeend", `<div class="gi-body is-${tab}">${body}</div>`);
  if (!root.dataset.giBound) {
    root.dataset.giBound = "1";
    const rerender = () => {
      const current = (root as HTMLElement & { giDetails?: GameDetails }).giDetails;
      if (current) renderGameInfo(root, current);
    };
    root.addEventListener("click", (event) => {
      const target = event.target as HTMLElement;
      const chosenTab = target.closest<HTMLElement>("[data-gi-tab]")?.dataset.giTab as Tab | undefined;
      if (chosenTab) tab = chosenTab;
      const group = target.closest<HTMLElement>("[data-gi-group]")?.dataset.giGroup;
      if (group) collapsed.has(group) ? collapsed.delete(group) : collapsed.add(group);
      const rune = target.closest<HTMLElement>("[data-gi-rune]")?.dataset.giRune;
      if (rune) runePlayer = Number(rune);
      const map = target.closest<HTMLElement>("[data-gi-map]")?.dataset.giMap;
      if (map) mapPlayer = mapPlayer === Number(map) ? 0 : Number(map);
      if (chosenTab || group || rune || map) return rerender();
      // Overview: a time on the graph, the strips or the map plays the video there.
      let seconds = Number(target.closest<HTMLElement | SVGElement>("[data-gi-time]")?.dataset.giTime ?? NaN);
      const gold = target.closest<SVGSVGElement>("svg.gi-gold");
      if (Number.isNaN(seconds) && gold) {
        const matrix = gold.getScreenCTM();
        const points = JSON.parse(gold.dataset.points ?? "[]") as HoverPoint[];
        const times = JSON.parse(gold.dataset.times ?? "[]") as number[];
        if (matrix && points.length) {
          const x = ((event as MouseEvent).clientX - matrix.e) / matrix.a;
          const nearest = points.reduce((best, point, index) => (Math.abs(point.x - x) < Math.abs(points[best].x - x) ? index : best), 0);
          seconds = times[nearest] ?? NaN;
        }
      }
      if (!Number.isNaN(seconds)) root.dispatchEvent(new CustomEvent("gi-seek", { detail: seconds, bubbles: true }));
    });
    root.addEventListener("change", (event) => {
      const input = event.target as HTMLInputElement;
      const stat = input.dataset.giGraph;
      const group = GROUPS.find((entry) => entry.id === input.dataset.giGraphGroup);
      if (!stat && !group) return;
      if (stat) input.checked ? graphStats.add(stat) : graphStats.delete(stat);
      group?.stats.filter((entry) => typeof entry.value({}) === "number").forEach((entry) => (input.checked ? graphStats.add(entry.key) : graphStats.delete(entry.key)));
      rerender();
    });
    wireChartHover(root);
  }
  (root as HTMLElement & { giDetails?: GameDetails }).giDetails = details;
  // Names and icons arrive a moment later the first time.
  if (!assets) void loadGameAssets().then(() => assets && (root as HTMLElement & { giDetails?: GameDetails }).giDetails === details && renderGameInfo(root, details));
}
