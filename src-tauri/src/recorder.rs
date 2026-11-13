//! Game recorder glue: preferences (`recorder.json`, off by default), a light
//! watcher that starts a recording when a League game starts and stops it when
//! the game ends, the Recordings page commands, and the `larec` scheme that
//! plays videos from the chosen folder only.

use crate::refresh::Shared;
use crate::views::{fail, fail_with, CommandResult};
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::recorder::capture::find_game_window;
use leagueaccounts::recorder::library::{self, Entry, UNKNOWN};
use leagueaccounts::recorder::session::{self, Outcome, RecordRequest, RecordSettings, Recording};
use leagueaccounts::riot_client;
use leagueaccounts::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecorderPrefs {
    pub enabled: bool,
    /// Main folder; empty = Videos\League Companion.
    pub folder: String,
    /// light, balanced, high or custom.
    pub preset: String,
    /// Custom: largest video height (0 = the window's size), frames per second, Mbps.
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    /// auto (hardware first) or software.
    pub encoder: String,
    /// window (Windows Graphics Capture) or screen (Desktop Duplication).
    pub capture: String,
    pub game_audio: bool,
    pub microphone: bool,
    /// Gigabytes for all recordings; 0 = no limit.
    pub disk_limit_gb: u32,
}

impl Default for RecorderPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            folder: String::new(),
            preset: "light".into(),
            height: 1080,
            fps: 30,
            bitrate: 6,
            encoder: "auto".into(),
            capture: "window".into(),
            game_audio: true,
            microphone: false,
            disk_limit_gb: 100,
        }
    }
}

impl RecorderPrefs {
    /// (max height, fps, Mbps) for the chosen quality.
    fn quality(&self) -> (u32, u32, u32) {
        match self.preset.as_str() {
            "balanced" => (1080, 60, 10),
            "high" => (0, 60, 16),
            "custom" => (self.height.min(4320), self.fps.clamp(10, 120), self.bitrate.clamp(1, 100)),
            _ => (1080, 30, 6),
        }
    }

    fn root(&self) -> PathBuf {
        if !self.folder.trim().is_empty() {
            return PathBuf::from(self.folder.trim());
        }
        std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default().join("Videos").join("League Companion")
    }

    fn sanitized(mut self) -> Self {
        if !matches!(self.preset.as_str(), "light" | "balanced" | "high" | "custom") {
            self.preset = "light".into();
        }
        if self.encoder != "software" {
            self.encoder = "auto".into();
        }
        if self.capture != "screen" {
            self.capture = "window".into();
        }
        self.height = self.height.min(4320);
        self.fps = self.fps.clamp(10, 120);
        self.bitrate = self.bitrate.clamp(1, 100);
        self.disk_limit_gb = self.disk_limit_gb.min(100_000);
        self
    }
}

fn prefs_path() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("recorder.json"))
}

static PREFS: Mutex<Option<RecorderPrefs>> = Mutex::new(None);

fn prefs() -> RecorderPrefs {
    let mut cached = PREFS.lock().unwrap_or_else(|error| error.into_inner());
    cached
        .get_or_insert_with(|| {
            prefs_path()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<RecorderPrefs>(&bytes).ok())
                .unwrap_or_default()
                .sanitized()
        })
        .clone()
}

/// What the interface shows about the recorder right now.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStatus {
    pub enabled: bool,
    pub recording: bool,
    pub account: String,
    /// Unix ms.
    pub started_at: u64,
    /// The file being written, relative to the root.
    pub path: String,
    /// The last recording's error code, when it failed.
    pub error: String,
}

struct Active {
    recording: Recording,
    window: isize,
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static STATUS: Mutex<Option<RecorderStatus>> = Mutex::new(None);

fn set_status(app: &AppHandle, status: RecorderStatus) {
    if let Ok(mut current) = STATUS.lock() {
        *current = Some(status.clone());
    }
    let _ = app.emit("recorder-status", status);
}

fn status() -> RecorderStatus {
    let mut status = STATUS.lock().ok().and_then(|status| status.clone()).unwrap_or_default();
    status.enabled = prefs().enabled;
    status
}

/// The saved account matching the signed-in Riot ID, as a folder name.
fn account_folder(app: &AppHandle, game_name: &str, tag_line: &str) -> (String, String) {
    let riot_id = if tag_line.is_empty() { game_name.to_owned() } else { format!("{game_name}#{tag_line}") };
    let wanted = riot_id.replace(' ', "").to_lowercase();
    let matched = app.try_state::<Arc<Shared>>().and_then(|state| {
        let manager = state.manager.lock().ok()?;
        manager
            .accounts
            .iter()
            .find(|account| !wanted.is_empty() && account.name.replace(' ', "").to_lowercase() == wanted)
            .map(|account| account.name.clone())
    });
    match matched {
        Some(name) => (library::safe_name(&name), riot_id),
        None => (UNKNOWN.to_owned(), riot_id),
    }
}

fn queue_name(session: &Value) -> String {
    let queue = &session["gameData"]["queue"];
    let id = queue["id"].as_i64().unwrap_or(-1);
    let name = match id {
        420 => "Ranked Solo",
        440 => "Ranked Flex",
        400 => "Normal Draft",
        430 => "Normal Blind",
        480 | 490 => "Quickplay",
        450 => "ARAM",
        900 | 1010 | 1900 => "URF",
        1700 | 1710 => "Arena",
        830 | 840 | 850 | 870 | 880 | 890 => "Co-op vs AI",
        0 => "Custom",
        _ => "",
    };
    if !name.is_empty() {
        return name.to_owned();
    }
    let mode = session["gameData"]["queue"]["gameMode"].as_str().or_else(|| session["map"]["gameMode"].as_str()).unwrap_or_default();
    match mode {
        "PRACTICETOOL" => "Practice Tool".into(),
        "" => String::new(),
        other => {
            let lower = other.to_lowercase();
            let mut chars = lower.chars();
            chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
    }
}

fn start_recording(app: &AppHandle, window: leagueaccounts::recorder::capture::WindowHandle, prefs: &RecorderPrefs) {
    let client = riot_client::league_client();
    let summoner = client.as_ref().and_then(|client| client.get_json("/lol-summoner/v1/current-summoner")).unwrap_or(Value::Null);
    let session_info = client.as_ref().and_then(|client| client.get_json("/lol-gameflow/v1/session")).unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let (account, riot_id) = account_folder(app, &text(&summoner["gameName"]), &text(&summoner["tagLine"]));
    let (max_height, fps, mbps) = prefs.quality();
    let request = RecordRequest {
        root: prefs.root(),
        account: account.clone(),
        riot_id,
        puuid: text(&summoner["puuid"]),
        queue: queue_name(&session_info),
        settings: RecordSettings {
            max_height,
            fps,
            bitrate: mbps * 1_000_000,
            hardware: prefs.encoder != "software",
            screen: prefs.capture == "screen",
            game_audio: prefs.game_audio,
            microphone: prefs.microphone,
        },
    };
    match session::start(request, window) {
        Ok(recording) => {
            logging::record(Event::RecordingStarted, Reason::None);
            let relative = recording.path.strip_prefix(prefs.root()).map(|path| path.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            set_status(app, RecorderStatus { enabled: true, recording: true, account, started_at: recording.started_at, path: relative, error: String::new() });
            if let Ok(mut active) = ACTIVE.lock() {
                *active = Some(Active { recording, window: window.0 as isize });
            }
        }
        Err(_) => {
            logging::record(Event::RecordingFailed, Reason::None);
            set_status(app, RecorderStatus { enabled: true, error: "file_write".into(), ..RecorderStatus::default() });
        }
    }
}

/// Wait for the recording thread, then apply the disk limit and tell the interface.
fn finish(app: &AppHandle, active: Active) {
    let outcome = active.recording.wait();
    let prefs = prefs();
    let error = match outcome {
        Outcome::Saved(_) => {
            logging::record(Event::RecordingSaved, Reason::None);
            String::new()
        }
        Outcome::Failed(code) => {
            logging::record(Event::RecordingFailed, Reason::None);
            code.to_owned()
        }
    };
    library::enforce_limit(&prefs.root(), u64::from(prefs.disk_limit_gb) * 1_000_000_000, None);
    set_status(app, RecorderStatus { enabled: prefs.enabled, error, ..RecorderStatus::default() });
    let _ = app.emit("recordings-changed", ());
}

fn in_game_phase() -> Option<bool> {
    let client = riot_client::league_client()?;
    let phase = client.get_json("/lol-gameflow/v1/gameflow-phase")?;
    Some(matches!(phase.as_str(), Some("GameStart" | "InProgress" | "Reconnect")))
}

/// Checks every 2 seconds while the recorder is on (nothing at all when off).
/// Recordings made as fragmented MP4 (before this version) are rewritten once
/// as normal MP4, one at a time in the background, so they open at once.
fn convert_old_recordings(app: &AppHandle) {
    let root = prefs().root();
    for entry in library::list(&root).into_iter().filter(|entry| entry.kind == "game") {
        let Some(path) = library::resolve(&root, &entry.path) else { continue };
        if is_being_recorded(&path) || !library::is_fragmented(&path) {
            continue;
        }
        if leagueaccounts::recorder::make_seekable(&path).is_ok() {
            let _ = app.emit("recordings-changed", ());
        }
    }
}

pub fn spawn(app: AppHandle) {
    {
        let app = app.clone();
        thread::spawn(move || {
            // After startup settles; quick when there is nothing to convert.
            thread::sleep(Duration::from_secs(15));
            convert_old_recordings(&app);
        });
    }
    thread::spawn(move || {
        // The window of the game that was just recorded: not recorded twice.
        let mut finished_window: Option<isize> = None;
        let mut limit_checked = std::time::Instant::now();
        loop {
            let prefs = prefs();
            let active = ACTIVE.lock().ok().and_then(|mut active| {
                let done = active.as_ref().is_some_and(|active| active.recording.is_finished() || !prefs.enabled);
                if done { active.take() } else { None }
            });
            if let Some(active) = active {
                finished_window = Some(active.window);
                finish(&app, active);
            }
            let recording = ACTIVE.lock().is_ok_and(|active| active.is_some());
            // A long game can pass the disk limit: check once a minute, never the file being written.
            if recording && limit_checked.elapsed() >= Duration::from_secs(60) {
                limit_checked = std::time::Instant::now();
                let current = ACTIVE.lock().ok().and_then(|active| active.as_ref().map(|active| active.recording.path.clone()));
                if library::enforce_limit(&prefs.root(), u64::from(prefs.disk_limit_gb) * 1_000_000_000, current.as_deref()) > 0 {
                    let _ = app.emit("recordings-changed", ());
                }
            }
            if !prefs.enabled {
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            let window = find_game_window();
            if window.map(|window| window.0 as isize) != finished_window {
                if window.is_none() {
                    finished_window = None;
                }
                if let (Some(window), false) = (window, recording) {
                    // A game window with the client in game (not a replay).
                    if in_game_phase().unwrap_or(false) {
                        start_recording(&app, window, &prefs);
                    }
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}

/// Stop recording when the app quits, so the file is finished properly.
pub fn stop_now() {
    let active = ACTIVE.lock().ok().and_then(|mut active| active.take());
    if let Some(active) = active {
        let _ = active.recording.wait();
    }
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderPrefsView {
    #[serde(flatten)]
    prefs: RecorderPrefs,
    /// The folder actually used (the default when `folder` is empty).
    root: String,
}

fn prefs_view(prefs: RecorderPrefs) -> RecorderPrefsView {
    let root = prefs.root().display().to_string();
    RecorderPrefsView { prefs, root }
}

#[tauri::command]
pub async fn recorder_prefs() -> CommandResult<RecorderPrefsView> {
    Ok(prefs_view(prefs()))
}

#[tauri::command]
pub async fn set_recorder_prefs(app: AppHandle, prefs: RecorderPrefs) -> CommandResult<RecorderPrefsView> {
    let prefs = prefs.sanitized();
    if prefs.enabled {
        std::fs::create_dir_all(prefs.root()).map_err(|error| fail_with("recordings_folder", error))?;
    }
    let path = prefs_path().ok_or_else(|| fail("file_write"))?;
    let body = serde_json::to_vec_pretty(&prefs).map_err(|_| fail("file_write"))?;
    std::fs::write(path, body).map_err(|error| fail_with("file_write", error))?;
    if let Ok(mut cached) = PREFS.lock() {
        *cached = Some(prefs.clone());
    }
    let _ = app.emit("recorder-status", status());
    Ok(prefs_view(prefs))
}

#[tauri::command]
pub async fn pick_recordings_folder(title: String) -> CommandResult<Option<String>> {
    let start = prefs().root();
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if start.is_dir() {
            dialog = dialog.set_directory(start);
        }
        dialog.pick_folder().map(|path| path.display().to_string())
    })
    .await
    .map_err(|_| fail("dialog_failed"))
}

#[tauri::command]
pub async fn recorder_status() -> CommandResult<RecorderStatus> {
    Ok(status())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingsView {
    root: String,
    total_bytes: u64,
    limit_bytes: u64,
    entries: Vec<Entry>,
}

#[tauri::command]
pub async fn recordings() -> CommandResult<RecordingsView> {
    tauri::async_runtime::spawn_blocking(|| {
        let prefs = prefs();
        let root = prefs.root();
        let entries = library::list(&root);
        RecordingsView {
            root: root.display().to_string(),
            total_bytes: library::total_size(&entries),
            limit_bytes: u64::from(prefs.disk_limit_gb) * 1_000_000_000,
            entries,
        }
    })
    .await
    .map_err(|_| fail("operation_failed"))
}

fn is_being_recorded(path: &std::path::Path) -> bool {
    ACTIVE.lock().is_ok_and(|active| {
        active.as_ref().is_some_and(|active| {
            std::fs::canonicalize(&active.recording.path).ok() == std::fs::canonicalize(path).ok()
        })
    })
}

#[tauri::command]
pub async fn delete_recording(app: AppHandle, path: String) -> CommandResult<()> {
    let file = library::resolve(&prefs().root(), &path).ok_or_else(|| fail("recording_missing"))?;
    if is_being_recorded(&file) {
        return Err(fail("recording_busy"));
    }
    library::delete(&file).map_err(|error| fail_with("file_write", error))?;
    let _ = app.emit("recordings-changed", ());
    Ok(())
}

/// Explorer on the recording (selected), or on the main folder.
#[tauri::command]
pub async fn open_recordings_folder(path: Option<String>) -> CommandResult<()> {
    let root = prefs().root();
    let file = path.and_then(|path| library::resolve(&root, &path));
    let mut command = std::process::Command::new("explorer.exe");
    match file {
        Some(file) => command.arg(format!("/select,{}", file.display())),
        None => {
            let _ = std::fs::create_dir_all(&root);
            command.arg(&root)
        }
    };
    command.spawn().map(|_| ()).map_err(|_| fail("browser"))
}

#[tauri::command]
/// `parts`: [start, end] seconds, joined into one clip with plain cuts.
/// `mode`: "fast" (stream copy), "exact" (re-encoded) or "discord" (re-encoded under 9.5 MB).
pub async fn save_clip(app: AppHandle, path: String, parts: Vec<[f64; 2]>, mode: String, name: Option<String>) -> CommandResult<Entry> {
    use leagueaccounts::recorder::ClipMode;
    let mode = match mode.as_str() {
        "exact" => ClipMode::Exact,
        "discord" => ClipMode::Discord,
        _ => ClipMode::Fast,
    };
    let parts: Vec<(f64, f64)> = parts.iter().map(|part| (part[0], part[1])).collect();
    let root = prefs().root();
    let result = tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::save_clip(&root, &path, &parts, mode, name.as_deref()))
        .await
        .map_err(|_| fail("operation_failed"))?;
    match result {
        Ok(entry) => {
            logging::record(Event::ClipSaved, Reason::None);
            let _ = app.emit("recordings-changed", ());
            Ok(entry)
        }
        Err(code) => {
            logging::record(Event::ClipFailed, Reason::None);
            Err(fail(code))
        }
    }
}

// ---------------------------------------------------------------- larec scheme

/// Largest piece sent per request; the player asks for the next one.
const CHUNK: u64 = 2 * 1024 * 1024;

/// `http://larec.localhost/<percent-encoded relative path>`: MP4s inside the
/// recordings folder only, with byte ranges so the player can seek.
pub fn serve(path: &str, range: Option<&str>) -> tauri::http::Response<Vec<u8>> {
    let not_found = || tauri::http::Response::builder().status(404).body(Vec::new()).unwrap_or_default();
    let Some(file) = library::resolve_url(&prefs().root(), path) else { return not_found() };
    let Ok(mut handle) = std::fs::File::open(&file) else { return not_found() };
    let total = handle.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if total == 0 {
        return not_found();
    }
    let (start, end) = range
        .and_then(|range| range.strip_prefix("bytes="))
        .and_then(|range| {
            let (start, end) = range.split_once('-')?;
            let start: u64 = start.trim().parse().ok()?;
            let end: u64 = end.trim().parse().unwrap_or(total - 1);
            Some((start, end))
        })
        .unwrap_or((0, total - 1));
    if start >= total {
        return tauri::http::Response::builder()
            .status(416)
            .header("Content-Range", format!("bytes */{total}"))
            .body(Vec::new())
            .unwrap_or_default();
    }
    let end = end.min(total - 1).min(start + CHUNK - 1);
    let mut body = vec![0u8; (end - start + 1) as usize];
    if handle.seek(SeekFrom::Start(start)).is_err() || handle.read_exact(&mut body).is_err() {
        return not_found();
    }
    tauri::http::Response::builder()
        .status(206)
        .header("Content-Type", "video/mp4")
        .header("Accept-Ranges", "bytes")
        .header("Content-Range", format!("bytes {start}-{end}/{total}"))
        .header("Content-Length", body.len().to_string())
        .header("Cache-Control", "no-store")
        .body(body)
        .unwrap_or_default()
}
