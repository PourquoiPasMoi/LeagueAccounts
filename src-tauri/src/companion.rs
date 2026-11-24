//! Commands of the Overview page (all accounts at a glance) and of the
//! automatic, passphrase-encrypted accounts backup.

use crate::refresh::Shared;
use crate::views::{fail, fail_with, CommandResult};
use leagueaccounts::backup;
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::overview::{self, Overview};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

type AppState<'a> = State<'a, Arc<Shared>>;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|_| fail("operation_failed"))?
}

/// `day_start`, `week_start`, `pool_since`: Unix ms (the page knows local time).
#[tauri::command]
pub async fn accounts_overview(state: AppState<'_>, day_start: u64, week_start: u64, pool_since: u64) -> CommandResult<Overview> {
    let state = Arc::clone(&state);
    blocking(move || {
        let accounts = state.manager.lock().map_err(|_| fail("operation_failed"))?.accounts.clone();
        Ok(overview::build(&accounts, day_start, week_start, pool_since, now_ms()))
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupView {
    enabled: bool,
    folder: String,
    every_days: u32,
    keep: u32,
    last_at: u64,
    /// A passphrase was chosen (it is not kept; only its key is, in Windows Credential Manager).
    has_passphrase: bool,
    last_error: String,
    /// Backup files in the folder now.
    files: usize,
}

fn backup_view(prefs: backup::Prefs) -> BackupView {
    let files = if prefs.folder.is_empty() { 0 } else { backup::list(std::path::Path::new(&prefs.folder)).len() };
    BackupView {
        enabled: prefs.enabled,
        has_passphrase: !prefs.salt.is_empty(),
        folder: prefs.folder,
        every_days: prefs.every_days,
        keep: prefs.keep,
        last_at: prefs.last_at,
        last_error: prefs.last_error,
        files,
    }
}

#[tauri::command]
pub async fn backup_prefs() -> CommandResult<BackupView> {
    blocking(|| Ok(backup_view(backup::prefs()))).await
}

#[tauri::command]
pub async fn set_backup_prefs(enabled: bool, folder: String, every_days: u32, keep: u32) -> CommandResult<BackupView> {
    blocking(move || {
        let mut prefs = backup::prefs();
        prefs.enabled = enabled;
        prefs.folder = folder;
        prefs.every_days = every_days;
        prefs.keep = keep;
        backup::save_prefs(&prefs).map_err(|error| fail_with("file_write", error))?;
        Ok(backup_view(backup::prefs()))
    })
    .await
}

#[tauri::command]
pub async fn set_backup_passphrase(passphrase: String) -> CommandResult<BackupView> {
    if passphrase.chars().count() < 8 {
        return Err(fail("backup_passphrase_short"));
    }
    blocking(move || backup::set_passphrase(&passphrase).map(backup_view).map_err(|error| fail_with("backup_failed", error))).await
}

#[tauri::command]
pub async fn pick_backup_folder(window: tauri::WebviewWindow, title: String) -> CommandResult<Option<String>> {
    crate::views::file_dialog(window, move |dialog| dialog.set_title(title).pick_folder().map(|path| path.display().to_string())).await
}

/// Write a backup now; returns the file.
#[tauri::command]
pub async fn backup_now(state: AppState<'_>) -> CommandResult<String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let json = state.manager.lock().map_err(|_| fail("operation_failed"))?.export_accounts().map_err(|error| {
            logging::record(Event::ExportFailed, Reason::from_error(error.as_ref()));
            fail_with("export_failed", error)
        })?;
        backup::write(&json).map(|path| path.display().to_string()).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => fail("backup_not_set"),
            _ => fail_with("backup_failed", error),
        })
    })
    .await
}

#[derive(Serialize)]
pub struct Restored {
    added: usize,
    skipped: usize,
}

/// Read a backup file with its passphrase and add its accounts (the ones
/// already in the app are skipped, as with Import).
#[tauri::command]
pub async fn restore_backup(state: AppState<'_>, path: String, passphrase: String) -> CommandResult<Restored> {
    let state = Arc::clone(&state);
    blocking(move || {
        let file = PathBuf::from(&path);
        if !file.extension().is_some_and(|extension| extension.eq_ignore_ascii_case(backup::EXTENSION)) {
            return Err(fail("import_failed"));
        }
        let contents = std::fs::read(&file).map_err(|error| fail_with("file_read", error))?;
        let plain = backup::open(&contents, &passphrase).map_err(|error| match error.kind() {
            std::io::ErrorKind::InvalidInput => fail("backup_wrong_passphrase"),
            _ => fail_with("import_failed", error),
        })?;
        let json = String::from_utf8(plain).map_err(|_| fail("import_failed"))?;
        let (added, skipped) = state.manager.lock().map_err(|_| fail("operation_failed"))?.import_accounts(&json).map_err(|error| {
            logging::record(Event::ImportFailed, Reason::from_error(error.as_ref()));
            fail_with("import_failed", error)
        })?;
        Ok(Restored { added, skipped })
    })
    .await
}
