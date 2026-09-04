//! Zroutery desktop shell.
//!
//! Runs the aggregating proxy in-process and exposes it through a menu bar
//! item plus a dashboard window.

mod commands;
pub mod ccswitch;
mod logs;
pub mod platform;
pub mod secrets;
pub mod state;
pub mod store;
mod tray;

use std::sync::Arc;

use tauri::{Manager, RunEvent, WindowEvent};

use crate::logs::LogBuffer;
use crate::secrets::KeychainSecrets;
use crate::state::Desktop;

pub const KEYCHAIN_SERVICE: &str = platform::APP_ID;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let log_buffer = LogBuffer::new(2000);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZROUTERY_LOG")
                .unwrap_or_else(|_| "info,zroutery_core=info".into()),
        )
        .with_target(false)
        .with_writer(log_buffer.clone())
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::get_activity,
            commands::get_logs,
            commands::reveal_token,
            commands::copy_token,
            commands::save_config,
            commands::set_provider_key,
            commands::clear_provider_key,
            commands::fetch_provider_models,
            commands::refresh_balance,
            commands::refresh_balances,
            commands::run_election,
            commands::start_proxy,
            commands::stop_proxy,
            commands::regenerate_token,
            commands::clear_stats,
            commands::reset_model_health,
            commands::copy_text,
            commands::hide_window,
            commands::quit_app,
            commands::ccswitch_preview,
            commands::ccswitch_import,
        ])
        .setup(move |app| {
            // Menu bar only: no dock icon, no app switcher entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Autostart is an OS registration, so the plugin owns it; no
            // arguments — a login launch should behave exactly like a manual
            // one, including silent-start handling.
            #[cfg(desktop)]
            {
                use tauri_plugin_autostart::MacosLauncher;
                app.handle()
                    .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))?;
            }

            // ZROUTERY_CONFIG_DIR wins; otherwise Tauri's own resolution,
            // which agrees with platform::default_config_dir on every
            // platform we ship.
            let config_dir = platform::default_config_dir();
            std::fs::create_dir_all(&config_dir)?;

            let (config, warning) = store::load(&config_dir);
            // Make sure a freshly generated token reaches disk.
            store::save(&config_dir, &config)?;

            // Sync the OS registration with the setting, so a manual config
            // edit (or an uninstaller that removed the entry) converges back
            // to what the user asked for.
            #[cfg(desktop)]
            crate::commands::sync_autostart_public(app.handle(), config.window.launch_on_login);

            // The window opens on launch unless silent start is asked for —
            // and always on macOS, where the app is a menu bar accessory:
            // there "the window never opens by itself" is the platform
            // convention, not a hidden process.
            let show_window = if cfg!(target_os = "macos") {
                false
            } else {
                !config.window.silent_start
            };
            if show_window {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }

            let autostart = config.server.autostart;
            let secrets = Arc::new(KeychainSecrets::new(KEYCHAIN_SERVICE));
            let desktop = Arc::new(Desktop::new(config_dir, config, secrets));
            desktop.set_warning(warning);
            app.manage(Arc::clone(&desktop));
            app.manage(log_buffer.clone());

            tray::build(app.handle())?;

            // Debug-only hook so the quit path can be exercised without a
            // pointer: it runs exactly the code the tray item runs.
            #[cfg(debug_assertions)]
            if let Ok(delay) = std::env::var("ZROUTERY_SELFTEST_QUIT") {
                let handle = app.handle().clone();
                let secs: u64 = delay.parse().unwrap_or(5);
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    tracing::info!("selftest: quitting");
                    tray::quit(&handle);
                });
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if autostart {
                    if let Err(e) = desktop.start().await {
                        tracing::error!("autostart failed: {e}");
                        desktop.set_warning(Some(format!("The proxy could not start: {e}")));
                    }
                }
                tray::refresh(&handle, &desktop).await;
                // After the listener is up, so a slow round of probes cannot delay
                // the port being ready.
                desktop.elect_if_configured().await;

                // Spend is flushed on a timer as well as at shutdown, so a hard kill
                // loses seconds of history rather than the whole run.
                let ledger_keeper = Arc::clone(&desktop);
                tauri::async_runtime::spawn(async move {
                    let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
                    loop {
                        tick.tick().await;
                        ledger_keeper.flush_ledger();
                    }
                });
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // The close button is a window action; what it does to the
                // process is the user's setting. Keep-in-tray hides the
                // window and everything keeps running; otherwise closing
                // quits through the same path the tray's Quit item uses, so
                // the gateway stops and the ledger is flushed either way.
                let desktop = window
                    .app_handle()
                    .try_state::<Arc<Desktop>>()
                    .map(|s| s.inner().clone());
                let keep_in_tray = desktop
                    .as_ref()
                    .map(|d| d.window_rules().keep_in_tray)
                    .unwrap_or(true);
                if keep_in_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    crate::tray::quit(window.app_handle());
                }
            }
        })
        .build(tauri::generate_context!());

    // A panic here would surface as a crash report; a message plus a non-zero
    // exit code is more use to whoever has to fix it.
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            tracing::error!("Zroutery cannot start: {e}");
            eprintln!("Zroutery cannot start: {e}");
            std::process::exit(1);
        }
    };

    app.run(|_app, event| {
        if let RunEvent::ExitRequested { code, api, .. } = event {
            // `code` is None when the request comes from user interaction, i.e.
            // the last window was closed: stay alive in the menu bar. An explicit
            // quit goes through `AppHandle::exit`, which carries a code, and has
            // to be honoured.
            if should_stay_resident(code) {
                api.prevent_exit();
            }
        }
    });
}

/// Whether an exit request should be ignored so the app keeps living in the menu
/// bar.
fn should_stay_resident(exit_code: Option<i32>) -> bool {
    exit_code.is_none()
}

#[cfg(test)]
mod tests {
    use super::should_stay_resident;

    #[test]
    fn closing_the_window_keeps_the_app_alive_but_quitting_does_not() {
        // Last window closed: Tauri reports no exit code.
        assert!(should_stay_resident(None));
        // Tray "Quit" and the dashboard button both call AppHandle::exit(0).
        assert!(!should_stay_resident(Some(0)));
        assert!(!should_stay_resident(Some(1)));
    }
}
