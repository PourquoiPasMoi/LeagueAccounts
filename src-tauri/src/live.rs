//! The current game for the app's live view. A light watcher checks the game
//! phase every 2 seconds (one local request); when a game starts it reads
//! everyone's rank once and tells the interface (`live-game`), and when it
//! ends it clears that. Live stats are read only when the interface asks
//! (`live_stats`), which it does every few seconds while the view is on
//! screen. Everything stays on this computer.

use crate::views::{fail, CommandResult};
use leagueaccounts::livegame::{self, LiveGame, LiveStats};
use leagueaccounts::riot_client;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

static GAME: Mutex<Option<LiveGame>> = Mutex::new(None);

fn set_game(app: &AppHandle, game: Option<LiveGame>) {
    if let Ok(mut state) = GAME.lock() {
        *state = game.clone();
    }
    let _ = app.emit("live-game", game);
}

fn has_game() -> bool {
    GAME.lock().is_ok_and(|state| state.is_some())
}

pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        let mut in_game = false;
        let mut fetched = Instant::now() - Duration::from_secs(60);
        loop {
            if !crate::champselect::enabled() {
                // Turned off in Settings: no polling at all.
                if in_game || has_game() {
                    in_game = false;
                    set_game(&app, None);
                }
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            let client = riot_client::league_client();
            let phase = client
                .as_ref()
                .and_then(|client| client.get_json("/lol-gameflow/v1/gameflow-phase"))
                .and_then(|phase| phase.as_str().map(str::to_owned))
                .unwrap_or_default();
            let now_in_game = matches!(phase.as_str(), "GameStart" | "InProgress" | "Reconnect");
            if now_in_game && !in_game {
                fetched = Instant::now() - Duration::from_secs(60);
                // Other apps' item sets must not hide this app's (Blitz may write late).
                crate::champselect::clean_item_sets();
            }
            if !now_in_game && in_game {
                set_game(&app, None);
            }
            in_game = now_in_game;
            // Everyone's rank, once per game (retried every 10 s until the client has them).
            if in_game && !has_game() && fetched.elapsed() >= Duration::from_secs(10) {
                fetched = Instant::now();
                if let Some(game) = client.as_ref().and_then(livegame::live_game) {
                    set_game(&app, Some(game));
                }
            }
            thread::sleep(Duration::from_secs(if client.is_some() { 2 } else { 6 }));
        }
    });
}

#[tauri::command]
pub async fn live_game_state() -> CommandResult<Option<LiveGame>> {
    Ok(GAME.lock().ok().and_then(|state| state.clone()))
}

/// Live numbers for everyone (local requests only); `None` before the match starts.
#[tauri::command]
pub async fn live_stats() -> CommandResult<Option<LiveStats>> {
    if !crate::champselect::enabled() {
        return Ok(None);
    }
    tauri::async_runtime::spawn_blocking(livegame::live_stats).await.map_err(|_| fail("operation_failed"))
}
