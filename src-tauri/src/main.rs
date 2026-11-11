#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri shell: exposes the account manager to the web interface.
//!
//! Passwords never cross into the web view. The UI only receives account
//! metadata; copy/login commands read the password on the Rust side.

mod login_control;
mod refresh;
mod scale;
mod updater;
mod views;

use leagueaccounts::account_manager::KEYRING_SERVICE;
use leagueaccounts::autotype::auto_type_credentials;
use leagueaccounts::credentials;
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::models::{Account, AccountKey, Ban};
use leagueaccounts::rank_fetcher::{RankFetcher, RankProvider};
use leagueaccounts::locales;
use leagueaccounts::riot_client::{self, Game, LoginError, LoginOutcome, LoginStep};
use leagueaccounts::utils::{app_data_dir, parse_account_line, region_from_display, settings_file, REGION_MAP};
use leagueaccounts::AccountManager;
use leagueaccounts::updates;
use login_control::LoginControl;
use refresh::{
    schedule, spawn_scheduler, start_refresh, LoginMethod, RefreshKind, Schedule, Settings, Shared, MAX_INTERVAL_MINUTES,
    MAX_RP_PER_EURO, MIN_INTERVAL_MINUTES, MIN_RP_PER_EURO,
};
use leagueaccounts::profiles::{self, MatchRecord, Profile, RankPoint};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use updater::{UpdateStatus, Updater};
use views::{account_view, fail, fail_with, AccountView, AppError, CommandResult, Key};

/// Copied passwords are wiped from the clipboard after this delay.
const PASSWORD_CLIPBOARD_TTL: Duration = Duration::from_secs(30);
/// Command-line flag the autostart entry passes to the app.
const AUTOSTART_FLAG: &str = "--autostart";

type AppState<'a> = State<'a, Arc<Shared>>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    auto_refresh: bool,
    interval_minutes: u32,
    start_minimized: bool,
    launch_at_startup: bool,
    login_method: LoginMethod,
    launch_game: bool,
    auto_update: bool,
    ui_scale: u32,
    /// Empty until the welcome dialog has been answered once.
    ui_language: String,
    rp_per_euro: u32,
    riot_client_found: bool,
    tft_installed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    auto_refresh: bool,
    interval_minutes: u32,
    start_minimized: bool,
    launch_at_startup: bool,
    login_method: LoginMethod,
    launch_game: bool,
    auto_update: bool,
    ui_scale: u32,
    /// Older interfaces do not send it; keep the saved value then.
    #[serde(default)]
    rp_per_euro: Option<u32>,
}

/// Fields the edit dialog can change. `password: None` keeps the stored
/// credential; an empty value is a no-op so an untouched field cannot wipe it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAccount {
    name: String,
    description: String,
    locale: Option<String>,
    password: Option<String>,
    skip_profile_update: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocaleOption {
    code: &'static str,
    name: &'static str,
    /// False for locales whose client home page does not render, so the UI
    /// can warn before the user picks one.
    home_page_works: bool,
    /// False when the Riot Client substitutes another locale at launch, so the
    /// UI does not promise a language the game will not load.
    launchable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    accounts: Vec<AccountView>,
    regions: Vec<String>,
    /// Every game locale the client can install, with display names.
    locales: Vec<LocaleOption>,
    version: &'static str,
    logging_available: bool,
    load_error: Option<AppError>,
    /// The version that failed to start; the previous version was restored.
    update_failed: Option<String>,
    settings: SettingsView,
    schedule: Schedule,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewAccount {
    account_id: String,
    #[serde(default)]
    name: String,
    region: String,
    password: String,
    description: String,
    /// Product locale the game should start in; omitted means Riot's choice.
    #[serde(default)]
    locale: Option<String>,
    /// Skip Riot's "update your profile" flow at sign-in (VN accounts).
    #[serde(default)]
    skip_profile_update: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchResult {
    added: usize,
    skipped: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkAddResult {
    added: usize,
    skipped_lines: Vec<usize>,
}

/// Validate a locale coming from the web view. An empty value clears the
/// override; an unknown one is rejected instead of silently becoming `en_US`.
fn normalize_locale(value: Option<&str>) -> CommandResult<Option<String>> {
    if value.is_some_and(|value| value.chars().any(char::is_control)) {
        return Err(fail("invalid_locale"));
    }
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => locales::normalize(value)
            .map(|locale| Some(locale.to_owned()))
            .ok_or_else(|| fail("invalid_locale")),
    }
}

fn manager(state: &Shared) -> CommandResult<MutexGuard<'_, AccountManager>> {
    state.manager.lock().map_err(|_| fail("storage_locked"))
}

fn views(state: &Shared) -> CommandResult<Vec<AccountView>> {
    let manager = manager(state)?;
    Ok(manager
        .accounts
        .iter()
        .map(account_view)
        .collect())
}

fn password_for(account: &Account) -> String {
    if !account.password.is_empty() {
        return account.password.clone();
    }
    credentials::get_password(
        KEYRING_SERVICE,
        &format!("{}:{}", account.region, account.account_id),
    )
    .ok()
    .flatten()
    .unwrap_or_default()
}

fn find_account(state: &Shared, key: Key) -> CommandResult<Account> {
    manager(state)?
        .account(&AccountKey::from(key))
        .cloned()
        .ok_or_else(|| fail("not_found"))
}

fn settings_view(app: &AppHandle, state: &Shared) -> SettingsView {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    SettingsView {
        auto_refresh: settings.auto_refresh,
        interval_minutes: settings.interval_minutes,
        start_minimized: settings.start_minimized,
        launch_at_startup: app.autolaunch().is_enabled().unwrap_or(false),
        login_method: settings.login_method,
        launch_game: settings.launch_game,
        auto_update: settings.auto_update,
        ui_scale: settings.ui_scale,
        ui_language: settings.ui_language.clone(),
        rp_per_euro: settings.rp_per_euro,
        riot_client_found: riot_client::client_path().is_some(),
        tft_installed: riot_client::game_installed(Game::Tft),
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|_| fail("operation_failed"))?
}

#[tauri::command]
async fn bootstrap(app: AppHandle, state: AppState<'_>) -> CommandResult<Bootstrap> {
    let state = Arc::clone(&state);
    blocking(move || {
    Ok(Bootstrap {
        accounts: views(&state)?,
        regions: REGION_MAP
            .iter()
            .map(|(label, _)| (*label).to_owned())
            .collect(),
        locales: locales::GAME_LOCALES
            .iter()
            .map(|code| LocaleOption {
                code,
                name: locales::locale_name(code),
                home_page_works: locales::home_page_works(code),
                launchable: locales::is_launchable(code),
            })
            .collect(),
        version: env!("CARGO_PKG_VERSION"),
        logging_available: logging::is_available(),
        load_error: state.load_error.clone(),
        update_failed: leagueaccounts::self_update::failed_update(&std::env::args_os().collect::<Vec<_>>()),
        settings: settings_view(&app, &state),
        schedule: schedule(&state),
    })
    }).await
}

#[tauri::command]
async fn list_accounts(state: AppState<'_>) -> CommandResult<Vec<AccountView>> {
    let state = Arc::clone(&state);
    blocking(move || views(&state)).await
}

#[tauri::command]
async fn add_account(app: AppHandle, state: AppState<'_>, input: NewAccount) -> CommandResult<AccountView> {
    let state = Arc::clone(&state);
    blocking(move || {
    let account_id = input.account_id.trim().to_owned();
    let name = input.name.trim().to_owned();
    let password = input.password.trim().to_owned();
    if account_id.is_empty() || password.is_empty() {
        return Err(fail("required_fields"));
    }
    let region_label = input.region.trim().to_owned();
    let region = region_from_display(&region_label).ok_or_else(|| fail("invalid_region"))?;
    let locale = normalize_locale(input.locale.as_deref())?;
    let account = {
        let mut manager = manager(&state)?;
        if manager.accounts.iter().any(|account| {
            account.account_id.eq_ignore_ascii_case(&account_id) && account.region == region
        }) {
            return Err(fail("duplicate"));
        }
        let account = Account {
            account_id,
            name,
            region: region.to_owned(),
            region_display: region_label,
            password,
            description: input.description.trim().to_owned(),
            locale,
            skip_profile_update: input.skip_profile_update,
            tier: "Unranked".to_owned(),
            reached_last_season: "N/A".to_owned(),
            finished_last_season: "N/A".to_owned(),
            ..Account::default()
        };
        let result = manager
            .add_accounts(vec![account])
            .map_err(|error| fail_with("save_failed", error))?;
        result.added.into_iter().next().ok_or_else(|| fail("password_save"))?
    };
    let view = account_view(&account);
    start_refresh(app, Arc::clone(&state), vec![account], RefreshKind::Partial);
    Ok(view)
    }).await
}

#[tauri::command]
async fn multi_add(
    app: AppHandle,
    state: AppState<'_>,
    text: String,
    region: String,
    locale: Option<String>,
) -> CommandResult<BulkAddResult> {
    let state = Arc::clone(&state);
    blocking(move || {
    let region_label = region.trim().to_owned();
    let region = region_from_display(&region_label).ok_or_else(|| fail("invalid_region"))?;
    let locale = normalize_locale(locale.as_deref())?;
    let mut candidates = Vec::new();
    let mut source_lines = Vec::new();
    let mut skipped_lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((account_id, name, password)) = parse_account_line(line) else {
            skipped_lines.push(index);
            continue;
        };
        source_lines.push(index);
        candidates.push(Account {
            account_id: account_id.to_owned(),
            name: name.to_owned(),
            region: region.to_owned(),
            region_display: region_label.clone(),
            password: password.to_owned(),
            description: String::new(),
            locale: locale.clone(),
            tier: "Unranked".to_owned(),
            reached_last_season: "N/A".to_owned(),
            finished_last_season: "N/A".to_owned(),
            ..Account::default()
        });
    }
    let result = manager(&state)?
        .add_accounts(candidates)
        .map_err(|error| fail_with("save_failed", error))?;
    skipped_lines.extend(result.skipped_indices.iter().map(|&index| source_lines[index]));
    skipped_lines.sort_unstable();
    let added = result.added.len();
    start_refresh(app, Arc::clone(&state), result.added, RefreshKind::Partial);
    Ok(BulkAddResult { added, skipped_lines })
    }).await
}

#[tauri::command]
async fn update_account(
    app: AppHandle,
    state: AppState<'_>,
    key: Key,
    input: UpdateAccount,
) -> CommandResult<AccountView> {
    let state = Arc::clone(&state);
    blocking(move || {
    let key = AccountKey::from(key);
    let changes = leagueaccounts::account_manager::AccountChanges {
        name: input.name.trim().to_owned(),
        description: input.description.trim().to_owned(),
        locale: normalize_locale(input.locale.as_deref())?,
        skip_profile_update: input.skip_profile_update.unwrap_or(false),
        password: input.password.map(|password| password.trim().to_owned()).filter(|password| !password.is_empty()),
    };
    let (updated, renamed) = manager(&state)?.update_account(&key, changes)
        .map_err(|error| fail_with("save_failed", error))?;
    let view = account_view(&updated);
    if renamed {
        start_refresh(app, Arc::clone(&state), vec![updated], RefreshKind::Partial);
    }
    Ok(view)
    }).await
}

#[tauri::command]
async fn delete_account(state: AppState<'_>, key: Key) -> CommandResult<()> {
    let state = Arc::clone(&state);
    blocking(move || {
    manager(&state)?
        .delete_account(&key.account_id, &key.region)
        .map_err(|error| fail_with("delete_failed", error))?;
    profiles::remove(&AccountKey::from(key));
    Ok(())
    }).await
}

#[tauri::command]
async fn refresh_ranks(app: AppHandle, state: AppState<'_>, force: Option<bool>) -> CommandResult<usize> {
    let state = Arc::clone(&state);
    blocking(move || {
    let accounts = manager(&state)?.accounts.clone();
    if accounts.is_empty() {
        return Err(fail("no_accounts"));
    }
    let count = accounts.len();
    let kind = if force.unwrap_or(false) { RefreshKind::Force } else { RefreshKind::Manual };
    if !start_refresh(app, Arc::clone(&state), accounts, kind) {
        return Err(fail("refresh_running"));
    }
    Ok(count)
    }).await
}

#[tauri::command]
async fn get_settings(app: AppHandle, state: AppState<'_>) -> CommandResult<SettingsView> {
    let state = Arc::clone(&state);
    blocking(move || Ok(settings_view(&app, &state))).await
}

/// Record the interface language the user picked.
///
/// The webview's localStorage already holds it, but that storage is a cache: a
/// rebuilt WebView2 profile loses it, and the welcome dialog then reappeared on
/// launches that were not the first. This is the durable copy.
#[tauri::command]
async fn set_ui_language(state: AppState<'_>, language: String) -> CommandResult<()> {
    let state = Arc::clone(&state);
    blocking(move || {
    // A short ASCII code; the webview rejects anything it has no dictionary for.
    if language.len() > 8 || !language.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(fail("invalid_language"));
    }
    let mut settings = state.settings.lock().map_err(|_| fail("storage_locked"))?;
    if settings.ui_language == language {
        return Ok(());
    }
    let mut candidate = settings.clone();
    candidate.ui_language = language;
    candidate.save(state.settings_path.as_ref()).map_err(|error| fail_with("save_failed", error))?;
    *settings = candidate;
    Ok(())
    }).await
}

#[tauri::command]
async fn update_settings(app: AppHandle, state: AppState<'_>, input: SettingsInput) -> CommandResult<SettingsView> {
    let state = Arc::clone(&state);
    blocking(move || {
    let ui_scale = input.ui_scale.clamp(scale::MIN_UI_SCALE, scale::MAX_UI_SCALE);
    let autolaunch = app.autolaunch();
    let previous_autostart = autolaunch.is_enabled().map_err(|error| fail_with("autostart_failed", error))?;
    let mut settings = state.settings.lock().map_err(|_| fail("storage_locked"))?;
    let mut candidate = settings.clone();
    candidate.auto_refresh = input.auto_refresh;
    candidate.interval_minutes = input.interval_minutes.clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES);
    candidate.start_minimized = input.start_minimized;
    candidate.login_method = input.login_method;
    candidate.launch_game = input.launch_game;
    candidate.auto_update = input.auto_update;
    candidate.ui_scale = ui_scale;
    if let Some(rate) = input.rp_per_euro {
        candidate.rp_per_euro = rate.clamp(MIN_RP_PER_EURO, MAX_RP_PER_EURO);
    }
    if previous_autostart != input.launch_at_startup {
        let result = if input.launch_at_startup { autolaunch.enable() } else { autolaunch.disable() };
        result.map_err(|error| fail_with("autostart_failed", error))?;
    }
    if let Err(error) = candidate.save(state.settings_path.as_ref()) {
        if previous_autostart != input.launch_at_startup {
            let rollback = if previous_autostart { autolaunch.enable() } else { autolaunch.disable() };
            rollback.map_err(|rollback| fail_with("autostart_failed", rollback))?;
        }
        return Err(fail_with("save_failed", error));
    }
    let previous_scale = settings.ui_scale;
    *settings = candidate;
    drop(settings);
    if previous_scale != ui_scale {
        if let Some(window) = app.get_webview_window("main") {
            scale::apply(&window, previous_scale, ui_scale);
        }
    }
    let _ = app.emit("schedule", schedule(&state));
    Ok(settings_view(&app, &state))
    }).await
}

fn set_clipboard(text: &str) -> CommandResult<()> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| leagueaccounts::clipboard::set_private_text(&mut clipboard, text))
        .map_err(|_| {
            logging::record(Event::ClipboardFailed, Reason::Other);
            fail("clipboard")
        })
}

#[tauri::command]
async fn copy_account_id(state: AppState<'_>, key: Key) -> CommandResult<()> {
    let state = Arc::clone(&state);
    blocking(move || {
    let account = find_account(&state, key)?;
    set_clipboard(&account.account_id)?;
    state.clipboard_generation.fetch_add(1, Ordering::SeqCst);
    Ok(())
    }).await
}

#[tauri::command]
async fn copy_password(state: AppState<'_>, key: Key) -> CommandResult<u64> {
    let state = Arc::clone(&state);
    blocking(move || {
    let account = find_account(&state, key)?;
    let password = password_for(&account);
    if password.is_empty() {
        return Err(fail("no_password"));
    }
    set_clipboard(&password)?;
    // Wipe the password later, unless something else was copied meanwhile.
    let generation = state.clipboard_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let shared = Arc::clone(&state);
    thread::spawn(move || {
        thread::sleep(PASSWORD_CLIPBOARD_TTL);
        if shared.clipboard_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if clipboard.get_text().is_ok_and(|text| text == password) {
                let _ = clipboard.clear();
            }
        }
    });
    Ok(PASSWORD_CLIPBOARD_TTL.as_secs())
    }).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum LoginResult {
    /// Signed in and confirmed by the Riot Client (game launched if enabled).
    SignedIn,
    /// Credentials typed in explicit previous-window mode.
    Typed,
    /// This account was already signed in (game launched if enabled).
    AlreadySignedIn,
}

/// Sign in with the account. With the Riot method the client is opened (and
/// the game launched when enabled). A different signed-in account is signed out
/// automatically before connecting the selected account. A League ban stops
/// the login before the game launches, unless `launch_banned` is set.
#[tauri::command]
async fn login(
    app: AppHandle,
    state: AppState<'_>,
    key: Key,
    tft: bool,
    close_running: bool,
    launch_banned: bool,
) -> CommandResult<LoginResult> {
    let account = find_account(&state, key)?;
    let guard = state.login.try_start().ok_or_else(|| fail("login_busy"))?;
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    let shared = Arc::clone(&state);
    let account_key = account.key();
    let result = tauri::async_runtime::spawn_blocking(move || {
        // Acquired before queueing; released only after native renderer cleanup.
        let _guard = guard;
        if shared.login.cancel.load(Ordering::SeqCst) {
            return Err(LoginError::Cancelled);
        }
        let password = password_for(&account);
        logging::record(
            Event::LoginStarted,
            if settings.login_method == LoginMethod::Previous {
                Reason::PreviousWindow
            } else {
                Reason::RiotClient
            },
        );
        let mut progress = |step: LoginStep| {
            step.record();
            let _ = app.emit("login-step", step.code());
        };
        let mut on_update = |progress: Option<riot_client::UpdateProgress>| {
            let _ = app.emit("login-update", progress);
        };
        let mut on_ban = |ban: Option<Ban>| {
            remember_ban(&app, &shared, &account_key, ban);
            launch_banned
        };
        if settings.login_method == LoginMethod::Previous {
            progress(LoginStep::Focus);
            progress(LoginStep::Type);
            return if auto_type_credentials(&account.account_id, &password) {
                progress(LoginStep::Done);
                Ok(LoginOutcome::Typed)
            } else {
                Err(LoginError::TypingFailed)
            };
        }
        let game = settings.launch_game.then(|| {
            if tft && riot_client::game_installed(Game::Tft) {
                Game::Tft
            } else {
                Game::League
            }
        });
        let outcome = riot_client::login(
            &account.account_id,
            &password,
            riot_client::LaunchOptions {
                game,
                locale: account.locale.as_deref(),
                close_running,
                skip_profile_update: account.skip_profile_update,
            },
            &mut progress,
            &mut on_update,
            &mut on_ban,
            &shared.login.cancel,
        );
        // Bulk-imported accounts usually have no Riot ID. The client now knows
        // it, so record it instead of making the user look it up. Done on its
        // own thread: the lookup can wait up to a minute, and the login (its
        // guard, the overlay) has already succeeded and must not wait on it.
        // Snapshot champions, skins, ranked stats and games once the League
        // client is up for this account (only it knows what the account owns).
        if outcome.is_ok() && game.is_some() {
            let (app, shared, key, id) = (app.clone(), Arc::clone(&shared), account_key.clone(), account.account_id.clone());
            thread::spawn(move || sync_after_login(&app, &shared, &key, &id));
        }
        if outcome.is_ok() && needs_riot_id(&shared, &account_key) {
            let (app, shared, key) = (app.clone(), Arc::clone(&shared), account_key.clone());
            let wait_for_client = game.is_some();
            thread::spawn(move || adopt_riot_id(&app, &shared, &key, wait_for_client));
        }
        outcome
    })
    .await
    .unwrap_or(Err(LoginError::WorkerPanicked));
    riot_client::record_login_result(&result);
    result
        .map(|outcome| match outcome {
            LoginOutcome::SignedIn => LoginResult::SignedIn,
            LoginOutcome::Typed => LoginResult::Typed,
            LoginOutcome::AlreadySignedIn => LoginResult::AlreadySignedIn,
        })
        .map_err(|error| match error {
            // The detail carries the end time (Unix ms), empty when permanent.
            LoginError::Banned(ban) => fail_with(
                error.code(),
                ban.until.map(|until| until.to_string()).unwrap_or_default(),
            ),
            _ => fail(error.code()),
        })
}

/// Keep the suspension Riot reported at sign-in, or clear it when Riot reports
/// none. Typed logins never reach Riot's account information.
fn remember_ban(app: &AppHandle, state: &Shared, key: &AccountKey, ban: Option<Ban>) {
    let Ok(mut manager) = manager(state) else { return };
    let Some(stored) = manager.account_mut(key) else { return };
    if stored.ban == ban {
        return;
    }
    stored.ban = ban;
    let view = account_view(stored);
    // A failed save is logged by the manager; the next save retries it.
    let _ = manager.save_accounts();
    drop(manager);
    let _ = app.emit("account-update", view);
}

/// Wait for the League client's session, record the Riot ID it reports, then
/// refresh that account's rank.
///
/// The client is still starting when the Riot sign-in finishes: it needs a few
/// more seconds before `/lol-summoner/v1/current-summoner` answers. A single
/// immediate read therefore usually found nothing and the account stayed
/// nameless, so this polls for a bounded time.
///
/// Only an empty name is filled: a name the user typed is never replaced, and
/// giving up (no client, unnamed account) leaves the account untouched. The
/// rank refresh rides on that same condition — it only runs when this call
/// actually supplied the ID, because a rank lookup needs a Riot ID and the
/// account that already had one has been refreshable all along.
fn adopt_riot_id(app: &AppHandle, shared: &Arc<Shared>, key: &AccountKey, wait_for_client: bool) {
    if !needs_riot_id(shared, key) {
        return;
    }
    let Some(riot_id) = wait_for_riot_id(
        &shared.login.cancel,
        wait_for_client,
        riot_client::game_client_running,
        || riot_client::signed_in_riot_id_for(&key.account_id),
    ) else { return };
    if shared.login.cancel.load(Ordering::SeqCst) { return; }
    let Ok(mut manager) = manager(shared) else { return };
    if shared.login.cancel.load(Ordering::SeqCst) {
        return;
    }
    let Some(stored) = manager.account_mut(key) else { return };
    // Re-checked against the stored value: an edit may have landed meanwhile.
    if !stored.name.trim().is_empty() {
        return;
    }
    let changes = leagueaccounts::account_manager::AccountChanges {
        name: riot_id,
        description: stored.description.clone(),
        locale: stored.locale.clone(),
        skip_profile_update: stored.skip_profile_update,
        password: None,
    };
    let Ok((account, _)) = manager.update_account(key, changes) else { return };
    let view = account_view(&account);
    drop(manager);
    let _ = app.emit("account-update", view);
    start_refresh(app.clone(), Arc::clone(shared), vec![account], RefreshKind::Partial);
}

fn wait_for_riot_id(
    cancel: &AtomicBool,
    wait_for_client: bool,
    mut client_running: impl FnMut() -> bool,
    mut lookup: impl FnMut() -> Option<String>,
) -> Option<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if cancel.load(Ordering::SeqCst) || (!wait_for_client && !client_running()) { return None; }
        let found = lookup();
        if cancel.load(Ordering::SeqCst) { return None; }
        if found.is_some() { return found; }
        if !wait_for_client { return None; }
        // Each lookup snapshots every process and queries the client's API.
        thread::sleep(Duration::from_millis(500));
    }
    None
}

/// Whether the account still has no Riot ID to fill in.
fn needs_riot_id(state: &Shared, key: &AccountKey) -> bool {
    manager(state)
        .ok()
        .and_then(|manager| {
            manager
                .account(key)
                .map(|account| account.name.trim().is_empty())
        })
        .unwrap_or(false)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileView {
    /// `None` until the account has been synced once.
    profile: Option<Profile>,
    /// LoL and TFT standings over time, oldest first.
    history: Vec<RankPoint>,
    /// Every archived game (client and OP.GG), newest first.
    games: Vec<MatchRecord>,
    /// Past seasons' Solo/Duo ranks from OP.GG, newest first.
    seasons: Vec<leagueaccounts::models::SeasonRank>,
}

fn profile_view(key: &AccountKey) -> ProfileView {
    let profile = profiles::load(key).map(|mut profile| {
        // Snapshots taken before the filter may still hold game-mode copies.
        profile.champions.retain(|champion| profiles::is_real_champion(champion.id));
        profile
    });
    ProfileView { profile, history: profiles::history(key), games: profiles::archive(key), seasons: profiles::seasons(key) }
}

/// Who is signed in right now, so the UI can mark that account.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionView {
    /// Riot Client username of the signed-in account.
    account_id: Option<String>,
    client_open: bool,
    in_game: bool,
}

#[tauri::command]
async fn session_status() -> CommandResult<SessionView> {
    let status = tauri::async_runtime::spawn_blocking(riot_client::status).await.unwrap_or_default();
    Ok(SessionView { account_id: status.signed_in_user, client_open: status.client_open, in_game: status.in_game })
}

fn view_key(key: &AccountKey) -> Key {
    Key { account_id: key.account_id.clone(), region: key.region.clone() }
}

/// How long to wait after a login for the League client to load the account.
const PROFILE_SYNC_WAIT: Duration = Duration::from_secs(240);

fn sync_after_login(app: &AppHandle, shared: &Arc<Shared>, key: &AccountKey, account_id: &str) {
    let deadline = std::time::Instant::now() + PROFILE_SYNC_WAIT;
    // The League client needs a few seconds after launch before it answers.
    thread::sleep(Duration::from_secs(8));
    while std::time::Instant::now() < deadline {
        // Exiting, or another login is replacing this account.
        if shared.login.is_exiting() || shared.login.is_running() {
            return;
        }
        if let Some(api) = riot_client::league_api_for(account_id) {
            let previous = profiles::load(key);
            if let Some((profile, games)) = profiles::collect(&api, previous.as_ref(), key) {
                if profiles::store_snapshot(key, &profile, games).is_ok() {
                    let _ = app.emit("profile-update", view_key(key));
                }
                return;
            }
        }
        thread::sleep(Duration::from_secs(4));
    }
    logging::record(Event::ProfileSyncFailed, Reason::Other);
}

/// OP.GG's season history is read again at most this often per account.
const OPGG_HISTORY_INTERVAL_MS: u64 = 6 * 3_600_000;
/// Match-list pages read the first time (20 games each); later reads stop at
/// the first game already stored.
const OPGG_FIRST_PAGES: usize = 8;
const OPGG_NEXT_PAGES: usize = 3;

/// Read the season's LP graph and match list from OP.GG into the account's
/// files, so history and stats exist without signing in. Returns whether
/// anything new was stored.
#[tauri::command]
async fn sync_opgg_history(app: AppHandle, state: AppState<'_>, key: Key, force: Option<bool>) -> CommandResult<bool> {
    let account = find_account(&state, key)?;
    blocking(move || {
        if account.name.trim().is_empty() {
            return Ok(false);
        }
        let key = account.key();
        let sync = profiles::opgg_sync(&key);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default();
        if !force.unwrap_or(false) && sync.at > 0 && now.saturating_sub(sync.at) < OPGG_HISTORY_INTERVAL_MS {
            return Ok(false);
        }
        let archive = profiles::archive(&key);
        let pages = if sync.complete { OPGG_NEXT_PAGES } else { OPGG_FIRST_PAGES };
        let fetcher = RankFetcher::default();
        let history = fetcher
            .fetch_history(&account.region, &account.name, pages, |game| sync.complete && profiles::has_detailed(&archive, game))
            .map_err(|reason| {
                logging::record(Event::ProfileSyncFailed, reason);
                fail("opgg_history_failed")
            })?;
        let before = (profiles::history(&key).len(), profiles::archive(&key));
        profiles::record_opgg_lp(&key, &history.lp);
        profiles::merge_games(&key, history.games);
        profiles::store_opgg_sync(&key, &profiles::OpggSync { at: now, complete: sync.complete || history.reached_end || pages == OPGG_FIRST_PAGES });
        let changed = before != (profiles::history(&key).len(), profiles::archive(&key));
        if changed {
            let _ = app.emit("profile-update", view_key(&key));
        }
        Ok(changed)
    })
    .await
}

/// The stored snapshot and rank history for the account page.
#[tauri::command]
async fn get_profile(state: AppState<'_>, key: Key) -> CommandResult<ProfileView> {
    let account = find_account(&state, key)?;
    blocking(move || Ok(profile_view(&account.key()))).await
}

/// Take a snapshot now. Needs the League client open and signed in as this account.
#[tauri::command]
async fn sync_profile(app: AppHandle, state: AppState<'_>, key: Key) -> CommandResult<ProfileView> {
    let account = find_account(&state, key)?;
    blocking(move || {
        let api = riot_client::league_api_for(&account.account_id).ok_or_else(|| fail("profile_not_signed_in"))?;
        let key = account.key();
        let previous = profiles::load(&key);
        let (profile, games) = profiles::collect(&api, previous.as_ref(), &key).ok_or_else(|| {
            logging::record(Event::ProfileSyncFailed, Reason::Other);
            fail("profile_not_ready")
        })?;
        profiles::store_snapshot(&key, &profile, games).map_err(|error| fail_with("save_failed", error))?;
        let _ = app.emit("profile-update", view_key(&key));
        Ok(profile_view(&key))
    }).await
}

/// One owned skin for the cross-account skin finder.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkinOwner {
    key: Key,
    champion_id: i64,
    champion_name: String,
    skin_id: i64,
    skin_name: String,
    tile: String,
    rarity: String,
}

/// Every owned skin of every synced account.
#[tauri::command]
async fn skin_index(state: AppState<'_>) -> CommandResult<Vec<SkinOwner>> {
    let state = Arc::clone(&state);
    blocking(move || {
        let accounts = manager(&state)?.accounts.clone();
        let mut owners = Vec::new();
        for (key, profile) in profiles::load_all(&accounts) {
            // Snapshots taken before the filter may still hold game-mode copies.
            for champion in profile.champions.into_iter().filter(|champion| profiles::is_real_champion(champion.id)) {
                for skin in champion.skins {
                    owners.push(SkinOwner {
                        key: view_key(&key),
                        champion_id: champion.id,
                        champion_name: champion.name.clone(),
                        skin_id: skin.id,
                        skin_name: skin.name,
                        tile: skin.tile,
                        rarity: skin.rarity,
                    });
                }
            }
        }
        Ok(owners)
    }).await
}

/// League's shared settings file, as the Game settings page shows it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GameSettingsView {
    /// `None` when no League install with a settings file was found.
    path: Option<String>,
    /// Read-only: the client cannot replace it with an account's cloud copy.
    locked: bool,
    /// The League client or a game is running (changes apply on its next start).
    league_open: bool,
    files: Vec<leagueaccounts::game_settings::SettingsFile>,
    last_backup: Option<u64>,
    backups: usize,
}

fn game_settings_view() -> CommandResult<GameSettingsView> {
    use leagueaccounts::game_settings as settings;
    let (last_backup, backups) = settings::backup_summary();
    let league_open = riot_client::game_client_running() || riot_client::in_game();
    let Some(path) = settings::settings_path() else {
        return Ok(GameSettingsView { path: None, locked: false, league_open, files: Vec::new(), last_backup, backups });
    };
    let files = settings::read(&path).map_err(|error| fail_with("game_settings_unreadable", error))?;
    Ok(GameSettingsView {
        locked: settings::is_locked(&path),
        path: Some(path.display().to_string()),
        league_open,
        files,
        last_backup,
        backups,
    })
}

fn game_settings_path() -> CommandResult<std::path::PathBuf> {
    leagueaccounts::game_settings::settings_path().ok_or_else(|| fail("game_settings_missing"))
}

#[tauri::command]
async fn game_settings() -> CommandResult<GameSettingsView> {
    blocking(game_settings_view).await
}

/// Write the edited values, then lock the file again when `lock` is set.
#[tauri::command]
async fn apply_game_settings(changes: Vec<leagueaccounts::game_settings::Change>, lock: bool) -> CommandResult<GameSettingsView> {
    blocking(move || {
        let path = game_settings_path()?;
        leagueaccounts::game_settings::apply(&path, &changes, lock).map_err(|error| fail_with("game_settings_save", error))?;
        game_settings_view()
    }).await
}

#[tauri::command]
async fn lock_game_settings(locked: bool) -> CommandResult<GameSettingsView> {
    blocking(move || {
        let path = game_settings_path()?;
        leagueaccounts::game_settings::set_locked(&path, locked).map_err(|error| fail_with("game_settings_lock", error))?;
        game_settings_view()
    }).await
}

/// Undo the last apply by putting the newest backup back.
#[tauri::command]
async fn restore_game_settings() -> CommandResult<GameSettingsView> {
    blocking(move || {
        let path = game_settings_path()?;
        leagueaccounts::game_settings::restore_latest(&path).map_err(|error| fail_with("game_settings_restore", error))?;
        game_settings_view()
    }).await
}

#[tauri::command]
async fn open_game_settings_folder() -> CommandResult<()> {
    blocking(move || {
        let folder = leagueaccounts::game_settings::config_dir().ok_or_else(|| fail("game_settings_missing"))?;
        std::process::Command::new("explorer.exe")
            .arg(folder)
            .spawn()
            .map(|_| ())
            .map_err(|_| fail("browser"))
    }).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GameStatusView {
    client_open: bool,
    in_game: bool,
    /// Whether Riot and any running game client both confirm this account.
    same_account: bool,
}

/// What is open right now, so the UI can warn before switching accounts.
#[tauri::command]
async fn game_status(state: AppState<'_>, key: Key) -> CommandResult<GameStatusView> {
    let account = find_account(&state, key)?;
    let status = tauri::async_runtime::spawn_blocking(riot_client::status)
        .await
        .unwrap_or_default();
    Ok(GameStatusView {
        client_open: status.client_open,
        in_game: status.in_game,
        same_account: status.signed_in_as(&account.account_id),
    })
}

#[tauri::command]
fn cancel_login(state: AppState<'_>) {
    state.login.request_cancel();
}

#[tauri::command]
async fn export_data(state: AppState<'_>, title: String) -> CommandResult<Option<String>> {
    let Some(path) = tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(title)
            .set_file_name("credentials.json")
            .add_filter("JSON", &["json"])
            .save_file()
    })
    .await
    .map_err(|_| fail("dialog_failed"))?
    else {
        return Ok(None);
    };
    let state = Arc::clone(&state);
    blocking(move || {
    let json = manager(&state)?.export_accounts().map_err(|error| {
        logging::record(Event::ExportFailed, Reason::from_error(error.as_ref()));
        fail_with("export_failed", error)
    })?;
    std::fs::write(&path, json).map_err(|error| {
        logging::record(Event::ExportFailed, Reason::from_error(&error));
        fail_with("file_write", error)
    })?;
    Ok(Some(path.display().to_string()))
    }).await
}

#[tauri::command]
async fn import_data(state: AppState<'_>, title: String) -> CommandResult<Option<BatchResult>> {
    let Some(path) = tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(title)
            .add_filter("JSON", &["json"])
            .pick_file()
    })
    .await
    .map_err(|_| fail("dialog_failed"))?
    else {
        return Ok(None);
    };
    let state = Arc::clone(&state);
    blocking(move || {
    let json = std::fs::read_to_string(&path).map_err(|error| {
        logging::record(Event::ImportFailed, Reason::from_error(&error));
        fail_with("file_read", error)
    })?;
    let (added, skipped) = manager(&state)?.import_accounts(&json).map_err(|error| {
        logging::record(Event::ImportFailed, Reason::from_error(error.as_ref()));
        fail_with("import_failed", error)
    })?;
    Ok(Some(BatchResult { added, skipped }))
    }).await
}

/// Open the account's OP.GG page (LoL or TFT) in the default browser.
#[tauri::command]
async fn open_profile(state: AppState<'_>, key: Key, tft: bool) -> CommandResult<()> {
    let state = Arc::clone(&state);
    blocking(move || {
    let account = find_account(&state, key)?;
    if account.name.is_empty() {
        return Err(fail("riot_id_missing"));
    }
    let fetcher = RankFetcher::default();
    let url = if tft {
        fetcher.build_opgg_tft_url(&account.region, &account.name)
    } else {
        fetcher.build_opgg_url(&account.region, &account.name)
    };
    std::process::Command::new("explorer.exe")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|_| fail("browser"))
    }).await
}

/// The update state now; `update-status` events carry every later change.
#[tauri::command]
fn update_status(state: AppState<'_>) -> UpdateStatus {
    state.updater.status()
}

/// Settings › Check now: installs a newer release when automatic updates are on.
#[tauri::command]
async fn check_update(app: AppHandle, state: AppState<'_>) -> CommandResult<UpdateStatus> {
    let shared = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || updater::check_now(app, shared))
        .await
        .map_err(|_| fail("update_failed"))?
}

#[tauri::command]
async fn update_idle(state: AppState<'_>) -> CommandResult<bool> {
    let shared = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || updater::idle(&shared))
        .await
        .map_err(|_| fail("update_failed"))
}

#[tauri::command]
async fn restart_to_update(app: AppHandle, state: AppState<'_>) -> CommandResult<()> {
    let state = Arc::clone(&state);
    blocking(move || updater::restart(&app, &state)).await
}

/// Open a release page of this app's repository in the default browser.
#[tauri::command]
async fn open_release(url: String) -> CommandResult<()> {
    blocking(move || {
    if !updates::is_release_url(&url) {
        return Err(fail("browser"));
    }
    std::process::Command::new("explorer.exe")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|_| fail("browser"))
    }).await
}

/// Open ABC's Bilibili page, where the zh-cn-home-locale Pengu plugin is
/// published. The URL is fixed here rather than passed in from the UI so the
/// command cannot be repurposed to open an arbitrary link.
#[tauri::command]
async fn open_home_fix_author() -> CommandResult<()> {
    blocking(move || {
    std::process::Command::new("explorer.exe")
        .arg("https://space.bilibili.com/230327779")
        .spawn()
        .map(|_| ())
        .map_err(|_| fail("browser"))
    }).await
}

/// Open Pengu Loader's site, which the zh-cn-home-locale plugin needs. Fixed
/// here for the same reason as the author link above.
#[tauri::command]
async fn open_pengu_loader() -> CommandResult<()> {
    blocking(move || {
    std::process::Command::new("explorer.exe")
        .arg("https://pengu.lol")
        .spawn()
        .map(|_| ())
        .map_err(|_| fail("browser"))
    }).await
}

#[tauri::command]
async fn open_logs_folder() -> CommandResult<()> {
    blocking(move || {
    // Fall back to the default location when file logging failed to start,
    // and recreate the folder if it was deleted while the app was running.
    let directory = logging::directory()
        .or_else(|| app_data_dir().ok().map(|directory| directory.join("logs")))
        .ok_or_else(|| fail("logs_unavailable"))?;
    std::fs::create_dir_all(&directory).map_err(|error| fail_with("logs_unavailable", error))?;
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            logging::record(Event::LogFolderOpenFailed, Reason::from_error(&error));
            fail("logs_folder")
        })
    }).await
}

/// Windows 11 rounds undecorated windows that keep a shadow; the League client's
/// frame is square, so opt out of DWM corner rounding. Earlier Windows ignore it.
fn square_corners(window: &tauri::WebviewWindow) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND};
    let Ok(hwnd) = window.hwnd() else { return };
    let preference = DWMWCP_DONOTROUND;
    // SAFETY: `hwnd` is this process's live window and the attribute value is a
    // DWM_WINDOW_CORNER_PREFERENCE that outlives the call.
    unsafe {
        DwmSetWindowAttribute(
            hwnd.0,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&preference as *const i32).cast(),
            size_of::<i32>() as u32,
        );
    }
}

#[tauri::command]
async fn app_ready(state: AppState<'_>) -> CommandResult<bool> {
    let state = Arc::clone(&state);
    blocking(move || {
        if state.load_error.is_some() { return Ok(false); }
        let Some(paths) = leagueaccounts::self_update::Paths::current() else { return Ok(false); };
        if leagueaccounts::self_update::installed_target(&paths).as_deref() != Some(env!("CARGO_PKG_VERSION")) {
            return Ok(false);
        }
        leagueaccounts::self_update::mark_healthy(&paths, env!("CARGO_PKG_VERSION")).map_err(|error| fail_with("update_failed", error))?;
        // Healthy: the hidden previous version is no longer needed.
        thread::spawn(move || leagueaccounts::self_update::remove_previous(&paths));
        Ok(true)
    }).await
}

/// Rollback watcher mode (`--update-watch`): no window, just guard the update.
fn run_update_watcher(exe: std::path::PathBuf, pid: u32) -> ! {
    use leagueaccounts::self_update::{watch_update, WatchOutcome, FAILED_FLAG};
    if let WatchOutcome::RolledBack { failed_version } = watch_update(&exe, pid) {
        logging::record(Event::UpdateFailed, Reason::Other);
        // Never offer the release that failed to start again automatically.
        if let Ok(path) = settings_file() {
            let mut settings = Settings::load(Some(&path));
            settings.mislabeled_release = Some(failed_version.clone());
            let _ = settings.save(Some(&path));
        }
        let _ = std::process::Command::new(&exe).arg(FAILED_FLAG).arg(&failed_version).spawn();
    }
    std::process::exit(0);
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some((exe, pid)) = leagueaccounts::self_update::watch_request(&args) {
        let _ = logging::init();
        run_update_watcher(exe, pid);
    }

    let result = tauri::Builder::default()
        // Must come first. A second launch (shortcut, Start menu, autostart while the
        // app is already open) exits and brings this window back instead of opening
        // another one. An update restart starts only after the old version has exited.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|arg| arg == AUTOSTART_FLAG) {
                return;
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            app_ready,
            list_accounts,
            add_account,
            multi_add,
            update_account,
            delete_account,
            refresh_ranks,
            get_settings,
            update_settings,
            set_ui_language,
            copy_account_id,
            copy_password,
            login,
            cancel_login,
            game_status,
            export_data,
            import_data,
            open_profile,
            update_status,
            check_update,
            update_idle,
            restart_to_update,
            open_release,
            open_logs_folder,
            open_home_fix_author,
            open_pengu_loader,
            get_profile,
            sync_opgg_history,
            sync_profile,
            skin_index,
            session_status,
            game_settings,
            apply_game_settings,
            lock_game_settings,
            restore_game_settings,
            open_game_settings_folder,
        ])
        // Champion and skin pictures: http://lacache.localhost/<game-data path>,
        // downloaded once from CommunityDragon and then served from disk.
        .register_asynchronous_uri_scheme_protocol("lacache", |_context, request, responder| {
            let path = request.uri().path().to_owned();
            thread::spawn(move || {
                let response = match leagueaccounts::image_cache::load(&path) {
                    Some((bytes, mime)) => tauri::http::Response::builder()
                        .header("Content-Type", mime)
                        .header("Cache-Control", "max-age=604800")
                        .body(bytes),
                    None => tauri::http::Response::builder().status(404).body(Vec::new()),
                };
                if let Ok(response) = response {
                    responder.respond(response);
                }
            });
        })
        .setup(move |app| {
    logging::install_panic_hook();
    // A logging failure must not prevent users from opening their accounts.
    let _ = logging::init();
    updater::finish_previous_update();
    // A VN login that was killed mid-way could not restore `hosts` itself.
    leagueaccounts::vn_hosts::restore_leftovers();

    let provider: Arc<dyn RankProvider> = Arc::new(RankFetcher::new());
    let mut manager = match AccountManager::new(provider) {
        Ok(manager) => manager,
        Err(_) => {
            logging::record(Event::AppRunFailed, Reason::Storage);
            std::process::exit(1);
        }
    };
    let load_error = manager
        .load_accounts()
        .err()
        .map(|error| fail_with("load_failed", error));
    let settings_path = settings_file().ok();
    let mut settings = Settings::load(settings_path.as_ref());
    // Remember a mislabeled release across launches, not just this restart.
    if let Some(release) = updater::mislabeled_release() {
        settings.mislabeled_release = Some(release);
        let _ = settings.save(settings_path.as_ref());
    }
    let updater = Updater::new(&settings);
    let ui_scale = settings.ui_scale;
    let start_minimized = (settings.start_minimized && std::env::args().any(|arg| arg == AUTOSTART_FLAG))
        || updater::start_minimized_after_restart();

    let shared = Arc::new(Shared {
        manager: Mutex::new(manager),
        settings: Mutex::new(settings),
        settings_path,
        clipboard_generation: AtomicU64::new(0),
        login: Arc::new(LoginControl::default()),
        updater,
        load_error,
    });
            app.manage(Arc::clone(&shared));
            app.handle().plugin(login_control::plugin(Arc::clone(&shared.login)))?;
            app.handle().plugin(updater::plugin(Arc::clone(&shared)))?;
            if let Some(window) = app.get_webview_window("main") {
                square_corners(&window);
                // The window opens at its configured size, which is 100%.
                scale::apply(&window, 100, ui_scale);
                let _ = window.show();
                if start_minimized {
                    let _ = window.minimize();
                } else {
                    let _ = window.set_focus();
                }
            }
            spawn_scheduler(app.handle().clone(), Arc::clone(&shared));
            updater::spawn(app.handle().clone(), Arc::clone(&shared));
            Ok(())
        })
        .build(tauri::generate_context!());
    let Ok(app) = result else {
        logging::record(Event::AppRunFailed, Reason::Other);
        // Do not return the raw error to Rust's stderr termination handler.
        std::process::exit(1);
    };
    // The event loop ends the process itself, so code after `run` never executes:
    // the last event is the only place to mark the end of the session.
    app.run(|_app, event| {
        if let tauri::RunEvent::Exit = event {
            logging::record(Event::SessionEnded, Reason::None);
        }
    });
}
