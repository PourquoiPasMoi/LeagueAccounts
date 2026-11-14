//! The executable was `LeagueAccounts.exe` until v4.6.7 and is `LeagueCompanion.exe`
//! since v4.6.8. An install still under the old name renames itself once, at a
//! normal start (never during an update hand-over or while another copy runs),
//! then points the autostart entry and the user's shortcuts at the new file.
//! Data, passwords and settings do not depend on the file name.

use std::path::{Path, PathBuf};

pub const NEW_NAME: &str = "LeagueCompanion.exe";
pub const OLD_NAME: &str = "LeagueAccounts.exe";

/// Rename `exe` to `LeagueCompanion.exe` beside it when it still has the old
/// default name and nothing else uses it. Returns the new path.
pub fn rename_if_old(exe: &Path) -> Option<PathBuf> {
    let name = exe.file_name()?.to_str()?;
    if !name.eq_ignore_ascii_case(OLD_NAME) {
        return None;
    }
    let target = exe.with_file_name(NEW_NAME);
    if target.exists() {
        return None;
    }
    // An update that is not finished yet keeps its files under the old name.
    let paths = crate::self_update::Paths::for_exe(exe)?;
    if paths.old.exists() || paths.legacy_old.exists() || crate::self_update::update_pending(&paths) {
        return None;
    }
    if other_process_runs(exe) {
        return None;
    }
    std::fs::rename(exe, &target).ok()?;
    Some(target)
}

/// After the renamed copy was started: autostart entry and shortcuts follow it.
pub fn follow_rename(old: &Path, new: &Path) {
    fix_autostart(old, new);
    fix_shortcuts(&shortcut_folders(), old, new);
}

/// Another process started from the same file (the app open in the tray).
#[cfg(windows)]
fn other_process_runs(exe: &Path) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let me = std::process::id();
    let wanted = exe.to_string_lossy().to_lowercase();
    let file_name = exe.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
    // SAFETY: plain ToolHelp walk; every handle opened here is closed.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return true };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]).to_lowercase();
            if entry.th32ProcessID != me && name == file_name {
                // Same name: compare the full path (another folder is another install).
                match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, entry.th32ProcessID) {
                    Ok(process) => {
                        let mut buffer = [0u16; 1024];
                        let mut size = buffer.len() as u32;
                        let same = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buffer.as_mut_ptr()), &mut size).is_ok()
                            && String::from_utf16_lossy(&buffer[..size as usize]).to_lowercase() == wanted;
                        let _ = CloseHandle(process);
                        found |= same;
                    }
                    // Unknown: be careful.
                    Err(_) => found = true,
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

#[cfg(not(windows))]
fn other_process_runs(_exe: &Path) -> bool {
    true
}

/// `data` of a Run entry with the old exe path replaced (case-insensitive), if it had it.
pub fn retarget(data: &str, old: &Path, new: &Path) -> Option<String> {
    let old = old.to_string_lossy();
    let start = data.to_lowercase().find(&old.to_lowercase())?;
    Some(format!("{}{}{}", &data[..start], new.to_string_lossy(), &data[start + old.len()..]))
}

#[cfg(windows)]
fn fix_autostart(old: &Path, new: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let Ok(output) = std::process::Command::new("reg").args(["query", RUN]).creation_flags(CREATE_NO_WINDOW).output() else { return };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((name, data)) = line.trim_start().split_once("    REG_SZ    ") else { continue };
        if let Some(data) = retarget(data.trim_end(), old, new) {
            let _ = std::process::Command::new("reg")
                .args(["add", RUN, "/v", name.trim(), "/t", "REG_SZ", "/d", &data, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
    }
}

#[cfg(not(windows))]
fn fix_autostart(_old: &Path, _new: &Path) {}

/// Desktop, Start menu and pinned taskbar/Start items of this Windows user.
fn shortcut_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        folders.push(appdata.join(r"Microsoft\Windows\Start Menu\Programs"));
        folders.push(appdata.join(r"Microsoft\Internet Explorer\Quick Launch"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        folders.push(profile.join("Desktop"));
        folders.push(profile.join(r"OneDrive\Desktop"));
    }
    if let Some(public) = std::env::var_os("PUBLIC").map(PathBuf::from) {
        folders.push(public.join("Desktop"));
    }
    folders
}

/// Point every `.lnk` in `folders` (and two levels below) that opens `old` at `new`.
/// Returns how many were changed.
#[cfg(windows)]
pub fn fix_shortcuts(folders: &[PathBuf], old: &Path, new: &Path) -> usize {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READWRITE};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};
    fn links(folder: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(folder) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && depth > 0 {
                links(&path, depth - 1, out);
            } else if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("lnk")) {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for folder in folders {
        links(folder, 2, &mut files);
    }
    let old_text = old.to_string_lossy().to_lowercase();
    let mut changed = 0;
    // SAFETY: COM calls on this thread with interfaces that live for each iteration.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        for file in files {
            let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { continue };
            let Ok(persist) = link.cast::<IPersistFile>() else { continue };
            if persist.Load(&HSTRING::from(file.as_os_str()), STGM_READWRITE).is_err() {
                continue;
            }
            let mut buffer = [0u16; 1024];
            if link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).is_err() {
                continue;
            }
            let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            if String::from_utf16_lossy(&buffer[..length]).to_lowercase() != old_text {
                continue;
            }
            let _ = link.SetPath(&HSTRING::from(new.as_os_str()));
            // The icon came from the exe itself: follow it too.
            let mut icon = [0u16; 1024];
            let mut index = 0;
            if link.GetIconLocation(&mut icon, &mut index).is_ok() {
                let length = icon.iter().position(|&c| c == 0).unwrap_or(icon.len());
                if String::from_utf16_lossy(&icon[..length]).to_lowercase() == old_text {
                    let _ = link.SetIconLocation(&HSTRING::from(new.as_os_str()), index);
                }
            }
            if persist.Save(None, true).is_ok() {
                changed += 1;
            }
        }
    }
    changed
}

#[cfg(not(windows))]
pub fn fix_shortcuts(_folders: &[PathBuf], _old: &Path, _new: &Path) -> usize {
    0
}
