//! Candidate selection, health tracking and failover ordering.
//!
//! The router is long lived: it keeps health state across configuration
//! reloads, keyed by exposed model id.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`candidate`] | [`Candidate`], [`ModelHealth`], and how a failure is disposed of |
//! | [`plan`] | turning a resolved model into an ordered list of attempts |
//! | [`candidates`] | narrowing a pool to what may be tried, and scoring it |
//! | [`classifier`] | the Auto Mode classifier pool, which shares the tier implementation |
//! | [`selection`] | ordering the pool once known: strategy, election, round robin |
//! | [`health`] | reading health, and what a failure means for retry and fallback |
//! | [`reporting`] | writing health back: observations, circuit state, counters |
//! | [`tiers`] | tier escalation, and the per-model health record the router keeps |
//!
//! The first seven are the method groups of one type. Rust merges split inherent
//! `impl` blocks, so this is legal rather than a workaround, but it does mean
//! the table above is the only map from a method name to a file.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::election::Election;
use crate::observation::ObservationStore;
use crate::stats_ext::StatsStore;
use tiers::HealthState;

/// Round robin cursor key for the classifier pool, which is not a tier.
const CLASSIFIER_POOL: &str = "classifier";

#[derive(Debug)]
pub struct Router {
    health: Mutex<HashMap<String, HealthState>>,
    /// Round robin cursors, keyed by pool name (a tier's virtual id, or the
    /// classifier pool) rather than by `ModelTier`: routing pools exist that
    /// are not a tier.
    rr: Mutex<HashMap<String, usize>>,
    /// The last election, when one has been held. `Balanced` follows the order it
    /// decided instead of re-deciding per request, which is the whole point: a
    /// route that changes under load cannot be reasoned about.
    election: Mutex<Option<Election>>,
    /// Runtime observations (latency, health, cost) keyed by provider+model.
    /// Used by the policy scorer as the primary signal, falling back to legacy
    /// circuit-breaker state when no observation exists.
    observations: ObservationStore,
    /// Runtime statistics (percentiles, EWMA, failure breakdown) keyed by
    /// provider+model. Accumulated over the lifetime of the process.
    pub stats_store: StatsStore,
    /// Providers whose rate limit is shared by every model they serve.
    ///
    /// OpenCode's free and Console tiers meter by account (or by source IP for
    /// the anonymous free tier), so a 429 on one model means every model under
    /// that provider is unusable at the same time. The set is refreshed from
    /// the configuration; a provider that is not listed keeps today's
    /// behaviour, where a rate limit is model-scoped and never opens a circuit.
    group_limited: Mutex<HashSet<String>>,
    /// Cooldown deadlines for shared rate limits or empty provider wallets,
    /// keyed by provider id. Wallet gates apply to every provider kind.
    ///
    /// Deliberately separate from the per-model health rows: the GUI lists
    /// models, and a synthetic "provider" row there would be a fact that does
    /// not exist. A gate is a time window, not a model circuit.
    provider_gates: Mutex<HashMap<String, std::time::Instant>>,
    account_gates: Mutex<HashMap<(String, String), std::time::Instant>>,
    key_gates: Mutex<HashMap<(String, String, String), std::time::Instant>>,
    wallet_holds: Mutex<std::collections::BTreeMap<String, WalletHold>>,
    /// When the hold map was last reconciled with the shared database.
    wallet_synced: Mutex<Option<std::time::Instant>>,
    runtime_store: Mutex<Option<std::sync::Arc<crate::runtime_store::RuntimeStore>>>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

mod candidate;
mod candidates;
mod classifier;
mod health;
mod helpers;
mod plan;
mod reporting;
mod selection;
mod tiers;
mod wallets;

#[cfg(test)]
mod tests;

pub use candidate::{Candidate, FailureDisposition, InferenceCredential, ModelHealth};
pub use tiers::{
    tier_escalation_change, tiers_fully_cooling, TierAnnouncement, TierEscalationChange,
};
pub use wallets::WalletHold;
