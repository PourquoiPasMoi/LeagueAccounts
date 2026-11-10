//! Settings › Interface size: zooms the web view and scales the window with it,
//! like the client's window sizes, so the layout keeps the room it was designed for.

use tauri::{LogicalSize, Manager, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewWindow};

pub const MIN_UI_SCALE: u32 = 80;
pub const MAX_UI_SCALE: u32 = 200;

/// Zoom `window` to `to` percent. Its size and minimum size follow, from the
/// `from` percent it was shown at, but never outgrow the screen's work area.
/// A maximized window keeps filling the screen.
pub fn apply(window: &WebviewWindow, from: u32, to: u32) {
    let zoom = f64::from(to) / 100.0;
    let _ = window.set_zoom(zoom);
    let Ok(Some(monitor)) = window.current_monitor() else { return };
    let dpi = monitor.scale_factor();
    let area = *monitor.work_area();
    if let Some(config) = window.config().app.windows.iter().find(|config| config.label == window.label()) {
        if let (Some(width), Some(height)) = (config.min_width, config.min_height) {
            let screen = area.size.to_logical::<f64>(dpi);
            let min = LogicalSize::new((width * zoom).min(screen.width), (height * zoom).min(screen.height));
            let _ = window.set_min_size(Some(min));
        }
    }
    if from == to || window.is_maximized().unwrap_or(true) || window.is_fullscreen().unwrap_or(true) {
        return;
    }
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else { return };
    let (position, size) = scaled(position, size, f64::from(to) / f64::from(from), area);
    let _ = window.set_size(size);
    let _ = window.set_position(position);
}

/// The window scaled by `ratio`: at most the size of the work area, centered
/// where it was, and moved inside the work area when it would overflow it.
fn scaled(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    ratio: f64,
    area: PhysicalRect<i32, u32>,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let fit = |length: u32, limit: u32| ((f64::from(length) * ratio).round() as u32).min(limit);
    let target = PhysicalSize::new(fit(size.width, area.size.width), fit(size.height, area.size.height));
    let place = |start: i32, length: u32, new: u32, area_start: i32, area_length: u32| {
        let center = start + (length / 2) as i32;
        (center - (new / 2) as i32)
            .min(area_start + (area_length - new) as i32)
            .max(area_start)
    };
    let x = place(position.x, size.width, target.width, area.position.x, area.size.width);
    let y = place(position.y, size.height, target.height, area.position.y, area.size.height);
    (PhysicalPosition::new(x, y), target)
}
