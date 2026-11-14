//! Atomic durable publication. A failed disable write never re-enables memory.
use crate::grants::{Authority, GrantRecord, InstallationIdentity};
use crate::install::Package;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zroutery_plugin_api::{strict, PermissionRequest, PluginError, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub grant: GrantRecord,
    pub version: String,
    pub previous_digest: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    schema_version: u16,
    installations: BTreeMap<String, Installation>,
}
pub struct Manager {
    root: PathBuf,
    state: Mutex<RegistryFile>,
    pub authority: Authority,
    packages: Mutex<BTreeMap<String, Arc<Package>>>,
}
impl Manager {
    pub fn open(root: PathBuf) -> Result<Self> {
        let path = root.join("grants.json");
        let state = if path.exists() {
            let bytes = std::fs::read(&path).map_err(|_| PluginError::StorageUnavailable)?;
            let state: RegistryFile =
                strict::decode(&bytes, 1024 * 1024).map_err(|_| PluginError::StorageUnavailable)?;
            if state.schema_version != 1 {
                return Err(PluginError::StorageUnavailable);
            }
            state
        } else {
            RegistryFile {
                schema_version: 1,
                installations: BTreeMap::new(),
            }
        };
        let manager = Self {
            root,
            state: Mutex::new(state),
            authority: Authority::default(),
            packages: Mutex::new(BTreeMap::new()),
        };
        // Missing/tampered packages are unavailable, never native fallbacks.
        let installations: Vec<_> = manager
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?
            .installations
            .values()
            .cloned()
            .collect();
        for installation in installations {
            if let Ok(package) = manager.read_package(&installation.grant.identity.package_digest) {
                if manager
                    .authority
                    .replace(installation.grant.clone(), &package.manifest)
                    .is_ok()
                {
                    manager
                        .packages
                        .lock()
                        .map_err(|_| PluginError::StorageUnavailable)?
                        .insert(package.digest.clone(), Arc::new(package));
                }
            }
        }
        Ok(manager)
    }
    fn read_package(&self, digest: &str) -> Result<Package> {
        if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(PluginError::InvalidManifest);
        }
        let bytes = std::fs::read(self.root.join("packages").join(format!("{digest}.json")))
            .map_err(|_| PluginError::StorageUnavailable)?;
        let package = Package::from_archived_bytes(&bytes, false)?;
        if package.digest != digest {
            return Err(PluginError::InvalidManifest);
        }
        Ok(package)
    }
    pub fn install(&self, package: Package) -> Result<Installation> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?;
        if state
            .installations
            .values()
            .any(|i| i.grant.identity.plugin_id == package.manifest.id)
        {
            return Err(PluginError::PermissionDenied);
        }
        let identity = InstallationIdentity {
            installation_id: uuid::Uuid::new_v4().to_string(),
            plugin_id: package.manifest.id.clone(),
            package_digest: package.digest.clone(),
        };
        let installation = Installation {
            grant: GrantRecord {
                identity,
                epoch: 1,
                enabled: false,
                permissions: vec![],
            },
            version: package.manifest.version.clone(),
            previous_digest: None,
        };
        let bytes = package.archive_bytes()?;
        atomic_write(
            &self.root.join("packages"),
            &format!("{}.json", package.digest),
            &bytes,
        )?;
        state.installations.insert(
            installation.grant.identity.installation_id.clone(),
            installation.clone(),
        );
        if let Err(error) = self.save(&state) {
            state
                .installations
                .remove(&installation.grant.identity.installation_id);
            return Err(error);
        }
        self.authority
            .replace(installation.grant.clone(), &package.manifest)?;
        self.packages
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?
            .insert(package.digest.clone(), Arc::new(package));
        Ok(installation)
    }
    pub fn list(&self) -> Result<Vec<Installation>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?
            .installations
            .values()
            .cloned()
            .collect())
    }
    pub fn package(&self, installation_id: &str) -> Result<Arc<Package>> {
        let state = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?;
        let installation = state
            .installations
            .get(installation_id)
            .ok_or(PluginError::PermissionDenied)?;
        self.packages
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?
            .get(&installation.grant.identity.package_digest)
            .cloned()
            .ok_or(PluginError::InvalidManifest)
    }
    pub fn activate(
        &self,
        installation_id: &str,
        permissions: Vec<PermissionRequest>,
    ) -> Result<Installation> {
        let package = self.package(installation_id)?;
        if permissions
            .iter()
            .any(|p| !package.manifest.permissions.contains(p))
            || package
                .manifest
                .permissions
                .iter()
                .any(|p| !p.optional() && !permissions.contains(p))
        {
            return Err(PluginError::PermissionDenied);
        }
        // Published declarations are Rust-held, so an unavailable UI/control
        // worker disables only that facet, not a data contribution. The worker
        // port remains fail-closed independently; runtime-only packages cannot
        // pretend they are usable without a verified sandbox.
        if package.manifest.runtime.is_some()
            && package.manifest.contributions.providers.is_empty()
            && package.manifest.contributions.clients.is_empty()
            && package.manifest.contributions.themes.is_empty()
            && crate::supervisor::sandbox_status() != crate::supervisor::SandboxStatus::Verified
        {
            return Err(PluginError::SandboxUnavailable);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?;
        let old = state
            .installations
            .get(installation_id)
            .cloned()
            .ok_or(PluginError::PermissionDenied)?;
        let mut next = old.clone();
        next.grant.epoch = next
            .grant
            .epoch
            .checked_add(1)
            .ok_or(PluginError::PermissionDenied)?;
        next.grant.enabled = true;
        next.grant.permissions = permissions;
        state
            .installations
            .insert(installation_id.into(), next.clone());
        if let Err(error) = self.save(&state) {
            state.installations.insert(installation_id.into(), old);
            return Err(error);
        }
        self.authority
            .replace(next.grant.clone(), &package.manifest)?;
        Ok(next)
    }
    pub fn disable(&self, installation_id: &str) -> Result<Installation> {
        let revoked = self.authority.revoke(installation_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?;
        let next = state
            .installations
            .get_mut(installation_id)
            .ok_or(PluginError::PermissionDenied)?;
        next.grant = revoked;
        let result = next.clone();
        self.save(&state)?;
        Ok(result)
    }
    pub fn remove(&self, installation_id: &str) -> Result<()> {
        self.disable(installation_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?;
        let removed = state
            .installations
            .remove(installation_id)
            .ok_or(PluginError::PermissionDenied)?;
        if let Err(error) = self.save(&state) {
            state.installations.insert(installation_id.into(), removed);
            return Err(error);
        }
        // Archive/credentials/preferences survive: removal is not secret deletion.
        Ok(())
    }
    /// Updates publish disabled with an empty grant. Same ID/version/publisher
    /// never transfers authority to new bytes; explicit approval follows preview.
    pub fn update(&self, installation_id: &str, package: Package) -> Result<Installation> {
        let old_package = self.package(installation_id)?;
        if old_package.manifest.id != package.manifest.id || old_package.digest == package.digest {
            return Err(PluginError::InvalidManifest);
        }
        if old_package.manifest.options_schema != package.manifest.options_schema {
            return Err(PluginError::InvalidManifest);
        }
        self.disable(installation_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?;
        let old = state
            .installations
            .get(installation_id)
            .cloned()
            .ok_or(PluginError::PermissionDenied)?;
        atomic_write(
            &self.root.join("packages"),
            &format!("{}.json", package.digest),
            &package.archive_bytes()?,
        )?;
        let mut next = old.clone();
        next.previous_digest = Some(old.grant.identity.package_digest.clone());
        next.grant.identity.package_digest = package.digest.clone();
        next.grant.epoch += 1;
        next.grant.enabled = false;
        next.grant.permissions.clear();
        next.version = package.manifest.version.clone();
        state
            .installations
            .insert(installation_id.into(), next.clone());
        if let Err(error) = self.save(&state) {
            state.installations.insert(installation_id.into(), old);
            return Err(error);
        }
        self.authority
            .replace(next.grant.clone(), &package.manifest)?;
        self.packages
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?
            .insert(package.digest.clone(), Arc::new(package));
        Ok(next)
    }
    pub fn rollback(&self, installation_id: &str) -> Result<Installation> {
        let digest = self
            .state
            .lock()
            .map_err(|_| PluginError::StorageUnavailable)?
            .installations
            .get(installation_id)
            .and_then(|i| i.previous_digest.clone())
            .ok_or(PluginError::StaleRevision)?;
        let package = self.read_package(&digest)?;
        self.update(installation_id, package)
    }
    fn save(&self, state: &RegistryFile) -> Result<()> {
        atomic_write(
            &self.root,
            "grants.json",
            &serde_json::to_vec(state).map_err(|_| PluginError::StorageUnavailable)?,
        )
    }
}

pub fn atomic_write(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|_| PluginError::StorageUnavailable)?;
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|_| PluginError::StorageUnavailable)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| PluginError::StorageUnavailable)?;
        drop(file);
        std::fs::rename(&tmp, dir.join(name)).map_err(|_| PluginError::StorageUnavailable)?;
        #[cfg(unix)]
        {
            std::fs::File::open(dir)
                .and_then(|file| file.sync_all())
                .map_err(|_| PluginError::StorageUnavailable)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
