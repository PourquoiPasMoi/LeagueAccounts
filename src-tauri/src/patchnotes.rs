//! Commands of the patch notes page (the reader itself is `leagueaccounts::patchnotes`).

use crate::views::{fail, CommandResult};
use leagueaccounts::patchnotes::{self, ListView, Notes, SearchView};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|_| fail("operation_failed"))?
}

#[tauri::command]
pub async fn patch_notes_list(locale: String, refresh: bool) -> CommandResult<ListView> {
    blocking(move || patchnotes::list(&locale, refresh).map_err(fail)).await
}

#[tauri::command]
pub async fn patch_notes(locale: String, slug: String, refresh: bool) -> CommandResult<Notes> {
    blocking(move || patchnotes::notes(&locale, &slug, refresh).map_err(fail)).await
}

#[tauri::command]
pub async fn patch_notes_search(locale: String, query: String, topic: String) -> CommandResult<SearchView> {
    blocking(move || patchnotes::search(&locale, &query, &topic).map_err(fail)).await
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    done: usize,
    total: usize,
    running: bool,
}

/// Download the notes missing on disk in the background (`count` newest patches, 0 = all);
/// `patch-notes-progress` after each one. False when a download is already running.
#[tauri::command]
pub fn patch_notes_download(app: AppHandle, locale: String, count: usize) -> bool {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return false;
    }
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let last = std::sync::Mutex::new((0, 0));
        let _ = patchnotes::download(&locale, count, &CANCEL, |done, total| {
            *last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = (done, total);
            let _ = app.emit("patch-notes-progress", Progress { done, total, running: true });
        });
        let (done, total) = *last.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        DOWNLOADING.store(false, Ordering::SeqCst);
        let _ = app.emit("patch-notes-progress", Progress { done, total, running: false });
    });
    true
}

#[tauri::command]
pub fn patch_notes_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Open a patch's page on Riot's site in the default browser.
#[tauri::command]
pub async fn open_patch_notes(locale: String, slug: String) -> CommandResult<()> {
    blocking(move || {
        let url = patchnotes::page_url(&locale, &slug).ok_or_else(|| fail("browser"))?;
        std::process::Command::new("explorer.exe").arg(url).spawn().map(|_| ()).map_err(|_| fail("browser"))
    })
    .await
}
