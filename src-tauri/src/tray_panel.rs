//! A lazily attached tray webview. The page owns its content and hover intent;
//! this module owns visibility, focus and the monitor/DPI boundary.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow};

pub const LABEL: &str = "tray-panel";
pub const URL: &str = "index.html?mode=tray-panel";
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 520.0;
const BLUR_GRACE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Controller(Mutex<PanelState>);

#[derive(Default)]
struct PanelState {
    requested: bool,
    ready: bool,
    creating: bool,
    serial: u64,
    anchor: Option<tauri::Rect>,
    blurred_at: Option<Instant>,
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Physical pixels throughout, including monitors with a negative origin.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn placement(icon: Area, work: Area, scale: f64) -> Area {
    let margin = (8.0 * scale).min(work.width / 4.0).min(work.height / 4.0);
    let width = (WIDTH * scale).min(work.width - 2.0 * margin).max(1.0);
    let height = (HEIGHT * scale).min(work.height - 2.0 * margin).max(1.0);
    let left = work.x + margin;
    let top = work.y + margin;
    let right = (work.x + work.width - margin - width).max(left);
    let bottom = (work.y + work.height - margin - height).max(top);
    let above = icon.y + icon.height / 2.0 > work.y + work.height / 2.0;
    Area {
        x: (icon.x + icon.width / 2.0 - width / 2.0).clamp(left, right),
        y: if above {
            icon.y - 6.0 * scale - height
        } else {
            icon.y + icon.height + 6.0 * scale
        }
        .clamp(top, bottom),
        width,
        height,
    }
}

fn position(app: &AppHandle, window: &WebviewWindow, rect: &tauri::Rect) -> Result<(), String> {
    // Tauri tray events and cursor_position carry physical screen coordinates.
    let point = rect.position.to_physical::<f64>(1.0);
    let size = rect.size.to_physical::<f64>(1.0);
    let monitors = app
        .available_monitors()
        .map_err(|error| error.to_string())?;
    let center = (point.x + size.width / 2.0, point.y + size.height / 2.0);
    let monitor = monitors
        .iter()
        .find(|monitor| {
            let origin = monitor.position();
            let extent = monitor.size();
            center.0 >= f64::from(origin.x)
                && center.0 < f64::from(origin.x) + f64::from(extent.width)
                && center.1 >= f64::from(origin.y)
                && center.1 < f64::from(origin.y) + f64::from(extent.height)
        })
        .or_else(|| monitors.first())
        .ok_or("no monitor is available for the tray panel")?;
    let work = monitor.work_area();
    let frame = placement(
        Area {
            x: point.x,
            y: point.y,
            width: size.width,
            height: size.height,
        },
        Area {
            x: f64::from(work.position.x),
            y: f64::from(work.position.y),
            width: f64::from(work.size.width),
            height: f64::from(work.size.height),
        },
        monitor.scale_factor(),
    );
    #[cfg(target_os = "macos")]
    {
        let scale = monitor.scale_factor();
        window
            .set_position(tauri::LogicalPosition::new(
                frame.x / scale,
                frame.y / scale,
            ))
            .map_err(|error| error.to_string())?;
        window
            .set_size(tauri::LogicalSize::new(
                frame.width / scale,
                frame.height / scale,
            ))
            .map_err(|error| error.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        window
            .set_position(PhysicalPosition::new(
                frame.x.round() as i32,
                frame.y.round() as i32,
            ))
            .map_err(|error| error.to_string())?;
        window
            .set_size(PhysicalSize::new(
                frame.width.round() as u32,
                frame.height.round() as u32,
            ))
            .map_err(|error| error.to_string())
    }
}

fn create(app: &AppHandle) -> Result<WebviewWindow, String> {
    let window = tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(URL.into()))
        .title("Zroutery")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .visible(false)
        .focused(false)
        .initialization_script(
            r#"
            document.addEventListener('keydown', event => {
                if (event.key === 'Escape') {
                    event.preventDefault();
                    window.__TAURI_INTERNALS__.invoke('hide_tray_panel').catch(() => {});
                }
            });
        "#,
        )
        .on_page_load(|window, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Started {
                let controller = window.app_handle().state::<Controller>();
                lock(&controller.0).ready = false;
                let _ = window.hide();
            }
        })
        .build()
        .map_err(|error| error.to_string())?;
    let handle = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Focused(false) => {
            if handle
                .get_webview_window(LABEL)
                .is_some_and(|window| window.is_visible().unwrap_or(false))
            {
                lock(&handle.state::<Controller>().0).blurred_at = Some(Instant::now());
                let _ = hide(&handle);
            }
        }
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = hide(&handle);
        }
        tauri::WindowEvent::Destroyed => {
            let controller = handle.state::<Controller>();
            let mut state = lock(&controller.0);
            state.ready = false;
            state.creating = false;
        }
        _ => {}
    });
    Ok(window)
}

fn reveal(app: &AppHandle) -> Result<(), String> {
    let anchor = {
        let controller = app.state::<Controller>();
        let state = lock(&controller.0);
        if !state.requested || !state.ready {
            return Ok(());
        }
        state.anchor
    };
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    if let Some(anchor) = anchor {
        position(app, &window, &anchor)?;
    }
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

/// Called only after the panel page has mounted, so a main-window page or a
/// partially loaded bundle is never exposed as the quick panel.
pub fn ready(app: &AppHandle) -> Result<(), String> {
    lock(&app.state::<Controller>().0).ready = true;
    reveal(app)
}

pub fn hide(app: &AppHandle) -> Result<(), String> {
    if let Some(controller) = app.try_state::<Controller>() {
        let mut state = lock(&controller.0);
        state.requested = false;
        state.serial = state.serial.wrapping_add(1);
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn toggle(app: &AppHandle, rect: tauri::Rect) {
    let close = {
        let controller = app.state::<Controller>();
        let mut state = lock(&controller.0);
        state.requested
            || state
                .blurred_at
                .take()
                .is_some_and(|at| at.elapsed() < BLUR_GRACE)
    };
    if close {
        let _ = hide(app);
    } else {
        request_open(app, rect);
    }
}

pub fn show_at_cursor(app: &AppHandle) {
    let point = app
        .cursor_position()
        .unwrap_or(PhysicalPosition::new(0.0, 0.0));
    request_open(
        app,
        tauri::Rect {
            position: point.into(),
            size: PhysicalSize::new(1.0, 1.0).into(),
        },
    );
}

fn request_open(app: &AppHandle, rect: tauri::Rect) {
    let has_window = app.get_webview_window(LABEL).is_some();
    let (serial, should_create) = {
        let controller = app.state::<Controller>();
        let mut state = lock(&controller.0);
        state.requested = true;
        state.anchor = Some(rect);
        state.serial = state.serial.wrapping_add(1);
        let create = !has_window && !state.creating;
        if create {
            state.creating = true;
            state.ready = false;
        }
        (state.serial, create)
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if should_create {
            let result = create(&app);
            lock(&app.state::<Controller>().0).creating = false;
            if let Err(error) = result {
                tracing::warn!(%error, "tray panel could not be attached");
                crate::tray::show_dashboard(&app);
                return;
            }
        }
        if let Err(error) = reveal(&app) {
            tracing::warn!(%error, "tray panel could not be shown");
            crate::tray::show_dashboard(&app);
            return;
        }
        // An older UI bundle does not speak the readiness contract. Keep the
        // tray usable during a frontend/backend rolling upgrade.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let stalled = {
            let controller = app.state::<Controller>();
            let mut state = lock(&controller.0);
            let stalled = state.serial == serial && state.requested && !state.ready;
            if stalled {
                state.requested = false;
                state.serial = state.serial.wrapping_add(1);
            }
            stalled
        };
        if stalled {
            crate::tray::show_dashboard(&app);
            if let Some(window) = app.get_webview_window(LABEL) {
                let _ = window.destroy();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_stays_on_the_clicked_monitor_at_every_dpi() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            for x in [-1920.0, 0.0, 2560.0] {
                let work = Area {
                    x,
                    y: -200.0,
                    width: 1280.0,
                    height: 672.0,
                };
                for (icon_x, icon_y) in [(x, -224.0), (x + 1250.0, 478.0)] {
                    let panel = placement(
                        Area {
                            x: icon_x,
                            y: icon_y,
                            width: 24.0,
                            height: 24.0,
                        },
                        work,
                        scale,
                    );
                    assert!(panel.x >= work.x && panel.y >= work.y);
                    assert!(panel.x + panel.width <= work.x + work.width);
                    assert!(panel.y + panel.height <= work.y + work.height);
                }
            }
        }
    }

    #[test]
    fn taskbar_opens_up_and_menu_bar_opens_down() {
        let work = Area {
            x: 0.0,
            y: 24.0,
            width: 1920.0,
            height: 1008.0,
        };
        let bottom = placement(
            Area {
                x: 1800.0,
                y: 1040.0,
                width: 24.0,
                height: 40.0,
            },
            work,
            1.0,
        );
        let top = placement(
            Area {
                x: 1800.0,
                y: 0.0,
                width: 24.0,
                height: 24.0,
            },
            work,
            1.0,
        );
        assert!(bottom.y + bottom.height <= 1032.0);
        assert_eq!(top.y, 32.0);
    }
}
