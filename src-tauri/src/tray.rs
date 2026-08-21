//! Menu bar presence.
//!
//! On macOS the app runs as an accessory: no dock icon, no menu bar app menu,
//! just this tray item. Everything the user needs while working is reachable
//! from here without opening the window.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::state::Desktop;

pub const TRAY_ID: &str = "zroutery-tray";

/// Menu items whose labels change with the proxy state.
pub struct TrayHandles {
    pub status: MenuItem<Wry>,
    pub toggle: MenuItem<Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Proxy: stopped", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start proxy", true, None::<&str>)?;
    let copy_url = MenuItem::with_id(app, "copy_url", "Copy base URL", true, None::<&str>)?;
    let copy_token = MenuItem::with_id(app, "copy_token", "Copy API token", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Zroutery", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &toggle,
            &copy_url,
            &copy_token,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Zroutery")
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show_dashboard(tray.app_handle());
            }
        });

    // A monochrome template image is what macOS expects in the menu bar; it
    // adapts to light and dark automatically.
    if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
        builder = builder.icon(icon).icon_as_template(true);
    } else if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;
    app.manage(TrayHandles { status, toggle });
    Ok(())
}

fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let app = app.clone();
    match event.id().as_ref() {
        "open" => show_dashboard(&app),
        "quit" => quit(&app),
        "toggle" => {
            tauri::async_runtime::spawn(async move {
                let Some(desktop) = app.try_state::<Arc<Desktop>>().map(|s| s.inner().clone())
                else {
                    return;
                };
                if desktop.is_running().await {
                    desktop.stop().await;
                } else if let Err(e) = desktop.start().await {
                    tracing::error!("cannot start proxy from tray: {e}");
                }
                refresh(&app, &desktop).await;
            });
        }
        "copy_url" | "copy_token" => {
            let want_token = event.id().as_ref() == "copy_token";
            tauri::async_runtime::spawn(async move {
                let Some(desktop) = app.try_state::<Arc<Desktop>>().map(|s| s.inner().clone())
                else {
                    return;
                };
                let snapshot = desktop.snapshot().await;
                let text = if want_token {
                    snapshot.server.token
                } else {
                    snapshot.server.base_url.unwrap_or_else(|| {
                        format!("http://{}:{}", snapshot.server.host, snapshot.server.port)
                    })
                };
                let _ = app.clipboard().write_text(text);
            });
        }
        _ => {}
    }
}

pub fn show_dashboard(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Stop the proxy and end the process.
///
/// Shared by the tray item and the dashboard's Quit button so both behave
/// identically. `AppHandle::exit` reports a `Some(code)` exit request, which is
/// how the run loop tells a real quit apart from the last window closing.
pub fn quit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(desktop) = app.try_state::<Arc<Desktop>>().map(|s| s.inner().clone()) {
            desktop.stop().await;
        }
        app.exit(0);
    });
}

/// Push the current proxy state into the tray labels and tooltip.
pub async fn refresh(app: &AppHandle, desktop: &Desktop) {
    let running = desktop.is_running().await;
    let config = desktop.core.config();
    let label = if running {
        format!("Proxy: {}:{}", config.server.host, config.server.port)
    } else {
        "Proxy: stopped".to_string()
    };

    if let Some(handles) = app.try_state::<TrayHandles>() {
        let _ = handles.status.set_text(&label);
        let _ = handles
            .toggle
            .set_text(if running { "Stop proxy" } else { "Start proxy" });
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let models = config.models.iter().filter(|m| m.enabled).count();
        let _ = tray.set_tooltip(Some(format!("Zroutery — {label} — {models} models")));
    }
}
