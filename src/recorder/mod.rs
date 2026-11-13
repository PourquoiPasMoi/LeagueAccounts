//! Game recorder (Medal-like): captures the League game window with Windows
//! Graphics Capture (or Desktop Duplication) and encodes it with Media
//! Foundation, with game sound and microphone through WASAPI, and key moments
//! from Riot's Live Client Data API. Capture only: the game process is never
//! opened, hooked, read or drawn over.

pub mod audio;
pub mod capture;
pub mod clip;
pub mod encoder;
pub mod library;
pub mod metrics;
pub mod moments;
pub mod session;

use library::{Entry, RecordingMeta, CLIPS};
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
    let result = clip::cut(path, &temporary, &[(0.0, 1.0e7)], false);
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

pub fn save_clip(root: &Path, relative: &str, parts: &[(f64, f64)], mode: ClipMode, name: Option<&str>) -> Result<Entry, &'static str> {
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
    startup().map_err(|_| "recorder_media")?;
    let cut = match mode {
        ClipMode::Fast => clip::cut(&source, &target, &joined, false),
        ClipMode::Exact => clip::cut(&source, &target, &joined, true),
        ClipMode::Discord => clip::cut_for_size(&source, &target, &joined, DISCORD_BYTES),
    };
    let duration = if cut.is_ok() { clip::duration(&target) } else { 0.0 };
    shutdown();
    let landed = cut.map_err(|_| "clip_failed")?;
    let original = library::read_meta(&source).unwrap_or_default();
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
