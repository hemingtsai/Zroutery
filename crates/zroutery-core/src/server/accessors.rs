//! The read-only handles other subsystems hold on the running process.

use std::sync::Arc;

use super::state::AppState;
use crate::router::Router;
use crate::stats::{OutcomeLog, Stats};
use crate::upstream::Upstream;

impl AppState {
    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// The terminal outcomes the request lifecycle built, newest first.
    pub fn outcomes(&self) -> &OutcomeLog {
        &self.outcomes
    }

    pub fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    /// The shadow decision engine (record-only; never influences routing).
    #[cfg(feature = "ml")]
    pub fn shadow(&self) -> &crate::ml::ShadowEngine {
        &self.shadow
    }

    /// The canonical training dataset.
    ///
    /// Read by the offline half of the loop — training, comparison, promotion —
    /// and written from the request path. Nothing on the request path reads it
    /// to make a decision.
    #[cfg(feature = "ml")]
    pub fn dataset(&self) -> &crate::ml::DatasetStore {
        &self.dataset
    }

    /// Runtime state for the accounts the configured providers host.
    ///
    /// Absent without the `account` feature rather than empty, so a build that
    /// ships no account subsystem says so instead of reporting zero accounts —
    /// the same distinction `ml_status` makes between "no model" and "no model
    /// store".
    #[cfg(feature = "account")]
    pub fn accounts(&self) -> &crate::account::AccountStore {
        &self.accounts
    }

    /// The durable routing trace log.
    #[cfg(feature = "ml")]
    pub fn traces(&self) -> Option<&crate::ml::TraceLog> {
        self.traces.as_ref()
    }

    /// The learned model's role in serving.
    ///
    /// The request path may *rank* through this and may not obtain a predictor
    /// from it. Ranking needs nothing but the plan and the model's opinion;
    /// handing out the predictor would put the whole artifact one call away from
    /// a response, which is a different and much larger surface than the one a
    /// routing strategy needs.
    #[cfg(feature = "ml")]
    pub fn ml_routing(&self) -> &crate::ml::MlRouter {
        &self.ml_routing
    }

    /// The durable active-model pointer, when a state directory was available.
    #[cfg(feature = "ml")]
    pub fn active_models(&self) -> Option<&crate::ml::ActiveModelStore> {
        self.active_models.as_ref()
    }
}
