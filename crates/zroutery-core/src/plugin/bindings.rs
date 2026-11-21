use crate::config::PluginCredentialBinding;
use crate::error::{Error, Result};
use std::sync::Arc;
use std::task::Poll;
use zroutery_plugin_api::provider::HostOperationBinding;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotIdentity {
    pub slot_id: String,
    pub slot_uuid: String,
    pub credential_revision: u64,
    pub account_generation: Option<String>,
    pub key_ref: String,
}
impl From<&PluginCredentialBinding> for SlotIdentity {
    fn from(value: &PluginCredentialBinding) -> Self {
        Self {
            slot_id: value.slot_id.clone(),
            slot_uuid: value.slot_uuid.clone(),
            credential_revision: value.credential_revision,
            account_generation: value.account_generation.clone(),
            key_ref: value.key_ref.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
struct Identity {
    installation_id: String,
    provider_instance: String,
    contribution_id: String,
    package_digest: String,
    grant_epoch: u64,
    slots: Vec<SlotIdentity>,
    operations: Vec<HostOperationBinding>,
    transport_profile: Option<String>,
}
#[derive(Clone)]
pub struct PluginBinding {
    identity: Identity,
    port: Arc<dyn PluginPort>,
}
impl std::fmt::Debug for PluginBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PluginBinding")
            .field(&self.identity)
            .finish()
    }
}
impl PartialEq for PluginBinding {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

pub struct PluginSendScope<'a> {
    pub binding: &'a PluginBinding,
    pub operation: &'a HostOperationBinding,
    pub origin: &'a str,
}

/// Composition layer implements this against its broker and existing keyring.
/// The closure is polled under the broker's send/revoke synchronization boundary.
pub trait PluginPort: Send + Sync {
    fn authorize(&self, binding: &PluginBinding, operation: &HostOperationBinding) -> Result<()>;
    fn credential(
        &self,
        binding: &PluginBinding,
        slot: &SlotIdentity,
        operation: &HostOperationBinding,
    ) -> Result<Option<String>>;
    fn poll_send(
        &self,
        scope: PluginSendScope<'_>,
        send: &mut dyn FnMut() -> Poll<Result<reqwest::Response>>,
    ) -> Poll<Result<reqwest::Response>>;
    fn synthesize(&self, _config: &crate::config::AppConfig) -> Vec<crate::config::ProviderConfig> {
        Vec::new()
    }
}

impl PluginBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn verified(
        installation_id: String,
        provider_instance: String,
        contribution_id: String,
        package_digest: String,
        grant_epoch: u64,
        slots: Vec<SlotIdentity>,
        operations: Vec<HostOperationBinding>,
        transport_profile: Option<String>,
        port: Arc<dyn PluginPort>,
    ) -> Self {
        Self {
            identity: Identity {
                installation_id,
                provider_instance,
                contribution_id,
                package_digest,
                grant_epoch,
                slots,
                operations,
                transport_profile,
            },
            port,
        }
    }
    pub fn installation_id(&self) -> &str {
        &self.identity.installation_id
    }
    pub fn provider_instance(&self) -> &str {
        &self.identity.provider_instance
    }
    pub fn contribution_id(&self) -> &str {
        &self.identity.contribution_id
    }
    pub fn package_digest(&self) -> &str {
        &self.identity.package_digest
    }
    pub fn grant_epoch(&self) -> u64 {
        self.identity.grant_epoch
    }
    pub fn profile(&self) -> Option<&str> {
        self.identity.transport_profile.as_deref()
    }
    pub fn operations(&self) -> &[HostOperationBinding] {
        &self.identity.operations
    }
    pub fn slot_for_reference(&self, key_ref: &str) -> Result<&SlotIdentity> {
        self.identity
            .slots
            .iter()
            .find(|s| s.key_ref == key_ref)
            .ok_or_else(|| Error::NoCandidate("plugin credential ownership denied".into()))
    }
    pub fn slot_for_operation(
        &self,
        operation: &HostOperationBinding,
    ) -> Result<Option<&SlotIdentity>> {
        let Some(slot) = operation.credential_slots.first() else {
            return Ok(None);
        };
        self.identity
            .slots
            .iter()
            .find(|s| &s.slot_id == slot && s.account_generation.is_none())
            .map(Some)
            .ok_or_else(|| Error::NoCandidate("plugin credential is not bound".into()))
    }
    pub fn selected_slot(
        &self,
        key_ref: &str,
        operation: &HostOperationBinding,
    ) -> Result<Option<&SlotIdentity>> {
        if key_ref.is_empty() {
            return self.slot_for_operation(operation);
        }
        let slot = self.slot_for_reference(key_ref)?;
        if !operation.credential_slots.contains(&slot.slot_id) {
            return Err(Error::NoCandidate(
                "plugin slot was not declared for this operation".into(),
            ));
        }
        Ok(Some(slot))
    }
    pub fn operation_for_url(&self, url: &str) -> Result<&HostOperationBinding> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| Error::NoCandidate("plugin endpoint denied".into()))?;
        self.identity
            .operations
            .iter()
            .find(|op| {
                let Ok(base) = reqwest::Url::parse(&op.base_url) else {
                    return false;
                };
                if base.origin() != parsed.origin()
                    || parsed.query().is_some()
                    || parsed.fragment().is_some()
                {
                    return false;
                }
                let suffix = match op.service.id.as_str() {
                    "host.protocol.openai_chat" => "/chat/completions",
                    "host.protocol.openai_responses" => "/responses",
                    "host.protocol.anthropic" => "/messages",
                    "host.catalog.openai" | "host.catalog.anthropic" => "/models",
                    _ => return false,
                };
                let mut prefix = base.path().trim_end_matches('/').to_string();
                if !crate::config::versioned_suffix(&prefix) && !prefix.ends_with("/v1") {
                    prefix.push_str("/v1");
                }
                parsed.path() == format!("{prefix}{suffix}")
            })
            .ok_or_else(|| {
                Error::NoCandidate("plugin operation was not declared for this path".into())
            })
    }
    pub fn authorize(&self, operation: &HostOperationBinding) -> Result<()> {
        self.port.authorize(self, operation)
    }
    pub fn credential(
        &self,
        key_ref: &str,
        operation: &HostOperationBinding,
    ) -> Result<Option<String>> {
        let slot = self.slot_for_reference(key_ref)?;
        self.port.credential(self, slot, operation)
    }
    pub fn poll_send(
        &self,
        operation: &HostOperationBinding,
        origin: &str,
        send: &mut dyn FnMut() -> Poll<Result<reqwest::Response>>,
    ) -> Poll<Result<reqwest::Response>> {
        self.port.poll_send(
            PluginSendScope {
                binding: self,
                operation,
                origin,
            },
            send,
        )
    }
}
