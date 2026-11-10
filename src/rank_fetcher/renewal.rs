//! OP.GG's public Update button currently uses Next.js server actions.
//! Discover references from the page's own build; never pin action hashes.
use super::{RankFetcher, RefreshMode};
use crate::models::{Freshness, RankFetchError};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Single freshness threshold for Manual and Auto; the UI ages it via `fresh_until`.
pub const MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// Wall-clock budget for one renewal exchange: shared-reference discovery plus
/// the POST/poll exchange. The permit wait is budgeted separately by
/// `PERMIT_WAIT`, so a queued renewal never eats another one's budget.
pub const RENEWAL_TIMEOUT: Duration = Duration::from_secs(20);
/// Waiting for a permit has its own budget so it never eats the renewal's.
const PERMIT_WAIT: Duration = Duration::from_secs(40);
pub const MAX_CONCURRENT_RENEWALS: usize = 2;
const FAILED_RETRY_DELAY: Duration = Duration::from_secs(60);
const ACTION_CACHE_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// Cap for any rate-limit cooldown, including provider-supplied `Retry-After`
/// values. 15 minutes covers OP.GG's normal bursts without ever suspending
/// renewals for hours on a malformed or absurd header.
const MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);
const ASSET_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct RenewalState {
    active: Mutex<usize>,
    available: Condvar,
    actions: Mutex<Option<CachedActions>>,
    attempts: Mutex<HashMap<String, Instant>>,
    cooldown: Mutex<Option<Instant>>,
}

#[derive(Clone, PartialEq, Eq)]
struct CachedActions {
    build: String,
    /// The asset that supplied both references. Other profile chunks can differ.
    script_url: Option<String>,
    result: Result<Actions, RankFetchError>,
    at: Instant,
}

struct Permit(Arc<RenewalState>);
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active = active.saturating_sub(1);
            self.0.available.notify_one();
        }
    }
}

impl RenewalState {
    fn invalidate_action(&self, action: &str) {
        if let Ok(mut cache) = self.actions.lock() {
            // A late response from an old worker must not erase newer references.
            if cache.as_ref().is_some_and(|cached| {
                cached
                    .result
                    .as_ref()
                    .is_ok_and(|actions| action == actions.renewal || action == actions.status)
            }) {
                *cache = None;
            }
        }
    }

    pub(super) fn rate_limited(&self) -> bool {
        self.cooldown
            .lock()
            .ok()
            .and_then(|value| *value)
            .is_some_and(|until| until > Instant::now())
    }

    /// A provider `Retry-After` is honored up to [`MAX_COOLDOWN`], so a
    /// malformed or absurd header can never suspend renewal indefinitely.
    pub(super) fn set_cooldown(&self, seconds: Option<u64>) {
        let wait = cooldown(seconds);
        if let Ok(mut until) = self.cooldown.lock() {
            let next = Instant::now() + wait;
            *until = Some(until.map_or(next, |previous| previous.max(next)));
        }
    }

    /// Atomically reserve this profile's retry slot. Only call this when the
    /// renewal POST is about to be sent: a pre-flight failure is not an attempt.
    fn admit_attempt(&self, url: &str) -> Result<(), RankFetchError> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?;
        attempts.retain(|_, at| at.elapsed() < FAILED_RETRY_DELAY);
        if attempts.contains_key(url) {
            return Err(RankFetchError::RenewalFailed);
        }
        attempts.insert(url.to_owned(), Instant::now());
        Ok(())
    }

    fn acquire(self: &Arc<Self>, deadline: Instant) -> Result<Permit, RankFetchError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?;
        while *active >= MAX_CONCURRENT_RENEWALS {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(RankFetchError::Timeout)?;
            let (next, result) = self
                .available
                .wait_timeout(active, remaining)
                .map_err(|_| RankFetchError::ProviderUnavailable)?;
            active = next;
            if result.timed_out() {
                return Err(RankFetchError::Timeout);
            }
        }
        *active += 1;
        Ok(Permit(Arc::clone(self)))
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Honor a provider wait up to [`MAX_COOLDOWN`]; a missing value uses the
/// burst default. The cap is the single place that bounds every cooldown.
fn cooldown(seconds: Option<u64>) -> Duration {
    let requested = Duration::from_secs(seconds.unwrap_or(DEFAULT_COOLDOWN.as_secs()).max(1));
    requested.min(MAX_COOLDOWN)
}

pub(super) fn retry_after(value: &str) -> Option<u64> {
    value.parse().ok().or_else(|| {
        let date =
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc2822)
                .ok()?;
        let at = u64::try_from(date.unix_timestamp()).ok()?;
        Some(at.saturating_sub(now_ms() / 1000).max(1))
    })
}

pub(super) fn timestamp(value: &Value) -> Option<u64> {
    let parsed = time::OffsetDateTime::parse(
        value.as_str()?,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    u64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

#[derive(Debug)]
pub(super) struct ProfileUpdate {
    pub puuid: String,
    pub updated_at: Option<u64>,
    pub renewable_at: Option<u64>,
}

impl ProfileUpdate {
    pub fn parse(payload: &str) -> Option<Self> {
        let marker = payload.find("\"initRenewableAt\"")?;
        let start = payload[..marker].rfind('{')?;
        let data = serde_json::Deserializer::from_str(&payload[start..])
            .into_iter::<Value>()
            .next()?
            .ok()?;
        Some(Self {
            puuid: data["puuid"].as_str()?.to_owned(),
            updated_at: timestamp(&data["initUpdatedAt"]),
            renewable_at: timestamp(&data["initRenewableAt"]),
        })
    }

    pub fn should_renew(&self, mode: RefreshMode, now: u64) -> bool {
        self.renewable_at.is_none_or(|at| at <= now)
            && (mode == RefreshMode::Force || freshness(self.updated_at, now) != Freshness::Fresh)
    }
}

pub(super) fn freshness(updated: Option<u64>, now: u64) -> Freshness {
    match updated {
        Some(at) if at <= now + 30_000 && now.saturating_sub(at) <= MAX_AGE.as_millis() as u64 => {
            Freshness::Fresh
        }
        Some(_) => Freshness::Stale,
        None => Freshness::Unknown,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Actions {
    renewal: String,
    status: String,
}

impl Actions {
    fn parse(bundle: &str) -> Option<Self> {
        static REFERENCES: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
            r#"createServerReference\)\("([a-f0-9]{40,})",.{0,160}?"(renewalStatus|renewal)"\)"#
        ).expect("valid action reference regex")
        });
        let mut renewal = None;
        let mut status = None;
        for capture in REFERENCES.captures_iter(bundle) {
            match &capture[2] {
                "renewal" => renewal = Some(capture[1].to_owned()),
                "renewalStatus" => status = Some(capture[1].to_owned()),
                _ => {}
            }
        }
        Some(Self {
            renewal: renewal?,
            status: status?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RenewalResponse {
    Pending(Duration),
    Finished(u64),
}

fn parse_response(body: &str) -> Result<RenewalResponse, RankFetchError> {
    // A Flight stream can have other rows before and after the action result.
    for line in body.lines() {
        let Some((_, row)) = line.split_once(':') else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        let Some(status) = data["status"].as_str() else {
            continue;
        };
        return match status {
            "RENEWING" => Ok(RenewalResponse::Pending(Duration::from_millis(
                data["delay"].as_u64().unwrap_or(1000).clamp(500, 3000),
            ))),
            "RENEWAL_FINISH" => timestamp(&data["lastUpdatedAt"])
                .map(RenewalResponse::Finished)
                .ok_or(RankFetchError::ParsingFailed),
            "TOO_MANY_RENEWALS" => Err(RankFetchError::RateLimited),
            _ => Err(RankFetchError::RenewalFailed),
        };
    }
    Err(RankFetchError::ParsingFailed)
}

/// Pre-flight failures mean no renewal request reached OP.GG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenewalError {
    NotSent(RankFetchError),
    Failed(RankFetchError),
}

fn remaining(deadline: Instant) -> Result<Duration, RankFetchError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(RankFetchError::Timeout)
}

impl RankFetcher {
    fn discover_actions(&self, body: &str, deadline: Instant) -> Result<Actions, RankFetchError> {
        self.discover_actions_with(body, deadline, |url, timeout| {
            self.request_text(self.client.get(url).timeout(timeout))
                .map_err(super::fetch_error)
        })
    }

    fn discover_actions_with(
        &self,
        body: &str,
        deadline: Instant,
        mut fetch_bundle: impl FnMut(&str, Duration) -> Result<String, RankFetchError>,
    ) -> Result<Actions, RankFetchError> {
        static SCRIPTS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
            r#"<script[^>]+src="(https://c-lol-web\.op\.gg/[^"<>]+/_next/static/chunks/[^"<>]+\.js)""#
        ).expect("valid OP.GG scripts regex")
        });
        let scripts: Vec<String> = SCRIPTS
            .captures_iter(body)
            .map(|c| c[1].to_owned())
            .collect();
        if scripts.is_empty() || scripts.len() > 64 {
            return Err(RankFetchError::ParsingFailed);
        }
        // Share valid references across profiles, but rediscover on action-asset
        // rotation or expiry even if the release prefix stays the same.
        let build = scripts[0]
            .split("/_next/static/")
            .next()
            .unwrap_or_default()
            .to_owned();
        // Snapshot the shared references, then drop the lock: another profile
        // must be able to read or refresh them while this one fetches assets.
        let snapshot = self
            .renewal
            .actions
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?
            .clone();
        if let Some(cached) = &snapshot {
            let reusable = if cached.result.is_ok() {
                cached.at.elapsed() < ACTION_CACHE_MAX_AGE
                    && cached
                        .script_url
                        .as_ref()
                        .is_some_and(|url| scripts.contains(url))
            } else {
                cached.at.elapsed() < FAILED_RETRY_DELAY
            };
            if cached.build == build && reusable {
                return cached.result.clone();
            }
        }
        let mut script_url = None;
        let result = (|| {
            for url in scripts.iter().rev() {
                let timeout = remaining(deadline)?.min(ASSET_TIMEOUT);
                let bundle = fetch_bundle(url, timeout)?;
                if let Some(actions) = Actions::parse(&bundle) {
                    script_url = Some(url.clone());
                    return Ok(actions);
                }
            }
            Err(RankFetchError::ParsingFailed)
        })();
        let mut cache = self
            .renewal
            .actions
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?;
        // Commit only when no other worker replaced the cache meanwhile: a slow
        // round must not overwrite newer references or a fresh invalidation.
        if *cache == snapshot {
            *cache = Some(CachedActions {
                build,
                script_url,
                result: result.clone(),
                at: Instant::now(),
            });
        }
        result
    }

    fn call_update(
        &self,
        url: &str,
        action: &str,
        arguments: Value,
        deadline: Instant,
    ) -> Result<RenewalResponse, RankFetchError> {
        let result = self
            .request_text(
                self.client
                    .post(url)
                    .header("Next-Action", action)
                    .header("Origin", "https://op.gg")
                    .header("Accept", "text/x-component")
                    .header("Content-Type", "text/plain;charset=UTF-8")
                    .body(json!([arguments]).to_string())
                    .timeout(remaining(deadline)?),
            )
            .map_err(super::fetch_error)
            .and_then(|body| parse_response(&body));
        match result {
            Err(RankFetchError::RateLimited) => self.renewal.set_cooldown(None),
            // A missing action or an unrecognized Flight result can indicate
            // rotated references. Refresh discovery on the next allowed attempt;
            // never replay a POST after an ambiguous timeout/server failure.
            Err(RankFetchError::ProfileNotFound) => {
                self.renewal.invalidate_action(action);
                // The profile page itself was just parsed; a 404 here is the
                // action contract, not a missing profile.
                return Err(RankFetchError::RenewalFailed);
            }
            Err(RankFetchError::ParsingFailed) => self.renewal.invalidate_action(action),
            _ => {}
        }
        result
    }

    /// Renew one profile standing.
    ///
    /// Everything inside is bounded by [`RENEWAL_TIMEOUT`], which starts after
    /// the permit wait and the rate-limit check and covers both reference
    /// discovery and the POST/poll exchange. Pre-flight failures (permit, cooldown,
    /// discovery) report [`RenewalError::NotSent`] and do not consume this
    /// profile's retry slot, so a later normal request can still proceed.
    pub(super) fn renew_profile(
        &self,
        url: &str,
        region: &str,
        body: &str,
        profile: &ProfileUpdate,
    ) -> Result<u64, RenewalError> {
        let _permit = self
            .renewal
            .acquire(Instant::now() + PERMIT_WAIT)
            .map_err(RenewalError::NotSent)?;
        if self.renewal.rate_limited() {
            return Err(RenewalError::NotSent(RankFetchError::RateLimited));
        }
        let deadline = Instant::now() + RENEWAL_TIMEOUT;
        let actions = self
            .discover_actions(body, deadline)
            .map_err(RenewalError::NotSent)?;
        // Admit only now that the POST is one step away: this single check+insert
        // keeps concurrent renewals of the same profile from double-posting, and
        // a failure after admission is reported as a real attempt. Nothing is
        // ever replayed.
        self.renewal
            .admit_attempt(url)
            .map_err(RenewalError::NotSent)?;
        self.poll_renewal(url, region, profile, &actions, deadline)
            .map_err(RenewalError::Failed)
    }

    fn poll_renewal(
        &self,
        url: &str,
        region: &str,
        profile: &ProfileUpdate,
        actions: &Actions,
        deadline: Instant,
    ) -> Result<u64, RankFetchError> {
        let mut status = self.call_update(
            url,
            &actions.renewal,
            json!({"region":region,"puuid":profile.puuid,"isPremiumPrimary":false}),
            deadline,
        )?;
        let mut backoff = Duration::from_millis(500);
        loop {
            match status {
                RenewalResponse::Finished(at) => {
                    // FINISH can also mean "still in cooldown". The caller verifies
                    // the newly fetched page against this timestamp and its age.
                    return Ok(at);
                }
                RenewalResponse::Pending(delay) => {
                    let wait = delay.max(backoff);
                    if wait >= remaining(deadline)? {
                        return Err(RankFetchError::Timeout);
                    }
                    std::thread::sleep(wait);
                    status = self.call_update(
                        url,
                        &actions.status,
                        json!({"region":region,"puuid":profile.puuid}),
                        deadline,
                    )?;
                    backoff = (backoff * 2).min(Duration::from_secs(3));
                }
            }
        }
    }
}
