//! One recorded game, on its own thread: capture → GPU conversion → encoder,
//! sound mixed in, key moments polled on a second light thread. When the game
//! window closes (or `stop` is called) the file is finished, renamed to e.g.
//! `2026-11-13 21-40 Ahri Ranked Solo Win.mp4` and its moments are written next
//! to it.
//!
//! With champ select on, the recording starts on the League client window when
//! a match is found and moves to the game window once it draws, through a
//! short GPU transition (`transition.rs`), all in one file.

use super::audio::{self, AudioMixer};
use super::capture::{find_game_window, monitor_size, window_process_id, Capture, Gpu};
use super::encoder::{EncoderSettings, VideoEncoder};
use super::library::{self, Entry, RecordingMeta, GAMES};
use super::moments::MomentTracker;
use super::{output_size, shutdown, startup, transition};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::SystemInformation::GetLocalTime;

#[derive(Clone, Debug)]
pub struct RecordSettings {
    /// 0 = the window's own size.
    pub max_height: u32,
    pub fps: u32,
    /// Bits per second.
    pub bitrate: u32,
    pub hardware: bool,
    /// Desktop Duplication instead of window capture.
    pub screen: bool,
    pub game_audio: bool,
    /// Discord's sound only (voice chat), as its own track.
    pub discord: bool,
    pub microphone: bool,
    /// Windows endpoint id of the microphone; empty = the default communication device.
    pub microphone_device: String,
}

#[derive(Clone, Debug)]
pub struct RecordRequest {
    pub root: PathBuf,
    /// Account folder name (`Unknown` when no saved account matched).
    pub account: String,
    pub riot_id: String,
    pub puuid: String,
    pub queue: String,
    pub queue_id: i64,
    pub settings: RecordSettings,
    /// Start on the League client window (champ select) and move to the game
    /// window when it appears.
    pub pregame: bool,
}

/// Longest wait on the client window for the game to start.
const PREGAME_LIMIT: Duration = Duration::from_secs(20 * 60);
/// The game window's first pictures can be black: wait a moment before the transition.
const GAME_SETTLE: Duration = Duration::from_millis(1800);
/// A game window that never draws: transition anyway (into the dark colour).
const GAME_NO_PICTURE: Duration = Duration::from_secs(15);

pub enum Outcome {
    Saved(Box<Entry>),
    /// Error code for the log and the interface.
    Failed(&'static str),
}

pub struct Recording {
    stop: Arc<AtomicBool>,
    discard: Arc<AtomicBool>,
    /// The window being captured (the client's, then the game's).
    window: Arc<AtomicIsize>,
    /// The game window is being recorded (always true without champ select).
    in_game: Arc<AtomicBool>,
    thread: Option<JoinHandle<Outcome>>,
    /// The file being written.
    pub path: PathBuf,
    pub account: String,
    /// Unix ms.
    pub started_at: u64,
}

impl Recording {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stop and delete the file: the match did not start (declined, dodged).
    /// Only before the game: a game is never thrown away.
    pub fn discard(&self) {
        if !self.in_game() {
            self.discard.store(true, Ordering::Relaxed);
            self.stop();
        }
    }

    pub fn in_game(&self) -> bool {
        self.in_game.load(Ordering::Relaxed)
    }

    pub fn window(&self) -> isize {
        self.window.load(Ordering::Relaxed)
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn wait(mut self) -> Outcome {
        self.stop();
        self.thread.take().and_then(|thread| thread.join().ok()).unwrap_or(Outcome::Failed("recorder_crashed"))
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

/// Local date and time for file names: "2026-11-13 21-40".
fn stamp() -> String {
    let time = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}-{:02}", time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute)
}

/// Start recording the game window. The work happens on a new thread.
pub fn start(request: RecordRequest, window: HWND) -> std::io::Result<Recording> {
    let directory = request.root.join(&request.account).join(GAMES);
    std::fs::create_dir_all(&directory)?;
    let stamp = stamp();
    let path = library::unique_file(&directory, &format!("{stamp} recording"));
    let stop = Arc::new(AtomicBool::new(false));
    let discard = Arc::new(AtomicBool::new(false));
    let in_game = Arc::new(AtomicBool::new(!request.pregame));
    let window = Arc::new(AtomicIsize::new(window.0 as isize));
    let started_at = now_ms();
    let thread = {
        let shared = Shared { stop: Arc::clone(&stop), discard: Arc::clone(&discard), window: Arc::clone(&window), in_game: Arc::clone(&in_game) };
        let path = path.clone();
        let request = request.clone();
        thread::Builder::new()
            .name("recorder".into())
            .spawn(move || {
                let outcome = record(&request, &shared, &path, &stamp, started_at);
                shutdown();
                outcome
            })?
    };
    Ok(Recording { stop, discard, window, in_game, thread: Some(thread), path, account: request.account, started_at })
}

struct Shared {
    stop: Arc<AtomicBool>,
    discard: Arc<AtomicBool>,
    window: Arc<AtomicIsize>,
    in_game: Arc<AtomicBool>,
}

/// Where the recording is: on the client window, moving to the game, or in the game.
enum Stage {
    Pregame { since: Instant, looked: Instant },
    /// Game window open; `first` = its first picture, `start` = transition start.
    Switch { opened: Instant, first: Option<Instant>, start: Option<Instant> },
    Game,
}

fn record(request: &RecordRequest, shared: &Shared, path: &Path, stamp: &str, started_at: u64) -> Outcome {
    let outcome = record_inner(request, shared, path, stamp, started_at);
    if shared.discard.load(Ordering::Relaxed) && !shared.in_game.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(path);
        return Outcome::Failed("recorder_cancelled");
    }
    outcome
}

fn record_inner(request: &RecordRequest, shared: &Shared, path: &Path, stamp: &str, started_at: u64) -> Outcome {
    let stop = &*shared.stop;
    let window = HWND(shared.window.load(Ordering::Relaxed) as *mut core::ffi::c_void);
    if startup().is_err() {
        return Outcome::Failed("recorder_media");
    }
    let settings = &request.settings;
    let Ok(gpu) = Gpu::for_window(Some(window)) else { return Outcome::Failed("recorder_gpu") };
    // The window can be created a moment before it can be captured.
    let mut capture = None;
    for _ in 0..20 {
        if stop.load(Ordering::Relaxed) {
            return Outcome::Failed("recorder_stopped");
        }
        if let Ok(opened) = Capture::open(&gpu, window, settings.screen) {
            capture = Some(opened);
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    let Some(mut capture) = capture else { return Outcome::Failed("recorder_capture") };

    let mut sources = Vec::new();
    let mut audio_source = Vec::new();
    if settings.game_audio {
        let pid = window_process_id(window);
        match audio::game_sound(pid) {
            Ok(source) => {
                sources.push(source);
                audio_source.push("game");
            }
            Err(_) => {
                if let Ok(source) = audio::pc_sound() {
                    sources.push(source);
                    audio_source.push("pc");
                }
            }
        }
    }
    if settings.microphone {
        if let Ok(source) = audio::microphone(&settings.microphone_device) {
            sources.push(source);
            audio_source.push("mic");
        }
    }
    if settings.discord {
        // Only when Discord runs as the game starts.
        if let Ok(source) = audio::discord_sound() {
            sources.push(source);
            audio_source.push("discord");
        }
    }
    let mut mixer = AudioMixer::new(sources);
    let audio_tracks = mixer.tracks();
    let fps = settings.fps.clamp(10, 120);
    // Champ select: the video takes the game's size (the monitor), not the client's.
    let source_size = if request.pregame { monitor_size(window).unwrap_or(capture.size()) } else { capture.size() };
    let (width, height) = output_size(source_size, settings.max_height);
    let encoder_settings = EncoderSettings {
        width,
        height,
        fps,
        bitrate: settings.bitrate.clamp(1_000_000, 100_000_000),
        hardware: settings.hardware,
        // A normal MP4: the index is written once at the end and players open it at once.
        // (Fragmented MP4 made the player read the whole file before showing anything.)
        fragmented: false,
        audio_tracks: mixer.tracks().len(),
    };
    let Ok(mut encoder) = VideoEncoder::new(&gpu, path, encoder_settings) else { return Outcome::Failed("recorder_encoder") };

    // Key moments: their own light thread, so a slow answer never delays frames.
    let video_start: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let facts = Arc::new(Mutex::new(None));
    let moments_done = Arc::new(AtomicBool::new(false));
    let moments = {
        let video_start = Arc::clone(&video_start);
        let facts = Arc::clone(&facts);
        let done = Arc::clone(&moments_done);
        thread::spawn(move || {
            let mut tracker: Option<MomentTracker> = None;
            while !done.load(Ordering::Relaxed) {
                if let Some(start) = video_start.get() {
                    let tracker = tracker.get_or_insert_with(|| MomentTracker::new(*start));
                    tracker.poll();
                    if let Ok(mut facts) = facts.lock() {
                        *facts = Some(tracker.facts.clone());
                    }
                }
                for _ in 0..20 {
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            // One last read for the end of the game.
            if let Some(tracker) = tracker.as_mut() {
                tracker.poll();
                if let Ok(mut facts) = facts.lock() {
                    *facts = Some(tracker.facts.clone());
                }
            }
            tracker.map(|tracker| tracker.live).unwrap_or_default()
        })
    };

    let frame_ns = 1_000_000_000 / u64::from(fps);
    let mut clock: Option<Instant> = None;
    let mut tick: u64 = 0;
    let mut failed = false;
    let mut idle_since = Instant::now();
    let now = Instant::now();
    let mut stage = if request.pregame { Stage::Pregame { since: now, looked: now } } else { Stage::Game };
    // Video seconds where the game picture takes over (champ select before it).
    let mut game_from = 0.0;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Stage::Pregame { since, looked } = &mut stage {
            if since.elapsed() > PREGAME_LIMIT {
                shared.discard.store(true, Ordering::Relaxed);
                break;
            }
            if looked.elapsed() >= Duration::from_secs(1) {
                *looked = Instant::now();
                if let Some(game) = find_game_window() {
                    if let Ok(opened) = Capture::open(&gpu, game, settings.screen) {
                        // The game's own sound from now on, on the same track.
                        if settings.game_audio {
                            if let Ok(source) = audio::game_sound(window_process_id(game)) {
                                mixer.replace(source);
                            }
                        }
                        stage = if encoder.hold().is_ok() {
                            Stage::Switch { opened: Instant::now(), first: None, start: None }
                        } else {
                            game_from = tick as f64 / f64::from(fps);
                            Stage::Game
                        };
                        capture = opened;
                        shared.window.store(game.0 as isize, Ordering::Relaxed);
                        shared.in_game.store(true, Ordering::Relaxed);
                    }
                }
            }
        } else if !capture.is_alive() {
            break;
        }
        let pregame = matches!(stage, Stage::Pregame { .. });
        match capture.latest() {
            Ok(Some(frame)) => {
                if encoder.set_input(&frame.texture, frame.region).is_err() {
                    failed = true;
                    break;
                }
            }
            Ok(None) => {}
            // The client window can hide or close while the game starts: keep its last picture.
            Err(_) if pregame => {}
            Err(_) => {
                failed = true;
                break;
            }
        }
        if !encoder.has_input() {
            // Nothing to show yet (loading); give up if the window never draws
            // (the client can stay minimized during champ select: no limit there).
            if !pregame && idle_since.elapsed() > Duration::from_secs(120) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        let start = *clock.get_or_insert_with(|| {
            let now = Instant::now();
            let _ = video_start.set(now);
            mixer.start();
            now
        });
        let time = (tick * 10_000_000 / u64::from(fps)) as i64;
        let written = match &mut stage {
            Stage::Switch { opened, first, start } => {
                if first.is_none() && encoder.has_live_input() {
                    *first = Some(Instant::now());
                }
                let ready = first.is_some_and(|first| first.elapsed() >= GAME_SETTLE) || opened.elapsed() >= GAME_NO_PICTURE;
                if start.is_none() && ready {
                    *start = Some(Instant::now());
                    game_from = tick as f64 / f64::from(fps) + transition::SECONDS / 2.0;
                }
                let progress = start.map_or(0.0, |start| start.elapsed().as_secs_f64() / transition::SECONDS);
                let result = encoder.write_transition(time, progress.min(1.0) as f32);
                if progress >= 1.0 {
                    encoder.release_hold();
                    stage = Stage::Game;
                }
                result
            }
            _ => encoder.write_frame(time),
        };
        if written.is_err() {
            failed = true;
            break;
        }
        if let Some((tracks, audio_time)) = mixer.take() {
            for (track, samples) in tracks.iter().enumerate() {
                let _ = encoder.write_audio(track, samples, audio_time);
            }
        }
        tick += 1;
        // Running late (the PC was busy): skip ticks instead of piling up.
        let due = start.elapsed().as_nanos() as u64 / frame_ns;
        tick = tick.max(due);
        let next = start + Duration::from_nanos(tick * frame_ns);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            thread::sleep(wait);
        }
        idle_since = Instant::now();
    }
    moments_done.store(true, Ordering::Relaxed);
    let encoder_name = encoder.name.clone();
    let hardware = encoder.hardware;
    let has_audio = encoder.has_audio();
    let wrote = clock.is_some();
    let finished = encoder.finish().is_ok();
    let screen = capture.is_screen();
    drop(capture);
    drop(mixer);
    let live = moments.join().unwrap_or_default();
    if !wrote {
        let _ = std::fs::remove_file(path);
        return Outcome::Failed(if failed { "recorder_capture" } else { "recorder_no_frames" });
    }
    let _ = finished;

    let facts = facts.lock().ok().and_then(|mut facts| facts.take()).unwrap_or_default();
    let duration = tick as f64 / f64::from(fps);
    let title = [stamp, facts.champion.as_str(), request.queue.as_str(), facts.result.as_str()]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let final_path = library::unique_file(&directory, &library::safe_name(&title));
    let final_path = if std::fs::rename(path, &final_path).is_ok() { final_path } else { path.to_path_buf() };
    let meta = RecordingMeta {
        kind: "game".into(),
        account: request.account.clone(),
        riot_id: request.riot_id.clone(),
        puuid: request.puuid.clone(),
        champion: facts.champion,
        queue: request.queue.clone(),
        queue_id: request.queue_id,
        result: facts.result,
        started_at,
        duration,
        game_start: facts.offset,
        champ_select: game_from,
        width,
        height,
        fps,
        encoder: if hardware { encoder_name } else { format!("{encoder_name} (software)") },
        capture: if screen { "screen".into() } else { "window".into() },
        audio: has_audio,
        audio_source: audio_source.join("+"),
        audio_tracks: audio_tracks.iter().map(|track| (*track).to_owned()).collect(),
        moments: facts.moments,
        source: String::new(),
        clip_start: 0.0,
        discord: false,
        title: String::new(),
        posts: Vec::new(),
    };
    let _ = library::write_meta(&final_path, &meta);
    if !live.snapshots.is_empty() {
        if let Ok(text) = serde_json::to_string(&live) {
            let _ = std::fs::write(super::gameinfo::live_path(&final_path), text);
        }
    }
    let size = std::fs::metadata(&final_path).map(|metadata| metadata.len()).unwrap_or(0);
    let name = final_path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    Outcome::Saved(Box::new(Entry {
        path: format!("{}/{GAMES}/{name}.mp4", request.account),
        name,
        account: request.account.clone(),
        kind: "game".into(),
        size,
        modified: now_ms(),
        meta: Some(meta),
    }))
}
