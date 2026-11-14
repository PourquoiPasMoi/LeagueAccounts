//! Replace the running portable executable with a verified download, and hand
//! over to the new process.
//!
//! Windows lets a running executable be renamed but not overwritten, and only
//! within its own drive, so the swap renames `<name>.exe` to a hidden
//! `.<name>.old.exe` beside it and moves the (also hidden) download into its
//! place. An install receipt in `%APPDATA%\LeagueAccounts\update` records the
//! target build. The new process marks it healthy once it is ready and then
//! deletes the hidden copy; if it dies before that, a watcher started from the
//! old copy puts the previous version back (see `watch_update`). Nothing but
//! the exe itself stays visible in the user's folder.

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `--updated-from <pid>`: the previous version started this process.
pub const UPDATED_FROM_FLAG: &str = "--updated-from";
/// Start minimized regardless of settings, as the window was before a restart.
pub const MINIMIZED_FLAG: &str = "--minimized";
/// A download older than this was abandoned (the app closed or crashed).
const STALE_DOWNLOAD: Duration = Duration::from_secs(60 * 60);
/// `--update-watch <pid>`: run as the rollback watcher for the new process.
pub const WATCH_FLAG: &str = "--update-watch";
/// `--update-exe <path>`: the executable the watcher guards.
pub const WATCH_EXE_FLAG: &str = "--update-exe";
/// `--update-failed <version>`: the previous version was restored after this
/// release failed to start.
pub const FAILED_FLAG: &str = "--update-failed";
/// How long the new version has to report healthy before the watcher stops
/// waiting (a slow start is not a failure; only an exit is).
const WATCH_TIMEOUT: Duration = Duration::from_secs(180);

/// Serialize receipt read-modify-write cycles in this process, so a background
/// swap and a readiness report cannot interleave. Held only across disk work.
static RECEIPT_LOCK: Mutex<()> = Mutex::new(());

fn lock_receipts() -> MutexGuard<'static, ()> {
    RECEIPT_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

/// What the last swap installed, persisted beside the executable. `healthy`
/// becomes true only after the new build has started and reported ready.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct InstallReceipt {
    pub target_version: String,
    pub installed_at_ms: u64,
    pub healthy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The running executable, under whatever name the user gave it.
    pub exe: PathBuf,
    /// `.<name>.exe.download` (hidden): on the same drive, so the swap is a rename.
    pub staged: PathBuf,
    /// `.<name>.old.exe` (hidden): the replaced version until the new one is healthy.
    pub old: PathBuf,
    /// The target build and its health, in the app's data folder.
    pub receipt: PathBuf,
    /// Visible files earlier versions left beside the exe, cleaned up at start.
    pub legacy_old: PathBuf,
    pub legacy_receipt: PathBuf,
    pub legacy_staged: PathBuf,
}

impl Paths {
    pub fn for_exe(exe: &Path) -> Option<Self> {
        let directory = exe.parent()?;
        let file_name = exe.file_name()?;
        let stem = exe.file_stem()?;
        Some(Self {
            exe: exe.to_owned(),
            staged: directory.join(concat(&concat(OsStr::new("."), &file_name.to_string_lossy()), ".download")),
            old: directory.join(concat(&concat(OsStr::new("."), &stem.to_string_lossy()), ".old.exe")),
            receipt: receipt_path(exe),
            legacy_old: directory.join(concat(stem, ".old.exe")),
            legacy_receipt: directory.join(concat(file_name, ".receipt.json")),
            legacy_staged: directory.join(concat(file_name, ".download")),
        })
    }

    pub fn current() -> Option<Self> {
        Self::for_exe(&std::env::current_exe().ok()?)
    }
}

/// One receipt per executable location, so copies in different folders don't mix.
fn receipt_path(exe: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(exe.to_string_lossy().to_lowercase().as_bytes());
    let name: String = digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect();
    let base = crate::utils::app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("LeagueAccounts"));
    base.join("update").join(format!("{name}.json"))
}

/// Set or clear the hidden file attribute (Explorer hides these by default).
#[cfg(windows)]
fn set_hidden(path: &Path, hidden: bool) {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let Ok(metadata) = fs::metadata(path) else { return };
    let attributes = metadata.file_attributes();
    let wanted = if hidden { attributes | FILE_ATTRIBUTE_HIDDEN } else { attributes & !FILE_ATTRIBUTE_HIDDEN };
    if wanted == attributes {
        return;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
    unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), wanted);
    }
}

#[cfg(not(windows))]
fn set_hidden(_path: &Path, _hidden: bool) {}

fn concat(name: &OsStr, suffix: &str) -> OsString {
    let mut name = name.to_owned();
    name.push(suffix);
    name
}

/// Create the (hidden) download file. Fails early in a read-only folder.
pub fn create_staged(paths: &Paths) -> io::Result<File> {
    // A hidden file can't be truncated by a plain create on Windows; start fresh.
    let _ = fs::remove_file(&paths.staged);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&paths.staged)?;
    set_hidden(&paths.staged, true);
    Ok(file)
}

/// Move the running executable aside and the verified download into place.
///
/// The receipt is written first, so the process that starts from the new image
/// can prove which build was installed. Any failure restores both the original
/// executable and the previous receipt; a target is never left that could be
/// mistaken for a successful install.
pub fn swap(paths: &Paths, target_version: &str) -> io::Result<()> {
    if target_version.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "update target version is empty",
        ));
    }
    let _guard = lock_receipts();
    if let Some(parent) = paths.receipt.parent() {
        fs::create_dir_all(parent)?;
    }
    let previous_bytes = match fs::read(&paths.receipt) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    ensure_rollback_window_closed(paths, previous_bytes.as_deref())?;
    let previous = previous_bytes
        .as_deref()
        .map(|bytes| prepare_receipt(&paths.receipt, bytes))
        .transpose()?;
    let receipt = InstallReceipt {
        target_version: target_version.to_owned(),
        installed_at_ms: now_ms(),
        healthy: false,
    };
    store_receipt(&paths.receipt, &receipt)?;
    match swap_executable(paths) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(restore_error) = restore_receipt(&paths.receipt, previous) {
                // Never retain the attempted target if restoring an older
                // receipt itself failed. Return the recovery failure instead.
                match fs::remove_file(&paths.receipt) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                return Err(restore_error);
            }
            Err(error)
        }
    }
}

/// A pending rollback copy may be replaced once the running build has proven
/// itself: its install is marked healthy, so the running executable becomes
/// the new rollback copy and the older one is no longer needed. Only an install
/// that never reported healthy keeps its rollback copy, since that copy may be
/// the last build known to work. (The original rule also waited 24 hours,
/// which refused every second release published on the same day.)
fn ensure_rollback_window_closed(paths: &Paths, previous: Option<&[u8]>) -> io::Result<()> {
    match fs::symlink_metadata(&paths.old) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let unproven = previous
        .and_then(|bytes| serde_json::from_slice::<InstallReceipt>(bytes).ok())
        .is_some_and(|receipt| !receipt.healthy);
    if unproven {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the installed version has not started successfully yet",
        ))
    } else {
        Ok(())
    }
}

fn swap_executable(paths: &Paths) -> io::Result<()> {
    // Keep any previous rollback copy until the entire new swap has succeeded.
    // In particular, an absent download must not delete the last good image.
    let old_backup = match fs::symlink_metadata(&paths.old) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "rollback copy is not a file"));
            }
            let parent = paths.old.parent().unwrap_or_else(|| Path::new("."));
            let folder = tempfile::tempdir_in(parent)?;
            let saved = folder.path().join("previous.old.exe");
            fs::rename(&paths.old, &saved)?;
            Some((folder, saved))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut original_restored = true;
    let result = (|| {
        fs::rename(&paths.exe, &paths.old)?;
        original_restored = false;
        set_hidden(&paths.old, true);
        if let Err(error) = fs::rename(&paths.staged, &paths.exe) {
            // Do not hide a failed recovery behind the original install error.
            set_hidden(&paths.old, false);
            fs::rename(&paths.old, &paths.exe)?;
            original_restored = true;
            return Err(error);
        }
        set_hidden(&paths.exe, false);
        Ok(())
    })();
    if result.is_err() {
        if let Some((folder, saved)) = old_backup {
            if !original_restored {
                // The current image remains at `old`; preserve the earlier
                // backup too rather than overwriting either during recovery.
                let _ = folder.keep();
                return result;
            }
            if let Err(error) = fs::rename(&saved, &paths.old) {
                // A filesystem failure during recovery must not delete the
                // saved image when the temporary directory is dropped.
                let _ = folder.keep();
                return Err(error);
            }
        }
    }
    result
}

fn store_receipt(path: &Path, receipt: &InstallReceipt) -> io::Result<()> {
    let json = serde_json::to_vec(receipt).map_err(io::Error::other)?;
    store_receipt_bytes(path, &json)
}

fn store_receipt_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    prepare_receipt(path, bytes)?
        .persist(path)
        .map(|_| ())
        .map_err(|error| error.error)
}

fn prepare_receipt(path: &Path, bytes: &[u8]) -> io::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // Unique sibling files avoid colliding with another instance. Dropping a
    // failed write removes its temporary file.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}

fn restore_receipt(path: &Path, previous: Option<tempfile::NamedTempFile>) -> io::Result<()> {
    if let Some(file) = previous {
        return match file.persist(path) {
            Ok(_) => Ok(()),
            Err(error) => {
                // Preserve the prepared rollback data if the filesystem
                // refused even its atomic restoration.
                let _ = error.file.keep();
                Err(error.error)
            }
        };
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_receipt(path: &Path) -> Option<InstallReceipt> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Remove what an update left behind: the hidden previous version once the new
/// build has reported healthy, an abandoned download, and the visible files
/// earlier versions kept beside the exe (moving an old receipt into place).
pub fn clean_up(paths: &Paths, after_update: bool) {
    let _guard = lock_receipts();
    // Earlier versions kept the receipt beside the exe: move it, then judge
    // their visible rollback copy by it.
    if let Ok(bytes) = fs::read(&paths.legacy_receipt) {
        if !paths.receipt.exists() {
            if let Some(parent) = paths.receipt.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = store_receipt_bytes(&paths.receipt, &bytes);
        }
        let _ = fs::remove_file(&paths.legacy_receipt);
    }
    let receipt = read_receipt(&paths.receipt);
    let proven = receipt.as_ref().is_none_or(|receipt| receipt.healthy);
    if proven {
        // The image of a process that has just exited can stay locked briefly.
        let attempts = if after_update { 10 } else { 1 };
        for old in [&paths.old, &paths.legacy_old] {
            for attempt in 1..=attempts {
                set_hidden(old, false);
                match fs::remove_file(old) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound && attempt < attempts => {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    _ => break,
                }
            }
        }
    }
    for staged in [&paths.staged, &paths.legacy_staged] {
        let stale = fs::metadata(staged)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_DOWNLOAD));
        if stale {
            let _ = fs::remove_file(staged);
        }
    }
}

/// After the new version reported healthy: delete the hidden previous version.
/// The watcher runs from that file until it sees the healthy receipt, so keep
/// trying for a few seconds.
pub fn remove_previous(paths: &Paths) {
    // Updates from older versions leave a visible copy instead of the hidden one.
    for old in [&paths.old, &paths.legacy_old] {
        for _ in 0..40 {
            set_hidden(old, false);
            match fs::remove_file(old) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => std::thread::sleep(Duration::from_millis(250)),
                _ => break,
            }
        }
    }
}

/// What the watcher concluded.
pub enum WatchOutcome {
    /// The new version reported healthy (or is still starting): nothing to do.
    Healthy,
    /// It exited before reporting healthy and the previous version is back.
    RolledBack { failed_version: String },
}

/// Run from the hidden previous version right after an update restart. Waits
/// for the new process to report healthy; if it exits first, puts this (the
/// previous) version back in place.
pub fn watch_update(exe: &Path, new_pid: u32) -> WatchOutcome {
    let Some(paths) = Paths::for_exe(exe) else { return WatchOutcome::Healthy };
    let started = std::time::Instant::now();
    loop {
        if read_receipt(&paths.receipt).is_some_and(|receipt| receipt.healthy) {
            return WatchOutcome::Healthy;
        }
        if wait_for_exit(new_pid, Duration::from_millis(500)) {
            // Exited. A healthy report may have landed just before.
            if read_receipt(&paths.receipt).is_some_and(|receipt| receipt.healthy) {
                return WatchOutcome::Healthy;
            }
            break;
        }
        if started.elapsed() > WATCH_TIMEOUT {
            // Still running but slow: leave it; its own start-up decides.
            return WatchOutcome::Healthy;
        }
    }
    let failed_version = read_receipt(&paths.receipt).map(|receipt| receipt.target_version).unwrap_or_default();
    let _guard = lock_receipts();
    // Put the previous version back: the failed build out, the hidden copy in.
    for attempt in 1..=20 {
        let moved = fs::remove_file(&paths.exe)
            .or_else(|error| if error.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(error) })
            .and_then(|()| {
                set_hidden(&paths.old, false);
                fs::rename(&paths.old, &paths.exe)
            });
        match moved {
            Ok(()) => break,
            Err(_) if attempt < 20 => std::thread::sleep(Duration::from_millis(250)),
            Err(_) => return WatchOutcome::Healthy,
        }
    }
    let _ = fs::remove_file(&paths.receipt);
    WatchOutcome::RolledBack { failed_version }
}

/// `--update-watch <pid>` and `--update-exe <path>` when this process is the watcher.
pub fn watch_request(args: &[OsString]) -> Option<(PathBuf, u32)> {
    let pid = flag_value(args, WATCH_FLAG)?.parse().ok()?;
    let exe = PathBuf::from(flag_value(args, WATCH_EXE_FLAG)?);
    Some((exe, pid))
}

/// `--update-failed <version>` when the previous version was just restored.
pub fn failed_update(args: &[OsString]) -> Option<String> {
    flag_value(args, FAILED_FLAG).map(str::to_owned).filter(|version| !version.is_empty())
}

fn flag_value<'a>(args: &'a [OsString], flag: &str) -> Option<&'a str> {
    let position = args.iter().position(|arg| arg == flag)?;
    args.get(position + 1)?.to_str()
}

/// The pid passed with `--updated-from`, if this process was started by an update.
pub fn previous_process(args: &[OsString]) -> Option<u32> {
    flag_value(args, UPDATED_FROM_FLAG)?.parse().ok()
}

/// The build the last swap installed, taken from the receipt beside the
/// executable. Healthy or not, this is what the new process compares with its
/// own version before reporting ready.
/// An update was installed here but has not reported healthy yet.
pub fn update_pending(paths: &Paths) -> bool {
    read_receipt(&paths.receipt).is_some_and(|receipt| !receipt.healthy)
}

pub fn installed_target(paths: &Paths) -> Option<String> {
    read_receipt(&paths.receipt).map(|receipt| receipt.target_version)
}

/// Mark the installed target healthy after the process it started has passed
/// its readiness checks. The receipt is read and verified under the same lock
/// as `swap`, so a background install cannot change the target between the
/// caller's check and this write. A receipt for a different build is refused,
/// leaving its bytes untouched.
pub fn mark_healthy(paths: &Paths, current_version: &str) -> io::Result<()> {
    let _guard = lock_receipts();
    let mut receipt = read_receipt(&paths.receipt)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "install receipt is missing"))?;
    if receipt.target_version != current_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "install receipt targets a different version",
        ));
    }
    receipt.healthy = true;
    store_receipt(&paths.receipt, &receipt)
}

/// Arguments for the new process: the original ones (so `--autostart` keeps
/// Start minimized working) without an earlier hand-over, plus this one.
pub fn restart_args(args: &[OsString], pid: u32, minimized: bool) -> Vec<OsString> {
    let mut kept = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if arg == UPDATED_FROM_FLAG || arg == FAILED_FLAG {
            skip_value = true;
        } else if arg != MINIMIZED_FLAG {
            kept.push(arg.clone());
        }
    }
    kept.extend([UPDATED_FROM_FLAG.into(), pid.to_string().into()]);
    if minimized {
        kept.push(MINIMIZED_FLAG.into());
    }
    kept
}

/// Wait for the previous version to exit, so its files and WebView2 profile
/// are released before this process opens them. False on a timeout.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    // SAFETY: a wait-only handle; an exited pid is reported as an error.
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if raw.is_null() {
        return true;
    }
    // SAFETY: OpenProcess returned a new owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: the handle is valid for the duration of the wait.
    unsafe { WaitForSingleObject(handle.as_raw_handle(), millis) == WAIT_OBJECT_0 }
}

#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32, _timeout: Duration) -> bool {
    true
}
