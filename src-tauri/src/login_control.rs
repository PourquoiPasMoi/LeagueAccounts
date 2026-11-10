//! Serialize login and refresh admission with shutdown, keeping the event
//! loop alive until native cleanup and admitted storage work have finished.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{plugin::TauriPlugin, AppHandle, RunEvent, WindowEvent, Wry};

#[derive(Default)]
struct State {
    running: bool,
    exiting: bool,
    refreshes: usize,
    full_refresh: bool,
}

#[derive(Default)]
pub struct LoginControl {
    state: Mutex<State>,
    idle: Condvar,
    pub cancel: AtomicBool,
}

pub struct LoginGuard(Arc<LoginControl>);

pub struct RefreshGuard {
    control: Arc<LoginControl>,
    exclusive: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ExitAction {
    Ready,
    Wait,
    Waiting,
}

impl LoginControl {
    pub fn try_start(self: &Arc<Self>) -> Option<LoginGuard> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.running || state.exiting {
            return None;
        }
        state.running = true;
        // Reset under the same lock as exit admission: a queued login cannot
        // erase a cancellation requested by the window-close handler.
        self.cancel.store(false, Ordering::SeqCst);
        Some(LoginGuard(Arc::clone(self)))
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).running
    }

    pub fn try_start_refresh(self: &Arc<Self>, exclusive: bool) -> Option<RefreshGuard> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.exiting || (exclusive && state.full_refresh) {
            return None;
        }
        state.refreshes += 1;
        state.full_refresh |= exclusive;
        Some(RefreshGuard { control: Arc::clone(self), exclusive })
    }

    pub fn refresh_running(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).full_refresh
    }

    pub fn has_refreshes(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).refreshes != 0
    }

    pub fn is_exiting(&self) -> bool {
        self.state.lock().unwrap_or_else(|error| error.into_inner()).exiting
    }

    /// Atomically close all admission only after login and refreshes are idle.
    pub fn close_if_idle(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.running || state.refreshes != 0 {
            return false;
        }
        state.exiting = true;
        true
    }

    fn request_exit(&self) -> ExitAction {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let first_request = !state.exiting;
        state.exiting = true;
        self.request_cancel();
        if !state.running && state.refreshes == 0 {
            ExitAction::Ready
        } else if first_request {
            ExitAction::Wait
        } else {
            ExitAction::Waiting
        }
    }

    fn wait_idle(&self) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let _idle = self
            .idle
            .wait_while(state, |state| state.running || state.refreshes != 0)
            .unwrap_or_else(|error| error.into_inner());
    }
}

impl Drop for LoginGuard {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|error| error.into_inner());
        state.running = false;
        self.0.idle.notify_all();
    }
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        let mut state = self.control.state.lock().unwrap_or_else(|error| error.into_inner());
        state.refreshes -= 1;
        if self.exclusive {
            state.full_refresh = false;
        }
        self.control.idle.notify_all();
    }
}

fn defer_exit(app: &AppHandle, control: &Arc<LoginControl>, code: i32) -> bool {
    match control.request_exit() {
        ExitAction::Ready => false,
        ExitAction::Waiting => true,
        ExitAction::Wait => {
            let app = app.clone();
            let control = Arc::clone(control);
            std::thread::spawn(move || {
                // Do not block Tauri's event loop while native cleanup runs.
                control.wait_idle();
                app.exit(code);
            });
            true
        }
    }
}

pub fn plugin(control: Arc<LoginControl>) -> TauriPlugin<Wry> {
    tauri::plugin::Builder::new("login-cleanup")
        .on_event(move |app, event| match event {
            RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" && defer_exit(app, &control, 0) => {
                api.prevent_close();
            }
            RunEvent::ExitRequested { api, code, .. }
                if defer_exit(app, &control, code.unwrap_or(0)) =>
            {
                api.prevent_exit();
            }
            _ => {}
        })
        .build()
}
