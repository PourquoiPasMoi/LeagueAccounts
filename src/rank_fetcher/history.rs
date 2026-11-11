//! Season history from OP.GG's own server actions, the same ones its profile
//! page uses: `getTierGraph` (LP over the season, by day, week and month) and
//! `getGames` (the match list, 20 games per page with full stats). This works
//! for every account, whether or not it was ever signed in through the app.

use super::RankFetcher;
use crate::logging::Reason;
use crate::models::OpggLpPoint;
use crate::profiles::MatchRecord;
use html_escape::decode_html_entities;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Action ids by name, per OP.GG build (they change with every deploy).
static ACTIONS: Mutex<Option<(String, HashMap<String, String>)>> = Mutex::new(None);

/// What one history fetch brought back.
#[derive(Debug, Default)]
pub struct OpggHistory {
    pub lp: Vec<OpggLpPoint>,
    pub games: Vec<MatchRecord>,
    /// The match list ran out before the page limit (the whole history is in).
    pub reached_end: bool,
}

fn parse_time(value: &Value) -> Option<u64> {
    use time::format_description::well_known::Rfc3339;
    let at = time::OffsetDateTime::parse(value.as_str()?, &Rfc3339).ok()?;
    u64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok()
}

fn number(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .unwrap_or(0)
}

fn count(value: &Value) -> u32 {
    u32::try_from(number(value).max(0)).unwrap_or(0)
}

/// `[{created_at, tier_info: {tier, label: "P 4", lp}}]` as LP points.
pub fn lp_points(entries: &Value) -> Vec<OpggLpPoint> {
    entries
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let info = &entry["tier_info"];
            let tier = super::title_tier(info["tier"].as_str()?);
            if tier.is_empty() {
                return None;
            }
            let division = info["label"]
                .as_str()
                .and_then(|label| label.split_whitespace().nth(1))
                .and_then(|number| number.parse().ok())
                .map(super::roman)
                .unwrap_or_default();
            Some(OpggLpPoint {
                at: parse_time(&entry["created_at"])?,
                division: if super::is_apex(&tier) { String::new() } else { division },
                tier,
                lp: number(&info["lp"]),
            })
        })
        .collect()
}

/// OP.GG's position names as the client's.
fn position(value: &str) -> String {
    match value {
        "TOP" => "TOP",
        "JUNGLE" => "JUNGLE",
        "MID" | "MIDDLE" => "MIDDLE",
        "ADC" | "BOTTOM" => "BOTTOM",
        "SUPPORT" | "UTILITY" => "UTILITY",
        _ => "",
    }
    .to_owned()
}

/// One `getGames` entry as an archive record (the player's own stats).
pub fn game_record(game: &Value) -> Option<MatchRecord> {
    let created = parse_time(&game["created_at"])?;
    let side = game["summoner_team"].as_str().unwrap_or_default().to_ascii_uppercase();
    let mine = if side == "RED" { "team_red" } else { "team_blue" };
    let team = game[mine].as_array().cloned().unwrap_or_default();
    let participant_id = number(&game["participant_id"]);
    let puuid = game["puuid"].as_str().unwrap_or_default();
    let me = team
        .iter()
        .find(|player| number(&player["participant_id"]) == participant_id && participant_id > 0)
        .or_else(|| team.iter().find(|player| player["summoner"]["puuid"].as_str() == Some(puuid)))?;
    let stats = &me["stats"];
    let summary = &game["stats"];
    let result = game["game_result"].as_str().unwrap_or_default();
    let ids = |list: &Value| -> Vec<i64> {
        list.as_array()
            .into_iter()
            .flatten()
            .map(|entry| if entry.is_object() { number(&entry["id"]) } else { number(entry) })
            .filter(|id| *id > 0)
            .collect()
    };
    let team_kills = count(&summary["teamChampionKill"]).max(team.iter().map(|player| count(&player["stats"]["kill"])).sum());
    Some(MatchRecord {
        source: "opgg".into(),
        game_id: 0,
        created,
        duration_seconds: u64::try_from(number(&game["game_length"]).max(0)).unwrap_or(0),
        queue_id: crate::profiles::opgg_queue_id(game["game_type"]["game_type"].as_str().unwrap_or_default()),
        queue: game["game_type"]["game_type"].as_str().unwrap_or_default().to_owned(),
        mode: game["game_map"].as_str().unwrap_or_default().to_owned(),
        champion_id: number(&me["champion_id"]),
        champion: game["champion"]["name"].as_str().unwrap_or_default().to_owned(),
        win: result == "WIN",
        remake: result == "REMAKE" || game["is_remake"].as_bool().unwrap_or(false),
        kills: count(&stats["kill"]),
        deaths: count(&stats["death"]),
        assists: count(&stats["assist"]),
        cs: count(&stats["minion_kill"]) + count(&stats["neutral_minion_kill"]),
        champion_level: count(&stats["champion_level"]),
        gold: count(&stats["gold_earned"]),
        damage: count(&stats["total_damage_dealt_to_champions"]),
        damage_taken: count(&stats["total_damage_taken"]),
        damage_mitigated: count(&stats["damage_self_mitigated"]),
        healing: count(&stats["total_heal"]),
        vision: count(&stats["vision_score"]),
        wards_placed: count(&stats["ward_place"]),
        wards_killed: count(&stats["ward_kill"]),
        control_wards: count(&stats["vision_wards_bought_in_game"]),
        cc_seconds: count(&stats["time_ccing_others"]),
        turret_kills: count(&stats["turret_kill"]),
        team_kills,
        team_damage: team.iter().map(|player| count(&player["stats"]["total_damage_dealt_to_champions"])).sum(),
        position: position(game["position"].as_str().or_else(|| me["position"].as_str()).unwrap_or_default()),
        items: ids(&me["items"]),
        spells: ids(&me["spells"]),
        largest_multi_kill: count(&stats["largest_multi_kill"]),
        double_kills: 0,
        triple_kills: 0,
        quadra_kills: 0,
        penta_kills: 0,
        first_blood: false,
        op_score_x10: stats["op_score"].as_f64().map(|score| (score * 10.0).round().max(0.0) as u32),
        side: if side == "RED" { "red".into() } else if side == "BLUE" { "blue".into() } else { String::new() },
    })
}

/// The first Flight row whose JSON matches `pick`.
fn flight_row(body: &str, pick: impl Fn(&Value) -> bool) -> Option<Value> {
    body.lines().find_map(|line| {
        let (_, row) = line.split_once(':')?;
        let value: Value = serde_json::from_str(row).ok()?;
        pick(&value).then_some(value)
    })
}

impl RankFetcher {
    /// Ids of the named server actions, found in the page's script bundles.
    fn opgg_actions(&self, body: &str) -> Result<HashMap<String, String>, Reason> {
        static SCRIPTS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"<script[^>]+src="(https://c-lol-web\.op\.gg/[^"<>]+/_next/static/chunks/[^"<>]+\.js)""#).expect("valid regex")
        });
        static REFERENCES: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"createServerReference\)\("([a-f0-9]{40,})",.{0,160}?"(getTierGraph|getGames)"\)"#).expect("valid regex")
        });
        let scripts: Vec<String> = SCRIPTS.captures_iter(body).map(|capture| capture[1].to_owned()).collect();
        let build = scripts.first().and_then(|url| url.split("/_next/static/").next()).unwrap_or_default().to_owned();
        if let Ok(cache) = ACTIONS.lock() {
            if let Some((cached_build, actions)) = cache.as_ref() {
                if *cached_build == build && actions.len() == 2 {
                    return Ok(actions.clone());
                }
            }
        }
        let mut actions = HashMap::new();
        for url in scripts.iter().take(64) {
            let Ok(bundle) = self.request_text(self.client.get(url).timeout(Duration::from_secs(10))) else { continue };
            for capture in REFERENCES.captures_iter(&bundle) {
                actions.insert(capture[2].to_owned(), capture[1].to_owned());
            }
            if actions.len() == 2 {
                break;
            }
        }
        if actions.len() < 2 {
            return Err(Reason::ResponseBody);
        }
        if let Ok(mut cache) = ACTIONS.lock() {
            *cache = Some((build, actions.clone()));
        }
        Ok(actions)
    }

    fn call_action(&self, url: &str, action: &str, arguments: Value) -> Result<String, Reason> {
        self.request_text(
            self.client
                .post(url)
                .header("Next-Action", action)
                .header("Origin", "https://op.gg")
                .header("Accept", "text/x-component")
                .header("Content-Type", "text/plain;charset=UTF-8")
                .body(json!([arguments]).to_string())
                .timeout(Duration::from_secs(30)),
        )
    }

    /// LP history for the season and up to `max_pages` pages of games, newest
    /// first, stopping early at the first game `known` says is already stored.
    pub fn fetch_history(
        &self,
        region: &str,
        riot_id: &str,
        max_pages: usize,
        known: impl Fn(&MatchRecord) -> bool,
    ) -> Result<OpggHistory, Reason> {
        let url = self.build_opgg_url(region, riot_id);
        let body = self.fetch_page(&url)?;
        let decoded = decode_html_entities(&body).replace("\\\"", "\"");
        let puuid = super::renewal::ProfileUpdate::parse(&decoded).map(|profile| profile.puuid).ok_or(Reason::ProfileMissing)?;
        let actions = self.opgg_actions(&body)?;
        let mut history = OpggHistory::default();
        for period in ["MONTH", "WEEK", "DAY"] {
            let Ok(response) = self.call_action(&url, &actions["getTierGraph"], json!({"puuid": puuid, "region": region, "lpHistoryType": period})) else { continue };
            if let Some(entries) = flight_row(&response, |value| value.as_array().is_some_and(|list| list.iter().any(|entry| entry.get("tier_info").is_some()))) {
                history.lp.extend(lp_points(&entries));
            }
        }
        let mut ended_at = String::new();
        for _ in 0..max_pages {
            let response = self.call_action(
                &url,
                &actions["getGames"],
                json!({"locale": "en_US", "region": region, "puuid": puuid, "gameType": "TOTAL", "endedAt": ended_at, "champion": ""}),
            )?;
            let Some(page) = flight_row(&response, |value| value.get("data").is_some_and(Value::is_array)) else { break };
            let games = page["data"].as_array().cloned().unwrap_or_default();
            if games.is_empty() {
                history.reached_end = true;
                break;
            }
            let records: Vec<MatchRecord> = games.iter().filter_map(game_record).collect();
            let caught_up = records.iter().any(&known);
            history.games.extend(records);
            if caught_up {
                break;
            }
            if games.len() < 20 {
                history.reached_end = true;
                break;
            }
            match page["meta"]["last_game_created_at"].as_str() {
                Some(next) if next != ended_at => ended_at = next.to_owned(),
                _ => break,
            }
        }
        Ok(history)
    }
}
