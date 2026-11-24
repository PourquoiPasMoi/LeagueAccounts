//! Game recorder glue: preferences (`recorder.json`, off by default), a light
//! watcher that starts a recording when a League game starts and stops it when
//! the game ends, the Recordings page commands, and the `larec` scheme that
//! plays videos from the chosen folder only.

use crate::refresh::Shared;
use crate::views::{fail, fail_with, CommandResult};
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::recorder::capture::{find_client_window, find_game_window};
use leagueaccounts::recorder::encoder::{Codec, Rate};
use leagueaccounts::recorder::gate::{self, GateSettings, NoiseGate};
use leagueaccounts::recorder::library::{self, Entry, UNKNOWN};
use leagueaccounts::recorder::session::{self, Outcome, RecordRequest, RecordSettings, Recording};
use leagueaccounts::riot_client;
use leagueaccounts::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::collections::BTreeMap;
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
    /// The mouse pointer is recorded. On by default.
    pub cursor: bool,
    pub game_audio: bool,
    pub microphone: bool,
    /// Discord's sound only (voice chat), as its own track. Off by default.
    pub discord: bool,
    /// The music player's sound as its own track (only while the player is on). Off by default.
    pub music: bool,
    /// Windows endpoint id of the microphone; empty = Windows' default communication device.
    pub mic_device: String,
    /// Gigabytes for all recordings; 0 = no limit.
    pub disk_limit_gb: u32,
    /// Start when a match is found (champ select on the client), then follow into the game.
    pub champ_select: bool,
    /// After the game, keep recording the client's end-of-game screen until the player leaves it.
    pub post_game: bool,
    /// Make a highlight reel clip after every recorded game.
    pub auto_highlights: bool,
    /// Video format: h264 (plays everywhere) or av1 (smaller; GPUs with an AV1 encoder).
    pub codec: String,
    /// constant (always the preset's bitrate) or variable (less when little moves).
    pub rate: String,
    /// One rule per mode group, most important first (the disk limit deletes
    /// from the end of the list first).
    pub modes: Vec<ModeRule>,
    /// Hotkey that bookmarks the moment while a game is recorded ("F8",
    /// "Ctrl+F8"…); empty = off.
    pub bookmark_key: String,
    /// Seconds before and after the press that a bookmark clip keeps.
    pub bookmark_before: u32,
    pub bookmark_after: u32,
    /// The Windows sound when a bookmark is taken.
    pub bookmark_sound: bool,
    /// Noise gate of each microphone, by Windows endpoint id (none = off).
    pub mic_gates: BTreeMap<String, GateSettings>,
    /// v5.17.0 kept one gate for all microphones: read once, moved to `mic_gates`.
    #[serde(skip_serializing)]
    pub mic_gate: Option<GateSettings>,
}

/// What to do with the games of one mode group (`queues::GROUPS`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ModeRule {
    pub key: String,
    pub record: bool,
    /// Hours a recording of this mode is kept; 0 = until the disk limit.
    pub keep_hours: u32,
}

impl Default for ModeRule {
    fn default() -> Self {
        Self { key: String::new(), record: true, keep_hours: 0 }
    }
}

/// Every group once, in the saved order, missing ones added at the end.
fn complete_modes(modes: Vec<ModeRule>) -> Vec<ModeRule> {
    use leagueaccounts::recorder::queues::GROUPS;
    let mut result: Vec<ModeRule> = Vec::new();
    for rule in modes {
        if GROUPS.contains(&rule.key.as_str()) && !result.iter().any(|known| known.key == rule.key) {
            result.push(ModeRule { keep_hours: rule.keep_hours.min(24 * 365), ..rule });
        }
    }
    for key in GROUPS {
        if !result.iter().any(|known| known.key == key) {
            result.push(ModeRule { key: key.into(), ..ModeRule::default() });
        }
    }
    result
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
            music: false,
            mic_device: String::new(),
            disk_limit_gb: 100,
            champ_select: true,
            cursor: true,
            post_game: true,
            auto_highlights: false,
            codec: "h264".into(),
            rate: "variable".into(),
            modes: complete_modes(Vec::new()),
            bookmark_key: "F8".into(),
            bookmark_before: 60,
            bookmark_after: 10,
            bookmark_sound: true,
            mic_gates: BTreeMap::new(),
            mic_gate: None,
        }
    }
}

impl RecorderPrefs {
    fn rule(&self, group: &str) -> Option<&ModeRule> {
        self.modes.iter().find(|rule| rule.key == group)
    }

    /// Games of this queue are recorded.
    fn records(&self, queue_id: i64, queue: &str) -> bool {
        self.rule(leagueaccounts::recorder::queues::group(queue_id, queue)).is_none_or(|rule| rule.record)
    }

    fn keep_hours(&self, group: &str) -> u32 {
        self.rule(group).map_or(0, |rule| rule.keep_hours)
    }

    /// Place in the priority list (higher = deleted sooner at the disk limit).
    fn rank(&self, group: &str) -> usize {
        self.modes.iter().position(|rule| rule.key == group).unwrap_or(self.modes.len())
    }

    /// Delete what is past its keeping time, then apply the disk limit by priority.
    fn tidy(&self, keep: Option<&std::path::Path>) -> usize {
        let root = self.root();
        let expired = library::apply_retention(&root, &|group| self.keep_hours(group), keep);
        expired + library::enforce_limit_by(&root, u64::from(self.disk_limit_gb) * 1_000_000_000, keep, &|group| self.rank(group))
    }

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
        if self.codec != "av1" {
            self.codec = "h264".into();
        }
        if self.rate != "constant" {
            self.rate = "variable".into();
        }
        self.modes = complete_modes(std::mem::take(&mut self.modes));
        self.height = self.height.min(4320);
        self.fps = self.fps.clamp(10, 120);
        self.bitrate = self.bitrate.clamp(1, 100);
        self.disk_limit_gb = self.disk_limit_gb.min(100_000);
        if !self.bookmark_key.is_empty() && leagueaccounts::recorder::hotkey::parse(&self.bookmark_key).is_none() {
            self.bookmark_key = "F8".into();
        }
        self.bookmark_before = self.bookmark_before.clamp(1, 600);
        self.bookmark_after = self.bookmark_after.min(300);
        if let Some(old) = self.mic_gate.take() {
            if !self.mic_device.is_empty() {
                self.mic_gates.entry(self.mic_device.clone()).or_insert(old);
            }
        }
        self.mic_gates = std::mem::take(&mut self.mic_gates)
            .into_iter()
            .filter(|(device, _)| !device.is_empty() && device.len() < 300)
            .take(64)
            .map(|(device, gate)| (device, gate.sanitized()))
            .collect();
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
            let prefs = prefs_path()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<RecorderPrefs>(&bytes).ok())
                .unwrap_or_default()
                .sanitized();
            // The microphones' gates follow the saved settings (also read by the recording thread).
            gate::set_all(&prefs.mic_gates);
            prefs
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

/// Games of the current queue are recorded (Settings › Game modes). Unknown
/// queue (client not answering): recorded.
fn queue_recorded(prefs: &RecorderPrefs) -> bool {
    let Some(session) = riot_client::league_client().and_then(|client| client.get_json("/lol-gameflow/v1/session")) else { return true };
    let (id, name) = queue_name(&session);
    prefs.records(id, &name)
}

/// The log's reason for a recording that failed.
fn failure_reason(code: &str) -> Reason {
    match code {
        "recorder_gpu" => Reason::RecorderGpu,
        "recorder_capture" => Reason::RecorderCapture,
        "recorder_encoder" => Reason::RecorderEncoder,
        "recorder_no_frames" => Reason::RecorderNoFrames,
        "recorder_media" => Reason::RecorderMedia,
        "recorder_crashed" => Reason::RecorderCrashed,
        _ => Reason::Other,
    }
}

/// `attempt`: 0 = as set; after a failed try the same game is tried again
/// another way: 1 = the whole screen cropped to the game, 2 = also without the
/// graphics card's encoder.
fn start_recording_attempt(app: &AppHandle, window: leagueaccounts::recorder::capture::WindowHandle, prefs: &RecorderPrefs, attempt: u32) {
    let mut prefs = prefs.clone();
    if attempt >= 1 {
        prefs.capture = "screen".into();
    }
    if attempt >= 2 {
        prefs.encoder = "software".into();
    }
    start_recording(app, window, &prefs, false);
}

fn start_recording(app: &AppHandle, window: leagueaccounts::recorder::capture::WindowHandle, prefs: &RecorderPrefs, pregame: bool) {
    let client = riot_client::league_client();
    let summoner = client.as_ref().and_then(|client| client.get_json("/lol-summoner/v1/current-summoner")).unwrap_or(Value::Null);
    let session_info = client.as_ref().and_then(|client| client.get_json("/lol-gameflow/v1/session")).unwrap_or(Value::Null);
    if session_info.is_object() {
        let (id, name) = queue_name(&session_info);
        if !prefs.records(id, &name) {
            return;
        }
    }
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let (account, riot_id) = account_folder(app, &text(&summoner["gameName"]), &text(&summoner["tagLine"]));
    let (max_height, fps, mbps) = prefs.quality();
    let queue = queue_name(&session_info);
    // AV1 only when the GPU can (otherwise the encoder falls back to H.264 anyway).
    let codec = if prefs.codec == "av1" && leagueaccounts::recorder::formats().av1_encoder { Codec::Av1 } else { Codec::H264 };
    let h264_bitrate = mbps * 1_000_000;
    let bitrate = if codec == Codec::Av1 { leagueaccounts::recorder::av1_bitrate(h264_bitrate) } else { h264_bitrate };
    let rate = if prefs.rate == "constant" { Rate::Constant } else { Rate::Variable };
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
            bitrate,
            hardware: prefs.encoder != "software",
            screen: prefs.capture == "screen",
            cursor: prefs.cursor,
            game_audio: prefs.game_audio,
            microphone: prefs.microphone,
            discord: prefs.discord,
            music: prefs.music && leagueaccounts::music::prefs().enabled,
            microphone_device: prefs.mic_device.clone(),
            codec,
            rate,
            post_game: prefs.post_game,
        },
        pregame,
    };
    match session::start(request, window) {
        Ok(recording) => {
            logging::record(Event::RecordingStarted, Reason::None);
            let relative = recording.path.strip_prefix(prefs.root()).map(|path| path.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            set_status(app, RecorderStatus { enabled: true, recording: true, account, started_at: recording.started_at, path: relative, error: String::new() });
            if let Ok(mut active) = ACTIVE.lock() {
                *active = Some(Active { recording });
            }
        }
        Err(_) => {
            logging::record(Event::RecordingFailed, Reason::RecorderFile);
            set_status(app, RecorderStatus { enabled: true, error: "file_write".into(), ..RecorderStatus::default() });
        }
    }
}

/// Wait for the recording thread, then apply the disk limit and tell the interface.
/// True when the recording failed (not when it was cancelled on purpose).
fn finish(app: &AppHandle, active: Active) -> bool {
    let outcome = active.recording.wait();
    let prefs = prefs();
    let mut saved = None;
    let failed = matches!(outcome, Outcome::Failed(code) if code != "recorder_cancelled" && code != "recorder_stopped");
    let error = match outcome {
        Outcome::Saved(entry) => {
            logging::record(Event::RecordingSaved, Reason::None);
            crate::notify::send(app, crate::notify::Kind::RecordingSaved(entry.name.clone()));
            saved = Some(entry.path);
            String::new()
        }
        // Match declined or dodged before the game: nothing kept, nothing to report.
        Outcome::Failed("recorder_cancelled") => String::new(),
        Outcome::Failed(code) => {
            logging::record(Event::RecordingFailed, failure_reason(code));
            code.to_owned()
        }
    };
    prefs.tidy(None);
    set_status(app, RecorderStatus { enabled: prefs.enabled, error, ..RecorderStatus::default() });
    let _ = app.emit("recordings-changed", ());
    // Each sound source as its own file now (disk reading only), so the
    // player's volume per source is ready when the game is watched; then the
    // end-of-game details from the client's match history (it lists a game a
    // little after it ends: a few tries, 30 s apart).
    if let Some(path) = saved {
        let root = prefs.root();
        let app = app.clone();
        let highlights = prefs.auto_highlights;
        thread::spawn(move || {
            let _ = leagueaccounts::recorder::sound_tracks(&root, &path);
            let _ = leagueaccounts::recorder::waves(&root, &path);
            if highlights {
                let _ = leagueaccounts::recorder::startup();
                if leagueaccounts::recorder::save_highlights(&root, &path).is_ok() {
                    let _ = app.emit("recordings-changed", ());
                }
                leagueaccounts::recorder::shutdown();
            }
            let Some(file) = library::resolve(&root, &path) else { return };
            let meta = library::read_meta(&file).unwrap_or_default();
            if meta.kind != "game" {
                return;
            }
            for _ in 0..10 {
                thread::sleep(Duration::from_secs(30));
                match leagueaccounts::recorder::gameinfo::fetch(&file, &meta) {
                    Ok(details) => {
                        if fill_result(&file, &details) {
                            let _ = app.emit("recordings-changed", ());
                        }
                        let _ = app.emit("recording-game", path.clone());
                        return;
                    }
                    Err("game_not_found") => continue,
                    Err(_) => return,
                }
            }
        });
    }
    failed
}

fn gameflow_phase() -> Option<String> {
    let client = riot_client::league_client()?;
    client.get_json("/lol-gameflow/v1/gameflow-phase")?.as_str().map(str::to_owned)
}

fn in_game_phase() -> Option<bool> {
    Some(matches!(gameflow_phase()?.as_str(), "GameStart" | "InProgress" | "Reconnect"))
}

/// End-of-game recording: stop once the player leaves the end-of-game screen
/// (Play again, back to the lobby, a new queue, or the client closed).
fn watch_end_of_game() {
    let after = ACTIVE.lock().ok().is_some_and(|active| active.as_ref().is_some_and(|active| active.recording.after_game()));
    if !after {
        return;
    }
    let left = match gameflow_phase() {
        None => riot_client::league_client().is_none(),
        Some(phase) => !matches!(phase.as_str(), "WaitingForStats" | "PreEndOfGame" | "EndOfGame" | "InProgress" | "GameStart" | "Reconnect"),
    };
    if left {
        if let Ok(active) = ACTIVE.lock() {
            if let Some(active) = active.as_ref() {
                active.recording.stop();
            }
        }
    }
}

/// Champ select recording: start when a match is found, drop it when the
/// match does not happen (declined ready check, dodge, left the lobby).
fn watch_champ_select(app: &AppHandle, prefs: &RecorderPrefs) {
    let pregame = ACTIVE.lock().ok().and_then(|active| active.as_ref().map(|active| !active.recording.in_game()));
    match pregame {
        // Recording the game already.
        Some(false) => {}
        Some(true) => {
            if let Some("None" | "Lobby" | "Matchmaking") = gameflow_phase().as_deref() {
                if let Ok(active) = ACTIVE.lock() {
                    if let Some(active) = active.as_ref() {
                        active.recording.discard();
                    }
                }
            }
        }
        None => {
            if find_game_window().is_some() {
                return;
            }
            if let Some("ReadyCheck" | "ChampSelect") = gameflow_phase().as_deref() {
                if let Some(window) = find_client_window() {
                    if queue_recorded(prefs) {
                        start_recording(app, window, prefs, true);
                    }
                }
            }
        }
    }
}

/// Checks every 2 seconds while the recorder is on (nothing at all when off).
/// Recordings made as fragmented MP4 (before this version) are rewritten once
/// as normal MP4, one at a time in the background, so they open at once.
/// Older recordings got internal mode names ("Kiwi" for ARAM: Mayhem): name
/// them again from the queue id in their saved game details, file name too.
/// Game details left without their recording are removed.
/// A game whose recording stopped before Live Client sent its end (the window
/// closes fast when the Nexus falls) has no result: take it from the match
/// history details and add it to the file name, as a normal recording has.
fn fill_result(file: &std::path::Path, details: &serde_json::Value) -> bool {
    let Some(mut meta) = library::read_meta(file) else { return false };
    if meta.kind != "game" || !meta.result.is_empty() {
        return false;
    }
    let Some(result) = leagueaccounts::recorder::gameinfo::result(details) else { return false };
    meta.result = result.to_owned();
    let mut current = file.to_path_buf();
    // Only names made by the recorder ("<date> <champion> <queue>"), not a name the player chose.
    let stem = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    if meta.title.is_empty() && !meta.queue.is_empty() && stem.ends_with(&library::safe_name(&meta.queue)) {
        if let Ok(target) = library::rename(file, &format!("{stem} {result}")) {
            current = target;
        }
    }
    library::write_meta(&current, &meta).is_ok()
}

fn relabel_old_recordings(root: &std::path::Path) -> bool {
    let mut changed = false;
    for entry in library::list(root) {
        let Some(path) = library::resolve(root, &entry.path) else { continue };
        if entry.kind != "game" || entry.meta.as_ref().is_none_or(|meta| !meta.result.is_empty()) || is_being_recorded(&path) {
            continue;
        }
        if let Some(details) = leagueaccounts::recorder::gameinfo::load(&path) {
            changed |= fill_result(&path, &details);
        }
    }
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
                if let Some(stem) = name.strip_suffix(".game.json").or_else(|| name.strip_suffix(".live.json")) {
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
    // Games cut short by a crash or a power cut: index rebuilt, named, listed.
    let recovered = session::recover(&root, &|path| is_being_recorded(path));
    if !recovered.is_empty() {
        logging::record(Event::RecordingSaved, Reason::None);
        let _ = app.emit("recordings-changed", ());
        let _ = app.emit("recordings-recovered", recovered.len());
        crate::notify::send(app, crate::notify::Kind::Recovered(recovered.len()));
    }
    if relabel_old_recordings(&root) {
        let _ = app.emit("recordings-changed", ());
    }
    for entry in library::list(&root).into_iter().filter(|entry| entry.kind == "game") {
        let Some(path) = library::resolve(&root, &entry.path) else { continue };
        // Finished by an older version: one data box, so the player opens it at once.
        if !is_being_recorded(&path) && leagueaccounts::recorder::defrag::is_chained(&path) {
            if let Ok(true) = leagueaccounts::recorder::defrag::unchain(&path) {
                let _ = app.emit("recordings-changed", ());
            }
            continue;
        }
        if is_being_recorded(&path) || !library::is_fragmented(&path) {
            continue;
        }
        if leagueaccounts::recorder::make_seekable(&path).is_ok() {
            let _ = app.emit("recordings-changed", ());
        }
    }
    // A move of the recordings to another folder that the last run did not finish.
    resume_move(app);
}

/// The bookmark hotkey was pressed: mark the moment in the game being recorded.
fn bookmark(app: &AppHandle) {
    let prefs = prefs();
    let marked = ACTIVE.lock().ok().and_then(|active| {
        let recording = &active.as_ref()?.recording;
        let time = recording.bookmark(prefs.bookmark_before, prefs.bookmark_after)?;
        Some((time, recording.bookmark_count()))
    });
    if let Some((time, count)) = marked {
        if prefs.bookmark_sound {
            leagueaccounts::recorder::hotkey::beep();
        }
        let _ = app.emit("recording-bookmark", serde_json::json!({ "time": time, "count": count }));
    }
}

pub fn spawn(app: AppHandle) {
    {
        let app = app.clone();
        // The key is only taken while a game is being recorded.
        leagueaccounts::recorder::hotkey::watch(
            || {
                let prefs = prefs();
                let recording = ACTIVE.lock().is_ok_and(|active| active.is_some());
                (prefs.enabled && recording && !prefs.bookmark_key.is_empty()).then_some(prefs.bookmark_key)
            },
            move || bookmark(&app),
        );
    }
    {
        let app = app.clone();
        thread::spawn(move || {
            // Before anything is saved: unfinished files of a save that was cut off or froze.
            library::sweep(&prefs().root());
            // After startup settles; quick when there is nothing to convert.
            thread::sleep(Duration::from_secs(15));
            convert_old_recordings(&app);
        });
    }
    thread::spawn(move || loop {
        // A fault in one round must not end recording until the app restarts.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| watch(&app))).is_err() {
            logging::record(Event::RecordingFailed, Reason::WorkerPanicked);
        }
        thread::sleep(Duration::from_secs(5));
    });
}

/// The recorder's watcher (never returns).
fn watch(app: &AppHandle) {
    {
        // The window of the game that was just recorded: not recorded twice.
        let mut finished_window: Option<isize> = None;
        // A game whose recording failed is tried again, another way: (its window, tries so far).
        let mut retry: (isize, u32) = (0, 0);
        let mut limit_checked = std::time::Instant::now();
        // Keeping times: soon after start, then every 10 minutes.
        // checked_sub: an Instant cannot go before Windows started (it panicked
        // every round in the first 9 minutes after boot, so nothing was recorded).
        let now = std::time::Instant::now();
        let mut tidied = now.checked_sub(Duration::from_secs(9 * 60)).unwrap_or(now);
        loop {
            let prefs = prefs();
            let active = ACTIVE.lock().ok().and_then(|mut active| {
                let done = active.as_ref().is_some_and(|active| active.recording.is_finished() || !prefs.enabled);
                if done { active.take() } else { None }
            });
            if let Some(active) = active {
                if !prefs.enabled {
                    // Turned off during champ select: no half recording.
                    active.recording.discard();
                }
                let window = active.recording.window();
                let in_game = active.recording.in_game();
                let failed = finish(app, active);
                if retry.0 != window {
                    retry = (window, 0);
                }
                if failed && in_game && prefs.enabled && retry.1 < 2 {
                    // Left unmarked: the next round starts it again while the game runs.
                    retry.1 += 1;
                } else {
                    finished_window = Some(window);
                }
            }
            let recording = ACTIVE.lock().is_ok_and(|active| active.is_some());
            // A long game can pass the disk limit: check once a minute, never the file being written.
            if recording && limit_checked.elapsed() >= Duration::from_secs(60) {
                limit_checked = std::time::Instant::now();
                let current = ACTIVE.lock().ok().and_then(|active| active.as_ref().map(|active| active.recording.path.clone()));
                if prefs.tidy(current.as_deref()) > 0 {
                    let _ = app.emit("recordings-changed", ());
                }
            }
            if !recording && tidied.elapsed() >= Duration::from_secs(10 * 60) {
                tidied = std::time::Instant::now();
                if prefs.tidy(None) > 0 {
                    let _ = app.emit("recordings-changed", ());
                }
            }
            if !prefs.enabled {
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            if prefs.champ_select {
                watch_champ_select(app, &prefs);
            }
            watch_end_of_game();
            let recording = ACTIVE.lock().is_ok_and(|active| active.is_some());
            let window = find_game_window();
            if window.map(|window| window.0 as isize) != finished_window {
                if window.is_none() {
                    finished_window = None;
                }
                if let (Some(window), false) = (window, recording) {
                    // A game window with the client in game (not a replay).
                    if in_game_phase().unwrap_or(false) {
                        let attempt = if retry.0 == window.0 as isize { retry.1 } else { 0 };
                        start_recording_attempt(app, window, &prefs, attempt);
                    }
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
    }
}

/// Stop recording when the app quits, so the file is finished properly.
/// A conversion in progress stops too (the original file stays).
pub fn stop_now() {
    CONVERT_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
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
    /// What the PC can do with AV1.
    formats: leagueaccounts::recorder::Formats,
}

fn prefs_view(prefs: RecorderPrefs) -> RecorderPrefsView {
    let root = prefs.root().display().to_string();
    RecorderPrefsView { prefs, root, formats: leagueaccounts::recorder::formats() }
}

#[tauri::command]
pub async fn recorder_prefs() -> CommandResult<RecorderPrefsView> {
    tauri::async_runtime::spawn_blocking(|| prefs_view(prefs())).await.map_err(|_| fail("operation_failed"))
}

/// Save a highlight reel clip of a game (kills, multikills, aces, objectives).
#[tauri::command]
pub async fn save_highlights(app: AppHandle, path: String) -> CommandResult<Entry> {
    let root = prefs().root();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _ = leagueaccounts::recorder::startup();
        let result = leagueaccounts::recorder::save_highlights(&root, &path);
        leagueaccounts::recorder::shutdown();
        result
    })
    .await
    .map_err(|_| fail("operation_failed"))?;
    match result {
        Ok(entry) => {
            logging::record(Event::ClipSaved, Reason::None);
            let _ = app.emit("recordings-changed", ());
            Ok(entry)
        }
        Err(code) => Err(fail(code)),
    }
}

// ------------------------------------------------- convert recordings to AV1

/// The "Save space" job: recordings are encoded again as AV1 one at a time,
/// paused while a game is being recorded.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertStatus {
    pub running: bool,
    /// Waiting for the game being recorded to end.
    pub paused: bool,
    /// Waiting while a recording plays in the player.
    pub watching: bool,
    pub done: usize,
    pub total: usize,
    /// Name of the recording being converted.
    pub current: String,
    /// 0..1 of the current recording.
    pub progress: f64,
    pub saved_bytes: u64,
    pub failed: usize,
    /// Size of everything to convert, and how much of it is done.
    pub total_bytes: u64,
    pub done_bytes: u64,
    /// Estimated seconds left (0 = not known yet).
    pub eta_seconds: u64,
    /// Recordings that are not AV1 yet, and their size (when not running).
    pub remaining: usize,
    pub remaining_bytes: u64,
}

static CONVERT: Mutex<Option<ConvertStatus>> = Mutex::new(None);
/// When the player last read a recording (Unix ms): the conversion waits so a
/// hard disk is not shared between both.
static LAST_PLAYED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

fn player_reading() -> bool {
    now_ms().saturating_sub(LAST_PLAYED.load(std::sync::atomic::Ordering::Relaxed)) < 5_000
}

fn recording_now() -> bool {
    ACTIVE.lock().is_ok_and(|active| active.is_some())
}
static CONVERT_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Recordings picked while a conversion runs: done after the current list.
static CONVERT_MORE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Recordings still in H.264 (biggest first), never the one being recorded.
fn convert_candidates(root: &std::path::Path) -> Vec<Entry> {
    let mut entries: Vec<Entry> = library::list(root)
        .into_iter()
        .filter(|entry| !leagueaccounts::recorder::is_av1(entry.meta.as_ref()))
        .filter(|entry| library::resolve(root, &entry.path).is_some_and(|path| !is_being_recorded(&path)))
        .collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.size));
    entries
}

fn convert_status() -> ConvertStatus {
    let status = CONVERT.lock().ok().and_then(|status| status.clone()).unwrap_or_default();
    if status.running {
        return status;
    }
    let candidates = convert_candidates(&prefs().root());
    ConvertStatus { remaining: candidates.len(), remaining_bytes: candidates.iter().map(|entry| entry.size).sum(), ..status }
}

fn set_convert(app: &AppHandle, change: impl FnOnce(&mut ConvertStatus)) {
    let snapshot = {
        let Ok(mut status) = CONVERT.lock() else { return };
        let status = status.get_or_insert_with(ConvertStatus::default);
        change(status);
        status.clone()
    };
    let _ = app.emit("convert-status", snapshot);
}

#[tauri::command]
pub async fn convert_info() -> CommandResult<ConvertStatus> {
    tauri::async_runtime::spawn_blocking(convert_status).await.map_err(|_| fail("operation_failed"))
}

/// Convert every recording that is not AV1 yet, or only `paths` (relative,
/// from the library's Select mode). While a conversion runs, picked recordings
/// are added to it.
#[tauri::command]
pub async fn start_convert(app: AppHandle, paths: Option<Vec<String>>) -> CommandResult<ConvertStatus> {
    if !leagueaccounts::recorder::formats().av1_encoder {
        return Err(fail("av1_unsupported"));
    }
    let root = prefs().root();
    let mut candidates = convert_candidates(&root);
    if let Some(paths) = &paths {
        candidates.retain(|entry| paths.contains(&entry.path));
    }
    if CONVERT.lock().ok().and_then(|status| status.as_ref().map(|status| status.running)).unwrap_or(false) {
        if let Ok(mut more) = CONVERT_MORE.lock() {
            let added: Vec<Entry> = candidates.into_iter().filter(|entry| !more.iter().any(|known| known.path == entry.path)).collect();
            let (count, size) = (added.len(), added.iter().map(|entry| entry.size).sum::<u64>());
            more.extend(added);
            set_convert(&app, |status| {
                status.total += count;
                status.total_bytes += size;
            });
        }
        return Ok(convert_status());
    }
    if candidates.is_empty() {
        return Ok(convert_status());
    }
    CONVERT_STOP.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut more) = CONVERT_MORE.lock() {
        more.clear();
    }
    let total = candidates.len();
    let total_bytes: u64 = candidates.iter().map(|entry| entry.size).sum();
    set_convert(&app, |status| *status = ConvertStatus { running: true, total, total_bytes, ..ConvertStatus::default() });
    thread::spawn(move || {
        // Pieces left by a conversion cut short (the app was closed).
        let mut folders: Vec<std::path::PathBuf> = candidates.iter().filter_map(|entry| library::resolve(&root, &entry.path)?.parent().map(|parent| parent.join(".convert"))).collect();
        folders.dedup();
        for folder in folders {
            let _ = std::fs::remove_dir_all(folder);
        }
        let stopped = || CONVERT_STOP.load(std::sync::atomic::Ordering::Relaxed);
        let mut queue: std::collections::VecDeque<Entry> = candidates.into();
        // Speed for the time estimate: bytes of recordings per second of converting.
        let mut finished_bytes = 0u64;
        let mut busy = Duration::ZERO;
        loop {
            let next = queue.pop_front().or_else(|| {
                let mut more = CONVERT_MORE.lock().ok()?;
                (!more.is_empty()).then(|| more.remove(0))
            });
            let Some(entry) = next else { break };
            // A game is being recorded (champ select included): the GPU and
            // the disk are for it.
            while recording_now() && !stopped() {
                set_convert(&app, |status| status.paused = true);
                thread::sleep(Duration::from_secs(3));
            }
            if stopped() {
                break;
            }
            let Some(file) = library::resolve(&root, &entry.path) else { continue };
            let name = entry.meta.as_ref().map(|meta| meta.title.clone()).filter(|title| !title.is_empty()).unwrap_or(entry.name.clone());
            set_convert(&app, |status| {
                status.paused = false;
                status.current = name;
                status.progress = 0.0;
            });
            let _ = leagueaccounts::recorder::startup();
            let mut last = std::time::Instant::now();
            let mut interrupted = false;
            let mut running_since = std::time::Instant::now();
            let size = entry.size;
            let result = leagueaccounts::recorder::convert_to_av1(&file, |done| {
                // A game starts: give this file up now, it is done again later.
                if recording_now() {
                    interrupted = true;
                    return false;
                }
                // Someone is watching a recording: wait, the disk is theirs.
                if player_reading() {
                    busy += running_since.elapsed();
                    set_convert(&app, |status| status.watching = true);
                    while player_reading() && !stopped() && !recording_now() {
                        thread::sleep(Duration::from_millis(500));
                    }
                    set_convert(&app, |status| status.watching = false);
                    running_since = std::time::Instant::now();
                }
                if last.elapsed() >= Duration::from_millis(400) || done >= 1.0 {
                    last = std::time::Instant::now();
                    let done_bytes = finished_bytes + (size as f64 * done) as u64;
                    let seconds = (busy + running_since.elapsed()).as_secs_f64();
                    let eta = if seconds > 5.0 && done_bytes > 0 {
                        (total_bytes.saturating_sub(done_bytes) as f64 / (done_bytes as f64 / seconds)) as u64
                    } else {
                        0
                    };
                    set_convert(&app, |status| {
                        status.progress = done;
                        status.done_bytes = done_bytes;
                        status.eta_seconds = eta;
                    });
                }
                !stopped()
            });
            leagueaccounts::recorder::shutdown();
            // A file given up for a game is done again later: its time and bytes do not count yet.
            if !interrupted {
                busy += running_since.elapsed();
                finished_bytes += size;
            }
            if interrupted && !stopped() {
                queue.push_front(entry);
                continue;
            }
            match result {
                Ok((before, after)) => set_convert(&app, |status| {
                    status.done += 1;
                    status.saved_bytes += before.saturating_sub(after);
                }),
                Err(_) if stopped() => break,
                Err(_) => set_convert(&app, |status| {
                    status.done += 1;
                    status.failed += 1;
                }),
            }
            let _ = app.emit("recordings-changed", ());
        }
        if let Ok(mut more) = CONVERT_MORE.lock() {
            more.clear();
        }
        set_convert(&app, |status| {
            status.running = false;
            status.paused = false;
            status.watching = false;
            status.current.clear();
        });
        let _ = app.emit("convert-status", convert_status());
        let _ = app.emit("recordings-changed", ());
    });
    Ok(convert_status())
}

#[tauri::command]
pub async fn stop_convert() -> CommandResult<()> {
    CONVERT_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn set_recorder_prefs(app: AppHandle, prefs: RecorderPrefs) -> CommandResult<RecorderPrefsView> {
    let prefs = prefs.sanitized();
    if prefs.enabled {
        std::fs::create_dir_all(prefs.root()).map_err(|error| fail_with("recordings_folder", error))?;
    }
    save_prefs(&app, prefs)
}

/// Change only the recordings folder, at once (Settings asks right after
/// whether the recordings of the old folder should follow).
#[tauri::command]
pub async fn set_recordings_folder(app: AppHandle, folder: String) -> CommandResult<RecorderPrefsView> {
    let folder = folder.trim().to_owned();
    if folder.is_empty() || !PathBuf::from(&folder).is_dir() {
        return Err(fail("recordings_folder"));
    }
    let mut prefs = prefs();
    prefs.folder = folder;
    save_prefs(&app, prefs.sanitized())
}

/// Write recorder.json and use the new settings at once.
fn store_prefs(prefs: &RecorderPrefs) -> CommandResult<()> {
    let path = prefs_path().ok_or_else(|| fail("file_write"))?;
    let body = serde_json::to_vec_pretty(prefs).map_err(|_| fail("file_write"))?;
    std::fs::write(path, body).map_err(|error| fail_with("file_write", error))?;
    if let Ok(mut cached) = PREFS.lock() {
        *cached = Some(prefs.clone());
    }
    gate::set_all(&prefs.mic_gates);
    Ok(())
}

fn save_prefs(app: &AppHandle, prefs: RecorderPrefs) -> CommandResult<RecorderPrefsView> {
    store_prefs(&prefs)?;
    let _ = app.emit("recorder-status", status());
    {
        let app = app.clone();
        let prefs = prefs.clone();
        thread::spawn(move || {
            let current = ACTIVE.lock().ok().and_then(|active| active.as_ref().map(|active| active.recording.path.clone()));
            if prefs.tidy(current.as_deref()) > 0 {
                let _ = app.emit("recordings-changed", ());
            }
        });
    }
    Ok(prefs_view(prefs))
}

// ------------------------------------------------- move recordings to another folder

/// The "move my recordings" job: everything under the old folder's accounts
/// goes to the folder in use, one file at a time, paused while a game is
/// being recorded. See `recorder::relocate`.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveStatus {
    pub running: bool,
    /// Waiting for the game being recorded to end.
    pub paused: bool,
    pub from: String,
    pub to: String,
    pub done_files: usize,
    pub total_files: usize,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Name of the file being moved.
    pub current: String,
    /// Files that could not be moved (still in the old folder).
    pub failed: usize,
    /// Files left in the old folder because another file has their name in the new one.
    pub kept: usize,
    /// The last run was stopped by the player.
    pub stopped: bool,
    /// The last run is over (the numbers above are its result).
    pub finished: bool,
}

static MOVE: Mutex<Option<MoveStatus>> = Mutex::new(None);
static MOVE_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Kept on disk while a move is unfinished, so the next start continues it.
#[derive(Serialize, Deserialize)]
struct MoveJob {
    from: String,
    to: String,
}

fn move_job_path() -> Option<PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("recorder-move.json"))
}

fn move_status() -> MoveStatus {
    MOVE.lock().ok().and_then(|status| status.clone()).unwrap_or_default()
}

fn set_move(app: &AppHandle, change: impl FnOnce(&mut MoveStatus)) {
    let snapshot = {
        let Ok(mut status) = MOVE.lock() else { return };
        let status = status.get_or_insert_with(MoveStatus::default);
        change(status);
        status.clone()
    };
    let _ = app.emit("recordings-move", snapshot);
}

fn same_folder(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn run_move(app: &AppHandle, from: PathBuf, to: PathBuf) {
    use leagueaccounts::recorder::relocate::{self, Moved};
    use std::sync::atomic::Ordering;
    MOVE_STOP.store(false, Ordering::Relaxed);
    let stopped = || MOVE_STOP.load(Ordering::Relaxed);
    let items = relocate::items(&from);
    set_move(app, |status| {
        *status = MoveStatus {
            running: true,
            from: from.display().to_string(),
            to: to.display().to_string(),
            total_files: items.len(),
            total_bytes: items.iter().map(|item| item.size).sum(),
            ..MoveStatus::default()
        };
    });
    // Waits while a game is being recorded (the disk is the recorder's); false = stopped.
    let wait = |app: &AppHandle| {
        let mut paused = false;
        while recording_now() && !stopped() {
            if !paused {
                paused = true;
                set_move(app, |status| status.paused = true);
            }
            thread::sleep(Duration::from_secs(1));
        }
        if paused {
            set_move(app, |status| status.paused = false);
        }
        !stopped()
    };
    let mut base = 0u64;
    let mut listed = std::time::Instant::now();
    let mut moved_any = false;
    for item in &items {
        if !wait(app) {
            break;
        }
        let name = item.relative.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        set_move(app, |status| status.current = name);
        let mut within = 0u64;
        let mut told = std::time::Instant::now();
        let result = relocate::move_file(item, &to, &mut |bytes| {
            within += bytes;
            if told.elapsed() >= Duration::from_millis(300) {
                told = std::time::Instant::now();
                set_move(app, |status| status.done_bytes = base + within.min(item.size));
            }
            wait(app)
        });
        if matches!(result, Ok(Moved::Stopped)) {
            break;
        }
        base += item.size;
        moved_any |= matches!(result, Ok(Moved::Done));
        set_move(app, |status| {
            status.done_bytes = base;
            status.done_files += 1;
        });
        // The library shows what has arrived so far.
        if listed.elapsed() >= Duration::from_secs(5) {
            listed = std::time::Instant::now();
            let _ = app.emit("recordings-changed", ());
        }
    }
    // What is still there: once more for files that were busy, then the result.
    let (mut failed, mut kept) = (0, 0);
    if !stopped() {
        for item in relocate::items(&from) {
            if !wait(app) {
                break;
            }
            match relocate::move_file(&item, &to, &mut |_| !stopped()) {
                Ok(Moved::Done) => moved_any = true,
                Ok(Moved::Kept) => kept += 1,
                Ok(Moved::Stopped) => break,
                Err(_) => failed += 1,
            }
        }
    }
    relocate::remove_empty(&from);
    let was_stopped = stopped();
    // Busy files are tried again at the next start; a stop by the player is final.
    if was_stopped || failed == 0 {
        if let Some(path) = move_job_path() {
            let _ = std::fs::remove_file(path);
        }
    }
    if moved_any {
        logging::record(Event::RecordingSaved, Reason::None);
    }
    set_move(app, |status| {
        status.running = false;
        status.paused = false;
        status.current.clear();
        status.failed = failed;
        status.kept = kept;
        status.stopped = was_stopped;
        status.finished = true;
    });
    let _ = app.emit("recordings-changed", ());
}

fn resume_move(app: &AppHandle) {
    let Some(path) = move_job_path() else { return };
    let Some(job) = std::fs::read(&path).ok().and_then(|body| serde_json::from_slice::<MoveJob>(&body).ok()) else { return };
    let (from, to) = (PathBuf::from(&job.from), prefs().root());
    // The folder was changed again since, or the old one is gone: nothing to continue.
    if !same_folder(&PathBuf::from(&job.to), &to) || !from.is_dir() || same_folder(&from, &to) || move_status().running {
        let _ = std::fs::remove_file(path);
        return;
    }
    run_move(app, from, to);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveSurvey {
    #[serde(flatten)]
    survey: leagueaccounts::recorder::relocate::Survey,
    from: String,
    to: String,
    /// Same drive: nothing is copied, the move is instant.
    same_drive: bool,
    /// Free space on the new folder's drive (absent when unknown).
    free_bytes: Option<u64>,
}

/// What moving the recordings of `from` into the folder in use would involve.
#[tauri::command]
pub async fn survey_recordings_move(from: String) -> CommandResult<MoveSurvey> {
    let to = prefs().root();
    tauri::async_runtime::spawn_blocking(move || {
        use leagueaccounts::recorder::relocate;
        let source = PathBuf::from(from.trim());
        if !source.is_dir() {
            return Err(fail("recordings_folder"));
        }
        if same_folder(&source, &to) {
            return Err(fail("recordings_move_same"));
        }
        Ok(MoveSurvey {
            survey: relocate::survey(&source),
            from: source.display().to_string(),
            to: to.display().to_string(),
            same_drive: relocate::same_drive(&source, &to),
            free_bytes: relocate::free_space(&to),
        })
    })
    .await
    .map_err(|_| fail("operation_failed"))?
}

/// Move every recording, clip and their data from `from` into the folder in use (in the background).
#[tauri::command]
pub async fn start_recordings_move(app: AppHandle, from: String) -> CommandResult<MoveStatus> {
    let (source, to) = (PathBuf::from(from.trim()), prefs().root());
    if move_status().running {
        return Ok(move_status());
    }
    if !source.is_dir() {
        return Err(fail("recordings_folder"));
    }
    if same_folder(&source, &to) {
        return Err(fail("recordings_move_same"));
    }
    std::fs::create_dir_all(&to).map_err(|error| fail_with("recordings_folder", error))?;
    if let Some(path) = move_job_path() {
        let job = MoveJob { from: source.display().to_string(), to: to.display().to_string() };
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&job).unwrap_or_default());
    }
    set_move(&app, |status| {
        *status = MoveStatus { running: true, from: source.display().to_string(), to: to.display().to_string(), ..MoveStatus::default() };
    });
    let worker = app.clone();
    thread::spawn(move || run_move(&worker, source, to));
    Ok(move_status())
}

#[tauri::command]
pub async fn stop_recordings_move() -> CommandResult<()> {
    MOVE_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn recordings_move_status() -> CommandResult<MoveStatus> {
    Ok(move_status())
}

#[tauri::command]
pub async fn pick_recordings_folder(window: tauri::WebviewWindow, title: String) -> CommandResult<Option<String>> {
    let start = prefs().root();
    crate::views::file_dialog(window, move |dialog| {
        let mut dialog = dialog.set_title(title);
        if start.is_dir() {
            dialog = dialog.set_directory(start);
        }
        dialog.pick_folder().map(|path| path.display().to_string())
    })
    .await
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

// ---------------------------------------------------------------- microphone noise gate

/// Save one microphone's noise gate at once (tuned live; a recording in
/// progress follows the change too). `device` = Windows endpoint id.
#[tauri::command]
pub async fn set_mic_gate(device: String, gate: GateSettings) -> CommandResult<GateSettings> {
    if device.is_empty() {
        return Err(fail("microphone_failed"));
    }
    let gate = gate.sanitized();
    let mut prefs = prefs();
    prefs.mic_gates.insert(device, gate);
    store_prefs(&prefs.sanitized())?;
    Ok(gate)
}

/// The microphone recorded (empty = Windows' default communication device), saved at once.
#[tauri::command]
pub async fn set_mic_device(device: String) -> CommandResult<()> {
    let mut prefs = prefs();
    prefs.mic_device = device.trim().to_owned();
    store_prefs(&prefs.sanitized())
}

/// The tuning window's microphone: one at a time (a new start ends the old
/// one), stops by itself when the window stops pinging (closed, app hidden).
static MONITOR_RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MONITOR_PING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MONITOR_LISTEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GateLevel {
    /// Loudest input and output in the last 50 ms, dB (-100 = silence).
    input: f32,
    output: f32,
    open: bool,
    /// The microphone could not be opened.
    failed: bool,
}

/// Listen to the microphone through the gate and send its level 20 times a
/// second (`mic-gate-level`); `listen` plays it on the speakers too.
#[tauri::command]
pub async fn start_mic_monitor(app: AppHandle, id: String, listen: bool) -> CommandResult<()> {
    use std::sync::atomic::Ordering;
    let run = MONITOR_RUN.fetch_add(1, Ordering::SeqCst) + 1;
    MONITOR_PING.store(now_ms(), Ordering::SeqCst);
    MONITOR_LISTEN.store(listen, Ordering::SeqCst);
    thread::spawn(move || {
        let _ = leagueaccounts::recorder::startup();
        mic_monitor(&app, &id, run);
        leagueaccounts::recorder::shutdown();
    });
    Ok(())
}

fn mic_monitor(app: &AppHandle, id: &str, run: u64) {
    use leagueaccounts::recorder::audio::{self, Speaker};
    use std::sync::atomic::Ordering;
    let Ok(mut mic) = audio::microphone_raw(id) else {
        let _ = app.emit("mic-gate-level", GateLevel { input: -100.0, output: -100.0, open: false, failed: true });
        return;
    };
    let mut noise_gate = NoiseGate::new();
    let mut speaker: Option<Speaker> = None;
    let (mut input, mut output, mut open) = (0f32, 0f32, false);
    let mut sent = std::time::Instant::now();
    while MONITOR_RUN.load(Ordering::SeqCst) == run && now_ms().saturating_sub(MONITOR_PING.load(Ordering::SeqCst)) < 6000 {
        thread::sleep(Duration::from_millis(10));
        let mut samples = mic.drain();
        let block = noise_gate.process(&mut samples, gate::current_for(mic.device()));
        input = input.max(block.input_peak);
        output = output.max(block.output_peak);
        open |= block.open;
        if MONITOR_LISTEN.load(Ordering::SeqCst) {
            if speaker.is_none() {
                speaker = Speaker::open().ok();
            }
            if let Some(speaker) = speaker.as_mut() {
                speaker.play(&samples);
            }
        } else {
            speaker = None;
        }
        if sent.elapsed() >= Duration::from_millis(50) {
            sent = std::time::Instant::now();
            let _ = app.emit("mic-gate-level", GateLevel { input: gate::db(input), output: gate::db(output), open, failed: false });
            (input, output, open) = (0.0, 0.0, false);
        }
    }
}

#[tauri::command]
pub async fn mic_monitor_ping(listen: bool) -> CommandResult<()> {
    use std::sync::atomic::Ordering;
    MONITOR_PING.store(now_ms(), Ordering::SeqCst);
    MONITOR_LISTEN.store(listen, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub async fn stop_mic_monitor() -> CommandResult<()> {
    MONITOR_RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(())
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

/// The end-of-game details of a game in an account's list (scoreboard, stats,
/// graphs, runes), read from the League client once and then kept.
#[tauri::command]
pub async fn match_details(game_id: i64, champion_id: i64, kills: i64, deaths: i64, assists: i64) -> CommandResult<Value> {
    tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::gameinfo::by_id(game_id, champion_id, (kills, deaths, assists)))
        .await
        .map_err(|_| fail("operation_failed"))?
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
            return Ok(Some(leagueaccounts::recorder::gameinfo::upgrade(&file, details)));
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

/// Every player's state sampled during the recording (None for games
/// recorded before it was kept).
#[tauri::command]
pub async fn recording_live(path: String) -> CommandResult<Option<Value>> {
    let root = prefs().root();
    let file = library::resolve(&root, &path).ok_or_else(|| fail("recording_missing"))?;
    tauri::async_runtime::spawn_blocking(move || leagueaccounts::recorder::gameinfo::load_live(&file))
        .await
        .map_err(|_| fail("operation_failed"))
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

/// The player's notes on a recording (review): replaces the whole list.
#[tauri::command]
pub async fn set_recording_notes(path: String, notes: Vec<library::Note>) -> CommandResult<Vec<library::Note>> {
    let root = prefs().root();
    tauri::async_runtime::spawn_blocking(move || {
        let file = library::resolve(&root, &path).ok_or("recording_missing")?;
        let mut meta = library::read_meta(&file).unwrap_or_default();
        let mut notes: Vec<library::Note> = notes
            .into_iter()
            .filter(|note| note.time.is_finite() && !note.text.trim().is_empty())
            .map(|note| library::Note { time: note.time.max(0.0), text: note.text.trim().chars().take(500).collect() })
            .take(300)
            .collect();
        notes.sort_by(|a, b| a.time.total_cmp(&b.time));
        meta.notes = notes.clone();
        library::write_meta(&file, &meta).map_err(|_| "file_write")?;
        Ok(notes)
    })
    .await
    .map_err(|_| fail("operation_failed"))?
    .map_err(fail)
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
pub async fn rename_discord_webhook(id: String, name: String) -> CommandResult<Vec<WebhookView>> {
    leagueaccounts::discord::rename(&id, &name).map(webhook_views).map_err(fail)
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
        let mut posted = Vec::new();
        let results: Vec<PostResult> = ids
            .iter()
            .filter_map(|id| hooks.iter().find(|hook| &hook.id == id))
            .map(|hook| {
                let error = leagueaccounts::discord::post(&hook.url, &file, &message).err().map(str::to_owned);
                if error.is_none() {
                    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64);
                    posted.push(library::DiscordPost { webhook: hook.id.clone(), channel: hook.name.clone(), at });
                }
                PostResult { name: hook.name.clone(), error }
            })
            .collect();
        // Keep the history in the clip's `.json` (the video file is not touched).
        if !posted.is_empty() {
            if let Some(mut meta) = library::read_meta(&file) {
                meta.posts.extend(posted);
                let _ = library::write_meta(&file, &meta);
            }
        }
        results
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
    LAST_PLAYED.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
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
