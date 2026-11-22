//! The end-of-game details of a recorded game (what the League client shows
//! after a game: scoreboard, stats, runes, gold and kills over time), read
//! from the client's match history over its local API and kept next to the
//! recording as `<name>.game.json`, so they stay after the client forgets.

use super::library::RecordingMeta;
use crate::riot_client::{league_client, LeagueApi};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `<folder>/<name>.game.json`.
pub fn path(video: &Path) -> PathBuf {
    video.with_extension("game.json")
}

pub fn load(video: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path(video)).ok()?).ok()
}

/// The game in the account's recent history that started closest to the
/// recording (the recording starts with the loading screen, a minute or so
/// after the game is created).
fn find_game(client: &LeagueApi, started_at: u64) -> Option<i64> {
    let list = client.get_json("/lol-match-history/v1/products/lol/current-summoner/matches?begIndex=0&endIndex=30")?;
    list["games"]["games"]
        .as_array()?
        .iter()
        .filter_map(|game| Some((game["gameId"].as_i64()?, game["gameCreation"].as_u64()?)))
        .map(|(id, created)| (id, created.abs_diff(started_at)))
        .filter(|(_, gap)| *gap <= 15 * 60 * 1000)
        .min_by_key(|(_, gap)| *gap)
        .map(|(id, _)| id)
}

/// Gold, level and CS per player each minute and the kills, objectives and
/// buildings with their time and place (a few kilobytes instead of the
/// client's full timeline). Arrays are in participant id order.
fn compact_timeline(timeline: &Value) -> Value {
    let frames = timeline["frames"].as_array().cloned().unwrap_or_default();
    let gold: Vec<Value> = frames
        .iter()
        .map(|frame| {
            let mut players: Vec<(i64, i64, i64, i64)> = frame["participantFrames"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(_, player)| {
                    let number = |key: &str| player[key].as_i64().unwrap_or(0);
                    (number("participantId"), number("totalGold"), number("level"), number("minionsKilled") + number("jungleMinionsKilled"))
                })
                .collect();
            players.sort_unstable();
            json!({
                "t": frame["timestamp"],
                "gold": players.iter().map(|player| player.1).collect::<Vec<_>>(),
                "level": players.iter().map(|player| player.2).collect::<Vec<_>>(),
                "cs": players.iter().map(|player| player.3).collect::<Vec<_>>(),
            })
        })
        .collect();
    let events: Vec<Value> = frames
        .iter()
        .flat_map(|frame| frame["events"].as_array().cloned().unwrap_or_default())
        .filter(|event| matches!(event["type"].as_str(), Some("CHAMPION_KILL" | "ELITE_MONSTER_KILL" | "BUILDING_KILL")))
        .map(|event| {
            json!({
                "t": event["timestamp"],
                "type": event["type"],
                "killer": event["killerId"],
                "victim": event["victimId"],
                "assists": event["assistingParticipantIds"],
                "x": event["position"]["x"],
                "y": event["position"]["y"],
                "monster": event["monsterType"],
                "sub": event["monsterSubType"],
                "building": event["buildingType"],
                "team": event["teamId"],
            })
        })
        .collect();
    json!({ "v": TIMELINE_VERSION, "frames": gold, "events": events })
}

/// 2: frames keep level and CS, monster kills keep their type (dragon element).
const TIMELINE_VERSION: i64 = 2;

/// "Win" or "Loss" for the recorded player, from saved details (None for a
/// remade game or when the player is not found).
pub fn result(details: &Value) -> Option<&'static str> {
    let game = &details["game"];
    if game["endOfGameResult"].as_str().is_some_and(|end| end.contains("Abort")) {
        return None;
    }
    let puuid = details["puuid"].as_str().filter(|puuid| !puuid.is_empty())?;
    let id = game["participantIdentities"].as_array()?.iter().find(|identity| identity["player"]["puuid"].as_str() == Some(puuid))?["participantId"].as_i64()?;
    let player = game["participants"].as_array()?.iter().find(|player| player["participantId"].as_i64() == Some(id))?;
    Some(if player["stats"]["win"].as_bool()? { "Win" } else { "Loss" })
}

/// Details saved by an older version (no levels and CS per minute, no dragon
/// types): read the timeline again by its game id while the client still has
/// it (any signed-in account).
/// Returns the details as they are when that is not possible.
pub fn upgrade(video: &Path, mut details: Value) -> Value {
    let has_frames = details["timeline"]["frames"].as_array().is_some_and(|frames| !frames.is_empty());
    if !has_frames || details["timeline"]["v"].as_i64().unwrap_or(1) >= TIMELINE_VERSION {
        return details;
    }
    let Some(id) = details["game"]["gameId"].as_i64() else { return details };
    let Some(client) = league_client() else { return details };
    let Some(timeline) = client.get_json(&format!("/lol-match-history/v1/game-timelines/{id}")) else { return details };
    let compact = compact_timeline(&timeline);
    if compact["frames"].as_array().is_none_or(Vec::is_empty) {
        return details;
    }
    details["timeline"] = compact;
    if video.is_file() {
        let _ = std::fs::write(path(video), details.to_string());
    }
    details
}

/// `<folder>/<name>.live.json`: every player's items, scores and level
/// sampled from the Live Client while the game was recorded.
pub fn live_path(video: &Path) -> PathBuf {
    video.with_extension("live.json")
}

pub fn load_live(video: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(live_path(video)).ok()?).ok()
}

/// The details of any game by its id (the account page's game list), kept in
/// the app's cache once read. The player is found by champion and score, so
/// no account has to be named. Same error codes as [`fetch`].
pub fn by_id(id: i64, champion: i64, score: (i64, i64, i64)) -> Result<Value, &'static str> {
    if id <= 0 {
        return Err("game_not_found");
    }
    let folder = crate::utils::app_data_dir().map_err(|_| "file_write")?.join("cache").join("games");
    let file = folder.join(format!("{id}.json"));
    if let Some(details) = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()) {
        return Ok(details);
    }
    let client = league_client().ok_or("game_client_needed")?;
    let game = client.get_json(&format!("/lol-match-history/v1/games/{id}")).ok_or("game_not_found")?;
    let empty = Vec::new();
    let participants = game["participants"].as_array().unwrap_or(&empty);
    if participants.is_empty() {
        return Err("game_not_found");
    }
    let stats = |participant: &Value| (participant["stats"]["kills"].as_i64().unwrap_or(-1), participant["stats"]["deaths"].as_i64().unwrap_or(-1), participant["stats"]["assists"].as_i64().unwrap_or(-1));
    let played = |participant: &&Value| participant["championId"].as_i64() == Some(champion);
    let me = participants.iter().find(|participant| played(participant) && stats(participant) == score).or_else(|| participants.iter().find(played));
    let puuid = me
        .and_then(|me| game["participantIdentities"].as_array()?.iter().find(|identity| identity["participantId"] == me["participantId"]))
        .and_then(|identity| identity["player"]["puuid"].as_str())
        .unwrap_or_default()
        .to_owned();
    let timeline = client.get_json(&format!("/lol-match-history/v1/game-timelines/{id}")).map(|timeline| compact_timeline(&timeline)).unwrap_or(Value::Null);
    let details = json!({ "puuid": puuid, "game": game, "timeline": timeline });
    if std::fs::create_dir_all(&folder).is_ok() {
        let _ = std::fs::write(&file, details.to_string());
    }
    Ok(details)
}

/// Read the game from the client and save it. Error codes: `game_client_needed`
/// (no client), `game_other_account` (another account is signed in),
/// `game_not_found` (not in the account's recent games, or not there yet).
pub fn fetch(video: &Path, meta: &RecordingMeta) -> Result<Value, &'static str> {
    let client = league_client().ok_or("game_client_needed")?;
    let summoner = client.get_json("/lol-summoner/v1/current-summoner").ok_or("game_client_needed")?;
    let puuid = summoner["puuid"].as_str().unwrap_or_default();
    if puuid.is_empty() {
        return Err("game_client_needed");
    }
    if !meta.puuid.is_empty() && meta.puuid != puuid {
        return Err("game_other_account");
    }
    // Recordings with champ select start minutes before the game itself.
    let started = meta.started_at + (meta.champ_select.max(0.0) * 1000.0) as u64;
    let id = find_game(&client, started).ok_or("game_not_found")?;
    let game = client.get_json(&format!("/lol-match-history/v1/games/{id}")).ok_or("game_not_found")?;
    if game["participants"].as_array().is_none_or(Vec::is_empty) {
        return Err("game_not_found");
    }
    let timeline = client.get_json(&format!("/lol-match-history/v1/game-timelines/{id}")).map(|timeline| compact_timeline(&timeline)).unwrap_or(Value::Null);
    let details = json!({ "puuid": puuid, "game": game, "timeline": timeline });
    // The recording may have been deleted meanwhile: never leave details alone.
    if !video.is_file() {
        return Err("recording_missing");
    }
    std::fs::write(path(video), details.to_string()).map_err(|_| "file_write")?;
    Ok(details)
}
