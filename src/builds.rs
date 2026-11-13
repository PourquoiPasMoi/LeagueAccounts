//! Champion builds for champ select: runes, summoner spells, items and skill
//! order from OP.GG's champion statistics, and putting the chosen ones into
//! the League client.
//!
//! Everything here goes through the League client's own local API, the same
//! calls its interface makes and the ones Riot allows companion apps (OP.GG,
//! Blitz, Porofessor) to use: create or edit a rune page, pick summoner spells
//! for the local player, save an item set. Nothing picks, locks, bans or
//! dodges, nothing reads or touches the game process, and nothing reveals
//! information the client hides.

use crate::logging::{self, Event, Reason};
use crate::riot_client::LeagueApi;
use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

const OPGG_API: &str = "https://lol-api-champion.op.gg/api/global/champions";
const GAME_DATA: &str = "https://raw.communitydragon.org/latest/plugins/rcp-be-lol-game-data/global/default/v1/";
/// Names of rune pages and item sets this app creates. Only pages with this
/// prefix are ever edited without asking.
pub const PREFIX: &str = "LC:";
/// A build is fetched again after this long (OP.GG updates a few times a day).
const BUILD_TTL: Duration = Duration::from_secs(30 * 60);
/// Cached game data (rune, item and spell names) is refreshed after this long.
const GAME_DATA_TTL: Duration = Duration::from_secs(3 * 24 * 3600);

static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()
        .ok()
});

/// Ranked (Summoner's Rift, any queue) or ARAM statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Ranked,
    Aram,
}

impl Mode {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("aram") { Self::Aram } else { Self::Ranked }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Ranked => "ranked",
            Self::Aram => "aram",
        }
    }
}

/// OP.GG's lane names; the client's (`middle`, `bottom`, `utility`) map to them.
pub fn opgg_position(value: &str) -> &'static str {
    match value.to_ascii_lowercase().as_str() {
        "top" => "top",
        "jungle" => "jungle",
        "mid" | "middle" => "mid",
        "adc" | "bottom" | "bot" => "adc",
        "support" | "utility" => "support",
        _ => "",
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rates {
    pub games: u64,
    pub win_rate: f64,
    pub pick_rate: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionShare {
    pub position: String,
    pub role_rate: f64,
    pub win_rate: f64,
    pub games: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunePage {
    pub primary_style: i64,
    pub sub_style: i64,
    /// Keystone, three primary runes, two secondary runes, three shards.
    pub perks: Vec<i64>,
    #[serde(flatten)]
    pub rates: Rates,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdSet {
    pub ids: Vec<i64>,
    #[serde(flatten)]
    pub rates: Rates,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillOrder {
    /// Skill per level, `Q`/`W`/`E`/`R`.
    pub order: Vec<String>,
    /// Max order, e.g. `Q E W`.
    pub priority: Vec<String>,
    #[serde(flatten)]
    pub rates: Rates,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Matchup {
    pub champion_id: i64,
    pub games: u64,
    /// This champion's win rate in the matchup.
    pub win_rate: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChampionBuild {
    pub champion_id: i64,
    pub mode: Mode,
    /// OP.GG lane (`top`, `jungle`, `mid`, `adc`, `support`), empty for ARAM.
    pub position: String,
    pub positions: Vec<PositionShare>,
    pub tier: Option<i64>,
    pub rank: Option<i64>,
    pub win_rate: f64,
    pub pick_rate: f64,
    pub ban_rate: f64,
    pub games: u64,
    pub rune_pages: Vec<RunePage>,
    pub spells: Vec<IdSet>,
    pub starter_items: Vec<IdSet>,
    pub core_items: Vec<IdSet>,
    pub boots: Vec<IdSet>,
    /// Single items most often finished, minus the core and boots.
    pub situational: Vec<IdSet>,
    pub skills: Option<SkillOrder>,
    pub weak_against: Vec<Matchup>,
    pub strong_against: Vec<Matchup>,
}

fn rates(entry: &Value) -> Rates {
    let games = entry["play"].as_u64().unwrap_or(0);
    let wins = entry["win"].as_u64().unwrap_or(0);
    Rates {
        games,
        win_rate: if games > 0 { wins as f64 / games as f64 } else { 0.0 },
        pick_rate: entry["pick_rate"].as_f64().unwrap_or(0.0),
    }
}

fn ids(entry: &Value) -> Vec<i64> {
    entry.as_array().into_iter().flatten().filter_map(Value::as_i64).collect()
}

fn id_sets(list: &Value, limit: usize) -> Vec<IdSet> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|entry| !ids(&entry["ids"]).is_empty())
        .take(limit)
        .map(|entry| IdSet { ids: ids(&entry["ids"]), rates: rates(entry) })
        .collect()
}

/// OP.GG's champion statistics as a build.
pub fn parse_build(champion_id: i64, mode: Mode, position: &str, data: &Value) -> Option<ChampionBuild> {
    let summary = &data["summary"];
    let average = &summary["average_stats"];
    let positions: Vec<PositionShare> = summary["positions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| PositionShare {
            position: opgg_position(entry["name"].as_str().unwrap_or_default()).to_owned(),
            role_rate: entry["stats"]["role_rate"].as_f64().unwrap_or(0.0),
            win_rate: entry["stats"]["win_rate"].as_f64().unwrap_or(0.0),
            games: entry["stats"]["play"].as_u64().unwrap_or(0),
        })
        .filter(|entry| !entry.position.is_empty())
        .collect();
    // Stats for the lane itself when OP.GG lists it, else the champion's.
    let lane = summary["positions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| opgg_position(entry["name"].as_str().unwrap_or_default()) == position)
        .map(|entry| &entry["stats"]);
    let stats = lane.unwrap_or(average);
    let tier = &stats["tier_data"];

    let rune_pages: Vec<RunePage> = data["rune_pages"]
        .as_array()
        .into_iter()
        .flatten()
        // The most played page of each rune-tree combination, with that
        // combination's own pick rate; then single pages as fallback.
        .filter_map(|group| Some((group["builds"].as_array()?.first()?, group)))
        .chain(data["runes"].as_array().into_iter().flatten().map(|build| (build, build)))
        .filter_map(|(build, group)| {
            let mut perks = ids(&build["primary_rune_ids"]);
            perks.extend(ids(&build["secondary_rune_ids"]));
            perks.extend(ids(&build["stat_mod_ids"]));
            (perks.len() == 9).then(|| RunePage {
                primary_style: build["primary_page_id"].as_i64().unwrap_or(0),
                sub_style: build["secondary_page_id"].as_i64().unwrap_or(0),
                perks,
                rates: Rates { pick_rate: group["pick_rate"].as_f64().unwrap_or(0.0), ..rates(build) },
            })
        })
        .fold(Vec::<RunePage>::new(), |mut pages, page| {
            if pages.len() < 4 && !pages.iter().any(|known| known.perks == page.perks) {
                pages.push(page);
            }
            pages
        });
    if rune_pages.is_empty() {
        return None;
    }

    let core_items = id_sets(&data["core_items"], 3);
    let boots = id_sets(&data["boots"], 2);
    let taken: Vec<i64> = core_items.iter().take(1).chain(boots.iter().take(1)).flat_map(|set| set.ids.clone()).collect();
    let situational: Vec<IdSet> = id_sets(&data["last_items"], 20)
        .into_iter()
        .filter(|set| set.ids.iter().all(|id| !taken.contains(id)))
        .take(8)
        .collect();

    let skills = data["skill_masteries"].as_array().and_then(|list| list.first()).map(|mastery| {
        let build = mastery["builds"].as_array().and_then(|builds| builds.first());
        let order = build
            .map(|build| build["order"].as_array().into_iter().flatten().filter_map(|skill| skill.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        SkillOrder {
            order,
            priority: mastery["ids"].as_array().into_iter().flatten().filter_map(|skill| skill.as_str().map(str::to_owned)).collect(),
            rates: build.map(rates).unwrap_or_else(|| rates(mastery)),
        }
    });

    // Matchups with enough games to mean something.
    let mut matchups: Vec<Matchup> = data["counters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let games = entry["play"].as_u64()?;
            Some(Matchup {
                champion_id: entry["champion_id"].as_i64()?,
                games,
                win_rate: entry["win"].as_u64().unwrap_or(0) as f64 / games.max(1) as f64,
            })
        })
        .collect();
    let most = matchups.iter().map(|matchup| matchup.games).max().unwrap_or(0);
    matchups.retain(|matchup| matchup.games >= (most / 40).max(30));
    matchups.sort_by(|left, right| left.win_rate.total_cmp(&right.win_rate));
    let weak_against = matchups.iter().take(5).cloned().collect();
    let strong_against = matchups.iter().rev().take(5).cloned().collect();

    Some(ChampionBuild {
        champion_id,
        mode,
        position: position.to_owned(),
        positions,
        tier: tier["tier"].as_i64().or_else(|| average["tier"].as_i64()),
        rank: tier["rank"].as_i64().or_else(|| average["rank"].as_i64()),
        win_rate: stats["win_rate"].as_f64().unwrap_or(0.0),
        pick_rate: stats["pick_rate"].as_f64().unwrap_or(0.0),
        ban_rate: stats["ban_rate"].as_f64().unwrap_or(0.0),
        games: stats["play"].as_u64().unwrap_or(0),
        rune_pages,
        spells: id_sets(&data["summoner_spells"], 3),
        starter_items: id_sets(&data["starter_items"], 2),
        core_items,
        boots,
        situational,
        skills,
        weak_against,
        strong_against,
    })
}

fn get_json(url: &str) -> Result<Value, Reason> {
    let client = CLIENT.as_ref().ok_or(Reason::Request)?;
    let response = client.get(url).send().map_err(|error| Reason::from_request(&error))?;
    let response = response.error_for_status().map_err(|error| Reason::from_request(&error))?;
    let text = response.text().map_err(|_| Reason::ResponseBody)?;
    serde_json::from_str(&text).map_err(|_| Reason::ResponseBody)
}

type BuildKey = (i64, Mode, String);
static BUILDS: LazyLock<Mutex<HashMap<BuildKey, (Instant, ChampionBuild)>>> = LazyLock::new(Default::default);
/// Lanes per champion, most played first, and when they were read.
type Lanes = (Instant, HashMap<i64, Vec<String>>);
static MAIN_POSITIONS: LazyLock<Mutex<Option<Lanes>>> = LazyLock::new(Default::default);

/// Each champion's lanes, most played first, from OP.GG's tier list (cached for an hour).
fn main_positions(champion_id: i64) -> Vec<String> {
    if let Ok(cache) = MAIN_POSITIONS.lock() {
        if let Some((at, map)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(3600) {
                return map.get(&champion_id).cloned().unwrap_or_default();
            }
        }
    }
    let Ok(list) = get_json(&format!("{OPGG_API}/ranked")) else { return Vec::new() };
    let map: HashMap<i64, Vec<String>> = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|champion| {
            let mut lanes: Vec<(f64, String)> = champion["positions"]
                .as_array()?
                .iter()
                .map(|entry| (entry["stats"]["role_rate"].as_f64().unwrap_or(0.0), opgg_position(entry["name"].as_str().unwrap_or_default()).to_owned()))
                .filter(|(_, lane)| !lane.is_empty())
                .collect();
            lanes.sort_by(|left, right| right.0.total_cmp(&left.0));
            Some((champion["id"].as_i64()?, lanes.into_iter().map(|(_, lane)| lane).collect()))
        })
        .collect();
    let lanes = map.get(&champion_id).cloned().unwrap_or_default();
    if let Ok(mut cache) = MAIN_POSITIONS.lock() {
        *cache = Some((Instant::now(), map));
    }
    lanes
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub champion_id: i64,
    pub tier: i64,
    pub rank: i64,
    pub win_rate: f64,
    pub pick_rate: f64,
}

static TIER_LIST: LazyLock<Mutex<Option<(Instant, Value)>>> = LazyLock::new(Default::default);

/// The strongest champions in a lane right now, from OP.GG's tier list.
pub fn lane_suggestions(position: &str) -> Result<Vec<Suggestion>, Reason> {
    let lane = opgg_position(position);
    let cached = TIER_LIST.lock().ok().and_then(|cache| cache.as_ref().filter(|(at, _)| at.elapsed() < Duration::from_secs(3600)).map(|(_, list)| list.clone()));
    let list = match cached {
        Some(list) => list,
        None => {
            let list = get_json(&format!("{OPGG_API}/ranked"))?;
            if let Ok(mut cache) = TIER_LIST.lock() {
                *cache = Some((Instant::now(), list.clone()));
            }
            list
        }
    };
    let mut picks: Vec<Suggestion> = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|champion| {
            let entry = champion["positions"].as_array()?.iter().find(|entry| lane.is_empty() || opgg_position(entry["name"].as_str().unwrap_or_default()) == lane)?;
            let stats = &entry["stats"];
            Some(Suggestion {
                champion_id: champion["id"].as_i64()?,
                tier: stats["tier_data"]["tier"].as_i64().unwrap_or(5),
                rank: stats["tier_data"]["rank"].as_i64().unwrap_or(999),
                win_rate: stats["win_rate"].as_f64().unwrap_or(0.0),
                pick_rate: stats["pick_rate"].as_f64().unwrap_or(0.0),
            })
        })
        .collect();
    picks.sort_by_key(|pick| pick.rank);
    picks.truncate(12);
    Ok(picks)
}

/// The build for a champion in a lane (or its main lane when `position` is empty).
pub fn champion_build(champion_id: i64, mode: Mode, position: &str) -> Result<ChampionBuild, Reason> {
    let position = match mode {
        Mode::Aram => "none".to_owned(),
        Mode::Ranked => match opgg_position(position) {
            "" => main_positions(champion_id).into_iter().next().unwrap_or_else(|| "mid".to_owned()),
            lane => lane.to_owned(),
        },
    };
    let key = (champion_id, mode, position.clone());
    if let Ok(cache) = BUILDS.lock() {
        if let Some((at, build)) = cache.get(&key) {
            if at.elapsed() < BUILD_TTL {
                return Ok(build.clone());
            }
        }
    }
    let result = get_json(&format!("{OPGG_API}/{}/{champion_id}/{position}", mode.path()))
        .and_then(|body| parse_build(champion_id, mode, if mode == Mode::Aram { "" } else { &position }, &body["data"]).ok_or(Reason::ResponseBody));
    match result {
        Ok(build) => {
            if let Ok(mut cache) = BUILDS.lock() {
                cache.insert(key, (Instant::now(), build.clone()));
            }
            Ok(build)
        }
        Err(reason) => {
            logging::record(Event::BuildFetchFailed, reason);
            Err(reason)
        }
    }
}

// ---------------------------------------------------------------- one build to follow

/// The enemy team as champ select shows it: how many deal mostly magic or
/// physical damage, are tanks, or heal a lot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Threats {
    pub magic: u32,
    pub physical: u32,
    pub tanks: u32,
    pub healers: u32,
}

/// A single build to follow: one start, six items in buying order, and swaps.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalBuild {
    pub starter: Vec<i64>,
    /// Six items in the order to buy them (boots second, like most players).
    pub order: Vec<i64>,
    /// Good replacements, best first.
    pub swaps: Vec<i64>,
    /// Items picked because of the enemy team.
    pub adapted: Vec<Adaptation>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Adaptation {
    pub item: i64,
    /// `magicResist`, `armor` or `antiHeal`.
    pub reason: String,
}

type ItemTest = fn(&ItemInfo) -> bool;

/// Turn OP.GG's lists into one build: the most played start and core, the
/// most played boots (or defensive boots against a mostly magic or physical
/// team), then finished items by popularity, with resistances and anti-heal
/// moved up when the enemy team calls for them. Only items this champion's
/// players actually build are used.
pub fn final_build(build: &ChampionBuild, items: &HashMap<i64, ItemInfo>, threats: Threats) -> FinalBuild {
    let finished = |id: &i64| items.get(id).is_none_or(|info| info.finished && info.price >= 2000 && !info.has("Boots") && !info.has("Consumable"));
    let enemies = f64::from((threats.magic + threats.physical).max(1));
    let magic_heavy = threats.magic >= 3 && f64::from(threats.magic) / enemies >= 0.6;
    let physical_heavy = threats.physical >= 3 && f64::from(threats.physical) / enemies >= 0.6;

    let starter = build.starter_items.first().map(|set| set.ids.clone()).unwrap_or_default();
    let core: Vec<i64> = build.core_items.first().map(|set| set.ids.iter().copied().filter(finished).collect()).unwrap_or_default();
    let mut adapted = Vec::new();

    // Boots: the most played, or the defensive pair this champion's players also buy.
    let boot_options: Vec<i64> = build.boots.iter().filter_map(|set| set.ids.first().copied()).collect();
    let defensive_boots = |category: &str| boot_options.iter().copied().find(|id| items.get(id).is_some_and(|info| info.has(category)));
    let mut boots = boot_options.first().copied();
    let wanted_boots = if magic_heavy { Some(("SpellBlock", "magicResist")) } else if physical_heavy { Some(("Armor", "armor")) } else { None };
    if let Some((category, reason)) = wanted_boots {
        if let Some(id) = defensive_boots(category) {
            boots = Some(id);
            adapted.push(Adaptation { item: id, reason: reason.to_owned() });
        }
    }

    // Candidates after the core: the most finished items, then other cores' items.
    let mut candidates: Vec<(i64, f64)> = Vec::new();
    let mut push = |id: i64, score: f64| {
        if !candidates.iter().any(|(known, _)| *known == id) {
            candidates.push((id, score));
        }
    };
    for set in &build.situational {
        if let Some(id) = set.ids.first() {
            push(*id, set.rates.pick_rate);
        }
    }
    for set in build.core_items.iter().skip(1) {
        for id in &set.ids {
            push(*id, set.rates.pick_rate * 0.5);
        }
    }
    let mut candidates: Vec<(i64, f64)> = candidates.into_iter().filter(|(id, _)| finished(id) && !core.contains(id)).collect();
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));

    let mut order: Vec<i64> = Vec::new();
    if let Some(first) = core.first() {
        order.push(*first);
    }
    if let Some(boots) = boots {
        order.push(boots);
    }
    order.extend(core.iter().skip(1));

    // One item for each need the enemy team creates.
    let mut needs: Vec<(&str, ItemTest)> = Vec::new();
    if threats.healers >= 2 {
        needs.push(("antiHeal", |info| info.anti_heal));
    }
    if magic_heavy {
        needs.push(("magicResist", |info| info.has("SpellBlock")));
    }
    if physical_heavy {
        needs.push(("armor", |info| info.has("Armor")));
    }
    let mut extra: Vec<i64> = Vec::new();
    for (reason, test) in needs {
        // Boots alone do not answer a whole team of one damage type.
        let covered = order.iter().chain(&extra).any(|id| items.get(id).is_some_and(|info| !info.has("Boots") && test(info)));
        if covered {
            continue;
        }
        let found = candidates.iter().map(|(id, _)| *id).find(|id| !extra.contains(id) && items.get(id).is_some_and(test));
        // No anti-heal among this champion's usual items: the standard one for its damage.
        let fallback = || {
            let core_has = |category: &str| core.iter().filter(|id| items.get(id).is_some_and(|info| info.has(category))).count() >= 2;
            let id = if core_has("SpellDamage") { 3165 } else if core_has("CriticalStrike") { 3033 } else { 6609 };
            (reason == "antiHeal" && items.contains_key(&id) && !order.contains(&id)).then_some(id)
        };
        if let Some(id) = found.or_else(fallback) {
            extra.push(id);
            adapted.push(Adaptation { item: id, reason: reason.to_owned() });
        }
    }
    // Adapted items right after the core, then the most built items.
    for id in extra.iter().chain(candidates.iter().map(|(id, _)| id)) {
        if order.len() >= 6 {
            break;
        }
        if !order.contains(id) {
            order.push(*id);
        }
    }
    let swaps = candidates.iter().map(|(id, _)| *id).filter(|id| !order.contains(id)).take(5).collect();
    FinalBuild { starter, order, swaps, adapted }
}

// ---------------------------------------------------------------- game data (names and icons)

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub name: String,
    /// Image path for the `lacache` scheme (lower-case game-data path).
    pub icon: String,
    /// Total gold cost (items only; 0 otherwise and then left out).
    #[serde(skip_serializing_if = "is_zero")]
    pub price: i64,
    /// Runes: what the end-of-game screen shows ("Total Damage Dealt: @eogvar1@").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stats: Vec<String>,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuneStyle {
    pub name: String,
    pub icon: String,
    /// Rune rows: keystones, three rows, then the shard rows.
    pub slots: Vec<Vec<i64>>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameAssets {
    pub perks: HashMap<i64, Asset>,
    pub styles: HashMap<i64, RuneStyle>,
    pub items: HashMap<i64, Asset>,
    pub spells: HashMap<i64, Asset>,
    /// What each item gives, for choosing items against the enemy team.
    #[serde(skip)]
    pub item_info: HashMap<i64, ItemInfo>,
}

#[derive(Clone, Debug, Default)]
pub struct ItemInfo {
    pub categories: Vec<String>,
    pub price: i64,
    /// Builds into nothing: a finished item.
    pub finished: bool,
    /// Applies Grievous Wounds.
    pub anti_heal: bool,
}

impl ItemInfo {
    fn has(&self, category: &str) -> bool {
        self.categories.iter().any(|known| known == category)
    }
}

/// Items known to apply Grievous Wounds, if a description does not say so.
const ANTI_HEAL: [i64; 7] = [3033, 3075, 3165, 6609, 3011, 3123, 3916];

/// `/lol-game-data/assets/v1/perk-images/Styles/X.png` → `v1/perk-images/styles/x.png`.
fn icon_path(path: &str) -> String {
    let path = path.trim();
    let path = path.strip_prefix("/lol-game-data/assets/").unwrap_or(path.trim_start_matches('/'));
    path.to_ascii_lowercase()
}

fn assets_from(perks: &Value, styles: &Value, items: &Value, spells: &Value) -> GameAssets {
    let asset = |entry: &Value| Asset {
        name: entry["name"].as_str().unwrap_or_default().to_owned(),
        icon: icon_path(entry["iconPath"].as_str().unwrap_or_default()),
        price: entry["priceTotal"].as_i64().unwrap_or(0),
        stats: entry["endOfGameStatDescs"].as_array().into_iter().flatten().filter_map(|text| text.as_str().map(str::to_owned)).collect(),
    };
    let by_id = |list: &Value| -> HashMap<i64, Asset> {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| Some((entry["id"].as_i64()?, asset(entry))))
            .collect()
    };
    let item_info = items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry["id"].as_i64()?;
            Some((
                id,
                ItemInfo {
                    categories: entry["categories"].as_array().into_iter().flatten().filter_map(|value| value.as_str().map(str::to_owned)).collect(),
                    price: entry["priceTotal"].as_i64().unwrap_or(0),
                    finished: entry["to"].as_array().is_none_or(Vec::is_empty),
                    anti_heal: ANTI_HEAL.contains(&id) || entry["description"].as_str().is_some_and(|text| text.contains("Grievous")),
                },
            ))
        })
        .collect();
    GameAssets {
        item_info,
        perks: by_id(perks),
        items: by_id(items),
        spells: by_id(spells),
        styles: styles["styles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|style| {
                Some((
                    style["id"].as_i64()?,
                    RuneStyle {
                        name: style["name"].as_str().unwrap_or_default().to_owned(),
                        icon: icon_path(style["iconPath"].as_str().unwrap_or_default()),
                        slots: style["slots"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|slot| ids(&slot["perks"]))
                            .collect(),
                    },
                ))
            })
            .collect(),
    }
}

static ASSETS: LazyLock<Mutex<Option<GameAssets>>> = LazyLock::new(Default::default);
const GAME_DATA_FILES: [&str; 4] = ["perks.json", "perkstyles.json", "items.json", "summoner-spells.json"];

/// One game-data file: from the running client (free), else the disk cache,
/// else CommunityDragon (downloaded once, then cached).
fn game_data_file(name: &str, client: Option<&LeagueApi>) -> Value {
    let directory = app_data_dir().ok().map(|directory| directory.join("cache").join("gamedata"));
    let cached = directory.as_ref().map(|directory| directory.join(name));
    let fresh = cached
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < GAME_DATA_TTL);
    let read_cache = || cached.as_ref().and_then(|path| serde_json::from_slice::<Value>(&std::fs::read(path).ok()?).ok());
    if fresh {
        if let Some(value) = read_cache() {
            return value;
        }
    }
    let downloaded = client
        .and_then(|client| client.get_json(&format!("/lol-game-data/assets/v1/{name}")))
        .or_else(|| get_json(&format!("{GAME_DATA}{name}")).ok());
    match downloaded {
        Some(value) => {
            if let (Some(directory), Some(path)) = (directory, cached) {
                let _ = std::fs::create_dir_all(&directory);
                let _ = std::fs::write(path, value.to_string());
            }
            value
        }
        None => read_cache().unwrap_or_default(),
    }
}

/// Names and icons of runes, rune trees, items and summoner spells.
pub fn game_assets(client: Option<&LeagueApi>) -> GameAssets {
    if let Ok(cache) = ASSETS.lock() {
        if let Some(assets) = cache.as_ref() {
            return assets.clone();
        }
    }
    let [perks, styles, items, spells] = GAME_DATA_FILES.map(|name| game_data_file(name, client));
    let assets = assets_from(&perks, &styles, &items, &spells);
    if !assets.perks.is_empty() && !assets.items.is_empty() {
        if let Ok(mut cache) = ASSETS.lock() {
            *cache = Some(assets.clone());
        }
    }
    assets
}

// ---------------------------------------------------------------- putting a build into the client

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuneOutcome {
    /// A new page was made (or this app's page was updated).
    Saved,
    /// Every page slot was taken, so the selected editable page was replaced.
    ReplacedCurrent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The League client is not open or not signed in.
    NoClient,
    /// No free page and the selected page cannot be edited.
    RunePagesFull,
    /// Summoner spells can only be picked in champ select.
    NotInChampSelect,
    /// The client refused the change.
    Rejected,
}

impl ApplyError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoClient => "build_no_client",
            Self::RunePagesFull => "build_rune_pages_full",
            Self::NotInChampSelect => "build_not_in_champ_select",
            Self::Rejected => "build_rejected",
        }
    }
}

fn ok(status: u16) -> bool {
    (200..300).contains(&status)
}

fn failed(error: ApplyError) -> ApplyError {
    logging::record(Event::BuildApplyFailed, Reason::Other);
    error
}

/// Save `page` as this app's rune page and select it.
pub fn apply_runes(client: &LeagueApi, name: &str, page: &RunePage) -> Result<RuneOutcome, ApplyError> {
    let body = json!({
        "name": name,
        "primaryStyleId": page.primary_style,
        "subStyleId": page.sub_style,
        "selectedPerkIds": page.perks,
        "current": true,
    });
    let pages = client.get_json("/lol-perks/v1/pages").ok_or_else(|| failed(ApplyError::NoClient))?;
    let ours = pages.as_array().into_iter().flatten().find(|existing| {
        existing["isDeletable"].as_bool() == Some(true)
            && existing["name"].as_str().is_some_and(|existing| existing.starts_with(PREFIX))
    });
    // Our own page: replace it (delete + create keeps the client's page order valid).
    if let Some(id) = ours.and_then(|existing| existing["id"].as_i64()) {
        let _ = client.send("DELETE", &format!("/lol-perks/v1/pages/{id}"), None);
    }
    match client.send("POST", "/lol-perks/v1/pages", Some(&body)) {
        Some((status, _)) if ok(status) => {
            logging::record(Event::BuildApplied, Reason::Other);
            return Ok(RuneOutcome::Saved);
        }
        None => return Err(failed(ApplyError::NoClient)),
        _ => {}
    }
    // Every slot is used: replace the selected page if the player may edit it.
    let current = client.get_json("/lol-perks/v1/currentpage").ok_or_else(|| failed(ApplyError::RunePagesFull))?;
    let editable = current["isEditable"].as_bool() == Some(true) && current["isDeletable"].as_bool() == Some(true);
    let id = current["id"].as_i64().filter(|_| editable).ok_or_else(|| failed(ApplyError::RunePagesFull))?;
    match client.send("PUT", &format!("/lol-perks/v1/pages/{id}"), Some(&body)) {
        Some((status, _)) if ok(status) => {
            logging::record(Event::BuildApplied, Reason::Other);
            Ok(RuneOutcome::ReplacedCurrent)
        }
        _ => Err(failed(ApplyError::Rejected)),
    }
}

/// Pick summoner spells for the local player. Flash stays on the key the
/// player already has it on.
pub fn apply_spells(client: &LeagueApi, spells: &[i64]) -> Result<(), ApplyError> {
    let [first, second] = spells[..] else { return Err(failed(ApplyError::Rejected)) };
    let session = client.get_json("/lol-champ-select/v1/session").ok_or_else(|| failed(ApplyError::NotInChampSelect))?;
    let cell = session["localPlayerCellId"].as_i64();
    let me = session["myTeam"].as_array().into_iter().flatten().find(|player| player["cellId"].as_i64() == cell);
    const FLASH: i64 = 4;
    let flash_on_f = me.is_some_and(|player| player["spell2Id"].as_i64() == Some(FLASH));
    let flash_on_d = me.is_some_and(|player| player["spell1Id"].as_i64() == Some(FLASH));
    let (spell1, spell2) = match (first == FLASH, second == FLASH) {
        (true, _) if flash_on_f => (second, first),
        (_, true) if flash_on_d => (second, first),
        _ => (first, second),
    };
    match client.send("PATCH", "/lol-champ-select/v1/session/my-selection", Some(&json!({"spell1Id": spell1, "spell2Id": spell2}))) {
        Some((status, _)) if ok(status) => Ok(()),
        Some(_) => Err(failed(ApplyError::NotInChampSelect)),
        None => Err(failed(ApplyError::NoClient)),
    }
}

/// Item ids with repeats folded into counts (two potions → one entry ×2).
fn block(title: String, ids: &[i64]) -> Value {
    let mut items: Vec<(i64, u32)> = Vec::new();
    for id in ids {
        match items.last_mut() {
            Some((last, count)) if last == id => *count += 1,
            _ => items.push((*id, 1)),
        }
    }
    json!({
        "type": title,
        "showIfSummonerSpell": "",
        "hideIfSummonerSpell": "",
        "items": items.iter().map(|(id, count)| json!({"id": id.to_string(), "count": count})).collect::<Vec<_>>(),
    })
}

/// Titles of an item set's blocks, translated by the caller.
pub struct ItemSetText {
    /// "Start"
    pub starter: String,
    /// "Build order"
    pub core: String,
    /// "Swaps"
    pub situational: String,
    pub consumables: String,
}

/// The item-set file this app writes in `Config/Global/Recommended`. The shop
/// lists that folder before the champion's own (and before the player's
/// account sets), file names in order, and opens on the first one; other apps
/// use `!!!` (`!!!blitz-0.json`), so this name starts with `!!!!`. The file is
/// tagged with the champion and rewritten for each game.
const ITEM_SET_FILE: &str = "!!!!LeagueCompanion";
/// The file name earlier builds of this app used.
const OLD_ITEM_SET_FILE: &str = "0_LeagueCompanion";

fn is_our_file(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(ITEM_SET_FILE) || name.starts_with(OLD_ITEM_SET_FILE))
}

/// Delete other build apps' item-set files in a `Recommended` folder.
fn remove_other_files(directory: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    for path in entries.flatten().map(|entry| entry.path()).filter(|path| !is_our_file(path)) {
        let title = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|set| set["title"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_lowercase();
        if from_other_app(&title) || OTHER_APPS.iter().any(|app| name.trim_start_matches('!').starts_with(app)) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The champion's folder name under `Config/Champions` (`MonkeyKing` for Wukong).
fn champion_alias(client: &LeagueApi, champion_id: i64) -> Option<String> {
    client
        .get_json("/lol-game-data/assets/v1/champion-summary.json")?
        .as_array()?
        .iter()
        .find(|champion| champion["id"].as_i64() == Some(champion_id))?["alias"]
        .as_str()
        .filter(|alias| !alias.is_empty() && alias.chars().all(|letter| letter.is_ascii_alphanumeric()))
        .map(str::to_owned)
}

/// Title prefixes of item sets other build apps save. Their sets are listed
/// before this app's in the shop, so they are removed for champions this app
/// has a set for (the player asked for this app's build to be the default).
const OTHER_APPS: [&str; 13] = [
    "blitz", "porofessor", "op.gg", "opgg", "u.gg", "ugg", "mobalytics", "lolalytics", "itero", "champion.gg", "probuilds", "metasrc", "deeplol",
];

fn from_other_app(title: &str) -> bool {
    let title = title.trim().to_lowercase();
    OTHER_APPS.iter().any(|prefix| title.starts_with(prefix))
}

/// Remove other build apps' item sets, so this app's set is the one the shop
/// opens on: their files in the champion's `Recommended` folder, and their
/// sets saved to the account (plus this app's old account sets, `lc-*`).
/// Does nothing unless this app has a set for the champion. The player's own
/// item sets are never touched.
pub fn remove_other_item_sets(client: &LeagueApi, champion_id: i64) {
    let Some(config) = crate::game_settings::config_dir() else { return };
    let global = config.join("Global").join("Recommended");
    let ours = global.join(format!("{ITEM_SET_FILE}.json"));
    let tagged = std::fs::read(&ours)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|set| set["associatedChampions"].as_array()?.first()?.as_i64());
    match tagged {
        // Written for another champion (no build applied this game): it must not show.
        Some(other) if other != champion_id => {
            let _ = std::fs::remove_file(&ours);
            return;
        }
        Some(_) => {}
        None => return,
    }
    remove_other_files(&global);
    if let Some(alias) = champion_alias(client, champion_id) {
        remove_other_files(&config.join("Champions").join(alias).join("Recommended"));
    }
    let Some(summoner_id) = client.get_json("/lol-summoner/v1/current-summoner").and_then(|summoner| summoner["summonerId"].as_i64()) else { return };
    let path = format!("/lol-item-sets/v1/item-sets/{summoner_id}/sets");
    let Some(mut sets) = client.get_json(&path) else { return };
    let list = sets["itemSets"].as_array().cloned().unwrap_or_default();
    let kept: Vec<Value> = list
        .iter()
        .filter(|set| {
            let ours = set["uid"].as_str().is_some_and(|uid| uid.starts_with("lc-"));
            !ours && !from_other_app(set["title"].as_str().unwrap_or_default())
        })
        .cloned()
        .collect();
    if kept.len() != list.len() {
        sets["itemSets"] = Value::Array(kept);
        let _ = client.send("PUT", &path, Some(&sets));
    }
}

/// Save the build as an item set file at the top of
/// `Config/Global/Recommended`, tagged with the champion and map, so the shop
/// lists it first and opens on it. Applying again replaces it.
pub fn apply_item_set(client: &LeagueApi, title: &str, build: &ChampionBuild, items: &HashMap<i64, ItemInfo>, threats: Threats, text: &ItemSetText) -> Result<(), ApplyError> {
    let config = crate::game_settings::config_dir().ok_or_else(|| failed(ApplyError::NoClient))?;
    let alias = champion_alias(client, build.champion_id).ok_or_else(|| failed(ApplyError::NoClient))?;
    let plan = final_build(build, items, threats);
    let mut blocks = vec![block(text.starter.clone(), &plan.starter), block(text.core.clone(), &plan.order)];
    if !plan.swaps.is_empty() {
        blocks.push(block(text.situational.clone(), &plan.swaps));
    }
    // Health potion, control ward.
    blocks.push(block(text.consumables.clone(), &[2003, 2055]));
    let map = match build.mode {
        Mode::Ranked => "SR",
        Mode::Aram => "HA",
    };
    let set = json!({
        "title": title,
        "type": "custom",
        "map": map,
        "mode": "any",
        "priority": true,
        "sortrank": 0,
        "champion": alias,
        "associatedChampions": [build.champion_id],
        "associatedMaps": [if build.mode == Mode::Aram { 12 } else { 11 }],
        "blocks": blocks,
    });
    // Earlier builds of this app wrote into the champion's folder.
    if let Ok(champions) = std::fs::read_dir(config.join("Champions")) {
        for folder in champions.flatten() {
            if let Ok(files) = std::fs::read_dir(folder.path().join("Recommended")) {
                for file in files.flatten().filter(|file| is_our_file(&file.path())) {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
    let directory = config.join("Global").join("Recommended");
    std::fs::create_dir_all(&directory).map_err(|_| failed(ApplyError::Rejected))?;
    let file = directory.join(format!("{ITEM_SET_FILE}.json"));
    let body = serde_json::to_vec_pretty(&set).map_err(|_| failed(ApplyError::Rejected))?;
    std::fs::write(&file, body).map_err(|_| failed(ApplyError::Rejected))?;
    remove_other_item_sets(client, build.champion_id);
    logging::record(Event::BuildApplied, Reason::Other);
    Ok(())
}

/// What champ select currently shows for the local player.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChampSelect {
    pub active: bool,
    /// Locked or hovered champion, 0 when none yet.
    pub champion_id: i64,
    /// The champion is locked in (not just hovered).
    pub locked: bool,
    /// OP.GG lane for the assigned position, empty when none was assigned.
    pub position: String,
    pub mode: Option<Mode>,
    pub queue_id: i64,
    /// Enemy champions shown so far (picked or revealed), in pick order.
    pub enemies: Vec<i64>,
}

/// Read champ select from the client (`active: false` outside it).
pub fn champ_select(client: &LeagueApi) -> ChampSelect {
    let phase = client.get_json("/lol-gameflow/v1/gameflow-phase");
    if phase.as_ref().and_then(Value::as_str) != Some("ChampSelect") {
        return ChampSelect::default();
    }
    let Some(session) = client.get_json("/lol-champ-select/v1/session") else { return ChampSelect::default() };
    let cell = session["localPlayerCellId"].as_i64();
    let me = session["myTeam"].as_array().into_iter().flatten().find(|player| player["cellId"].as_i64() == cell);
    let locked_id = me.and_then(|player| player["championId"].as_i64()).unwrap_or(0);
    let hovered_id = me.and_then(|player| player["championPickIntent"].as_i64()).unwrap_or(0);
    // A pick action of ours that is completed means the champion is locked in.
    let completed = session["actions"].as_array().into_iter().flatten().flat_map(|turn| turn.as_array().into_iter().flatten()).any(|action| {
        action["actorCellId"].as_i64() == cell && action["type"].as_str() == Some("pick") && action["completed"].as_bool() == Some(true)
    });
    let flow = client.get_json("/lol-gameflow/v1/session").unwrap_or_default();
    let queue = &flow["gameData"]["queue"];
    let game_mode = queue["gameMode"].as_str().unwrap_or_default();
    let mode = match game_mode {
        "ARAM" => Some(Mode::Aram),
        "CLASSIC" | "" => Some(Mode::Ranked),
        // Arena and rotating modes have no OP.GG build of this kind.
        _ => None,
    };
    ChampSelect {
        active: true,
        champion_id: if locked_id > 0 { locked_id } else { hovered_id },
        locked: locked_id > 0 && (completed || session["actions"].as_array().is_none_or(Vec::is_empty)),
        position: opgg_position(me.and_then(|player| player["assignedPosition"].as_str()).unwrap_or_default()).to_owned(),
        mode,
        queue_id: queue["id"].as_i64().unwrap_or(0),
        enemies: session["theirTeam"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|player| player["championId"].as_i64().filter(|id| *id > 0))
            .collect(),
    }
}
