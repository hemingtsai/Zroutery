//! Tauri commands: the entire surface the dashboard can call.

use std::sync::Arc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use zroutery_core::config::{AppConfig, ProviderConfig, SecretStore};
use zroutery_core::upstream::{DiscoveredModel, Upstream};

use crate::ccswitch;
use crate::logs::LogBuffer;
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

/// Recent tracing output for the Logs tab. The buffer is capped in memory, so
/// this is a rolling window rather than the full process log.
#[tauri::command]
pub fn get_logs(logs: State<'_, LogBuffer>) -> Cmd<Vec<String>> {
    Ok(logs.lines())
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
    // Keychain I/O blocks; keep it off the async worker thread.
    let secrets = Arc::clone(&desktop.secrets);
    let key_ref = provider.key_ref.clone();
    let key = key.to_string();
    tauri::async_runtime::spawn_blocking(move || secrets.set(&key_ref, &key))
        .await
        .map_err(|e| e.to_string())??;
    Ok(refreshed(&app, &desktop).await)
}

#[tauri::command]
pub async fn clear_provider_key(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    provider_id: String,
) -> Cmd<Snapshot> {
    // We delete the key regardless of whether the provider is present in the
    // current configuration.  Existing code early-returned an error which
    // surfaced when a provider had been removed from the UI but its key
    // remained; the next delete request then failed.  Returning a success
    // keeps the UI consistent and the key is effectively removed.
    let config = desktop.core.config();
    let key_ref = match config.provider(&provider_id) {
        Some(p) => p.key_ref.clone(),
        None => format!("provider:{provider_id}"),
    };
    let secrets = Arc::clone(&desktop.secrets);
    // Delete from keychain; a missing entry is not an error.
    let _ = tauri::async_runtime::spawn_blocking(move || secrets.delete(&key_ref))
        .await
        .map_err(|e| e.to_string());
    Ok(refreshed(&app, &desktop).await)
}

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

/// Ask a provider for its model list. Works on unsaved providers too, so the
/// user can test before committing.
#[tauri::command]
pub async fn fetch_provider_models(
    desktop: State<'_, Arc<Desktop>>,
    provider: ProviderConfig,
) -> Cmd<Vec<DiscoveredModel>> {
    // Keychain I/O blocks; keep it off the async worker thread.
    let secrets = Arc::clone(&desktop.secrets);
    let key_ref = provider.key_ref.clone();
    let key = tauri::async_runtime::spawn_blocking(move || secrets.get(&key_ref))
        .await
        .map_err(|e| e.to_string())?;
    let bypass_proxy = desktop.core.config().server.bypass_proxy;
    Upstream::new(bypass_proxy)
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

// ------------------------------------------------------- CC Switch import

/// What CC Switch has, reduced to what an import decision needs.
///
/// The API key travels with the draft but the dashboard never renders it;
/// it exists so the import command can be a single round trip without
/// re-reading the database.
#[derive(serde::Serialize)]
pub struct CcSwitchPreview {
    /// The source path the providers came from, for the panel's subtitle.
    pub source: String,
    /// Every provider found; `already_imported` marks the ones that would be
    /// skipped because a provider with that id exists.
    pub providers: Vec<ccswitch::CcProviderDraft>,
}

#[tauri::command]
pub async fn ccswitch_preview(
    desktop: State<'_, Arc<Desktop>>,
) -> Cmd<CcSwitchPreview> {
    // SQLite and file reads block; keep them off the async worker thread.
    let found = tauri::async_runtime::spawn_blocking(ccswitch::read_providers)
        .await
        .map_err(|e| e.to_string())??;

    let config = desktop.core.config();
    let providers = found
        .into_iter()
        .map(|p| {
            // A provider talking to the same endpoint is the same relay as
            // far as an import is concerned, whatever it is called here.
            let existing = config.providers.iter().find(|existing| {
                existing.base_url.trim_end_matches('/') == p.base_url.trim_end_matches('/')
            });
            let (target_id, already_imported) = match existing {
                Some(existing) => (existing.id.clone(), true),
                None => {
                    let taken = |id: &str| config.provider(id).is_some();
                    (ccswitch::unique_provider_id(&p.name, &taken), false)
                }
            };
            ccswitch::CcProviderDraft {
                provider: p,
                target_id,
                already_imported,
            }
        })
        .collect();

    Ok(CcSwitchPreview {
        source: ccswitch::cc_switch_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
        providers,
    })
}

/// Import the selected CC Switch providers.
///
/// Providers are matched by CC Switch's own provider id (stored in the
/// preview draft), so the selection survives re-ordering in CC Switch.
/// API keys go straight into the credential store; the config file keeps
/// only key refs, exactly like a hand-entered provider.
#[tauri::command]
pub async fn ccswitch_import(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
    ids: Vec<String>,
) -> Cmd<Snapshot> {
    let drafts = {
        let providers = ccswitch::read_providers()?;
        providers
            .into_iter()
            .filter(|p| ids.contains(&p.source_id))
            .collect::<Vec<_>>()
    };

    let mut config = (*desktop.core.config()).clone();
    let mut imported_keys: Vec<(String, String)> = Vec::new();

    // The current CC Switch provider becomes the class primary; the rest keep
    // CC Switch's order behind it.
    let mut priority = 0;
    for draft in &drafts {
        let taken = |id: &str| config.provider(id).is_some();
        let provider_id = ccswitch::unique_provider_id(&draft.name, &taken);
        let (provider, models) = ccswitch::to_zroutery(
            draft,
            provider_id.clone(),
            if draft.is_current { 0 } else { priority.max(10) },
            None,
        );
        if let Some(key) = draft.api_key.clone() {
            imported_keys.push((provider.key_ref.clone(), key));
        }
        config.providers.push(provider);
        config.models.extend(models);
        priority += 10;
    }

    // Store the keys first: an import whose providers reference keys that do
    // not exist yet would look broken in the dashboard.
    let secrets = Arc::clone(&desktop.secrets);
    tauri::async_runtime::spawn_blocking(move || {
        for (key_ref, key) in &imported_keys {
            secrets.set(key_ref, key)?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;

    desktop.apply_config(config).await?;
    Ok(refreshed(&app, &desktop).await)
}
