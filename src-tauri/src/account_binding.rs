//! One binding workflow for manual tokens and browser capture. No secret crosses
//! a view/journal. Config publication is serialized, CAS checked and compensated.

use serde::Serialize;
use zroutery_core::config::{
    AccountBinding, AccountConfig, AccountProbeKind, BindingVerification, RemoteIdentity,
};
use zroutery_core::runtime_store::BindingPublication;

use crate::state::Desktop;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingPreview {
    pub publication_id: String,
    pub identity: Option<RemoteIdentity>,
    pub verification: BindingVerification,
    pub identity_changed: bool,
    pub previous_identity: Option<RemoteIdentity>,
    pub reason: Option<String>,
}

pub fn version(account: &AccountConfig) -> String {
    account
        .binding
        .as_ref()
        .map(|binding| binding.version())
        .unwrap_or_else(|| {
            format!(
                "legacy:{}:{}",
                account.key_ref,
                account.maintenance.new_api_user.as_deref().unwrap_or("")
            )
        })
}

impl Desktop {
    pub fn binding_version(&self, provider_id: &str, account_id: &str) -> Result<String, String> {
        let config = self.core.config();
        let account = config
            .provider(provider_id)
            .and_then(|provider| {
                provider
                    .accounts
                    .iter()
                    .find(|account| account.account_id == account_id)
            })
            .ok_or_else(|| format!("unknown account `{account_id}` on provider `{provider_id}`"))?;
        Ok(version(account))
    }

    /// Stage first, verify second, publish only after review/identity confirmation.
    /// `browser_identity` may only come from accepted `/api/user/self`, not DOM
    /// names or unverified token claims. Rust verification remains independent.
    pub async fn stage_account_binding(
        &self,
        provider_id: &str,
        account_id: &str,
        token: &str,
        user_hint: Option<&str>,
        expected: Option<&str>,
        browser_identity: Option<RemoteIdentity>,
    ) -> Result<BindingPreview, String> {
        let token = token.trim();
        if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_whitespace) {
            return Err("the panel token is empty, too long or contains whitespace".into());
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("durable binding storage is unavailable")?;
        let (account, panel_root, probe_kind) = {
            let config = self.core.config();
            let provider = config.provider(provider_id).ok_or("unknown provider")?;
            let account = provider
                .accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .ok_or("unknown account")?;
            let kind = account.probe_kind.unwrap_or(AccountProbeKind::NewApi);
            let panel = zroutery_core::config::account_panel_root(
                &provider.base_url,
                account.maintenance.panel_override(),
            );
            (account.clone(), panel, kind)
        };
        let previous_version = version(&account);
        if expected.is_some_and(|expected| expected != previous_version) {
            return Err(
                "the account binding changed while capturing the token; start again".into(),
            );
        }
        let mut binding = account
            .binding
            .clone()
            .unwrap_or_else(AccountBinding::fresh);
        binding.credential_revision = binding
            .credential_revision
            .checked_add(1)
            .ok_or("credential revision overflow")?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let staged_ref = format!("provider:{provider_id}:account:{account_id}:binding:{id}");
        let mut publication = BindingPublication {
            id,
            provider_id: provider_id.into(),
            account_id: account_id.into(),
            previous_version,
            previous_ref: account.key_ref.clone(),
            staged_ref,
            binding,
            phase: "staging".into(),
        };
        runtime.journal_binding(&publication)?;
        if let Err(error) =
            crate::commands::write_account_secret(self, &publication.staged_ref, token).await
        {
            publication.phase = "discarded".into();
            let _ = runtime.journal_binding(&publication);
            return Err(error);
        }
        let mut reason = None;
        let identity: Result<Option<RemoteIdentity>, String> = if probe_kind
            == AccountProbeKind::NewApi
        {
            #[cfg(feature = "newapi")]
            {
                use zroutery_core::account::adapters::newapi::{NewApiAdapter, NewApiConfig};
                match NewApiAdapter::new(NewApiConfig {
                    base_url: panel_root,
                    api_key: token.into(),
                    new_api_user: user_hint.map(str::to_string),
                    request_timeout_secs: 20,
                    ..Default::default()
                }) {
                    Ok(adapter) => match adapter.verify_identity(token).await {
                        Ok(identity) => Ok(Some(identity)),
                        Err(error) => match error {
                            zroutery_core::Error::Transport { .. }
                            | zroutery_core::Error::Timeout(_)
                            | zroutery_core::Error::Upstream {
                                status: 403 | 429 | 500..=599,
                                ..
                            } => {
                                reason = Some(error.safe_message());
                                Ok(browser_identity)
                            }
                            other => Err(other.safe_message()),
                        },
                    },
                    Err(error) => Err(error.safe_message()),
                }
            }
            #[cfg(not(feature = "newapi"))]
            {
                let _ = (panel_root, user_hint, browser_identity);
                reason = Some("this build cannot verify NewAPI identity".into());
                Ok(None)
            }
        } else {
            let _ = (panel_root, browser_identity);
            reason =
                Some("no identity probe declared; credential remains pending verification".into());
            Ok(None)
        };
        let identity = match identity {
            Ok(identity) => identity,
            Err(error) => {
                publication.phase = "discarded".into();
                runtime.journal_binding(&publication)?;
                self.secrets.delete(&publication.staged_ref)?;
                return Err(error);
            }
        };
        let old_identity = account
            .binding
            .as_ref()
            .and_then(|binding| binding.identity.clone());
        let changed = match (&old_identity, &identity) {
            (Some(old), Some(new)) => !old.same_user(new),
            (None, Some(new)) => account
                .maintenance
                .new_api_user
                .as_deref()
                .is_some_and(|old| old != new.user_id),
            _ => false,
        };
        if changed || (old_identity.is_none() && identity.is_some()) {
            publication.binding.identity_revision = publication
                .binding
                .identity_revision
                .checked_add(1)
                .ok_or("identity revision overflow")?;
        }
        publication.binding.identity = identity.clone();
        publication.binding.verification = if identity.is_some() {
            BindingVerification::Verified
        } else {
            BindingVerification::Pending
        };
        publication.binding.reason = reason.clone();
        publication.phase = "ready".into();
        runtime.journal_binding(&publication)?;
        Ok(BindingPreview {
            publication_id: publication.id,
            identity,
            verification: publication.binding.verification,
            identity_changed: changed,
            previous_identity: old_identity,
            reason,
        })
    }

    pub async fn publish_account_binding(
        &self,
        id: &str,
        confirm_identity_change: bool,
    ) -> Result<(), String> {
        let _migration = self.migration.lock().await;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("durable binding storage is unavailable")?;
        let mut publication = runtime
            .binding_publication(id)?
            .ok_or("unknown binding publication")?;
        if publication.phase != "ready" {
            return Err("binding publication is not ready".into());
        }
        let mut config = (*self.core.config()).clone();
        let account = config
            .providers
            .iter_mut()
            .find(|provider| provider.id == publication.provider_id)
            .and_then(|provider| {
                provider
                    .accounts
                    .iter_mut()
                    .find(|account| account.account_id == publication.account_id)
            })
            .ok_or("the account was removed before binding publication")?;
        if version(account) != publication.previous_version {
            return Err(
                "the account changed before binding publication; discard this preview and retry"
                    .into(),
            );
        }
        if account.maintenance.panel_override().is_some_and(|root| {
            publication
                .binding
                .identity
                .as_ref()
                .is_some_and(|identity| {
                    root.trim_end_matches('/') != identity.panel_root.trim_end_matches('/')
                })
        }) {
            return Err(
                "the panel root changed after identity verification; create a new preview".into(),
            );
        }
        let changed = match (
            account
                .binding
                .as_ref()
                .and_then(|binding| binding.identity.as_ref()),
            publication.binding.identity.as_ref(),
        ) {
            (Some(old), Some(new)) => !old.same_user(new),
            (None, Some(new)) => account
                .maintenance
                .new_api_user
                .as_deref()
                .is_some_and(|old| old != new.user_id),
            _ => false,
        };
        if changed && !confirm_identity_change {
            return Err("remote identity changed; explicit confirmation is required".into());
        }
        if !self.secrets.has(&publication.staged_ref) {
            return Err("the staged credential is no longer available".into());
        }
        if let Some(old) = &account.binding {
            runtime.save_binding_version(&publication.provider_id, &publication.account_id, old)?;
        }
        account.key_ref = publication.staged_ref.clone();
        account.binding = Some(publication.binding.clone());
        if let Some(identity) = &publication.binding.identity {
            account.maintenance.new_api_user = Some(identity.user_id.clone());
        } else {
            // A new unverified token must not inherit an old verified user header.
            account.maintenance.new_api_user = None;
        }
        crate::store::save(&self.config_dir, &config)?;
        self.core.set_config(config.clone());
        runtime.save_binding_version(
            &publication.provider_id,
            &publication.account_id,
            &publication.binding,
        )?;
        publication.phase = "published".into();
        runtime.journal_binding(&publication)?;
        let live = crate::secret_refs::referenced_secrets(&config);
        if !publication.previous_ref.is_empty() && !live.contains(&publication.previous_ref) {
            if let Err(error) = self.secrets.delete(&publication.previous_ref) {
                self.set_warning(Some(error));
            }
        }
        Ok(())
    }

    pub async fn discard_account_binding(&self, id: &str) -> Result<(), String> {
        let _migration = self.migration.lock().await;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or("durable binding storage is unavailable")?;
        let mut publication = runtime
            .binding_publication(id)?
            .ok_or("unknown binding publication")?;
        if !crate::secret_refs::referenced_secrets(&self.core.config())
            .contains(&publication.staged_ref)
        {
            self.secrets.delete(&publication.staged_ref)?;
        }
        publication.phase = "discarded".into();
        runtime.journal_binding(&publication)
    }

    pub(crate) fn recover_bindings(&self) -> Result<(), String> {
        let Some(runtime) = &self.runtime else {
            return Ok(());
        };
        let config = self.core.config();
        let live = crate::secret_refs::referenced_secrets(&config);
        for mut publication in runtime.pending_bindings()? {
            let published = config
                .provider(&publication.provider_id)
                .and_then(|provider| {
                    provider
                        .accounts
                        .iter()
                        .find(|account| account.account_id == publication.account_id)
                })
                .is_some_and(|account| {
                    account.key_ref == publication.staged_ref
                        && account.binding.as_ref() == Some(&publication.binding)
                });
            if published {
                runtime.save_binding_version(
                    &publication.provider_id,
                    &publication.account_id,
                    &publication.binding,
                )?;
                publication.phase = "published".into();
            } else {
                if !live.contains(&publication.staged_ref) {
                    self.secrets.delete(&publication.staged_ref)?;
                }
                publication.phase = "discarded".into();
            }
            runtime.journal_binding(&publication)?;
        }
        Ok(())
    }
}
