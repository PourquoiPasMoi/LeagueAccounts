//! The bookmark hotkey (Settings › Recorder): a normal Windows hotkey
//! (`RegisterHotKey`, as recording apps use), registered only while a game is
//! being recorded, so the key works as usual the rest of the time. Nothing
//! touches the game process.

use std::thread;
use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT};
use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
use windows_sys::Win32::UI::WindowsAndMessaging::{PeekMessageW, MB_OK, MSG, PM_REMOVE, WM_HOTKEY};

const ID: i32 = 0x4C43;

/// "Ctrl+Shift+F8" → (modifiers, virtual key). Keys: F1-F24, A-Z, 0-9,
/// Numpad0-9, Insert, Delete, Home, End, PageUp, PageDown, Pause, ScrollLock.
pub fn parse(text: &str) -> Option<(u32, u32)> {
    let mut modifiers = 0;
    let mut key = None;
    for part in text.split('+').map(str::trim).filter(|part| !part.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            _ => key = Some(virtual_key(part)?),
        }
    }
    key.map(|key| (modifiers, key))
}

fn virtual_key(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    if let Some(number) = upper.strip_prefix('F').and_then(|rest| rest.parse::<u32>().ok()) {
        return (1..=24).contains(&number).then_some(0x70 + number - 1);
    }
    if let Some(number) = upper.strip_prefix("NUMPAD").and_then(|rest| rest.parse::<u32>().ok()) {
        return (number <= 9).then_some(0x60 + number);
    }
    if upper.len() == 1 {
        let byte = upper.as_bytes()[0];
        return (byte.is_ascii_uppercase() || byte.is_ascii_digit()).then_some(u32::from(byte));
    }
    Some(match upper.as_str() {
        "INSERT" => 0x2D,
        "DELETE" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "PAUSE" => 0x13,
        "SCROLLLOCK" => 0x91,
        _ => return None,
    })
}

/// The Windows "OK" sound: the player hears that the bookmark was taken.
pub fn beep() {
    unsafe {
        MessageBeep(MB_OK);
    }
}

/// Run forever on a new thread: `wanted` says which key should be active right
/// now (None = none: no recording), `pressed` runs on each press.
pub fn watch(wanted: impl Fn() -> Option<String> + Send + 'static, pressed: impl Fn() + Send + 'static) {
    thread::Builder::new()
        .name("bookmark-key".into())
        .spawn(move || {
            let mut active: Option<String> = None;
            loop {
                let want = wanted().filter(|key| parse(key).is_some());
                if want != active {
                    unsafe {
                        if active.is_some() {
                            UnregisterHotKey(std::ptr::null_mut(), ID);
                        }
                    }
                    active = None;
                    if let Some((modifiers, key)) = want.as_deref().and_then(parse) {
                        // Taken by another program: tried again on the next turn.
                        if unsafe { RegisterHotKey(std::ptr::null_mut(), ID, modifiers | MOD_NOREPEAT, key) } != 0 {
                            active = want;
                        }
                    }
                }
                let mut message: MSG = unsafe { std::mem::zeroed() };
                while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                    if message.message == WM_HOTKEY && message.wParam == ID as usize {
                        pressed();
                    }
                }
                thread::sleep(Duration::from_millis(if active.is_some() { 30 } else { 500 }));
            }
        })
        .ok();
}
