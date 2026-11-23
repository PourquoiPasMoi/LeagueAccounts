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
use super::defrag;
use super::capture::{find_client_window, find_game_window, monitor_size, window_process_id, Capture, Gpu};
use super::encoder::{AudioFile, Codec, EncoderSettings, Rate, VideoEncoder};
use super::library::{self, Entry, RecordingMeta, GAMES};
use super::moments::{GameFacts, Moment, MomentTracker};
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
    /// The mouse pointer is part of the picture.
    pub cursor: bool,
    pub game_audio: bool,
    /// Discord's sound only (voice chat), as its own track.
    pub discord: bool,
    /// The app's music player, as its own track.
    pub music: bool,
    pub microphone: bool,
    /// Windows endpoint id of the microphone; empty = the default communication device.
    pub microphone_device: String,
    /// H.264, or AV1 when the GPU has an AV1 encoder (falls back to H.264).
    pub codec: Codec,
    pub rate: Rate,
    /// After the game, go back to the League client's end-of-game screen.
    pub post_game: bool,
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
/// Longest time on the end-of-game screen (the watcher usually stops sooner,
/// when the player leaves that screen).
const POSTGAME_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How long the game window may refuse window capture before screen capture is used.
const WINDOW_CAPTURE_PATIENCE: Duration = Duration::from_secs(10);
/// A game window that never draws: transition anyway (into the dark colour).
const GAME_NO_PICTURE: Duration = Duration::from_secs(15);

/// How often the game's details so far are saved next to the file being
/// written (what a recording cut short by a crash is rebuilt from).
const JOURNAL_EVERY: Duration = Duration::from_secs(20);

/// The details of a game while it is being recorded: `<file>.recovery.json`.
/// Removed when the recording ends normally; found at the next start after a
/// crash or a power cut (`recover`).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Journal {
    /// Date and time for the file name.
    stamp: String,
    meta: RecordingMeta,
}

fn journal_path(video: &Path) -> PathBuf {
    video.with_extension("recovery.json")
}

fn write_journal(video: &Path, stamp: &str, meta: &RecordingMeta) {
    let Ok(body) = serde_json::to_vec(&Journal { stamp: stamp.to_owned(), meta: meta.clone() }) else { return };
    // Written beside, then swapped in: never half a file.
    let target = journal_path(video);
    let temporary = target.with_extension("tmp");
    if std::fs::write(&temporary, body).is_ok() {
        let _ = std::fs::rename(&temporary, &target);
    }
}

/// What does not change during a recording.
struct Details<'a> {
    request: &'a RecordRequest,
    started_at: u64,
    size: (u32, u32),
    fps: u32,
    codec: String,
    encoder: String,
    audio: bool,
    audio_source: String,
    audio_tracks: Vec<String>,
}

/// The game so far.
struct Progress {
    facts: GameFacts,
    bookmarks: Vec<(f64, u32, u32)>,
    duration: f64,
    game_from: f64,
    post_from: f64,
    screen: bool,
}

impl Details<'_> {
    fn meta(&self, progress: Progress) -> RecordingMeta {
        let Progress { mut facts, bookmarks, duration, game_from, post_from, screen } = progress;
        // Bookmarks join the key moments; `detail` keeps the seconds before and
        // after the press that a clip of the bookmark holds ("60+10").
        for (time, before, after) in bookmarks {
            let time = time.min(duration);
            facts.moments.push(Moment {
                kind: "bookmark".into(),
                time,
                game_time: facts.offset.map_or(0.0, |offset| (time - offset).max(0.0)),
                detail: format!("{before}+{after}"),
                ally: None,
                mine: true,
            });
        }
        facts.moments.sort_by(|a, b| a.time.total_cmp(&b.time));
        RecordingMeta {
            kind: "game".into(),
            account: self.request.account.clone(),
            riot_id: self.request.riot_id.clone(),
            puuid: self.request.puuid.clone(),
            champion: facts.champion,
            queue: self.request.queue.clone(),
            queue_id: self.request.queue_id,
            result: facts.result,
            started_at: self.started_at,
            duration,
            game_start: facts.offset,
            champ_select: game_from,
            post_game: post_from,
            highlights: false,
            codec: self.codec.clone(),
            width: self.size.0,
            height: self.size.1,
            fps: self.fps,
            encoder: self.encoder.clone(),
            capture: if screen { "screen".into() } else { "window".into() },
            audio: self.audio,
            audio_source: self.audio_source.clone(),
            audio_tracks: self.audio_tracks.clone(),
            moments: facts.moments,
            source: String::new(),
            clip_start: 0.0,
            discord: false,
            title: String::new(),
            posts: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// "2026-11-13 21-40 Ahri Ranked Solo Win".
fn file_title(stamp: &str, meta: &RecordingMeta) -> String {
    [stamp, meta.champion.as_str(), meta.queue.as_str(), meta.result.as_str()].iter().filter(|part| !part.is_empty()).copied().collect::<Vec<_>>().join(" ")
}

/// Recordings cut short (the app or the PC stopped mid-game): their index is
/// rebuilt from what reached the disk, and they get their name and details
/// from the journal. `busy` = the file being recorded right now. Returns the
/// recovered files.
pub fn recover(root: &Path, busy: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut recovered = Vec::new();
    let Ok(accounts) = std::fs::read_dir(root) else { return recovered };
    for folder in accounts.flatten().map(|account| account.path().join(GAMES)) {
        let Ok(files) = std::fs::read_dir(&folder) else { continue };
        for journal_file in files.flatten().map(|file| file.path()) {
            let name = journal_file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let Some(stem) = name.strip_suffix(".recovery.json") else { continue };
            let video = folder.join(format!("{stem}.mp4"));
            if busy(&video) {
                continue;
            }
            let journal: Option<Journal> = std::fs::read(&journal_file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
            let _ = std::fs::remove_file(journal_file.with_extension("tmp"));
            // The sound files of an unfinished recording have no index either;
            // they are made again from the recording when it is first played.
            let forget_sounds = || {
                for source in library::SOURCES {
                    let _ = std::fs::remove_file(library::track_file(&video, source));
                }
            };
            // Still in fragments, or stopped between the index and the clean-up.
            let seconds = if video.is_file() { defrag::defragment(&video).ok().map(|fixed| fixed.seconds) } else { None };
            let (Some(seconds), Some(journal)) = (seconds.filter(|seconds| *seconds >= 1.0), journal) else {
                // Nothing usable reached the disk.
                if video.is_file() && std::fs::metadata(&video).is_ok_and(|metadata| metadata.len() < 4 * 1024 * 1024) || seconds.is_some() {
                    let _ = std::fs::remove_file(&video);
                    forget_sounds();
                }
                let _ = std::fs::remove_file(&journal_file);
                continue;
            };
            forget_sounds();
            let mut meta = journal.meta;
            meta.duration = seconds;
            meta.moments.retain(|moment| moment.time <= seconds);
            if meta.post_game > seconds {
                meta.post_game = 0.0;
            }
            let wanted = library::unique_file(&folder, &library::safe_name(&file_title(&journal.stamp, &meta)));
            let target = if std::fs::rename(&video, &wanted).is_ok() { wanted } else { video };
            let _ = library::write_meta(&target, &meta);
            let _ = std::fs::remove_file(&journal_file);
            recovered.push(target);
        }
    }
    recovered
}

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
    /// Back on the League client after the game.
    after: Arc<AtomicBool>,
    /// When the first frame was written (video second 0).
    video_start: Arc<OnceLock<Instant>>,
    /// Bookmarks the player made with the hotkey: (video seconds, seconds
    /// before, seconds after) kept by a clip of it.
    bookmarks: Arc<Mutex<Vec<(f64, u32, u32)>>>,
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

    /// On the end-of-game screen (the watcher stops it when the player leaves).
    pub fn after_game(&self) -> bool {
        self.after.load(Ordering::Relaxed)
    }

    pub fn window(&self) -> isize {
        self.window.load(Ordering::Relaxed)
    }

    /// Mark this moment of the video (the bookmark hotkey). The video seconds,
    /// or None before the first frame.
    pub fn bookmark(&self, before: u32, after: u32) -> Option<f64> {
        let time = self.video_start.get()?.elapsed().as_secs_f64();
        let mut bookmarks = self.bookmarks.lock().ok()?;
        // A double press (key held, bounce) is one bookmark.
        if bookmarks.last().is_some_and(|(last, _, _)| time - last < 1.0) {
            return None;
        }
        bookmarks.push((time, before, after));
        Some(time)
    }

    pub fn bookmark_count(&self) -> usize {
        self.bookmarks.lock().map_or(0, |bookmarks| bookmarks.len())
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
    let after = Arc::new(AtomicBool::new(false));
    let video_start: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let bookmarks = Arc::new(Mutex::new(Vec::new()));
    let started_at = now_ms();
    let thread = {
        let shared = Shared {
            stop: Arc::clone(&stop),
            discard: Arc::clone(&discard),
            window: Arc::clone(&window),
            in_game: Arc::clone(&in_game),
            after: Arc::clone(&after),
            video_start: Arc::clone(&video_start),
            bookmarks: Arc::clone(&bookmarks),
        };
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
    Ok(Recording { stop, discard, window, in_game, after, video_start, bookmarks, thread: Some(thread), path, account: request.account, started_at })
}

struct Shared {
    stop: Arc<AtomicBool>,
    discard: Arc<AtomicBool>,
    window: Arc<AtomicIsize>,
    in_game: Arc<AtomicBool>,
    after: Arc<AtomicBool>,
    video_start: Arc<OnceLock<Instant>>,
    bookmarks: Arc<Mutex<Vec<(f64, u32, u32)>>>,
}

/// Where the recording is: on the client window, moving to the game, or in the game.
enum Stage {
    Pregame { since: Instant, looked: Instant },
    /// New window open (the game's, or the client's again after the game);
    /// `first` = its first picture, `start` = transition start.
    Switch { opened: Instant, first: Option<Instant>, start: Option<Instant>, to_client: bool },
    Game,
    /// The client's end-of-game screen.
    After { since: Instant },
}

fn record(request: &RecordRequest, shared: &Shared, path: &Path, stamp: &str, started_at: u64) -> Outcome {
    let outcome = record_inner(request, shared, path, stamp, started_at);
    // The recording ended on its own: its details are in its `.json` now.
    let _ = std::fs::remove_file(journal_path(path));
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
        if let Ok(opened) = Capture::open(&gpu, window, settings.screen, settings.cursor) {
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
    if settings.music {
        if let Ok(source) = audio::music_sound() {
            sources.push(source);
            audio_source.push("music");
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
        // In fragments while recording, so a crash or a power cut loses only
        // the last moment; made a normal MP4 in place when the game ends
        // (`defrag`), because players read every fragment before showing anything.
        fragmented: true,
        audio_tracks: mixer.tracks().len(),
        codec: settings.codec,
        rate: settings.rate,
    };
    let Ok(mut encoder) = VideoEncoder::new(&gpu, path, encoder_settings) else { return Outcome::Failed("recorder_encoder") };
    // Each source also goes to its own small file now (player volume per source).
    let mut source_files: Vec<Option<AudioFile>> = Vec::new();
    if audio_tracks.len() >= 2 {
        if let Some(directory) = path.parent().map(|parent| parent.join(library::TRACKS)) {
            let _ = std::fs::create_dir_all(&directory);
            source_files = audio_tracks.iter().skip(1).map(|label| AudioFile::create(&library::track_file(path, label)).ok()).collect();
        }
    }

    let details = Details {
        request,
        started_at,
        size: (width, height),
        fps,
        codec: encoder.codec.name().into(),
        encoder: if encoder.hardware { encoder.name.clone() } else { format!("{} (software)", encoder.name) },
        audio: encoder.has_audio(),
        audio_source: audio_source.join("+"),
        audio_tracks: audio_tracks.iter().map(|track| (*track).to_owned()).collect(),
    };
    let mut journal_written = Instant::now();
    write_journal(path, stamp, &details.meta(Progress { facts: GameFacts::default(), bookmarks: Vec::new(), duration: 0.0, game_from: 0.0, post_from: 0.0, screen: capture.is_screen() }));

    // Key moments: their own light thread, so a slow answer never delays frames.
    let video_start = Arc::clone(&shared.video_start);
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
    let mut game_seen: Option<Instant> = None;
    // Video seconds where the end-of-game screen starts (0 = not recorded).
    let mut post_from = 0.0;
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
                    let first_seen = *game_seen.get_or_insert_with(Instant::now);
                    let opened = if first_seen.elapsed() < WINDOW_CAPTURE_PATIENCE {
                        Capture::open_strict(&gpu, game, settings.screen, settings.cursor)
                    } else {
                        Capture::open(&gpu, game, settings.screen, settings.cursor)
                    };
                    if let Ok(opened) = opened {
                        // The game's own sound from now on, on the same track.
                        if settings.game_audio {
                            if let Ok(source) = audio::game_sound(window_process_id(game)) {
                                mixer.replace(source);
                            }
                        }
                        stage = if encoder.hold().is_ok() {
                            Stage::Switch { opened: Instant::now(), first: None, start: None, to_client: false }
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
            // The game ended: back to the client's end-of-game screen, with the
            // same transition, when asked; otherwise the recording ends here.
            let mut moved = false;
            if matches!(stage, Stage::Game) && settings.post_game {
                if let Some(client) = find_client_window() {
                    if let Ok(opened) = Capture::open(&gpu, client, settings.screen, settings.cursor) {
                        if encoder.hold().is_ok() {
                            if settings.game_audio {
                                if let Ok(source) = audio::game_sound(window_process_id(client)) {
                                    mixer.replace(source);
                                }
                            }
                            capture = opened;
                            shared.window.store(client.0 as isize, Ordering::Relaxed);
                            shared.after.store(true, Ordering::Relaxed);
                            stage = Stage::Switch { opened: Instant::now(), first: None, start: None, to_client: true };
                            moved = true;
                        }
                    }
                }
            }
            if !moved {
                break;
            }
        }
        if let Stage::After { since } = &stage {
            if since.elapsed() > POSTGAME_LIMIT {
                break;
            }
        }
        // On the client (champ select, end of game): its window may hide or close.
        let pregame = matches!(stage, Stage::Pregame { .. }) || shared.after.load(Ordering::Relaxed);
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
            Stage::Switch { opened, first, start, to_client } => {
                let to_client = *to_client;
                if first.is_none() && encoder.has_live_input() {
                    *first = Some(Instant::now());
                }
                let ready = first.is_some_and(|first| first.elapsed() >= GAME_SETTLE) || opened.elapsed() >= GAME_NO_PICTURE;
                if start.is_none() && ready {
                    *start = Some(Instant::now());
                    let middle = tick as f64 / f64::from(fps) + transition::SECONDS / 2.0;
                    if to_client {
                        post_from = middle;
                    } else {
                        game_from = middle;
                    }
                }
                let progress = start.map_or(0.0, |start| start.elapsed().as_secs_f64() / transition::SECONDS);
                let result = encoder.write_transition(time, progress.min(1.0) as f32);
                if progress >= 1.0 {
                    encoder.release_hold();
                    stage = if to_client { Stage::After { since: Instant::now() } } else { Stage::Game };
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
                if let Some(Some(file)) = track.checked_sub(1).and_then(|index| source_files.get_mut(index)) {
                    let _ = file.write(samples, audio_time);
                }
            }
        }
        tick += 1;
        if journal_written.elapsed() >= JOURNAL_EVERY {
            journal_written = Instant::now();
            let so_far = Progress {
                facts: facts.lock().ok().and_then(|facts| facts.clone()).unwrap_or_default(),
                bookmarks: shared.bookmarks.lock().map(|bookmarks| bookmarks.clone()).unwrap_or_default(),
                duration: tick as f64 / f64::from(fps),
                game_from,
                post_from,
                screen: capture.is_screen(),
            };
            write_journal(path, stamp, &details.meta(so_far));
        }
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
    let wrote = clock.is_some();
    let _ = encoder.finish();
    // The index of a normal MP4, written at the end of the same file. Should
    // it fail, the recording stays in fragments and is done at the next start.
    if wrote && !shared.discard.load(Ordering::Relaxed) {
        let _ = defrag::defragment(path);
    }
    // After the video, so the sound files count as up to date (newer than it).
    let mut written_sources = Vec::new();
    for (label, file) in audio_tracks.iter().skip(1).zip(source_files) {
        if let Some(file) = file {
            if file.finish().is_ok() {
                written_sources.push(*label);
            } else {
                let _ = std::fs::remove_file(library::track_file(path, label));
            }
        }
    }
    let screen = capture.is_screen();
    drop(capture);
    drop(mixer);
    let live = moments.join().unwrap_or_default();
    if !wrote || shared.discard.load(Ordering::Relaxed) {
        for label in &written_sources {
            let _ = std::fs::remove_file(library::track_file(path, label));
        }
    }
    if !wrote {
        let _ = std::fs::remove_file(path);
        return Outcome::Failed(if failed { "recorder_capture" } else { "recorder_no_frames" });
    }

    let duration = tick as f64 / f64::from(fps);
    let meta = details.meta(Progress {
        facts: facts.lock().ok().and_then(|mut facts| facts.take()).unwrap_or_default(),
        bookmarks: shared.bookmarks.lock().map(|bookmarks| bookmarks.clone()).unwrap_or_default(),
        duration,
        game_from,
        post_from,
        screen,
    });
    let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let final_path = library::unique_file(&directory, &library::safe_name(&file_title(stamp, &meta)));
    let final_path = if std::fs::rename(path, &final_path).is_ok() { final_path } else { path.to_path_buf() };
    for label in &written_sources {
        let _ = std::fs::rename(library::track_file(path, label), library::track_file(&final_path, label));
    }
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
