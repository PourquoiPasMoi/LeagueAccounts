//! Replace the running portable executable with a verified download, and hand
//! over to the new process.
//!
//! Windows lets a running executable be renamed but not overwritten, so the
//! swap renames `<name>.exe` to `<name>.old.exe` and moves the download into
//! its place. An install receipt written before the swap records the target
//! build; the next process marks it healthy once it is ready, and only then,
//! a day later, removes the old copy.

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
/// Keep the replaced executable this long after a healthy install, so a bad
/// release can still be rolled back by hand.
const ROLLBACK_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;

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
    /// `<name>.exe.download`: on the same volume, so the swap is a rename.
    pub staged: PathBuf,
    /// `<name>.old.exe`: the replaced version until it has exited.
    pub old: PathBuf,
    /// `<name>.exe.receipt.json`: the target build and its health.
    pub receipt: PathBuf,
}

impl Paths {
    pub fn for_exe(exe: &Path) -> Option<Self> {
        let directory = exe.parent()?;
        let file_name = exe.file_name()?;
        let stem = exe.file_stem()?;
        Some(Self {
            exe: exe.to_owned(),
            staged: directory.join(concat(file_name, ".download")),
            old: directory.join(concat(stem, ".old.exe")),
            receipt: directory.join(concat(file_name, ".receipt.json")),
        })
    }

    pub fn current() -> Option<Self> {
        Self::for_exe(&std::env::current_exe().ok()?)
    }
}

fn concat(name: &OsStr, suffix: &str) -> OsString {
    let mut name = name.to_owned();
    name.push(suffix);
    name
}

/// Create the download file. Fails early in a read-only folder.
pub fn create_staged(paths: &Paths) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&paths.staged)
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
        if let Err(error) = fs::rename(&paths.staged, &paths.exe) {
            // Do not hide a failed recovery behind the original install error.
            fs::rename(&paths.old, &paths.exe)?;
            original_restored = true;
            return Err(error);
        }
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

/// Remove what an update left behind: the replaced version, once the new build
/// has reported healthy and its rollback window has passed, and a download
/// abandoned long enough ago that no other instance can still be writing it.
/// Only the two derived names are ever deleted.
pub fn clean_up(paths: &Paths, after_update: bool) {
    let _guard = lock_receipts();
    let rollback_expired = read_receipt(&paths.receipt).is_some_and(|receipt| {
        receipt.installed_at_ms > 0
            && receipt.healthy
            && now_ms().saturating_sub(receipt.installed_at_ms) >= ROLLBACK_WINDOW_MS
    });
    if rollback_expired {
        // The image of a process that has just exited can stay locked briefly.
        let attempts = if after_update { 10 } else { 1 };
        for attempt in 1..=attempts {
            match fs::remove_file(&paths.old) {
                Err(error) if error.kind() != io::ErrorKind::NotFound && attempt < attempts => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                _ => break,
            }
        }
    }
    let stale = fs::metadata(&paths.staged)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_DOWNLOAD));
    if stale {
        let _ = fs::remove_file(&paths.staged);
    }
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
        if arg == UPDATED_FROM_FLAG {
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
