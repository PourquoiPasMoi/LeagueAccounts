//! Riot login through the native renderer and local session API.
//! No keyboard input is injected; game launch requires the selected account.

#[path = "riot_renderer.rs"]
mod renderer;

use crate::logging::{self, Event, Reason};
use crate::models::Ban;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Allow cold starts and client updates without relaxing login readiness.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Time the player has for steps Riot asks for before the game, as for
/// confirming the sign-in.
const SESSION_TIMEOUT: Duration = Duration::from_secs(300);
/// Time for the Riot Client to apply a region change.
const REGION_TIMEOUT: Duration = Duration::from_secs(10);
/// Time the client gets to accept the account's game language. A client that
/// has just been started still answers the request before its product session
/// is up, then ignores it; only once its metadata has loaded does the value
/// stick. Polling until it does costs nothing on a warm client, which accepts
/// the first request.
const LOCALE_TIMEOUT: Duration = Duration::from_secs(30);
const LOCALE_POLL: Duration = Duration::from_millis(300);
/// Launch-wait ticks (about a second each) between locale re-checks.
const LOCALE_CHECK_TICKS: u32 = 5;
const POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Game {
    League,
    Tft,
}

impl Game {
    fn product(self) -> &'static str {
        match self {
            Game::League => "league_of_legends",
            Game::Tft => "teamfighttactics",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    /// The selected account is confirmed signed in.
    SignedIn,
    /// Credentials were typed in explicit previous-window mode.
    Typed,
    /// This account was already signed in; nothing was typed.
    AlreadySignedIn,
}

/// How far the update that blocks a launch has progressed.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    /// Share of the update already applied, 0-100, when Riot reports it.
    pub percent: Option<f32>,
    /// Riot's estimate of the time left, when it reports one.
    pub remaining_ms: Option<u64>,
}

/// A progress sample plus the work it reports, used to tell a stalled update
/// from a running one. Bytes move during phases where the percentage does not.
#[derive(Clone, Copy, Debug, PartialEq)]
struct UpdateSnapshot {
    updating: bool,
    /// Queued/manual patches need a launch request; active jobs only need polling.
    needs_launch: bool,
    progress: UpdateProgress,
    work: u64,
}

/// Progress reported to the UI while signing in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStep {
    CloseLeague,
    /// Starting the client and waiting until its session is readable.
    OpenClient,
    SignOut,
    /// Finding the login form, typing the credentials and confirming them.
    SignIn,
    Verification,
    Focus,
    Type,
    LaunchGame,
    /// The game has an update to download or install before it can start.
    Updating,
    Done,
}

impl LoginStep {
    pub fn record(self) {
        let reason = match self {
            Self::CloseLeague => Reason::CloseLeague,
            Self::OpenClient => Reason::OpenClient,
            Self::SignOut => Reason::SignOut,
            Self::SignIn => Reason::SignIn,
            Self::Verification => Reason::AuthInteraction,
            Self::Focus => Reason::Focus,
            Self::Type => Reason::Type,
            Self::LaunchGame => Reason::LaunchGame,
            Self::Updating => Reason::Updating,
            Self::Done => Reason::Done,
        };
        logging::record(Event::LoginProgress, reason);
    }

    pub fn code(self) -> &'static str {
        match self {
            LoginStep::CloseLeague => "closeLeague",
            LoginStep::OpenClient => "openClient",
            LoginStep::SignOut => "signOut",
            LoginStep::SignIn => "signIn",
            LoginStep::Verification => "verification",
            LoginStep::Focus => "focus",
            LoginStep::Type => "type",
            LoginStep::LaunchGame => "launchGame",
            LoginStep::Updating => "updating",
            LoginStep::Done => "done",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginError {
    ClientMissing,
    LaunchFailed,
    /// The Riot Client runs, or must run, as administrator, so this app can
    /// neither start nor close it.
    RiotElevated,
    /// The Riot Client did not close before the login form could be opened.
    RiotCloseFailed,
    /// The login window or sign-in state never became observable.
    Timeout,
    /// Signing out of the current session is not available.
    SignOutFailed,
    /// The Riot session signed out before the game could be launched.
    SessionEnded,
    /// The Riot Client did not switch to the account's League region.
    RegionFailed,
    RendererFailed,
    AuthRejected,
    CaptchaRejected,
    RateLimited,
    TypingFailed,
    /// Cancelled from the UI.
    Cancelled,
    /// A League/TFT client or match is open for another account and closing
    /// it was not confirmed.
    LeagueRunning,
    /// The League/TFT client did not close.
    CloseFailed,
    /// The game client did not confirm the selected account in time.
    GameLaunchFailed,
    /// Riot or League reports a different account during launch.
    AccountMismatch,
    /// Riot reports a League suspension; the game would disconnect at once.
    Banned(Ban),
    /// The VN profile-update skip needs `hosts`, which could not be written
    /// (usually because the app is not running as administrator).
    HostsPatchFailed,
    WorkerPanicked,
}

impl LoginError {
    pub fn code(self) -> &'static str {
        match self {
            LoginError::ClientMissing => "riot_client_missing",
            LoginError::LaunchFailed => "riot_launch_failed",
            LoginError::RiotElevated => "riot_elevated",
            LoginError::RiotCloseFailed => "riot_close_failed",
            LoginError::Timeout => "riot_timeout",
            LoginError::SignOutFailed => "riot_sign_out_failed",
            LoginError::SessionEnded => "riot_session_ended",
            LoginError::RegionFailed => "riot_region_failed",
            LoginError::RendererFailed => "riot_renderer_failed",
            LoginError::AuthRejected => "riot_auth_rejected",
            LoginError::CaptchaRejected => "riot_captcha_rejected",
            LoginError::RateLimited => "riot_rate_limited",
            LoginError::TypingFailed => "autotype_failed",
            LoginError::Cancelled => "login_cancelled",
            LoginError::LeagueRunning => "league_running",
            LoginError::CloseFailed => "league_close_failed",
            LoginError::GameLaunchFailed => "game_launch_failed",
            LoginError::AccountMismatch => "game_account_mismatch",
            LoginError::Banned(_) => "account_banned",
            LoginError::HostsPatchFailed => "hosts_patch_failed",
            LoginError::WorkerPanicked => "autotype_failed",
        }
    }

    /// Only fixed categories cross the logging boundary; never account identifiers.
    fn reason(self) -> Reason {
        match self {
            LoginError::ClientMissing => Reason::ClientMissing,
            LoginError::LaunchFailed => Reason::LaunchFailed,
            LoginError::RiotElevated => Reason::RiotElevated,
            LoginError::RiotCloseFailed => Reason::RiotCloseFailed,
            LoginError::Timeout => Reason::Timeout,
            LoginError::SignOutFailed => Reason::SignOutFailed,
            LoginError::SessionEnded => Reason::SessionEnded,
            LoginError::RegionFailed => Reason::RegionFailed,
            LoginError::RendererFailed => Reason::RendererFailed,
            LoginError::AuthRejected => Reason::AuthRejected,
            LoginError::CaptchaRejected => Reason::CaptchaRejected,
            LoginError::RateLimited => Reason::HttpRateLimited,
            LoginError::TypingFailed => Reason::TypingFailed,
            LoginError::Cancelled => Reason::Cancelled,
            LoginError::LeagueRunning => Reason::LeagueRunning,
            LoginError::CloseFailed => Reason::CloseFailed,
            LoginError::GameLaunchFailed => Reason::GameLaunchFailed,
            LoginError::AccountMismatch => Reason::AccountMismatch,
            LoginError::Banned(_) => Reason::AccountBanned,
            LoginError::HostsPatchFailed => Reason::HostsPatchFailed,
            LoginError::WorkerPanicked => Reason::WorkerPanicked,
        }
    }
}

pub fn record_login_result(result: &Result<LoginOutcome, LoginError>) {
    let (event, reason) = match result {
        Ok(outcome) => (
            Event::LoginCompleted,
            match outcome {
                LoginOutcome::SignedIn => Reason::SignedIn,
                LoginOutcome::Typed => Reason::Typed,
                LoginOutcome::AlreadySignedIn => Reason::AlreadySignedIn,
            },
        ),
        Err(error) => (
            if *error == LoginError::Cancelled {
                Event::LoginCancelled
            } else {
                Event::LoginFailed
            },
            error.reason(),
        ),
    };
    logging::record(event, reason);
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// Path of `RiotClientServices.exe` from Riot's install registry file.
pub fn client_path() -> Option<PathBuf> {
    let installs = program_data().join(r"Riot Games\RiotClientInstalls.json");
    let from_file = std::fs::read_to_string(installs)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|value| {
            ["rc_default", "rc_live"]
                .iter()
                .find_map(|key| value[*key].as_str().map(PathBuf::from))
        })
        .filter(|path| path.exists());
    from_file.or_else(|| {
        let fallback = PathBuf::from(r"C:\Riot Games\Riot Client\RiotClientServices.exe");
        fallback.exists().then_some(fallback)
    })
}

/// Whether Riot's metadata lists the product's live patchline as installed.
pub fn game_installed(game: Game) -> bool {
    program_data()
        .join(r"Riot Games\Metadata")
        .join(format!("{}.live", game.product()))
        .exists()
}

/// Riot's per-product locale resource for the live patchline.
fn product_locale_path(game: Game) -> String {
    format!(
        "/riotclient/product-locales/products/{}/patchlines/live",
        game.product()
    )
}

/// Start `game` for the signed-in account and wait until its client process
/// confirms the same account. Requests go through the local API, after
/// rechecking the Riot session immediately before each launch request.
fn launch_game(
    game: Game,
    account_id: &str,
    locale: Option<&str>,
    progress: &mut dyn FnMut(LoginStep),
    on_update: &mut dyn FnMut(Option<UpdateProgress>),
    cancel: &AtomicBool,
) -> Result<(), LoginError> {
    // The patch belongs to the launch row: while it runs, the row is renamed
    // and carries its percentage instead of a second step appearing.
    let mut reported_step: Option<bool> = None;
    let mut reported_progress: Option<UpdateProgress> = None;
    let mut report = |update: Option<UpdateProgress>| {
        let updating = update.is_some();
        if reported_step != Some(updating) {
            progress(if updating {
                LoginStep::Updating
            } else {
                LoginStep::LaunchGame
            });
            reported_step = Some(updating);
        }
        if reported_progress != update {
            on_update(update);
            reported_progress = update;
        }
    };
    let mut locale_ticks: u32 = 0;
    let result = launch_game_with(
        account_id,
        cancel,
        status,
        || {
            let Some(api) = LocalApi::read().filter(|api| api.session() == Session::SignedIn)
            else {
                return Ok(Launch::Rejected);
            };
            let Some(name) = api.username() else {
                return Ok(Launch::Rejected);
            };
            check_cancelled(cancel)?;
            if !name.eq_ignore_ascii_case(account_id) {
                return Err(LoginError::AccountMismatch);
            }
            api.follow_league_region(REGION_TIMEOUT, cancel)?;
            Ok(api.launch(game))
        },
        || LocalApi::read().and_then(|api| api.update_progress(game)),
        &mut report,
        || {
            // The client rewrites the product settings from its own metadata
            // a few seconds after it starts, which can undo the locale the
            // login applied and put the region default back before the game
            // process is spawned. One attempt every few ticks, so a client
            // that is still starting never blocks the loop, and a long patch
            // does not re-read and re-write Riot's settings file every second.
            if let Some(locale) = locale {
                if locale_ticks.is_multiple_of(LOCALE_CHECK_TICKS) {
                    ensure_locale(game, locale);
                }
                locale_ticks += 1;
            }
            thread::sleep(Duration::from_secs(1));
        },
    );

    if result.is_ok() {
        if let Some(locale) = locale {
            // The launch was confirmed; record whether the client kept the
            // language the account asked for.
            if let Some(current) = LocalApi::read().and_then(|api| api.product_locale(game)) {
                if current != locale {
                    logging::record(Event::LoginProgress, Reason::LocaleRejected);
                }
            }
        }
    }
    result
}

/// Riot answers a launch request only once the game process exists, which
/// can take half a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Launch {
    Accepted,
    /// No answer yet; Riot keeps launching after the request times out and
    /// answers repeated requests with "already launched".
    Pending,
    /// Riot answered 424: the product is not up to date and it patches it
    /// before a launch can be accepted.
    Updating,
    Rejected,
}

fn launch_game_with(
    account_id: &str,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> GameStatus,
    mut request: impl FnMut() -> Result<Launch, LoginError>,
    mut update: impl FnMut() -> Option<UpdateSnapshot>,
    report: &mut dyn FnMut(Option<UpdateProgress>),
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    let mut requested = false;
    let mut blocked = false;
    let mut attempts = 0;
    let mut update_ticks = 0;
    let mut stalled = 0;
    let mut work = None;
    let mut diagnostics = LoginDiagnostics::default();
    while attempts < ATTEMPTS {
        check_cancelled(cancel)?;
        let current = observe();
        check_cancelled(cancel)?;
        let confirmed = launch_confirmed(&current, account_id)?;
        // Keep checking after each patch and during login confirmation. Even
        // an observed game process must not hide a newly active patch.
        let snapshot = update();
        if blocked && work.is_some() && snapshot.is_some_and(|snapshot| !snapshot.updating) {
            // A late language patch must still leave the game a full startup
            // window. An unavailable API alone cannot reset that deadline, and
            // neither can a 424 whose patch never showed up as active work:
            // that would re-request every tick and never count as a stall.
            attempts = 0;
            blocked = false;
        }
        let snapshot = snapshot.filter(|snapshot| snapshot.updating);
        check_cancelled(cancel)?;
        // Only an active job (Updating/Repairing) holds back a confirmed game.
        // `OutOfDate`/`NeedsRepair` can describe the *next* launch while this
        // one runs, and no launch request is sent while the game is up, so
        // waiting on them would end a working login as a stalled patch.
        if confirmed && snapshot.is_none_or(|snapshot| snapshot.needs_launch) {
            report(None);
            return Ok(());
        }
        let mut reason = if current.league_running() {
            Reason::GameIdentityPending
        } else if current.signed_in_as(account_id) {
            Reason::GameProcessPending
        } else {
            Reason::AuthUnavailable
        };
        if snapshot.is_some() {
            // Resume after a patch even when Riot previously accepted a launch.
            requested = false;
            blocked = true;
        }
        if !requested
            && snapshot.is_none_or(|snapshot| snapshot.needs_launch)
            && !current.league_running()
            && current.signed_in_as(account_id)
            && if blocked {
                update_ticks % UPDATE_RETRY_TICKS == 0
            } else {
                attempts % 2 == 0
            }
        {
            match request()? {
                Launch::Accepted => {
                    requested = true;
                    blocked = snapshot.is_some();
                }
                Launch::Pending => {
                    blocked = snapshot.is_some();
                    reason = Reason::LaunchPending;
                }
                Launch::Updating => blocked = true,
                Launch::Rejected => {
                    blocked = snapshot.is_some();
                    reason = Reason::LaunchRejected;
                }
            }
        }
        if blocked {
            let progress = snapshot.map(|snapshot| snapshot.progress).unwrap_or(UpdateProgress {
                percent: None,
                remaining_ms: None,
            });
            report(Some(progress));
            if let Some(snapshot) = snapshot {
                let current_work = (snapshot.work, snapshot.progress.percent);
                if work == Some(current_work) {
                    stalled += 1;
                } else {
                    stalled = 0;
                    work = Some(current_work);
                }
            } else {
                // Blocked (Riot answered 424) with no active patch to show:
                // nothing is progressing, so it counts towards the stall limit.
                stalled += 1;
            }
            diagnostics.waiting(Reason::Updating);
            update_ticks += 1;
            if update_ticks >= UPDATE_TICKS || stalled >= UPDATE_STALL_TICKS {
                report(None);
                diagnostics.waiting(Reason::UpdateFailed);
                return Err(LoginError::GameLaunchFailed);
            }
        } else {
            report(None);
            stalled = 0;
            work = None;
            diagnostics.waiting(reason);
            attempts += 1;
        }
        wait();
    }
    check_cancelled(cancel)?;
    Err(LoginError::GameLaunchFailed)
}

/// Launch attempts that are not spent waiting for an update.
const ATTEMPTS: u32 = 40;
/// One tick per second: at most 30 minutes of patch waiting per launch.
const UPDATE_TICKS: u32 = 1_800;
/// Ticks with neither byte nor percentage progress: the patch is stuck.
const UPDATE_STALL_TICKS: u32 = 240;
/// Retry a blocked launch when the patch API has no active state to report.
const UPDATE_RETRY_TICKS: u32 = 3;

fn launch_confirmed(current: &GameStatus, account_id: &str) -> Result<bool, LoginError> {
    let different = |name: &Option<String>| {
        name.as_deref()
            .is_some_and(|name| !name.eq_ignore_ascii_case(account_id))
    };
    if different(&current.signed_in_user)
        || (current.league_running() && different(&current.game_user))
    {
        return Err(LoginError::AccountMismatch);
    }
    Ok(current.league_running() && current.signed_in_as(account_id))
}

/// Launch the requested game; TFT falls back to the League client, which
/// also hosts TFT, when the standalone TFT client cannot start.
///
/// `locale` is applied to the product before the launch request. A locale the
/// client refuses is not fatal: the game still starts in whatever language the
/// client already had.
fn start_game(
    game: Game,
    account_id: &str,
    locale: Option<&str>,
    progress: &mut dyn FnMut(LoginStep),
    on_update: &mut dyn FnMut(Option<UpdateProgress>),
    cancel: &AtomicBool,
) -> Result<(), LoginError> {
    // Region changes can rebuild product metadata. Set the account's region
    // before its language, so the first launch patches the intended locale.
    check_cancelled(cancel)?;
    if let Some(api) = LocalApi::read().filter(|api| api.session() == Session::SignedIn) {
        if api.username().is_some_and(|name| !name.eq_ignore_ascii_case(account_id)) {
            return Err(LoginError::AccountMismatch);
        }
        api.follow_league_region(REGION_TIMEOUT, cancel)?;
    }
    if let Some(locale) = locale {
        apply_locale(game, locale, cancel);
    }
    let launched = match launch_game(game, account_id, locale, progress, on_update, cancel) {
        Err(LoginError::GameLaunchFailed) if game == Game::Tft => {
            if let Some(locale) = locale {
                apply_locale(Game::League, locale, cancel);
            }
            launch_game(Game::League, account_id, locale, progress, on_update, cancel)
        }
        result => result,
    };
    // Only once the game is actually running: the Riot window is the only way
    // back if the launch failed, so it must not be hidden before that.
    if launched.is_ok() {
        windows::hide_riot_client();
    }
    launched
}

/// Set the game's language and record the outcome.
///
/// The Riot Client's local API is what the launcher reads when it starts the
/// game, and it is also what makes the client rewrite the product settings
/// file. A freshly started client answers the request before that state is up
/// and then discards it, which is how the game could start in the region's
/// default language even though the write reported success — so the locale is
/// read back and the request repeated until the client agrees or time runs out.
///
/// The file is written afterwards, for the cold start and for `zh_CN`, which it
/// lists as available even though Riot's registry does not.
///
/// Failures stay non-fatal; the user still gets the game, just not the
/// requested language.
fn apply_locale(game: Game, locale: &str, cancel: &AtomicBool) {
    let started = Instant::now();
    let mut by_api = false;
    while !by_api && started.elapsed() < LOCALE_TIMEOUT && !cancel.load(Ordering::SeqCst) {
        by_api = LocalApi::read().is_some_and(|api| {
            api.set_product_locale(game, locale)
                && api.product_locale(game).as_deref() == Some(locale)
        });
        if !by_api {
            thread::sleep(LOCALE_POLL);
        }
    }
    // Written after the request settled: the client rewrites this file from
    // its own state when it accepts a locale, and a write made before that
    // rewrite is what used to be lost.
    let by_file = crate::game_locale::settings_file(game.product())
        .is_some_and(|path| crate::game_locale::write(&path, locale));
    logging::record(
        Event::LoginProgress,
        match (by_api, by_file) {
            (true, _) => Reason::LocaleApplied,
            // Without the client's own state, a later rewrite can still undo
            // the file, so a file-only result is not "applied".
            (false, true) => Reason::LocaleRejected,
            (false, false) => Reason::LocaleUnavailable,
        },
    );
}

/// Put the locale back after the client rewrote it from its own metadata.
///
/// One attempt per call: the launch loop ticks about once a second, so a
/// client that is still starting is retried there instead of blocking here.
/// The settings file is brought in step as well — the client's own rewrite
/// drops `zh_CN` from `available_locales`, and the launcher reads that list
/// when it builds the game's `--locale=`. The write is skipped when nothing
/// changed.
fn ensure_locale(game: Game, locale: &str) -> bool {
    let Some(api) = LocalApi::read() else {
        return false;
    };
    if api.product_locale(game).as_deref() != Some(locale)
        && !(api.set_product_locale(game, locale)
            && api.product_locale(game).as_deref() == Some(locale))
    {
        return false;
    }
    if let Some(path) = crate::game_locale::settings_file(game.product()) {
        crate::game_locale::write(&path, locale);
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Session {
    SignedIn,
    SignedOut,
    Unknown(Reason),
}

/// The lifecycle endpoint is available before RSO initializes on a cold start.
/// Only its explicit login-strategy state permits typing; neither HTTP errors
/// nor an absent RSO authorization prove that the login form is ready.
fn session_state(response: Option<(u16, serde_json::Value)>) -> Session {
    let Some((200, body)) = response else {
        return Session::Unknown(Reason::AuthUnavailable);
    };
    if has_identity(&body) {
        return Session::SignedIn;
    }
    match body["loginState"].as_str() {
        Some("PendingLoginStrategy") => Session::SignedOut,
        Some(_) if body["actionRequired"] == true => Session::Unknown(Reason::AuthInteraction),
        Some(_) => Session::Unknown(Reason::AuthStarting),
        None => Session::Unknown(Reason::AuthUnavailable),
    }
}

fn has_identity(body: &serde_json::Value) -> bool {
    body["puuid"].as_str().is_some_and(|id| !id.is_empty())
}

/// The identity exists before Riot finishes the session: new terms, a League
/// region or a Riot ID can still need the player. Only a session waiting for
/// a product can be handed to a game; one launched earlier cannot log in.
/// The session stays in that state while and after a game runs.
fn product_context(response: Option<(u16, serde_json::Value)>) -> Result<(), Reason> {
    let Some((200, body)) = response else {
        return Err(Reason::AuthUnavailable);
    };
    let state = body["loginState"].as_str();
    // Signed out meanwhile, for example from the Riot window. The login form
    // also requires an action, so check this first.
    if state == Some("PendingLoginStrategy") && !has_identity(&body) {
        return Err(Reason::SessionEnded);
    }
    if body["actionRequired"] == true {
        return Err(Reason::SessionActionRequired);
    }
    match state {
        Some("PendingProductContext") => Ok(()),
        _ => Err(Reason::SessionPending),
    }
}

/// Wait until the signed-in session can be handed to a game, showing the
/// Riot window once if Riot needs the player first.
fn await_product_context(
    client: &mut renderer::Renderer,
    progress: &mut dyn FnMut(LoginStep),
    cancel: &AtomicBool,
) -> Result<(), LoginError> {
    await_product_context_with(
        cancel,
        SESSION_TIMEOUT,
        || {
            LocalApi::read().map_or(Err(Reason::AuthUnavailable), |api| {
                product_context(api.get("/player-session-lifecycle/v1/session"))
            })
        },
        || {
            client.show()?;
            progress(LoginStep::Verification);
            Ok(())
        },
        || thread::sleep(POLL),
    )
}

fn await_product_context_with(
    cancel: &AtomicBool,
    timeout: Duration,
    mut observe: impl FnMut() -> Result<(), Reason>,
    mut show: impl FnMut() -> Result<(), LoginError>,
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    let started = Instant::now();
    let mut shown = false;
    let mut diagnostics = LoginDiagnostics::default();
    loop {
        check_cancelled(cancel)?;
        let reason = match observe() {
            Ok(()) => return Ok(()),
            Err(reason) => reason,
        };
        check_cancelled(cancel)?;
        if reason == Reason::SessionEnded {
            return Err(LoginError::SessionEnded);
        }
        if reason == Reason::SessionActionRequired && !shown {
            show()?;
            shown = true;
        }
        diagnostics.waiting(reason);
        if started.elapsed() >= timeout {
            return Err(LoginError::Timeout);
        }
        wait();
    }
}

#[derive(Default)]
struct LoginDiagnostics(Option<Reason>);

impl LoginDiagnostics {
    fn waiting(&mut self, reason: Reason) {
        if self.0 != Some(reason) {
            logging::record(Event::LoginWaiting, reason);
            self.0 = Some(reason);
        }
    }
}

/// The Riot Client's local HTTPS API, described by its lockfile.
struct LocalApi {
    base: String,
    password: String,
    client: reqwest::blocking::Client,
}

impl LocalApi {
    fn read() -> Option<Self> {
        let lockfile = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)?
            .join(r"Riot Games\Riot Client\Config\lockfile");
        let contents = std::fs::read_to_string(lockfile).ok()?;
        Self::from_lockfile(&contents)
    }

    fn from_lockfile(contents: &str) -> Option<Self> {
        Self::from_lockfile_with_timeout(contents, Duration::from_secs(3))
    }

    fn from_lockfile_with_timeout(contents: &str, timeout: Duration) -> Option<Self> {
        // name:pid:port:password:protocol
        let parts: Vec<&str> = contents.trim().split(':').collect();
        let [_, _, port, password, _] = parts[..] else {
            return None;
        };
        let client = reqwest::blocking::Client::builder()
            // The API is bound to loopback with a self-signed certificate.
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .timeout(timeout)
            .build()
            .ok()?;
        Some(Self {
            base: format!("https://127.0.0.1:{}", port.parse::<u16>().ok()?),
            password: password.to_owned(),
            client,
        })
    }

    fn functions(&self) -> Option<HashSet<String>> {
        let body = self
            .client
            .get(format!("{}/help", self.base))
            .basic_auth("riot", Some(&self.password))
            .send()
            .ok()?
            .text()
            .ok()?;
        let help: serde_json::Value = serde_json::from_str(&body).ok()?;
        Some(help["functions"].as_object()?.keys().cloned().collect())
    }

    fn get(&self, path: &str) -> Option<(u16, serde_json::Value)> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .basic_auth("riot", Some(&self.password))
            .send()
            .ok()?;
        let status = response.status().as_u16();
        let body = response.text().ok().unwrap_or_default();
        Some((status, serde_json::from_str(&body).unwrap_or_default()))
    }

    fn session(&self) -> Session {
        session_state(self.get("/player-session-lifecycle/v1/session"))
    }

    /// Ask the client to launch a product.
    fn launch(&self, game: Game) -> Launch {
        let response = self
            .client
            .post(format!(
                "{}/product-launcher/v1/products/{}/patchlines/live",
                self.base,
                game.product()
            ))
            .basic_auth("riot", Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body("{}")
            .send();
        match response {
            // 423: an earlier request already launched it.
            Ok(response) if response.status().is_success() || response.status() == 423 => {
                Launch::Accepted
            }
            // 424: the product is not up to date; Riot patches it before it
            // can launch, and accepts a new request once the patch is done.
            Ok(response) if response.status().as_u16() == 424 => Launch::Updating,
            Err(error) if error.is_timeout() => Launch::Pending,
            _ => Launch::Rejected,
        }
    }

    /// How far the update that keeps `game` from launching has progressed.
    /// Riot reports a zero estimate until it knows how long the rest takes.
    fn update_progress(&self, game: Game) -> Option<UpdateSnapshot> {
        let (status, body) = self.get(&format!(
            "/patch-proxy/v2/patch-states/products/{}/patchlines/live",
            game.product()
        ))?;
        if status != 200 {
            return None;
        }
        let progress = &body["progress"];
        let percent = progress["overallProgress"]
            .as_f64()
            .map(|value| (value.clamp(0.0, 100.0) * 10.0).round() as f32 / 10.0);
        let remaining_ms = progress["totalTimeRemainingMs"]
            .as_u64()
            .filter(|ms| *ms > 0)
            .map(|ms| ms / 1000 * 1000);
        let work = ["totalBytesDone", "totalBytesWritten", "totalBytesDownloaded"]
            .iter()
            .filter_map(|key| progress[*key].as_u64())
            .sum();
        let state = body["state"].as_str();
        let needs_launch = matches!(state, Some("OutOfDate" | "NeedsRepair"));
        Some(UpdateSnapshot {
            updating: needs_launch || matches!(state, Some("Updating" | "Repairing")),
            needs_launch,
            progress: UpdateProgress {
                percent,
                remaining_ms,
            },
            work,
        })
    }

    /// Read the product locale the client will launch `game` with.
    fn product_locale(&self, game: Game) -> Option<String> {
        let (status, body) = self.get(&product_locale_path(game))?;
        (status == 200).then(|| body.as_str().map(str::to_owned)).flatten()
    }

    /// Set the product locale before launching. Riot answers 201 on success and
    /// keeps the previous value when the locale is not installable, so callers
    /// verify with [`LocalApi::product_locale`] instead of trusting the status.
    fn set_product_locale(&self, game: Game, locale: &str) -> bool {
        self.client
            .put(format!("{}{}", self.base, product_locale_path(game)))
            .basic_auth("riot", Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!(locale).to_string())
            .send()
            .is_ok_and(|response| response.status().is_success())
    }

    /// The game signs in to the region the Riot Client is set to. Riot's
    /// window moves it to the account's League region after each sign-in;
    /// without that window, an account from another region would start
    /// League in the previous account's region and fail to log in. An
    /// unreadable state changes nothing.
    fn follow_league_region(
        &self,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<(), LoginError> {
        const REGION_LOCALE: &str = "/riotclient/region-locale";
        let (Some(info), Some((200, current)), Some((200, regions))) = (
            self.userinfo(),
            self.get(REGION_LOCALE),
            self.get("/data-store/v1/system-settings/region_data"),
        ) else {
            return Ok(());
        };
        let Some(region) = current["region"]
            .as_str()
            .and_then(|current| league_region(&info, &regions, current))
        else {
            return Ok(());
        };
        check_cancelled(cancel)?;
        logging::record(Event::LoginWaiting, Reason::RegionChange);
        // The same request as Riot's window; the result is read back below.
        let _ = self
            .client
            .put(format!("{}{REGION_LOCALE}", self.base))
            .basic_auth("riot", Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({"region": region, "locale": current["locale"]}).to_string())
            .send();
        let started = Instant::now();
        loop {
            check_cancelled(cancel)?;
            if self
                .get(REGION_LOCALE)
                .is_some_and(|(status, body)| status == 200 && body["region"] == region)
            {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(LoginError::RegionFailed);
            }
            thread::sleep(POLL);
        }
    }

    /// Riot account information of the signed-in account.
    fn userinfo(&self) -> Option<serde_json::Value> {
        let (status, body) = self.get("/rso-auth/v1/authorization/userinfo")?;
        if status != 200 {
            return None;
        }
        // `userInfo` is itself a JSON document encoded as a string.
        match &body["userInfo"] {
            serde_json::Value::String(text) => serde_json::from_str(text).ok(),
            other => Some(other.clone()),
        }
    }

    /// Username of the signed-in account, if any.
    fn username(&self) -> Option<String> {
        self.userinfo()?["username"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    /// Read the game client's own session, not the Riot launcher's session.
    fn game_username(&self) -> Option<String> {
        let (status, body) = self.get("/lol-login/v1/session")?;
        if status != 200 || body["state"] != "SUCCEEDED" {
            return None;
        }
        body["username"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    /// The signed-in player's `GameName#TagLine`, read from the game client.
    ///
    /// Accounts added in bulk often carry only a login name and a password.
    /// Once the game client has a session, its own summoner record knows the
    /// Riot ID, so the account can be filled in without asking the user.
    /// `gameName` is empty for an unnamed account, and `tagLine` is
    /// upper-cased to match how Riot displays it.
    fn riot_id(&self) -> Option<String> {
        let (status, body) = self.get("/lol-summoner/v1/current-summoner")?;
        if status != 200 {
            return None;
        }
        riot_id_from_summoner(&body)
    }

    fn sign_out(&self, cancel: &AtomicBool) -> Result<(), LoginError> {
        check_cancelled(cancel)?;
        if !self
            .functions()
            .is_some_and(|functions| functions.contains("DeletePlayerSessionLifecycleV1Session"))
        {
            return Err(LoginError::SignOutFailed);
        }
        check_cancelled(cancel)?;
        let sent = self
            .client
            .delete(format!("{}/player-session-lifecycle/v1/session", self.base))
            .basic_auth("riot", Some(&self.password))
            .send()
            .is_ok_and(|response| response.status().is_success());
        if sent {
            Ok(())
        } else {
            Err(LoginError::SignOutFailed)
        }
    }
}

/// `GameName#TAGLINE` from a `/lol-summoner/v1/current-summoner` body.
///
/// `tagLine` arrives lower-cased (`tw2`) while Riot displays it upper-cased.
/// An unnamed account reports empty strings for both, which is not an error.
fn riot_id_from_summoner(body: &serde_json::Value) -> Option<String> {
    let game_name = body["gameName"].as_str()?.trim();
    let tag_line = body["tagLine"].as_str()?.trim();
    if game_name.is_empty() || tag_line.is_empty() {
        return None;
    }
    Some(format!("{game_name}#{}", tag_line.to_uppercase()))
}

/// The Riot Client region (a `region_data` key such as `EUW`) to switch to so
/// that League signs in to one of the account's platforms (such as `EUW1`).
/// Like Riot's window, keep a region the account plays on, otherwise take
/// the account's current platform. `None` when nothing needs to change or
/// the account has no known League platform.
fn league_region(
    info: &serde_json::Value,
    regions: &serde_json::Value,
    current: &str,
) -> Option<String> {
    let platforms: Vec<&str> = std::iter::once(&info["lol"])
        .chain(info["lol_region"].as_array().into_iter().flatten())
        .filter(|account| account["active"] != false)
        .filter_map(|account| account["cpid"].as_str())
        .collect();
    let platform = *platforms.first()?;
    let platform_of = |region: &str| regions[region]["rso_platform_id"].as_str();
    if platform_of(current).is_some_and(|current| platforms.contains(&current)) {
        return None;
    }
    // Some regions are named after their platform (KR); most are not (EUW).
    let region = if platform_of(platform).is_some() {
        platform.to_owned()
    } else {
        regions
            .as_object()?
            .iter()
            .find(|(_, data)| data["rso_platform_id"] == platform)?
            .0
            .clone()
    };
    (region != current).then_some(region)
}

/// The League suspension in Riot's userinfo `ban.restrictions`, if any.
/// Chat, ranked and reputation restrictions still allow playing.
fn league_ban(info: &serde_json::Value, now_millis: u64) -> Option<Ban> {
    let mut ban: Option<Ban> = None;
    for restriction in info["ban"]["restrictions"].as_array()? {
        if restriction["scope"] != "lol" {
            continue;
        }
        let until = match restriction["type"].as_str() {
            Some("PERMANENT_BAN") => None,
            Some("TIME_BAN") => match restriction["dat"]["expirationMillis"].as_u64() {
                Some(until) if until > now_millis => Some(until),
                _ => continue,
            },
            _ => continue,
        };
        // A permanent ban outlasts any timed one; otherwise the latest end wins.
        ban = Some(Ban {
            until: match (ban, until) {
                (Some(Ban { until: None }), _) | (_, None) => None,
                (Some(Ban { until: Some(other) }), Some(until)) => Some(other.max(until)),
                (None, until) => until,
            },
        });
    }
    ban
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Hand the League suspension Riot reports for the signed-in account to
/// `on_ban`. Riot disconnects a suspended account as soon as the game starts,
/// so stop before launching one unless `on_ban` returns true. An unreadable
/// userinfo is not reported and does not block the login.
fn check_ban(
    game: Option<Game>,
    on_ban: &mut dyn FnMut(Option<Ban>) -> bool,
) -> Result<(), LoginError> {
    let Some(info) = LocalApi::read().and_then(|api| api.userinfo()) else {
        return Ok(());
    };
    let ban = league_ban(&info, now_millis());
    let launch_banned = on_ban(ban);
    match ban {
        Some(ban) if game.is_some() && !launch_banned => Err(LoginError::Banned(ban)),
        _ => Ok(()),
    }
}

/// League/TFT client processes (closing them only ends the lobby).
const CLIENT_PROCESSES: [&str; 4] = [
    "leagueclient.exe",
    "leagueclientux.exe",
    "leagueclientuxrender.exe",
    "tftclient.exe",
];
/// The in-match game process (closing it abandons the match).
const GAME_PROCESS: &str = "league of legends.exe";

struct Process {
    name: String,
    pid: u32,
}

fn running_processes() -> Vec<Process> {
    let mut command = std::process::Command::new("tasklist");
    command.args(["/FO", "CSV", "/NH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split("\",\"");
            Some(Process {
                name: fields.next()?.trim_matches('"').to_ascii_lowercase(),
                pid: fields.next()?.trim_matches('"').parse().ok()?,
            })
        })
        .collect()
}

/// Whether a match is being played, so an update restart can wait for it.
pub fn in_game() -> bool {
    running_processes().iter().any(|process| process.name == GAME_PROCESS)
}

fn game_username(processes: &[Process]) -> Option<String> {
    let mut username: Option<String> = None;
    for process in processes
        .iter()
        .filter(|process| matches!(process.name.as_str(), "leagueclient.exe" | "tftclient.exe"))
    {
        let executable = windows::process_path(process.pid)?;
        let contents = std::fs::read_to_string(executable.parent()?.join("lockfile")).ok()?;
        // Do not trust a lockfile left by a different process or installation.
        if contents.split(':').nth(1)?.parse::<u32>().ok()? != process.pid {
            return None;
        }
        let name = LocalApi::from_lockfile(&contents)?.game_username()?;
        if username
            .as_ref()
            .is_some_and(|previous| !previous.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        username = Some(name);
    }
    username
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameStatus {
    pub client_open: bool,
    pub in_game: bool,
    /// Username signed in to the Riot Client, when the API answers.
    pub signed_in_user: Option<String>,
    /// Username independently confirmed by every running game client's API.
    pub game_user: Option<String>,
}

impl GameStatus {
    pub fn league_running(&self) -> bool {
        self.client_open || self.in_game
    }

    pub fn signed_in_as(&self, account_id: &str) -> bool {
        let matches = |name: &Option<String>| {
            name.as_deref()
                .is_some_and(|name| !name.is_empty() && name.eq_ignore_ascii_case(account_id))
        };
        matches(&self.signed_in_user) && (!self.league_running() || matches(&self.game_user))
    }
}

/// Whether a game client exists; does not perform local API requests.
pub fn game_client_running() -> bool {
    running_processes().iter().any(|process| {
        matches!(process.name.as_str(), "leagueclient.exe" | "tftclient.exe")
    })
}

/// Only adopt profile metadata from a game session for this account.
pub fn signed_in_riot_id_for(account_id: &str) -> Option<String> {
    let processes = running_processes();
    let username = game_username(&processes)?;
    if !username.eq_ignore_ascii_case(account_id) { return None; }
    riot_id_from_processes(&processes)
}

/// The signed-in player's `GameName#TagLine`, from the game client's own API.
///
/// `/lol-summoner/v1/current-summoner` belongs to the League client, which
/// listens on its own port described by its own lockfile. Asking the Riot
/// Client's API for it always fails, which is why this asks through the same
/// path as [`game_username`]: the League client process, its install directory,
/// and a lockfile whose PID matches that process.
fn riot_id_from_processes(processes: &[Process]) -> Option<String> {
    let mut riot_id: Option<String> = None;
    for process in processes
        .iter()
        .filter(|process| matches!(process.name.as_str(), "leagueclient.exe" | "tftclient.exe"))
    {
        let executable = windows::process_path(process.pid)?;
        let contents = std::fs::read_to_string(executable.parent()?.join("lockfile")).ok()?;
        // Do not trust a lockfile left by a different process or installation.
        if contents.split(':').nth(1)?.parse::<u32>().ok()? != process.pid {
            return None;
        }
        let found = LocalApi::from_lockfile(&contents)?.riot_id()?;
        // Two clients on one machine must not disagree about who is signed in.
        if riot_id
            .as_ref()
            .is_some_and(|previous| !previous.eq_ignore_ascii_case(&found))
        {
            return None;
        }
        riot_id = Some(found);
    }
    riot_id
}

/// Read-only access to the League client's local API (champions, skins,
/// ranked stats, match history), handed out only while that client is signed
/// in as one specific account.
pub struct LeagueApi(LocalApi);

impl LeagueApi {
    /// The JSON body of a successful GET; `None` for any other answer.
    pub fn get_json(&self, path: &str) -> Option<serde_json::Value> {
        let (status, body) = self.0.get(path)?;
        (status == 200).then_some(body)
    }
}

/// The League client's API, only when every running game client confirms it
/// is signed in as `account_id`. Same lockfile checks as [`game_username`].
pub fn league_api_for(account_id: &str) -> Option<LeagueApi> {
    let processes = running_processes();
    let username = game_username(&processes)?;
    if !username.eq_ignore_ascii_case(account_id) {
        return None;
    }
    let process = processes.iter().find(|process| process.name == "leagueclient.exe")?;
    let executable = windows::process_path(process.pid)?;
    let contents = std::fs::read_to_string(executable.parent()?.join("lockfile")).ok()?;
    if contents.split(':').nth(1)?.parse::<u32>().ok()? != process.pid {
        return None;
    }
    // The champion inventory is several megabytes and match history is slow.
    LocalApi::from_lockfile_with_timeout(&contents, Duration::from_secs(30)).map(LeagueApi)
}

pub fn status() -> GameStatus {
    let processes = running_processes();
    let signed_in_user = LocalApi::read()
        .filter(|api| api.session() == Session::SignedIn)
        .and_then(|api| api.username());
    GameStatus {
        client_open: processes
            .iter()
            .any(|process| CLIENT_PROCESSES.contains(&process.name.as_str())),
        in_game: processes.iter().any(|process| process.name == GAME_PROCESS),
        signed_in_user,
        game_user: game_username(&processes),
    }
}

/// Close the League/TFT client and game: politely first, then forcefully.
fn close_league(cancel: &AtomicBool) -> Result<(), LoginError> {
    let names: Vec<&str> = CLIENT_PROCESSES
        .iter()
        .copied()
        .chain([GAME_PROCESS])
        .collect();
    let taskkill = |force: bool| {
        let mut command = std::process::Command::new("taskkill");
        if force {
            command.arg("/F");
        }
        // `/T` also ends the crash-handler and crashpad children, which are not
        // in `names` and would otherwise keep the tree alive.
        command.arg("/T");
        for name in &names {
            command.args(["/IM", name]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let _ = command.output();
    };
    let closed = || {
        let processes = running_processes();
        !processes
            .iter()
            .any(|process| names.contains(&process.name.as_str()))
    };
    close_league_with(cancel, taskkill, closed, || {
        thread::sleep(Duration::from_millis(500));
    })
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), LoginError> {
    if cancel.load(Ordering::SeqCst) {
        Err(LoginError::Cancelled)
    } else {
        Ok(())
    }
}

fn close_league_with(
    cancel: &AtomicBool,
    mut taskkill: impl FnMut(bool),
    mut closed: impl FnMut() -> bool,
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    check_cancelled(cancel)?;
    taskkill(false);
    for attempt in 0..20 {
        wait();
        check_cancelled(cancel)?;
        if closed() {
            return Ok(());
        }
        if attempt == 10 {
            check_cancelled(cancel)?;
            taskkill(true);
        }
    }
    check_cancelled(cancel)?;
    if closed() {
        Ok(())
    } else {
        Err(LoginError::CloseFailed)
    }
}

/// What to do once the account is signed in.
#[derive(Clone, Copy, Debug, Default)]
pub struct LaunchOptions<'a> {
    /// Product to start after signing in; `None` leaves the client open.
    pub game: Option<Game>,
    /// Product locale to apply before the launch request.
    pub locale: Option<&'a str>,
    /// Close a League client that belongs to another account.
    pub close_running: bool,
    /// Block the VN identity-provider domains in `hosts` for this login and
    /// restore them afterwards. Riot then skips its "update your profile"
    /// flow, which otherwise hangs a VN account until its retry limit.
    pub skip_profile_update: bool,
}

/// Sign in through Riot's native form, then launch the requested game.
/// `on_ban` receives the League suspension Riot reports once signed in and
/// returns whether a suspended account may still launch the game.
pub fn login(
    account_id: &str,
    password: &str,
    options: LaunchOptions<'_>,
    progress: &mut dyn FnMut(LoginStep),
    on_update: &mut dyn FnMut(Option<UpdateProgress>),
    on_ban: &mut dyn FnMut(Option<Ban>) -> bool,
    cancel: &AtomicBool,
) -> Result<LoginOutcome, LoginError> {
    let LaunchOptions {
        game,
        locale,
        close_running,
        skip_profile_update,
    } = options;
    check_cancelled(cancel)?;
    // The patch must stay in place for the whole login: Riot's profile flow
    // runs during it, not before. Restored in a scope guard so an error or
    // cancellation cannot leave the machine patched.
    let _vn_patch = crate::vn_hosts::Guard::new(skip_profile_update)
        .map_err(|_| LoginError::HostsPatchFailed)?;
    let path = client_path().ok_or(LoginError::ClientMissing)?;
    let current = status();
    check_cancelled(cancel)?;
    if current.league_running() && (close_running || !current.signed_in_as(account_id)) {
        if !close_running {
            return Err(LoginError::LeagueRunning);
        }
        progress(LoginStep::CloseLeague);
        close_league(cancel)?;
    }
    // Otherwise a running game already belongs to this account.
    let game_open = current.league_running() && !close_running;
    // Keep startup verification visible; temporary renderer ownership starts
    // only after the lifecycle confirms that the client is signed out.
    let mut client = renderer::Renderer::new(&path);
    progress(LoginStep::OpenClient);
    client.ensure_service(cancel)?;
    let started = Instant::now();
    let mut sign_out_sent: Option<Instant> = None;
    let mut verification_shown = false;
    let mut diagnostics = LoginDiagnostics::default();
    loop {
        check_cancelled(cancel)?;
        if started.elapsed() > LOGIN_TIMEOUT {
            return Err(LoginError::Timeout);
        }
        if let Some(api) = LocalApi::read() {
            match api.session() {
                Session::SignedIn => match api.username() {
                    Some(name) if name.eq_ignore_ascii_case(account_id) => {
                        client.restore()?;
                        check_cancelled(cancel)?;
                        check_ban(game, on_ban)?;
                        if let Some(game) = game {
                            if !game_open {
                                await_product_context(&mut client, progress, cancel)?;
                            }
                            progress(LoginStep::LaunchGame);
                            start_game(game, account_id, locale, progress, on_update, cancel)?;
                        }
                        progress(LoginStep::Done);
                        return Ok(LoginOutcome::AlreadySignedIn);
                    }
                    Some(_) if sign_out_sent.is_none() => {
                        // Signing in needs a client this app can restart; do
                        // not sign the other account out without one.
                        client.ensure_controllable()?;
                        progress(LoginStep::SignOut);
                        api.sign_out(cancel)?;
                        sign_out_sent = Some(Instant::now());
                    }
                    Some(_)
                        if sign_out_sent
                            .is_some_and(|sent| sent.elapsed() > Duration::from_secs(15)) =>
                    {
                        return Err(LoginError::SignOutFailed)
                    }
                    _ => diagnostics.waiting(Reason::AuthIdentityPending),
                },
                Session::SignedOut => break,
                Session::Unknown(reason) => {
                    if reason == Reason::AuthInteraction && !verification_shown {
                        progress(LoginStep::Verification);
                        verification_shown = true;
                    }
                    diagnostics.waiting(reason);
                }
            }
        }
        thread::sleep(POLL);
    }
    renderer::sign_in(&mut client, account_id, password, progress, cancel)?;
    // Dropping `client` on a ban reopens the normal Riot window.
    check_ban(game, on_ban)?;
    if let Some(game) = game {
        await_product_context(&mut client, progress, cancel)?;
        // The game replaces the Riot window, so do not reopen it in between.
        // If the launch fails or is cancelled, dropping `client` reopens it.
        client.close_debugger()?;
        check_cancelled(cancel)?;
        progress(LoginStep::LaunchGame);
        start_game(game, account_id, locale, progress, on_update, cancel)?;
        client.replaced_by_game();
    } else {
        // No launch now, but the next manual start should already be in the
        // account's language, so apply it to the product the user plays.
        if let Some(locale) = locale {
            apply_locale(Game::League, locale, cancel);
        }
        client.restore()?;
        check_cancelled(cancel)?;
    }
    progress(LoginStep::Done);
    Ok(LoginOutcome::SignedIn)
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    };

    /// Whether Windows refuses to let this process end `pid`, as for a process
    /// running as administrator when this one is not. An exited process is
    /// not refused.
    pub fn terminate_denied(pid: u32) -> bool {
        // SAFETY: plain terminate-right probe, closed below; nothing is ended.
        let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            // SAFETY: reads this thread's error from the call above.
            return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
        }
        // SAFETY: handle opened above.
        unsafe { CloseHandle(process) };
        false
    }

    pub fn process_path(pid: u32) -> Option<std::path::PathBuf> {
        // SAFETY: plain query handle, closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        // SAFETY: buffer and length describe a writable UTF-16 buffer.
        let ok =
            unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) };
        // SAFETY: handle opened above.
        unsafe { CloseHandle(process) };
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        Some(std::path::PathBuf::from(path))
    }

    /// Hide the Riot Client's window, leaving it running in the tray.
    ///
    /// Riot's renderer calls `UxService.minimize()` once the launch request is
    /// accepted, which keeps a taskbar button around. The client owns a tray
    /// icon for as long as it runs, so the window is hidden instead: it leaves
    /// the taskbar entirely and comes back from that icon, the same way its
    /// own "run in background" mode behaves.
    pub fn hide_riot_client() {
        use windows_sys::Win32::Foundation::LPARAM;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, IsWindowVisible, ShowWindow, SW_HIDE,
        };

        let targets: Vec<u32> = crate::riot_client::running_processes()
            .into_iter()
            .filter(|process| process.name == "riot client.exe")
            .map(|process| process.pid)
            .collect();
        if targets.is_empty() {
            return;
        }

        struct Search<'a> {
            targets: &'a [u32],
        }
        unsafe extern "system" fn callback(window: *mut core::ffi::c_void, param: LPARAM) -> i32 {
            // SAFETY: `param` is the `&mut Search` passed to `EnumWindows`.
            let search = unsafe { &mut *(param as *mut Search) };
            let mut pid = 0u32;
            // SAFETY: plain query of the window's owning process.
            unsafe { GetWindowThreadProcessId(window, &mut pid) };
            if search.targets.contains(&pid) && unsafe { IsWindowVisible(window) } != 0 {
                // SAFETY: `window` came from the enumeration.
                unsafe { ShowWindow(window, SW_HIDE) };
            }
            1
        }

        let mut search = Search { targets: &targets };
        // SAFETY: the callback only reads `search`, which outlives the call.
        unsafe {
            EnumWindows(Some(callback), &mut search as *mut Search as LPARAM);
        }
    }

    /// Arguments `pid` was started with, split as Windows splits them.
    pub fn process_args(pid: u32) -> Option<Vec<std::ffi::OsString>> {
        use std::mem::size_of;
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Wdk::System::Threading::{
            NtQueryInformationProcess, ProcessCommandLineInformation,
        };
        use windows_sys::Win32::Foundation::{LocalFree, UNICODE_STRING};
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

        // SAFETY: plain query handle, closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let mut bytes = 0;
        // SAFETY: the empty-buffer call only reports the required size.
        unsafe {
            NtQueryInformationProcess(
                process,
                ProcessCommandLineInformation,
                std::ptr::null_mut(),
                0,
                &mut bytes,
            )
        };
        // Word-aligned for the UNICODE_STRING header that starts the buffer.
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        // SAFETY: buffer and size describe a writable, word-aligned buffer.
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<usize>()) as u32,
                &mut bytes,
            )
        };
        // SAFETY: handle opened above.
        unsafe { CloseHandle(process) };
        if status < 0 || buffer.len() * size_of::<usize>() < size_of::<UNICODE_STRING>() {
            return None;
        }
        // SAFETY: on success the buffer starts with a UNICODE_STRING whose
        // text lies inside the same buffer.
        let mut line = unsafe {
            let text = &*buffer.as_ptr().cast::<UNICODE_STRING>();
            if text.Buffer.is_null() {
                return None;
            }
            std::slice::from_raw_parts(text.Buffer, text.Length as usize / 2).to_vec()
        };
        if line.is_empty() {
            return None;
        }
        line.push(0);
        let mut count = 0;
        // SAFETY: `line` is a terminated UTF-16 string.
        let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut count) };
        if argv.is_null() {
            return None;
        }
        let args = (0..count as usize)
            .map(|index| {
                // SAFETY: Windows returned `count` terminated strings.
                unsafe {
                    let arg = *argv.add(index);
                    let length = (0..).take_while(|&unit| *arg.add(unit) != 0).count();
                    std::ffi::OsString::from_wide(std::slice::from_raw_parts(arg, length))
                }
            })
            .collect();
        // SAFETY: CommandLineToArgvW allocates the array with LocalAlloc.
        unsafe { LocalFree(argv.cast()) };
        Some(args)
    }
}

#[cfg(not(windows))]
mod windows {
    pub fn terminate_denied(_: u32) -> bool {
        false
    }

    pub fn hide_riot_client() {}

    pub fn process_path(_: u32) -> Option<std::path::PathBuf> {
        None
    }

    pub fn process_args(_: u32) -> Option<Vec<std::ffi::OsString>> {
        None
    }
}
