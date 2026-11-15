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
