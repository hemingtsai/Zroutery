//! Scoped authorizations are rechecked for every operation/cache/send poll.
use super::PluginState;
use crate::secrets::KeychainSecrets;
use std::sync::Arc;
use std::task::Poll;
use zroutery_core::{
    config::{AppConfig, ProviderConfig},
    error::{Error, Result},
    plugin::{PluginBinding, PluginPort, PluginSendScope, SlotIdentity},
};
use zroutery_plugin_api::{
    capability::Scope, provider::HostOperationBinding, Capability, Principal,
};
use zroutery_plugin_host::grants::{Binding, InstallationIdentity, Owner};

pub struct HostBindings {
    plugins: Arc<PluginState>,
    secrets: Arc<KeychainSecrets>,
}
impl HostBindings {
    pub fn new(plugins: Arc<PluginState>, secrets: Arc<KeychainSecrets>) -> Self {
        Self { plugins, secrets }
    }
    fn host_binding(&self, binding: &PluginBinding, account_generation: Option<String>) -> Binding {
        Binding {
            identity: InstallationIdentity {
                installation_id: binding.installation_id().into(),
                plugin_id: self
                    .plugins
                    .manager
                    .as_ref()
                    .ok()
                    .and_then(|m| m.package(binding.installation_id()).ok())
                    .map(|p| p.manifest.id.clone())
                    .unwrap_or_default(),
                package_digest: binding.package_digest().into(),
            },
            contribution: binding.contribution_id().into(),
            grant_epoch: binding.grant_epoch(),
            owner: Owner {
                installation_id: binding.installation_id().into(),
                provider_instance: binding.provider_instance().into(),
                account_generation,
            },
        }
    }
    fn operation_scope<'a>(
        binding: &'a PluginBinding,
        operation: &'a HostOperationBinding,
    ) -> Result<Scope<'a>> {
        let origin =
            zroutery_plugin_api::manifest::static_origin(&operation.base_url).ok_or_else(denied)?;
        Ok(Scope {
            operation: Some(&operation.id),
            origin: Some(origin),
            ..Scope::contribution(binding.contribution_id())
        })
    }
}
fn denied() -> Error {
    Error::NoCandidate("plugin permission denied".into())
}
impl PluginPort for HostBindings {
    fn authorize(&self, binding: &PluginBinding, operation: &HostOperationBinding) -> Result<()> {
        if self
            .plugins
            .safe_mode
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(denied());
        }
        let manager = self.plugins.manager.as_ref().map_err(|_| denied())?;
        manager
            .authority
            .authorize(
                &self.host_binding(binding, None),
                Principal::HostBinding,
                Capability::HostOperationBind,
                Self::operation_scope(binding, operation)?,
            )
            .map_err(|_| denied())
    }
    fn credential(
        &self,
        binding: &PluginBinding,
        slot: &SlotIdentity,
        operation: &HostOperationBinding,
    ) -> Result<Option<String>> {
        self.authorize(binding, operation)?;
        let manager = self.plugins.manager.as_ref().map_err(|_| denied())?;
        let scope = Scope {
            slot: Some(&slot.slot_id),
            ..Self::operation_scope(binding, operation)?
        };
        let owner = self.host_binding(binding, slot.account_generation.clone());
        // Before keyring/cache: UUID/revision/reference have to be the currently
        // published ownership record, not merely a caller-supplied string.
        super::scoped_secrets::verify_slot(&self.plugins, &owner, slot)?;
        manager
            .authority
            .with_authorized(
                &owner,
                Principal::HostBinding,
                Capability::CredentialUse,
                scope,
                || {
                    self.secrets
                        .get_keyring_plugin(&slot.key_ref)
                        .map(Some)
                        .map_err(|_| zroutery_plugin_api::PluginError::CredentialUnavailable)
                },
            )
            .map_err(|_| Error::MissingApiKey("plugin credential unavailable".into()))
    }
    fn poll_send(
        &self,
        scope: PluginSendScope<'_>,
        send: &mut dyn FnMut() -> Poll<Result<reqwest::Response>>,
    ) -> Poll<Result<reqwest::Response>> {
        if self
            .plugins
            .safe_mode
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Poll::Ready(Err(denied()));
        }
        let Ok(manager) = self.plugins.manager.as_ref() else {
            return Poll::Ready(Err(denied()));
        };
        let Ok(operation_scope) = Self::operation_scope(scope.binding, scope.operation) else {
            return Poll::Ready(Err(denied()));
        };
        let binding = self.host_binding(scope.binding, None);
        match manager.authority.with_authorized(
            &binding,
            Principal::HostBinding,
            Capability::HostOperationBind,
            operation_scope,
            || Ok(send()),
        ) {
            Ok(result) => result,
            Err(_) => Poll::Ready(Err(denied())),
        }
    }
    fn synthesize(&self, config: &AppConfig) -> Vec<ProviderConfig> {
        if self
            .plugins
            .safe_mode
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Vec::new();
        }
        let Ok(manager) = self.plugins.manager.as_ref() else {
            return Vec::new();
        };
        let port: Arc<dyn PluginPort> = Arc::new(Self {
            plugins: Arc::clone(&self.plugins),
            secrets: Arc::clone(&self.secrets),
        });
        let mut result = Vec::new();
        for instance in &config.plugin_instances {
            if !instance.enabled || config.providers.iter().any(|p| p.id == instance.id) {
                continue;
            }
            let Ok(package) = manager.package(&instance.installation_id) else {
                continue;
            };
            if package.digest != instance.package_digest {
                continue;
            }
            let Some(grant) = manager
                .authority
                .record(&instance.installation_id)
                .filter(|g| g.enabled)
            else {
                continue;
            };
            let Some(descriptor) = package
                .manifest
                .contributions
                .providers
                .iter()
                .find(|p| p.id == instance.contribution_id)
            else {
                continue;
            };
            if descriptor.transport_profile.is_some() {
                continue;
            }
            if descriptor.operations.iter().any(|o| {
                !zroutery_core::plugin::registry::services().contains(&o.service.id.as_str())
            }) {
                continue;
            }
            let binding = PluginBinding::verified(
                instance.installation_id.clone(),
                instance.id.clone(),
                instance.contribution_id.clone(),
                package.digest.clone(),
                grant.epoch,
                instance
                    .credentials
                    .iter()
                    .map(SlotIdentity::from)
                    .collect(),
                descriptor.operations.clone(),
                descriptor.transport_profile.clone(),
                Arc::clone(&port),
            );
            let Ok(operation) = zroutery_core::plugin::registry::inference(descriptor) else {
                continue;
            };
            if binding.authorize(operation).is_err() {
                continue;
            }
            if let Ok(mut provider) = zroutery_core::plugin::registry::provider(
                &instance.id,
                &instance.name,
                descriptor,
                binding,
            ) {
                provider.accounts = instance.accounts.clone();
                // Until the account service transport is scoped, do not hand
                // plugin accounts to native reconciliation/maintenance adapters.
                for account in &mut provider.accounts {
                    account.probe_kind = Some(zroutery_core::config::AccountProbeKind::None);
                    account.maintenance = Default::default();
                    account.key_ref.clear();
                }
                if let Some(default_slot) = instance
                    .credentials
                    .iter()
                    .find(|slot| slot.account_generation.is_none())
                {
                    provider.key_ref = default_slot.key_ref.clone();
                }
                result.push(provider);
            }
        }
        result
    }
}
