//! Game recorder (Medal-like): captures the League game window with Windows
//! Graphics Capture (or Desktop Duplication) and encodes it with Media
//! Foundation, with game sound and microphone through WASAPI, and key moments
//! from Riot's Live Client Data API. Capture only: the game process is never
//! opened, hooked, read or drawn over.

pub mod audio;
pub mod capture;
pub mod clip;
pub mod encoder;
pub mod gameinfo;
pub mod library;
pub mod metrics;
pub mod moments;
pub mod queues;
pub mod session;

use library::{Entry, RecordingMeta, CLIPS};
use std::collections::HashMap;
use std::path::Path;
use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Per working thread: WinRT/COM (multithreaded) and Media Foundation.
pub fn startup() -> windows::core::Result<()> {
    // Already initialised on this thread is fine.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
}

pub fn shutdown() {
    let _ = unsafe { MFShutdown() };
}

/// The video size for a window: the window's own size when it is not taller
/// than `max_height` (0 = no limit), otherwise scaled down to it. Even numbers.
pub fn output_size(window: (u32, u32), max_height: u32) -> (u32, u32) {
    let (width, height) = (window.0.max(2), window.1.max(2));
    let (width, height) = if max_height > 0 && height > max_height {
        ((u64::from(width) * u64::from(max_height) / u64::from(height)) as u32, max_height)
    } else {
        (width, height)
    };
    (width.max(2) & !1, height.max(2) & !1)
}

fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}-{:02}", total / 60, total % 60)
}

/// Save parts of a recording (start, end seconds) as one clip in the account's
/// Clips folder, joined with plain cuts in time order, with the moments inside
/// them. `name` is the clip's file name chosen by the player (a default when
/// empty). Error codes for the interface.
/// Rewrite a fragmented recording as a normal MP4 (stream copy, no quality
/// loss) so it opens and seeks at once. The file keeps its name.
pub fn make_seekable(path: &Path) -> Result<(), &'static str> {
    let temporary = path.with_extension("seekable.tmp");
    startup().map_err(|_| "recorder_media")?;
    let result = clip::cut(path, &temporary, &[(0.0, 1.0e7)], false, &clip::AudioPlan::Track(0));
    shutdown();
    result.map_err(|_| "clip_failed")?;
    std::fs::rename(&temporary, path).map_err(|_| {
        let _ = std::fs::remove_file(&temporary);
        "file_write"
    })
}

/// How a clip is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipMode {
    /// Stream copy from keyframes.
    Fast,
    /// Re-encoded, frame-exact.
    Exact,
    /// Re-encoded under Discord's upload limit for normal accounts.
    Discord,
}

/// Discord's limit is 10 MB; 9.5 MB leaves a margin.
pub const DISCORD_BYTES: u64 = 9_500_000;

/// Highest level for one source (500 %).
pub const MAX_GAIN: f32 = 5.0;

/// The sound for a clip: `tracks` is the recording's track list (empty =
/// one mixed track), `wanted` the sources to keep (`None` = everything),
/// `gains` each source's level (missing = 1.0, as recorded).
pub fn audio_plan(tracks: &[String], wanted: Option<&[String]>, gains: &HashMap<String, f32>) -> clip::AudioPlan {
    let sources: Vec<(usize, &String)> = tracks.iter().enumerate().filter(|(_, track)| track.as_str() != "mix").collect();
    if sources.is_empty() {
        // Older recordings: one mixed track that cannot be split.
        return if wanted.is_some_and(<[String]>::is_empty) { clip::AudioPlan::None } else { clip::AudioPlan::Track(0) };
    }
    let gain = |track: &String| gains.get(track).copied().filter(|gain| gain.is_finite()).unwrap_or(1.0).clamp(0.0, MAX_GAIN);
    let chosen: Vec<(usize, f32)> = sources
        .iter()
        .filter(|(_, track)| wanted.is_none_or(|wanted| wanted.contains(track)))
        .map(|(number, track)| (*number, gain(track)))
        .filter(|(_, gain)| *gain > 0.005)
        .collect();
    let unchanged = chosen.iter().all(|(_, gain)| (gain - 1.0).abs() < 0.005);
    match chosen.len() {
        0 => clip::AudioPlan::None,
        count if unchanged && count == sources.len() => clip::AudioPlan::Track(0),
        1 if unchanged => clip::AudioPlan::Track(chosen[0].0),
        _ => clip::AudioPlan::Mix(chosen),
    }
}

/// Each sound source of a recording as its own audio-only MP4 (label and path
/// relative to the root), copied out of the recording the first time. Empty
/// when the recording has only one track.
pub fn sound_tracks(root: &Path, relative: &str) -> Result<Vec<(String, String)>, &'static str> {
    let source = library::resolve(root, relative).ok_or("recording_missing")?;
    let meta = library::read_meta(&source).unwrap_or_default();
    if meta.audio_tracks.len() < 2 {
        return Ok(Vec::new());
    }
    let folder = relative.rsplit_once('/').map_or("", |(folder, _)| folder);
    let stem = source.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let mut wanted = Vec::new();
    let mut result = Vec::new();
    for (number, label) in meta.audio_tracks.iter().enumerate() {
        if !library::SOURCES.contains(&label.as_str()) {
            continue;
        }
        let file = library::track_file(&source, label);
        let fresh = std::fs::metadata(&file).ok().zip(std::fs::metadata(&source).ok()).is_some_and(|(track, video)| {
            track.len() > 0 && track.modified().ok() >= video.modified().ok()
        });
        if !fresh {
            wanted.push((number, file));
        }
        result.push((label.clone(), format!("{folder}/{}/{stem}.{label}.mp4", library::TRACKS)));
    }
    if !wanted.is_empty() {
        // One copy at a time (the player and the end of a recording may ask together).
        static SPLITTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = SPLITTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        wanted.retain(|(_, file)| std::fs::metadata(file).map_or(true, |track| track.len() == 0 || track.modified().ok() < std::fs::metadata(&source).and_then(|video| video.modified()).ok()));
        if wanted.is_empty() {
            return Ok(result);
        }
        let directory = source.parent().ok_or("recording_missing")?.join(library::TRACKS);
        std::fs::create_dir_all(&directory).map_err(|_| "file_write")?;
        startup().map_err(|_| "recorder_media")?;
        let extracted = clip::extract_tracks(&source, &wanted);
        shutdown();
        extracted.map_err(|_| "clip_failed")?;
    }
    Ok(result)
}

/// `sounds`: the audio sources to keep in the clip (`None` = all of them);
/// `gains`: their levels (the player's volume per source).
#[allow(clippy::too_many_arguments)]
pub fn save_clip(root: &Path, relative: &str, parts: &[(f64, f64)], mode: ClipMode, name: Option<&str>, sounds: Option<&[String]>, gains: &HashMap<String, f32>) -> Result<Entry, &'static str> {
    let source = library::resolve(root, relative).ok_or("recording_missing")?;
    let mut parts: Vec<(f64, f64)> = parts.iter().copied().filter(|(start, end)| start.is_finite() && end.is_finite() && *end > *start + 0.5).map(|(start, end)| (start.max(0.0), end)).collect();
    parts.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Overlapping parts would repeat footage: join them.
    let mut joined: Vec<(f64, f64)> = Vec::new();
    for (start, end) in parts {
        match joined.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => joined.push((start, end)),
        }
    }
    if joined.is_empty() {
        return Err("clip_range");
    }
    let first = joined[0].0;
    let account_dir = source.parent().and_then(Path::parent).ok_or("recording_missing")?;
    let account = account_dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let clips = account_dir.join(CLIPS);
    std::fs::create_dir_all(&clips).map_err(|_| "file_write")?;
    let stem = source.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let wanted = name.map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("{stem} clip {}", format_time(first)));
    let target = library::unique_file(&clips, &library::safe_name(&wanted));
    let original = library::read_meta(&source).unwrap_or_default();
    let plan = audio_plan(&original.audio_tracks, sounds, gains);
    // What the clip's single sound track holds, for its moments file.
    let kept: Vec<String> = match &plan {
        clip::AudioPlan::None => Vec::new(),
        clip::AudioPlan::Track(number) => match original.audio_tracks.get(*number).map(String::as_str) {
            Some("mix") | None => original.audio_source.split('+').filter(|label| !label.is_empty()).map(str::to_owned).collect(),
            Some(label) => vec![label.to_owned()],
        },
        clip::AudioPlan::Mix(numbers) => numbers.iter().filter_map(|(number, _)| original.audio_tracks.get(*number).cloned()).collect(),
    };
    startup().map_err(|_| "recorder_media")?;
    let cut = match mode {
        ClipMode::Fast => clip::cut(&source, &target, &joined, false, &plan),
        ClipMode::Exact => clip::cut(&source, &target, &joined, true, &plan),
        ClipMode::Discord => clip::cut_for_size(&source, &target, &joined, DISCORD_BYTES, &plan),
    };
    let duration = if cut.is_ok() { clip::duration(&target) } else { 0.0 };
    shutdown();
    let landed = cut.map_err(|_| "clip_failed")?;
    let clip_start = landed.first().map_or(first, |part| part.source_start);
    let total = landed.last().map_or(0.0, |part| part.clip_start + (part.source_end - part.source_start));
    let meta = RecordingMeta {
        kind: "clip".into(),
        account: account.clone(),
        duration: if duration > 0.0 { duration } else { total },
        game_start: original.game_start.map(|offset| offset - clip_start),
        moments: original
            .moments
            .iter()
            .filter_map(|moment| {
                let part = landed.iter().find(|part| moment.time >= part.source_start && moment.time <= part.source_end)?;
                Some(moments::Moment { time: part.clip_start + moment.time - part.source_start, ..moment.clone() })
            })
            .collect(),
        source: format!("{stem}.mp4"),
        clip_start,
        audio: !kept.is_empty(),
        audio_source: kept.join("+"),
        // One track in a clip.
        audio_tracks: Vec::new(),
        started_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64),
        ..original
    };
    let _ = library::write_meta(&target, &meta);
    let name = target.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Entry {
        path: format!("{account}/{CLIPS}/{name}.mp4"),
        name,
        account,
        kind: "clip".into(),
        size: std::fs::metadata(&target).map(|metadata| metadata.len()).unwrap_or(0),
        modified: meta.started_at,
        meta: Some(meta),
    })
}
