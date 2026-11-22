//! Auto accept (off by default, Settings › Login): when League finds a match,
//! the ready check is accepted through the League client's own local API, the
//! same call its Accept button makes. Nothing touches the game. A light
//! watcher asks the client for its phase once a second while the switch is
//! on, and does nothing at all while it is off.

use crate::views::{fail, CommandResult};
use leagueaccounts::riot_client;
use leagueaccounts::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// Longest wait before accepting (the ready check itself lasts about 10 s).
const MAX_DELAY: u32 = 6;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub enabled: bool,
    /// Seconds between the match being found and accepting it: time to
    /// decline by hand.
    pub delay_seconds: u32,
}

fn prefs_path() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("autoaccept.json"))
}

static PREFS: Mutex<Option<Prefs>> = Mutex::new(None);

fn prefs() -> Prefs {
    let mut cached = PREFS.lock().unwrap_or_else(|error| error.into_inner());
    *cached.get_or_insert_with(|| {
        let mut prefs: Prefs = prefs_path().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        prefs.delay_seconds = prefs.delay_seconds.min(MAX_DELAY);
        prefs
    })
}

/// The ready check as the client reports it: (still waiting for this player's
/// answer, seconds it has been shown).
fn ready_check(client: &riot_client::LeagueApi) -> Option<(bool, f64)> {
    let check = client.get_json("/lol-matchmaking/v1/ready-check")?;
    let waiting = check["state"].as_str() == Some("InProgress") && check["playerResponse"].as_str() == Some("None");
    Some((waiting, check["timer"].as_f64().unwrap_or(0.0)))
}

pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        // When this ready check was first seen (one match found = one check).
        let mut seen: Option<Instant> = None;
        loop {
            let prefs = prefs();
            if !prefs.enabled {
                seen = None;
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            let Some(client) = riot_client::league_client() else {
                // No client: look again a little later.
                seen = None;
                thread::sleep(Duration::from_secs(5));
                continue;
            };
            let phase = client.get_json("/lol-gameflow/v1/gameflow-phase");
            if phase.as_ref().and_then(|phase| phase.as_str()) != Some("ReadyCheck") {
                seen = None;
                thread::sleep(Duration::from_secs(1));
                continue;
            }
            let since = *seen.get_or_insert_with(Instant::now);
            // Declined or accepted by hand meanwhile: leave it.
            if let Some((true, shown)) = ready_check(&client) {
                let waited = since.elapsed().as_secs_f64().max(shown);
                if waited >= f64::from(prefs.delay_seconds) {
                    let accepted = client.send("POST", "/lol-matchmaking/v1/ready-check/accept", Some(&serde_json::json!({}))).is_some_and(|(status, _)| (200..300).contains(&status));
                    if accepted {
                        let _ = app.emit("match-accepted", ());
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

#[tauri::command]
pub async fn auto_accept_prefs() -> CommandResult<Prefs> {
    Ok(prefs())
}

#[tauri::command]
pub async fn set_auto_accept_prefs(enabled: bool, delay_seconds: u32) -> CommandResult<Prefs> {
    let prefs = Prefs { enabled, delay_seconds: delay_seconds.min(MAX_DELAY) };
    let path = prefs_path().ok_or_else(|| fail("file_write"))?;
    let body = serde_json::to_vec_pretty(&prefs).map_err(|_| fail("file_write"))?;
    std::fs::write(path, body).map_err(|_| fail("file_write"))?;
    *PREFS.lock().map_err(|_| fail("operation_failed"))? = Some(prefs);
    Ok(prefs)
}
