//! League's shared game settings (`Config/PersistedSettings.json`).
//!
//! The League client loads every interface, camera, sound and keybind setting
//! from this file and normally replaces it with the account's cloud copy at
//! sign-in. Players who share one set of settings across accounts make the file
//! read-only so the client cannot replace it. This module edits that file while
//! keeping the lock: it backs the file up, clears the attribute, writes the
//! changes (and mirrors them into `game.cfg` / `input.ini`), then locks it again.

use crate::utils::app_data_dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_NAME: &str = "PersistedSettings.json";
/// Backups kept in `%APPDATA%\LeagueAccounts\settings-backups` (newest kept).
const BACKUP_LIMIT: usize = 20;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub name: String,
    pub settings: Vec<Setting>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsFile {
    /// `Game.cfg` or `Input.ini`.
    pub name: String,
    pub sections: Vec<Section>,
}

/// One edited value.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub file: String,
    pub section: String,
    pub name: String,
    pub value: String,
}

#[derive(Debug)]
pub enum SettingsError {
    /// No League install with a settings file was found.
    NotFound,
    /// The file is not the JSON layout League writes.
    Unreadable,
    Io(std::io::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("League settings file not found"),
            Self::Unreadable => formatter.write_str("League settings file could not be read"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<std::io::Error> for SettingsError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// League's `Config` folder, from Riot's install metadata, else the default path.
pub fn config_dir() -> Option<PathBuf> {
    let from_metadata = crate::game_locale::settings_file("league_of_legends")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix("product_install_full_path:")?;
                Some(PathBuf::from(value.trim().trim_matches('"').trim_matches('\'')))
            })
        });
    from_metadata
        .into_iter()
        .chain([PathBuf::from(r"C:\Riot Games\League of Legends")])
        .map(|install| install.join("Config"))
        .find(|config| config.join(FILE_NAME).exists())
}

pub fn settings_path() -> Option<PathBuf> {
    config_dir().map(|config| config.join(FILE_NAME))
}

/// Display and graphics settings League keeps only in this PC's `game.cfg`
/// (not in the shared PersistedSettings.json, so the client and the cloud never
/// carry them). Shown in the editor as their own file under this name.
pub const LOCAL_FILE: &str = "game.cfg";
const LOCAL_KEYS: &[(&str, &str)] = &[
    ("General", "WindowMode"),
    ("General", "Width"),
    ("General", "Height"),
    ("General", "WaitForVerticalSync"),
    ("General", "HideEyeCandy"),
    ("General", "ShowGodray"),
    ("Performance", "CharacterQuality"),
    ("Performance", "EnvironmentQuality"),
    ("Performance", "EffectsQuality"),
    ("Performance", "ShadowQuality"),
    ("Performance", "FrameCapType"),
    ("Performance", "EnableFXAA"),
    ("Performance", "AutoPerformanceSettings"),
    // Game and interface options League keeps only in game.cfg too.
    ("General", "EnableCustomAnnouncer"),
    ("General", "EnableGlobalSpellCastNotifications"),
    ("General", "AlwaysShowExtendedTooltip"),
    ("General", "ClampCastTargetLocationWithinMaxRange"),
    ("General", "EnableChampionSpellPreview"),
    ("General", "LearnSpellOnCast"),
    ("General", "MinimizeCameraMotion"),
    ("General", "ShowCursorLocator"),
    ("HUD", "ShowLoadingScreenBackground"),
    ("Voice", "InputVolume"),
    ("Voice", "ActivationSensitivity"),
    ("Voice", "MuteOnConnect"),
    ("Voice", "AutoJoinParty"),
    ("Voice", "AutoJoinTeam"),
    ("MapSkinOptions", "MapSkinOptionDisableSnowdown"),
    ("MapSkinOptions", "MapSkinOptionDisableLunarRevel"),
    ("MapSkinOptions", "MapSkinOptionDisablePoolParty"),
    ("MapSkinOptions", "MapSkinOptionDisableStarGuardian"),
    ("MapSkinOptions", "MapSkinOptionDisableOdyssey"),
    ("MapSkinOptions", "MapSkinOptionDisableProject"),
    ("MapSkinOptions", "MapSkinOptionDisablePopstar"),
    ("MapSkinOptions", "MapSkinOptionDisableArcade"),
    ("MapSkinOptions", "MapSkinOptionDisableURF"),
    ("MapSkinOptions", "MapSkinOptionDisableWorlds"),
    ("MapSkinOptions", "MapSkinOptionDisableMSI"),
];
const LOCAL_BACKUP_LIMIT: usize = 10;

/// `game.cfg` next to the shared settings file.
pub fn local_path(settings: &Path) -> Option<PathBuf> {
    settings.parent().map(|config| config.join("game.cfg"))
}

/// The display settings found in `game.cfg` (first value of each key).
pub fn read_local(path: &Path) -> Option<SettingsFile> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut current = String::new();
    let mut sections: Vec<Section> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current = trimmed[1..trimmed.len() - 1].to_owned();
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else { continue };
        let name = name.trim();
        if !LOCAL_KEYS.contains(&(current.as_str(), name)) {
            continue;
        }
        if !sections.iter().any(|section| section.name == current) {
            sections.push(Section { name: current.clone(), settings: Vec::new() });
        }
        if let Some(section) = sections.iter_mut().find(|section| section.name == current) {
            if !section.settings.iter().any(|setting| setting.name == name) {
                section.settings.push(Setting { name: name.to_owned(), value: value.trim().to_owned() });
            }
        }
    }
    Some(SettingsFile { name: LOCAL_FILE.into(), sections })
}

/// Write display settings into `game.cfg`: existing keys replaced, missing ones
/// added to their section. A copy is kept first; the lock (read-only) stays as it was.
fn write_local(path: &Path, changes: &[&Change]) -> Result<(), SettingsError> {
    let text = std::fs::read_to_string(path)?;
    if let Ok(directory) = backup_dir() {
        let backup = directory.join(format!("game-{}.cfg", now_ms()));
        if std::fs::copy(path, &backup).is_ok() {
            crate::game_locale::set_read_only(&backup, false);
        }
        if let Ok(entries) = std::fs::read_dir(&directory) {
            let mut old: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|file| {
                file.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("game-") && name.ends_with(".cfg"))
            }).collect();
            old.sort();
            while old.len() > LOCAL_BACKUP_LIMIT {
                let _ = std::fs::remove_file(old.remove(0));
            }
        }
    }
    let mut wanted: Vec<(String, String, String)> = changes.iter().map(|change| (change.section.clone(), change.name.clone(), change.value.clone())).collect();
    // A resolution chosen by hand: League keeps it only with this flag.
    if wanted.iter().any(|(section, name, _)| section == "General" && (name == "Width" || name == "Height")) && !wanted.iter().any(|(_, name, _)| name == "UserSetResolution") {
        wanted.push(("General".into(), "UserSetResolution".into(), "1".into()));
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut section = String::new();
    let mut done: Vec<usize> = Vec::new();
    for line in lines.iter_mut() {
        let trimmed = line.trim().to_owned();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].to_owned();
            continue;
        }
        let Some((name, _)) = trimmed.split_once('=') else { continue };
        if let Some(index) = wanted.iter().position(|(want_section, want_name, _)| *want_section == section && want_name == name.trim()) {
            if !done.contains(&index) {
                *line = format!("{}={}", name.trim(), wanted[index].2);
                done.push(index);
            }
        }
    }
    for (index, (want_section, name, value)) in wanted.iter().enumerate() {
        if done.contains(&index) {
            continue;
        }
        let header = format!("[{want_section}]");
        match lines.iter().position(|line| line.trim() == header) {
            Some(start) => {
                let end = lines[start + 1..].iter().position(|line| line.trim().starts_with('[')).map_or(lines.len(), |offset| start + 1 + offset);
                let mut at = end;
                while at > start + 1 && lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                lines.insert(at, format!("{name}={value}"));
            }
            None => {
                lines.push(header);
                lines.push(format!("{name}={value}"));
            }
        }
    }
    let mut output = lines.join(newline);
    if text.ends_with('\n') {
        output.push_str(newline);
    }
    let locked = is_locked(path);
    write_unlocked(path, &output, locked)
}

/// Screen resolutions the main display offers (largest first), for the editor.
pub fn display_resolutions() -> Vec<(u32, u32)> {
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_DISPLAY_SETTINGS_MODE};
    let mut found: Vec<(u32, u32)> = Vec::new();
    for index in 0..1000u32 {
        let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !unsafe { EnumDisplaySettingsW(windows::core::PCWSTR::null(), ENUM_DISPLAY_SETTINGS_MODE(index), &mut mode) }.as_bool() {
            break;
        }
        let size = (mode.dmPelsWidth, mode.dmPelsHeight);
        if size.0 >= 800 && size.1 >= 600 && !found.contains(&size) {
            found.push(size);
        }
    }
    found.sort_by_key(|(width, height)| std::cmp::Reverse(u64::from(*width) * u64::from(*height)));
    found
}

pub fn is_locked(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
}

pub fn read(path: &Path) -> Result<Vec<SettingsFile>, SettingsError> {
    let text = std::fs::read_to_string(path)?;
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| SettingsError::Unreadable)?;
    let mut files: Vec<SettingsFile> = serde_json::from_value(json["files"].clone()).map_err(|_| SettingsError::Unreadable)?;
    // The game's input.ini lists every hotkey (bound or not); the persisted
    // settings only those set from the options menu. Show both.
    if let Some(ini) = path.parent().and_then(|config| std::fs::read_to_string(config.join("input.ini")).ok()) {
        let mut section = String::new();
        let mut extra: Vec<Setting> = Vec::new();
        for line in ini.lines() {
            let line = line.trim();
            if line.starts_with('[') && line.ends_with(']') {
                section = line[1..line.len() - 1].to_owned();
            } else if section == "GameEvents" {
                if let Some((name, value)) = line.split_once('=') {
                    if name.starts_with("ev") {
                        extra.push(Setting { name: name.to_owned(), value: value.to_owned() });
                    }
                }
            }
        }
        if !files.iter().any(|file| file.name == "Input.ini") {
            files.push(SettingsFile { name: "Input.ini".into(), sections: Vec::new() });
        }
        if let Some(input) = files.iter_mut().find(|file| file.name == "Input.ini") {
            if !input.sections.iter().any(|section| section.name == "GameEvents") {
                input.sections.push(Section { name: "GameEvents".into(), settings: Vec::new() });
            }
            if let Some(events) = input.sections.iter_mut().find(|section| section.name == "GameEvents") {
                for setting in extra {
                    if !events.settings.iter().any(|known| known.name == setting.name) {
                        events.settings.push(setting);
                    }
                }
            }
        }
    }
    if let Some(local) = local_path(path).and_then(|file| read_local(&file)) {
        if !local.sections.is_empty() {
            files.push(local);
        }
    }
    Ok(files)
}

/// Set or clear the read-only attribute (the lock).
pub fn set_locked(path: &Path, locked: bool) -> Result<(), SettingsError> {
    crate::game_locale::set_read_only(path, locked);
    if is_locked(path) == locked {
        Ok(())
    } else {
        Err(SettingsError::Io(std::io::Error::other("could not change the read-only attribute")))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn backup_dir() -> std::io::Result<PathBuf> {
    let directory = app_data_dir()?.join("settings-backups");
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

/// Copy the current file into the backups folder, keeping the newest few.
fn back_up(path: &Path) -> std::io::Result<()> {
    let directory = backup_dir()?;
    let backup = directory.join(format!("PersistedSettings-{}.json", now_ms()));
    std::fs::copy(path, &backup)?;
    // The copy keeps the read-only attribute, which would block pruning it later.
    crate::game_locale::set_read_only(&backup, false);
    let mut backups = backups()?;
    while backups.len() > BACKUP_LIMIT {
        let oldest = backups.remove(0);
        let _ = std::fs::remove_file(oldest.1);
    }
    Ok(())
}

/// Backups as (unix ms, path), oldest first.
fn backups() -> std::io::Result<Vec<(u64, PathBuf)>> {
    let mut list: Vec<(u64, PathBuf)> = std::fs::read_dir(backup_dir()?)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stamp = name.strip_prefix("PersistedSettings-")?.strip_suffix(".json")?.parse().ok()?;
            Some((stamp, entry.path()))
        })
        .collect();
    list.sort();
    Ok(list)
}

/// Backups of `game.cfg` as (unix ms, path), oldest first.
fn local_backups() -> Vec<(u64, PathBuf)> {
    let Ok(directory) = backup_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(directory) else { return Vec::new() };
    let mut list: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stamp = name.strip_prefix("game-")?.strip_suffix(".cfg")?.parse().ok()?;
            Some((stamp, entry.path()))
        })
        .collect();
    list.sort();
    list
}

/// Unix ms of the newest backup (shared settings or game.cfg), and how many there are.
pub fn backup_summary() -> (Option<u64>, usize) {
    let shared = backups().unwrap_or_default();
    let local = local_backups();
    let newest = shared.last().map(|(stamp, _)| *stamp).max(local.last().map(|(stamp, _)| *stamp));
    (newest, shared.len() + local.len())
}

/// Write text over a file that may be read-only, then set the lock as asked.
fn write_unlocked(path: &Path, text: &str, lock_after: bool) -> Result<(), SettingsError> {
    crate::game_locale::set_read_only(path, false);
    let result = std::fs::write(path, text);
    if lock_after {
        crate::game_locale::set_read_only(path, true);
    }
    result.map_err(SettingsError::Io)
}

/// Apply edits to `PersistedSettings.json` (and mirror them into `game.cfg` /
/// `input.ini`), then leave the file locked when `lock` is set.
pub fn apply(path: &Path, changes: &[Change], lock: bool) -> Result<(), SettingsError> {
    let local: Vec<&Change> = changes.iter().filter(|change| change.file == LOCAL_FILE).collect();
    if !local.is_empty() {
        write_local(&local_path(path).ok_or(SettingsError::NotFound)?, &local)?;
    }
    let shared: Vec<Change> = changes.iter().filter(|change| change.file != LOCAL_FILE).cloned().collect();
    if shared.is_empty() {
        return Ok(());
    }
    let changes = shared.as_slice();
    let text = std::fs::read_to_string(path)?;
    let crlf = text.contains("\r\n");
    let mut json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| SettingsError::Unreadable)?;
    let files = json["files"].as_array_mut().ok_or(SettingsError::Unreadable)?;
    for change in changes {
        // Existing values are changed. The one exception is a hotkey the game
        // has not written yet (never bound, e.g. Fist Bump): it is added to
        // the hotkey section, as the game does when it is bound in game.
        let addable = change.file == "Input.ini" && change.section == "GameEvents" && change.name.starts_with("ev");
        if addable {
            let section = files
                .iter_mut()
                .find(|file| file["name"].as_str() == Some("Input.ini"))
                .and_then(|file| file["sections"].as_array_mut())
                .and_then(|sections| sections.iter_mut().find(|section| section["name"].as_str() == Some("GameEvents")))
                .and_then(|section| section["settings"].as_array_mut());
            if let Some(settings) = section {
                if !settings.iter().any(|setting| setting["name"].as_str() == Some(change.name.as_str())) {
                    settings.push(serde_json::json!({ "name": change.name, "value": change.value }));
                    continue;
                }
            }
        }
        let setting = files
            .iter_mut()
            .find(|file| file["name"].as_str() == Some(change.file.as_str()))
            .and_then(|file| file["sections"].as_array_mut())
            .and_then(|sections| sections.iter_mut().find(|section| section["name"].as_str() == Some(change.section.as_str())))
            .and_then(|section| section["settings"].as_array_mut())
            .and_then(|settings| settings.iter_mut().find(|setting| setting["name"].as_str() == Some(change.name.as_str())));
        if let Some(setting) = setting {
            setting["value"] = Value::String(change.value.clone());
        }
    }
    back_up(path)?;
    let mut output = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, formatter);
    json.serialize(&mut serializer).map_err(|_| SettingsError::Unreadable)?;
    let mut written = String::from_utf8(output).map_err(|_| SettingsError::Unreadable)?;
    if text.ends_with('\n') {
        written.push('\n');
    }
    if crlf {
        written = written.replace('\n', "\r\n");
    }
    write_unlocked(path, &written, lock)?;
    if let Some(config) = path.parent() {
        mirror(&config.join("game.cfg"), "Game.cfg", changes);
        mirror(&config.join("input.ini"), "Input.ini", changes);
    }
    Ok(())
}

/// Replace existing `key=value` lines in an ini file League generates from the
/// persisted settings, so the game sees the change even before it regenerates.
fn mirror(path: &Path, file: &str, changes: &[Change]) {
    let relevant: Vec<&Change> = changes.iter().filter(|change| change.file == file).collect();
    if relevant.is_empty() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut section = String::new();
    let mut changed = false;
    let mut seen: Vec<&str> = Vec::new();
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains('=') {
                section = trimmed[1..trimmed.len() - 1].to_owned();
                return line.to_owned();
            }
            if let Some((key, _)) = line.split_once('=') {
                if let Some(change) = relevant.iter().find(|change| change.section == section && change.name == key) {
                    changed = true;
                    seen.push(change.name.as_str());
                    return format!("{key}={}", change.value);
                }
            }
            line.to_owned()
        })
        .collect();
    // Hotkeys the file does not list yet go at the end of their section.
    for change in relevant.iter().filter(|change| change.section == "GameEvents" && change.name.starts_with("ev") && !seen.contains(&change.name.as_str())) {
        let header = format!("[{}]", change.section);
        let Some(start) = lines.iter().position(|line| line.trim() == header) else { continue };
        let end = lines[start + 1..].iter().position(|line| line.trim().starts_with('[')).map_or(lines.len(), |offset| start + 1 + offset);
        // Before any blank lines that close the section.
        let mut at = end;
        while at > start + 1 && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        lines.insert(at, format!("{}={}", change.name, change.value));
        changed = true;
    }
    if !changed {
        return;
    }
    let mut output = lines.join(newline);
    if text.ends_with('\n') {
        output.push_str(newline);
    }
    let was_locked = is_locked(path);
    let _ = write_unlocked(path, &output, was_locked);
}

/// Undo the last apply: put back the newest backup of the shared settings
/// and/or of `game.cfg` (both when one apply changed both: backups made within
/// a few seconds), keeping each file's lock state.
pub fn restore_latest(path: &Path) -> Result<(), SettingsError> {
    let shared = backups()?.pop();
    let local = local_backups().pop();
    let newest = shared.as_ref().map(|(stamp, _)| *stamp).max(local.as_ref().map(|(stamp, _)| *stamp)).ok_or(SettingsError::NotFound)?;
    let together = |stamp: u64| newest.saturating_sub(stamp) < 5_000;
    if let Some((stamp, latest)) = local.filter(|(stamp, _)| together(*stamp)) {
        let _ = stamp;
        let target = local_path(path).ok_or(SettingsError::NotFound)?;
        let text = std::fs::read_to_string(&latest)?;
        let locked = is_locked(&target);
        write_unlocked(&target, &text, locked)?;
        let _ = std::fs::remove_file(latest);
    }
    if let Some((_, latest)) = shared.filter(|(stamp, _)| together(*stamp)) {
        let text = std::fs::read_to_string(&latest)?;
        // Check it is still a settings file before putting it in place.
        let _: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| SettingsError::Unreadable)?;
        let locked = is_locked(path);
        write_unlocked(path, &text, locked)?;
        let _ = std::fs::remove_file(latest);
    }
    Ok(())
}
