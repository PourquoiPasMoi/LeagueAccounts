//! `hosts` patching for Vietnamese accounts.
//!
//! VN sign-in requires Riot's "update your profile" flow, which calls the
//! region's identity provider. With that provider unreachable, Riot retries
//! internally, exhausts its limit and continues anyway — the login takes
//! longer but completes. Blocking the two provider domains in `hosts` makes
//! the retries fail instantly instead of hanging on a connection, and marks
//! the flow as unavailable so the client skips it.
//!
//! The patch is idempotent: existing lines are left alone, missing ones are
//! appended with a marker comment, and `restore` removes only the marked
//! lines. Admin rights are required to write `hosts`; the caller surfaces the
//! error instead of silently skipping, because without the patch the skip flag
//! does nothing.

use crate::logging::{self, Event, Reason};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The two VN identity-provider endpoints Riot blocks the login on.
pub const VN_HOSTS: &[&str] = &["login.pp.m.zing.vn", "vn-api.vnggames.app"];

/// Marker so `restore` removes only the lines this tool added.
const MARKER: &str = "# LeagueAccounts VN skip";

fn hosts_path() -> PathBuf {
    PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
}

fn entries() -> Vec<String> {
    VN_HOSTS
        .iter()
        .map(|host| format!("127.0.0.1 {host} {MARKER}"))
        .collect()
}

/// Are all VN domains already blocked?
pub fn is_patched() -> bool {
    is_patched_at(&hosts_path())
}

fn is_patched_at(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    VN_HOSTS.iter().all(|host| {
        contents.lines().any(|line| {
            line.trim_start().starts_with("127.0.0.1")
                && line.split_whitespace().any(|token| token == *host)
        })
    })
}

/// Append the block entries. Idempotent; admin rights required.
pub fn patch() -> std::io::Result<()> {
    patch_at(&hosts_path())
}

fn patch_at(path: &Path) -> std::io::Result<()> {
    if is_patched_at(path) {
        return Ok(());
    }
    let existing = std::fs::read_to_string(path)?;
    let newline = line_ending(&existing);
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    // Only terminate an unterminated last line; an unconditional newline left
    // a blank line behind after every patch/restore cycle.
    if !existing.is_empty() && !existing.ends_with('\n') {
        write!(file, "{newline}")?;
    }
    for entry in entries() {
        write!(file, "{entry}{newline}")?;
    }
    file.flush()?;
    logging::record(Event::HostsPatched, Reason::None);
    Ok(())
}

/// Remove the marked lines. Leaves every other line untouched.
pub fn restore() -> std::io::Result<()> {
    restore_at(&hosts_path())
}

fn restore_at(path: &Path) -> std::io::Result<()> {
    let contents = std::fs::read_to_string(path)?;
    if !contents.contains(MARKER) {
        return Ok(());
    }
    let newline = line_ending(&contents);
    let kept: Vec<&str> = contents
        .lines()
        .filter(|line| !line.contains(MARKER))
        .collect();
    let mut text = kept.join(newline);
    if !text.is_empty() && contents.ends_with('\n') {
        text.push_str(newline);
    }
    std::fs::write(path, text)?;
    logging::record(Event::HostsRestored, Reason::None);
    Ok(())
}

/// Remove entries a previous run left behind when it was killed or crashed
/// mid-login, before its guard could restore them. Call once at startup.
pub fn restore_leftovers() {
    let _ = restore();
}

/// The file's own line ending, so a rewrite does not convert CRLF to LF.
fn line_ending(contents: &str) -> &'static str {
    if contents.contains("\r\n") || contents.is_empty() {
        "\r\n"
    } else {
        "\n"
    }
}

/// Patch on creation, restore on drop. `enabled == false` makes it a no-op, so
/// the caller can pass the account flag directly.
pub struct Guard {
    enabled: bool,
}

impl Guard {
    /// Fails when `hosts` cannot be written (usually: not elevated). The caller
    /// reports it, since the skip flag silently does nothing without the patch.
    pub fn new(enabled: bool) -> std::io::Result<Self> {
        if enabled {
            if let Err(error) = patch() {
                logging::record(Event::LoginWaiting, Reason::HostsPatchFailed);
                // A partial append must not outlive the failed login.
                let _ = restore();
                return Err(error);
            }
        }
        Ok(Self { enabled })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = restore();
        }
    }
}
