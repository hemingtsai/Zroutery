//! Non-secret ownership records. Keyring names alone are not an ACL.
use super::PluginState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use zroutery_core::{
    config::PluginCredentialBinding,
    error::{Error, Result},
    plugin::SlotIdentity,
};
use zroutery_plugin_host::grants::{Binding, Owner};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedSlot {
    pub owner: Owner,
    pub contribution_id: String,
    pub slot: PluginCredentialBinding,
    pub published: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipFile {
    schema_version: u16,
    records: BTreeMap<String, OwnedSlot>,
}
pub struct Ownership {
    root: PathBuf,
    pub records: Mutex<BTreeMap<String, OwnedSlot>>,
    pub available: bool,
}
impl Ownership {
    pub fn open(root: PathBuf) -> Self {
        let result = if root.join("ownership.json").exists() {
            std::fs::read(root.join("ownership.json"))
                .map_err(|_| ())
                .and_then(|bytes| {
                    let state: OwnershipFile =
                        zroutery_plugin_api::strict::decode(&bytes, 1024 * 1024).map_err(|_| ())?;
                    if state.schema_version != 1 || state.records.len() > 4096 {
                        return Err(());
                    }
                    Ok(state.records)
                })
        } else {
            Ok(BTreeMap::new())
        };
        let available = result.is_ok();
        Self {
            root,
            records: Mutex::new(result.unwrap_or_default()),
            available,
        }
    }
    pub fn save(&self, records: &BTreeMap<String, OwnedSlot>) -> std::result::Result<(), String> {
        if !self.available {
            return Err("plugin credential ownership storage unavailable".into());
        }
        let bytes = serde_json::to_vec(&OwnershipFile {
            schema_version: 1,
            records: records.clone(),
        })
        .map_err(|_| "credential_unavailable".to_string())?;
        zroutery_plugin_host::lifecycle::atomic_write(&self.root, "ownership.json", &bytes)
            .map_err(|e| e.to_string())
    }
}
pub fn verify_slot(plugins: &PluginState, binding: &Binding, slot: &SlotIdentity) -> Result<()> {
    let records = plugins
        .ownership
        .records
        .lock()
        .map_err(|_| Error::NoCandidate("plugin credential denied".into()))?;
    if !plugins.ownership.available
        || !records.get(&slot.slot_uuid).is_some_and(|record| {
            record.published
                && record.owner == binding.owner
                && record.contribution_id == binding.contribution
                && SlotIdentity::from(&record.slot) == *slot
                && record.slot.key_ref == format!("plugin:{}", record.slot.slot_uuid)
                && records
                    .values()
                    .filter(|other| {
                        other.published
                            && other.owner == record.owner
                            && other.slot.slot_id == record.slot.slot_id
                    })
                    .all(|other| other.slot.credential_revision <= record.slot.credential_revision)
        })
    {
        return Err(Error::NoCandidate("plugin credential denied".into()));
    }
    Ok(())
}

/// Host-only secret form path. It reuses the existing durable inference
/// publication journal, staging reference, config-save lock and compensation.
impl crate::state::Desktop {
    fn plugin_slot_owner(
        &self,
        instance_id: &str,
        slot_id: &str,
        account_id: Option<&str>,
    ) -> std::result::Result<(Binding, Option<PluginCredentialBinding>), String> {
        let config = self.core.config();
        let instance = config
            .plugin_instances
            .iter()
            .find(|i| i.id == instance_id)
            .ok_or("permission_denied")?;
        let manager = self.plugins.manager.as_ref().map_err(|e| e.to_string())?;
        let package = manager
            .package(&instance.installation_id)
            .map_err(|e| e.to_string())?;
        let descriptor = package
            .manifest
            .contributions
            .providers
            .iter()
            .find(|p| p.id == instance.contribution_id)
            .ok_or("permission_denied")?;
        let declared = descriptor
            .credentials
            .iter()
            .find(|s| s.id == slot_id)
            .ok_or("permission_denied")?;
        if (declared.scope == zroutery_plugin_api::provider::CredentialScope::Account)
            != account_id.is_some()
        {
            return Err("permission_denied".into());
        }
        let account_generation = account_id
            .map(|id| {
                instance
                    .accounts
                    .iter()
                    .find(|a| a.account_id == id)
                    .and_then(|a| a.binding.as_ref())
                    .map(|b| b.generation.clone())
                    .ok_or("permission_denied")
            })
            .transpose()?;
        let owner = Owner {
            installation_id: instance.installation_id.clone(),
            provider_instance: instance.id.clone(),
            account_generation: account_generation.clone(),
        };
        let binding = manager
            .authority
            .bind(
                &zroutery_plugin_host::grants::InstallationIdentity {
                    installation_id: instance.installation_id.clone(),
                    plugin_id: package.manifest.id.clone(),
                    package_digest: package.digest.clone(),
                },
                &instance.contribution_id,
                owner,
            )
            .map_err(|e| e.to_string())?;
        let slot = instance
            .credentials
            .iter()
            .find(|s| s.slot_id == slot_id && s.account_generation == account_generation)
            .cloned();
        Ok((binding, slot))
    }

    pub fn plugin_slot_status(
        &self,
        instance_id: &str,
        slot_id: &str,
        account_id: Option<&str>,
    ) -> std::result::Result<super::credentials::CredentialStatus, String> {
        let (binding, slot) = self.plugin_slot_owner(instance_id, slot_id, account_id)?;
        let manager = self.plugins.manager.as_ref().map_err(|e| e.to_string())?;
        let scope = zroutery_plugin_api::capability::Scope {
            slot: Some(slot_id),
            ..zroutery_plugin_api::capability::Scope::contribution(&binding.contribution)
        };
        manager
            .authority
            .authorize(
                &binding,
                zroutery_plugin_api::Principal::Ui,
                zroutery_plugin_api::Capability::CredentialStatus,
                scope,
            )
            .map_err(|e| e.to_string())?;
        let Some(slot) = slot else {
            return Ok(super::credentials::CredentialStatus {
                present: false,
                state: "missing",
            });
        };
        verify_slot(&self.plugins, &binding, &SlotIdentity::from(&slot))
            .map_err(|_| "permission_denied")?;
        // Ownership is checked before backend/cache access; other owners have the
        // same denial whether their slot exists or not.
        let status = match self.secrets.get_keyring_plugin(&slot.key_ref) {
            Ok(value) => super::credentials::CredentialStatus {
                present: !value.is_empty(),
                state: "ready",
            },
            Err(crate::secrets::StoreError::NoEntry) => super::credentials::CredentialStatus {
                present: false,
                state: "missing",
            },
            Err(crate::secrets::StoreError::Locked) => super::credentials::CredentialStatus {
                present: false,
                state: "locked",
            },
            Err(crate::secrets::StoreError::Backend(_)) => super::credentials::CredentialStatus {
                present: false,
                state: "unavailable",
            },
        };
        Ok(status)
    }

    pub async fn clear_plugin_slot(
        &self,
        instance_id: &str,
        slot_id: &str,
        expected_revision: &str,
        account_id: Option<&str>,
    ) -> std::result::Result<(), String> {
        let _migration = self.migration.lock().await;
        let before = self.core.config();
        if crate::state::config_revision(&before) != expected_revision {
            return Err("stale_revision".into());
        }
        let (binding, slot) = self.plugin_slot_owner(instance_id, slot_id, account_id)?;
        let manager = self.plugins.manager.as_ref().map_err(|e| e.to_string())?;
        manager
            .authority
            .authorize(
                &binding,
                zroutery_plugin_api::Principal::HostBinding,
                zroutery_plugin_api::Capability::CredentialClear,
                zroutery_plugin_api::capability::Scope {
                    slot: Some(slot_id),
                    ..zroutery_plugin_api::capability::Scope::contribution(&binding.contribution)
                },
            )
            .map_err(|e| e.to_string())?;
        let Some(slot) = slot else {
            return Ok(());
        };
        verify_slot(&self.plugins, &binding, &SlotIdentity::from(&slot))
            .map_err(|_| "permission_denied")?;
        let mut next = (*before).clone();
        let instance = next
            .plugin_instances
            .iter_mut()
            .find(|i| i.id == instance_id)
            .ok_or("permission_denied")?;
        instance
            .credentials
            .retain(|s| s.slot_uuid != slot.slot_uuid);
        for account in &mut instance.accounts {
            account.inference_keys.retain(|k| k.key_ref != slot.key_ref);
        }
        crate::store::save(&self.config_dir, &next)?;
        {
            let mut records = self
                .plugins
                .ownership
                .records
                .lock()
                .map_err(|_| "storage_unavailable")?;
            if let Some(record) = records.get_mut(&slot.slot_uuid) {
                record.published = false;
            }
            self.plugins.ownership.save(&records)?;
        }
        self.core.set_config(next.clone());
        if !crate::secret_refs::referenced_secrets(&next).contains(&slot.key_ref) {
            self.secrets
                .delete_keyring_plugin(&slot.key_ref)
                .map_err(|_| "credential_unavailable")?;
        }
        Ok(())
    }

    pub(crate) fn recover_plugin_slots(&self) -> std::result::Result<(), String> {
        if !self.plugins.ownership.available {
            return Err("storage_unavailable".into());
        }
        let config = self.core.config();
        let live = crate::secret_refs::referenced_secrets(&config);
        let runtime = self.runtime.as_ref().ok_or("storage_unavailable")?;
        let mut records = self
            .plugins
            .ownership
            .records
            .lock()
            .map_err(|_| "storage_unavailable")?;
        for record in records.values_mut() {
            if uuid::Uuid::parse_str(&record.slot.slot_uuid).is_err()
                || record.slot.key_ref != format!("plugin:{}", record.slot.slot_uuid)
                || record.slot.credential_revision == 0
            {
                return Err("plugin credential ownership record invalid".into());
            }
            let referenced = live.contains(&record.slot.key_ref);
            if referenced
                && !config.plugin_instances.iter().any(|instance| {
                    instance.installation_id == record.owner.installation_id
                        && instance.id == record.owner.provider_instance
                        && instance.contribution_id == record.contribution_id
                        && instance.credentials.contains(&record.slot)
                })
            {
                return Err("plugin credential owner mismatch".into());
            }
            record.published = referenced;
            if referenced {
                runtime.complete_inference_publication(&record.slot.key_ref)?;
            } else {
                let pending = runtime
                    .inference_publication(&record.slot.key_ref)?
                    .is_some_and(|p| p.phase == "pending");
                // Only compensate an unpublished transaction. Removing a
                // package/instance does not itself authorize secret deletion.
                if pending
                    && self
                        .secrets
                        .delete_keyring_plugin(&record.slot.key_ref)
                        .is_ok()
                {
                    runtime.discard_inference_publication(&record.slot.key_ref)?;
                }
            }
        }
        self.plugins.ownership.save(&records)
    }

    pub async fn store_plugin_slot(
        &self,
        instance_id: &str,
        slot_id: &str,
        secret: &str,
        expected_revision: &str,
        account_id: Option<&str>,
    ) -> std::result::Result<(), String> {
        let _migration = self.migration.lock().await;
        let before = self.core.config();
        if crate::state::config_revision(&before) != expected_revision {
            return Err("stale_revision".into());
        }
        if secret.is_empty() {
            return Err("credential_unavailable: empty secret".into());
        }
        if secret.len() > 4096 {
            return Err("credential_unavailable: secret exceeds 4096 bytes".into());
        }
        if secret.contains('\0') {
            return Err("credential_unavailable: secret contains a NUL byte".into());
        }
        let mut next = (*before).clone();
        let instance = next
            .plugin_instances
            .iter_mut()
            .find(|i| i.id == instance_id)
            .ok_or("permission_denied")?;
        let manager = self.plugins.manager.as_ref().map_err(|e| e.to_string())?;
        let package = manager
            .package(&instance.installation_id)
            .map_err(|e| e.to_string())?;
        let descriptor = package
            .manifest
            .contributions
            .providers
            .iter()
            .find(|p| p.id == instance.contribution_id)
            .ok_or("permission_denied")?;
        let slot = descriptor
            .credentials
            .iter()
            .find(|s| s.id == slot_id)
            .ok_or("permission_denied")?;
        if (slot.scope == zroutery_plugin_api::provider::CredentialScope::Account)
            != account_id.is_some()
        {
            return Err("permission_denied".into());
        }
        if !matches!(
            slot.kind,
            zroutery_plugin_api::provider::CredentialKind::ApiKey
                | zroutery_plugin_api::provider::CredentialKind::AppToken
        ) || !matches!(
            slot.acquisition,
            zroutery_plugin_api::provider::Acquisition::HostSecretForm
        ) {
            return Err("unsupported_host_service".into());
        }
        let account_generation = account_id
            .map(|id| {
                instance
                    .accounts
                    .iter()
                    .find(|a| a.account_id == id)
                    .and_then(|a| a.binding.as_ref())
                    .map(|b| b.generation.clone())
                    .ok_or("permission_denied")
            })
            .transpose()?;
        let owner = Owner {
            installation_id: instance.installation_id.clone(),
            provider_instance: instance.id.clone(),
            account_generation: account_generation.clone(),
        };
        let binding = manager
            .authority
            .bind(
                &zroutery_plugin_host::grants::InstallationIdentity {
                    installation_id: instance.installation_id.clone(),
                    plugin_id: package.manifest.id.clone(),
                    package_digest: package.digest.clone(),
                },
                &instance.contribution_id,
                owner.clone(),
            )
            .map_err(|e| e.to_string())?;
        let scope = zroutery_plugin_api::capability::Scope {
            slot: Some(slot_id),
            ..zroutery_plugin_api::capability::Scope::contribution(&instance.contribution_id)
        };
        manager
            .authority
            .authorize(
                &binding,
                zroutery_plugin_api::Principal::HostBinding,
                zroutery_plugin_api::Capability::CredentialStore,
                scope,
            )
            .map_err(|e| e.to_string())?;
        let previous = instance
            .credentials
            .iter()
            .find(|s| s.slot_id == slot_id && s.account_generation == account_generation)
            .cloned();
        let uuid = uuid::Uuid::new_v4().to_string();
        let staged = PluginCredentialBinding {
            slot_id: slot_id.into(),
            slot_uuid: uuid.clone(),
            key_ref: format!("plugin:{uuid}"),
            account_generation,
            credential_revision: previous
                .as_ref()
                .map(|s| s.credential_revision)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("credential_unavailable")?,
        };
        let runtime = self.runtime.as_ref().ok_or("storage_unavailable")?;
        runtime.journal_inference_publication(
            &zroutery_core::runtime_store::InferencePublication::pending(
                &format!("plugin:{}:{}", instance.installation_id, instance.id),
                account_id.unwrap_or("$provider"),
                slot_id,
                &staged.key_ref,
            ),
        )?;
        let record = OwnedSlot {
            owner,
            contribution_id: instance.contribution_id.clone(),
            slot: staged.clone(),
            published: false,
        };
        {
            let mut records = self
                .plugins
                .ownership
                .records
                .lock()
                .map_err(|_| "storage_unavailable")?;
            records.insert(uuid.clone(), record);
            self.plugins.ownership.save(&records)?;
        }
        let secrets = Arc::clone(&self.secrets);
        let reference = staged.key_ref.clone();
        let secret = secret.to_string();
        let stored = tauri::async_runtime::spawn_blocking(move || {
            secrets.set_keyring_plugin(&reference, &secret)
        })
        .await
        .map_err(|_| "credential_unavailable: worker pool join failed".to_string())?;
        stored.map_err(|error| format!("credential_unavailable: {error}"))?;
        instance.credentials.retain(|s| {
            !(s.slot_id == slot_id && s.account_generation == staged.account_generation)
        });
        instance.credentials.push(staged.clone());
        if let Some(account_id) = account_id {
            let account = instance
                .accounts
                .iter_mut()
                .find(|a| a.account_id == account_id)
                .ok_or("permission_denied")?;
            account.inference_keys.retain(|k| k.id != slot_id);
            account
                .inference_keys
                .push(zroutery_core::config::InferenceKeyConfig {
                    id: slot_id.into(),
                    key_ref: staged.key_ref.clone(),
                    enabled: true,
                    priority: 0,
                    note: String::new(),
                });
        }
        next.schema_version = zroutery_core::config::CONFIG_SCHEMA_VERSION;
        if let Err(error) = crate::store::save(&self.config_dir, &next) {
            // Save failure cannot replay an external auth action. This path only
            // stored user input, so compensate exactly its own staging UUID.
            if self.secrets.delete_keyring_plugin(&staged.key_ref).is_ok() {
                runtime.discard_inference_publication(&staged.key_ref)?;
            }
            return Err(error);
        }
        {
            let mut records = self
                .plugins
                .ownership
                .records
                .lock()
                .map_err(|_| "storage_unavailable")?;
            if let Some(previous) = &previous {
                if let Some(record) = records.get_mut(&previous.slot_uuid) {
                    record.published = false;
                }
            }
            if let Some(record) = records.get_mut(&uuid) {
                record.published = true;
            }
            self.plugins.ownership.save(&records)?;
        }
        self.core.set_config(next);
        runtime.complete_inference_publication(&staged.key_ref)?;
        // Keep old references until all live/pending references permit cleanup;
        // clearing is an independently authorized host action, not store/use.
        Ok(())
    }
}
