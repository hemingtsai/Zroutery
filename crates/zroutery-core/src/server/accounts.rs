//! The first production caller of the account reconciler.
//!
//! # Why this file exists
//!
//! `AppState` owned an [`AccountStore`], [`ProviderConfig`] declared accounts, and
//! [`AccountReconciler`] knew how to fill one — and nothing in the repository ever
//! called any of it. That is the entire reason `ACCOUNT` sat at `PARTIAL`: not a
//! missing engine, a missing call site. A store nobody fills is a store a panel
//! cannot render, so this is the smallest thing that makes the six account-surface
//! nodes describable as integrated rather than as scaffolding.
//!
//! # The decisions this makes, and why none of them is a guess
//!
//! **One adapter per account, not per provider.** [`NewApiAdapter`] holds exactly
//! one credential and one session, and [`AccountConfig::key_ref`] is declared per
//! account. One adapter per provider would probe the second account with the first
//! account's key and report it as healthy, which is the same class of defect the
//! reconciler exists to prevent.
//!
//! **A credential that resolves to nothing is probed with an empty one.** The
//! adapter installs no session, the first authenticated call fails, and the
//! reconciler publishes that as `AuthenticationExpired` with `last_failure` set.
//! Skipping the probe instead would leave the previous reading in the store; using
//! any other credential would report an account as reachable that is not.
//!
//! **A provider no backend speaks to, and a backend that refused to build, are
//! different facts.** The first is [`Unreachable::NoBackend`] and lands through the
//! reconciler's `Unsupported` path as `Unknown` with `last_success: None`. The
//! second is [`Unreachable::Construction`] and lands as a *failure carrying the
//! reason*, because there the declaration itself is what is wrong. Collapsing them
//! would mean a misconfigured panel and a feature-gated build report the same
//! reading.
//!
//! # What this deliberately does not do
//!
//! It does not run on a timer and it does not run at construction. Probing costs a
//! network request per account, and a proxy that probes on startup turns a slow
//! panel into a slow start. The caller decides when; this is the operation to call.
//!
//! It probes sequentially, one account at a time. That is the honest default for a
//! handful of declared accounts and it avoids a burst against a panel that rate
//! limits, but it is a real limitation for a large fleet and is recorded rather
//! than hidden.
//!
//! [`AccountStore`]: crate::account::AccountStore
//! [`AccountReconciler`]: crate::account::AccountReconciler
//! [`NewApiAdapter`]: crate::account::adapters::newapi::NewApiAdapter

use crate::account::reconcile::{
    AccountProbe, AccountReconciler, BoxFuture, DynAccountProvider, ReconcileReport,
};
use crate::account::types::{AccountCapabilities, AccountId, AccountRuntime, AccountStatus};
use crate::config::{AccountConfig, ProviderConfig, ProviderKind};
use crate::Error;

use super::AppState;

/// Why no real adapter is answering for a provider.
enum Unreachable {
    /// This build contains no account backend that speaks to this provider.
    ///
    /// Not an error and not a fault: `account` can be on while `newapi` is off.
    /// It is reported through the reconciler's existing `Unsupported` path, which
    /// publishes `Unknown` with `last_success: None`. That is the true reading —
    /// nothing has been established about the account — and it is why the branch
    /// goes through the reconciler rather than around it.
    NoBackend,
    /// A backend was selected and refused to be built, so the declaration is what
    /// is wrong. Carried into the store as a failure so the reason survives to the
    /// panel instead of being logged and forgotten.
    ///
    /// Gated on `newapi`, because it is only reachable when an adapter is selected
    /// at all. In an `account`-only build every provider takes the
    /// [`Unreachable::NoBackend`] path, so this variant would be constructed
    /// nowhere and clippy is right to call it dead.
    #[cfg(feature = "newapi")]
    Construction(String),
}

/// Stands in for an adapter that cannot answer, so every declared account still
/// gets a published runtime.
struct UnreachableProvider {
    provider_id: String,
    /// All-false for [`Unreachable::NoBackend`], which is what routes the
    /// reconciler to `publish_unsupported`; the real backend's capabilities for
    /// [`Unreachable::Construction`], so the reconciler records a failure.
    capabilities: AccountCapabilities,
    unreachable: Unreachable,
}

impl UnreachableProvider {
    fn no_backend(provider_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            capabilities: AccountCapabilities::default(),
            unreachable: Unreachable::NoBackend,
        }
    }

    #[cfg(feature = "newapi")]
    fn construction_failed(
        provider_id: &str,
        capabilities: AccountCapabilities,
        reason: String,
    ) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            capabilities,
            unreachable: Unreachable::Construction(reason),
        }
    }

    fn refused(&self) -> Error {
        match &self.unreachable {
            Unreachable::NoBackend => {
                Error::internal("no account backend in this build speaks to this provider")
            }
            #[cfg(feature = "newapi")]
            Unreachable::Construction(reason) => {
                Error::invalid(format!("account backend could not be built: {reason}"))
            }
        }
    }
}

impl DynAccountProvider for UnreachableProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn capabilities(&self) -> AccountCapabilities {
        self.capabilities.clone()
    }

    fn refresh_dyn<'a>(
        &'a self,
        _account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<AccountRuntime, Error>> {
        let error = self.refused();
        Box::pin(async move { Err(error) })
    }

    fn fetch_usage_dyn<'a>(
        &'a self,
        _account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<crate::account::types::AccountUsage, Error>> {
        let error = self.refused();
        Box::pin(async move { Err(error) })
    }

    fn fetch_quota_dyn<'a>(
        &'a self,
        _account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<crate::account::types::AccountQuota, Error>> {
        let error = self.refused();
        Box::pin(async move { Err(error) })
    }

    fn health_check_dyn<'a>(
        &'a self,
        _account_id: &'a AccountId,
    ) -> BoxFuture<'a, Result<AccountStatus, Error>> {
        let error = self.refused();
        Box::pin(async move { Err(error) })
    }
}

impl AppState {
    /// Probe every declared, enabled account and publish the result into the store.
    ///
    /// This is the production caller `ACCOUNT` was missing. It reads the
    /// *currently loaded* configuration rather than a captured one, so a provider
    /// added or removed by a config reload is reconciled without a restart.
    ///
    /// Every declared account produces exactly one entry in the report, including
    /// the ones no backend could reach. An account that is absent from the report
    /// is an account that is disabled or not declared; an account present with
    /// `Unknown` is one that was probed and established nothing. Those are
    /// different states and a panel must be able to tell them apart.
    #[cfg(feature = "account")]
    pub async fn reconcile_accounts(&self) -> ReconcileReport {
        let config = self.registry().snapshot();
        let providers: Vec<&ProviderConfig> = config.providers.iter().collect();
        self.reconcile_providers(&providers).await
    }

    /// Probe one provider's declared accounts and publish the result.
    ///
    /// The per-provider half of [`Self::reconcile_accounts`], and it exists for a
    /// reason the all-provider sweep cannot serve: a panel that asks for one
    /// provider's balance must not pay for a request to every other provider to
    /// get it. The sweep exists for a page that refreshes everything at once,
    /// and the cost model of the two is different.
    ///
    /// A provider id that names nothing yields an empty report rather than an
    /// error, because the caller is asking about a balance and the answer to
    /// "that provider is gone" is the same as "that provider has no accounts".
    #[cfg(feature = "account")]
    pub async fn reconcile_provider_accounts(&self, provider_id: &str) -> ReconcileReport {
        let config = self.registry().snapshot();
        let providers: Vec<&ProviderConfig> = config
            .providers
            .iter()
            .filter(|p| p.id == provider_id)
            .collect();
        self.reconcile_providers(&providers).await
    }

    /// The loop both entry points share.
    ///
    /// Split out so the per-provider path cannot drift from the sweep: the two
    /// differ only in which providers they are handed, and a second copy of the
    /// loop is a second set of rules for what counts as a declared account.
    #[cfg(feature = "account")]
    async fn reconcile_providers(&self, providers: &[&ProviderConfig]) -> ReconcileReport {
        let store = &self.accounts;
        store.sync_declarations(&self.config());
        let mut outcomes = Vec::new();

        for provider in providers {
            if !provider.enabled {
                continue;
            }
            for declaration in provider.accounts.iter().filter(|a| a.enabled) {
                // The sweep captured a provider earlier; a changed declaration
                // cannot start a new probe with the old credential after a swap.
                let Some(version) = store.begin_declared_probe(provider, declaration) else {
                    outcomes.push((
                        provider.id.clone(),
                        AccountId(declaration.account_id.clone()),
                        crate::account::AccountSyncOutcome::Stale,
                    ));
                    continue;
                };
                let probe = AccountProbe {
                    provider_id: provider.id.clone(),
                    account_id: AccountId(declaration.account_id.clone()),
                };
                let adapter = self.adapter_for(provider, declaration);
                let reconciler = AccountReconciler::new(store);
                let report = reconciler
                    .reconcile_versioned(adapter.as_ref(), &probe, version)
                    .await;
                outcomes.extend(report.outcomes);
            }
        }

        ReconcileReport { outcomes }
    }

    /// The adapter that will answer for one declared account.
    ///
    /// Never returns `None`: an account with no reachable backend still has to be
    /// published, and the reconciler is the thing that knows how to publish a
    /// runtime for a probe that could not be made.
    #[cfg(feature = "account")]
    fn adapter_for(
        &self,
        provider: &ProviderConfig,
        declaration: &AccountConfig,
    ) -> Box<dyn DynAccountProvider> {
        let credential = self.credential_for(provider, declaration);
        newapi_adapter(provider, declaration, &credential)
    }

    /// The secret this account should be probed with.
    ///
    /// Borrowed for the duration of the probe and never stored in the runtime,
    /// because [`AccountRuntime`] derives `Serialize` and a credential must not
    /// reach a struct that a panel can serialise.
    #[cfg(feature = "account")]
    fn credential_for(&self, provider: &ProviderConfig, declaration: &AccountConfig) -> String {
        resolve_credential(self.secrets().as_ref(), provider, declaration)
    }
}

/// The secret one declared account should be probed with.
///
/// An account with no `key_ref` of its own inherits the provider's, which is the
/// single-account relay case the configuration documents. A reference that names
/// nothing resolves to an empty credential rather than to some other account's:
/// the adapter then installs no session, the authenticated call fails, and the
/// reconciler publishes that failure. Falling back to the provider's key here
/// would be the dangerous direction — it would probe one account with another
/// credential and report it healthy.
///
/// Split out as a free function so it can be tested directly. It is not
/// observable from the store: an account probed with a resolved credential and one
/// probed with an empty one both end as a failed probe, because a refused
/// connection and a missing session fail the same way. Testing it here is the
/// only place the difference is decidable.
#[cfg(feature = "account")]
pub(crate) fn resolve_credential(
    secrets: &dyn crate::config::SecretStore,
    provider: &ProviderConfig,
    declaration: &AccountConfig,
) -> String {
    let key_ref = if declaration.key_ref.trim().is_empty() {
        provider.key_ref.as_str()
    } else {
        declaration.key_ref.as_str()
    };
    secrets.get(key_ref).unwrap_or_default()
}

#[cfg(all(test, feature = "account"))]
mod tests {
    use super::resolve_credential;
    use crate::config::{AccountConfig, MemorySecretStore, ProviderConfig, ProviderKind};

    fn provider() -> ProviderConfig {
        let mut p = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
        p.key_ref = "provider-key".to_string();
        p
    }

    fn account(key_ref: &str) -> AccountConfig {
        AccountConfig {
            account_id: "acct-0".to_string(),
            key_ref: key_ref.to_string(),
            enabled: true,
            ..AccountConfig::default()
        }
    }

    /// The account's own reference wins over the provider's.
    #[test]
    fn an_account_reference_beats_the_provider_reference() {
        let secrets = MemorySecretStore::new()
            .with("provider-key", "the-provider-secret")
            .with("acct-key", "the-account-secret");
        assert_eq!(
            resolve_credential(&secrets, &provider(), &account("acct-key")),
            "the-account-secret"
        );
    }

    /// The documented single-account relay case: no account reference inherits the
    /// provider's.
    #[test]
    fn a_blank_account_reference_inherits_the_provider_reference() {
        for blank in ["", "   ", "\t"] {
            let secrets = MemorySecretStore::new().with("provider-key", "the-provider-secret");
            assert_eq!(
                resolve_credential(&secrets, &provider(), &account(blank)),
                "the-provider-secret",
                "a blank account key_ref must inherit, not resolve to nothing"
            );
        }
    }

    /// A reference nothing is registered under is EMPTY, not the provider's key.
    ///
    /// This is the direction that matters. Probing an account with a credential
    /// that belongs to a different account would report it reachable when it is
    /// not, which is the exact class of defect this file refuses to introduce.
    #[test]
    fn an_unregistered_reference_resolves_to_nothing_rather_than_the_provider_key() {
        let secrets = MemorySecretStore::new().with("provider-key", "the-provider-secret");
        assert_eq!(
            resolve_credential(&secrets, &provider(), &account("nobody-registered-this")),
            "",
            "an unresolvable account must not borrow the provider's credential"
        );
    }

    /// With no secrets at all, every account is probed with nothing.
    #[test]
    fn an_empty_secret_store_resolves_to_nothing() {
        let secrets = MemorySecretStore::new();
        assert_eq!(resolve_credential(&secrets, &provider(), &account("")), "");
        assert_eq!(
            resolve_credential(&secrets, &provider(), &account("acct-key")),
            ""
        );
    }

    /// The caller passes the account it is probing, not a default.
    ///
    /// The tests above exercise `resolve_credential` directly, which left the line
    /// that *calls* it uncovered: substituting `AccountConfig::default()` there
    /// compiles, passes every test above, and silently makes every account inherit
    /// the provider's credential. That is the most dangerous form of this bug —
    /// one account probed with another's key, reported healthy — so the wiring is
    /// asserted here, against a real `AppState` and its real `SecretStore`.
    #[test]
    fn the_caller_passes_the_account_being_probed() {
        use super::AppState;
        use crate::config::{AppConfig, SecretStore};
        use std::sync::Arc;

        let state = AppState::new(
            AppConfig::default(),
            Arc::new(
                MemorySecretStore::new()
                    .with("provider-key", "the-provider-secret")
                    .with("acct-key", "the-account-secret"),
            ) as Arc<dyn SecretStore>,
        );

        assert_eq!(
            state.credential_for(&provider(), &account("acct-key")),
            "the-account-secret",
            "the caller must forward the account it is probing"
        );
        assert_eq!(
            state.credential_for(&provider(), &account("")),
            "the-provider-secret"
        );
    }
}

/// Build the NewAPI adapter for a provider, or say why there is none.
///
/// NewAPI panels are OpenAI-compatible relays, so that is the only provider kind
/// this can legitimately be pointed at; an Anthropic-kind provider that declares
/// accounts is recorded as having no backend rather than probed with the wrong
/// protocol. A second account backend would need an explicit declaration of which
/// backend serves an account — there is currently nothing in the configuration to
/// hold one, and inferring it from the base URL would be exactly the guess this
/// file refuses to make.
#[cfg(feature = "newapi")]
fn newapi_adapter(
    provider: &ProviderConfig,
    declaration: &AccountConfig,
    credential: &str,
) -> Box<dyn DynAccountProvider> {
    use crate::account::adapters::newapi::{NewApiAdapter, NewApiConfig};

    let kind = declaration.probe_kind.unwrap_or_else(|| {
        // Compatibility for old configurations only; new declarations carry
        // an explicit probe kind, independent of the inference protocol.
        if provider.kind == ProviderKind::OpenAICompatible {
            crate::config::AccountProbeKind::NewApi
        } else {
            crate::config::AccountProbeKind::None
        }
    });
    if kind != crate::config::AccountProbeKind::NewApi {
        return Box::new(UnreachableProvider::no_backend(&provider.id));
    }

    let mut config = NewApiConfig {
        // The wallet can live on a different host than inference, and some
        // panels require the declared user id even to answer /api/user/self.
        // Both values belong to this account, never the first sibling account.
        base_url: crate::config::account_panel_root(
            &provider.base_url,
            declaration.maintenance.panel_override(),
        ),
        api_key: credential.to_string(),
        new_api_user: declaration.maintenance.new_api_user.clone(),
        capabilities: NewApiConfig::default().capabilities,
        // The provider's own timeouts govern the panel as they govern relayed
        // traffic; leaving the adapter's defaults would mean an account probe
        // outlives the request budget the operator configured for the provider.
        connect_timeout_secs: provider.connect_timeout_secs.max(1),
        request_timeout_secs: provider.timeout_secs.max(1),
        ..NewApiConfig::default()
    };
    if config.api_key.trim().is_empty() {
        config.api_key = String::new();
    }

    match NewApiAdapter::new(config) {
        Ok(adapter) => Box::new(adapter),
        Err(error) => Box::new(UnreachableProvider::construction_failed(
            &provider.id,
            NewApiConfig::default().capabilities,
            error.to_string(),
        )),
    }
}

/// No NewAPI backend is compiled into this build.
#[cfg(not(feature = "newapi"))]
fn newapi_adapter(
    provider: &ProviderConfig,
    _declaration: &AccountConfig,
    _credential: &str,
) -> Box<dyn DynAccountProvider> {
    let _ = ProviderKind::OpenAICompatible;
    Box::new(UnreachableProvider::no_backend(&provider.id))
}
