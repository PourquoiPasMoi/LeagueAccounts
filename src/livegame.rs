//! The current game for the app's live view: everyone's rank, season win rate
//! and recent form (once per game, from the League client's local API: game
//! session, ranked stats and match history by player id), and live stats
//! (from Riot's Live Client Data API on 127.0.0.1:2999, read-only, published by
//! Riot for this purpose). Everything is local: no internet traffic, and
//! nothing reads or touches the game process.

use crate::riot_client::LeagueApi;
use serde::Serialize;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QueueRank {
    pub tier: String,
    pub division: String,
    pub lp: i64,
    pub wins: u32,
    pub losses: u32,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LivePlayer {
    pub game_name: String,
    pub tag_line: String,
    pub champion_id: i64,
    /// 1 or 2 (Summoner's Rift sides), or the Arena team number.
    pub team: i64,
    pub is_me: bool,
    pub position: String,
    pub solo: Option<QueueRank>,
    pub flex: Option<QueueRank>,
    /// Last season's final Solo/Duo tier, when the player is unranked now.
    pub previous_tier: String,
    pub recent_games: u32,
    pub recent_wins: u32,
    /// Games and wins on the champion they play now, among the recent games.
    pub champion_games: u32,
    pub champion_wins: u32,
    /// The client had no data for this player (hidden name, bot).
    pub unknown: bool,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveGame {
    pub game_id: i64,
    pub queue_id: i64,
    pub game_mode: String,
    pub my_team: i64,
    pub players: Vec<LivePlayer>,
}

/// Recent games looked at per player.
const RECENT: usize = 20;

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn queue_rank(entry: &Value) -> Option<QueueRank> {
    let tier = entry["tier"].as_str().unwrap_or_default();
    if tier.is_empty() || tier == "NONE" {
        return None;
    }
    Some(QueueRank {
        tier: tier.to_owned(),
        division: text(&entry["division"]).replace("NA", ""),
        lp: entry["leaguePoints"].as_i64().unwrap_or(0),
        wins: entry["wins"].as_u64().unwrap_or(0) as u32,
        losses: entry["losses"].as_u64().unwrap_or(0) as u32,
    })
}

/// Fill in one player's rank and recent games from the client.
fn enrich(client: &LeagueApi, player: &mut LivePlayer, puuid: &str) {
    if puuid.is_empty() {
        player.unknown = true;
        return;
    }
    if player.game_name.is_empty() {
        if let Some(summoner) = client.get_json(&format!("/lol-summoner/v2/summoners/puuid/{puuid}")) {
            player.game_name = text(&summoner["gameName"]);
            player.tag_line = text(&summoner["tagLine"]);
        }
    }
    match client.get_json(&format!("/lol-ranked/v1/ranked-stats/{puuid}")) {
        Some(stats) => {
            player.solo = queue_rank(&stats["queueMap"]["RANKED_SOLO_5x5"]);
            player.flex = queue_rank(&stats["queueMap"]["RANKED_FLEX_SR"]);
            let previous = stats["queueMap"]["RANKED_SOLO_5x5"]["previousSeasonEndTier"].as_str().unwrap_or_default();
            if previous != "NONE" {
                player.previous_tier = previous.to_owned();
            }
        }
        None => player.unknown = true,
    }
    let history = client.get_json(&format!("/lol-match-history/v1/products/lol/{puuid}/matches?begIndex=0&endIndex={RECENT}"));
    for game in history.as_ref().and_then(|history| history["games"]["games"].as_array()).into_iter().flatten() {
        // Remakes say nothing about the player.
        if game["gameDuration"].as_i64().unwrap_or(0) < 300 {
            continue;
        }
        let Some(me) = game["participants"].as_array().and_then(|list| list.first()) else { continue };
        let win = me["stats"]["win"].as_bool().unwrap_or(false);
        player.recent_games += 1;
        player.recent_wins += u32::from(win);
        if me["championId"].as_i64() == Some(player.champion_id) {
            player.champion_games += 1;
            player.champion_wins += u32::from(win);
        }
    }
}

/// Riot's Live Client Data API (in-game only, loopback, self-signed).
static LIVE_CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
});

fn live_client(path: &str) -> Option<Value> {
    let response = LIVE_CLIENT.as_ref()?.get(format!("https://127.0.0.1:2999{path}")).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    serde_json::from_str(&response.text().ok()?).ok()
}

/// The match has started (the loading screen is over).
pub fn game_started() -> bool {
    live_client("/liveclientdata/gamestats")
        .and_then(|stats| stats["gameTime"].as_f64())
        .is_some_and(|time| time > 0.0)
}

/// Everyone in the current game with their stats, or `None` outside a game.
pub fn live_game(client: &LeagueApi) -> Option<LiveGame> {
    let session = client.get_json("/lol-gameflow/v1/session")?;
    let data = &session["gameData"];
    let me = client.get_json("/lol-summoner/v1/current-summoner").map(|summoner| text(&summoner["puuid"])).unwrap_or_default();
    let mut entries: Vec<(LivePlayer, String)> = Vec::new();
    for (team, key) in [(1, "teamOne"), (2, "teamTwo")] {
        for player in data[key].as_array().into_iter().flatten() {
            let puuid = text(&player["puuid"]);
            entries.push((
                LivePlayer {
                    game_name: text(&player["gameName"]).trim().to_owned(),
                    tag_line: text(&player["tagLine"]),
                    champion_id: player["championId"].as_i64().unwrap_or(0),
                    team,
                    is_me: !me.is_empty() && puuid == me,
                    position: text(&player["selectedPosition"]).to_ascii_lowercase(),
                    ..LivePlayer::default()
                },
                puuid,
            ));
        }
    }
    // Modes whose session lists no teams (Arena): Riot's live game data has everyone.
    if entries.is_empty() {
        let players = live_client("/liveclientdata/playerlist")?;
        for player in players.as_array().into_iter().flatten() {
            let game_name = text(&player["riotIdGameName"]);
            let tag_line = text(&player["riotIdTagLine"]);
            let puuid = client
                .get_json(&format!(
                    "/lol-summoner/v1/alias/lookup?gameName={}&tagLine={}",
                    urlencoding::encode(&game_name),
                    urlencoding::encode(&tag_line)
                ))
                .map(|summoner| text(&summoner["puuid"]))
                .unwrap_or_default();
            entries.push((
                LivePlayer {
                    game_name,
                    tag_line,
                    team: if player["team"].as_str() == Some("CHAOS") { 2 } else { 1 },
                    is_me: !me.is_empty() && puuid == me,
                    ..LivePlayer::default()
                },
                puuid,
            ));
        }
    }
    if entries.is_empty() {
        return None;
    }
    // Ten lookups at once: each waits on Riot's servers through the client.
    std::thread::scope(|scope| {
        for (player, puuid) in entries.iter_mut() {
            scope.spawn(move || enrich(client, player, puuid));
        }
    });
    let players: Vec<LivePlayer> = entries.into_iter().map(|(player, _)| player).collect();
    let my_team = players.iter().find(|player| player.is_me).map(|player| player.team).unwrap_or(1);
    Some(LiveGame {
        game_id: data["gameId"].as_i64().unwrap_or(0),
        queue_id: data["queue"]["id"].as_i64().unwrap_or(0),
        game_mode: text(&data["queue"]["gameMode"]),
        my_team,
        players,
    })
}

// ---------------------------------------------------------------- live stats

/// One player's live numbers, as Riot's Live Client Data API reports them.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveStatsPlayer {
    pub game_name: String,
    pub tag_line: String,
    /// Champion display name (the API gives no id).
    pub champion: String,
    /// 1 = blue side (ORDER), 2 = red side (CHAOS).
    pub team: i64,
    pub position: String,
    pub level: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub cs: i64,
    pub vision: f64,
    /// Item ids in inventory order (trinket last).
    pub items: Vec<i64>,
    pub dead: bool,
    pub respawn: f64,
}

/// The match clock, every player's numbers and objectives per side.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LiveStats {
    pub game_time: f64,
    pub players: Vec<LiveStatsPlayer>,
    /// `[blue, red]` counts.
    pub dragons: [u32; 2],
    pub barons: [u32; 2],
    pub heralds: [u32; 2],
    pub towers: [u32; 2],
    pub inhibitors: [u32; 2],
}

fn side(team: &str) -> i64 {
    if team == "CHAOS" { 2 } else { 1 }
}

/// Live numbers for everyone (three small local requests), or `None` outside a match.
pub fn live_stats() -> Option<LiveStats> {
    let list = live_client("/liveclientdata/playerlist")?;
    let game_time = live_client("/liveclientdata/gamestats").and_then(|stats| stats["gameTime"].as_f64()).unwrap_or(0.0);
    let players: Vec<LiveStatsPlayer> = list
        .as_array()
        .into_iter()
        .flatten()
        .map(|player| {
            let scores = &player["scores"];
            let mut items: Vec<(i64, i64)> = player["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| Some((item["slot"].as_i64().unwrap_or(9), item["itemID"].as_i64()?)))
                .collect();
            items.sort_unstable();
            LiveStatsPlayer {
                game_name: text(&player["riotIdGameName"]),
                tag_line: text(&player["riotIdTagLine"]),
                champion: text(&player["championName"]),
                team: side(player["team"].as_str().unwrap_or_default()),
                position: text(&player["position"]).to_ascii_lowercase(),
                level: player["level"].as_i64().unwrap_or(0),
                kills: scores["kills"].as_i64().unwrap_or(0),
                deaths: scores["deaths"].as_i64().unwrap_or(0),
                assists: scores["assists"].as_i64().unwrap_or(0),
                cs: scores["creepScore"].as_i64().unwrap_or(0),
                vision: scores["wardScore"].as_f64().unwrap_or(0.0),
                items: items.into_iter().map(|(_, id)| id).collect(),
                dead: player["isDead"].as_bool().unwrap_or(false),
                respawn: player["respawnTimer"].as_f64().unwrap_or(0.0),
            }
        })
        .collect();
    let mut stats = LiveStats { game_time, players, ..LiveStats::default() };
    // Objectives: the killer's side, or for structures the side that lost them.
    let team_of = |name: &str| {
        list.as_array().into_iter().flatten().find_map(|player| {
            let game_name = player["riotIdGameName"].as_str().unwrap_or_default();
            let riot_id = format!("{game_name}#{}", player["riotIdTagLine"].as_str().unwrap_or_default());
            let matches = !name.is_empty()
                && (name.eq_ignore_ascii_case(game_name) || name.eq_ignore_ascii_case(&riot_id) || player["summonerName"].as_str() == Some(name));
            matches.then(|| side(player["team"].as_str().unwrap_or_default()))
        })
    };
    let index = |team: i64| usize::from(team == 2);
    let events = live_client("/liveclientdata/eventdata");
    for event in events.as_ref().and_then(|events| events["Events"].as_array()).into_iter().flatten() {
        let killer = event["KillerName"].as_str().unwrap_or_default();
        match event["EventName"].as_str().unwrap_or_default() {
            "DragonKill" => {
                if let Some(team) = team_of(killer) {
                    stats.dragons[index(team)] += 1;
                }
            }
            "BaronKill" => {
                if let Some(team) = team_of(killer) {
                    stats.barons[index(team)] += 1;
                }
            }
            "HeraldKill" => {
                if let Some(team) = team_of(killer) {
                    stats.heralds[index(team)] += 1;
                }
            }
            name @ ("TurretKilled" | "InhibKilled") => {
                // "Turret_T1_…" / "Barracks_T1_…": T1 belongs to blue, so red took it.
                let structure = event[name].as_str().unwrap_or_default();
                let taker = if structure.contains("_T1_") {
                    1
                } else if structure.contains("_T2_") {
                    0
                } else {
                    continue;
                };
                if name == "TurretKilled" {
                    stats.towers[taker] += 1;
                } else {
                    stats.inhibitors[taker] += 1;
                }
            }
            _ => {}
        }
    }
    Some(stats)
}
