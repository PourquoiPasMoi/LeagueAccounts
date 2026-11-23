//! Moving every recording, clip and their data from one recordings folder to
//! another (the player changed the folder, or wants an older folder's
//! recordings brought in). Only what the app wrote is touched:
//! `<account>\Games` and `<account>\Clips` with everything inside. On the same
//! drive a file is renamed (instant); across drives it is copied to
//! `<name>.part`, checked, renamed, and only then removed from the old place,
//! so a power cut never loses a recording. Running it again continues where it
//! stopped.

use super::library::{CLIPS, GAMES};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// One file to move: where it is and its path under the recordings folder.
#[derive(Clone, Debug)]
pub struct Item {
    pub source: PathBuf,
    pub relative: PathBuf,
    pub size: u64,
}

/// What a folder holds that would be moved.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Survey {
    pub files: usize,
    pub bytes: u64,
    /// Recordings and clips (the videos) among the files.
    pub videos: usize,
}

/// What happened to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    /// Now in the new folder, gone from the old one.
    Done,
    /// A different file of that name is already in the new folder: left where it was.
    Kept,
    /// Stopped by `go` before it was complete: the old file is untouched.
    Stopped,
}

fn is_video(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
}

fn walk(folder: &Path, relative: &Path, out: &mut Vec<Item>) {
    let Ok(files) = std::fs::read_dir(folder) else { return };
    for file in files.flatten() {
        let path = file.path();
        let below = relative.join(file.file_name());
        let Ok(kind) = file.file_type() else { continue };
        if kind.is_dir() {
            walk(&path, &below, out);
        } else if kind.is_file() && !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("part")) {
            out.push(Item { source: path, relative: below, size: file.metadata().map_or(0, |metadata| metadata.len()) });
        }
    }
}

/// Every file to move, ordered so that a video always arrives before its data
/// (a details file without its video would look like a leftover), newest
/// videos first.
pub fn items(from: &Path) -> Vec<Item> {
    let mut all = Vec::new();
    let Ok(accounts) = std::fs::read_dir(from) else { return all };
    for account in accounts.flatten() {
        if !account.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        for folder in [GAMES, CLIPS] {
            let relative = Path::new(&account.file_name()).join(folder);
            walk(&account.path().join(folder), &relative, &mut all);
        }
    }
    let modified = |item: &Item| std::fs::metadata(&item.source).and_then(|metadata| metadata.modified()).ok();
    let mut videos: Vec<Item> = all.iter().filter(|item| is_video(&item.source) && item.relative.components().count() == 3).cloned().collect();
    videos.sort_by_key(|item| std::cmp::Reverse(modified(item)));
    let mut taken = vec![false; all.len()];
    let mut ordered = Vec::with_capacity(all.len());
    for video in videos {
        let Some(folder) = video.relative.parent() else { continue };
        let stem = video.relative.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        let prefix = format!("{stem}.");
        // The video first, then everything named after it (also in sub folders such as the sound tracks).
        let mut group: Vec<usize> = (0..all.len())
            .filter(|&index| !taken[index])
            .filter(|&index| all[index].relative.starts_with(folder) && all[index].relative.file_name().is_some_and(|name| name.to_string_lossy().starts_with(&prefix)))
            .collect();
        group.sort_by_key(|&index| all[index].relative != video.relative);
        for index in group {
            taken[index] = true;
            ordered.push(all[index].clone());
        }
    }
    ordered.extend(all.iter().zip(&taken).filter(|(_, taken)| !**taken).map(|(item, _)| item.clone()));
    ordered
}

pub fn survey(from: &Path) -> Survey {
    let items = items(from);
    Survey { files: items.len(), bytes: items.iter().map(|item| item.size).sum(), videos: items.iter().filter(|item| is_video(&item.source)).count() }
}

/// Both folders are on the same drive (a move is then a rename, no copy).
pub fn same_drive(from: &Path, to: &Path) -> bool {
    let drive = |path: &Path| path.components().next().map(|part| part.as_os_str().to_string_lossy().to_ascii_uppercase());
    drive(from).is_some() && drive(from) == drive(to)
}

/// Free bytes on the drive of `path` (its nearest existing folder).
pub fn free_space(path: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let existing = path.ancestors().find(|folder| folder.is_dir())?;
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(existing.as_os_str()), Some(&mut free), None, None).ok()? };
    Some(free)
}

fn temporary(target: &Path) -> PathBuf {
    let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!("{name}.part"))
}

/// Copy with progress. `go(bytes)` gets the bytes just written and returns
/// false to stop. The copy keeps the file's modified time.
fn copy(source: &Path, target: &Path, go: &mut dyn FnMut(u64) -> bool) -> std::io::Result<bool> {
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::File::create(target)?;
    let mut buffer = vec![0u8; 4 << 20];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        if !go(read as u64) {
            return Ok(false);
        }
    }
    output.sync_all()?;
    if let Ok(modified) = input.metadata().and_then(|metadata| metadata.modified()) {
        let _ = output.set_modified(modified);
    }
    Ok(true)
}

/// Move one file into `to`. See the module text for the order of steps.
pub fn move_file(item: &Item, to: &Path, go: &mut dyn FnMut(u64) -> bool) -> std::io::Result<Moved> {
    move_file_with(item, to, go, true)
}

/// `rename: false` always copies, as between two drives (for tests on one drive).
pub fn move_file_with(item: &Item, to: &Path, go: &mut dyn FnMut(u64) -> bool, rename: bool) -> std::io::Result<Moved> {
    let target = to.join(&item.relative);
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder)?;
    }
    if let Ok(existing) = std::fs::metadata(&target) {
        if existing.len() != item.size {
            return Ok(Moved::Kept);
        }
        // Already there (a move that was cut off after the copy): only the old one is left to remove.
        std::fs::remove_file(&item.source)?;
        go(item.size);
        return Ok(Moved::Done);
    }
    if rename && std::fs::rename(&item.source, &target).is_ok() {
        go(item.size);
        return Ok(Moved::Done);
    }
    let part = temporary(&target);
    let complete = copy(&item.source, &part, go);
    let copied = matches!(complete, Ok(true)) && std::fs::metadata(&part).is_ok_and(|metadata| metadata.len() == item.size);
    if !copied {
        let _ = std::fs::remove_file(&part);
        return match complete {
            Ok(false) => Ok(Moved::Stopped),
            Ok(true) => Err(std::io::Error::other("size")),
            Err(error) => Err(error),
        };
    }
    std::fs::rename(&part, &target)?;
    // The new file is complete; an old one that is still open (being played) is removed by the next run.
    std::fs::remove_file(&item.source)?;
    Ok(Moved::Done)
}

/// Remove the folders the move emptied (never `from` itself).
pub fn remove_empty(from: &Path) {
    fn prune(folder: &Path) -> bool {
        let Ok(files) = std::fs::read_dir(folder) else { return false };
        let mut empty = true;
        for file in files.flatten() {
            let path = file.path();
            // An unfinished file of a save that was cut off: nothing to keep.
            let leftover = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("part")) && std::fs::remove_file(&path).is_ok();
            let keep = if file.file_type().is_ok_and(|kind| kind.is_dir()) { !prune(&path) } else { !leftover };
            empty &= !keep;
        }
        empty && std::fs::remove_dir(folder).is_ok()
    }
    let Ok(accounts) = std::fs::read_dir(from) else { return };
    for account in accounts.flatten() {
        if !account.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let had = [GAMES, CLIPS].iter().filter(|folder| account.path().join(folder).is_dir()).count();
        let gone = [GAMES, CLIPS].iter().filter(|folder| account.path().join(folder).is_dir() && prune(&account.path().join(folder))).count();
        if had > 0 && had == gone {
            let _ = std::fs::remove_dir(account.path());
        }
    }
}
