//! Own the entire Riot login attempt, including startup and renderer cleanup.
#[path = "riot_process.rs"]
mod owned_process;

use super::{
    check_cancelled, running_processes, windows, LocalApi, LoginError, LoginStep, Process, Session,
};
use crate::logging::{self, Event, Reason};
use owned_process::OwnedProcess;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn path_arg(name: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(name);
    arg.push(path);
    arg
}

/// Only the elevation category of a failed start is kept, never its text.
fn launch_error(error: &io::Error, fallback: LoginError) -> LoginError {
    const ERROR_ELEVATION_REQUIRED: i32 = 740;
    if error.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) {
        LoginError::RiotElevated
    } else {
        fallback
    }
}

fn start_service(path: &Path, headless: bool, kept: &[OsString]) -> Result<(), LoginError> {
    let mut process = command(path);
    if headless {
        process.arg("--headless");
    }
    process
        .args(kept)
        .spawn()
        .map(|_| ())
        .map_err(|error| launch_error(&error, LoginError::LaunchFailed))
}

fn wait(cancel: &AtomicBool, duration: Duration) -> Result<(), LoginError> {
    let start = Instant::now();
    while start.elapsed() < duration {
        check_cancelled(cancel)?;
        thread::sleep(Duration::from_millis(50));
    }
    check_cancelled(cancel)
}

fn belongs_to_installation(path: &Path, root: &Path) -> bool {
    // Riot's install metadata uses '/', while Windows process paths use '\\'.
    // Compare path components so either separator works and sibling folders
    // cannot match merely because their names share a prefix.
    let path = PathBuf::from(path.as_os_str().to_ascii_lowercase());
    let root = PathBuf::from(root.as_os_str().to_ascii_lowercase());
    path.starts_with(root)
}

/// How long a client tree may take to exit after `taskkill /F /T`.
/// Riot's crash handler and renderer children can take a few seconds each.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Kill sweeps before giving up; covers the service re-launching its UI once.
const MAX_SWEEPS: u32 = 4;

/// Client processes of this installation that are still alive.
///
/// A process that is exiting has no queryable path for a moment, so a missing
/// path is treated as "already gone" and simply skipped. Failing there turned
/// every kill into a hard error: the process is in the task list, but
/// `OpenProcess` can no longer open it.
fn client_processes(root: &Path) -> Result<Vec<Process>, LoginError> {
    let mut processes = Vec::new();
    for process in running_processes().into_iter().filter(|p| {
        matches!(
            p.name.as_str(),
            "riotclientservices.exe" | "riot client.exe"
        )
    }) {
        let Some(path) = windows::process_path(process.pid) else {
            if running_processes().iter().any(|p| p.pid == process.pid) {
                return Err(LoginError::RiotCloseFailed);
            }
            continue;
        };
        if belongs_to_installation(&path, root) {
            processes.push(process);
        }
    }
    Ok(processes)
}

/// Whether a restarted client needs `arg` to work as it did. Chat proxies
/// such as Deceive start the client with their own configuration server;
/// restarting without it would show the player online.
fn kept_arg(arg: &OsString) -> bool {
    arg.to_str().is_some_and(|arg| arg.starts_with("--client-config-url="))
}

/// The running service's arguments that a restart must keep.
fn service_args(path: &Path) -> Result<Vec<OsString>, LoginError> {
    let root = path.parent().ok_or(LoginError::ClientMissing)?;
    let mut kept = Vec::new();
    for process in client_processes(root)?
        .iter()
        .filter(|p| p.name == "riotclientservices.exe")
    {
        let Some(args) = windows::process_args(process.pid) else {
            if running_processes().iter().any(|p| p.pid == process.pid) {
                return Err(LoginError::RiotCloseFailed);
            }
            continue;
        };
        for arg in args.into_iter().skip(1).filter(kept_arg) {
            if !kept.contains(&arg) {
                kept.push(arg);
            }
        }
    }
    Ok(kept)
}

/// End every client process of this installation.
///
/// `taskkill /F /PID` ends only the named process: Riot's UX spawns renderer,
/// GPU, utility and crash-handler children, and `RiotClientServices` restarts
/// its UI. The children keep the tree alive, so `/T` is required to take the
/// whole installation down.
fn stop_client(path: &Path, cancel: &AtomicBool) -> Result<(), LoginError> {
    let root = path.parent().ok_or(LoginError::ClientMissing)?;
    stop_client_with(
        cancel,
        CLOSE_TIMEOUT,
        || client_processes(root),
        |processes| {
            // Exact processes under this Riot installation only; never game
            // processes. One call keeps the restart quick: Deceive exits when
            // no Riot Client replaces its own within 3 seconds.
            let mut taskkill = command(Path::new("taskkill.exe"));
            taskkill.args(["/F", "/T"]);
            for process in processes {
                taskkill.args(["/PID", &process.pid.to_string()]);
            }
            let _ = taskkill.status();
        },
        || {
            let _ = wait(cancel, Duration::from_millis(200));
        },
    )
}

/// Sweep the client tree until it is gone. A survivor after the first sweep is
/// killed again, because Riot's service owns the UI and can restart it.
fn stop_client_with(
    cancel: &AtomicBool,
    timeout: Duration,
    mut remaining: impl FnMut() -> Result<Vec<Process>, LoginError>,
    mut kill: impl FnMut(&[Process]),
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    check_cancelled(cancel)?;
    let start = Instant::now();
    let mut sweeps = 0;
    loop {
        let processes = remaining()?;
        if processes.is_empty() {
            return Ok(());
        }
        // Bounded: a tree that refuses to die is reported, not hammered.
        if sweeps >= MAX_SWEEPS || start.elapsed() > timeout {
            return Err(LoginError::RiotCloseFailed);
        }
        sweeps += 1;
        kill(&processes);
        wait();
        check_cancelled(cancel)?;
    }
}

pub(super) fn debugger_url(value: &str, port: u16) -> bool {
    port != 0
        && reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "ws"
                && url.host_str() == Some("127.0.0.1")
                && url.port() == Some(port)
                && url.username().is_empty()
                && url.password().is_none()
                && url.path().starts_with("/devtools/page/")
                && url.query().is_none()
                && url.fragment().is_none()
        })
}

struct Cdp {
    socket: WebSocket<TcpStream>,
    sequence: u64,
}

impl Cdp {
    fn connect(url: &str, port: u16) -> Result<Self, LoginError> {
        if !debugger_url(url, port) {
            return Err(LoginError::RendererFailed);
        }
        let stream =
            TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))
                .map_err(|_| LoginError::RendererFailed)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| LoginError::RendererFailed)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| LoginError::RendererFailed)?;
        let (socket, _) =
            tungstenite::client(url, stream).map_err(|_| LoginError::RendererFailed)?;
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| LoginError::RendererFailed)?;
        Ok(Self {
            socket,
            sequence: 0,
        })
    }

    fn eval(&mut self, expression: &str, cancel: &AtomicBool) -> Result<Value, LoginError> {
        check_cancelled(cancel)?;
        self.sequence += 1;
        // Expressions may contain credentials. Neither requests nor errors are logged.
        self.socket
            .send(Message::Text(
                json!({"id": self.sequence, "method":"Runtime.evaluate",
            "params":{"expression":expression,"returnByValue":true,"awaitPromise":true}})
                .to_string()
                .into(),
            ))
            .map_err(|_| LoginError::RendererFailed)?;
        let start = Instant::now();
        loop {
            check_cancelled(cancel)?;
            if start.elapsed() > Duration::from_secs(15) {
                return Err(LoginError::RendererFailed);
            }
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let response: Value =
                        serde_json::from_str(&text).map_err(|_| LoginError::RendererFailed)?;
                    if response["id"].as_u64() != Some(self.sequence) {
                        continue;
                    }
                    if response.get("error").is_some()
                        || response["result"].get("exceptionDetails").is_some()
                    {
                        return Err(LoginError::RendererFailed);
                    }
                    return Ok(response["result"]["result"]["value"].clone());
                }
                Ok(Message::Close(_)) => return Err(LoginError::RendererFailed),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => return Err(LoginError::RendererFailed),
            }
        }
    }
}

pub(super) struct Renderer {
    service: PathBuf,
    child: Option<OwnedProcess>,
    cdp: Option<Cdp>,
    normal: Option<(PathBuf, Vec<OsString>, String)>,
    restore_needed: bool,
    /// Arguments of the service this login replaced, for every later start.
    service_args: Vec<OsString>,
    // One seam for deterministic lifecycle tests without launching Riot.
    service_start: fn(&Path, bool, &[OsString]) -> Result<(), LoginError>,
}

impl Renderer {
    pub(super) fn new(service: &Path) -> Self {
        Self {
            service: service.to_owned(),
            child: None,
            cdp: None,
            normal: None,
            restore_needed: false,
            service_args: Vec::new(),
            service_start: start_service,
        }
    }

    pub(super) fn ensure_service(&mut self, cancel: &AtomicBool) -> Result<(), LoginError> {
        check_cancelled(cancel)?;
        let root = self.service.parent().ok_or(LoginError::ClientMissing)?;
        let processes = client_processes(root)?;
        // A parseable lockfile is discovery data, not evidence of a live
        // service. Check executable ownership even when the API is starting.
        self.ensure_service_with(
            processes.iter().any(|p| p.name == "riotclientservices.exe"),
            processes.iter().any(|p| p.name == "riot client.exe"),
            cancel,
        )
    }

    fn ensure_service_with(
        &mut self,
        service_running: bool,
        renderer_running: bool,
        cancel: &AtomicBool,
    ) -> Result<(), LoginError> {
        check_cancelled(cancel)?;
        if !service_running || !renderer_running {
            // Startup can require MFA or another interactive step before the
            // lifecycle becomes signed out. Keep the normal UI available until
            // sign_in owns a temporary renderer for a confirmed login form.
            (self.service_start)(&self.service, false, &self.service_args)?;
        }
        Ok(())
    }

    /// An elevated Riot Client can be neither closed nor reopened from here.
    /// Check before closing or signing anything out, and leave it as it is.
    pub(super) fn ensure_controllable(&self) -> Result<(), LoginError> {
        let root = self.service.parent().ok_or(LoginError::ClientMissing)?;
        if client_processes(root)?
            .iter()
            .any(|process| windows::terminate_denied(process.pid))
        {
            return Err(LoginError::RiotElevated);
        }
        Ok(())
    }

    /// End the temporary debug renderer without opening a normal window yet.
    /// A later `restore` (or drop) still opens one unless `replaced_by_game`.
    pub(super) fn close_debugger(&mut self) -> Result<(), LoginError> {
        if let Some(cdp) = self.cdp.as_mut() {
            let _ = cdp.socket.send(Message::Text(
                json!({"id":0,"method":"Browser.close"}).to_string().into(),
            ));
        }
        if let Some(child) = self.child.as_ref() {
            let start = Instant::now();
            while !child.has_exited().unwrap_or(true)
                && start.elapsed() < Duration::from_secs(3)
            {
                thread::sleep(Duration::from_millis(100));
            }
        }
        self.cdp = None;
        if let Some(mut child) = self.child.take() {
            child.close().map_err(|_| LoginError::RendererFailed)?;
        }
        Ok(())
    }

    /// The launched game took over from the Riot window: reopening it would
    /// look like the Riot Client restarting for no reason.
    pub(super) fn replaced_by_game(&mut self) {
        self.restore_needed = false;
    }

    pub(super) fn restore(&mut self) -> Result<(), LoginError> {
        if !self.restore_needed {
            return Ok(());
        }
        self.close_debugger()?;
        let restored = self.normal.as_ref().is_some_and(|(path, args, endpoint)| {
            LocalApi::read().is_some_and(|api| {
                &api.base == endpoint && api.session() != Session::Unknown(Reason::AuthUnavailable)
            }) && command(path).args(args).spawn().is_ok()
        });
        if !restored {
            (self.service_start)(&self.service, false, &self.service_args)?;
        }
        self.restore_needed = false;
        Ok(())
    }

    /// Bring the normal Riot window forward for a step that needs the player.
    pub(super) fn show(&mut self) -> Result<(), LoginError> {
        if self.restore_needed {
            return self.restore();
        }
        // An already signed-in client has no temporary renderer to replace;
        // starting the service again opens or raises its normal window.
        (self.service_start)(&self.service, false, &self.service_args)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Covers renderer setup, MFA, cancellation, unwinding, suspended
        // accounts and failed game launches. Successful paths restore first or are replaced by the game.
        // The login result is logged separately; this is only the cleanup.
        if let Err(error) = self.restore() {
            logging::record(Event::RiotRestoreFailed, error.reason());
        }
    }
}

pub(super) fn auth_result(value: &str) -> Result<(), LoginError> {
    match value {
        "pending" | "success" | "verification" => Ok(()),
        "captcha_rejected" => Err(LoginError::CaptchaRejected),
        "rate_limited" => Err(LoginError::RateLimited),
        "rejected" => Err(LoginError::AuthRejected),
        _ => Err(LoginError::RendererFailed),
    }
}

pub(super) fn sign_in(
    renderer: &mut Renderer,
    username: &str,
    password: &str,
    progress: &mut dyn FnMut(LoginStep),
    cancel: &AtomicBool,
) -> Result<(), LoginError> {
    if password.is_empty() {
        return Err(LoginError::AuthRejected);
    }
    check_cancelled(cancel)?;
    let service = renderer.service.clone();
    progress(LoginStep::SignIn);
    renderer.ensure_controllable()?;
    renderer.service_args = service_args(&service)?;
    renderer.restore_needed = true;
    stop_client(&service, cancel)?;
    (renderer.service_start)(&service, true, &renderer.service_args)?;
    let start = Instant::now();
    let api = loop {
        check_cancelled(cancel)?;
        if start.elapsed() > Duration::from_secs(45) {
            return Err(LoginError::Timeout);
        }
        if let Some(api) = LocalApi::read() {
            match api.session() {
                Session::SignedOut => break api,
                Session::SignedIn => {
                    if api
                        .username()
                        .is_some_and(|name| name.eq_ignore_ascii_case(username))
                    {
                        return Ok(());
                    }
                    return Err(LoginError::AccountMismatch);
                }
                _ => {}
            }
        }
        wait(cancel, Duration::from_millis(300))?;
    };
    let root = service.parent().ok_or(LoginError::ClientMissing)?;
    let ux = root.join("RiotClientElectron").join("Riot Client.exe");
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or(LoginError::RendererFailed)?;
    let data = local.join("Riot Games").join("Riot Client");
    let lock = std::fs::read_to_string(data.join("Config").join("lockfile"))
        .map_err(|_| LoginError::RendererFailed)?;
    let pid = lock
        .split(':')
        .nth(1)
        .and_then(|p| p.parse::<u32>().ok())
        .ok_or(LoginError::RendererFailed)?;
    let machine = api
        .get("/riotclient/machine-id")
        .filter(|(code, _)| *code == 200)
        .and_then(|(_, v)| v.as_str().map(str::to_owned))
        .ok_or(LoginError::RendererFailed)?;
    let session = api
        .get("/product-session/v1/host-session/id")
        .filter(|(code, _)| *code == 200)
        .and_then(|(_, v)| v.as_str().map(str::to_owned))
        .ok_or(LoginError::RendererFailed)?;
    let args: Vec<OsString> = vec![
        format!(
            "--app-port={}",
            api.base
                .rsplit(':')
                .next()
                .ok_or(LoginError::RendererFailed)?
        )
        .into(),
        format!("--remoting-auth-token={}", api.password).into(),
        format!("--app-pid={pid}").into(),
        format!("--machine-id={machine}").into(),
        format!("--session-id={session}").into(),
        path_arg("--app-root=", root),
        path_arg("--user-data-root=", &data),
        path_arg("--log-dir=", &data.join("Logs").join("Riot Client UX Logs")),
        "--crashpad-environment=prod".into(),
        "--enable-hardware-acceleration".into(),
    ];
    renderer.normal = Some((ux.clone(), args.clone(), api.base.clone()));
    // Port 0 in Chromium enables its automation mode; pass a concrete port.
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| LoginError::RendererFailed)?;
    let port = listener
        .local_addr()
        .map_err(|_| LoginError::RendererFailed)?
        .port();
    drop(listener);
    check_cancelled(cancel)?;
    let mut debug_args = args;
    debug_args.extend([
        OsString::from("--allow-chrome-dev-tools"),
        OsString::from("--remote-debugging-address=127.0.0.1"),
        OsString::from(format!("--remote-debugging-port={port}")),
    ]);
    renderer.child = Some(
        OwnedProcess::spawn(&ux, &debug_args)
            .map_err(|error| launch_error(&error, LoginError::RendererFailed))?,
    );
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| LoginError::RendererFailed)?;
    let start = Instant::now();
    let endpoint = loop {
        check_cancelled(cancel)?;
        if start.elapsed() > Duration::from_secs(30) {
            return Err(LoginError::RendererFailed);
        }
        if renderer
            .child
            .as_ref()
            .is_some_and(|child| child.has_exited().unwrap_or(true))
        {
            return Err(LoginError::RendererFailed);
        }
        let targets = http
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .ok()
            .and_then(|r| r.text().ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        if let Some(target) = targets
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|t| t["type"] == "page" && t["title"] == "Riot Client")
            })
        {
            if let Some(url) = target["webSocketDebuggerUrl"]
                .as_str()
                .filter(|url| debugger_url(url, port))
            {
                break url.to_owned();
            }
        }
        wait(cancel, Duration::from_millis(200))?;
    };
    renderer.cdp = Some(Cdp::connect(&endpoint, port)?);
    let cdp = renderer.cdp.as_mut().ok_or(LoginError::RendererFailed)?;
    cdp.eval(include_str!("riot_renderer.js"), cancel)?;
    let start = Instant::now();
    while cdp.eval("globalThis.__leagueAccountsLogin.ready()", cancel)? != true {
        if start.elapsed() > Duration::from_secs(30) {
            return Err(LoginError::RendererFailed);
        }
        wait(cancel, Duration::from_millis(200))?;
    }
    // Allow the native authentication initialization to settle before filling.
    wait(cancel, Duration::from_millis(1500))?;
    let current = LocalApi::read().ok_or(LoginError::RendererFailed)?;
    if current.base != api.base || current.session() != Session::SignedOut {
        return Err(LoginError::AccountMismatch);
    }
    let mut expression = format!(
        "globalThis.__leagueAccountsLogin.fill(...{})",
        json!([username, password])
    );
    let filled = cdp.eval(&expression, cancel);
    expression.clear();
    if filled? != true {
        return Err(LoginError::RendererFailed);
    }
    if cdp.eval("globalThis.__leagueAccountsLogin.submit()", cancel)? != true {
        return Err(LoginError::RendererFailed);
    }
    let start = Instant::now();
    let mut verification = false;
    loop {
        check_cancelled(cancel)?;
        if start.elapsed() > Duration::from_secs(300) {
            return Err(LoginError::Timeout);
        }
        let current = LocalApi::read().ok_or(LoginError::RendererFailed)?;
        if current.base != api.base {
            return Err(LoginError::RendererFailed);
        }
        if current.session() == Session::SignedIn {
            if let Some(name) = current.username() {
                return if name.eq_ignore_ascii_case(username) {
                    Ok(())
                } else {
                    Err(LoginError::AccountMismatch)
                };
            }
        }
        let state = cdp.eval("globalThis.__leagueAccountsLogin.status()", cancel)?;
        let state = state.as_str().ok_or(LoginError::RendererFailed)?;
        auth_result(state)?;
        let multifactor = !verification
            && current
                .get("/rso-authenticator/v1/authentication")
                .is_some_and(|(status, body)| status == 200 && body["type"] == "multifactor");
        if (state == "verification" || multifactor) && !verification {
            cdp.eval(
                "globalThis.__leagueAccountsLogin.showVerification()",
                cancel,
            )?;
            progress(LoginStep::Verification);
            verification = true;
        }
        wait(cancel, Duration::from_millis(300))?;
    }
}
