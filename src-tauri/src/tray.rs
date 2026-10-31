//! Tray presence.
//!
//! On macOS the app runs as an accessory: no dock icon, no menu bar app menu,
//! just this tray item. On Windows it lives in the notification area. Either
//! way, everything the user needs while working is reachable from here
//! without opening the window.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::state::Desktop;

pub const TRAY_ID: &str = "zroutery-tray";

/// Announced after the tray starts or stops the gateway.
///
/// The tray has no snapshot to return, and the window that owns the status
/// display is usually hidden while the tray is used. The window listens for
/// this and re-reads the state instead of showing the one it loaded at
/// startup.
pub const GATEWAY_STATE_EVENT: &str = "zroutery://gateway-state-changed";

/// Menu labels, in the OS language. The webview has its own locale machinery;
/// the tray is drawn by the OS before any webview exists, so it follows the
/// system directly.
struct Labels {
    open: &'static str,
    panel: &'static str,
    gateway_stopped: &'static str,
    start: &'static str,
    copy_url: &'static str,
    copy_token: &'static str,
    quit: &'static str,
}

fn labels() -> Labels {
    if system_language_is_chinese() {
        Labels {
            open: "打开 Zroutery",
            panel: "打开快切面板",
            gateway_stopped: "网关:已停止",
            start: "启动网关",
            copy_url: "复制 Base URL",
            copy_token: "复制令牌",
            quit: "退出 Zroutery",
        }
    } else {
        Labels {
            open: "Open Zroutery",
            panel: "Open quick panel",
            gateway_stopped: "Gateway: stopped",
            start: "Start gateway",
            copy_url: "Copy base URL",
            copy_token: "Copy API token",
            quit: "Quit Zroutery",
        }
    }
}

/// Whether the OS UI language is Chinese.
///
/// The tray is drawn by the OS before any webview exists, so the webview's
/// locale cannot inform it — this reads the system directly. The shell's
/// notifications are drawn by the OS as well, so they read the same way.
#[cfg(target_os = "windows")]
pub(crate) fn system_language_is_chinese() -> bool {
    // PRIMARYLANGID of the user's default UI language: 0x04 is Chinese.
    let lang = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3ff;
    lang == 0x04
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn system_language_is_chinese() -> bool {
    std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .map(|v| v.to_ascii_lowercase().starts_with("zh"))
        .unwrap_or(false)
}

/// Menu items whose labels change with the proxy state.
pub struct TrayHandles {
    pub status: MenuItem<Wry>,
    pub toggle: MenuItem<Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    app.manage(crate::tray_panel::Controller::default());
    let l = labels();
    let open = MenuItem::with_id(app, "open", l.open, true, None::<&str>)?;
    let panel = MenuItem::with_id(app, "panel", l.panel, true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", l.gateway_stopped, false, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", l.start, true, None::<&str>)?;
    let copy_url = MenuItem::with_id(app, "copy_url", l.copy_url, true, None::<&str>)?;
    let copy_token = MenuItem::with_id(app, "copy_token", l.copy_token, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", l.quit, true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open,
            &panel,
            &PredefinedMenuItem::separator(app)?,
            &status,
            &toggle,
            &PredefinedMenuItem::separator(app)?,
            &copy_url,
            &copy_token,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Zroutery")
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                crate::tray_panel::toggle(tray.app_handle(), rect);
            }
        });

    // A monochrome template image is what macOS expects in the menu bar; it
    // adapts to light and dark automatically. Windows and Linux have no
    // template concept — there the icon must carry its own colour.
    if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
        #[cfg(target_os = "macos")]
        {
            builder = builder.icon(icon).icon_as_template(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            builder = builder.icon(icon);
        }
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
        "panel" => crate::tray_panel::show_at_cursor(&app),
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
                    // The tray menu has nowhere to write an error down and
                    // the dashboard was not asked to start anything, so
                    // nothing on screen says this happened. The operator
                    // has to be told: they clicked "start" and the gateway
                    // is not running, and the reason is not in the menu.
                    #[cfg(desktop)]
                    crate::shell::notify_gateway_failure(&app, &e);
                }
                refresh(&app, &desktop).await;
                // The window owns the status display and did not run this
                // command, so tell it the state it is drawing is stale.
                if let Err(e) = app.emit(GATEWAY_STATE_EVENT, ()) {
                    tracing::warn!("cannot announce the tray gateway change: {e}");
                }
            });
        }
        "copy_url" | "copy_token" => {
            let want_token = event.id().as_ref() == "copy_token";
            tauri::async_runtime::spawn(async move {
                let Some(desktop) = app.try_state::<Arc<Desktop>>().map(|s| s.inner().clone())
                else {
                    return;
                };
                let text = if want_token {
                    desktop.auth_token()
                } else {
                    let snapshot = desktop.snapshot().await;
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
    let _ = crate::tray_panel::hide(app);
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
            // Browsers are closed before the process ends. A check-in paused on a
            // challenge is holding a real window and a real profile lock; exiting
            // with it open would leave both behind with nothing owning them.
            #[cfg(feature = "account-maint")]
            desktop.shutdown_browsers().await;
        }
        app.exit(0);
    });
}

/// Push the current proxy state into the tray labels and tooltip.
pub async fn refresh(app: &AppHandle, desktop: &Desktop) {
    let running = desktop.is_running().await;
    let config = desktop.core.config();
    let zh = system_language_is_chinese();
    let label = if running {
        format!(
            "{} {}:{}",
            if zh { "网关" } else { "Gateway" },
            config.server.host,
            config.server.port
        )
    } else if zh {
        "网关:已停止".to_string()
    } else {
        "Gateway: stopped".to_string()
    };

    if let Some(handles) = app.try_state::<TrayHandles>() {
        let _ = handles.status.set_text(&label);
        let _ = handles.toggle.set_text(if running {
            if zh {
                "停止网关"
            } else {
                "Stop gateway"
            }
        } else if zh {
            "启动网关"
        } else {
            "Start gateway"
        });
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(format!("Zroutery — {label}")));
    }
}
