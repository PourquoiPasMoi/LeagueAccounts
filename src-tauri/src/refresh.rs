//! Shared state, persisted settings, rank refreshes and the auto-refresh
//! scheduler.

use crate::login_control::LoginControl;
use crate::scale::{MAX_UI_SCALE, MIN_UI_SCALE};
use crate::updater::Updater;
use crate::views::{account_view, fail, fail_with, AppError, Key};
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::models::{Account, RankInfo};
use leagueaccounts::rank_fetcher::RefreshMode;
use leagueaccounts::utils::sort_accounts;
use leagueaccounts::AccountManager;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

pub const MIN_INTERVAL_MINUTES: u32 = 5;
pub const MAX_INTERVAL_MINUTES: u32 = 24 * 60;
/// Typical RP per euro of Riot's mid-sized EU bundles (e.g. 1380 RP for 10.99).
pub const DEFAULT_RP_PER_EURO: u32 = 125;
pub const MIN_RP_PER_EURO: u32 = 50;
pub const MAX_RP_PER_EURO: u32 = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub auto_refresh: bool,
    pub interval_minutes: u32,
    pub start_minimized: bool,
    /// "riot": open the Riot Client and sign in there; "previous": Alt+Tab
    /// to the previously focused window and type (the original behavior).
    pub login_method: LoginMethod,
    /// Launch the game for the current mode after signing in.
    pub launch_game: bool,
    /// Check for and install new releases automatically; when off, GitHub is
    /// contacted only from Settings › Check now. Settings from before automatic
    /// updates stored this as "check for updates".
    #[serde(alias = "checkUpdates")]
    pub auto_update: bool,
    /// Interface size in percent, from Settings › Interface size.
    pub ui_scale: u32,
    /// Interface language code, and the record that the choice was made.
    /// Empty means "never chosen", which is what the welcome dialog keys off.
    /// It lives here as well as in the webview's storage because that storage
    /// is a cache: a rebuilt WebView2 profile loses it and the dialog would
    /// reappear on a launch that is not the first.
    #[serde(default)]
    pub ui_language: String,
    /// A release whose executable reported an older version than the release.
    /// It is never installed automatically, so it cannot update in a loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mislabeled_release: Option<String>,
    /// Unix ms of the last completed full refresh (manual or automatic).
    pub last_full_refresh: u64,
    /// Riot Points per euro, for the account value estimate.
    pub rp_per_euro: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginMethod {
    #[default]
    Riot,
    Previous,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_refresh: true,
            interval_minutes: 30,
            start_minimized: false,
            login_method: LoginMethod::Riot,
            launch_game: true,
            auto_update: true,
            ui_scale: 100,
            ui_language: String::new(),
            mislabeled_release: None,
            last_full_refresh: 0,
            rp_per_euro: DEFAULT_RP_PER_EURO,
        }
    }
}

impl Settings {
    pub fn load(path: Option<&PathBuf>) -> Self {
        let mut settings: Settings = path
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        settings.interval_minutes = settings
            .interval_minutes
            .clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES);
        settings.ui_scale = settings.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE);
        settings.rp_per_euro = settings.rp_per_euro.clamp(MIN_RP_PER_EURO, MAX_RP_PER_EURO);
        settings
    }

    pub fn save(&self, path: Option<&PathBuf>) -> std::io::Result<()> {
        let result = (|| {
            let path = path.ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::NotFound, "settings location is unavailable",
            ))?;
            let parent = path.parent().ok_or_else(|| std::io::Error::other("settings need a parent directory"))?;
            std::fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer_pretty(&mut file, self).map_err(std::io::Error::other)?;
            file.flush()?;
            file.as_file().sync_all()?;
            file.persist(path).map_err(|error| error.error)?;
            Ok(())
        })();
        if let Err(error) = &result {
            logging::record(Event::SettingsSaveFailed, Reason::from_error(error));
        }
        result
    }

    fn next_refresh_at(&self) -> Option<u64> {
        self.auto_refresh
            .then(|| self.last_full_refresh + u64::from(self.interval_minutes) * 60_000)
    }
}

pub struct Shared {
    pub manager: Mutex<AccountManager>,
    pub settings: Mutex<Settings>,
    pub settings_path: Option<PathBuf>,
    pub clipboard_generation: AtomicU64,
    pub login: Arc<LoginControl>,
    pub updater: Updater,
    pub load_error: Option<AppError>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    enabled: bool,
    interval_minutes: u32,
    next_at: Option<u64>,
    last_at: Option<u64>,
    running: bool,
}

pub fn schedule(shared: &Shared) -> Schedule {
    let settings = shared.settings.lock().map(|s| s.clone()).unwrap_or_default();
    let running = shared.login.refresh_running();
    Schedule {
        enabled: settings.auto_refresh,
        interval_minutes: settings.interval_minutes,
        next_at: settings.next_refresh_at(),
        last_at: (settings.last_full_refresh > 0).then_some(settings.last_full_refresh),
        running,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RefreshKind {
    /// Full refresh requested from the UI.
    Manual,
    /// Explicit user request; still respects provider cooldowns.
    Force,
    /// Full refresh started by the scheduler.
    Auto,
    /// A few accounts after adding or renaming them.
    Partial,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshStarted {
    total: usize,
    auto: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshDone {
    error: Option<AppError>,
    exclusive: bool,
    auto: bool,
}

/// Fetch ranks on background workers and stream each result to the UI.
/// Full refreshes are exclusive; partial ones may run alongside.
/// Returns false when shutdown or another full refresh prevents admission.
pub fn start_refresh(app: AppHandle, shared: Arc<Shared>, accounts: Vec<Account>, kind: RefreshKind) -> bool {
    if accounts.is_empty() {
        return true;
    }
    let exclusive = kind != RefreshKind::Partial;
    let Some(guard) = shared.login.try_start_refresh(exclusive) else { return false };
    let jobs = match shared.manager.lock() {
        Ok(mut manager) => {
            let provider = Arc::clone(&manager.rank_fetcher);
            let jobs = accounts.into_iter().filter_map(|account| {
                manager.begin_rank_refresh(&account.key())
            }).collect::<Vec<_>>();
            (provider, jobs)
        }
        Err(_) => return false,
    };
    let (provider, accounts) = jobs;
    if exclusive {
        // Admission and exit share one lock; errors below drop the guard.
        let _ = app.emit(
            "refresh-started",
            RefreshStarted {
                total: accounts.len(),
                auto: kind == RefreshKind::Auto,
            },
        );
    }
    logging::record(
        if kind == RefreshKind::Auto {
            Event::AutoRefreshStarted
        } else {
            Event::RankRefreshStarted
        },
        Reason::None,
    );
    let keys: Vec<Key> = accounts.iter().map(|job| Key::from(&job.account)).collect();
    let _ = app.emit("rank-pending", &keys);

    thread::spawn(move || {
        let guard = guard;
        let queue = Arc::new(Mutex::new(VecDeque::from(accounts)));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let queue = Arc::clone(&queue);
                let provider = Arc::clone(&provider);
                let shared = Arc::clone(&shared);
                let app = app.clone();
                thread::spawn(move || loop {
                    if shared.login.is_exiting() { break; }
                    let job = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                    let Some(job) = job else { break };
                    let info = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        provider.fetch_rank_with_mode(&job.account, match kind {
                            RefreshKind::Auto => RefreshMode::Auto,
                            RefreshKind::Force => RefreshMode::Force,
                            _ => RefreshMode::Manual,
                        })
                    }))
                    .unwrap_or_else(|_| {
                        logging::record(Event::RankWorkerPanicked, Reason::None);
                        RankInfo::error()
                    });
                    let key = job.account.key();
                    // OP.GG's recent games feed the account's match archive.
                    if !info.recent_games.is_empty() {
                        leagueaccounts::profiles::record_opgg_games(&key, &info.recent_games);
                    }
                    let Ok(mut manager) = shared.manager.lock() else { break };
                    if !manager.apply_rank_info(&job, &info) {
                        continue;
                    }
                    if let Some(updated) = manager.account(&key) {
                        let _ = app.emit("rank-update", account_view(updated));
                        // Feeds the LP chart on the account page.
                        leagueaccounts::profiles::record_ranks(updated);
                    }
                })
            })
            .collect();
        for worker in workers {
            let _ = worker.join();
        }

        let mut error = match shared.manager.lock() {
            Ok(mut manager) => {
                sort_accounts(&mut manager.accounts);
                manager
                    .save_accounts()
                    .err()
                    .map(|error| fail_with("save_failed", error))
            }
            Err(_) => Some(fail("storage_locked")),
        };
        if error.is_none() {
            logging::record(Event::RankRefreshCompleted, Reason::None);
        }
        if exclusive {
            if let Ok(mut settings) = shared.settings.lock() {
                let mut candidate = settings.clone();
                candidate.last_full_refresh = now_ms();
                match candidate.save(shared.settings_path.as_ref()) {
                    Ok(()) => *settings = candidate,
                    Err(failure) if error.is_none() => error = Some(fail_with("save_failed", failure)),
                    Err(_) => {},
                }
            }
        }
        drop(guard);
        let _ = app.emit(
            "refresh-done",
            RefreshDone {
                error,
                exclusive,
                auto: kind == RefreshKind::Auto,
            },
        );
        if exclusive {
            let _ = app.emit("schedule", schedule(&shared));
        }
    });
    true
}

/// Start the background loop that triggers a full refresh whenever the
/// configured interval has elapsed since the last one.
pub fn spawn_scheduler(app: AppHandle, shared: Arc<Shared>) {
    thread::spawn(move || {
        // Give the window a moment to load before the first network burst.
        thread::sleep(Duration::from_secs(8));
        loop {
            let due = shared
                .settings
                .lock()
                .ok()
                .and_then(|settings| settings.next_refresh_at())
                .is_some_and(|next| now_ms() >= next);
            if due {
                let accounts = shared
                    .manager
                    .lock()
                    .map(|manager| manager.accounts.clone())
                    .unwrap_or_default();
                if !accounts.is_empty() {
                    start_refresh(app.clone(), Arc::clone(&shared), accounts, RefreshKind::Auto);
                }
            }
            thread::sleep(Duration::from_secs(15));
        }
    });
}
