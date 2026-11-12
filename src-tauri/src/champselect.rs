//! Champ select helper: watches the League client for champ select, tells the
//! interface which champion and lane the player has, and puts the chosen
//! OP.GG build into the client (rune page, summoner spells, item set).
//!
//! Only the client's local API is used, read every two seconds while the
//! client runs; changes are made only for the local player, either when the
//! player presses Apply or, if they turned it on, once their pick is locked.

use crate::views::{fail, CommandResult};
use leagueaccounts::builds::{self, ApplyError, ChampSelect, ChampionBuild, FinalBuild, GameAssets, ItemSetText, Mode, RuneOutcome, Suggestion, Threats};
use leagueaccounts::riot_client;
use leagueaccounts::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Champ select preferences, kept in `%APPDATA%\LeagueAccounts\champselect.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildPrefs {
    /// Experimental features (champ select builds, live game view). Off by default.
    pub experimental: bool,
    /// Bring the window up with the build when champ select starts.
    pub open_on_champ_select: bool,
    /// Apply the best runes, spells and item set when the pick is locked.
    pub auto_apply: bool,
    pub auto_runes: bool,
    pub auto_spells: bool,
    pub auto_items: bool,
}

impl Default for BuildPrefs {
    fn default() -> Self {
        Self { experimental: false, open_on_champ_select: true, auto_apply: false, auto_runes: true, auto_spells: true, auto_items: true }
    }
}

pub(crate) fn save_prefs(prefs: &BuildPrefs) -> bool {
    let (Some(path), Ok(body)) = (prefs_path(), serde_json::to_vec_pretty(prefs)) else { return false };
    std::fs::write(path, body).is_ok()
}

fn prefs_path() -> Option<std::path::PathBuf> {
    app_data_dir().ok().map(|directory| directory.join("champselect.json"))
}

pub(crate) fn load_prefs() -> BuildPrefs {
    prefs_path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

static STATE: Mutex<Option<ChampSelect>> = Mutex::new(None);
/// Whether the experimental features are on (Settings); read by both watchers.
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}
/// The champion picked in the latest champ select (kept after it ends).
static LAST_PICK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Remove other build apps' item sets for the last picked champion, in the background.
pub(crate) fn clean_item_sets() {
    let champion_id = LAST_PICK.load(std::sync::atomic::Ordering::Relaxed);
    if champion_id <= 0 {
        return;
    }
    thread::spawn(move || {
        // Once as the game starts, once more while it loads (other apps may write late).
        for wait in [2, 12] {
            thread::sleep(Duration::from_secs(wait));
            if let Some(client) = riot_client::league_client() {
                builds::remove_other_item_sets(&client, champion_id);
            }
        }
    });
}

/// Poll the client; emit `champ-select` whenever what the player has changes.
pub fn spawn_watcher(app: AppHandle) {
    ENABLED.store(load_prefs().experimental, std::sync::atomic::Ordering::Relaxed);
    thread::spawn(move || {
        let mut last = ChampSelect::default();
        loop {
            if !enabled() {
                // Turned off: nothing is read from the client.
                if last != ChampSelect::default() {
                    last = ChampSelect::default();
                    if let Ok(mut state) = STATE.lock() {
                        *state = Some(last.clone());
                    }
                    let _ = app.emit("champ-select", &last);
                }
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            let client = riot_client::league_client();
            let current = client.as_ref().map(builds::champ_select).unwrap_or_default();
            if current != last {
                if current.active && !last.active && load_prefs().open_on_champ_select {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.unminimize();
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                if current.active && current.champion_id > 0 {
                    LAST_PICK.store(current.champion_id, std::sync::atomic::Ordering::Relaxed);
                }
                if let Ok(mut state) = STATE.lock() {
                    *state = Some(current.clone());
                }
                let _ = app.emit("champ-select", &current);
                last = current;
            }
            thread::sleep(Duration::from_secs(if client.is_some() { 2 } else { 6 }));
        }
    });
}

#[tauri::command]
pub async fn champ_select_state() -> CommandResult<ChampSelect> {
    Ok(STATE.lock().ok().and_then(|state| state.clone()).unwrap_or_default())
}

#[tauri::command]
pub async fn build_prefs() -> CommandResult<BuildPrefs> {
    Ok(load_prefs())
}

#[tauri::command]
pub async fn set_build_prefs(prefs: BuildPrefs) -> CommandResult<BuildPrefs> {
    super::blocking(move || {
        if !save_prefs(&prefs) {
            return Err(fail("settings_save_failed"));
        }
        ENABLED.store(prefs.experimental, std::sync::atomic::Ordering::Relaxed);
        Ok(prefs)
    })
    .await
}

/// Rune, rune tree, item and summoner spell names and icons.
#[tauri::command]
pub async fn build_assets() -> CommandResult<GameAssets> {
    super::blocking(|| {
        let client = riot_client::league_client();
        Ok(builds::game_assets(client.as_ref()))
    })
    .await
}

/// OP.GG's statistics and the single build to follow against these enemies.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildView {
    #[serde(flatten)]
    build: ChampionBuild,
    plan: FinalBuild,
}

#[tauri::command]
pub async fn champion_build(champion_id: i64, mode: String, position: String, threats: Option<Threats>) -> CommandResult<BuildView> {
    super::blocking(move || {
        let build = builds::champion_build(champion_id, Mode::parse(&mode), &position).map_err(|_| fail("build_fetch_failed"))?;
        let client = riot_client::league_client();
        let assets = builds::game_assets(client.as_ref());
        let plan = builds::final_build(&build, &assets.item_info, threats.unwrap_or_default());
        Ok(BuildView { build, plan })
    })
    .await
}

/// The strongest champions in a lane, for before the pick.
#[tauri::command]
pub async fn lane_suggestions(position: String) -> CommandResult<Vec<Suggestion>> {
    super::blocking(move || builds::lane_suggestions(&position).map_err(|_| fail("build_fetch_failed"))).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSetTextInput {
    title: String,
    starter: String,
    core: String,
    situational: String,
    consumables: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRequest {
    champion_id: i64,
    mode: String,
    position: String,
    /// Index into the build's rune pages, or none to leave runes alone.
    rune_page: Option<usize>,
    rune_name: String,
    /// Index into the build's summoner spell pairs.
    spells: Option<usize>,
    /// Item set texts, or none to skip the item set.
    item_set: Option<ItemSetTextInput>,
    #[serde(default)]
    threats: Threats,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    runes: Option<RuneOutcome>,
    spells: bool,
    item_set: bool,
    /// Error codes for the parts that failed.
    errors: Vec<String>,
}

#[tauri::command]
pub async fn apply_build(request: ApplyRequest) -> CommandResult<ApplyResult> {
    if !enabled() {
        return Err(fail("operation_failed"));
    }
    super::blocking(move || {
        let client = riot_client::league_client().ok_or_else(|| fail(ApplyError::NoClient.code()))?;
        let build = builds::champion_build(request.champion_id, Mode::parse(&request.mode), &request.position)
            .map_err(|_| fail("build_fetch_failed"))?;
        let mut result = ApplyResult::default();
        if let Some(page) = request.rune_page.and_then(|index| build.rune_pages.get(index)) {
            match builds::apply_runes(&client, &request.rune_name, page) {
                Ok(outcome) => result.runes = Some(outcome),
                Err(error) => result.errors.push(error.code().to_owned()),
            }
        }
        if let Some(spells) = request.spells.and_then(|index| build.spells.get(index)) {
            match builds::apply_spells(&client, &spells.ids) {
                Ok(()) => result.spells = true,
                Err(error) => result.errors.push(error.code().to_owned()),
            }
        }
        if let Some(text) = request.item_set {
            let blocks = ItemSetText { starter: text.starter, core: text.core, situational: text.situational, consumables: text.consumables };
            let assets = builds::game_assets(Some(&client));
            match builds::apply_item_set(&client, &text.title, &build, &assets.item_info, request.threats, &blocks) {
                Ok(()) => result.item_set = true,
                Err(error) => result.errors.push(error.code().to_owned()),
            }
        }
        Ok(result)
    })
    .await
}
