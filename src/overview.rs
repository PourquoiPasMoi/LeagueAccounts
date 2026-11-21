//! All accounts at a glance: LP won or lost today and this week, games
//! played, LP decay countdowns and the champions played across every account.
//! Everything comes from what is already on disk (rank history, match archive,
//! profile snapshots): nothing is fetched.

use crate::models::{Account, AccountKey};
use crate::profiles;
use serde::Serialize;
use std::collections::HashMap;

const DAY_MS: u64 = 86_400_000;

/// LP decay of one account's ranked queue (Diamond and above).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Decay {
    pub account_id: String,
    pub region: String,
    /// `RANKED_SOLO_5x5` or `RANKED_FLEX_SR`.
    pub queue: String,
    pub tier: String,
    /// Riot's own count (`warnings.daysUntilDecay`) when the client gave one.
    pub days: Option<i64>,
    /// Unix ms of the snapshot that holds `days`.
    pub at: u64,
    /// Unix ms of the last ranked game of this queue (client or OP.GG history); 0 = none known.
    pub last_game: u64,
    /// Days of decay protection a full bank holds at this tier (28 Diamond, 14 Master and above).
    pub bank: i64,
}

impl Decay {
    /// Days left before LP is lost, counted from now.
    pub fn left(&self, now: u64) -> i64 {
        let since = |time: u64| (now.saturating_sub(time) / DAY_MS) as i64;
        match self.days {
            Some(days) => days - since(self.at),
            None => self.bank - since(self.last_game),
        }
        .max(0)
    }
}

/// Decay countdowns: Riot's number when the client reported one, else an
/// estimate from the last ranked game of the queue (client first, OP.GG games
/// in the same archive otherwise).
pub fn decay(accounts: &[Account]) -> Vec<Decay> {
    let mut list = Vec::new();
    for (key, profile) in profiles::load_all(accounts) {
        let account_tier = accounts.iter().find(|account| account.account_id == key.account_id && account.region == key.region).map(|account| account.tier.clone()).unwrap_or_default();
        let games = profiles::archive(&key);
        for (queue, queue_id, opgg) in [("RANKED_SOLO_5x5", 420, "SOLORANKED"), ("RANKED_FLEX_SR", 440, "FLEXRANKED")] {
            let ranked = profile.ranked.iter().find(|entry| entry.queue == queue);
            let tier = ranked.map(|entry| entry.tier.clone()).filter(|tier| !tier.is_empty() && tier != "Unranked").unwrap_or_else(|| if queue_id == 420 { account_tier.clone() } else { String::new() });
            let bank = match tier.to_ascii_lowercase().as_str() {
                "diamond" => 28,
                "master" | "grandmaster" | "challenger" => 14,
                _ => continue,
            };
            let last_game = games.iter().filter(|game| game.queue_id == queue_id || game.queue == opgg).map(|game| game.created).max().unwrap_or(0);
            let days = ranked.and_then(|entry| entry.decay_days);
            if days.is_none() && last_game == 0 {
                continue;
            }
            list.push(Decay { account_id: key.account_id.clone(), region: key.region.clone(), queue: queue.into(), tier, days, at: profile.synced_at, last_game, bank });
        }
    }
    list
}

/// A standing as one number, to subtract two of them: 100 a division, 400 a
/// tier, Master and above continue with their LP.
fn ladder(tier: &str, division: &str, lp: i64) -> Option<i64> {
    let tier = match tier.to_ascii_lowercase().as_str() {
        "iron" => 0,
        "bronze" => 1,
        "silver" => 2,
        "gold" => 3,
        "platinum" => 4,
        "emerald" => 5,
        "diamond" => 6,
        "master" | "grandmaster" | "challenger" => return Some(2800 + lp),
        _ => return None,
    };
    let division = match division.trim().to_ascii_uppercase().as_str() {
        "IV" | "4" => 0,
        "III" | "3" => 1,
        "II" | "2" => 2,
        "I" | "1" => 3,
        _ => 0,
    };
    Some(tier * 400 + division * 100 + lp)
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountOverview {
    pub account_id: String,
    pub region: String,
    pub name: String,
    pub region_display: String,
    pub tier: String,
    pub division: String,
    pub lp: String,
    /// Solo/Duo LP won (or lost) since the start of today / of the week;
    /// `None` when the history does not reach back that far or nothing is ranked.
    pub lp_day: Option<i64>,
    pub lp_week: Option<i64>,
    pub games_day: u32,
    pub wins_day: u32,
    pub games_week: u32,
    pub wins_week: u32,
    /// Unix ms of the last game in the archive (0 = none).
    pub last_game: u64,
    /// Soonest LP decay in days and its queue; `None` below Diamond.
    pub decay_left: Option<i64>,
    pub decay_queue: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChampionOverview {
    pub champion_id: i64,
    /// Display name when the archive has one (OP.GG games).
    pub champion: String,
    pub games: u32,
    pub wins: u32,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    /// Accounts it was played on.
    pub accounts: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub accounts: Vec<AccountOverview>,
    /// Most played first.
    pub champions: Vec<ChampionOverview>,
    /// Games counted in `champions`.
    pub pool_games: u32,
}

/// `day_start` / `week_start`: Unix ms of the start of today and of the last
/// seven days (the interface knows the local time). `pool_since`: champions
/// are counted from games after it (0 = every game kept).
pub fn build(accounts: &[Account], day_start: u64, week_start: u64, pool_since: u64, now: u64) -> Overview {
    let decays = decay(accounts);
    let mut pool: HashMap<i64, (ChampionOverview, Vec<usize>)> = HashMap::new();
    let mut pool_games = 0;
    let mut rows = Vec::with_capacity(accounts.len());
    for (index, account) in accounts.iter().enumerate() {
        let key: AccountKey = account.key();
        let history: Vec<_> = profiles::history(&key).into_iter().filter(|point| point.mode == "lol" || point.mode.is_empty()).collect();
        let current = history.last().and_then(|point| ladder(&point.tier, &point.division, point.lp)).or_else(|| ladder(&account.tier, &account.division, account.lp.trim().parse().unwrap_or(0)));
        // The standing when the period began: the last point before it. A
        // history that starts inside the period counts from its first point.
        let change = |start: u64| {
            let current = current?;
            let before = history.iter().rev().find(|point| point.at < start).or_else(|| history.iter().find(|point| point.at >= start))?;
            Some(current - ladder(&before.tier, &before.division, before.lp)?)
        };
        let games = profiles::archive(&key);
        let mut row = AccountOverview {
            account_id: account.account_id.clone(),
            region: account.region.clone(),
            name: account.name.clone(),
            region_display: account.region_display.clone(),
            tier: account.tier.clone(),
            division: account.division.clone(),
            lp: account.lp.clone(),
            lp_day: change(day_start),
            lp_week: change(week_start),
            ..AccountOverview::default()
        };
        for game in games.iter().filter(|game| !game.remake) {
            row.last_game = row.last_game.max(game.created);
            if game.created >= week_start {
                row.games_week += 1;
                row.wins_week += u32::from(game.win);
            }
            if game.created >= day_start {
                row.games_day += 1;
                row.wins_day += u32::from(game.win);
            }
            if game.created >= pool_since && game.champion_id > 0 {
                pool_games += 1;
                let (champion, played_on) = pool.entry(game.champion_id).or_insert_with(|| (ChampionOverview { champion_id: game.champion_id, ..ChampionOverview::default() }, Vec::new()));
                if champion.champion.is_empty() {
                    champion.champion = game.champion.clone();
                }
                champion.games += 1;
                champion.wins += u32::from(game.win);
                champion.kills += game.kills;
                champion.deaths += game.deaths;
                champion.assists += game.assists;
                if !played_on.contains(&index) {
                    played_on.push(index);
                }
            }
        }
        if let Some(soonest) = decays.iter().filter(|decay| decay.account_id == account.account_id && decay.region == account.region).min_by_key(|decay| decay.left(now)) {
            row.decay_left = Some(soonest.left(now));
            row.decay_queue = soonest.queue.clone();
        }
        rows.push(row);
    }
    let mut champions: Vec<ChampionOverview> = pool
        .into_values()
        .map(|(mut champion, played_on)| {
            champion.accounts = played_on.len() as u32;
            champion
        })
        .collect();
    champions.sort_by(|a, b| b.games.cmp(&a.games).then(b.wins.cmp(&a.wins)).then(a.champion_id.cmp(&b.champion_id)));
    champions.truncate(40);
    Overview { accounts: rows, champions, pool_games }
}
