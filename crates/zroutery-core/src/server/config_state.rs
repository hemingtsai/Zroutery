//! The retained projections, swapping configuration and the registry it feeds, and holding an election.

use std::sync::Arc;

use crate::config::{AppConfig, ModelEntry, ModelTier, ProviderConfig, ProviderKind, SecretStore};
use crate::election::{self, Election, Measurement};
use crate::error::{Error, Result};
use crate::registry::Registry;

use super::projection_log::ProjectionLog;
use super::state::AppState;

impl AppState {
    /// The retained outcomes paired with their routing decisions, so the
    /// observability projection can be read back.
    ///
    /// Not a routing input and not a training source: a diagnostic view of the
    /// same single accounting the request log records.
    pub fn projections(&self) -> &ProjectionLog {
        &self.projections
    }

    /// Rebuild the upstream HTTP client (e.g. when bypass_proxy changes).
    pub fn rebuild_upstream(&self, bypass_proxy: bool) {
        self.upstream.rebuild_client(bypass_proxy);
    }

    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    pub fn config(&self) -> Arc<AppConfig> {
        self.registry().snapshot()
    }

    /// Swap in a new configuration, rebuilding the lookup tables once here
    /// instead of per request. In-flight requests keep their snapshot.
    pub fn set_config(&self, config: AppConfig) {
        self.stats.set_limit(config.server.log_limit);
        // The shadow switch and retention are a running engine's configuration,
        // so a swap has to reach the engine instead of only the next process.
        #[cfg(feature = "ml")]
        self.shadow.reconfigure(
            config.shadow.enabled,
            config.shadow.max_decisions,
            config.shadow.max_age_secs,
        );
        let known: std::collections::HashSet<String> =
            config.models.iter().map(|m| m.exposed_id()).collect();
        // Health rows for models that no longer exist would otherwise linger
        // forever and keep appearing in the GUI snapshot.
        self.router.retain_models(|id| known.contains(id));
        let providers: std::collections::HashSet<&str> = config
            .providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect();
        self.router.retain_providers(|id| providers.contains(id));
        self.router.retain_credentials(&config);
        // Which providers meter the whole account rather than one model can
        // change with the configuration, so the gate set is refreshed here.
        self.router.set_group_limited_providers(
            config
                .providers
                .iter()
                .filter(|provider| provider.kind.is_opencode())
                .map(|provider| provider.id.clone()),
        );
        let registry = Arc::new(Registry::new(Arc::new(config)));
        let mut current = crate::sync::write(&self.registry);
        // Compare against the state actually being replaced. Another config
        // writer may have swapped it while this registry was being built.
        let ranking_stale = election_inputs_changed(current.config(), registry.config());
        // A rekey keeps the key identity `(provider, account, key.id)` while
        // replacing the secret behind it. The router's credential gates are
        // keyed by that identity, so the old secret's authentication cooldown
        // and key-level 429 window must be forgotten here — under the same
        // write lock as the swap, exactly like the election invalidation above.
        // An account's empty-wallet latch is a fact about the wallet, not the
        // secret, and is deliberately left alone.
        let rekeyed_keys = rekeyed_inference_keys(current.config(), registry.config());
        #[cfg(feature = "account")]
        self.accounts.sync_declarations(&registry.snapshot());
        // The standing election's ranking belongs to the configuration being
        // replaced. Dropping it here, under the same write lock as the swap,
        // makes "invalidate" and "swap" one indivisible step: a publication
        // from a `hold_election` that was still probing decides under this
        // very lock whether the snapshot it measured is still live, so a swap
        // either erases the cache before the late result arrives or makes it
        // refuse to install. Neither order can resurrect the other's casualty.
        if ranking_stale {
            self.router.invalidate_election();
        }
        for (provider, account, key) in &rekeyed_keys {
            self.router.invalidate_key_cooldowns(provider, account, key);
        }
        *current = registry;
    }

    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&crate::sync::read(&self.registry))
    }

    /// Resolve a provider's secret, failing loudly when one is expected but absent.
    pub fn api_key(&self, provider: &ProviderConfig) -> Result<Option<String>> {
        if provider.kind == ProviderKind::OpenCodeZenFree {
            return Ok(Some("public".into()));
        }
        if provider.key_ref.is_empty() {
            return Ok(None);
        }
        match self.secrets.get(&provider.key_ref) {
            Some(k) if !k.is_empty() => Ok(Some(k)),
            // The free tier is reachable without a credential — its gate is
            // the request shape, not a key. A provider preset that carries a
            // key_ref but never stored a secret is anonymous here, not
            // misconfigured, and failing it would take the whole tier down for
            // every member that happens to be reachable for free.
            _ if provider.kind == ProviderKind::OpenCodeZenFree => Ok(None),
            _ => Err(Error::MissingApiKey(provider.name.clone())),
        }
    }

    /// Resolve the OpenCode Console credential (access token + billing org),
    /// refreshing it if needed. Only meaningful for an `OpenCodeConsole` provider.
    #[cfg(feature = "opencode")]
    pub async fn console_credential(
        &self,
        provider: &ProviderConfig,
    ) -> Result<(String, Option<String>)> {
        crate::opencode::console::resolve(self.secrets.as_ref(), &provider.key_ref).await
    }

    /// Probe every tier member, rank the results, and pin the outcome.
    ///
    /// The runner lives here rather than in [`crate::election`] because it needs the
    /// registry, the HTTP client and the secret store, and keeping those out of the
    /// scoring module is what lets the scoring be a pure function with tests that
    /// never open a socket.
    ///
    /// Probes run one at a time on purpose: fired together they would queue behind
    /// each other on the same uplink and measure the queue instead of the providers.
    pub async fn hold_election(&self) -> Election {
        let registry = self.registry();
        let scoring = registry.config().routing.scoring.clone();
        let mut election = Election::new(scoring.clone());

        for tier in ModelTier::ALL {
            let members = registry.tier_members(tier);
            if members.is_empty() {
                continue;
            }

            let mut measurements = Vec::with_capacity(members.len());
            for entry in members {
                // An OpenCode provider cannot be probed: the gate forces a
                // streaming request a non-streaming probe cannot satisfy.
                if registry
                    .provider_of(entry)
                    .is_ok_and(|provider| provider.kind.is_opencode())
                {
                    continue;
                }
                let exposed = entry.exposed_id();
                let mut measurement = Measurement::new(&exposed);
                measurement.priority = entry.priority;
                measurement.price = entry.pricing.as_ref().map(|p| scoring.reference_cost(p));

                measurement = match registry
                    .provider_of(entry)
                    .and_then(|provider| Ok((provider, self.api_key(provider)?)))
                {
                    Ok((provider, key)) => {
                        match self
                            .upstream
                            .probe(provider, key.as_deref(), &entry.upstream_model)
                            .await
                        {
                            Ok(latency) => {
                                // A probe is a real call, so it is also the freshest
                                // health signal there is.
                                let routing = &registry.config().routing;
                                self.router.report_success(&exposed, latency, routing);
                                measurement.answered(latency)
                            }
                            Err(e) => {
                                let routing = &registry.config().routing;
                                self.router.report_failure(&exposed, &e, routing);
                                measurement.failed(e.to_string())
                            }
                        }
                    }
                    Err(e) => measurement.failed(e.to_string()),
                };
                measurements.push(measurement);
            }

            election
                .tiers
                .insert(tier, election::rank(tier, &measurements, &scoring));
        }

        // The probes awaited, so a configuration swap may have replaced the
        // snapshot they ran against — and invalidated the cache with it.
        // Install the ranking only while that snapshot is still live.
        if !self.publish_election(&registry, election.clone()) {
            tracing::info!(
                "an election finished after a configuration swap; its ranking was measured \
                 against the replaced snapshot and was not installed"
            );
        }
        election
    }

    /// Install an election only while the snapshot it measured is still live.
    ///
    /// [`Self::hold_election`] awaits real probes, so a configuration swap can
    /// land in the middle of one, and [`Self::set_config`] drops the cache
    /// when it does. Installing the stale ranking afterwards would quietly
    /// resurrect what the swap dropped. The install is therefore a compare
    /// and set against the registry itself: the election keeps the snapshot
    /// it measured, and it wins the cache only if that exact snapshot is
    /// still the live one. Every swap builds a fresh [`Registry`], so pointer
    /// equality is exactly "nothing was swapped since the election started".
    ///
    /// The check and the install share the registry's write lock with
    /// [`Self::set_config`]'s swap, so a swap cannot slip between them. Every
    /// lock here is taken registry first and election second, and nothing
    /// takes the registry lock while holding the election lock, so
    /// publication cannot deadlock against `set_config` or a routing decision
    /// in flight. No lock is held across a probe await.
    fn publish_election(&self, measured: &Arc<Registry>, election: Election) -> bool {
        let live = crate::sync::write(&self.registry);
        if !Arc::ptr_eq(&*live, measured) {
            return false;
        }
        self.router.set_election(election);
        true
    }
}

/// Inference keys whose secret reference changed between two configurations.
///
/// Keys are paired by `(provider id, account id, key id)`, the identity the
/// router uses for credential cooldowns, and only a changed `key_ref` counts:
/// renaming an account, reordering keys or editing a note moves no secret and
/// leaves every gate standing. Added and removed keys produce no entry — a
/// removed key's gates are dropped by `retain_credentials`, and a new identity
/// has no evidence to forget.
fn rekeyed_inference_keys(previous: &AppConfig, next: &AppConfig) -> Vec<(String, String, String)> {
    let mut rekeyed = Vec::new();
    let mut unpaired: std::collections::HashMap<&str, &ProviderConfig> = previous
        .providers
        .iter()
        .map(|provider| (provider.id.as_str(), provider))
        .collect();
    for provider in &next.providers {
        let Some(old_provider) = unpaired.remove(provider.id.as_str()) else {
            continue;
        };
        for account in &provider.accounts {
            let Some(old_account) = old_provider
                .accounts
                .iter()
                .find(|old| old.account_id == account.account_id)
            else {
                continue;
            };
            for key in &account.inference_keys {
                let Some(old_key) = old_account
                    .inference_keys
                    .iter()
                    .find(|old| old.id == key.id)
                else {
                    continue;
                };
                if old_key.key_ref != key.key_ref {
                    rekeyed.push((
                        provider.id.clone(),
                        account.account_id.clone(),
                        key.id.clone(),
                    ));
                }
            }
        }
    }
    rekeyed
}

/// Whether the configuration about to be swapped in makes the standing
/// election's pinned ranking stale.
///
/// The election measured three inputs per member and scored them with the
/// configured weights. When any of them changes, the pinned order no longer
/// describes the configuration it would order, so the cache must be dropped
/// and re-measured:
///
/// * `routing.scoring` — the price/latency weights and the reference request
///   every price is compared through;
/// * a provider's declaration — endpoint, protocol, credential and accounts.
///   The probe ran through exactly these, and an account's binding, keys and
///   effective rates are what a quote is bound to, so any credential, billing
///   group or outbound change starts from a fresh measurement. Only the
///   provider's display name is exempt, because no measurement depends on it;
/// * an existing model's price or manual priority, paired by identity
///   `(provider_id, upstream_model)`.
///
/// Deliberately *not* invalidating:
///
/// * a model the old configuration did not have. It was never measured, so it
///   cannot take a primary slot it did not earn: the pinned order keeps it
///   behind the measured members until the next election promotes it. This is
///   the contract `a_model_added_after_an_election_is_used_but_not_promoted`
///   pins in the e2e suite;
/// * display-only edits (a model's display name or aliases) and membership
///   flips (tier, enabled, removal). The pinned order is filtered against the
///   live tier membership when it is used, so members that left are skipped
///   and members it never saw fall back to priority. Probes cost real
///   requests, so an edit that cannot change the ranking must not throw the
///   measurement away.
fn election_inputs_changed(previous: &AppConfig, next: &AppConfig) -> bool {
    if previous.routing.scoring != next.routing.scoring {
        return true;
    }
    if election_providers_changed(&previous.providers, &next.providers) {
        return true;
    }
    election_models_changed(&previous.models, &next.models)
}

/// Whether any provider changed in a way the election measures.
///
/// Providers are paired by id, so reordering them alone keeps the standing
/// measurement. Everything but the display name takes part, and new provider
/// fields join the comparison by existing — the safe default for a probe
/// that is a real call against real credentials.
fn election_providers_changed(previous: &[ProviderConfig], next: &[ProviderConfig]) -> bool {
    if previous.len() != next.len() {
        return true;
    }
    let mut unpaired: std::collections::HashMap<&str, &ProviderConfig> = previous
        .iter()
        .map(|provider| (provider.id.as_str(), provider))
        .collect();
    next.iter().any(|provider| {
        unpaired
            .remove(provider.id.as_str())
            .is_none_or(|old| election_provider_view(old) != election_provider_view(provider))
    })
}

/// The provider as the election sees it: its whole declaration minus the GUI
/// display name.
fn election_provider_view(provider: &ProviderConfig) -> ProviderConfig {
    let mut view = provider.clone();
    view.name = String::new();
    view
}

/// Whether a model the old configuration already had changed an input the
/// election measures: its price, or its manual priority (the rank tie-break).
///
/// Paired by `(provider_id, upstream_model)`. A key absent from the old side
/// is a new model and does not invalidate; an old key that vanished simply
/// stops matching and leaves the rest of the pinned order standing.
fn election_models_changed(previous: &[ModelEntry], next: &[ModelEntry]) -> bool {
    let mut unpaired: std::collections::HashMap<(&str, &str), &ModelEntry> =
        previous.iter().map(|model| (model.key(), model)).collect();
    next.iter().any(|model| {
        unpaired
            .remove(&model.key())
            .is_some_and(|old| old.pricing != model.pricing || old.priority != model.priority)
    })
}
