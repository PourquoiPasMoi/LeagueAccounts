//! Commands of the phone remote (the server itself is `leagueaccounts::remote`).

use crate::views::{fail, CommandResult};
use leagueaccounts::remote::{self, RemoteView, TestResult};
use std::collections::HashMap;
use tauri::{AppHandle, Emitter};

pub fn spawn(app: AppHandle) {
    remote::spawn(move |running| {
        let _ = app.emit("remote-status", running);
    });
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|_| fail("operation_failed"))
}

#[tauri::command]
pub async fn remote_info() -> CommandResult<RemoteView> {
    blocking(remote::info).await
}

#[tauri::command]
pub async fn set_remote_enabled(enabled: bool) -> CommandResult<RemoteView> {
    blocking(move || remote::set_enabled(enabled)).await
}

#[tauri::command]
pub async fn remote_reset(forget: bool) -> CommandResult<RemoteView> {
    blocking(move || remote::reset(forget)).await
}

#[tauri::command]
pub async fn remote_open(minutes: u64) -> CommandResult<RemoteView> {
    blocking(move || remote::open(minutes)).await
}

#[tauri::command]
pub async fn remote_test() -> CommandResult<TestResult> {
    blocking(remote::test).await
}

#[tauri::command]
pub async fn set_remote_texts(texts: HashMap<String, String>) -> CommandResult<()> {
    remote::set_texts(texts);
    Ok(())
}
