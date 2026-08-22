//! Tauri commands: the entire surface the dashboard can call.

use std::sync::Arc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use zroutery_core::config::{AppConfig, ProviderConfig, SecretStore};
use zroutery_core::upstream::{DiscoveredModel, Upstream};

use crate::state::{Activity, Desktop, Snapshot};
use crate::store;
use crate::tray;

type Cmd<T> = Result<T, String>;

async fn refreshed(app: &AppHandle, desktop: &Desktop) -> Snapshot {
    tray::refresh(app, desktop).await;
    desktop.snapshot().await
}

#[tauri::command]
pub async fn get_snapshot(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    Ok(refreshed(&app, &desktop).await)
}

/// Counters and log only: what the Activity tab polls, without cloning the
/// configuration or asking the keychain about every provider.
#[tauri::command]
pub fn get_activity(desktop: State<'_, Arc<Desktop>>) -> Cmd<Activity> {
    Ok(desktop.activity())
}

/// The token in plain text, for the dashboard's explicit "Reveal" action. Every
/// other path only ever sees the hint.
#[tauri::command]
pub fn reveal_token(desktop: State<'_, Arc<Desktop>>) -> Cmd<String> {
    Ok(desktop.auth_token())
}

/// Put the token on the clipboard without it entering the webview at all.
#[tauri::command]
pub fn copy_token(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<()> {
    app.clipboard()
        .write_text(desktop.auth_token())
        .map_err(|e| e.to_string())
}

/// Replace the whole configuration document.
///
/// The dashboard always sends the full config, which keeps conflict handling
/// trivial for a single user desktop app.
#[tauri::command]
pub async fn save_config(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    config: AppConfig,
) -> Cmd<Snapshot> {
    desktop.apply_config(config).await?;
    desktop.set_warning(None);
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn set_provider_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
    api_key: String,
) -> Cmd<Snapshot> {
    let config = desktop.core.config();
    let provider = config
        .provider(&provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    let key = api_key.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    desktop.secrets.set(&provider.key_ref, key)?;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_provider_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
) -> Cmd<Snapshot> {
    let config = desktop.core.config();
    let provider = config
        .provider(&provider_id)
        .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
    desktop.secrets.delete(&provider.key_ref)?;
    Ok(refreshed(&app, &desktop).await)
}

/// Ask a provider for its model list. Works on unsaved providers too, so the
/// user can test before committing.
/// Ask a provider what credit is left. The stored answer, including a failure,
/// comes back in the snapshot.
#[tauri::command]
pub async fn refresh_balance(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
) -> Cmd<Snapshot> {
    // The error is already recorded against the provider, so the command itself
    // succeeds and the dashboard renders the reason next to the provider.
    let _ = desktop.refresh_balance(&provider_id).await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn refresh_balances(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    let _ = desktop.refresh_all_balances().await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn fetch_provider_models(
    desktop: State<'_, Arc<Desktop>>,
    provider: ProviderConfig,
) -> Cmd<Vec<DiscoveredModel>> {
    let key = desktop.secrets.get(&provider.key_ref);
    Upstream::new()
        .list_models(&provider, key.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Hold an election now and pin the outcome. Costs one tiny request per model.
#[tauri::command]
pub async fn run_election(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.hold_election().await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn start_proxy(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.start().await?;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn stop_proxy(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.stop().await;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn regenerate_token(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    let mut config = (*desktop.core.config()).clone();
    config.server.auth_token = store::generate_token();
    desktop.apply_config(config).await?;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_stats(app: AppHandle, desktop: State<'_, Arc<Desktop>>) -> Cmd<Snapshot> {
    desktop.core.stats().clear();
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn reset_model_health(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    model_id: String,
) -> Cmd<Snapshot> {
    desktop.core.router().reset(&model_id);
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub fn copy_text(app: AppHandle, text: String) -> Cmd<()> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_window(app: AppHandle) -> Cmd<()> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Same path as the tray's Quit item: stop the proxy, then end the process.
#[tauri::command]
pub fn quit_app(app: AppHandle) -> Cmd<()> {
    tray::quit(&app);
    Ok(())
}
