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

pub fn is_locked(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
}

pub fn read(path: &Path) -> Result<Vec<SettingsFile>, SettingsError> {
    let text = std::fs::read_to_string(path)?;
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| SettingsError::Unreadable)?;
    let files: Vec<SettingsFile> = serde_json::from_value(json["files"].clone()).map_err(|_| SettingsError::Unreadable)?;
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

/// Unix ms of the newest backup, and how many there are.
pub fn backup_summary() -> (Option<u64>, usize) {
    backups()
        .map(|list| (list.last().map(|(stamp, _)| *stamp), list.len()))
        .unwrap_or((None, 0))
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
    let text = std::fs::read_to_string(path)?;
    let crlf = text.contains("\r\n");
    let mut json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| SettingsError::Unreadable)?;
    let files = json["files"].as_array_mut().ok_or(SettingsError::Unreadable)?;
    for change in changes {
        // Only existing values are changed: League owns the list of settings.
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
    let lines: Vec<String> = text
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
                    return format!("{key}={}", change.value);
                }
            }
            line.to_owned()
        })
        .collect();
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

/// Put the newest backup back, keeping the current lock state.
pub fn restore_latest(path: &Path) -> Result<(), SettingsError> {
    let (_, latest) = backups()?.pop().ok_or(SettingsError::NotFound)?;
    let text = std::fs::read_to_string(&latest)?;
    // Check it is still a settings file before putting it in place.
    let _: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| SettingsError::Unreadable)?;
    let locked = is_locked(path);
    write_unlocked(path, &text, locked)?;
    let _ = std::fs::remove_file(latest);
    Ok(())
}
