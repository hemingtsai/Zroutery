use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::ml::coordinator::RoutingAction;
use crate::ml::features::FEATURE_SCHEMA_VERSION;
use crate::ml::model::Prediction;
use crate::ml::reward::{PredictionBundle, UtilityBreakdown};

use super::records::{shadow_decision_checksum, shadow_input_checksum, ShadowDecision};

// ---------------------------------------------------------------------------
// Finiteness helpers
// ---------------------------------------------------------------------------

/// Whether both f64 fields of a prediction are finite.
fn prediction_finite(prediction: &Prediction) -> bool {
    prediction.value.is_finite() && prediction.confidence.is_finite()
}

/// Name of the first non-finite utility component, or `None` when all are
/// finite.
fn utility_non_finite(utility: &UtilityBreakdown) -> Option<&'static str> {
    [
        ("success", utility.success),
        ("latency", utility.latency),
        ("ttft", utility.ttft),
        ("cost", utility.cost),
        ("fallback", utility.fallback),
        ("uncertainty", utility.uncertainty),
        ("switch_cost", utility.switch_cost),
        ("total", utility.total),
    ]
    .into_iter()
    .find(|(_, value)| !value.is_finite())
    .map(|(name, _)| name)
}

/// Replace non-finite predictions with cold zeros so the stored evidence
/// stays finite; the candidate's rejection reason records why.
pub(super) fn sanitized_bundle(bundle: &PredictionBundle) -> PredictionBundle {
    let sanitize = |prediction: &Prediction| {
        if prediction_finite(prediction) {
            prediction.clone()
        } else {
            Prediction::cold(0.0)
        }
    };
    PredictionBundle {
        candidate_model: bundle.candidate_model.clone(),
        candidate_provider: bundle.candidate_provider.clone(),
        success: sanitize(&bundle.success),
        latency: sanitize(&bundle.latency),
        ttft: sanitize(&bundle.ttft),
        cost: sanitize(&bundle.cost),
    }
}

// ---------------------------------------------------------------------------
// ShadowStore — bounded storage for shadow decisions
// ---------------------------------------------------------------------------

/// Bounded store for shadow decisions with retention, mirroring
/// [`DatasetStore`](super::dataset::DatasetStore).
///
/// The retention limits are interior-mutable on purpose: they come from
/// `ShadowConfig` and a running engine has to adopt a configuration change
/// without being rebuilt ([`ShadowStore::set_limits`]).
pub struct ShadowStore {
    decisions: Mutex<VecDeque<ShadowDecision>>,
    max_decisions: AtomicUsize,
    max_age_secs: AtomicI64,
}

/// Standalone-store default: 100k decisions, 30 days. The server never uses
/// this — it passes the configured `ShadowConfig` limits — but the bare
/// constructor and [`ShadowStore::default`] keep it as their documented
/// fallback.
pub(super) const DEFAULT_MAX_DECISIONS: usize = 100_000;
pub(super) const DEFAULT_MAX_AGE_SECS: i64 = 30 * 24 * 3600;

impl ShadowStore {
    pub fn new(max_decisions: usize, max_age_secs: i64) -> Self {
        Self {
            decisions: Mutex::new(VecDeque::with_capacity(max_decisions.min(10_000))),
            max_decisions: AtomicUsize::new(max_decisions),
            max_age_secs: AtomicI64::new(max_age_secs),
        }
    }

    /// Current retention limits as `(max_decisions, max_age_secs)`.
    pub fn limits(&self) -> (usize, i64) {
        (
            self.max_decisions.load(Ordering::Acquire),
            self.max_age_secs.load(Ordering::Acquire),
        )
    }

    /// Apply new retention limits to a live store and enforce them at once.
    ///
    /// Shrinking `max_decisions` evicts the oldest decisions down to the new
    /// capacity, and shrinking `max_age_secs` drops decisions that are already
    /// outside the new window, so what the store serves after this call is the
    /// new retention rather than whatever the previous limits accumulated.
    /// Returns how many decisions were dropped.
    pub fn set_limits(&self, max_decisions: usize, max_age_secs: i64) -> usize {
        self.max_decisions.store(max_decisions, Ordering::Release);
        self.max_age_secs.store(max_age_secs, Ordering::Release);
        let now = chrono::Utc::now().timestamp();
        let mut decisions = crate::sync::lock(&self.decisions);
        let before = decisions.len();
        while decisions.len() > max_decisions {
            decisions.pop_front();
        }
        decisions.retain(|decision| now - decision.timestamp < max_age_secs);
        before - decisions.len()
    }

    /// Store-boundary gate enforcement.
    ///
    /// A stored record is a replayable observation, not merely a rendered
    /// verdict. The store therefore verifies the retained input/commit
    /// relationship, candidate evidence identity, finite values, ranking
    /// eligibility, and both checksums before accepting the record.
    pub fn push(&self, decision: ShadowDecision) -> Result<(), String> {
        if decision.actual.request_id.is_empty() {
            return Err("empty request_id".to_string());
        }
        if decision.actual.decision_id.is_empty() {
            return Err("empty decision_id".to_string());
        }
        if decision.actual.selected.is_empty() {
            return Err("empty production selected model".to_string());
        }
        if decision.shadow.selected.is_empty() {
            return Err("empty shadow selected identity".to_string());
        }
        if decision.shadow.model_commit.as_str().is_empty() {
            return Err("empty model_commit".to_string());
        }
        if decision.observation.model_commit.as_str().is_empty() {
            return Err("empty observation model_commit".to_string());
        }
        if decision.shadow.model_commit != decision.observation.model_commit {
            return Err("observation model_commit does not match shadow verdict".to_string());
        }
        if decision.actual.decision_id != decision.observation.input.decision_id {
            return Err("observation decision_id does not match actual decision".to_string());
        }
        if decision.actual.selected != decision.observation.input.production_selected {
            return Err("observation planned selection does not match actual decision".to_string());
        }
        if decision.shadow.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(format!(
                "feature schema mismatch: {} != {}",
                decision.shadow.feature_schema, FEATURE_SCHEMA_VERSION
            ));
        }
        if decision.observation.input.feature_schema != FEATURE_SCHEMA_VERSION {
            return Err(format!(
                "observation feature schema mismatch: {} != {}",
                decision.observation.input.feature_schema, FEATURE_SCHEMA_VERSION
            ));
        }
        if decision.shadow.feature_schema != decision.observation.input.feature_schema {
            return Err("shadow and observation feature schemas differ".to_string());
        }
        if decision.observation.input.candidates.is_empty() {
            return Err("observation has no candidates".to_string());
        }
        if decision.candidates.is_empty() {
            return Err("no candidates".to_string());
        }
        if decision.observation.input.candidates.len() != decision.candidates.len() {
            return Err("observation and evidence candidate counts differ".to_string());
        }
        if decision.decision_input_checksum == 0 {
            return Err("zero decision_input_checksum".to_string());
        }
        if decision.decision_checksum == 0 {
            return Err("zero decision_checksum".to_string());
        }

        let mut ranked = HashSet::new();
        for candidate_id in &decision.shadow.ranked_candidates {
            if !ranked.insert(candidate_id.clone()) {
                return Err(format!("duplicate ranked candidate '{candidate_id}'"));
            }
            let Some(candidate) = decision
                .candidates
                .iter()
                .find(|candidate| &candidate.candidate_id == candidate_id)
            else {
                return Err(format!("ranked candidate '{candidate_id}' has no evidence"));
            };
            if !candidate.eligible || !candidate.valid {
                return Err(format!(
                    "ineligible or invalid candidate '{candidate_id}' cannot be ranked"
                ));
            }
        }
        for (input_candidate, candidate) in decision
            .observation
            .input
            .candidates
            .iter()
            .zip(decision.candidates.iter())
        {
            if input_candidate.candidate_id != candidate.candidate_id
                || input_candidate.provider_id != candidate.provider_id
                || input_candidate.tier != candidate.tier
                || input_candidate.eligible != candidate.eligible
            {
                return Err(format!(
                    "candidate '{}' evidence does not match observation input",
                    candidate.candidate_id
                ));
            }
            if input_candidate.rejection_reason.is_some()
                && input_candidate.rejection_reason != candidate.rejection_reason
            {
                return Err(format!(
                    "candidate '{}' rejection evidence does not match observation input",
                    candidate.candidate_id
                ));
            }
            if input_candidate.features.schema_version != FEATURE_SCHEMA_VERSION
                || input_candidate
                    .features
                    .values
                    .iter()
                    .any(|value| !value.is_finite())
            {
                return Err(format!(
                    "candidate '{}' has an invalid feature snapshot",
                    candidate.candidate_id
                ));
            }
            if candidate.prediction.candidate_model != candidate.candidate_id
                || candidate.prediction.candidate_provider != candidate.provider_id
            {
                return Err(format!(
                    "candidate '{}' prediction identity does not match evidence identity",
                    candidate.candidate_id
                ));
            }
            let bundle = &candidate.prediction;
            for (model, prediction) in [
                ("success", &bundle.success),
                ("latency", &bundle.latency),
                ("ttft", &bundle.ttft),
                ("cost", &bundle.cost),
            ] {
                if !prediction_finite(prediction) {
                    return Err(format!(
                        "candidate '{}' has a non-finite {} prediction",
                        candidate.candidate_id, model
                    ));
                }
            }
            if let Some(component) = utility_non_finite(&candidate.utility) {
                return Err(format!(
                    "candidate '{}' has a non-finite {} utility",
                    candidate.candidate_id, component
                ));
            }
        }

        // The selected identity is a semantic part of the record, not merely
        // a checksum field. Resolve it explicitly before checksum validation so
        // an unknown identity cannot be hidden behind a checksum mismatch.
        let selected_candidate = decision
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == decision.shadow.selected)
            .ok_or_else(|| {
                format!(
                    "unknown shadow selected identity '{}': candidate evidence is missing",
                    decision.shadow.selected
                )
            })?;

        match decision.shadow.action {
            RoutingAction::Keep => {
                if decision.shadow.selected != decision.actual.selected {
                    return Err(
                        "Keep action must select the planned production identity".to_string()
                    );
                }
                // Keep deliberately permits the planned identity to be invalid;
                // it is the non-executable terminal state used when no valid
                // candidate or a session guard applies.
            }
            RoutingAction::Switch | RoutingAction::Explore => {
                if !selected_candidate.eligible || !selected_candidate.valid {
                    return Err(format!(
                        "{:?} action selected identity '{}' must be eligible and valid",
                        decision.shadow.action, decision.shadow.selected
                    ));
                }
                if !ranked.contains(&decision.shadow.selected) {
                    return Err(format!(
                        "{:?} action selected identity '{}' is not in the ranked valid set",
                        decision.shadow.action, decision.shadow.selected
                    ));
                }
                if decision.shadow.selected == decision.actual.selected {
                    return Err(format!(
                        "{:?} action must select an identity different from the planned selection",
                        decision.shadow.action
                    ));
                }
            }
        }

        let valid_candidate_ids: HashSet<String> = decision
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible && candidate.valid)
            .map(|candidate| candidate.candidate_id.clone())
            .collect();
        if ranked != valid_candidate_ids {
            return Err("ranked candidates do not match the valid evidence set".to_string());
        }

        let expected_input_checksum = shadow_input_checksum(
            &decision.observation.input,
            &decision.observation.model_commit,
        );
        if expected_input_checksum != decision.decision_input_checksum {
            return Err("decision_input_checksum does not match retained observation".to_string());
        }
        let expected_decision_checksum = shadow_decision_checksum(
            decision.decision_input_checksum,
            &decision.candidates,
            &decision.shadow.ranked_candidates,
            &decision.shadow.selected,
            decision.shadow.action,
            &decision.shadow.reason,
        );
        if expected_decision_checksum != decision.decision_checksum {
            return Err(format!(
                "decision_checksum does not match retained verdict evidence: expected {}, got {}",
                expected_decision_checksum, decision.decision_checksum
            ));
        }

        let mut decisions = crate::sync::lock(&self.decisions);
        let max_decisions = self.max_decisions.load(Ordering::Acquire);
        if decisions.len() >= max_decisions {
            decisions.pop_front();
        }
        decisions.push_back(decision);
        Ok(())
    }

    /// Attach a production served identity to one already-stored decision.
    ///
    /// The served identity is a *terminal* fact: it only exists once the
    /// request has actually delivered an answer, which is necessarily after the
    /// counterfactual was recorded. This attaches that fact to the record the
    /// decision-time evaluation produced, and deliberately does nothing else —
    /// the decision identity (both checksums) was computed without it, so the
    /// stored counterfactual stays exactly replayable.
    ///
    /// `None` means "nothing was served" and leaves the record untouched, so a
    /// failed, cancelled or interrupted request can never be correlated as a
    /// success. An unknown `shadow_id` is a no-op rather than an error: the
    /// bounded store may legitimately have evicted the record by then, and that
    /// is never a fact about the request.
    pub fn correlate_served(&self, shadow_id: &str, served: Option<&str>) -> Result<bool, String> {
        let Some(served) = served else {
            return Ok(false);
        };
        if served.trim().is_empty() {
            return Err("empty served identity".to_string());
        }
        let mut decisions = crate::sync::lock(&self.decisions);
        let Some(decision) = decisions
            .iter_mut()
            .find(|entry| entry.shadow_id == shadow_id)
        else {
            return Ok(false);
        };
        // The served identity has to be one production actually chose from the
        // candidates this very record observed. Anything else is a wiring fault,
        // not evidence, and is refused rather than stored.
        if !decision
            .observation
            .input
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_id == served)
        {
            return Err(format!(
                "served identity '{served}' is not among the observed candidates"
            ));
        }
        if let Some(existing) = decision.actual.served.as_deref() {
            if existing != served {
                return Err(format!(
                    "decision already correlates served identity '{existing}'"
                ));
            }
            return Ok(false);
        }
        decision.actual.served = Some(served.to_string());
        Ok(true)
    }

    /// Number of decisions currently stored (regardless of age).
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.decisions).len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        crate::sync::lock(&self.decisions).is_empty()
    }

    /// Clear all decisions.
    pub fn clear(&self) {
        crate::sync::lock(&self.decisions).clear();
    }

    /// Decisions within the retention window, oldest first.
    pub fn decisions(&self) -> Vec<ShadowDecision> {
        let now = chrono::Utc::now().timestamp();
        let max_age_secs = self.max_age_secs.load(Ordering::Acquire);
        crate::sync::lock(&self.decisions)
            .iter()
            .filter(|d| now - d.timestamp < max_age_secs)
            .cloned()
            .collect()
    }
}

impl Default for ShadowStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_DECISIONS, DEFAULT_MAX_AGE_SECS)
    }
}
