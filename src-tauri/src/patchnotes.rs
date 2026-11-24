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
pub async fn patch_notes_search(locale: String, query: String, topic: String, modes: Vec<String>) -> CommandResult<SearchView> {
    blocking(move || patchnotes::search(&locale, &query, &topic, &modes).map_err(fail)).await
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

// ---------------------------------------------------------------- report images

/// Images made by the patch notes report builder: Pictures\League Companion.
fn reports_folder() -> std::path::PathBuf {
    std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_default().join("Pictures").join("League Companion")
}

/// The largest image the builder may hand over (it aims under 9.7 MB).
const MAX_REPORT_BYTES: usize = 12 * 1024 * 1024;

/// `%E4%B8%AD` → bytes → text (the page sends the file name percent-encoded in a header).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2])) {
                out.push(high * 16 + low);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A file name Windows accepts, from the report's title.
fn file_stem(title: &str) -> String {
    let clean: String = title.chars().map(|letter| if letter.is_control() || "<>:\"/\\|?*".contains(letter) { ' ' } else { letter }).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean: String = clean.trim_matches(|letter: char| letter == '.' || letter == ' ').chars().take(80).collect();
    if clean.is_empty() {
        "Patch notes".into()
    } else {
        clean
    }
}

/// Save a report image (the request's raw body: a PNG or JPEG made by the page; its name in the
/// `x-name` header, percent-encoded) into Pictures\League Companion. Returns the file's path.
#[tauri::command]
pub fn save_report_image(request: tauri::ipc::Request<'_>) -> CommandResult<String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else { return Err(fail("report_save_failed")) };
    let extension = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else {
        return Err(fail("report_save_failed"));
    };
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(fail("report_save_failed"));
    }
    let name = request.headers().get("x-name").and_then(|value| value.to_str().ok()).map(percent_decode).unwrap_or_default();
    let stem = file_stem(&name);
    let folder = reports_folder();
    std::fs::create_dir_all(&folder).map_err(|_| fail("report_save_failed"))?;
    let mut file = folder.join(format!("{stem}.{extension}"));
    let mut number = 2;
    while file.exists() {
        file = folder.join(format!("{stem} ({number}).{extension}"));
        number += 1;
    }
    std::fs::write(&file, bytes).map_err(|_| fail("report_save_failed"))?;
    Ok(file.display().to_string())
}

/// Show a saved report image in Explorer (only files of the reports folder).
#[tauri::command]
pub async fn show_report_image(path: String) -> CommandResult<()> {
    blocking(move || {
        let file = std::path::PathBuf::from(&path);
        let inside = file.parent().is_some_and(|parent| parent == reports_folder()) && file.is_file();
        if !inside {
            return Err(fail("browser"));
        }
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", file.display())).spawn().map(|_| ()).map_err(|_| fail("browser"))
    })
    .await
}
