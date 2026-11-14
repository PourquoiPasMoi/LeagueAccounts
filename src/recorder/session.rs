//! One recorded game, on its own thread: capture → GPU conversion → encoder,
//! sound mixed in, key moments polled on a second light thread. When the game
//! window closes (or `stop` is called) the file is finished, renamed to e.g.
//! `2026-11-13 21-40 Ahri Ranked Solo Win.mp4` and its moments are written next
//! to it.

use super::audio::{self, AudioMixer};
use super::capture::{window_process_id, Capture, Gpu};
use super::encoder::{EncoderSettings, VideoEncoder};
use super::library::{self, Entry, RecordingMeta, GAMES};
use super::moments::MomentTracker;
use super::{output_size, shutdown, startup};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
}

pub enum Outcome {
    Saved(Box<Entry>),
    /// Error code for the log and the interface.
    Failed(&'static str),
}

pub struct Recording {
    stop: Arc<AtomicBool>,
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
    let started_at = now_ms();
    let window = window.0 as isize;
    let thread = {
        let stop = Arc::clone(&stop);
        let path = path.clone();
        let request = request.clone();
        thread::Builder::new()
            .name("recorder".into())
            .spawn(move || {
                let outcome = record(&request, HWND(window as *mut core::ffi::c_void), &path, &stop, &stamp, started_at);
                shutdown();
                outcome
            })?
    };
    Ok(Recording { stop, thread: Some(thread), path, account: request.account, started_at })
}

fn record(request: &RecordRequest, window: HWND, path: &Path, stop: &AtomicBool, stamp: &str, started_at: u64) -> Outcome {
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
    let (width, height) = output_size(capture.size(), settings.max_height);
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
        })
    };

    let frame_ns = 1_000_000_000 / u64::from(fps);
    let mut clock: Option<Instant> = None;
    let mut tick: u64 = 0;
    let mut failed = false;
    let mut idle_since = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) || !capture.is_alive() {
            break;
        }
        match capture.latest() {
            Ok(Some(frame)) => {
                if encoder.set_input(&frame.texture, frame.region).is_err() {
                    failed = true;
                    break;
                }
            }
            Ok(None) => {}
            Err(_) => {
                failed = true;
                break;
            }
        }
        if !encoder.has_input() {
            // Nothing to show yet (loading); give up if the window never draws.
            if idle_since.elapsed() > Duration::from_secs(120) {
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
        if encoder.write_frame(time).is_err() {
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
    let _ = moments.join();
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
