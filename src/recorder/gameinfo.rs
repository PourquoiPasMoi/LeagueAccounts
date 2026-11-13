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

/// Gold per player each minute and the kills, objectives and buildings with
/// their time and place (a few kilobytes instead of the client's full timeline).
fn compact_timeline(timeline: &Value) -> Value {
    let frames = timeline["frames"].as_array().cloned().unwrap_or_default();
    let gold: Vec<Value> = frames
        .iter()
        .map(|frame| {
            let mut players: Vec<(i64, i64)> = frame["participantFrames"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(_, player)| (player["participantId"].as_i64().unwrap_or(0), player["totalGold"].as_i64().unwrap_or(0)))
                .collect();
            players.sort_unstable();
            json!({ "t": frame["timestamp"], "gold": players.into_iter().map(|(_, gold)| gold).collect::<Vec<_>>() })
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
                "building": event["buildingType"],
                "team": event["teamId"],
            })
        })
        .collect();
    json!({ "frames": gold, "events": events })
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
    let id = find_game(&client, meta.started_at).ok_or("game_not_found")?;
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
