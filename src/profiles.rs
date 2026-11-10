//! Per-account profile snapshots read from the League client, plus the rank
//! history recorded at every rank refresh.
//!
//! Owned champions and skins are only known to the League client while that
//! account is signed in, so a snapshot is taken after each login through the
//! app (and on demand). Files live in `%APPDATA%\LeagueAccounts\profiles\`,
//! named by a hash of `region:account_id` so no account data appears in paths.

use crate::logging::{self, Event, Reason};
use crate::models::{Account, AccountKey, OpggGame};
use crate::riot_client::LeagueApi;
use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Rank history points kept per account (oldest dropped first).
const HISTORY_LIMIT: usize = 2_000;
/// Recent games kept from the client's match history.
pub const MATCH_COUNT: usize = 20;
/// Games asked from the client's match history at each sync.
const HISTORY_REQUEST: usize = 50;
/// Detailed game lookups per sync (each is one local request).
const DETAIL_LOOKUPS: usize = 30;
/// Games kept in the archive per account (newest kept).
const ARCHIVE_LIMIT: usize = 3_000;
/// A standing that has not changed is still recorded this often, so the rank
/// chart has a timeline instead of waiting for the next LP change.
const HISTORY_HEARTBEAT_MS: u64 = 12 * 3_600_000;

/// Real champion ids are below this. The client's inventory also lists game-mode
/// copies of champions (e.g. `Jade_Ahri`, id 60103) that must not be counted.
pub const MAX_CHAMPION_ID: i64 = 1_000;

pub fn is_real_champion(id: i64) -> bool {
    id > 0 && id < MAX_CHAMPION_ID
}

/// Serializes writes: refresh workers and login threads may record at once.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    /// Unix ms of the last successful snapshot.
    pub synced_at: u64,
    pub summoner_level: u32,
    pub profile_icon_id: i64,
    pub ranked: Vec<RankedQueue>,
    pub champions: Vec<OwnedChampion>,
    pub matches: Vec<MatchSummary>,
    pub wallet: Wallet,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RankedQueue {
    /// Riot's queue type, e.g. `RANKED_SOLO_5x5`, `RANKED_FLEX_SR`, `RANKED_TFT`.
    pub queue: String,
    pub tier: String,
    pub division: String,
    pub lp: i64,
    pub wins: u32,
    pub losses: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct OwnedChampion {
    pub id: i64,
    pub name: String,
    pub alias: String,
    pub title: String,
    pub mastery_level: u32,
    pub mastery_points: u64,
    /// Unix ms when the champion was unlocked, when Riot reports it.
    pub purchased: u64,
    /// Owned non-default skins.
    pub skins: Vec<OwnedSkin>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct OwnedSkin {
    pub id: i64,
    pub name: String,
    /// Image path relative to CommunityDragon's game-data root, lower-cased.
    pub tile: String,
    /// `epic`, `legendary`, `mythic`, `ultimate`, ... or empty.
    pub rarity: String,
    pub chromas_owned: u32,
    pub chromas_total: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MatchSummary {
    pub game_id: i64,
    /// Unix ms when the game started.
    pub created: u64,
    pub duration_seconds: u64,
    pub queue_id: i64,
    pub mode: String,
    pub champion_id: i64,
    pub win: bool,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub cs: u32,
    pub champion_level: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Wallet {
    pub rp: Option<i64>,
    pub blue_essence: Option<i64>,
}

/// One game in the account's archive. Client games carry every field; OP.GG
/// games carry the result, K/D/A, CS and OP Score only.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MatchRecord {
    /// `client` or `opgg`.
    pub source: String,
    /// The client's game id; 0 for OP.GG games.
    pub game_id: i64,
    /// Unix ms when the game was created / started.
    pub created: u64,
    pub duration_seconds: u64,
    pub queue_id: i64,
    /// OP.GG's queue type (`SOLORANKED`, ...); empty for client games.
    pub queue: String,
    pub mode: String,
    pub champion_id: i64,
    /// Champion display name (always set for OP.GG games).
    pub champion: String,
    pub win: bool,
    pub remake: bool,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    pub cs: u32,
    pub champion_level: u32,
    pub gold: u32,
    pub damage: u32,
    pub damage_taken: u32,
    pub damage_mitigated: u32,
    pub healing: u32,
    pub vision: u32,
    pub wards_placed: u32,
    pub wards_killed: u32,
    pub control_wards: u32,
    pub cc_seconds: u32,
    pub turret_kills: u32,
    /// The player's team totals, for kill participation and damage share.
    pub team_kills: u32,
    pub team_damage: u32,
    /// `TOP`, `JUNGLE`, `MIDDLE`, `BOTTOM`, `UTILITY` or empty.
    pub position: String,
    pub items: Vec<i64>,
    pub spells: Vec<i64>,
    pub largest_multi_kill: u32,
    pub double_kills: u32,
    pub triple_kills: u32,
    pub quadra_kills: u32,
    pub penta_kills: u32,
    pub first_blood: bool,
    /// OP Score times ten, OP.GG games only.
    pub op_score_x10: Option<u32>,
}

/// One rank observation, recorded when the standing changes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RankPoint {
    /// Unix ms.
    pub at: u64,
    /// `lol` or `tft`.
    pub mode: String,
    pub tier: String,
    pub division: String,
    pub lp: i64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn directory() -> std::io::Result<PathBuf> {
    let directory = app_data_dir()?.join("profiles");
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn file_stem(key: &AccountKey) -> String {
    let digest = Sha256::digest(format!("{}:{}", key.region, key.account_id.to_lowercase()));
    digest.iter().take(12).map(|byte| format!("{byte:02x}")).collect()
}

fn profile_path(directory: &Path, key: &AccountKey) -> PathBuf {
    directory.join(format!("{}.json", file_stem(key)))
}

fn history_path(directory: &Path, key: &AccountKey) -> PathBuf {
    directory.join(format!("{}.ranks.json", file_stem(key)))
}

fn archive_path(directory: &Path, key: &AccountKey) -> PathBuf {
    directory.join(format!("{}.games.json", file_stem(key)))
}

/// Every archived game, newest first.
pub fn archive(key: &AccountKey) -> Vec<MatchRecord> {
    directory()
        .map(|directory| read_json(&archive_path(&directory, key)))
        .unwrap_or_default()
}

/// The same game reported twice: by id, or (across sources) by a close start
/// time with the same K/D/A.
fn same_game(a: &MatchRecord, b: &MatchRecord) -> bool {
    if a.game_id != 0 && a.game_id == b.game_id {
        return true;
    }
    // OP.GG games have no id: the same start time and champion is the same game.
    if a.game_id == 0 && b.game_id == 0 {
        return a.created == b.created && a.champion.eq_ignore_ascii_case(&b.champion);
    }
    a.source != b.source
        && a.created.abs_diff(b.created) < 20 * 60_000
        && (a.kills, a.deaths, a.assists) == (b.kills, b.deaths, b.assists)
}

/// Add games to the archive; client records replace OP.GG copies of the same
/// game. Returns how many games were new.
fn merge_into(archive: &mut Vec<MatchRecord>, incoming: Vec<MatchRecord>) -> usize {
    let mut added = 0;
    for record in incoming {
        match archive.iter().position(|existing| same_game(existing, &record)) {
            Some(index) => {
                if record.source == "client" && archive[index].source != "client" {
                    archive[index] = record;
                }
            }
            None => {
                archive.push(record);
                added += 1;
            }
        }
    }
    archive.sort_by(|a, b| b.created.cmp(&a.created));
    archive.truncate(ARCHIVE_LIMIT);
    added
}

pub fn merge_games(key: &AccountKey, incoming: Vec<MatchRecord>) -> usize {
    if incoming.is_empty() {
        return 0;
    }
    let _lock = WRITE_LOCK.lock();
    let Ok(directory) = directory() else { return 0 };
    let path = archive_path(&directory, key);
    let mut games: Vec<MatchRecord> = read_json(&path);
    let added = merge_into(&mut games, incoming);
    if added > 0 || games.iter().any(|game| game.source == "client") {
        if let Err(error) = write_json(&path, &games) {
            logging::record(Event::ProfileSaveFailed, Reason::from_error(&error));
        }
    }
    added
}

/// OP.GG's `queueType` as the client's queue id, so both sources filter alike.
fn opgg_queue_id(queue: &str) -> i64 {
    match queue {
        "SOLORANKED" => 420,
        "FLEXRANKED" => 440,
        "NORMAL" | "DRAFT" => 400,
        "BLIND" => 430,
        "QUICKPLAY" | "SWIFTPLAY" => 490,
        "ARAM" => 450,
        "ARENA" | "CHERRY" => 1700,
        "URF" | "ARURF" => 900,
        "BOT" => 800,
        _ => 0,
    }
}

/// Games listed on the account's OP.GG page, recorded at every rank refresh.
pub fn record_opgg_games(key: &AccountKey, games: &[OpggGame]) -> usize {
    let records = games
        .iter()
        .map(|game| MatchRecord {
            source: "opgg".into(),
            created: game.start,
            queue_id: opgg_queue_id(&game.queue),
            queue: game.queue.clone(),
            champion: game.champion.clone(),
            win: game.win == Some(true),
            remake: game.win.is_none(),
            kills: game.kills,
            deaths: game.deaths,
            assists: game.assists,
            cs: game.cs,
            op_score_x10: game.op_score_x10,
            ..MatchRecord::default()
        })
        .collect();
    merge_games(key, records)
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value).map_err(std::io::Error::other)?;
    file.flush()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn load(key: &AccountKey) -> Option<Profile> {
    let path = profile_path(&directory().ok()?, key);
    path.exists().then(|| read_json(&path))
}

pub fn history(key: &AccountKey) -> Vec<RankPoint> {
    directory()
        .map(|directory| read_json(&history_path(&directory, key)))
        .unwrap_or_default()
}

pub fn save(key: &AccountKey, profile: &Profile) -> std::io::Result<()> {
    let _lock = WRITE_LOCK.lock();
    let result = directory().and_then(|directory| write_json(&profile_path(&directory, key), profile));
    if let Err(error) = &result {
        logging::record(Event::ProfileSaveFailed, Reason::from_error(error));
    }
    result
}

/// Forget everything stored for a deleted account.
pub fn remove(key: &AccountKey) {
    let _lock = WRITE_LOCK.lock();
    if let Ok(directory) = directory() {
        let _ = std::fs::remove_file(profile_path(&directory, key));
        let _ = std::fs::remove_file(history_path(&directory, key));
        let _ = std::fs::remove_file(archive_path(&directory, key));
    }
}

/// Every stored profile, for searching skins across accounts.
pub fn load_all(accounts: &[Account]) -> Vec<(AccountKey, Profile)> {
    accounts
        .iter()
        .filter_map(|account| load(&account.key()).map(|profile| (account.key(), profile)))
        .collect()
}

/// Append a point when the standing differs from the last one for that mode.
fn push_point(points: &mut Vec<RankPoint>, point: RankPoint) -> bool {
    let unchanged = points.iter().rev().find(|last| last.mode == point.mode).is_some_and(|last| {
        last.tier.eq_ignore_ascii_case(&point.tier)
            && last.division.eq_ignore_ascii_case(&point.division)
            && last.lp == point.lp
            && point.at.saturating_sub(last.at) < HISTORY_HEARTBEAT_MS
    });
    if unchanged {
        return false;
    }
    points.push(point);
    if points.len() > HISTORY_LIMIT {
        let excess = points.len() - HISTORY_LIMIT;
        points.drain(..excess);
    }
    true
}

fn rankable(tier: &str) -> bool {
    !tier.is_empty() && !tier.eq_ignore_ascii_case("Error")
}

/// Record the account's current LoL and TFT standing after a rank refresh.
pub fn record_ranks(account: &Account) {
    let mut candidates = Vec::new();
    let at = now_ms();
    if rankable(&account.tier) && account.rank_provenance.as_ref().is_none_or(|p| p.error.is_none()) {
        candidates.push(RankPoint {
            at,
            mode: "lol".into(),
            tier: account.tier.clone(),
            division: account.division.clone(),
            lp: account.lp.trim().parse().unwrap_or(0),
        });
    }
    if rankable(&account.tft.tier) && account.tft_provenance.as_ref().is_none_or(|p| p.error.is_none()) {
        candidates.push(RankPoint {
            at,
            mode: "tft".into(),
            tier: account.tft.tier.clone(),
            division: account.tft.division.clone(),
            lp: account.tft.lp.trim().parse().unwrap_or(0),
        });
    }
    append_points(&account.key(), candidates);
}

fn append_points(key: &AccountKey, candidates: Vec<RankPoint>) {
    if candidates.is_empty() {
        return;
    }
    let _lock = WRITE_LOCK.lock();
    let Ok(directory) = directory() else { return };
    let path = history_path(&directory, key);
    let mut points: Vec<RankPoint> = read_json(&path);
    let mut changed = false;
    for point in candidates {
        changed |= push_point(&mut points, point);
    }
    if changed {
        if let Err(error) = write_json(&path, &points) {
            logging::record(Event::ProfileSaveFailed, Reason::from_error(&error));
        }
    }
}

// ------------------------------------------------------------------ parsing

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().trim().to_owned()
}

fn int(value: &Value) -> i64 {
    value.as_i64().or_else(|| value.as_f64().map(|v| v as i64)).unwrap_or(0)
}

fn uint(value: &Value) -> u32 {
    u32::try_from(int(value).max(0)).unwrap_or(u32::MAX)
}

/// `/lol-game-data/assets/ASSETS/Characters/Ahri/Skins/Skin14/Images/x.jpg`
/// → `assets/characters/ahri/skins/skin14/images/x.jpg` (CommunityDragon's layout).
pub fn game_data_path(path: &str) -> String {
    path.trim()
        .strip_prefix("/lol-game-data/assets/")
        .unwrap_or_default()
        .to_lowercase()
}

/// `.../rarity-gem-icons/legendary.png` → `legendary`.
fn rarity_from_gem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn owned(value: &Value) -> bool {
    value["ownership"]["owned"].as_bool().unwrap_or(false)
}

pub fn parse_champions(inventory: &Value, mastery: Option<&Value>) -> Vec<OwnedChampion> {
    let mastery: HashMap<i64, (u32, u64)> = mastery
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        int(&entry["championId"]),
                        (uint(&entry["championLevel"]), int(&entry["championPoints"]).max(0) as u64),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut champions: Vec<OwnedChampion> = inventory
        .as_array()
        .into_iter()
        .flatten()
        .filter(|champion| is_real_champion(int(&champion["id"])) && owned(champion))
        // A free-rotation or trial champion is not owned.
        .filter(|champion| !champion["ownership"]["rental"]["rented"].as_bool().unwrap_or(false))
        .map(|champion| {
            let id = int(&champion["id"]);
            let (mastery_level, mastery_points) = mastery.get(&id).copied().unwrap_or_default();
            let skins = champion["skins"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|skin| !skin["isBase"].as_bool().unwrap_or(false) && owned(skin))
                .map(|skin| {
                    let chromas = skin["chromas"].as_array().map(Vec::as_slice).unwrap_or_default();
                    OwnedSkin {
                        id: int(&skin["id"]),
                        name: text(&skin["name"]),
                        tile: game_data_path(skin["tilePath"].as_str().unwrap_or_default()),
                        rarity: rarity_from_gem(skin["rarityGemPath"].as_str().unwrap_or_default()),
                        chromas_owned: chromas.iter().filter(|chroma| owned(chroma)).count() as u32,
                        chromas_total: chromas.len() as u32,
                    }
                })
                .collect();
            OwnedChampion {
                id,
                name: text(&champion["name"]),
                alias: text(&champion["alias"]),
                title: text(&champion["title"]),
                mastery_level,
                mastery_points,
                purchased: int(&champion["purchased"]).max(0) as u64,
                skins,
            }
        })
        .collect();
    champions.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    champions
}

pub fn parse_ranked(stats: &Value) -> Vec<RankedQueue> {
    stats["queues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|queue| {
            let name = text(&queue["queueType"]);
            if name.is_empty() {
                return None;
            }
            let tier = text(&queue["tier"]);
            let division = text(&queue["division"]);
            Some(RankedQueue {
                queue: name,
                tier: if tier.is_empty() || tier == "NONE" { "Unranked".into() } else { title_case(&tier) },
                division: if division == "NA" { String::new() } else { division },
                lp: int(&queue["leaguePoints"]),
                wins: uint(&queue["wins"]),
                losses: uint(&queue["losses"]),
            })
        })
        .collect()
}

fn title_case(value: &str) -> String {
    let lower = value.to_lowercase();
    let mut chars = lower.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

pub fn parse_matches(history: &Value) -> Vec<MatchSummary> {
    history["games"]["games"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|game| {
            // The current-summoner endpoint lists only the player's own participant.
            let player = game["participants"].as_array()?.first()?;
            let stats = &player["stats"];
            Some(MatchSummary {
                game_id: int(&game["gameId"]),
                created: int(&game["gameCreation"]).max(0) as u64,
                duration_seconds: int(&game["gameDuration"]).max(0) as u64,
                queue_id: int(&game["queueId"]),
                mode: text(&game["gameMode"]),
                champion_id: int(&player["championId"]),
                win: stats["win"].as_bool().unwrap_or(false),
                kills: uint(&stats["kills"]),
                deaths: uint(&stats["deaths"]),
                assists: uint(&stats["assists"]),
                cs: uint(&stats["totalMinionsKilled"]) + uint(&stats["neutralMinionsKilled"]),
                champion_level: uint(&stats["champLevel"]),
            })
        })
        .take(MATCH_COUNT)
        .collect()
}

/// The player's lane from the client's legacy `timeline` fields.
fn position(lane: &str, role: &str) -> String {
    match (lane, role) {
        ("TOP", _) => "TOP",
        ("JUNGLE", _) => "JUNGLE",
        ("MIDDLE" | "MID", _) => "MIDDLE",
        ("BOTTOM" | "BOT", "DUO_SUPPORT") => "UTILITY",
        ("BOTTOM" | "BOT", _) => "BOTTOM",
        _ => "",
    }
    .to_owned()
}

/// One client game as an archive record. `puuid` picks the player in a full
/// game (all ten participants); the history list holds only the player.
pub fn parse_client_game(game: &Value, puuid: &str) -> Option<MatchRecord> {
    let identities = game["participantIdentities"].as_array();
    let participants = game["participants"].as_array()?;
    let participant_id = identities
        .and_then(|list| {
            list.iter()
                .find(|identity| !puuid.is_empty() && identity["player"]["puuid"].as_str() == Some(puuid))
        })
        .map(|identity| int(&identity["participantId"]));
    let player = match participant_id {
        Some(id) => participants.iter().find(|participant| int(&participant["participantId"]) == id)?,
        None if participants.len() == 1 => participants.first()?,
        None => return None,
    };
    let stats = &player["stats"];
    let team = int(&player["teamId"]);
    let teammates = || participants.iter().filter(move |participant| int(&participant["teamId"]) == team);
    let full = participants.len() > 1;
    let duration = int(&game["gameDuration"]).max(0) as u64;
    Some(MatchRecord {
        source: "client".into(),
        game_id: int(&game["gameId"]),
        created: int(&game["gameCreation"]).max(0) as u64,
        duration_seconds: duration,
        queue_id: int(&game["queueId"]),
        queue: String::new(),
        mode: text(&game["gameMode"]),
        champion_id: int(&player["championId"]),
        champion: String::new(),
        win: stats["win"].as_bool().unwrap_or(false),
        remake: duration > 0 && duration < 300 || stats["gameEndedInEarlySurrender"].as_bool().unwrap_or(false),
        kills: uint(&stats["kills"]),
        deaths: uint(&stats["deaths"]),
        assists: uint(&stats["assists"]),
        cs: uint(&stats["totalMinionsKilled"]) + uint(&stats["neutralMinionsKilled"]),
        champion_level: uint(&stats["champLevel"]),
        gold: uint(&stats["goldEarned"]),
        damage: uint(&stats["totalDamageDealtToChampions"]),
        damage_taken: uint(&stats["totalDamageTaken"]),
        damage_mitigated: uint(&stats["damageSelfMitigated"]),
        healing: uint(&stats["totalHeal"]),
        vision: uint(&stats["visionScore"]),
        wards_placed: uint(&stats["wardsPlaced"]),
        wards_killed: uint(&stats["wardsKilled"]),
        control_wards: uint(&stats["visionWardsBoughtInGame"]),
        cc_seconds: uint(&stats["timeCCingOthers"]),
        turret_kills: uint(&stats["turretKills"]),
        team_kills: if full { teammates().map(|mate| uint(&mate["stats"]["kills"])).sum() } else { 0 },
        team_damage: if full { teammates().map(|mate| uint(&mate["stats"]["totalDamageDealtToChampions"])).sum() } else { 0 },
        position: position(
            player["timeline"]["lane"].as_str().unwrap_or_default(),
            player["timeline"]["role"].as_str().unwrap_or_default(),
        ),
        items: (0..7).map(|slot| int(&stats[format!("item{slot}")])).filter(|item| *item > 0).collect(),
        spells: [int(&player["spell1Id"]), int(&player["spell2Id"])].into_iter().filter(|spell| *spell > 0).collect(),
        largest_multi_kill: uint(&stats["largestMultiKill"]),
        double_kills: uint(&stats["doubleKills"]),
        triple_kills: uint(&stats["tripleKills"]),
        quadra_kills: uint(&stats["quadraKills"]),
        penta_kills: uint(&stats["pentaKills"]),
        first_blood: stats["firstBloodKill"].as_bool().unwrap_or(false) || stats["firstBloodAssist"].as_bool().unwrap_or(false),
        op_score_x10: None,
    })
}

/// `{"RP": 1350}` or `{"lol_blue_essence": 4200}`: the first number in the body.
fn wallet_amount(body: Option<Value>) -> Option<i64> {
    let body = body?;
    body.as_i64()
        .or_else(|| body.as_object()?.values().find_map(Value::as_i64))
}

/// What a sync could read: the snapshot and the games for the archive.
/// `None` = the client is not ready (no summoner or no champion inventory
/// yet); the caller retries.
pub fn collect(api: &LeagueApi, previous: Option<&Profile>, key: &AccountKey) -> Option<(Profile, Vec<MatchRecord>)> {
    let summoner = api.get_json("/lol-summoner/v1/current-summoner")?;
    let summoner_id = int(&summoner["summonerId"]);
    if summoner_id <= 0 {
        return None;
    }
    let inventory = api.get_json(&format!("/lol-champions/v1/inventories/{summoner_id}/champions"))?;
    if inventory.as_array().is_none_or(Vec::is_empty) {
        return None;
    }
    let mastery = api.get_json("/lol-champion-mastery/v1/local-player/champion-mastery");
    let champions = parse_champions(&inventory, mastery.as_ref());
    let ranked = api.get_json("/lol-ranked/v1/current-ranked-stats").map(|stats| parse_ranked(&stats));
    let history = api.get_json(&format!(
        "/lol-match-history/v1/products/lol/current-summoner/matches?begIndex=0&endIndex={}",
        HISTORY_REQUEST - 1
    ));
    let matches = history.as_ref().map(parse_matches);
    let puuid = text(&summoner["puuid"]);
    let known: std::collections::HashSet<i64> = archive(key)
        .into_iter()
        .filter(|game| game.source == "client" && game.team_kills > 0)
        .map(|game| game.game_id)
        .collect();
    let mut lookups = 0;
    let records: Vec<MatchRecord> = history
        .as_ref()
        .and_then(|history| history["games"]["games"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|game| {
            let id = int(&game["gameId"]);
            // The full game adds team totals (kill participation, damage share).
            if !known.contains(&id) && lookups < DETAIL_LOOKUPS {
                lookups += 1;
                if let Some(full) = api.get_json(&format!("/lol-match-history/v1/games/{id}")) {
                    if let Some(record) = parse_client_game(&full, &puuid) {
                        return Some(record);
                    }
                }
            }
            parse_client_game(game, &puuid)
        })
        .collect();
    let wallet = Wallet {
        rp: wallet_amount(api.get_json("/lol-inventory/v1/wallet/RP")),
        blue_essence: wallet_amount(api.get_json("/lol-inventory/v1/wallet/lol_blue_essence")),
    };
    // A part that failed to load keeps what the last sync saw.
    let profile = Profile {
        synced_at: now_ms(),
        summoner_level: uint(&summoner["summonerLevel"]),
        profile_icon_id: int(&summoner["profileIconId"]),
        ranked: ranked
            .or_else(|| previous.map(|profile| profile.ranked.clone()))
            .unwrap_or_default(),
        champions,
        matches: matches
            .filter(|matches| !matches.is_empty())
            .or_else(|| previous.map(|profile| profile.matches.clone()))
            .unwrap_or_default(),
        wallet: Wallet {
            rp: wallet.rp.or(previous.and_then(|profile| profile.wallet.rp)),
            blue_essence: wallet.blue_essence.or(previous.and_then(|profile| profile.wallet.blue_essence)),
        },
    };
    Some((profile, records))
}

/// Store a fresh snapshot and add its ranked standing to the rank history.
pub fn store_snapshot(key: &AccountKey, profile: &Profile, games: Vec<MatchRecord>) -> std::io::Result<()> {
    save(key, profile)?;
    // Client games know the champion id only; name it from the inventory.
    let names: HashMap<i64, String> = profile.champions.iter().map(|c| (c.id, c.name.clone())).collect();
    let games = games
        .into_iter()
        .map(|mut game| {
            if let Some(name) = names.get(&game.champion_id) {
                game.champion = name.clone();
            }
            game
        })
        .collect();
    merge_games(key, games);
    let points = profile
        .ranked
        .iter()
        .filter_map(|queue| {
            let mode = match queue.queue.as_str() {
                "RANKED_SOLO_5x5" => "lol",
                "RANKED_TFT" => "tft",
                _ => return None,
            };
            (queue.tier != "Unranked").then(|| RankPoint {
                at: profile.synced_at,
                mode: mode.into(),
                tier: queue.tier.clone(),
                division: queue.division.clone(),
                lp: queue.lp,
            })
        })
        .collect();
    append_points(key, points);
    logging::record(Event::ProfileSynced, Reason::None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn champions_keep_only_owned_non_default_skins_and_skip_rentals() {
        let inventory = json!([
            {"id": -1, "name": "None", "ownership": {"owned": true}},
            {"id": 103, "name": "Ahri", "alias": "Ahri", "title": "the Nine-Tailed Fox", "purchased": 1500,
             "ownership": {"owned": true, "rental": {"rented": false}},
             "skins": [
                {"id": 103000, "isBase": true, "name": "Ahri", "ownership": {"owned": true}},
                {"id": 103014, "isBase": false, "name": "Star Guardian Ahri", "ownership": {"owned": true},
                 "tilePath": "/lol-game-data/assets/ASSETS/Characters/Ahri/Skins/Skin14/Images/ahri_splash_tile_14.jpg",
                 "rarityGemPath": "/lol-game-data/assets/v1/rarity-gem-icons/legendary.png",
                 "chromas": [{"id": 1, "ownership": {"owned": true}}, {"id": 2, "ownership": {"owned": false}}]},
                {"id": 103015, "isBase": false, "name": "K/DA Ahri", "ownership": {"owned": false}}
             ]},
            {"id": 1, "name": "Annie", "ownership": {"owned": true, "rental": {"rented": true}}, "skins": []},
            {"id": 2, "name": "Olaf", "ownership": {"owned": false}, "skins": []},
            {"id": 60103, "name": "Ahri", "alias": "Jade_Ahri", "ownership": {"owned": true}, "skins": [
                {"id": 60103001, "isBase": false, "name": "Copy", "ownership": {"owned": true}}]}
        ]);
        let mastery = json!([{"championId": 103, "championLevel": 7, "championPoints": 123456}]);
        let champions = parse_champions(&inventory, Some(&mastery));
        assert_eq!(champions.len(), 1);
        let ahri = &champions[0];
        assert_eq!((ahri.mastery_level, ahri.mastery_points, ahri.purchased), (7, 123456, 1500));
        assert_eq!(ahri.skins.len(), 1);
        let skin = &ahri.skins[0];
        assert_eq!(skin.tile, "assets/characters/ahri/skins/skin14/images/ahri_splash_tile_14.jpg");
        assert_eq!(skin.rarity, "legendary");
        assert_eq!((skin.chromas_owned, skin.chromas_total), (1, 2));
    }

    #[test]
    fn ranked_queues_and_matches_parse_from_client_shapes() {
        let ranked = parse_ranked(&json!({"queues": [
            {"queueType": "RANKED_SOLO_5x5", "tier": "GOLD", "division": "IV", "leaguePoints": 22, "wins": 30, "losses": 28},
            {"queueType": "RANKED_FLEX_SR", "tier": "", "division": "NA", "leaguePoints": 0, "wins": 0, "losses": 0}
        ]}));
        assert_eq!(ranked[0].tier, "Gold");
        assert_eq!((ranked[0].lp, ranked[0].wins, ranked[0].losses), (22, 30, 28));
        assert_eq!((ranked[1].tier.as_str(), ranked[1].division.as_str()), ("Unranked", ""));

        let matches = parse_matches(&json!({"games": {"games": [{
            "gameId": 9, "gameCreation": 1700000000000u64, "gameDuration": 1800, "queueId": 420, "gameMode": "CLASSIC",
            "participants": [{"championId": 103, "stats": {"win": true, "kills": 7, "deaths": 2, "assists": 9,
                "totalMinionsKilled": 180, "neutralMinionsKilled": 12, "champLevel": 16}}]
        }]}}));
        assert_eq!(matches.len(), 1);
        assert!(matches[0].win);
        assert_eq!((matches[0].cs, matches[0].champion_id, matches[0].queue_id), (192, 103, 420));
    }

    #[test]
    fn rank_history_records_changes_only_and_stays_bounded() {
        let point = |lp| RankPoint { at: 0, mode: "lol".into(), tier: "Gold".into(), division: "IV".into(), lp };
        let mut points = Vec::new();
        assert!(push_point(&mut points, point(10)));
        assert!(!push_point(&mut points, point(10)));
        assert!(push_point(&mut points, RankPoint { mode: "tft".into(), ..point(10) }));
        assert!(push_point(&mut points, point(30)));
        assert_eq!(points.len(), 3);
        for lp in 0..(HISTORY_LIMIT as i64 + 5) {
            push_point(&mut points, point(lp + 100));
        }
        assert_eq!(points.len(), HISTORY_LIMIT);
    }

    #[test]
    fn full_client_games_pick_the_player_and_team_totals() {
        let game = json!({
            "gameId": 77, "gameCreation": 1000, "gameDuration": 1800, "queueId": 420, "gameMode": "CLASSIC",
            "participantIdentities": [
                {"participantId": 1, "player": {"puuid": "other"}},
                {"participantId": 2, "player": {"puuid": "me"}},
                {"participantId": 3, "player": {"puuid": "enemy"}}
            ],
            "participants": [
                {"participantId": 1, "teamId": 100, "championId": 1, "stats": {"kills": 4, "totalDamageDealtToChampions": 10000}},
                {"participantId": 2, "teamId": 100, "championId": 103, "spell1Id": 4, "spell2Id": 14,
                 "timeline": {"lane": "BOTTOM", "role": "DUO_SUPPORT"},
                 "stats": {"win": true, "kills": 6, "deaths": 1, "assists": 8, "totalDamageDealtToChampions": 20000,
                   "goldEarned": 12000, "visionScore": 40, "item0": 3157, "item1": 0, "item6": 3364, "pentaKills": 1}},
                {"participantId": 3, "teamId": 200, "championId": 2, "stats": {"kills": 9, "totalDamageDealtToChampions": 50000}}
            ]
        });
        let record = parse_client_game(&game, "me").unwrap();
        assert_eq!((record.champion_id, record.kills, record.team_kills, record.team_damage), (103, 6, 10, 30000));
        assert_eq!(record.position, "UTILITY");
        assert_eq!(record.items, vec![3157, 3364]);
        assert_eq!(record.spells, vec![4, 14]);
        assert_eq!(record.penta_kills, 1);
        assert!(record.win && !record.remake);
        assert!(parse_client_game(&game, "missing").is_none());
    }

    #[test]
    fn archive_merges_sources_without_duplicates() {
        let opgg = MatchRecord { source: "opgg".into(), created: 10 * 60_000, kills: 3, deaths: 2, assists: 5, ..MatchRecord::default() };
        let client = MatchRecord { source: "client".into(), game_id: 9, created: 8 * 60_000, kills: 3, deaths: 2, assists: 5, gold: 9000, ..MatchRecord::default() };
        let mut archive = Vec::new();
        assert_eq!(merge_into(&mut archive, vec![opgg.clone()]), 1);
        assert_eq!(merge_into(&mut archive, vec![opgg.clone()]), 0);
        // The client's copy of the same game replaces OP.GG's.
        assert_eq!(merge_into(&mut archive, vec![client.clone()]), 0);
        assert_eq!(archive.len(), 1);
        assert_eq!(archive[0].gold, 9000);
        // OP.GG never replaces a client game.
        assert_eq!(merge_into(&mut archive, vec![opgg]), 0);
        assert_eq!(archive[0].source, "client");
        let other = MatchRecord { game_id: 10, created: 99 * 60_000, ..client };
        assert_eq!(merge_into(&mut archive, vec![other]), 1);
        assert_eq!(archive[0].game_id, 10);
    }

    #[test]
    fn unchanged_rank_is_still_recorded_after_the_heartbeat() {
        let point = |at| RankPoint { at, mode: "lol".into(), tier: "Gold".into(), division: "IV".into(), lp: 10 };
        let mut points = vec![point(0)];
        assert!(!push_point(&mut points, point(HISTORY_HEARTBEAT_MS - 1)));
        assert!(push_point(&mut points, point(HISTORY_HEARTBEAT_MS)));
    }

    #[test]
    fn file_names_hide_the_account() {
        let key = AccountKey { account_id: "SecretUser".into(), region: "euw".into() };
        let stem = file_stem(&key);
        assert_eq!(stem.len(), 24);
        assert!(!stem.to_lowercase().contains("secret"));
        assert_eq!(stem, file_stem(&AccountKey { account_id: "secretuser".into(), region: "euw".into() }));
    }
}
