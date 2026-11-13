//! Key moments of a recorded game, from Riot's Live Client Data API
//! (127.0.0.1:2999, local, published by Riot for this purpose). Polled about
//! every two seconds while a game is recorded. Event times are game seconds;
//! the offset between game time 0 and video time is measured from `gamestats`
//! (the smallest value seen, since a late answer only makes it larger).

use crate::livegame::live_client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Moment {
    /// kill, death, assist, multikill, firstBlood, dragon, baron, herald,
    /// grubs, tower, inhibitor, ace, gameEnd.
    pub kind: String,
    /// Seconds into the video.
    pub time: f64,
    /// Seconds into the game.
    pub game_time: f64,
    /// Kind-specific: multikill size, dragon type, Win/Loss, the champion killed.
    pub detail: String,
    /// For objectives: taken by the player's team (true) or the enemy (false).
    pub ally: Option<bool>,
    /// The player took part (every moment saved since objectives were filtered
    /// to the player's own; older files lack it on objectives of both teams).
    pub mine: bool,
}

/// What the recording learned about the game.
#[derive(Clone, Debug, Default)]
pub struct GameFacts {
    pub champion: String,
    /// "Win", "Loss" or empty (game left early).
    pub result: String,
    pub game_mode: String,
    /// Video seconds at game time 0.
    pub offset: Option<f64>,
    pub moments: Vec<Moment>,
}

/// Readings kept for the offset (one per poll, about every 2 s).
const SAMPLES: usize = 15;

pub struct MomentTracker {
    video_start: Instant,
    /// Lower-case game names that mean "me".
    me: Vec<String>,
    my_team: String,
    /// Lower-case game name → (team, champion).
    players: HashMap<String, (String, String)>,
    next_event: i64,
    /// Riot reported GameStart: game time readings are the in-game clock.
    started: bool,
    /// Recent video-minus-game-time readings (seconds).
    samples: Vec<f64>,
    pub facts: GameFacts,
}

/// "Name#TAG", "Name" → "name".
fn key(name: &str) -> String {
    name.split('#').next().unwrap_or_default().trim().to_lowercase()
}

impl MomentTracker {
    pub fn new(video_start: Instant) -> Self {
        Self { video_start, me: Vec::new(), my_team: String::new(), players: HashMap::new(), next_event: 0, started: false, samples: Vec::new(), facts: GameFacts::default() }
    }

    fn is_me(&self, name: &str) -> bool {
        let name = key(name);
        !name.is_empty() && self.me.contains(&name)
    }

    fn champion_of(&self, name: &str) -> String {
        self.players.get(&key(name)).map(|(_, champion)| champion.clone()).unwrap_or_default()
    }

    /// One round of requests (gamestats, players once, events).
    pub fn poll(&mut self) {
        let Some(events) = live_client("/liveclientdata/eventdata") else { return };
        let events = events["Events"].as_array().cloned().unwrap_or_default();
        // During the loading screen Riot's API already reports a running game time
        // that is not the in-game clock: only measure once the match has started.
        if !self.started {
            self.started = events.iter().any(|event| event["EventName"].as_str() == Some("GameStart"));
        }
        let asked = self.video_start.elapsed().as_secs_f64();
        let Some(stats) = live_client("/liveclientdata/gamestats") else { return };
        let answered = self.video_start.elapsed().as_secs_f64();
        if let Some(game_time) = stats["gameTime"].as_f64().filter(|time| self.started && *time > 0.0) {
            // The answer reflects some moment between asking and answering; a slow
            // answer only makes the gap larger, so the smallest recent one is kept.
            self.samples.push((asked + answered) / 2.0 - game_time);
            if self.samples.len() > SAMPLES {
                self.samples.remove(0);
            }
            self.facts.offset = self.samples.iter().copied().reduce(f64::min);
        }
        if self.facts.game_mode.is_empty() {
            self.facts.game_mode = stats["gameMode"].as_str().unwrap_or_default().to_owned();
        }
        if self.me.is_empty() || self.facts.champion.is_empty() {
            self.read_players();
        }
        let Some(offset) = self.facts.offset else { return };
        for event in &events {
            let id = event["EventID"].as_i64().unwrap_or(-1);
            if id < self.next_event {
                continue;
            }
            self.next_event = id + 1;
            self.handle(event, offset);
        }
    }

    fn read_players(&mut self) {
        if let Some(active) = live_client("/liveclientdata/activeplayer") {
            for field in ["riotId", "riotIdGameName", "summonerName"] {
                let name = key(active[field].as_str().unwrap_or_default());
                if !name.is_empty() && !self.me.contains(&name) {
                    self.me.push(name);
                }
            }
        }
        let Some(players) = live_client("/liveclientdata/playerlist") else { return };
        for player in players.as_array().into_iter().flatten() {
            let team = player["team"].as_str().unwrap_or_default().to_owned();
            let champion = player["championName"].as_str().unwrap_or_default().to_owned();
            let mut mine = false;
            for field in ["riotId", "riotIdGameName", "summonerName"] {
                let name = key(player[field].as_str().unwrap_or_default());
                if name.is_empty() {
                    continue;
                }
                mine |= self.me.contains(&name);
                self.players.insert(name, (team.clone(), champion.clone()));
            }
            if mine {
                self.my_team = team;
                self.facts.champion = champion;
            }
        }
    }

    fn push(&mut self, kind: &str, game_time: f64, offset: f64, detail: String, ally: Option<bool>) {
        self.facts.moments.push(Moment { kind: kind.to_owned(), time: (game_time + offset).max(0.0), game_time, detail, ally, mine: true });
    }

    /// First blood and multikills come as their own event next to the kill
    /// (same time): the kill moment becomes that one, so it shows once.
    fn upgrade_kill(&mut self, kind: &str, game_time: f64, offset: f64, detail: String) {
        let kill = self.facts.moments.iter_mut().rev().find(|moment| moment.kind == "kill" && (moment.game_time - game_time).abs() < 1.5);
        match kill {
            Some(kill) => {
                kill.kind = kind.to_owned();
                if kind == "multikill" {
                    kill.detail = detail;
                }
            }
            None => self.push(kind, game_time, offset, detail, None),
        }
    }

    /// A kill already shown as first blood or a multikill.
    fn kill_shown(&self, game_time: f64) -> bool {
        self.facts.moments.iter().any(|moment| matches!(moment.kind.as_str(), "firstBlood" | "multikill") && (moment.game_time - game_time).abs() < 1.5)
    }

    /// The player got the last hit or an assist.
    fn involved(&self, event: &Value) -> bool {
        self.is_me(event["KillerName"].as_str().unwrap_or_default())
            || event["Assisters"].as_array().into_iter().flatten().any(|name| self.is_me(name.as_str().unwrap_or_default()))
    }

    fn handle(&mut self, event: &Value, offset: f64) {
        let time = event["EventTime"].as_f64().unwrap_or(0.0);
        let text = |field: &str| event[field].as_str().unwrap_or_default().to_owned();
        match event["EventName"].as_str().unwrap_or_default() {
            "ChampionKill" => {
                let killer = text("KillerName");
                let victim = text("VictimName");
                if self.is_me(&killer) {
                    if !self.kill_shown(time) {
                        let champion = self.champion_of(&victim);
                        self.push("kill", time, offset, champion, None);
                    }
                } else if self.is_me(&victim) {
                    let champion = self.champion_of(&killer);
                    self.push("death", time, offset, champion, None);
                } else if event["Assisters"].as_array().into_iter().flatten().any(|name| self.is_me(name.as_str().unwrap_or_default())) {
                    let champion = self.champion_of(&victim);
                    self.push("assist", time, offset, champion, None);
                }
            }
            "Multikill" if self.is_me(&text("KillerName")) => {
                let streak = event["KillStreak"].as_i64().unwrap_or(2);
                // First blood wins when both land on the same kill.
                let first_blood = self.facts.moments.iter().any(|moment| moment.kind == "firstBlood" && (moment.game_time - time).abs() < 1.5);
                if !first_blood {
                    self.upgrade_kill("multikill", time, offset, streak.to_string());
                }
            }
            "FirstBlood" if self.is_me(&text("Recipient")) => self.upgrade_kill("firstBlood", time, offset, String::new()),
            // Objectives only when the player took part: last hit or assist.
            "DragonKill" if self.involved(event) => self.push("dragon", time, offset, text("DragonType"), Some(true)),
            "BaronKill" if self.involved(event) => self.push("baron", time, offset, String::new(), Some(true)),
            "HeraldKill" if self.involved(event) => self.push("herald", time, offset, String::new(), Some(true)),
            "HordeKill" if self.involved(event) => self.push("grubs", time, offset, String::new(), Some(true)),
            "TurretKilled" if self.involved(event) => self.push("tower", time, offset, String::new(), Some(true)),
            "InhibKilled" if self.involved(event) => self.push("inhibitor", time, offset, String::new(), Some(true)),
            // An ace counts when the player got the kill that finished it.
            "Ace" if self.is_me(&text("Acer")) => self.push("ace", time, offset, String::new(), Some(true)),
            "GameEnd" => {
                let result = if text("Result").eq_ignore_ascii_case("win") { "Win" } else { "Loss" };
                self.facts.result = result.to_owned();
                self.push("gameEnd", time, offset, result.to_owned(), None);
            }
            _ => {}
        }
    }
}
