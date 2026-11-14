//! Desktop/headless composition: plugin broker + existing Core/keyring services.
//! No dynamic code executes in the webview or in Core.
#[cfg(feature = "plugin")]
pub(crate) mod bindings;
pub mod commands;
pub mod credentials;
#[cfg(feature = "plugin")]
pub(crate) mod scoped_secrets;
pub mod views;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use zroutery_plugin_api::{PluginError, Result};
use zroutery_plugin_host::{install::Package, lifecycle::Manager};

pub struct PluginState {
    pub manager: Result<Manager>,
    stages: Mutex<BTreeMap<String, Package>>,
    #[cfg(feature = "plugin")]
    pub(crate) ownership: scoped_secrets::Ownership,
    pub safe_mode: std::sync::atomic::AtomicBool,
}
impl PluginState {
    pub fn new(root: &Path) -> Self {
        Self {
            manager: Manager::open(root.join("plugins")),
            stages: Mutex::new(BTreeMap::new()),
            safe_mode: std::sync::atomic::AtomicBool::new(
                std::env::args().any(|arg| arg == "--disable-plugins"),
            ),
            #[cfg(feature = "plugin")]
            ownership: scoped_secrets::Ownership::open(root.join("plugins")),
        }
    }
    pub fn stage(&self, package: Package) -> Result<String> {
        let mut stages = self.stages.lock().map_err(|_| PluginError::PluginBusy)?;
        if stages.len() >= 8 {
            return Err(PluginError::PluginBusy);
        }
        let id = uuid::Uuid::new_v4().to_string();
        stages.insert(id.clone(), package);
        Ok(id)
    }
    pub fn consume(&self, stage_id: &str, digest: &str) -> Result<Package> {
        let mut stages = self
            .stages
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        if stages.get(stage_id).is_none_or(|p| p.digest != digest) {
            return Err(PluginError::StaleRevision);
        }
        stages.remove(stage_id).ok_or(PluginError::StaleRevision)
    }
}

pub fn bundled_themes() -> Result<Vec<Package>> {
    let bytes = [
        (
            include_bytes!("../../../plugins/theme-md3/zroutery.plugin.json").as_slice(),
            include_bytes!("../../../plugins/theme-md3/theme.json").as_slice(),
        ),
        (
            include_bytes!("../../../plugins/theme-fluent/zroutery.plugin.json").as_slice(),
            include_bytes!("../../../plugins/theme-fluent/theme.json").as_slice(),
        ),
    ];
    bytes
        .into_iter()
        .map(|(manifest, theme)| {
            Package::from_files(
                BTreeMap::from([
                    ("zroutery.plugin.json".into(), manifest.to_vec()),
                    ("theme.json".into(), theme.to_vec()),
                ]),
                true,
            )
        })
        .collect()
}
