//! Zroutery desktop shell.
//!
//! Runs the aggregating proxy in-process and exposes it through a menu bar
//! item plus a dashboard window.

mod commands;
pub mod secrets;
pub mod state;
pub mod store;
mod tray;

use std::sync::Arc;

use tauri::{Manager, RunEvent, WindowEvent};

use crate::secrets::KeychainSecrets;
use crate::state::Desktop;

const KEYCHAIN_SERVICE: &str = "app.zroutery.desktop";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZROUTERY_LOG")
                .unwrap_or_else(|_| "info,zroutery_core=info".into()),
        )
        .with_target(false)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::save_config,
            commands::set_provider_key,
            commands::clear_provider_key,
            commands::fetch_provider_models,
            commands::start_proxy,
            commands::stop_proxy,
            commands::regenerate_token,
            commands::clear_stats,
            commands::reset_model_health,
            commands::copy_text,
            commands::hide_window,
            commands::quit_app,
        ])
        .setup(|app| {
            // Menu bar only: no dock icon, no app switcher entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_dir = match std::env::var("ZROUTERY_CONFIG_DIR") {
                // Escape hatch for testing and for keeping the config with a
                // dotfile repo.
                Ok(dir) => std::path::PathBuf::from(dir),
                Err(_) => app
                    .path()
                    .app_config_dir()
                    .map_err(|e| format!("cannot resolve the config directory: {e}"))?,
            };
            std::fs::create_dir_all(&config_dir)?;

            let (config, warning) = store::load(&config_dir);
            // Make sure a freshly generated token reaches disk.
            store::save(&config_dir, &config)?;

            let autostart = config.server.autostart;
            let secrets = Arc::new(KeychainSecrets::new(KEYCHAIN_SERVICE));
            let desktop = Arc::new(Desktop::new(config_dir, config, secrets));
            desktop.set_warning(warning);
            app.manage(Arc::clone(&desktop));

            tray::build(app.handle())?;

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if autostart {
                    if let Err(e) = desktop.start().await {
                        tracing::error!("autostart failed: {e}");
                        desktop.set_warning(Some(format!("The proxy could not start: {e}")));
                    }
                }
                tray::refresh(&handle, &desktop).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the dashboard keeps the proxy running in the menu bar.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Zroutery application")
        .run(|app, event| {
            if let RunEvent::ExitRequested { api, .. } = event {
                // Keep running when the last window closes.
                api.prevent_exit();
                let _ = app;
            }
        });
}
