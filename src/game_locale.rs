//! Writing the game's language into Riot's product settings file.
//!
//! The Riot Client builds the game's `--locale=` argument from
//! `<product>.live.product_settings.yaml`, so that file — not the local API —
//! is the value the game actually starts with. Two details make a single write
//! unreliable:
//!
//! - The client rewrites the file itself (on launch and after updates), so a
//!   value written while it is running can be replaced. The caller therefore
//!   writes, launches, and checks again afterwards instead of writing once.
//! - The file is often read-only, which a plain write fails on. The attribute
//!   is cleared, the file rewritten, and the attribute restored — the same
//!   thing LOLauncher does with `chmod`.
//!
//! The requested `settings.locale` is also added to `available_locales` when
//! missing, so the launcher accepts `zh_CN`. This changes the local settings
//! file only; it does not change Riot's published registry or prevent rewrites.

use std::path::{Path, PathBuf};

/// Where Riot keeps per-product metadata.
fn metadata_root() -> Option<PathBuf> {
    let program_data = std::env::var_os("ProgramData").map(PathBuf::from)?;
    Some(program_data.join(r"Riot Games\Metadata"))
}

/// The settings file for a product's live patchline.
pub fn settings_file(product: &str) -> Option<PathBuf> {
    let path = metadata_root()?
        .join(format!("{product}.live"))
        .join(format!("{product}.live.product_settings.yaml"));
    path.exists().then_some(path)
}

/// The locale currently written in the file, if it can be read.
pub fn read(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    locale_line(&contents).map(|(_, value)| value)
}

/// Make `locale` the game's language, and let the registry accept it.
///
/// Two things have to change, and the order matters:
///
/// 1. `locale_data.available_locales` is the list the Riot Client validates a
///    product locale against. `zh_CN` is missing from it even though the game
///    ships `Global.zh_CN.wad.client`, and a locale outside the list is not an
///    error the client reports — it silently rewrites `settings.locale` back to
///    `default_locale` (here `zh_TW`) the moment it starts the product. That is
///    why writing only `settings.locale` looked like it worked and then reverted.
/// 2. `settings.locale` then names the value to use.
///
/// Returns `true` only when the file reads back as the requested locale *and*
/// lists it as available, since a value the client would reject is not applied.
///
/// Both edits are line replacements: the client writes its own ordering and
/// quoting, and a YAML round-trip would churn every line of the file.
pub fn write(path: &Path, locale: &str) -> bool {
    // Validate before reading or writing: callers outside the editor must not
    // be able to put an unknown or quoted value into the client's settings.
    if locale.chars().any(char::is_control) || !crate::locales::is_known(locale) {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some((line, current)) = locale_line(&contents) else {
        return false;
    };
    let mut lines: Vec<String> = contents.lines().map(str::to_owned).collect();
    let mut changed = false;
    // The locale line is edited first: inserting into the list above it would
    // shift its index, and `line` was recorded before any change.
    if current != locale {
        // The line is `locale: "xx_YY"`; rebuild only the quoted value. A
        // substring replace of `{current}"` matched every quote when the
        // current value was empty and left the YAML malformed.
        let Some(key_end) = lines[line].find("locale:") else {
            return false;
        };
        let prefix = &lines[line][..key_end + "locale:".len()];
        lines[line] = format!("{prefix} \"{locale}\"");
        changed = true;
    }
    if !allows(&lines, locale) {
        let Some(index) = insertion_point(&lines, locale) else {
            return false;
        };
        lines.insert(index, format!("    - \"{locale}\""));
        changed = true;
    }
    if changed {
        // `lines()` drops `\r`; write the file back with its own line ending.
        let newline = if contents.contains("\r\n") { "\r\n" } else { "\n" };
        let mut text = lines.join(newline);
        if contents.ends_with('\n') {
            text.push_str(newline);
        }
        if !write_through_read_only(path, &text) {
            return false;
        }
    }
    read(path).as_deref() == Some(locale) && is_allowed_in_file(path, locale)
}

/// `true` when the file lists `locale` as available.
fn is_allowed_in_file(path: &Path, locale: &str) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|contents| allows(&contents.lines().map(str::to_owned).collect::<Vec<_>>(), locale))
}

/// `true` when `locale` appears in the `available_locales` list.
fn allows(lines: &[String], locale: &str) -> bool {
    let Some(start) = lines.iter().position(|line| line.trim_start() == "available_locales:") else {
        return false;
    };
    lines[start + 1..]
        .iter()
        .take_while(|line| entry_value(line).is_some())
        .any(|line| entry_value(line) == Some(locale))
}

/// Where to insert `locale` to keep the list in its existing order.
fn insertion_point(lines: &[String], locale: &str) -> Option<usize> {
    let start = lines.iter().position(|line| line.trim_start() == "available_locales:")?;
    for (offset, line) in lines[start + 1..].iter().enumerate() {
        match entry_value(line) {
            Some(value) if value > locale => return Some(start + 1 + offset),
            Some(_) => {}
            None => return Some(start + 1 + offset),
        }
    }
    None
}

/// The quoted value of a `    - "xx_YY"` list entry.
fn entry_value(line: &str) -> Option<&str> {
    let value = line.trim().strip_prefix("- ")?;
    Some(value.trim().trim_matches('"'))
}

/// Write the file, restoring the read-only attribute if it was set.
///
/// Riot sometimes leaves this file read-only so an update cannot change it;
/// clearing the attribute, writing and putting it back keeps that intent.
/// The attribute is *not* a way to protect the value from the running client —
/// it clears the attribute itself when it rewrites the file, which is why the
/// locale has to be accepted by `available_locales` instead.
fn write_through_read_only(path: &Path, text: &str) -> bool {
    let was_read_only = std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly());
    if was_read_only {
        set_read_only(path, false);
    }
    let written = std::fs::write(path, text).is_ok();
    if was_read_only {
        set_read_only(path, true);
    }
    written
}

/// Toggle the read-only file attribute.
///
/// `Permissions::set_readonly(false)` would work on Windows but is deprecated
/// there for a reason: on Unix it clears every write bit, and the pair of calls
/// here would turn a file's mode into something the user did not choose. Riot
/// only runs on Windows, so the attribute is set directly.
#[cfg(windows)]
fn set_read_only(path: &Path, read_only: bool) {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    let Ok(metadata) = std::fs::metadata(path) else { return };
    let attributes = metadata.file_attributes();
    let wanted = if read_only {
        attributes | FILE_ATTRIBUTE_READONLY
    } else {
        attributes & !FILE_ATTRIBUTE_READONLY
    };
    if wanted == attributes {
        return;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
    unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), wanted);
    }
}

#[cfg(not(windows))]
fn set_read_only(path: &Path, read_only: bool) {
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(read_only);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

/// Line index and value of `settings.locale`.
///
/// The file groups `locale` under `settings:`; `default_locale` and
/// `available_locales` under `locale_data:` come first, so the search starts at
/// the `settings:` block rather than matching the first `locale` it sees.
fn locale_line(contents: &str) -> Option<(usize, String)> {
    let start = contents.lines().position(|line| line.starts_with("settings:"))?;
    for (index, line) in contents.lines().enumerate().skip(start + 1) {
        // A top-level key ends the block.
        if !line.starts_with(' ') && !line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim_start();
        let Some(value) = trimmed.strip_prefix("locale:") else {
            continue;
        };
        let value = value.trim();
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        return Some((index, value.to_owned()));
    }
    None
}
