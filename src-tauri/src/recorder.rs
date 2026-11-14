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
    /// Discord's sound only (voice chat), as its own track. Off by default.
    pub discord: bool,
    /// Windows endpoint id of the microphone; empty = Windows' default communication device.
    pub mic_device: String,
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
            discord: false,
            mic_device: String::new(),
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

/// The queue's id and readable name: our table, else the client's own
/// description of the queue (new modes), else the game mode.
fn queue_name(session: &Value) -> (i64, String) {
    let queue = &session["gameData"]["queue"];
    let id = queue["id"].as_i64().unwrap_or(-1);
    let mode = queue["gameMode"].as_str().or_else(|| session["map"]["gameMode"].as_str()).unwrap_or_default();
    if let Some(name) = leagueaccounts::recorder::queues::label(id, mode) {
        return (id.max(0), name.to_owned());
    }
    let described = [&queue["description"], &queue["name"], &queue["shortName"]].into_iter().filter_map(Value::as_str).find(|text| !text.trim().is_empty());
    let name = described.map(str::to_owned).unwrap_or_else(|| {
        let lower = mode.to_lowercase();
        let mut chars = lower.chars();
        chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
    });
    (id.max(0), name)
}

fn start_recording(app: &AppHandle, window: leagueaccounts::recorder::capture::WindowHandle, prefs: &RecorderPrefs) {
    let client = riot_client::league_client();
    let summoner = client.as_ref().and_then(|client| client.get_json("/lol-summoner/v1/current-summoner")).unwrap_or(Value::Null);
    let session_info = client.as_ref().and_then(|client| client.get_json("/lol-gameflow/v1/session")).unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let (account, riot_id) = account_folder(app, &text(&summoner["gameName"]), &text(&summoner["tagLine"]));
    let (max_height, fps, mbps) = prefs.quality();
    let queue = queue_name(&session_info);
    let request = RecordRequest {
        root: prefs.root(),
        account: account.clone(),
        riot_id,
        puuid: text(&summoner["puuid"]),
        queue: queue.1,
        queue_id: queue.0,
        settings: RecordSettings {
            max_height,
            fps,
            bitrate: mbps * 1_000_000,
            hardware: prefs.encoder != "software",
            screen: prefs.capture == "screen",
            game_audio: prefs.game_audio,
            microphone: prefs.microphone,
            discord: prefs.discord,
            microphone_device: prefs.mic_device.clone(),
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
    let mut saved = None;
    let error = match outcome {
        Outcome::Saved(entry) => {
            logging::record(Event::RecordingSaved, Reason::None);
            saved = Some(entry.path);
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
    // Each sound source as its own file now (disk reading only), so the
    // player's volume per source is ready when the game is watched; then the
    // end-of-game details from the client's match history (it lists a game a
    // little after it ends: a few tries, 30 s apart).
    if let Some(path) = saved {
        let root = prefs.root();
        let app = app.clone();
        thread::spawn(move || {
            let _ = leagueaccounts::recorder::sound_tracks(&root, &path);
            let _ = leagueaccounts::recorder::waves(&root, &path);
            let Some(file) = library::resolve(&root, &path) else { return };
            let meta = library::read_meta(&file).unwrap_or_default();
            if meta.kind != "game" {
                return;
            }
            for _ in 0..10 {
                thread::sleep(Duration::from_secs(30));
                match leagueaccounts::recorder::gameinfo::fetch(&file, &meta) {
                    Ok(_) => {
                        let _ = app.emit("recording-game", path.clone());
                        return;
                    }
                    Err("game_not_found") => continue,
                    Err(_) => return,
                }
            }
        });
    }
}

fn in_game_phase() -> Option<bool> {
    let client = riot_client::league_client()?;
    let phase = client.get_json("/lol-gameflow/v1/gameflow-phase")?;
    Some(matches!(phase.as_str(), Some("GameStart" | "InProgress" | "Reconnect")))
}

/// Checks every 2 seconds while the recorder is on (nothing at all when off).
/// Recordings made as fragmented MP4 (before this version) are rewritten once
/// as normal MP4, one at a time in the background, so they open at once.
/// Older recordings got internal mode names ("Kiwi" for ARAM: Mayhem): name
/// them again from the queue id in their saved game details, file name too.
/// Game details left without their recording are removed.
fn relabel_old_recordings(root: &std::path::Path) -> bool {
    let mut changed = false;
    for entry in library::list(root) {
        let Some(path) = library::resolve(root, &entry.path) else { continue };
        let Some(mut meta) = entry.meta.clone() else { continue };
        if meta.queue_id > 0 || is_being_recorded(&path) {
            continue;
        }
        // Games: the queue id in their saved details. Clips (and games without
        // details): an internal mode name such as "Kiwi" read as a game mode.
        let (id, label) = match leagueaccounts::recorder::gameinfo::load(&path) {
            Some(details) => {
                let id = details["game"]["queueId"].as_i64().unwrap_or(0);
                (id, leagueaccounts::recorder::queues::label(id, details["game"]["gameMode"].as_str().unwrap_or_default()))
            }
            None if !meta.queue.contains(' ') && !meta.queue.is_empty() => (0, leagueaccounts::recorder::queues::label(-1, &meta.queue)),
            None => (0, None),
        };
        let Some(label) = label else { continue };
        if id == 0 && meta.queue == label {
            continue;
        }
        let old = meta.queue.clone();
        meta.queue = label.to_owned();
        meta.queue_id = id;
        let mut current = path.clone();
        if !old.is_empty() && old != label {
            let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let old_part = format!(" {} ", library::safe_name(&old));
            let padded = format!("{stem} ");
            if padded.contains(&old_part) {
                let renamed = padded.replacen(&old_part, &format!(" {} ", library::safe_name(label)), 1);
                if let Ok(target) = library::rename(&path, renamed.trim_end()) {
                    current = target;
                }
            }
        }
        if library::write_meta(&current, &meta).is_ok() {
            changed = true;
        }
    }
    // Details whose recording is gone.
    if let Ok(accounts) = std::fs::read_dir(root) {
        for folder in accounts.flatten().map(|account| account.path().join(library::GAMES)) {
            let Ok(files) = std::fs::read_dir(&folder) else { continue };
            for file in files.flatten().map(|file| file.path()) {
                let name = file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                if let Some(stem) = name.strip_suffix(".game.json") {
                    if !folder.join(format!("{stem}.mp4")).exists() {
                        let _ = std::fs::remove_file(&file);
                    }
                }
            }
        }
    }
    changed
}

fn convert_old_recordings(app: &AppHandle) {
    let root = prefs().root();
    if relabel_old_recordings(&root) {
        let _ = app.emit("recordings-changed", ());
    }
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

/// The PC's microphones with Windows' default roles.
#[tauri::command]
pub async fn microphones() -> CommandResult<Vec<leagueaccounts::recorder::audio::CaptureDevice>> {
    tauri::async_runtime::spawn_blocking(|| {
        let _ = leagueaccounts::recorder::startup();
        let devices = leagueaccounts::recorder::audio::capture_devices();
        leagueaccounts::recorder::shutdown();
        devices
    })
    .await
    .map_err(|_| fail("operation_failed"))
}

/// Loudest level (0..1) the microphone hears in 2 seconds; nothing is kept.
#[tauri::command]
pub async fn microphone_level(id: String) -> CommandResult<f32> {
    tauri::async_runtime::spawn_blocking(move || {
        let _ = leagueaccounts::recorder::startup();
        let level = leagueaccounts::recorder::audio::probe_level(&id, 2.0);
        leagueaccounts::recorder::shutdown();
        level
    })
    .await
    .map_err(|_| fail("operation_failed"))?
    .ok_or_else(|| fail("microphone_failed"))
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

/// Explorer on the recording (selected), on a folder inside the recordings
/// folder (`folder`, e.g. `Share Lock#EUW/Clips`), or on the main folder.
#[tauri::command]
pub async fn open_recordings_folder(path: Option<String>, folder: Option<String>) -> CommandResult<()> {
    let root = prefs().root();
    let file = path.and_then(|path| library::resolve(&root, &path));
    let inside = folder
        .filter(|folder| !folder.contains("..") && !folder.contains(':'))
        .map(|folder| root.join(folder.replace('/', "\\")))
        .filter(|directory| directory.is_dir());
    let mut command = std::process::Command::new("explorer.exe");
    match file {
        // Explorer reads `/select,"C:\a b\c.mp4"` itself; Rust's quoting of the
        // whole argument made it ignore it and open Documents.
        Some(file) => {
            use std::os::windows::process::CommandExt;
            command.raw_arg(format!("/select,\"{}\"", file.display()))
        }
        None => match inside {
            Some(directory) => command.arg(directory),
            None => {
                let _ = std::fs::create_dir_all(&root);
                command.arg(&root)
            }
        },
    };
    command.spawn().map(|_| ()).map_err(|_| fail("browser"))
}

#[tauri::command]
/// `parts`: [start, end] seconds, joined into one clip with plain cuts.
/// `mode`: "fast" (stream copy), "exact" (re-encoded) or "discord" (re-encoded under 9.5 MB).
/// `sounds`: the audio sources to keep ("game", "pc", "mic", "discord"); absent = all.
/// `gains`: level per source (1.0 = as recorded, up to 3.0); absent = as recorded.
pub async fn save_clip(
    app: AppHandle,
    path: String,
    parts: Vec<[f64; 2]>,
    mode: String,
    name: Option<String>,
    sounds: Option<Vec<String>>,
    gains: Option<std::collections::HashMap<String, f32>>,
) -> CommandResult<Entry> {
    use leagueaccounts::recorder::ClipMode;
    let mode = match mode.as_str() {
        "exact" => ClipMode::Exact,
        "discord" => ClipMode::Discord,
        _ => ClipMode::Fast,
    };
    let parts: Vec<(f64, f64)> = parts.iter().map(|part| (part[0], part[1])).collect();
    let root = prefs().root();
    let gains = gains.unwrap_or_default();
    let result = tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::save_clip(&root, &path, &parts, mode, name.as_deref(), sounds.as_deref(), &gains))
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

#[derive(Serialize)]
pub struct SoundTrack {
    label: String,
    /// Relative to the recordings folder (played through `larec`).
    path: String,
}

/// The recording's sound sources as separate files for the player's volume
/// per source (copied out of the recording the first time, a few seconds).
#[tauri::command]
pub async fn recording_sound_tracks(path: String) -> CommandResult<Vec<SoundTrack>> {
    let root = prefs().root();
    let file = library::resolve(&root, &path).ok_or_else(|| fail("recording_missing"))?;
    if is_being_recorded(&file) {
        return Err(fail("recording_busy"));
    }
    tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::sound_tracks(&root, &path))
        .await
        .map_err(|_| fail("operation_failed"))?
        .map(|tracks| tracks.into_iter().map(|(label, path)| SoundTrack { label, path }).collect())
        .map_err(fail)
}

/// A game recording's end-of-game details (saved next to it), read from the
/// League client the first time when `fetch` is set. `None`: not saved yet.
#[tauri::command]
pub async fn recording_game(path: String, fetch: bool) -> CommandResult<Option<Value>> {
    let root = prefs().root();
    tauri::async_runtime::spawn_blocking(move || {
        let file = library::resolve(&root, &path).ok_or("recording_missing")?;
        if let Some(details) = leagueaccounts::recorder::gameinfo::load(&file) {
            return Ok(Some(details));
        }
        if !fetch {
            return Ok(None);
        }
        let meta = library::read_meta(&file).unwrap_or_default();
        leagueaccounts::recorder::gameinfo::fetch(&file, &meta).map(Some)
    })
    .await
    .map_err(|_| fail("operation_failed"))?
    .map_err(fail)
}

/// Sound levels drawn under the player (worked out once, then read from disk).
#[tauri::command]
pub async fn recording_waves(path: String) -> CommandResult<leagueaccounts::recorder::Waves> {
    let root = prefs().root();
    let file = library::resolve(&root, &path).ok_or_else(|| fail("recording_missing"))?;
    if is_being_recorded(&file) {
        return Err(fail("recording_busy"));
    }
    tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::waves(&root, &path))
        .await
        .map_err(|_| fail("operation_failed"))?
        .map_err(fail)
}

/// Rename a recording; returns its new path.
#[tauri::command]
pub async fn rename_recording(app: AppHandle, path: String, name: String) -> CommandResult<String> {
    let root = prefs().root();
    let file = library::resolve(&root, &path).ok_or_else(|| fail("recording_missing"))?;
    if is_being_recorded(&file) {
        return Err(fail("recording_busy"));
    }
    let renamed = tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::rename_recording(&root, &path, &name))
        .await
        .map_err(|_| fail("operation_failed"))?
        .map_err(fail)?;
    let _ = app.emit("recordings-changed", ());
    Ok(renamed)
}

// ---------------------------------------------------------------- Discord webhooks

/// A webhook as the interface sees it: the address is shortened (it holds a token).
#[derive(Serialize)]
pub struct WebhookView {
    id: String,
    name: String,
    hint: String,
}

fn webhook_views(list: Vec<leagueaccounts::discord::Webhook>) -> Vec<WebhookView> {
    list.into_iter()
        .map(|webhook| {
            let tail: String = webhook.url.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            WebhookView { id: webhook.id, name: webhook.name, hint: format!("…{tail}") }
        })
        .collect()
}

#[tauri::command]
pub async fn discord_webhooks() -> CommandResult<Vec<WebhookView>> {
    Ok(webhook_views(leagueaccounts::discord::load()))
}

#[tauri::command]
pub async fn add_discord_webhook(name: String, url: String) -> CommandResult<Vec<WebhookView>> {
    leagueaccounts::discord::add(&name, &url).map(webhook_views).map_err(fail)
}

#[tauri::command]
pub async fn remove_discord_webhook(id: String) -> CommandResult<Vec<WebhookView>> {
    leagueaccounts::discord::remove(&id).map(webhook_views).map_err(fail)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostResult {
    name: String,
    error: Option<String>,
}

/// Post a recording (a clip) with a message to the chosen webhooks, one after the other.
#[tauri::command]
pub async fn post_to_discord(path: String, ids: Vec<String>, message: String) -> CommandResult<Vec<PostResult>> {
    let root = prefs().root();
    let file = library::resolve(&root, &path).ok_or_else(|| fail("recording_missing"))?;
    tauri::async_runtime::spawn_blocking(move || {
        let hooks = leagueaccounts::discord::load();
        ids.iter()
            .filter_map(|id| hooks.iter().find(|hook| &hook.id == id))
            .map(|hook| PostResult { name: hook.name.clone(), error: leagueaccounts::discord::post(&hook.url, &file, &message).err().map(str::to_owned) })
            .collect()
    })
    .await
    .map_err(|_| fail("operation_failed"))
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
            .header("Access-Control-Allow-Origin", "*")
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
        // The player routes the sound through Web Audio (volume above 100 %),
        // which needs the app's page to be allowed to read it.
        .header("Access-Control-Allow-Origin", "*")
        .header("Accept-Ranges", "bytes")
        .header("Content-Range", format!("bytes {start}-{end}/{total}"))
        .header("Content-Length", body.len().to_string())
        .header("Cache-Control", "no-store")
        .body(body)
        .unwrap_or_default()
}
