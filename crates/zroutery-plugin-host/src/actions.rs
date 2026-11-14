//! Real-user intents are host table entries, consumed before any side effect.
use crate::grants::Binding;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zroutery_plugin_api::ui::UiEffect;
use zroutery_plugin_api::{PluginError, Result};

pub struct ActionIntent {
    binding: Binding,
    generation: String,
    revision: String,
    action_id: String,
    options_action: bool,
    expires: Instant,
    consumed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{InstallationIdentity, Owner};
    fn binding() -> Binding {
        Binding {
            identity: InstallationIdentity {
                installation_id: "install".into(),
                plugin_id: "dev.test.ui".into(),
                package_digest: "digest".into(),
            },
            contribution: "ui".into(),
            owner: Owner {
                installation_id: "install".into(),
                provider_instance: "p".into(),
                account_generation: Some("a1".into()),
            },
            grant_epoch: 1,
        }
    }
    #[test]
    fn click_cannot_swap_action_entity_generation_or_replay() {
        let actions = Actions::default();
        let one = binding();
        let token = actions
            .issue(
                one.clone(),
                "worker1".into(),
                "view1".into(),
                "refresh".into(),
            )
            .unwrap();
        assert!(actions
            .consume(
                &token,
                &one,
                "worker1",
                "view1",
                &UiEffect::HostAction {
                    action_id: "delete".into()
                }
            )
            .is_err());
        let mut other = one.clone();
        other.owner.account_generation = Some("a2".into());
        assert!(actions
            .consume(
                &token,
                &other,
                "worker1",
                "view1",
                &UiEffect::HostAction {
                    action_id: "refresh".into()
                }
            )
            .is_err());
        assert!(actions
            .consume(
                &token,
                &one,
                "worker2",
                "view1",
                &UiEffect::HostAction {
                    action_id: "refresh".into()
                }
            )
            .is_err());
        assert_eq!(
            actions
                .consume(
                    &token,
                    &one,
                    "worker1",
                    "view1",
                    &UiEffect::HostAction {
                        action_id: "refresh".into()
                    }
                )
                .unwrap(),
            "refresh"
        );
        assert!(actions
            .consume(
                &token,
                &one,
                "worker1",
                "view1",
                &UiEffect::HostAction {
                    action_id: "refresh".into()
                }
            )
            .is_err());
    }
}
#[derive(Default)]
pub struct Actions {
    intents: Mutex<BTreeMap<String, ActionIntent>>,
}
impl Actions {
    /// Only the composition layer handling a trusted click calls issue. There is
    /// no issue-token RPC in a worker realm.
    pub fn issue(
        &self,
        binding: Binding,
        generation: String,
        revision: String,
        action_id: String,
    ) -> Result<String> {
        let mut intents = self.intents.lock().map_err(|_| PluginError::PluginBusy)?;
        intents.retain(|_, i| i.expires > Instant::now());
        if intents.len() >= 256 {
            return Err(PluginError::PluginBusy);
        }
        let token = uuid::Uuid::new_v4().to_string();
        intents.insert(
            token.clone(),
            ActionIntent {
                binding,
                generation,
                revision,
                action_id,
                options_action: false,
                expires: Instant::now() + Duration::from_secs(30),
                consumed: false,
            },
        );
        Ok(token)
    }
    pub fn consume(
        &self,
        token: &str,
        binding: &Binding,
        generation: &str,
        revision: &str,
        effect: &UiEffect,
    ) -> Result<String> {
        let mut intents = self
            .intents
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        let intent = intents
            .get_mut(token)
            .ok_or(PluginError::PermissionDenied)?;
        if intent.consumed
            || intent.expires <= Instant::now()
            || intent.binding.identity != binding.identity
            || intent.binding.owner != binding.owner
            || intent.binding.grant_epoch != binding.grant_epoch
            || intent.binding.contribution != binding.contribution
            || intent.generation != generation
            || intent.revision != revision
            || match effect {
                UiEffect::HostAction { action_id } => action_id != &intent.action_id,
                UiEffect::OptionsProposal {
                    expected_revision, ..
                } => !intent.options_action || expected_revision != revision,
                UiEffect::None => false,
            }
        {
            return Err(PluginError::PermissionDenied);
        }
        intent.consumed = true;
        Ok(intent.action_id.clone())
    }
    pub fn issue_options(
        &self,
        binding: Binding,
        generation: String,
        revision: String,
        action_id: String,
    ) -> Result<String> {
        let token = self.issue(binding, generation, revision, action_id)?;
        let mut intents = self
            .intents
            .lock()
            .map_err(|_| PluginError::PermissionDenied)?;
        intents
            .get_mut(&token)
            .ok_or(PluginError::PermissionDenied)?
            .options_action = true;
        Ok(token)
    }
}
