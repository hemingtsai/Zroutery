use serde::Serialize;
use std::collections::BTreeMap;
use zroutery_plugin_api::{
    manifest::PluginManifestV1, theme::ThemeV1, ui::UiNode, PermissionRequest,
};
use zroutery_plugin_host::{
    install::Package,
    lifecycle::{Installation, Manager},
    supervisor::{sandbox_status, SandboxStatus},
};

#[derive(Serialize)]
pub struct PluginReload {
    /// Whether a sandboxed worker executable was found beside the app. False
    /// means executable plugin facets stay denied until one is deployed.
    pub worker_found: bool,
    /// The fresh sandbox attestation: `verified` or `unavailable`.
    pub sandbox: SandboxStatus,
    /// AppContainer profiles swept from the ledger during this reload.
    pub cleaned_profiles: usize,
}

/// The reload path mirrors the startup wiring once: worker discovery, a fresh
/// sandbox attestation, and one AppContainer profile ledger sweep. The manager
/// state itself stays in memory — package installs and activations persist and
/// are re-read through the same manager.
pub(crate) fn reload_state(desktop: &crate::state::Desktop) -> PluginReload {
    let worker = crate::state::Desktop::discover_plugin_worker();
    let worker_found = worker.is_some();
    let probe_root = desktop.config_dir.join("plugins").join("probe");
    let sandbox = match &worker {
        Some(path) => {
            {
                let mut path_lock = desktop
                    .plugins
                    .worker_path
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                *path_lock = Some(path.clone());
            }
            zroutery_plugin_host::supervisor::verify_sandbox(path, &probe_root)
        }
        None => {
            {
                let mut path_lock = desktop
                    .plugins
                    .worker_path
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                *path_lock = None;
            }
            SandboxStatus::Unavailable
        }
    };
    let cleaned_profiles = super::commands::cleanup_profile_ledger(&desktop.config_dir);
    PluginReload {
        worker_found,
        sandbox,
        cleaned_profiles,
    }
}

#[derive(Serialize)]
pub struct PluginInstallation {
    pub installation_id: String,
    pub plugin_id: String,
    pub package_digest: String,
    pub version: String,
    pub enabled: bool,
    pub grant_epoch: String,
    pub permissions: Vec<PermissionRequest>,
    pub manifest: PluginManifestV1,
    pub sandbox_available: bool,
    pub previous_digest: Option<String>,
    pub diagnostic: Option<String>,
}
impl PluginInstallation {
    pub fn from(manager: &Manager, installation: Installation) -> Result<Self, String> {
        let package = manager
            .package(&installation.grant.identity.installation_id)
            .map_err(|e| e.to_string())?;
        let sandbox_available = sandbox_status() == SandboxStatus::Verified;
        let diagnostic = package
            .manifest
            .runtime
            .as_ref()
            .filter(|_| !sandbox_available)
            .map(|_| "sandbox_unavailable".into());
        Ok(Self {
            installation_id: installation.grant.identity.installation_id,
            plugin_id: installation.grant.identity.plugin_id,
            package_digest: installation.grant.identity.package_digest,
            version: installation.version,
            enabled: installation.grant.enabled,
            grant_epoch: installation.grant.epoch.to_string(),
            permissions: installation.grant.permissions,
            manifest: package.manifest.clone(),
            sandbox_available,
            previous_digest: installation.previous_digest,
            diagnostic,
        })
    }
}
#[derive(Serialize)]
pub struct PluginInspection {
    pub stage_id: String,
    pub package_digest: String,
    pub manifest: PluginManifestV1,
    pub added_permissions: Vec<PermissionRequest>,
    pub removed_permissions: Vec<PermissionRequest>,
}
impl PluginInspection {
    pub fn from(stage_id: String, package: &Package) -> Self {
        Self {
            stage_id,
            package_digest: package.digest.clone(),
            manifest: package.manifest.clone(),
            added_permissions: package.manifest.permissions.clone(),
            removed_permissions: vec![],
        }
    }
}
#[derive(Serialize)]
pub struct PluginTheme {
    pub theme_id: String,
    pub package_digest: String,
    pub theme: ThemeV1,
}

#[derive(Serialize)]
pub struct PluginUiView {
    pub installation_id: String,
    pub contribution_id: String,
    pub revision: String,
    pub node: UiNode,
    pub actions: BTreeMap<String, String>,
}
