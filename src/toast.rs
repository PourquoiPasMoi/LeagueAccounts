//! Windows notifications (the toasts in the corner, kept in the notification
//! centre). The app has no installer, so its name and picture for Windows are
//! written once under the current user's registry key for app ids; nothing
//! else is needed.

use std::path::Path;
use std::sync::Mutex;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

/// The id Windows files the notifications under.
const APP_ID: &str = "PourquoiPasMoi.LeagueCompanion";
const APP_NAME: &str = "League Companion";

/// Toasts on screen or in the notification centre: kept so a click on one
/// still reaches the app.
static SHOWN: Mutex<Vec<ToastNotification>> = Mutex::new(Vec::new());

fn escape(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

fn set_value(name: PCWSTR, value: &str) {
    let key = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{APP_ID}"));
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, &key, name, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32);
    }
}

/// Show a notification with a title and a line of text. `icon`: a PNG shown
/// as the app's picture. `clicked` runs when the player clicks it while the
/// app is running.
pub fn show(title: &str, body: &str, icon: Option<&Path>, clicked: impl Fn() + Send + 'static) -> windows::core::Result<()> {
    // Already initialised on this thread is fine.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    set_value(w!("DisplayName"), APP_NAME);
    if let Some(icon) = icon.filter(|icon| icon.is_file()) {
        set_value(w!("IconUri"), &icon.display().to_string());
    }
    // No sound: a notification must never be heard over a game or a call.
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><audio silent=\"true\"/></toast>",
        escape(title),
        escape(body)
    );
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    toast.Activated(&TypedEventHandler::new(move |_, _| {
        clicked();
        Ok(())
    }))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)?;
    if let Ok(mut shown) = SHOWN.lock() {
        if shown.len() >= 8 {
            shown.remove(0);
        }
        shown.push(toast);
    }
    Ok(())
}
