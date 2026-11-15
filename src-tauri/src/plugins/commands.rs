//! Typed management commands; paths come from a host file picker, never ESM.
use super::prune_action_intents;
use super::views::{PluginInspection, PluginInstallation, PluginTheme, PluginUiView};
#[cfg(target_os = "windows")]
use super::Watchdog;
use crate::state::Desktop;
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use zroutery_plugin_api::{
    capability::Scope,
    ipc::{
        self, Envelope, Locale, UiActionInput, UiInput, WorkerBootstrap, WorkerCall, WorkerReply,
    },
    theme::ThemeV1,
    ui::{Projection, UiEffect, UiNode},
    Capability, PermissionRequest, Principal, ResourceLimits,
};
#[cfg(target_os = "windows")]
use zroutery_plugin_host::sandbox;
use zroutery_plugin_host::{install::Package, lifecycle::Manager as PluginManager, supervisor};

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
    // One migration attempt per activation, after the grant is live. A failed
    // attempt keeps the previous envelope; options read as absent until a
    // later activation or an explicit save succeeds.
    migrate_plugin_options(&desktop, &installation_id);
    PluginInstallation::from(manager, installed)
}

#[tauri::command]
pub async fn disable_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
) -> Result<PluginInstallation, String> {
    let manager = manager(&desktop)?;
    let result = manager.disable(&installation_id);
    // In-memory revocation already happened; pending intents die with it.
    desktop.plugins.purge_actions(&installation_id);
    refresh(&desktop); // even a failed persistent disable stays denied in memory
    PluginInstallation::from(manager, result.map_err(|e| e.to_string())?)
}

#[tauri::command]
pub async fn remove_plugin(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
) -> Result<(), String> {
    let result = manager(&desktop)?.remove(&installation_id);
    desktop.plugins.purge_actions(&installation_id);
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
    desktop.plugins.purge_actions(&installation_id);
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
    desktop.plugins.purge_actions(&installation_id);
    refresh(&desktop);
    PluginInstallation::from(manager, result)
}

/// A real-user reset: the envelope becomes the current schema version's empty
/// options. The host owns the document, so reset never runs plugin code; it
/// is the one exit from a `Stale`/`Inactive` envelope that does not depend on
/// a working worker. Works for disabled installations too — it prepares clean
/// options for the next activation.
#[tauri::command]
pub async fn reset_plugin_options(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
) -> Result<(), String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let manager = manager(&desktop)?;
        let package = manager
            .package(&installation_id)
            .map_err(|e| e.to_string())?;
        let Some(schema) = package.schema.as_ref() else {
            return Err("invalid_result".into());
        };
        super::options::store(
            &desktop.config_dir,
            &installation_id,
            schema.schema_version,
            &BTreeMap::new(),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "plugin_crashed".to_string())?
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

fn collect_action_ids(node: &UiNode, out: &mut Vec<String>) {
    match node {
        UiNode::Stack { children } => children.iter().for_each(|c| collect_action_ids(c, out)),
        UiNode::Button { action_id, .. } => out.push(action_id.clone()),
        _ => {}
    }
}

fn worker_bootstrap(
    package: &Package,
    entry: &str,
    limits: ResourceLimits,
) -> std::result::Result<WorkerBootstrap, String> {
    let mut modules = BTreeMap::new();
    for (path, bytes) in &package.files {
        if path.ends_with(".mjs") || path.ends_with(".json") {
            let text = std::str::from_utf8(bytes).map_err(|_| "invalid_manifest".to_string())?;
            modules.insert(path.clone(), text.to_string());
        }
    }
    if !modules.contains_key(entry) {
        return Err("invalid_manifest".into());
    }
    Ok(WorkerBootstrap {
        entry: entry.to_string(),
        modules,
        limits,
        diagnostics: Vec::new(),
    })
}

fn run_worker_call(
    desktop: &Desktop,
    package: &Package,
    entry: &str,
    installation_id: &str,
    call: WorkerCall,
) -> std::result::Result<WorkerReply, String> {
    if desktop
        .plugins
        .safe_mode
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("sandbox_unavailable".into());
    }
    if supervisor::sandbox_status() != supervisor::SandboxStatus::Verified {
        return Err("sandbox_unavailable".into());
    }
    // Global admission: bounded concurrent workers even under a render flood.
    // The named binding holds the permit to the end of this scope; a plain
    // `_` would drop it before the launch.
    let _permit = desktop
        .plugins
        .supervisor
        .admit()
        .map_err(|_| "plugin_busy")?;
    let worker = desktop
        .plugins
        .worker_path
        .lock()
        .map_err(|_| "plugin_busy")?
        .clone()
        .ok_or("sandbox_unavailable")?;
    let manager = manager(desktop)?;
    let installation = manager
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|i| i.grant.identity.installation_id == installation_id)
        .ok_or("permission_denied")?;
    if !installation.grant.enabled {
        return Err("permission_denied".into());
    }
    let temp_root = desktop.config_dir.join("plugins").join("run");
    std::fs::create_dir_all(&temp_root).map_err(|_| "sandbox_unavailable")?;
    let work = temp_root.join(format!("run-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&work).map_err(|_| "sandbox_unavailable")?;

    let limits = package
        .manifest
        .limits
        .unwrap_or_default()
        .bounded()
        .map_err(|e| e.to_string())?;
    let bootstrap = worker_bootstrap(package, entry, limits)?;
    let envelope = Envelope {
        ipc_version: 1,
        call_id: uuid::Uuid::new_v4().to_string(),
        worker_generation: uuid::Uuid::new_v4().to_string(),
        grant_epoch: installation.grant.epoch.to_string(),
        value: call.clone(),
    };
    let expected_epoch = envelope.grant_epoch.clone();
    let expected_generation = envelope.worker_generation.clone();

    let result = run_sandboxed_process(
        &worker,
        &work,
        &bootstrap,
        &envelope,
        &expected_epoch,
        &expected_generation,
        limits.wall_ms,
    );
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// The restricted process and its private pipes exist for Windows builds.
#[cfg(target_os = "windows")]
fn run_sandboxed_process(
    worker: &std::path::Path,
    work: &std::path::Path,
    bootstrap: &WorkerBootstrap,
    envelope: &Envelope<WorkerCall>,
    expected_epoch: &str,
    expected_generation: &str,
    wall_ms: u64,
) -> std::result::Result<WorkerReply, String> {
    (|| {
        let mut process = sandbox::launch(worker, work, "").map_err(|e| e.to_string())?;
        // A wedged worker is terminated at the declared wall budget. The killed
        // job closes the private pipe, so the read below fails instead of
        // hanging a broker thread forever; the caller then reports a timeout.
        let watchdog = Watchdog::spawn(
            Duration::from_millis(wall_ms),
            process.watchdog().map_err(|e| e.to_string())?,
        );
        let outcome = (|| {
            ipc::write_frame(&mut process.input, bootstrap).map_err(|e| e.to_string())?;
            ipc::write_frame(&mut process.input, envelope).map_err(|e| e.to_string())?;
            let reply: Envelope<WorkerReply> =
                ipc::read_frame(&mut process.output).map_err(|e| e.to_string())?;
            let mut pending = supervisor::PendingCall::new(envelope.clone());
            pending
                .complete(reply, expected_epoch, expected_generation)
                .map_err(|e| e.to_string())
        })();
        let fired = watchdog.stand_down();
        let _ = process.wait(wall_ms.min(5000) as u32);
        // A terminated job surfaces as a broken pipe, which the IPC layer
        // reports as a crash; when the watchdog fired, the real breach was the
        // wall clock and the reply says so.
        match outcome {
            Err(error) if fired && error == "plugin_crashed" => Err("plugin_timeout".into()),
            outcome => outcome,
        }
    })()
}

/// Other hosts refuse the call with the same refusal the safe mode and the
/// sandbox gate produce: a plugin is never run outside the restricted process.
#[cfg(not(target_os = "windows"))]
fn run_sandboxed_process(
    _worker: &std::path::Path,
    _work: &std::path::Path,
    _bootstrap: &WorkerBootstrap,
    _envelope: &Envelope<WorkerCall>,
    _expected_epoch: &str,
    _expected_generation: &str,
    _wall_ms: u64,
) -> std::result::Result<WorkerReply, String> {
    Err("sandbox_unavailable".into())
}

fn default_locale() -> Locale {
    #[cfg(target_os = "windows")]
    {
        locale_from_langid(unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() })
    }
    #[cfg(not(target_os = "windows"))]
    {
        locale_from_environment(
            std::env::var("LC_ALL")
                .or_else(|_| std::env::var("LANG"))
                .ok()
                .as_deref(),
        )
    }
}

/// The tray reads the system the same way: it is drawn before any webview
/// exists. Plugin UI text selection follows the system language until the
/// composition layer passes an explicit per-view locale.
#[cfg(target_os = "windows")]
fn locale_from_langid(langid: u16) -> Locale {
    if langid & 0x3ff == 0x04 {
        // Sublanguage sits in bits 10-15: 1 zh-TW, 3 zh-HK, 5 zh-MO are
        // traditional; 0 neutral, 2 zh-CN and 4 zh-SG are simplified.
        match langid >> 10 {
            0x01 | 0x03 | 0x05 => Locale::ZhTw,
            _ => Locale::ZhCn,
        }
    } else {
        Locale::En
    }
}

#[cfg(not(target_os = "windows"))]
fn locale_from_environment(value: Option<&str>) -> Locale {
    let Some(value) = value else {
        return Locale::En;
    };
    let language = value
        .split(['.', '@'])
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase();
    match language.split('_').nth(1) {
        Some("tw") | Some("hk") | Some("mo") => Locale::ZhTw,
        Some("cn") | Some("sg") => Locale::ZhCn,
        _ if language.starts_with("zh") => Locale::ZhCn,
        _ => Locale::En,
    }
}

/// Field error codes travel back to the plugin's own UI as a bounded opaque
/// string. Codes come from the frozen host vocabulary plus package-declared
/// hook codes; option values are never echoed anywhere.
fn report_field_errors(errors: Vec<zroutery_plugin_api::ipc::FieldError>) -> String {
    let joined = errors
        .into_iter()
        .take(8)
        .map(|error| format!("{}:{}", error.field, error.message_code))
        .collect::<Vec<_>>()
        .join(",");
    if joined.is_empty() {
        "invalid_result".into()
    } else {
        format!("invalid_options:{joined}")
    }
}

/// One migration attempt per activation, per the frozen options contract: an
/// envelope version behind the package schema moves forward exactly once, and
/// any failure keeps the previous file untouched.
fn migrate_plugin_options(desktop: &Desktop, installation_id: &str) {
    let Ok(manager) = manager(desktop) else {
        return;
    };
    let Ok(package) = manager.package(installation_id) else {
        return;
    };
    let Some(schema) = package.schema.as_ref() else {
        return;
    };
    let super::options::StoredOptions::Stale { version, options } =
        super::options::load(&desktop.config_dir, installation_id, Some(schema))
    else {
        return;
    };
    let declares = |capability| {
        package.manifest.declares(
            capability,
            Principal::Control,
            Scope::contribution("$plugin"),
        )
    };
    if !declares(Capability::OptionsRead) || !declares(Capability::OptionsUpdate) {
        return;
    }
    let Some(control_entry) = package
        .manifest
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.entries.control.clone())
    else {
        return;
    };
    let Ok(reply) = run_worker_call(
        desktop,
        &package,
        &control_entry,
        installation_id,
        WorkerCall::MigrateOptions {
            input: ipc::OptionsMigrationInput {
                schema_version: version,
                revision: uuid::Uuid::new_v4().simple().to_string(),
                options,
                target_schema_version: schema.schema_version,
            },
        },
    ) else {
        return;
    };
    let WorkerReply::OptionsMigration { result } = reply else {
        return;
    };
    // The migrated shape passes the host schema before anything is stored.
    if !schema.validate_value(&result).is_empty() {
        return;
    }
    let hook_accepts = (|| {
        let Ok(reply) = run_worker_call(
            desktop,
            &package,
            &control_entry,
            installation_id,
            WorkerCall::ValidateOptions {
                input: ipc::OptionsInput {
                    schema_version: schema.schema_version,
                    revision: uuid::Uuid::new_v4().simple().to_string(),
                    options: result.clone(),
                },
            },
        ) else {
            return false;
        };
        matches!(
            reply,
            WorkerReply::OptionsValidation { result } if result.errors.is_empty()
        )
    })();
    if hook_accepts {
        let _ = super::options::store(
            &desktop.config_dir,
            installation_id,
            schema.schema_version,
            &result,
        );
    }
}

fn declared_action_kind(
    package: &Package,
    action_id: &str,
) -> Option<zroutery_plugin_api::ui::HostActionBinding> {
    package
        .manifest
        .contributions
        .actions
        .iter()
        .find(|a| a.id() == action_id)
        .cloned()
}

#[tauri::command]
pub async fn render_plugin_ui(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    contribution_id: String,
) -> Result<PluginUiView, String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let package = manager(&desktop)?
            .package(&installation_id)
            .map_err(|e| e.to_string())?;
        let ui = package
            .manifest
            .contributions
            .ui
            .iter()
            .find(|u| u.id == contribution_id)
            .ok_or("permission_denied")?
            .clone();
        let entry = package
            .manifest
            .runtime
            .as_ref()
            .and_then(|r| r.entries.ui.clone())
            .ok_or("sandbox_unavailable")?;
        let revision = format!("{}:{}", package.digest, uuid::Uuid::new_v4().simple());
        let reply = run_worker_call(
            &desktop,
            &package,
            &entry,
            &installation_id,
            WorkerCall::UiRender {
                hook: ui.render_hook.clone(),
                input: UiInput {
                    contribution_id: contribution_id.clone(),
                    locale: default_locale(),
                    view_revision: revision.clone(),
                    projection: Projection::default(),
                },
            },
        )?;
        let node = match reply {
            WorkerReply::UiTree { result } => result,
            WorkerReply::Error { code } => return Err(code.to_string()),
            _ => return Err("invalid_result".into()),
        };
        let declared_actions: Vec<String> = package
            .manifest
            .contributions
            .actions
            .iter()
            .map(|a| a.id().clone())
            .collect();
        node.validate(&declared_actions, &[])
            .map_err(|e| e.to_string())?;
        let mut ids = Vec::new();
        collect_action_ids(&node, &mut ids);
        let grant_epoch = manager(&desktop)?
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|i| i.grant.identity.installation_id == installation_id)
            .map(|i| i.grant.epoch.to_string())
            .ok_or("permission_denied")?;
        let mut actions = BTreeMap::new();
        let mut table = desktop.plugins.actions.lock().map_err(|_| "plugin_busy")?;
        // Expired intents from earlier views are dropped here; a bounded table
        // cannot be grown without end by repeated renders.
        if !prune_action_intents(&mut table) {
            return Err("plugin_busy".into());
        }
        for action_id in ids {
            if !declared_actions.contains(&action_id) {
                return Err("invalid_result".into());
            }
            let token = uuid::Uuid::new_v4().to_string();
            table.insert(
                token.clone(),
                super::PendingUiAction {
                    installation_id: installation_id.clone(),
                    contribution_id: contribution_id.clone(),
                    action_id: action_id.clone(),
                    revision: revision.clone(),
                    grant_epoch: grant_epoch.clone(),
                    issued_at: std::time::Instant::now(),
                },
            );
            actions.insert(action_id, token);
        }
        Ok(PluginUiView {
            installation_id,
            contribution_id,
            revision,
            node,
            actions,
        })
    })
    .await
    .map_err(|_| "plugin_crashed".to_string())?
}

#[tauri::command]
pub async fn invoke_plugin_ui_action(
    desktop: State<'_, Arc<Desktop>>,
    installation_id: String,
    action: String,
    revision: String,
    fields: std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let desktop = desktop.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let pending = {
            let mut table = desktop.plugins.actions.lock().map_err(|_| "plugin_busy")?;
            table.remove(&action).ok_or("permission_denied")?
        };
        if pending.installation_id != installation_id || pending.revision != revision {
            return Err("stale_revision".into());
        }
        let package = manager(&desktop)?
            .package(&installation_id)
            .map_err(|e| e.to_string())?;
        let installed = manager(&desktop)?
            .list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|i| i.grant.identity.installation_id == installation_id)
            .ok_or("permission_denied")?;
        if pending.grant_epoch != installed.grant.epoch.to_string() {
            return Err("stale_revision".into());
        }
        // A real-user action is a manifest-declared action the active grant
        // authorizes: host.action.request scoped to this UI contribution. The
        // declaration alone (activation preview) is not the authorization.
        let action_scope = Scope {
            action: Some(pending.action_id.as_str()),
            ..Scope::contribution(&pending.contribution_id)
        };
        if !installed
            .grant
            .permissions
            .iter()
            .any(|p| p.covers(Capability::HostActionRequest, Principal::Ui, action_scope))
        {
            return Err("permission_denied".into());
        }
        let ui = package
            .manifest
            .contributions
            .ui
            .iter()
            .find(|u| u.id == pending.contribution_id)
            .ok_or("permission_denied")?;
        let action_hook = ui.action_hook.clone().ok_or("permission_denied")?;
        let entry = package
            .manifest
            .runtime
            .as_ref()
            .and_then(|r| r.entries.ui.clone())
            .ok_or("sandbox_unavailable")?;
        let reply = run_worker_call(
            &desktop,
            &package,
            &entry,
            &installation_id,
            WorkerCall::UiAction {
                hook: action_hook,
                input: UiActionInput {
                    contribution_id: pending.contribution_id.clone(),
                    locale: default_locale(),
                    view_revision: revision.clone(),
                    projection: Projection::default(),
                    action: action.clone(),
                    action_id: pending.action_id.clone(),
                    fields,
                },
            },
        )?;
        let effect = match reply {
            WorkerReply::UiEffect { result } => result,
            WorkerReply::Error { code } => return Err(code.to_string()),
            _ => return Err("invalid_result".into()),
        };
        let declared = declared_action_kind(&package, &pending.action_id)
            .ok_or_else(|| "permission_denied".to_string())?;
        match effect {
            UiEffect::None => Ok(()),
            UiEffect::HostAction { action_id } if action_id == pending.action_id => {
                // Typed host broker actions: effect target must be the clicked
                // manifest-declared action. Unwired kinds remain fail-closed.
                match declared {
                    zroutery_plugin_api::ui::HostActionBinding::SaveOptions { .. } => {
                        // save_options host action with no options payload is a no-op
                        // acknowledgement; persistence happens via OptionsProposal.
                        Ok(())
                    }
                    _ => Err("unsupported_host_service".into()),
                }
            }
            UiEffect::OptionsProposal {
                expected_revision,
                options,
            } => {
                let zroutery_plugin_api::ui::HostActionBinding::SaveOptions { .. } = declared
                else {
                    return Err("permission_denied".into());
                };
                let has_update = package.manifest.permissions.iter().any(|p| {
                    matches!(
                        p,
                        zroutery_plugin_api::PermissionRequest::OptionsUpdate { .. }
                    )
                });
                if !has_update {
                    return Err("permission_denied".into());
                }
                // A SaveOptions action always declares a schema (manifest
                // contract); a proposal without one is a malformed surface.
                let Some(schema) = package.schema.as_ref() else {
                    return Err("invalid_result".into());
                };
                if expected_revision != revision {
                    return Err("stale_revision".into());
                }
                // The host schema is the authority; the hook only adds errors.
                let host_errors = schema.validate_value(&options);
                if !host_errors.is_empty() {
                    return Err(report_field_errors(host_errors));
                }
                let control_entry = package
                    .manifest
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.entries.control.clone())
                    .ok_or("invalid_result")?;
                let reply = run_worker_call(
                    &desktop,
                    &package,
                    &control_entry,
                    &installation_id,
                    WorkerCall::ValidateOptions {
                        input: ipc::OptionsInput {
                            schema_version: schema.schema_version,
                            revision: expected_revision.clone(),
                            options: options.clone(),
                        },
                    },
                )?;
                match reply {
                    WorkerReply::OptionsValidation { result } if result.errors.is_empty() => {}
                    WorkerReply::OptionsValidation { result } => {
                        return Err(report_field_errors(result.errors));
                    }
                    WorkerReply::Error { code } => return Err(code.to_string()),
                    _ => return Err("invalid_result".into()),
                }
                super::options::store(
                    &desktop.config_dir,
                    &installation_id,
                    schema.schema_version,
                    &options,
                )
                .map_err(|error| error.to_string())
            }
            _ => Err("permission_denied".into()),
        }
    })
    .await
    .map_err(|_| "plugin_crashed".to_string())?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn locale_maps_only_the_known_chinese_variants() {
        assert_eq!(locale_from_langid(0x0804), Locale::ZhCn); // zh-CN
        assert_eq!(locale_from_langid(0x1004), Locale::ZhCn); // zh-SG
        assert_eq!(locale_from_langid(0x0004), Locale::ZhCn); // neutral zh
        assert_eq!(locale_from_langid(0x0404), Locale::ZhTw); // zh-TW
        assert_eq!(locale_from_langid(0x0c04), Locale::ZhTw); // zh-HK
        assert_eq!(locale_from_langid(0x1404), Locale::ZhTw); // zh-MO
        assert_eq!(locale_from_langid(0x0409), Locale::En); // en-US
        assert_eq!(locale_from_langid(0x0407), Locale::En); // de-DE
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn locale_reads_posix_environment_without_guessing_a_script() {
        assert_eq!(locale_from_environment(Some("zh_CN.UTF-8")), Locale::ZhCn);
        assert_eq!(locale_from_environment(Some("zh_SG")), Locale::ZhCn);
        assert_eq!(locale_from_environment(Some("zh")), Locale::ZhCn);
        assert_eq!(locale_from_environment(Some("zh_TW.utf8")), Locale::ZhTw);
        assert_eq!(locale_from_environment(Some("zh_HK@latin")), Locale::ZhTw);
        assert_eq!(locale_from_environment(Some("en_US.UTF-8")), Locale::En);
        assert_eq!(locale_from_environment(Some("C")), Locale::En);
        assert_eq!(locale_from_environment(None), Locale::En);
    }
}
