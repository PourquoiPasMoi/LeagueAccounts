// Broadcast-style overlay for a recorded game, drawn over the video at the
// moment shown: a score banner (kills, gold and lead, towers, dragons by
// element, voidgrubs, Baron buff), objective timers and the players panel with
// the gold difference of each lane. Data: the match timeline saved with the
// recording (per minute, plus kills and objectives to the second) and, for
// games recorded with them, the recorder's snapshots of every player (items,
// scores, level, death timers every few seconds).
import { championIcon, gameImage } from "./api";
import { esc } from "./dom";
import { t } from "./i18n";
import { championName, itemImage, players, spellImage, type GameDetails } from "./gameinfo";

/* eslint-disable @typescript-eslint/no-explicit-any */
type Json = Record<string, any>;

/** Every player's state sampled by the recorder (`<name>.live.json`). */
export interface LiveLog {
  players: { name: string; tag: string; champion: string; team: string; position?: string }[];
  /** `p` rows: level, kills, deaths, assists, CS, ward score ×10, dead, respawn ×10, items 0..6. */
  snapshots: { t: number; p: number[][] }[];
}

interface Row {
  id: number;
  champion: number;
  name: string;
  team: number;
  me: boolean;
  spells: number[];
  level: number | null;
  k: number;
  d: number;
  a: number;
  cs: number | null;
  gold: number | null;
  items: number[] | null;
  respawn: number;
  /** 0 top … 4 support; 9 unknown. */
  role: number;
}

export interface Hud {
  /** Each region's markup; the caller replaces a region only when it changed. */
  banner: string;
  timers: string;
  panel: string;
}

const ROLES = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"];
const SMITE = 11;
/** Standard spawn rules (seconds): first dragon, dragon respawn, Elder respawn, first Baron, Baron respawn, Baron buff. */
const DRAGON_FIRST = 300;
const DRAGON_RESPAWN = 300;
const ELDER_RESPAWN = 360;
const BARON_FIRST = 1200;
const BARON_RESPAWN = 360;
const BARON_BUFF = 180;

/** The game's own icons (in-game scoreboard and minimap) and the client's match history ones. */
const DRAGON_ICONS: Record<string, string> = {
  FIRE_DRAGON: "x-ux/scoreboard/_infernaldrake.png",
  WATER_DRAGON: "x-ux/scoreboard/_oceandrake.png",
  EARTH_DRAGON: "x-ux/scoreboard/_mountaindrake.png",
  AIR_DRAGON: "x-ux/scoreboard/_clouddrake.png",
  HEXTECH_DRAGON: "x-ux/scoreboard/_hextechdrake.png",
  CHEMTECH_DRAGON: "x-ux/scoreboard/_chemtechdrake.png",
  ELDER_DRAGON: "x-ux/scoreboard/_elderdrake.png",
};
const ICONS = {
  dragon: "x-ux/scoreboard/_dragon.png",
  baron: "x-ux/scoreboard/_baronnashor.png",
  grubs: "x-ux/minimap/icons/grub.png",
  towerBlue: "x-mh/tower-100.png",
  towerRed: "x-mh/tower-200.png",
  gold: "x-mh/icon_gold.png",
};
const picture = (path: string, className: string) => `<img class="${className}" src="${esc(gameImage(path))}" alt="" draggable="false"/>`;
/** The gold coins picture holds two states stacked: only the top one is shown. */
const coins = () => `<span class="hud-coins">${picture(ICONS.gold, "")}</span>`;

const clock = (seconds: number) => {
  const total = Math.max(0, Math.floor(seconds));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
};
const short = (gold: number) => (gold >= 1000 ? `${(gold / 1000).toFixed(1)}K` : String(Math.round(gold)));
const normal = (name: string) => name.toLowerCase().replace(/[^a-z]/g, "");
const championId = (name: string): number => {
  const wanted = normal(name);
  for (let id = 1; id < 1000; id += 1) if (normal(championName(id)) === wanted) return id;
  return 0;
};
const liveTeam = (team: string) => (team === "CHAOS" ? 200 : 100);

/** The last snapshot at or before `seconds` (null before the first one). */
function snapshotAt(live: LiveLog | null, seconds: number): LiveLog["snapshots"][number] | null {
  const list = live?.snapshots ?? [];
  if (!list.length || seconds < list[0].t - 2) return null;
  let low = 0;
  let high = list.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >> 1;
    if (list[middle].t <= seconds) low = middle;
    else high = middle - 1;
  }
  return list[low];
}

/** A per-minute value at `ms`: the frame before it, or a straight line to the next one. */
function frameValue(frames: Json[], key: string, index: number, ms: number, between: boolean): number | null {
  if (!frames.length || !Array.isArray(frames[0][key])) return null;
  let at = 0;
  while (at + 1 < frames.length && frames[at + 1].t <= ms) at += 1;
  const here = Number(frames[at][key][index]) || 0;
  const next = frames[at + 1];
  if (!between || !next || next.t <= frames[at].t) return here;
  const share = Math.min(1, Math.max(0, (ms - frames[at].t) / (next.t - frames[at].t)));
  return here + ((Number(next[key][index]) || 0) - here) * share;
}

/**
 * Lanes for games without recorded positions: Smite = jungle, then the
 * client's support / carry / middle / top guesses (often wrong on their own),
 * the rest in the order left.
 */
function guessRoles(details: GameDetails, rows: Row[]): void {
  const info = new Map<number, Json>((details.game.participants ?? []).map((participant: Json) => [participant.participantId, participant.timeline ?? {}]));
  for (const team of [100, 200]) {
    const members = rows.filter((row) => row.team === team);
    const free = new Set([0, 1, 2, 3, 4]);
    const give = (row: Row | undefined, role: number) => {
      if (!row || row.role !== 9 || !free.has(role)) return;
      row.role = role;
      free.delete(role);
    };
    give(members.find((row) => row.spells.includes(SMITE)), 1);
    give(members.find((row) => row.role === 9 && /SUPPORT/.test(String(info.get(row.id)?.role))), 4);
    give(members.find((row) => row.role === 9 && /CARRY/.test(String(info.get(row.id)?.role))), 3);
    give(members.find((row) => row.role === 9 && info.get(row.id)?.lane === "MIDDLE"), 2);
    give(members.find((row) => row.role === 9 && info.get(row.id)?.lane === "TOP"), 0);
    for (const row of members) give(row, [...free][0]);
  }
}

/**
 * CS at `seconds`, as close as the data allows: the timeline is exact once a
 * minute, Riot's live CS (every snapshot) is rounded down to a step of 10, and
 * the snapshot where it goes up a step marks when that multiple of 10 was
 * reached. Between those points the CS grows in a straight line, kept inside
 * the step the live value allows.
 */
function csAt(frames: Json[], id: number, live: LiveLog | null, index: number, seconds: number): number | null {
  const anchors: [number, number][] = [];
  if (frames.length && Array.isArray(frames[0].cs)) for (const frame of frames) anchors.push([Number(frame.t) / 1000, Number(frame.cs[id - 1]) || 0]);
  let floor: number | null = null;
  if (live && index >= 0) {
    let previous: { t: number; cs: number } | null = null;
    for (const snapshot of live.snapshots) {
      const value = snapshot.p[index]?.[4];
      if (value === undefined) continue;
      if (previous && value > previous.cs) anchors.push([(previous.t + snapshot.t) / 2, value]);
      if (snapshot.t <= seconds) floor = value;
      previous = { t: snapshot.t, cs: value };
    }
  }
  if (!anchors.length) return floor;
  anchors.sort((a, b) => a[0] - b[0]);
  // Never going down (a step seen live can only be confirmed by a later minute).
  for (let at = 1; at < anchors.length; at += 1) anchors[at][1] = Math.max(anchors[at][1], anchors[at - 1][1]);
  let value: number;
  const after = anchors.findIndex(([time]) => time > seconds);
  if (after === 0) value = anchors[0][1] * Math.max(0, seconds / Math.max(1, anchors[0][0]));
  else if (after < 0) value = anchors.at(-1)![1];
  else {
    const [t0, v0] = anchors[after - 1];
    const [t1, v1] = anchors[after];
    value = v0 + ((v1 - v0) * (seconds - t0)) / Math.max(0.001, t1 - t0);
  }
  value = Math.floor(value);
  if (floor !== null) value = Math.min(Math.max(value, floor), floor + 9);
  return value;
}

function rowsAt(details: GameDetails | null, live: LiveLog | null, seconds: number, myName: string): Row[] {
  const ms = Math.max(0, seconds) * 1000;
  const snapshot = snapshotAt(live, seconds);
  const frames: Json[] = details?.timeline?.frames ?? [];
  const kills: Json[] = (details?.timeline?.events ?? []).filter((event) => event.type === "CHAMPION_KILL" && Number(event.t) <= ms);
  const respawn = (sample: number[] | undefined) => (sample?.length && sample[6] ? Math.max(0, sample[7] / 10 - (seconds - (snapshot?.t ?? seconds))) : 0);
  const roleOf = (index: number) => {
    const position = live?.players[index]?.position ?? "";
    const role = ROLES.indexOf(position);
    return role < 0 ? 9 : role;
  };
  if (details) {
    const rows = players(details).map((player): Row => {
      const index =
        live?.players.findIndex(
          (entry) =>
            (entry.name && entry.name.toLowerCase() === player.name.toLowerCase()) ||
            (liveTeam(entry.team) === player.team && championId(entry.champion) === player.champion),
        ) ?? -1;
      const sample = index >= 0 ? snapshot?.p[index] : undefined;
      return {
        id: player.id,
        champion: player.champion,
        name: player.name,
        team: player.team,
        me: player.me,
        spells: player.spells,
        level: sample?.length ? sample[0] : frameValue(frames, "level", player.id - 1, ms, false),
        k: sample?.length ? sample[1] : kills.filter((event) => event.killer === player.id).length,
        d: sample?.length ? sample[2] : kills.filter((event) => event.victim === player.id).length,
        a: sample?.length ? sample[3] : kills.filter((event) => (event.assists ?? []).includes(player.id)).length,
        cs: csAt(frames, player.id, live, index, seconds),
        gold: frameValue(frames, "gold", player.id - 1, ms, true),
        items: sample?.length ? sample.slice(8, 15) : null,
        respawn: respawn(sample),
        role: index >= 0 ? roleOf(index) : 9,
      };
    });
    if (rows.some((row) => row.role === 9)) {
      rows.forEach((row) => (row.role = 9));
      guessRoles(details, rows);
    }
    return rows;
  }
  const me = myName.split("#")[0].trim().toLowerCase();
  return (live?.players ?? []).map((entry, index): Row => {
    const sample = snapshot?.p[index];
    return {
      id: index + 1,
      champion: championId(entry.champion),
      name: entry.name || entry.champion,
      team: liveTeam(entry.team),
      me: Boolean(me) && entry.name.toLowerCase() === me,
      spells: [],
      level: sample?.length ? sample[0] : null,
      k: sample?.[1] ?? 0,
      d: sample?.[2] ?? 0,
      a: sample?.[3] ?? 0,
      cs: csAt([], index + 1, live, index, seconds),
      gold: null,
      items: sample?.length ? sample.slice(8, 15) : null,
      respawn: respawn(sample),
      role: roleOf(index),
    };
  });
}

/** The overlay at `seconds` of game time. */
export function broadcastHud(details: GameDetails | null, live: LiveLog | null, seconds: number, myName: string): Hud | null {
  const rows = rowsAt(details, live, seconds, myName);
  if (!rows.length) return null;
  const ms = Math.max(0, seconds) * 1000;
  const events: Json[] = (details?.timeline?.events ?? []).filter((event) => Number(event.t) <= ms);
  const teamOf = new Map<number, number>(rows.map((row) => [row.id, row.team]));
  const rift = !details || Number(details.game.mapId) === 11;
  const takenBy = (team: number, test: (event: Json) => boolean) =>
    events.filter((event) => test(event) && (event.type === "BUILDING_KILL" ? event.team !== team : teamOf.get(event.killer) === team));
  const sides = [100, 200].map((team) => {
    const members = rows.filter((row) => row.team === team).sort((a, b) => a.role - b.role || a.id - b.id);
    const barons = takenBy(team, (event) => event.monster === "BARON_NASHOR");
    const lastBaron = barons.at(-1);
    return {
      team,
      members,
      kills: members.reduce((total, row) => total + row.k, 0),
      gold: members.some((row) => row.gold !== null) ? members.reduce((total, row) => total + (row.gold ?? 0), 0) : null,
      towers: takenBy(team, (event) => event.building === "TOWER_BUILDING").length,
      grubs: takenBy(team, (event) => event.monster === "HORDE").length,
      dragons: takenBy(team, (event) => event.monster === "DRAGON").map((event) => String(event.sub || "")),
      baronLeft: lastBaron ? BARON_BUFF - (seconds - Number(lastBaron.t) / 1000) : 0,
    };
  });
  const [blue, red] = sides;
  const lead = blue.gold !== null && red.gold !== null ? blue.gold - red.gold : 0;

  // ---------------------------------------------------------- banner
  const dragonIcons = (list: string[]) => list.map((sub) => picture(DRAGON_ICONS[sub] ?? ICONS.dragon, "hud-drake")).join("");
  const side = (entry: (typeof sides)[number], className: string) => `
    <div class="hud-side ${className}">
      <span class="hud-team">${esc(t(entry.team === 100 ? "rec.hudBlue" : "rec.hudRed"))}</span>
      <span class="hud-stat">${picture(entry.team === 100 ? ICONS.towerBlue : ICONS.towerRed, "hud-tower")}<b>${entry.towers}</b></span>
      ${entry.gold !== null ? `<span class="hud-stat hud-gold">${coins()}<b>${esc(short(entry.gold))}</b>${(entry.team === 100 ? lead : -lead) > 0 ? `<em>+${esc(short(Math.abs(lead)))}</em>` : ""}</span>` : ""}
      <b class="hud-kills">${entry.kills}</b>
    </div>`;
  const under = (entry: (typeof sides)[number], className: string) => `
    <div class="hud-under ${className}">
      ${rift ? `<span class="hud-stat">${picture(ICONS.grubs, "hud-grub")}<b>${entry.grubs}</b></span><span class="hud-drakes">${dragonIcons(entry.dragons)}</span>` : ""}
      ${entry.baronLeft > 0 ? `<span class="hud-buff">${picture(ICONS.baron, "hud-buff-icon")}<b>${clock(entry.baronLeft)}</b></span>` : ""}
    </div>`;
  const banner = `
    <div class="hud-banner">
      <div class="hud-top">${side(blue, "is-blue")}<span class="hud-vs"></span>${side(red, "is-red")}</div>
      <div class="hud-bottom">${under(blue, "is-blue")}<span class="hud-clock">${clock(seconds)}</span>${under(red, "is-red")}</div>
    </div>`;

  // ---------------------------------------------------------- objective timers
  let timers = "";
  if (rift && details) {
    const monsters = events.filter((event) => event.type === "ELITE_MONSTER_KILL");
    const dragons = monsters.filter((event) => event.monster === "DRAGON");
    const soul = sides.some((entry) => entry.dragons.filter((sub) => sub !== "ELDER_DRAGON").length >= 4);
    const lastDragon = dragons.at(-1);
    const dragonAt = lastDragon ? Number(lastDragon.t) / 1000 + (soul ? ELDER_RESPAWN : DRAGON_RESPAWN) : DRAGON_FIRST;
    const lastBaron = monsters.filter((event) => event.monster === "BARON_NASHOR").at(-1);
    const baronAt = lastBaron ? Number(lastBaron.t) / 1000 + BARON_RESPAWN : BARON_FIRST;
    const timer = (path: string, at: number) =>
      `<span class="hud-timer${seconds >= at ? " is-up" : ""}">${picture(path, "")}<b>${seconds >= at ? esc(t("rec.hudUp")) : clock(at - seconds)}</b></span>`;
    timers = `<div class="hud-timers">${timer(soul ? DRAGON_ICONS.ELDER_DRAGON : ICONS.dragon, dragonAt)}${timer(ICONS.baron, baronAt)}</div>`;
  }

  // ---------------------------------------------------------- players
  const withItems = rows.some((row) => row.items !== null);
  const player = (row: Row | undefined, className: string) => {
    if (!row) return `<div class="hud-player ${className}"></div>`;
    return `
      <div class="hud-player ${className}${row.me ? " is-me" : ""}${row.respawn > 0 ? " is-dead" : ""}">
        ${withItems ? `<span class="hud-items">${(row.items ?? [0, 0, 0, 0, 0, 0, 0]).map((item) => itemImage(item, 30)).join("")}</span>` : ""}
        <span class="hud-spells">${row.spells.map((spell) => spellImage(spell, 21)).join("")}</span>
        <span class="hud-champ"><img src="${esc(championIcon(row.champion))}" alt="" width="44" height="44" draggable="false"/>${row.level !== null ? `<em>${row.level}</em>` : ""}${row.respawn > 0 ? `<i>${Math.ceil(row.respawn)}</i>` : ""}</span>
        <span class="hud-pname">${esc(row.name)}</span>
        <span class="hud-score"><b>${row.cs ?? ""}</b><span>${row.k}/${row.d}/${row.a}</span></span>
      </div>`;
  };
  const lines = [0, 1, 2, 3, 4]
    .map((index) => {
      const left = blue.members[index];
      const right = red.members[index];
      const diff = rift && left?.gold != null && right?.gold != null ? left.gold - right.gold : null;
      const middle = diff === null ? "" : `<span class="hud-diff ${diff >= 0 ? "lead-blue" : "lead-red"}">${esc(short(Math.abs(diff)))}</span>`;
      return `<div class="hud-line">${player(left, "is-blue")}<div class="hud-mid">${middle}</div>${player(right, "is-red")}</div>`;
    })
    .join("");
  const panel = `<div class="hud-panel${withItems ? "" : " no-items"}">${lines}</div>`;
  return { banner, timers, panel };
}
