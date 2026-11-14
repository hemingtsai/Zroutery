//! Authorization and send/revoke share one lock. Package names are not identities.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use zroutery_plugin_api::capability::Scope;
use zroutery_plugin_api::manifest::PluginManifestV1;
use zroutery_plugin_api::{Capability, PermissionRequest, PluginError, Principal, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationIdentity {
    pub installation_id: String,
    pub plugin_id: String,
    pub package_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub installation_id: String,
    pub provider_instance: String,
    pub account_generation: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRecord {
    pub identity: InstallationIdentity,
    pub epoch: u64,
    pub enabled: bool,
    pub permissions: Vec<PermissionRequest>,
}
#[derive(Debug, Default)]
struct State {
    grants: BTreeMap<String, GrantRecord>,
}
#[derive(Debug, Clone, Default)]
pub struct Authority {
    inner: Arc<Mutex<State>>,
}

/// An immutable Rust binding. Workers cannot deserialize or manufacture it.
#[derive(Debug, Clone)]
pub struct Binding {
    pub identity: InstallationIdentity,
    pub contribution: String,
    pub owner: Owner,
    pub grant_epoch: u64,
}

impl Authority {
    pub fn replace(&self, record: GrantRecord, manifest: &PluginManifestV1) -> Result<()> {
        manifest.validate()?;
        if record.identity.plugin_id != manifest.id
            || record.identity.installation_id != record.identity.installation_id.trim()
            || uuid::Uuid::parse_str(&record.identity.installation_id).is_err()
            || record
                .permissions
                .iter()
                .any(|p| !manifest.permissions.contains(p))
        {
            return Err(PluginError::PermissionDenied);
        }
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        if state
            .grants
            .get(&record.identity.installation_id)
            .is_some_and(|old| record.epoch <= old.epoch)
        {
            return Err(PluginError::StaleRevision);
        }
        state
            .grants
            .insert(record.identity.installation_id.clone(), record);
        Ok(())
    }
    pub fn bind(
        &self,
        identity: &InstallationIdentity,
        contribution: &str,
        owner: Owner,
    ) -> Result<Binding> {
        let state = self
            .inner
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        let record = state
            .grants
            .get(&identity.installation_id)
            .ok_or(PluginError::PermissionDenied)?;
        if !record.enabled
            || &record.identity != identity
            || owner.installation_id != identity.installation_id
        {
            return Err(PluginError::PermissionDenied);
        }
        Ok(Binding {
            identity: identity.clone(),
            contribution: contribution.into(),
            owner,
            grant_epoch: record.epoch,
        })
    }
    pub fn authorize(
        &self,
        binding: &Binding,
        principal: Principal,
        capability: Capability,
        scope: Scope<'_>,
    ) -> Result<()> {
        self.with_authorized(binding, principal, capability, scope, || Ok(()))
    }
    /// Ownership/epoch are checked BEFORE invoking a cache or keyring callback.
    /// A final send callback hands the future to transport under this same lock;
    /// it must be nonblocking and may not re-enter Authority.
    pub fn with_authorized<T>(
        &self,
        binding: &Binding,
        principal: Principal,
        capability: Capability,
        scope: Scope<'_>,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let state = self
            .inner
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        let record = state
            .grants
            .get(&binding.identity.installation_id)
            .ok_or(PluginError::PermissionDenied)?;
        if !record.enabled
            || record.identity != binding.identity
            || record.epoch != binding.grant_epoch
            || binding.owner.installation_id != binding.identity.installation_id
            || scope.contribution != binding.contribution
            || !record
                .permissions
                .iter()
                .any(|p| p.covers(capability, principal, scope))
        {
            return Err(PluginError::PermissionDenied);
        }
        operation()
    }
    /// Immediate in-memory denial even if durable publication subsequently fails.
    pub fn revoke(&self, installation: &str) -> Result<GrantRecord> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        let record = state
            .grants
            .get_mut(installation)
            .ok_or(PluginError::PermissionDenied)?;
        record.epoch = record
            .epoch
            .checked_add(1)
            .ok_or(PluginError::PermissionDenied)?;
        record.enabled = false;
        Ok(record.clone())
    }
    pub fn record(&self, installation: &str) -> Option<GrantRecord> {
        self.inner.lock().ok()?.grants.get(installation).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> PluginManifestV1 {
        PluginManifestV1::decode(r#"{
            "manifest_version":1,"id":"dev.test.one","version":"1.0.0",
            "name":{"en":"One","zh-CN":"一","zh-TW":"一"},"host_api":{"major":1,"min_minor":0},
            "runtime":null,"requires":[],"permissions":[{"capability":"theme.contribute","principal":"theme",
              "contributions":["theme"],"reason":{"en":"Appearance","zh-CN":"外观","zh-TW":"外觀"}}],
            "contributions":{"themes":[{"id":"theme","file":"theme.json"}]}}
        "#.as_bytes()).unwrap()
    }
    #[test]
    fn declaration_is_not_a_grant_and_revoke_invalidates_existing_binding() {
        let api = manifest();
        let authority = Authority::default();
        let identity = InstallationIdentity {
            installation_id: uuid::Uuid::new_v4().to_string(),
            plugin_id: api.id.clone(),
            package_digest: "digest".into(),
        };
        let owner = Owner {
            installation_id: identity.installation_id.clone(),
            provider_instance: "p".into(),
            account_generation: None,
        };
        let mut record = GrantRecord {
            identity: identity.clone(),
            epoch: 1,
            enabled: true,
            permissions: vec![],
        };
        authority.replace(record.clone(), &api).unwrap();
        let binding = authority.bind(&identity, "theme", owner.clone()).unwrap();
        assert!(authority
            .authorize(
                &binding,
                Principal::Theme,
                Capability::ThemeContribute,
                Scope::contribution("theme")
            )
            .is_err());
        record.epoch = 2;
        record.permissions = api.permissions.clone();
        authority.replace(record, &api).unwrap();
        let binding = authority.bind(&identity, "theme", owner).unwrap();
        authority
            .authorize(
                &binding,
                Principal::Theme,
                Capability::ThemeContribute,
                Scope::contribution("theme"),
            )
            .unwrap();
        assert!(authority
            .authorize(
                &binding,
                Principal::Ui,
                Capability::ThemeContribute,
                Scope::contribution("theme")
            )
            .is_err());
        authority.revoke(&identity.installation_id).unwrap();
        assert!(authority
            .with_authorized::<()>(
                &binding,
                Principal::Theme,
                Capability::ThemeContribute,
                Scope::contribution("theme"),
                || panic!("must not read a cache")
            )
            .is_err());
    }
}
