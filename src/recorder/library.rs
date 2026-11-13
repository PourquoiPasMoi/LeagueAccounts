//! The recordings folder: `<root>/<account>/Games/*.mp4` (full games) and
//! `<root>/<account>/Clips/*.mp4`, each with a `.json` of the same name that
//! holds the key moments and game facts. Listing, deleting, the disk limit
//! (oldest full games go first, clips are never deleted automatically) and
//! safe path checks for playback.

use super::moments::Moment;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const GAMES: &str = "Games";
pub const CLIPS: &str = "Clips";
pub const UNKNOWN: &str = "Unknown";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingMeta {
    /// "game" or "clip".
    pub kind: String,
    /// The account's folder name.
    pub account: String,
    pub riot_id: String,
    pub puuid: String,
    pub champion: String,
    pub queue: String,
    /// "Win", "Loss" or empty.
    pub result: String,
    /// Unix ms.
    pub started_at: u64,
    /// Seconds.
    pub duration: f64,
    /// Video seconds at game time 0.
    pub game_start: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    /// "window" or "screen".
    pub capture: String,
    pub audio: bool,
    /// "game" (game sound only), "pc" (all PC sound), plus "+mic".
    pub audio_source: String,
    /// Audio tracks in order: one source ("game", "pc", "mic", "discord"), or
    /// "mix" followed by each source. Empty in older files: one mixed track.
    pub audio_tracks: Vec<String>,
    pub moments: Vec<Moment>,
    /// Clips: the game recording they come from (file name) and where they start in it.
    pub source: String,
    pub clip_start: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Relative to the root, with `/`.
    pub path: String,
    /// File name without `.mp4`.
    pub name: String,
    pub account: String,
    /// "game" or "clip".
    pub kind: String,
    pub size: u64,
    /// Unix ms of the last change.
    pub modified: u64,
    pub meta: Option<RecordingMeta>,
}

/// Characters Windows refuses in file names become spaces; no leading/trailing dots or spaces.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() { UNKNOWN.to_owned() } else { trimmed.chars().take(120).collect() }
}

/// `directory/name.mp4`, or `name (2).mp4`… when it exists.
pub fn unique_file(directory: &Path, name: &str) -> PathBuf {
    let first = directory.join(format!("{name}.mp4"));
    if !first.exists() {
        return first;
    }
    (2..1000).map(|n| directory.join(format!("{name} ({n}).mp4"))).find(|path| !path.exists()).unwrap_or(first)
}

pub fn meta_path(video: &Path) -> PathBuf {
    video.with_extension("json")
}

pub fn read_meta(video: &Path) -> Option<RecordingMeta> {
    serde_json::from_slice(&std::fs::read(meta_path(video)).ok()?).ok()
}

pub fn write_meta(video: &Path, meta: &RecordingMeta) -> std::io::Result<()> {
    let body = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
    std::fs::write(meta_path(video), body)
}

fn modified_ms(metadata: &std::fs::Metadata) -> u64 {
    metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |time| time.as_millis() as u64)
}

/// Every recording under the root, newest first.
pub fn list(root: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let Ok(accounts) = std::fs::read_dir(root) else { return entries };
    for account in accounts.flatten() {
        if !account.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let account_name = account.file_name().to_string_lossy().into_owned();
        for (folder, kind) in [(GAMES, "game"), (CLIPS, "clip")] {
            let Ok(files) = std::fs::read_dir(account.path().join(folder)) else { continue };
            for file in files.flatten() {
                let path = file.path();
                if !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("mp4")) {
                    continue;
                }
                let Ok(metadata) = file.metadata() else { continue };
                let name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
                entries.push(Entry {
                    path: format!("{account_name}/{folder}/{name}.mp4"),
                    name,
                    account: account_name.clone(),
                    kind: kind.to_owned(),
                    size: metadata.len(),
                    modified: modified_ms(&metadata),
                    meta: read_meta(&path),
                });
            }
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(started(entry)));
    entries
}

fn started(entry: &Entry) -> u64 {
    entry.meta.as_ref().map(|meta| meta.started_at).filter(|time| *time > 0).unwrap_or(entry.modified)
}

/// A recording's full path from its relative path, only when it is an `.mp4`
/// inside the root (no climbing out, no other files).
pub fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() || relative.contains("..") || relative.contains(':') {
        return None;
    }
    let root = std::fs::canonicalize(root).ok()?;
    let path = std::fs::canonicalize(root.join(relative.replace('/', "\\"))).ok()?;
    let is_video = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"));
    (is_video && path.starts_with(&root) && path.is_file()).then(|| plain(path))
}

/// `\\?\C:\…` (what canonicalize returns) → `C:\…`: Media Foundation and
/// Explorer do not take the long form.
fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest.to_owned()),
        _ => path,
    }
}

/// The same check for a `larec://` URL path (percent-encoded).
pub fn resolve_url(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = urlencoding::decode(url_path.trim_start_matches('/')).ok()?;
    resolve(root, &decoded)
}

/// A fragmented MP4 (recordings made before 2026-11-13 evening): players must
/// read every fragment before they can show it. Reads only the first boxes.
pub fn is_fragmented(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut position = 0u64;
    for _ in 0..16 {
        let mut header = [0u8; 16];
        if file.seek(SeekFrom::Start(position)).is_err() || file.read_exact(&mut header).is_err() {
            return false;
        }
        let mut size = u64::from(u32::from_be_bytes([header[0], header[1], header[2], header[3]]));
        if size == 1 {
            size = u64::from_be_bytes([header[8], header[9], header[10], header[11], header[12], header[13], header[14], header[15]]);
        }
        let kind = &header[4..8];
        if kind == b"moov" {
            // A small box at the front; `mvex` inside means fragments follow.
            if size > 16 * 1024 * 1024 {
                return false;
            }
            let mut body = vec![0u8; size as usize];
            return file.seek(SeekFrom::Start(position)).is_ok() && file.read_exact(&mut body).is_ok() && body.windows(4).any(|window| window == b"mvex");
        }
        if kind == b"mdat" || size < 8 {
            return false;
        }
        position += size;
    }
    false
}

/// Hidden folder next to the games with each sound source of a recording as
/// its own audio-only MP4 (made when the recording is first played).
pub const TRACKS: &str = ".tracks";
/// Sound sources that can have their own track.
pub const SOURCES: [&str; 4] = ["game", "pc", "mic", "discord"];

/// `<folder>/.tracks/<name>.<source>.mp4`.
pub fn track_file(video: &Path, source: &str) -> PathBuf {
    let stem = video.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    video.parent().unwrap_or(Path::new(".")).join(TRACKS).join(format!("{stem}.{source}.mp4"))
}

/// Delete a recording, its moments file and its sound track files.
pub fn delete(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)?;
    let _ = std::fs::remove_file(meta_path(path));
    let _ = std::fs::remove_file(path.with_extension("game.json"));
    for source in SOURCES {
        let _ = std::fs::remove_file(track_file(path, source));
    }
    Ok(())
}

/// Total size of all recordings (games and clips).
pub fn total_size(entries: &[Entry]) -> u64 {
    entries.iter().map(|entry| entry.size).sum()
}

/// Delete the oldest full games until everything fits in `limit` bytes.
/// Clips and `keep` (the game being recorded) are never deleted. Returns the
/// number of deleted games.
pub fn enforce_limit(root: &Path, limit: u64, keep: Option<&Path>) -> usize {
    if limit == 0 {
        return 0;
    }
    let entries = list(root);
    let mut total = total_size(&entries);
    if total <= limit {
        return 0;
    }
    let keep = keep.and_then(|path| std::fs::canonicalize(path).ok()).map(plain);
    let mut games: Vec<&Entry> = entries.iter().filter(|entry| entry.kind == "game").collect();
    games.sort_by_key(|entry| started(entry));
    let mut deleted = 0;
    for game in games {
        if total <= limit {
            break;
        }
        let Some(path) = resolve(root, &game.path) else { continue };
        if keep.as_ref().is_some_and(|keep| *keep == path) {
            continue;
        }
        if delete(&path).is_ok() {
            total = total.saturating_sub(game.size);
            deleted += 1;
        }
    }
    deleted
}
