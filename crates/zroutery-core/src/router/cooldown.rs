//! Time-bounded rate limits are availability gates, not model-health failures.

use std::time::{Duration, Instant};

use super::Router;
use crate::config::{ModelEntry, ProviderConfig};

impl Router {
    pub(super) fn extend_model_gate(&self, provider: &str, model: &str, wait: Duration) {
        let Some(until) = Instant::now().checked_add(wait) else {
            return;
        };
        crate::sync::lock(&self.model_gates)
            .entry((provider.to_string(), model.to_string()))
            .and_modify(|previous| *previous = (*previous).max(until))
            .or_insert(until);
    }

    /// Read-only availability check: consumes no half-open circuit permit.
    pub fn model_rate_limit_remaining(&self, provider: &str, model: &str) -> Option<Duration> {
        let now = Instant::now();
        let mut gates = crate::sync::lock(&self.model_gates);
        gates.retain(|_, until| *until > now);
        gates
            .get(&(provider.to_string(), model.to_string()))
            .map(|until| until.saturating_duration_since(now))
    }

    /// A default credential's window cannot block independently selected Keys.
    pub(super) fn default_model_rate_limit(
        &self,
        model: &ModelEntry,
        provider: &ProviderConfig,
    ) -> Option<Duration> {
        if provider
            .accounts
            .iter()
            .any(|account| !account.inference_keys.is_empty())
        {
            return None;
        }
        self.model_rate_limit_remaining(&provider.id, &model.exposed_id())
    }

    pub(super) fn rate_limited_pool(
        &self,
        registry: &crate::registry::Registry,
        members: &[&ModelEntry],
        scope: String,
    ) -> crate::error::Error {
        let waits: Option<Vec<_>> = members
            .iter()
            .map(|model| {
                let provider = registry.provider_of(model).ok()?;
                self.default_model_rate_limit(model, provider)
            })
            .collect();
        match waits.and_then(|waits| waits.into_iter().min()) {
            Some(retry_after) => crate::error::Error::RateLimited { scope, retry_after },
            None => crate::error::Error::NoCandidate(scope),
        }
    }
}

pub(super) fn remaining_secs(wait: Duration) -> u64 {
    wait.as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() > 0))
}
