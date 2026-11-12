// Champ select builds: OP.GG's runes, summoner spells, items and skill order
// for the champion the player picks, and buttons that put them into the
// League client. Opens by itself when champ select starts (if enabled) and
// can be opened from the sidebar to look up any champion.
import {
  ApiError,
  api,
  championIcon,
  events,
  gameImage,
  type ApplyRequest,
  type BuildMode,
  type BuildPrefs,
  type ChampSelect,
  type ChampionBuild,
  type GameAssets,
  type IdSet,
  type LiveGame,
  type LiveStats,
  type LiveStatsPlayer,
  type Suggestion,
  type Threats,
} from "./api";
import { errorText, localeTag, t, type MessageKey } from "./i18n";
import { $, esc, fragment, leave, toast } from "./dom";
import { icon } from "./icons";
import championMeta from "./data/champion-meta.json";
import { emblem } from "./ranks";

const META = championMeta as Record<string, { n: string; c: string; d: string; p: string[] }>;
/** Champions whose kit heals a lot (anti-heal is worth buying against two or more). */
const HEALERS = new Set([2, 8, 9, 16, 19, 36, 37, 48, 50, 75, 106, 114, 154, 233, 266, 267, 350, 420, 517, 887, 902]);

function threatsOf(enemies: number[]): Threats {
  const threats: Threats = { magic: 0, physical: 0, tanks: 0, healers: 0 };
  for (const id of enemies) {
    const meta = META[String(id)];
    if (!meta) continue;
    if (meta.d === "M") threats.magic++;
    else threats.physical++;
    if (meta.c === "Tank") threats.tanks++;
    if (HEALERS.has(id)) threats.healers++;
  }
  return threats;
}
const threatsKey = (threats: Threats): string => `${threats.magic}:${threats.physical}:${threats.tanks}:${threats.healers}`;
const CHAMPIONS = Object.entries(META)
  .map(([id, meta]) => ({ id: Number(id), name: meta.n }))
  .sort((left, right) => left.name.localeCompare(right.name));
const championName = (id: number): string => META[String(id)]?.n ?? `#${id}`;

const LANES: [string, MessageKey][] = [
  ["top", "stats.role.top"], ["jungle", "stats.role.jungle"], ["mid", "stats.role.mid"],
  ["adc", "stats.role.bot"], ["support", "stats.role.support"],
];
const LANE_GLYPH: Record<string, string> = { top: "TOP", jungle: "JUNGLE", mid: "MIDDLE", adc: "BOTTOM", support: "UTILITY" };
const SKILLS = ["Q", "W", "E", "R"];

const GLYPHS: Record<string, string> = {
  TOP: '<path d="M4 4h13l-3 3H7v7l-3 3z"/><rect x="10" y="10" width="4" height="4"/><path d="M20 7v13H7l3-3h7V10z" opacity=".35"/>',
  JUNGLE: '<path d="M7 3c1 5 3 8 5 10-1-4-1-7 0-10 1 3 1 6 0 10 2-2 4-5 5-10 0 6-2 11-5 18C9 14 7 9 7 3z"/>',
  MIDDLE: '<path d="M4 4h6L4 10zM20 20h-6l6-6z" opacity=".35"/><path d="M17 4h3v3L7 20H4v-3z"/>',
  BOTTOM: '<path d="M20 20H7l3-3h7v-7l3-3z"/><rect x="10" y="10" width="4" height="4"/><path d="M4 17V4h13l-3 3H7v7z" opacity=".35"/>',
  UTILITY: '<path d="M12 7l2.5 3-2.5 9-2.5-9zM3 7h6l1.5 2-2.5 3-2-1zM21 7h-6l-1.5 2 2.5 3 2-1zM10 4h4l-2 2z"/>',
};
const laneGlyph = (lane: string, size = 16): string =>
  `<svg class="role-glyph" viewBox="0 0 24 24" width="${size}" height="${size}" aria-hidden="true" fill="currentColor">${GLYPHS[LANE_GLYPH[lane]] ?? ""}</svg>`;

const percent = (value: number, digits = 1): string =>
  new Intl.NumberFormat(localeTag(), { style: "percent", maximumFractionDigits: digits, minimumFractionDigits: digits }).format(value);
const count = (value: number): string => new Intl.NumberFormat(localeTag()).format(value);
const describe = (error: unknown): string =>
  error instanceof ApiError ? errorText(error.code, error.detail) : errorText("unknown", String(error));
const rateTone = (rate: number): string => (rate >= 0.52 ? "is-good" : rate < 0.48 ? "is-bad" : "");

let assets: GameAssets | null = null;
let assetsLoading: Promise<GameAssets | null> | null = null;
const loadAssets = (): Promise<GameAssets | null> => {
  assetsLoading ??= api.buildAssets().then((value) => (assets = value)).catch(() => null);
  return assetsLoading;
};

const image = (path: string | undefined, label: string, size: number, extra = ""): string =>
  path
    ? `<img src="${esc(gameImage(path))}" alt="${esc(label)}" width="${size}" height="${size}" loading="lazy" draggable="false" ${extra}/>`
    : `<span class="asset-blank" style="width:${size}px;height:${size}px"></span>`;
const perkImage = (id: number, size: number, extra = "") => image(assets?.perks[id]?.icon, assets?.perks[id]?.name ?? "", size, `data-tooltip="${esc(assets?.perks[id]?.name ?? "")}" ${extra}`);
const styleImage = (id: number, size: number) => image(assets?.styles[id]?.icon, assets?.styles[id]?.name ?? "", size, `data-tooltip="${esc(assets?.styles[id]?.name ?? "")}"`);
const itemImage = (id: number, size = 32) => image(assets?.items[id]?.icon, assets?.items[id]?.name ?? "", size, `data-tooltip="${esc(assets?.items[id]?.name ?? String(id))}"`);
const spellImage = (id: number, size = 30) => image(assets?.spells[id]?.icon, assets?.spells[id]?.name ?? "", size, `data-tooltip="${esc(assets?.spells[id]?.name ?? "")}"`);

// ---------------------------------------------------------------- page state

interface PageState {
  championId: number;
  mode: BuildMode;
  /** Requested lane; empty = the champion's main lane. */
  position: string;
  build: ChampionBuild | null;
  loading: boolean;
  error: string;
  rune: number;
  spells: number;
  query: string;
  picking: boolean;
  applying: boolean;
  /** Strong picks for the lane, shown before a champion is chosen. */
  suggestions: Suggestion[] | null;
  /** Enemy team the plan was made for. */
  threats: string;
  /** Live game numbers or the build (a game in progress opens on the live view). */
  view: "build" | "live";
}

let champSelect: ChampSelect = { active: false, championId: 0, locked: false, position: "", mode: null, queueId: 0, enemies: [] };
const currentThreats = (): Threats => threatsOf(champSelect.active ? champSelect.enemies : []);
let prefs: BuildPrefs = { experimental: false, openOnChampSelect: true, autoApply: false, autoRunes: true, autoSpells: true, autoItems: true };
let page: { state: PageState; render: () => void; load: () => Promise<void>; close: () => void } | null = null;
/** Champion and lane last applied automatically in this champ select. */
let autoApplied = "";

const runeName = (state: PageState, build: ChampionBuild): string => {
  const lane = LANES.find(([lane]) => lane === build.position)?.[1];
  return `LC: ${championName(state.championId)}${lane ? ` ${t(lane)}` : build.mode === "aram" ? " ARAM" : ""}`.slice(0, 40);
};

function applyRequest(state: PageState, build: ChampionBuild, parts: { runes: boolean; spells: boolean; items: boolean }): ApplyRequest {
  const name = runeName(state, build);
  return {
    championId: state.championId,
    mode: state.mode,
    position: build.position,
    runePage: parts.runes ? state.rune : null,
    runeName: name,
    spells: parts.spells ? state.spells : null,
    itemSet: parts.items
      ? {
          title: name,
          starter: t("builds.start"),
          core: t("builds.order"),
          situational: t("builds.swaps"),
          consumables: t("builds.consumables"),
        }
      : null,
    threats: currentThreats(),
  };
}

async function apply(state: PageState, parts: { runes: boolean; spells: boolean; items: boolean }, quiet = false): Promise<void> {
  const build = state.build;
  if (!build) return;
  state.applying = true;
  page?.render();
  try {
    const result = await api.applyBuild(applyRequest(state, build, parts));
    const done = [
      result.runes ? t(result.runes === "replacedCurrent" ? "builds.doneRunesReplaced" : "builds.doneRunes") : "",
      result.spells ? t("builds.doneSpells") : "",
      result.itemSet ? t("builds.doneItems") : "",
    ].filter(Boolean);
    if (done.length) toast("success", t("builds.applied", { champion: championName(state.championId) }), done.join(" · "), quiet ? 4000 : 5000);
    for (const code of result.errors) toast("error", t("builds.applyFailed"), errorText(code), 6000);
  } catch (error) {
    toast("error", t("builds.applyFailed"), describe(error), 6000);
  } finally {
    state.applying = false;
    page?.render();
  }
}

// ---------------------------------------------------------------- html

function headHtml(state: PageState): string {
  const build = state.build;
  const lanes = state.mode === "ranked"
    ? `<div class="segmented build-lanes">${LANES.map(([lane, label]) => {
        const share = build?.positions.find((entry) => entry.position === lane);
        const current = (build?.position || state.position) === lane;
        return `<button data-build-lane="${lane}" aria-pressed="${current}" ${share ? `data-tooltip="${esc(`${t(label)} · ${percent(share.roleRate, 0)}`)}"` : ""} class="${share ? "" : "is-rare"}">${laneGlyph(lane)}<span>${esc(t(label))}</span></button>`;
      }).join("")}</div>`
    : "";
  const status = liveGame && !champSelect.active
    ? `<span class="build-status is-live">${esc(t("live.tab"))}</span>`
    : champSelect.active
    ? `<span class="build-status is-live">${esc(champSelect.championId ? t(champSelect.locked ? "builds.locked" : "builds.hovering") : t("builds.waiting"))}</span>`
    : `<span class="build-status">${esc(t("builds.browsing"))}</span>`;
  const enemies = champSelect.active && champSelect.enemies.length
    ? (() => {
        const threats = currentThreats();
        return `<div class="build-enemies"><span>${esc(t("builds.enemyTeam"))}</span>${champSelect.enemies.map((id) => `<img src="${esc(championIcon(id))}" alt="" data-tooltip="${esc(championName(id))}" draggable="false" />`).join("")}<em>${esc(t("builds.enemyDamage", { magic: threats.magic, physical: threats.physical }))}</em></div>`;
      })()
    : "";
  const matches = state.query
    ? CHAMPIONS.filter((champion) => champion.name.toLowerCase().includes(state.query.toLowerCase())).slice(0, 8)
    : [];
  return `
    <div class="build-champion">
      ${state.championId ? `<img src="${esc(championIcon(state.championId))}" alt="" draggable="false" />` : `<span class="asset-blank"></span>`}
      <div>
        <h2>${esc(state.championId ? championName(state.championId) : t("builds.pick"))}</h2>
        <div class="build-meta">
          ${status}
          ${build ? `
            ${build.tier ? `<span class="build-tier tier-${build.tier}">${esc(t("builds.tier", { tier: build.tier }))}</span>` : ""}
            <span>${esc(t("stats.winRate"))} <b class="${rateTone(build.winRate)}">${esc(percent(build.winRate))}</b></span>
            <span>${esc(t("builds.pickRate"))} <b>${esc(percent(build.pickRate))}</b></span>
            ${build.banRate ? `<span>${esc(t("builds.banRate"))} <b>${esc(percent(build.banRate))}</b></span>` : ""}
            <span class="muted">${esc(t("stats.gamesCount", { count: count(build.games) }))}</span>` : ""}
        </div>
        ${enemies}
      </div>
    </div>
    <div class="build-controls">
      ${liveGame ? `
      <div class="segmented build-views">
        <button data-build-view="live" aria-pressed="${state.view === "live"}">${esc(t("live.tab"))}</button>
        <button data-build-view="build" aria-pressed="${state.view === "build"}">${esc(t("live.buildTab"))}</button>
      </div>` : ""}
      <div class="segmented">
        <button data-build-mode="ranked" aria-pressed="${state.mode === "ranked"}">${esc(t("builds.ranked"))}</button>
        <button data-build-mode="aram" aria-pressed="${state.mode === "aram"}">ARAM</button>
      </div>
      ${lanes}
      <div class="build-search">
        <label class="search">${icon("search", 14)}<input data-build-search type="search" placeholder="${esc(t("builds.search"))}" value="${esc(state.query)}" /></label>
        ${matches.length ? `<ul class="build-results">${matches.map((champion) => `
          <li><button data-build-champion="${champion.id}"><img src="${esc(championIcon(champion.id))}" alt="" draggable="false" />${esc(champion.name)}</button></li>`).join("")}</ul>` : ""}
      </div>
      <button class="icon-btn profile-close" data-build="close" aria-label="${esc(t("common.close"))}">${icon("x", 16)}</button>
    </div>`;
}

function runeTree(styleId: number, rows: number[][], selected: number[]): string {
  return `
    <div class="rune-tree">
      <div class="rune-tree-head">${styleImage(styleId, 22)}<span>${esc(assets?.styles[styleId]?.name ?? "")}</span></div>
      ${rows.map((row, index) => `
        <div class="rune-row${index === 0 && rows.length === 4 ? " is-keystones" : ""}">${row.map((perk) => {
          const chosen = selected.includes(perk);
          return `<span class="rune${chosen ? " is-chosen" : ""}">${perkImage(perk, index === 0 && rows.length === 4 ? 40 : 30)}</span>`;
        }).join("")}</div>`).join("")}
    </div>`;
}

function runesHtml(state: PageState, build: ChampionBuild): string {
  const page = build.runePages[state.rune] ?? build.runePages[0];
  const primary = assets?.styles[page.primaryStyle]?.slots ?? [];
  const secondary = assets?.styles[page.subStyle]?.slots ?? [];
  // Perk trees: keystones + three rows; then shard rows (same in every tree).
  const shards = primary.slice(4);
  const tabs = build.runePages.map((option, index) => `
    <button class="rune-option${index === state.rune ? " is-selected" : ""}" data-build-rune="${index}">
      <span class="rune-option-icons">${perkImage(option.perks[0], 34)}${styleImage(option.subStyle, 20)}</span>
      <span class="rune-option-rates"><b class="${rateTone(option.winRate)}">${esc(percent(option.winRate))}</b><em>${esc(t("builds.pickShort", { rate: percent(option.pickRate, 0) }))}</em><em>${esc(t("stats.gamesCount", { count: count(option.games) }))}</em></span>
    </button>`).join("");
  return `
    <section class="profile-panel build-runes">
      <header class="panel-head">
        <h3>${esc(t("builds.runes"))}</h3>
        <button class="btn btn-ghost btn-small" data-build-apply="runes" ${state.applying ? "disabled" : ""}>${icon("download", 13)}<span>${esc(t("builds.applyRunes"))}</span></button>
      </header>
      <div class="rune-options">${tabs}</div>
      ${assets ? `
      <div class="rune-trees">
        ${runeTree(page.primaryStyle, primary.slice(0, 4), page.perks)}
        ${runeTree(page.subStyle, secondary.slice(1, 4), page.perks)}
        <div class="rune-tree is-shards">
          <div class="rune-tree-head"><span>${esc(t("builds.shards"))}</span></div>
          ${shards.map((row, index) => `
            <div class="rune-row">${row.map((perk) => `<span class="rune is-shard${page.perks[6 + index] === perk ? " is-chosen" : ""}">${perkImage(perk, 22)}</span>`).join("")}</div>`).join("")}
        </div>
      </div>` : `<p class="muted">${esc(t("builds.loadingAssets"))}</p>`}
    </section>`;
}

function spellsHtml(state: PageState, build: ChampionBuild): string {
  if (!build.spells.length) return "";
  return `
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("builds.spells"))}</h3>
        <button class="btn btn-ghost btn-small" data-build-apply="spells" ${state.applying || !champSelect.active ? "disabled" : ""} ${champSelect.active ? "" : `data-tooltip="${esc(t("builds.spellsHint"))}"`}>${icon("download", 13)}<span>${esc(t("builds.applySpells"))}</span></button>
      </header>
      <div class="spell-options">${build.spells.map((option, index) => `
        <button class="spell-option${index === state.spells ? " is-selected" : ""}" data-build-spells="${index}">
          <span class="spell-icons">${option.ids.map((id) => spellImage(id)).join("")}</span>
          <b class="${rateTone(option.winRate)}">${esc(percent(option.winRate))}</b>
          <em>${esc(t("builds.pickShort", { rate: percent(option.pickRate, 0) }))}</em>
        </button>`).join("")}
      </div>
    </section>`;
}

function itemRow(label: string, sets: IdSet[], arrows: boolean): string {
  if (!sets.length) return "";
  return `
    <div class="item-block">
      <h4 class="sub-head"><span>${esc(label)}</span></h4>
      ${sets.map((set) => `
        <div class="item-set">
          <span class="item-icons${arrows ? " is-path" : ""}">${set.ids.map((id) => itemImage(id)).join(arrows ? `<span class="item-arrow">›</span>` : "")}</span>
          <span class="item-rates"><b class="${rateTone(set.winRate)}">${esc(percent(set.winRate))}</b><em>${esc(t("stats.gamesCount", { count: count(set.games) }))}</em></span>
        </div>`).join("")}
    </div>`;
}

const REASONS: Record<string, MessageKey> = {
  magicResist: "builds.reason.magicResist", armor: "builds.reason.armor", antiHeal: "builds.reason.antiHeal",
};

function itemsHtml(state: PageState, build: ChampionBuild): string {
  const plan = build.plan;
  const adapted = new Map(plan.adapted.map((entry) => [entry.item, entry.reason]));
  const step = (id: number, index: number) => {
    const reason = adapted.get(id);
    return `
      <span class="plan-step${reason ? " is-adapted" : ""}" ${reason ? `data-tooltip="${esc(`${assets?.items[id]?.name ?? ""} · ${t(REASONS[reason])}`)}"` : ""}>
        ${itemImage(id, 44)}
        <em>${index + 1}</em>
      </span>`;
  };
  return `
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("builds.plan"))}</h3>
        <button class="btn btn-ghost btn-small" data-build-apply="items" ${state.applying ? "disabled" : ""}>${icon("download", 13)}<span>${esc(t("builds.applyItems"))}</span></button>
      </header>
      <div class="plan">
        <div class="plan-start">
          <h4 class="sub-head"><span>${esc(t("builds.start"))}</span></h4>
          <span class="item-icons">${plan.starter.map((id) => itemImage(id, 36)).join("")}</span>
        </div>
        <div class="plan-order">
          <h4 class="sub-head"><span>${esc(t("builds.order"))}</span></h4>
          <div class="plan-steps">${plan.order.map(step).join(`<span class="item-arrow">›</span>`)}</div>
        </div>
      </div>
      ${plan.adapted.length ? `
        <div class="plan-adapted">
          <span>${esc(t("builds.adaptedTitle"))}</span>
          ${plan.adapted.map((entry) => `<span class="plan-chip">${itemImage(entry.item, 18)}${esc(assets?.items[entry.item]?.name ?? "")} · ${esc(t(REASONS[entry.reason]))}</span>`).join("")}
        </div>` : ""}
      ${plan.swaps.length ? `
        <div class="item-block">
          <h4 class="sub-head"><span>${esc(t("builds.swaps"))}</span></h4>
          <div class="item-grid">${plan.swaps.map((id) => {
            const set = build.situational.find((entry) => entry.ids[0] === id);
            return `<span class="item-cell" data-tooltip="${esc(assets?.items[id]?.name ?? "")}">${itemImage(id, 34)}${set ? `<em class="${rateTone(set.winRate)}">${esc(percent(set.winRate, 0))}</em>` : ""}</span>`;
          }).join("")}
          </div>
        </div>` : ""}
      <details class="build-more">
        <summary>${esc(t("builds.otherOptions"))}</summary>
        ${itemRow(t("builds.starter"), build.starterItems, false)}
        ${itemRow(t("builds.core"), build.coreItems, true)}
        ${itemRow(t("builds.boots"), build.boots.slice(0, 2), false)}
      </details>
    </section>`;
}

/** Before the pick: the strongest champions for the player's lane. */
function suggestionsHtml(state: PageState): string {
  const lane = LANES.find(([lane]) => lane === (champSelect.position || state.position))?.[1];
  const title = lane ? t("builds.suggestTitle", { lane: t(lane) }) : t("builds.suggestAll");
  const list = state.suggestions;
  return `
    <section class="profile-panel">
      <header class="panel-head"><h3>${esc(title)}</h3><span class="muted">${esc(t("builds.waiting"))}</span></header>
      ${list === null ? `<p class="muted">${esc(t("builds.loading"))}</p>` : `
      <div class="suggestions">${list.map((pick) => `
        <button class="suggestion" data-build-champion="${pick.championId}">
          <img src="${esc(championIcon(pick.championId))}" alt="" draggable="false" />
          <span><b>${esc(championName(pick.championId))}</b><em><span class="build-tier tier-${pick.tier}">${esc(t("builds.tier", { tier: pick.tier }))}</span> <span class="${rateTone(pick.winRate)}">${esc(percent(pick.winRate))}</span></em></span>
        </button>`).join("")}
      </div>`}
    </section>`;
}

function skillsHtml(build: ChampionBuild): string {
  const skills = build.skills;
  if (!skills || !skills.order.length) return "";
  return `
    <section class="profile-panel">
      <header class="panel-head">
        <h3>${esc(t("builds.skills"))}</h3>
        <span class="muted">${esc(`${percent(skills.winRate)} · ${t("stats.gamesCount", { count: count(skills.games) })}`)}</span>
      </header>
      <div class="skill-priority">${skills.priority.map((skill) => `<span class="skill-key">${esc(skill)}</span>`).join(`<span class="item-arrow">›</span>`)}</div>
      <div class="skill-grid" style="--levels:${skills.order.length}">
        ${SKILLS.map((skill) => `
          <span class="skill-name">${esc(skill)}</span>
          ${skills.order.map((level) => `<span class="skill-cell${level === skill ? ` is-on${skill === "R" ? " is-ult" : ""}` : ""}">${level === skill ? esc(skill) : ""}</span>`).join("")}`).join("")}
        <span></span>${skills.order.map((_, index) => `<span class="skill-level">${index + 1}</span>`).join("")}
      </div>
    </section>`;
}

function matchupsHtml(build: ChampionBuild): string {
  const list = (label: string, entries: ChampionBuild["weakAgainst"]) => entries.length ? `
    <div class="matchup-block">
      <h4 class="sub-head"><span>${esc(label)}</span></h4>
      <div class="matchups">${entries.map((entry) => `
        <span class="matchup" data-tooltip="${esc(`${championName(entry.championId)} · ${t("stats.gamesCount", { count: count(entry.games) })}`)}">
          <img src="${esc(championIcon(entry.championId))}" alt="" draggable="false" />
          <b class="${rateTone(entry.winRate)}">${esc(percent(entry.winRate))}</b>
        </span>`).join("")}
      </div>
    </div>` : "";
  if (!build.weakAgainst.length && !build.strongAgainst.length) return "";
  return `
    <section class="profile-panel">
      <h3>${esc(t("builds.matchups"))}</h3>
      ${list(t("builds.weakAgainst"), build.weakAgainst)}
      ${list(t("builds.strongAgainst"), build.strongAgainst)}
    </section>`;
}

function bodyHtml(state: PageState): string {
  const toggles = `
    <div class="build-bar">
      <button class="btn btn-play" data-build-apply="all" ${state.applying || !state.build ? "disabled" : ""}>${icon("download", 14)}<span>${esc(t("builds.applyAll"))}</span></button>
      <label class="toggle-row build-toggle"><span class="toggle-text"><strong>${esc(t("builds.autoApply"))}</strong></span><input type="checkbox" data-build-pref="autoApply" ${prefs.autoApply ? "checked" : ""}/><span class="switch"></span></label>
      <label class="toggle-row build-toggle"><span class="toggle-text"><strong>${esc(t("builds.openOnChampSelect"))}</strong></span><input type="checkbox" data-build-pref="openOnChampSelect" ${prefs.openOnChampSelect ? "checked" : ""}/><span class="switch"></span></label>
      <span class="build-source">${esc(t("builds.source"))}</span>
    </div>`;
  if (champSelect.active && champSelect.mode === null) {
    return `${toggles}<div class="profile-empty">${icon("info", 18)}<div><strong>${esc(t("builds.unsupportedTitle"))}</strong><p>${esc(t("builds.unsupportedText"))}</p></div></div>`;
  }
  if (!state.championId) return `${toggles}${suggestionsHtml(state)}`;
  if (state.loading && !state.build) return `${toggles}<p class="muted build-loading">${esc(t("builds.loading"))}</p>`;
  if (state.error) return `${toggles}<div class="profile-empty">${icon("alert", 18)}<div><strong>${esc(t("builds.failedTitle"))}</strong><p>${esc(state.error)}</p></div></div>`;
  const build = state.build;
  if (!build) return toggles;
  return `
    ${toggles}
    <div class="build-grid">
      <div class="build-col">
        ${runesHtml(state, build)}
        ${skillsHtml(build)}
      </div>
      <div class="build-col">
        ${spellsHtml(state, build)}
        ${itemsHtml(state, build)}
        ${matchupsHtml(build)}
      </div>
    </div>`;
}

// ---------------------------------------------------------------- page shell

export function openBuilds(championId = champSelect.championId, mode: BuildMode = champSelect.mode ?? "ranked", position = champSelect.position): void {
  if (page) {
    const state = page.state;
    if (championId !== state.championId || mode !== state.mode || position !== state.position) {
      if (!championId && state.championId && !champSelect.active) return page.render();
      Object.assign(state, { championId, mode, position, build: null, rune: 0, spells: 0, error: "", suggestions: championId ? state.suggestions : null });
      void page.load();
    }
    page.render();
    return;
  }
  const state: PageState = { championId, mode, position, build: null, loading: false, error: "", rune: 0, spells: 0, query: "", picking: false, applying: false, suggestions: null, threats: "", view: liveGame ? "live" : "build" };
  const overlay = fragment(`
    <div class="profile-overlay builds-overlay" role="dialog" aria-modal="true">
      <div class="profile-page">
        <header class="profile-head build-head"></header>
        <div class="profile-body build-body"></div>
      </div>
    </div>`);
  document.body.append(overlay);
  const head = $(".build-head", overlay);
  const body = $(".build-body", overlay);
  const render = () => {
    const search = head.querySelector<HTMLInputElement>("[data-build-search]");
    const focused = document.activeElement === search;
    head.innerHTML = headHtml(state);
    head.classList.toggle("is-live", state.view === "live" && Boolean(liveGame));
    body.innerHTML = state.view === "live" && liveGame ? liveHtml() : bodyHtml(state);
    if (focused) {
      const input = head.querySelector<HTMLInputElement>("[data-build-search]");
      input?.focus();
      input?.setSelectionRange(state.query.length, state.query.length);
    }
  };
  const load = async () => {
    if (!state.championId) {
      render();
      state.suggestions = await api.laneSuggestions(champSelect.position || state.position).catch(() => []);
      return render();
    }
    const wanted = `${state.championId}:${state.mode}:${state.position}`;
    state.loading = true;
    state.error = "";
    render();
    try {
      const threats = currentThreats();
      const [build] = await Promise.all([api.championBuild(state.championId, state.mode, state.position, threats), loadAssets()]);
      if (wanted !== `${state.championId}:${state.mode}:${state.position}`) return;
      const same = state.build?.championId === build.championId && state.build?.position === build.position;
      state.build = build;
      state.threats = threatsKey(threats);
      if (!same) {
        state.rune = 0;
        state.spells = 0;
      }
    } catch (error) {
      if (wanted !== `${state.championId}:${state.mode}:${state.position}`) return;
      state.build = null;
      state.error = describe(error);
    } finally {
      state.loading = false;
      render();
    }
  };
  const close = () => {
    document.removeEventListener("keydown", onKey, true);
    page = null;
    void leave(overlay, "profile-out");
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !document.querySelector("#modal-root > *")) {
      event.preventDefault();
      if (state.query) {
        state.query = "";
        render();
      } else close();
    }
  };
  overlay.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    if (target.closest('[data-build="close"]')) return close();
    const view = target.closest<HTMLElement>("[data-build-view]")?.dataset.buildView;
    if (view === "live" || view === "build") {
      state.view = view;
      render();
      if (view === "live") void refreshLive();
      return;
    }
    const mode = target.closest<HTMLElement>("[data-build-mode]")?.dataset.buildMode;
    if (mode === "ranked" || mode === "aram") {
      if (mode !== state.mode) {
        state.mode = mode;
        state.build = null;
        void load();
      }
      return;
    }
    const lane = target.closest<HTMLElement>("[data-build-lane]")?.dataset.buildLane;
    if (lane) {
      if (lane !== (state.build?.position || state.position)) {
        state.position = lane;
        void load();
      }
      return;
    }
    const champion = target.closest<HTMLElement>("[data-build-champion]")?.dataset.buildChampion;
    if (champion) {
      state.championId = Number(champion);
      state.position = "";
      state.query = "";
      state.build = null;
      void load();
      return;
    }
    const rune = target.closest<HTMLElement>("[data-build-rune]")?.dataset.buildRune;
    if (rune !== undefined) {
      state.rune = Number(rune);
      return render();
    }
    const spells = target.closest<HTMLElement>("[data-build-spells]")?.dataset.buildSpells;
    if (spells !== undefined) {
      state.spells = Number(spells);
      return render();
    }
    const part = target.closest<HTMLElement>("[data-build-apply]")?.dataset.buildApply;
    if (part) {
      void apply(state, {
        runes: part === "runes" || part === "all",
        spells: (part === "spells" || part === "all") && champSelect.active,
        items: part === "items" || part === "all",
      });
    }
  });
  overlay.addEventListener("input", (event) => {
    const target = event.target as HTMLInputElement;
    if (target.matches("[data-build-search]")) {
      state.query = target.value;
      render();
    }
  });
  overlay.addEventListener("change", (event) => {
    const target = event.target as HTMLInputElement;
    const key = target.dataset.buildPref as keyof BuildPrefs | undefined;
    if (key) {
      prefs = { ...prefs, [key]: target.checked };
      void api.setBuildPrefs(prefs).catch((error) => toast("error", t("toast.saveFailed"), describe(error), 5000));
    }
  });
  overlay.addEventListener("error", (event) => {
    if (event.target instanceof HTMLImageElement) event.target.classList.add("is-missing");
  }, true);
  document.addEventListener("keydown", onKey, true);
  page = { state, render, load, close };
  render();
  void load();
}

/** Whether the experimental features (builds, live game) are on. */
export function experimentalOn(): boolean {
  return prefs.experimental;
}

/** Turn the experimental features on or off (Settings). */
export async function setExperimental(on: boolean): Promise<void> {
  if (prefs.experimental === on) return;
  prefs = await api.setBuildPrefs({ ...prefs, experimental: on });
  if (!on) {
    page?.close();
    champSelect = { ...champSelect, active: false };
    liveGame = null;
    liveStats = null;
    liveChampion = null;
    lastPick = null;
    updateTile();
  }
}

/** Listen for champ select; call once at startup. */
export async function initBuilds(): Promise<void> {
  prefs = await api.buildPrefs().catch(() => prefs);
  champSelect = await api.champSelectState().catch(() => champSelect);
  await events.onChampSelect((state) => {
    const previous = champSelect;
    champSelect = state;
    if (!state.active) {
      autoApplied = "";
      page?.render();
      return;
    }
    const mode = state.mode ?? "ranked";
    const started = !previous.active;
    const changed = state.championId !== previous.championId || state.position !== previous.position || state.mode !== previous.mode;
    if ((started && prefs.openOnChampSelect) || (page && (changed || started))) {
      openBuilds(state.championId, mode, state.position);
    }
    // The enemy team changed: remake the plan (and the item set, if it was applied).
    const current = page;
    if (current && current.state.build && !changed && threatsKey(currentThreats()) !== current.state.threats) {
      void current.load().then(() => {
        if (prefs.autoApply && prefs.autoItems && autoApplied && current.state.build) {
          void apply(current.state, { runes: false, spells: false, items: true }, true);
        }
      });
    } else {
      page?.render();
    }
    // Auto-apply once per locked champion and lane.
    const key = `${state.championId}:${state.position}:${mode}`;
    if (prefs.autoApply && state.locked && state.championId && state.mode !== null && autoApplied !== key) {
      autoApplied = key;
      void (async () => {
        openBuilds(state.championId, mode, state.position);
        const target = page;
        if (!target) return;
        if (!target.state.build) await target.load();
        if (target.state.build) {
          await apply(target.state, { runes: prefs.autoRunes, spells: prefs.autoSpells, items: prefs.autoItems }, true);
        }
      })();
    }
  });
}

/** The app started while champ select was already running: open the build. */
export function resumeChampSelect(): void {
  if (champSelect.active && champSelect.mode !== null && prefs.openOnChampSelect && !page) {
    openBuilds(champSelect.championId, champSelect.mode, champSelect.position);
  }
}

// ---------------------------------------------------------------- during the game

/** The champion picked in the last champ select and the game being played. */
let lastPick: { championId: number; mode: BuildMode; position: string } | null = null;
let liveChampion: { championId: number; mode: BuildMode } | null = null;

/** Show the sidebar's Builds tile while there is a pick or a game to show. */
function updateTile(): void {
  const tile = document.getElementById("builds-tile");
  if (tile) tile.hidden = !(champSelect.active || liveChampion);
}

/** Open the build page for the current pick or the champion being played. */
export function openCurrentBuild(): void {
  if (champSelect.active) return openBuilds();
  const championId = liveChampion?.championId || lastPick?.championId || 0;
  const mode = liveChampion?.mode ?? lastPick?.mode ?? "ranked";
  const position = lastPick && lastPick.championId === championId ? lastPick.position : "";
  openBuilds(championId, mode, position);
}

/** Keep track of picks and games for the Builds tile; call once at startup. */
export async function trackGames(): Promise<void> {
  await events.onChampSelect((state) => {
    if (state.active && state.championId && state.mode) {
      lastPick = { championId: state.championId, mode: state.mode, position: state.position };
    }
    updateTile();
  });
  const live = (game: LiveGame | null) => {
    const started = Boolean(game) && !liveGame;
    liveGame = game;
    const me = game?.players.find((player) => player.isMe);
    liveChampion = game && me?.championId ? { championId: me.championId, mode: game.gameMode === "ARAM" ? "aram" : "ranked" } : null;
    if (!game) {
      liveStats = null;
      if (page && page.state.view === "live") page.state.view = "build";
    } else if (started) {
      void loadAssets();
      // The game started: show the live view (without taking focus from the game).
      if (!page && prefs.openOnChampSelect) openCurrentBuild();
      if (page) page.state.view = "live";
      void refreshLive();
    }
    page?.render();
    updateTile();
  };
  live(await api.liveGameState().catch(() => null));
  await events.onLiveGame(live);
  updateTile();
}

// ---------------------------------------------------------------- live game view

let liveGame: LiveGame | null = null;
let liveStats: LiveStats | null = null;
let liveBusy = false;

/** Read the live numbers (local only) when the live view is on screen. */
async function refreshLive(): Promise<void> {
  if (liveBusy || !liveGame || !page || page.state.view !== "live" || document.hidden) return;
  liveBusy = true;
  try {
    liveStats = await api.liveStats().catch(() => liveStats);
    if (page?.state.view === "live") page.render();
  } finally {
    liveBusy = false;
  }
}
setInterval(() => void refreshLive(), 3000);
document.addEventListener("visibilitychange", () => void refreshLive());

const sameName = (left: string, right: string): boolean => left.trim().toLowerCase() === right.trim().toLowerCase();
const championIdByName = (name: string): number =>
  Number(Object.entries(META).find(([, meta]) => sameName(meta.n, name))?.[0] ?? 0);
const clock = (seconds: number): string => `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, "0")}`;
const SHORT_TIER: Record<string, string> = {
  IRON: "I", BRONZE: "B", SILVER: "S", GOLD: "G", PLATINUM: "P", EMERALD: "E", DIAMOND: "D", MASTER: "M", GRANDMASTER: "GM", CHALLENGER: "C",
};
const DIVISION: Record<string, string> = { I: "1", II: "2", III: "3", IV: "4" };

interface LiveRow {
  stats: LiveStatsPlayer | null;
  rank: LiveGame["players"][number] | null;
  championId: number;
  team: number;
}

/** Live numbers joined with each player's rank (matched by Riot ID). */
function liveRows(game: LiveGame, stats: LiveStats | null): LiveRow[] {
  if (stats && stats.players.length) {
    return stats.players.map((player) => {
      const rank = game.players.find((entry) => entry.gameName && sameName(entry.gameName, player.gameName)) ?? null;
      return { stats: player, rank, championId: rank?.championId || championIdByName(player.champion), team: player.team };
    });
  }
  return game.players.map((player) => ({ stats: null, rank: player, championId: player.championId, team: player.team }));
}

function rankCell(rank: LiveRow["rank"]): string {
  const queue = rank?.solo ?? rank?.flex ?? null;
  if (!queue) return `<span class="live-rank muted">${esc(t("live.unranked"))}</span>`;
  const games = queue.wins + queue.losses;
  const rate = games ? queue.wins / games : 0;
  return `<span class="live-rank">${emblem(queue.tier, 16)}<b>${esc(`${SHORT_TIER[queue.tier] ?? queue.tier[0]}${DIVISION[queue.division] ?? ""}`)}</b><em class="${rateTone(rate)}">${esc(percent(rate, 0))}</em><small>${esc(`${games}G`)}</small></span>`;
}

function itemGold(row: LiveRow): number {
  return (row.stats?.items ?? []).reduce((sum, id) => sum + (assets?.items[id]?.price ?? 0), 0);
}

function teamHtml(rows: LiveRow[], team: number, mine: boolean, stats: LiveStats | null): string {
  const players = rows.filter((row) => row.team === team);
  const kills = players.reduce((sum, row) => sum + (row.stats?.kills ?? 0), 0);
  const minutes = Math.max(1, (stats?.gameTime ?? 0) / 60);
  const magic = players.filter((row) => META[String(row.championId)]?.d === "M").length;
  const tanks = players.filter((row) => META[String(row.championId)]?.c === "Tank").length;
  const index = team === 2 ? 1 : 0;
  const objectives = stats
    ? `<span>${esc(t("live.towers"))} <b>${stats.towers[index]}</b></span><span>${esc(t("live.dragons"))} <b>${stats.dragons[index]}</b></span><span>${esc(t("live.barons"))} <b>${stats.barons[index]}</b></span>`
    : "";
  const gold = players.reduce((sum, row) => sum + itemGold(row), 0);
  return `
    <section class="profile-panel live-team ${mine ? "is-ally" : "is-enemy"} side-${team === 2 ? "red" : "blue"}">
      <header class="live-team-head">
        <h3>${esc(t(mine ? "live.yourTeam" : "live.enemyTeam"))}<em>${esc(t(team === 2 ? "live.red" : "live.blue"))}</em></h3>
        <div class="live-team-stats">
          ${stats ? `<span>${esc(t("live.kills"))} <b>${kills}</b></span>${objectives}<span>${esc(t("live.itemGold"))} <b>${esc(count(gold))}</b></span>` : ""}
        </div>
      </header>
      <div class="live-comp">
        <span>${esc(t("live.comp", { magic, physical: players.length - magic }))}${tanks ? ` · ${esc(t("live.tanks", { count: tanks }))}` : ""}</span>
        <span class="live-comp-bar"><span style="--p:${players.length ? magic / players.length : 0}"></span></span>
      </div>
      <div class="live-table">
        <div class="live-row live-labels"><span></span><span></span><span>${esc(t("live.kda"))}</span><span>${esc(t("live.kp"))}</span><span data-tooltip="${esc(t("live.csHint"))}">${esc(t("live.cs"))} ≈</span><span>${esc(t("live.vision"))}</span><span>${esc(t("live.items"))}</span><span></span></div>
        ${players.map((row) => {
          const player = row.stats;
          const name = player?.gameName || row.rank?.gameName || t("live.hidden");
          const tag = player?.tagLine || row.rank?.tagLine || "";
          const ratio = player ? (player.kills + player.assists) / Math.max(1, player.deaths) : 0;
          const kp = player && kills ? (player.kills + player.assists) / kills : 0;
          return `
          <div class="live-row${row.rank?.isMe ? " is-me" : ""}${player?.dead ? " is-dead" : ""}">
            <span class="live-champ">${row.championId ? `<img src="${esc(championIcon(row.championId))}" alt="" draggable="false" />` : `<span class="asset-blank"></span>`}${player ? `<em>${player.level}</em>` : ""}</span>
            <span class="live-who"><b>${esc(name)}${tag ? `<small>#${esc(tag)}</small>` : ""}</b>${rankCell(row.rank)}${player?.dead ? `<small class="live-dead">${esc(t("live.respawn", { seconds: Math.ceil(player.respawn) }))}</small>` : ""}</span>
            <span class="live-kda">${player ? `<b>${player.kills} / <i>${player.deaths}</i> / ${player.assists}</b><small class="${ratio >= 3 ? "is-good" : ratio < 1.5 ? "is-bad" : ""}">${esc(ratio.toFixed(1))}</small>` : "—"}</span>
            <span>${player ? esc(percent(kp, 0)) : "—"}</span>
            <span>${player ? `<b>${player.cs}</b><small>${esc((player.cs / minutes).toFixed(1))}/m</small>` : "—"}</span>
            <span>${player ? esc(String(Math.round(player.vision))) : "—"}</span>
            <span class="live-items">${(player?.items ?? []).slice(0, 7).map((id) => itemImage(id, 24)).join("")}</span>
            <span class="live-gold">${player ? esc(count(itemGold(row))) : ""}</span>
          </div>`;
        }).join("")}
      </div>
    </section>`;
}

/** Both teams with rank, composition and live numbers. */
function liveHtml(): string {
  const game = liveGame!;
  const stats = liveStats;
  const rows = liveRows(game, stats);
  const me = rows.find((row) => row.rank?.isMe);
  const myTeam = me?.team ?? 1;
  const teams = [...new Set(rows.map((row) => row.team))].sort((left, right) => (left === myTeam ? -1 : right === myTeam ? 1 : left - right));
  return `
    <div class="live-bar">
      <h3>${esc(t("live.title"))}</h3>
      ${stats ? `<span class="live-clock">${esc(clock(stats.gameTime))}</span>` : `<span class="muted">${esc(t("live.waiting"))}</span>`}
      <span class="build-source">${esc(t("live.note"))}</span>
    </div>
    <div class="live-teams">${teams.map((team) => teamHtml(rows, team, team === myTeam, stats)).join("")}</div>`;
}
