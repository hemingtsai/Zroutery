//! The retained projections, swapping configuration and the registry it feeds, and holding an election.

use std::sync::Arc;

use crate::config::{AppConfig, ModelTier, ProviderConfig, ProviderKind, SecretStore};
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
        #[cfg(feature = "account")]
        self.accounts.sync_declarations(&registry.snapshot());
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

        self.router.set_election(election.clone());
        election
    }
}
