//! Typed management commands; paths come from a host file picker, never ESM.
use super::views::{PluginInspection, PluginInstallation, PluginTheme};
use crate::state::Desktop;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use zroutery_plugin_api::{theme::ThemeV1, PermissionRequest, PluginError};
use zroutery_plugin_host::{install::Package, lifecycle::Manager as PluginManager};

fn manager(desktop: &Desktop) -> Result<&PluginManager, String> {
    desktop
        .plugins
        .manager
        .as_ref()
        .map_err(|error| error.to_string())
}
fn refresh(desktop: &Desktop) {
    #[cfg(feature = "plugin")]
    desktop.core.refresh_plugin_bindings();
    #[cfg(not(feature = "plugin"))]
    let _ = desktop;
}

#[tauri::command]
pub async fn list_plugins(
    desktop: State<'_, Arc<Desktop>>,
) -> Result<Vec<PluginInstallation>, String> {
    let manager = manager(&desktop)?;
    manager
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|i| PluginInstallation::from(manager, i))
        .collect()
}

#[tauri::command]
pub async fn inspect_plugin_package(
    app: AppHandle,
    desktop: State<'_, Arc<Desktop>>,
) -> Result<Option<PluginInspection>, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = app
            .dialog()
            .file()
            .add_filter("Zroutery plugin", &["zrp"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = path
            .into_path()
            .map_err(|_| "invalid_manifest".to_string())?;
        let file = std::fs::File::open(path).map_err(|_| "invalid_manifest".to_string())?;
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take((zroutery_plugin_host::install::MAX_ARCHIVE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "invalid_manifest".to_string())?;
        let package = Package::from_archive(&bytes).map_err(|e| e.to_string())?;
        let stage_id = desktop
            .plugins
            .stage(package.clone())
            .map_err(|e| e.to_string())?;
        Ok(Some(PluginInspection::from(stage_id, &package)))
    })
    .await
    .map_err(|_| "plugin_crashed".to_string())?
}

#[tauri::command]
pub async fn install_plugin_package(
    desktop: State<'_, Arc<Desktop>>,
    stage_id: String,
    package_digest: String,
) -> Result<PluginInstallation, String> {
    let package = desktop
        .plugins
        .consume(&stage_id, &package_digest)
        .map_err(|e| e.to_string())?;
    let manager = manager(&desktop)?;
    let installation = manager.install(package).map_err(|e| e.to_string())?;
    PluginInstallation::from(manager, installation)
}

#[tauri::command]
pub async fn activate_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    package_digest: String,
    permissions: Vec<PermissionRequest>,
) -> Result<PluginInstallation, String> {
    let manager = manager(&desktop)?;
    if manager
        .package(&installation_id)
        .map_err(|e| e.to_string())?
        .digest
        != package_digest
    {
        return Err("stale_revision".into());
    }
    let installed = manager
        .activate(&installation_id, permissions)
        .map_err(|e| e.to_string())?;
    refresh(&desktop);
    PluginInstallation::from(manager, installed)
}

#[tauri::command]
pub async fn disable_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
) -> Result<PluginInstallation, String> {
    let manager = manager(&desktop)?;
    let result = manager.disable(&installation_id);
    refresh(&desktop); // even a failed persistent disable stays denied in memory
    PluginInstallation::from(manager, result.map_err(|e| e.to_string())?)
}

#[tauri::command]
pub async fn remove_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
) -> Result<(), String> {
    let result = manager(&desktop)?.remove(&installation_id);
    refresh(&desktop);
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    stage_id: String,
    package_digest: String,
) -> Result<PluginInstallation, String> {
    let package = desktop
        .plugins
        .consume(&stage_id, &package_digest)
        .map_err(|e| e.to_string())?;
    let manager = manager(&desktop)?;
    let result = manager
        .update(&installation_id, package)
        .map_err(|e| e.to_string())?;
    refresh(&desktop);
    PluginInstallation::from(manager, result)
}

#[tauri::command]
pub async fn rollback_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
) -> Result<PluginInstallation, String> {
    let manager = manager(&desktop)?;
    let result = manager
        .rollback(&installation_id)
        .map_err(|e| e.to_string())?;
    refresh(&desktop);
    PluginInstallation::from(manager, result)
}

#[tauri::command]
pub async fn get_plugin_themes(
    desktop: State<'_, Arc<Desktop>>,
) -> Result<Vec<PluginTheme>, String> {
    let mut themes = Vec::new();
    let manager = manager(&desktop)?;
    for installed in manager
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|i| i.grant.enabled)
    {
        let package = manager
            .package(&installed.grant.identity.installation_id)
            .map_err(|e| e.to_string())?;
        for contribution in &package.manifest.contributions.themes {
            let theme =
                ThemeV1::decode(&package.files[&contribution.file]).map_err(|e| e.to_string())?;
            themes.push(PluginTheme {
                theme_id: package.manifest.id.clone(),
                package_digest: package.digest.clone(),
                theme,
            });
        }
    }
    Ok(themes)
}

#[tauri::command]
pub async fn render_plugin_ui(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    contribution_id: String,
) -> Result<serde_json::Value, String> {
    let package = manager(&desktop)?
        .package(&installation_id)
        .map_err(|e| e.to_string())?;
    if !package
        .manifest
        .contributions
        .ui
        .iter()
        .any(|u| u.id == contribution_id)
    {
        return Err("permission_denied".into());
    }
    Err(PluginError::SandboxUnavailable.to_string())
}

#[tauri::command]
pub async fn invoke_plugin_ui_action(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    action: String,
    revision: String,
    fields: std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let _ = (desktop, installation_id, action, revision, fields);
    // No attested worker means no action table could have issued this handle.
    Err(PluginError::PermissionDenied.to_string())
}

pub fn recover_appearance(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Host-constant recovery script, never package code or text. It also
        // runs before a broken extension UI can subscribe to events.
        let _ = window.eval("document.documentElement.dataset.safeAppearance='true';document.getElementById('zroutery-plugin-theme')?.remove();delete document.documentElement.dataset.pluginTheme;");
    }
    let _ = app.emit("zroutery://safe-appearance", ());
}

#[tauri::command]
pub async fn enter_safe_appearance(app: AppHandle) -> Result<(), String> {
    recover_appearance(&app);
    Ok(())
}
