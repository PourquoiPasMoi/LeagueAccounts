//! Windows notifications: LP decay coming up, a suspension that ended, a
//! recording saved (or rescued after a crash), a backup that failed. Each kind
//! has its switch in Settings; a light check runs once an hour (disk only).

use crate::refresh::Shared;
use crate::views::{fail, CommandResult};
use leagueaccounts::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::AppHandle;

pub enum Kind {
    /// Recordings rescued after a crash.
    Recovered(usize),
    /// A game was recorded (its name).
    RecordingSaved(String),
    Decay { account: String, days: i64 },
    BanEnded(String),
    BackupFailed,
    Test,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Prefs {
    decay: bool,
    ban: bool,
    recording: bool,
    /// Decay warnings already shown: account and queue → the step shown (3 or 1 days).
    sent: HashMap<String, i64>,
    /// Suspensions seen while they ran: account → Unix ms of their end.
    bans: HashMap<String, u64>,
    /// Day (Unix days) a failed backup was last reported.
    backup_failed_day: u64,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { decay: true, ban: true, recording: true, sent: HashMap::new(), bans: HashMap::new(), backup_failed_day: 0 }
    }
}

fn prefs_path() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("notify.json"))
}

static PREFS: Mutex<Option<Prefs>> = Mutex::new(None);

fn prefs() -> Prefs {
    let mut cached = PREFS.lock().unwrap_or_else(|error| error.into_inner());
    cached.get_or_insert_with(|| prefs_path().and_then(|path| std::fs::read(path).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()).clone()
}

fn save(prefs: &Prefs) {
    if let Ok(mut cached) = PREFS.lock() {
        *cached = Some(prefs.clone());
    }
    if let (Some(path), Ok(body)) = (prefs_path(), serde_json::to_vec_pretty(prefs)) {
        let _ = std::fs::write(path, body);
    }
}

/// The notification texts in the interface language (the page sends them at
/// start and when the language changes); English until then.
static TEXTS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn text(key: &str, values: &[(&str, &str)]) -> String {
    let fallback = match key {
        "decayTitle" => "LP decay soon",
        "decayBody" => "{account}: {days} days left before LP decay. Play a ranked game to reset it.",
        "decayNowBody" => "{account}: LP decay has started. Play a ranked game to stop it.",
        "banTitle" => "Suspension ended",
        "banBody" => "{account} can play again.",
        "recordingTitle" => "Recording saved",
        "recoveredTitle" => "Recording recovered",
        "recoveredBody" => "{count} recording interrupted by a crash was repaired and is in your library.",
        "backupFailedTitle" => "Backup failed",
        "backupFailedBody" => "The accounts backup could not be written. Check the folder in Settings.",
        "testTitle" => "Notifications work",
        "testBody" => "This is how League Companion will tell you things.",
        _ => "",
    };
    let mut result = TEXTS.lock().ok().and_then(|texts| texts.as_ref().and_then(|texts| texts.get(key).cloned())).filter(|text| !text.is_empty()).unwrap_or_else(|| fallback.to_owned());
    for (name, value) in values {
        result = result.replace(&format!("{{{name}}}"), value);
    }
    result
}

/// The app's picture for Windows, written once to the cache folder.
fn icon() -> Option<PathBuf> {
    let path = app_data_dir().ok()?.join("cache").join("toast-icon.png");
    if !path.is_file() {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, include_bytes!("../icons/128x128.png")).ok()?;
    }
    Some(path)
}

pub fn send(app: &AppHandle, kind: Kind) {
    let prefs = prefs();
    let (title, body) = match &kind {
        Kind::Recovered(count) => (text("recoveredTitle", &[]), text("recoveredBody", &[("count", &count.to_string())])),
        Kind::RecordingSaved(_) if !prefs.recording => return,
        Kind::RecordingSaved(name) => (text("recordingTitle", &[]), name.clone()),
        Kind::Decay { .. } if !prefs.decay => return,
        Kind::Decay { account, days } if *days <= 0 => (text("decayTitle", &[]), text("decayNowBody", &[("account", account)])),
        Kind::Decay { account, days } => (text("decayTitle", &[]), text("decayBody", &[("account", account), ("days", &days.to_string())])),
        Kind::BanEnded(_) if !prefs.ban => return,
        Kind::BanEnded(account) => (text("banTitle", &[]), text("banBody", &[("account", account)])),
        Kind::BackupFailed => (text("backupFailedTitle", &[]), text("backupFailedBody", &[])),
        Kind::Test => (text("testTitle", &[]), text("testBody", &[])),
    };
    let app = app.clone();
    // Its own thread: Windows' notification service can take a moment.
    thread::spawn(move || {
        let _ = leagueaccounts::toast::show(&title, &body, icon().as_deref(), move || crate::show_main(&app));
    });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

/// Decay warnings, ended suspensions and the automatic backup. Reads files only.
fn check(app: &AppHandle, shared: &Shared) {
    let Some(accounts) = shared.manager.lock().ok().map(|manager| manager.accounts.clone()) else { return };
    let mut state = prefs();
    let before = (state.sent.clone(), state.bans.clone(), state.backup_failed_day);
    let now = now_ms();
    let label = |account_id: &str, region: &str| {
        accounts
            .iter()
            .find(|account| account.account_id == account_id && account.region == region)
            .map(|account| if account.name.is_empty() { account.account_id.clone() } else { account.name.clone() })
            .unwrap_or_default()
    };

    if state.decay {
        let decays = leagueaccounts::overview::decay(&accounts);
        let mut live = Vec::new();
        for decay in &decays {
            let key = format!("{}:{}:{}", decay.region, decay.account_id, decay.queue);
            let left = decay.left(now);
            if left > 3 {
                continue;
            }
            live.push(key.clone());
            // Once at three days, once more on the last day.
            let step = if left <= 1 { 1 } else { 3 };
            if state.sent.get(&key).is_none_or(|shown| *shown > step) {
                state.sent.insert(key, step);
                send(app, Kind::Decay { account: label(&decay.account_id, &decay.region), days: left });
            }
        }
        state.sent.retain(|key, _| live.contains(key));
    }

    for account in &accounts {
        let key = format!("{}:{}", account.region, account.account_id);
        match account.ban.and_then(|ban| ban.until) {
            Some(until) if until > now => {
                state.bans.insert(key, until);
            }
            // Seen running before, over now.
            Some(until) if state.bans.get(&key) == Some(&until) => {
                state.bans.remove(&key);
                send(app, Kind::BanEnded(label(&account.account_id, &account.region)));
            }
            _ => {
                state.bans.remove(&key);
            }
        }
    }

    if leagueaccounts::backup::due() {
        let exported = shared.manager.lock().ok().and_then(|mut manager| manager.export_accounts().ok());
        let written = exported.is_some_and(|json| leagueaccounts::backup::write(&json).is_ok());
        let day = now / 86_400_000;
        if !written && state.backup_failed_day != day {
            state.backup_failed_day = day;
            send(app, Kind::BackupFailed);
        }
    }

    if before != (state.sent.clone(), state.bans.clone(), state.backup_failed_day) {
        // Switches may have changed meanwhile: only the bookkeeping is written.
        let mut current = prefs();
        current.sent = state.sent;
        current.bans = state.bans;
        current.backup_failed_day = state.backup_failed_day;
        save(&current);
    }
}

pub fn spawn(app: AppHandle, shared: Arc<Shared>) {
    thread::spawn(move || {
        // After startup settles (the page has sent its texts by then).
        thread::sleep(Duration::from_secs(40));
        loop {
            check(&app, &shared);
            thread::sleep(Duration::from_secs(3600));
        }
    });
}

#[derive(Serialize, Deserialize)]
pub struct NotifyView {
    decay: bool,
    ban: bool,
    recording: bool,
}

#[tauri::command]
pub async fn notify_prefs() -> CommandResult<NotifyView> {
    let prefs = prefs();
    Ok(NotifyView { decay: prefs.decay, ban: prefs.ban, recording: prefs.recording })
}

#[tauri::command]
pub async fn set_notify_prefs(view: NotifyView) -> CommandResult<NotifyView> {
    let mut prefs = prefs();
    prefs.decay = view.decay;
    prefs.ban = view.ban;
    prefs.recording = view.recording;
    save(&prefs);
    Ok(view)
}

#[tauri::command]
pub async fn set_notify_texts(texts: HashMap<String, String>) -> CommandResult<()> {
    let texts: HashMap<String, String> = texts.into_iter().filter(|(key, value)| key.len() <= 40 && value.chars().count() <= 400).take(40).collect();
    *TEXTS.lock().map_err(|_| fail("operation_failed"))? = Some(texts);
    Ok(())
}

#[tauri::command]
pub async fn test_notification(app: AppHandle) -> CommandResult<()> {
    send(&app, Kind::Test);
    Ok(())
}
