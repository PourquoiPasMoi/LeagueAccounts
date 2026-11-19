//! Music player commands (Settings › Music, off by default), the `lamusic`
//! scheme for cover pictures (`http://lamusic.localhost/thumb/<id>/<hi|lo>`)
//! and the sound wave meter. Work is in `leagueaccounts::music`; the sound
//! itself plays in the page (YouTube's embedded player).

use crate::views::{fail, CommandResult};
use leagueaccounts::music::{self, CacheInfo, Library, Prefs, Track};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

async fn run<T: Send + 'static>(work: impl FnOnce() -> Result<T, &'static str> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|_| fail("operation_failed"))?.map_err(fail)
}

/// Everything but the settings needs the feature turned on.
fn enabled() -> Result<(), &'static str> {
    if music::prefs().enabled {
        Ok(())
    } else {
        Err("music_off")
    }
}

#[tauri::command]
pub async fn music_prefs() -> CommandResult<Prefs> {
    run(|| Ok(music::prefs())).await
}

#[tauri::command]
pub async fn set_music_prefs(app: AppHandle, prefs: Prefs) -> CommandResult<Prefs> {
    let saved = run(move || music::set_prefs(prefs)).await?;
    if !saved.enabled {
        set_wave(&app, false);
    }
    crate::refresh_tray(&app);
    Ok(saved)
}

/// The length of a song, learned by the player, saved where it was unknown.
#[tauri::command]
pub async fn music_set_duration(id: String, seconds: u32) -> CommandResult<()> {
    run(move || {
        music::remember_duration(&id, seconds);
        Ok(())
    })
    .await
}

/// The running wave meter's stop flag, and the windows that asked for it
/// (the music page "main", the tray player "tray-player").
static WAVE: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
static WAVE_USERS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Start or stop the sound wave meter (`music-wave` events, 48 levels ~30/s)
/// for the calling window. It runs while at least one window wants it.
#[tauri::command]
pub fn music_wave(app: AppHandle, window: tauri::Window, on: bool) {
    wave_user(&app, window.label(), on && music::prefs().enabled);
}

fn wave_user(app: &AppHandle, label: &str, on: bool) {
    let any = {
        let mut users = WAVE_USERS.lock().unwrap_or_else(|poison| poison.into_inner());
        let users = users.get_or_insert_with(HashSet::new);
        if on {
            users.insert(label.to_owned());
        } else {
            users.remove(label);
        }
        !users.is_empty()
    };
    set_wave(app, any);
}

pub fn set_wave(app: &AppHandle, on: bool) {
    let mut running = WAVE.lock().unwrap_or_else(|poison| poison.into_inner());
    if !on {
        if let Some(stop) = running.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if !music::prefs().enabled {
            if let Some(users) = WAVE_USERS.lock().unwrap_or_else(|poison| poison.into_inner()).as_mut() {
                users.clear();
            }
        }
        return;
    }
    if running.is_some() {
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    *running = Some(Arc::clone(&stop));
    let app = app.clone();
    std::thread::spawn(move || {
        let flag = Arc::clone(&stop);
        leagueaccounts::music_wave::run(stop, |levels| {
            let _ = app.emit("music-wave", levels);
        });
        // Ended by itself (no sound device): forget it so it can start again.
        let mut running = WAVE.lock().unwrap_or_else(|poison| poison.into_inner());
        if running.as_ref().is_some_and(|current| Arc::ptr_eq(current, &flag)) {
            *running = None;
        }
    });
}

// ---------------------------------------------------------------- tray player

/// The song in the player, told by the page (None = nothing loaded).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Seconds into the song when `at` (Unix ms) was set.
    pub time: f64,
    pub duration: f64,
    /// 0-100 (0 when muted): the tray wave lifts quiet sound back up.
    pub volume: f64,
    #[serde(default)]
    pub at: u64,
}

static NOW: Mutex<Option<NowPlaying>> = Mutex::new(None);
/// When the tray player last closed (a click on the tray icon closes it by
/// taking the focus; that same click must not open it again).
static TRAY_CLOSED: AtomicU64 = AtomicU64::new(0);
pub const TRAY_PLAYER: &str = "tray-player";
const TRAY_SIZE: (f64, f64) = (340.0, 212.0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis() as u64)
}

/// A song is loaded: right-click on the tray opens the tray player instead of the menu.
pub fn has_song() -> bool {
    music::prefs().enabled && NOW.lock().map(|now| now.is_some()).unwrap_or(false)
}

#[tauri::command]
pub fn music_now_playing(app: AppHandle, now: Option<NowPlaying>) {
    let had = has_song();
    let now = now.map(|mut now| {
        now.at = now_ms();
        now
    });
    *NOW.lock().unwrap_or_else(|poison| poison.into_inner()) = now.clone();
    if had != has_song() {
        crate::refresh_tray(&app);
    }
    if app.get_webview_window(TRAY_PLAYER).is_some() {
        let _ = app.emit_to(TRAY_PLAYER, "music-now", now);
    }
}

#[tauri::command]
pub fn music_now() -> Option<NowPlaying> {
    NOW.lock().ok().and_then(|now| now.clone())
}

/// Buttons of the tray player.
#[tauri::command]
pub fn music_tray(app: AppHandle, action: String) {
    match action.as_str() {
        "toggle" | "next" | "previous" => {
            let _ = app.emit_to("main", "music-control", action);
        }
        "open" => {
            close_tray_player(&app);
            crate::show_main(&app);
        }
        "quit" => crate::quit(&app),
        _ => close_tray_player(&app),
    }
}

fn close_tray_player(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(TRAY_PLAYER) {
        let _ = window.close();
    }
}

/// Right-click on the tray icon while a song is loaded: a small player above
/// the cursor. It is made now and destroyed when it loses focus, so it costs
/// nothing the rest of the time.
pub fn open_tray_player(app: &AppHandle, x: f64, y: f64) {
    if app.get_webview_window(TRAY_PLAYER).is_some() {
        close_tray_player(app);
        return;
    }
    if now_ms().saturating_sub(TRAY_CLOSED.load(Ordering::Relaxed)) < 400 {
        return;
    }
    let app = app.clone();
    // Built off the event loop thread (creating a webview there can stall it).
    std::thread::spawn(move || {
        let Ok(window) = tauri::WebviewWindowBuilder::new(&app, TRAY_PLAYER, tauri::WebviewUrl::App("tray.html".into()))
            .title("League Companion")
            .inner_size(TRAY_SIZE.0, TRAY_SIZE.1)
            .resizable(false)
            .decorations(false)
            .skip_taskbar(true)
            .always_on_top(true)
            .visible(false)
            .shadow(true)
            .background_color(tauri::window::Color(22, 22, 24, 255))
            .build()
        else {
            return;
        };
        // Bottom-right corner at the cursor, inside the screen's free area.
        if let Ok(Some(monitor)) = app.monitor_from_point(x, y) {
            let scale = monitor.scale_factor();
            let (width, height) = ((TRAY_SIZE.0 * scale) as i32, (TRAY_SIZE.1 * scale) as i32);
            let area = monitor.work_area();
            let (left, top) = (area.position.x, area.position.y);
            let (right, bottom) = (left + area.size.width as i32, top + area.size.height as i32);
            let px = (x as i32 - width).clamp(left + 8, (right - width - 8).max(left + 8));
            let py = (y as i32 - height - 12).clamp(top + 8, (bottom - height - 8).max(top + 8));
            let _ = window.set_position(tauri::PhysicalPosition::new(px, py));
        }
        crate::round_corners(&window);
        let handle = app.clone();
        let target = window.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::Focused(false) => {
                let _ = target.close();
            }
            tauri::WindowEvent::Destroyed => {
                TRAY_CLOSED.store(now_ms(), Ordering::Relaxed);
                wave_user(&handle, TRAY_PLAYER, false);
            }
            _ => {}
        });
        let _ = window.show();
        let _ = window.set_focus();
    });
}

#[tauri::command]
pub async fn music_library() -> CommandResult<Library> {
    run(|| enabled().map(|()| music::library())).await
}

#[tauri::command]
pub async fn music_create_playlist(name: String) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::create_playlist(&name))).await
}

#[tauri::command]
pub async fn music_rename_playlist(id: String, name: String) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::rename_playlist(&id, &name))).await
}

#[tauri::command]
pub async fn music_delete_playlist(id: String) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::delete_playlist(&id))).await
}

#[tauri::command]
pub async fn music_import_playlist(link: String) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::import_playlist(&link))).await
}

#[tauri::command]
pub async fn music_sync_playlist(id: String) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::sync_playlist(&id))).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Added {
    library: Library,
    added: usize,
}

#[tauri::command]
pub async fn music_add_links(id: String, input: String) -> CommandResult<Added> {
    run(move || enabled().and_then(|()| music::add_links(&id, &input)).map(|(library, added)| Added { library, added })).await
}

#[tauri::command]
pub async fn music_add_tracks(id: String, tracks: Vec<Track>) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::add_tracks(&id, tracks))).await
}

#[tauri::command]
pub async fn music_remove_track(id: String, index: usize) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::remove_track(&id, index))).await
}

#[tauri::command]
pub async fn music_move_track(id: String, from: usize, to: usize) -> CommandResult<Library> {
    run(move || enabled().and_then(|()| music::move_track(&id, from, to))).await
}

#[tauri::command]
pub async fn music_search(query: String) -> CommandResult<Vec<Track>> {
    run(move || enabled().and_then(|()| music::search(&query))).await
}

#[tauri::command]
pub async fn music_cache() -> CommandResult<CacheInfo> {
    run(|| Ok(music::cache_info())).await
}

#[tauri::command]
pub async fn music_clear_cache() -> CommandResult<CacheInfo> {
    run(|| Ok(music::clear_cache())).await
}

/// Answer for `http://lamusic.localhost/...`.
pub fn serve(path: &str) -> tauri::http::Response<Vec<u8>> {
    let empty = |status: u16| {
        tauri::http::Response::builder()
            .status(status)
            .header("Access-Control-Allow-Origin", "*")
            .body(Vec::new())
            .unwrap_or_default()
    };
    if !music::prefs().enabled {
        return empty(404);
    }
    let mut parts = path.trim_start_matches('/').split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("thumb"), Some(id), size) => match music::thumb(id, size.unwrap_or("lo")) {
            Some(bytes) => tauri::http::Response::builder()
                .header("Content-Type", "image/jpeg")
                // The page reads the cover's colours (canvas), so it must be allowed to.
                .header("Access-Control-Allow-Origin", "*")
                .header("Cache-Control", "max-age=604800")
                .body(bytes)
                .unwrap_or_default(),
            None => empty(404),
        },
        _ => empty(404),
    }
}
