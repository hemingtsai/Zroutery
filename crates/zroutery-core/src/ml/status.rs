//! What an operator can see about the learned model.
//!
//! # Why this exists
//!
//! Everything the ML stack knows was, until now, reachable only from Rust. An
//! operator could not answer the only question that matters in production —
//! *what is routing my requests right now, and who authorised it?* — without a
//! debugger.
//!
//! A status document is not a dashboard. It is the minimum an operator needs to
//! decide whether the mechanism is doing what it claims:
//!
//! * is a model attached, and which commit is it;
//! * which gate decision promoted it, over which body of evidence;
//! * is routing deterministic or model-driven right now;
//! * how often the model ranked, fell back, and explored;
//! * which configured candidates hold no evidence, and whether anything in the
//!   configuration can ever change that;
//! * what has been promoted and rolled back, and when.
//!
//! # Read-only, and honestly so
//!
//! Every field is read from the live component. Nothing here recomputes a
//! verdict, and nothing here reports a healthy-looking number for a model that
//! is not attached: [`MlStatus::active`] is `None` in exactly the state where no
//! model is serving, which is the state a fresh installation is in and the state
//! a rollback returns to.
//!
//! `shadow` and `dataset` counters are reported as counters, not as quality
//! metrics. A count of collected samples says collection is happening; it says
//! nothing about whether the model is any good, and presenting it as if it did
//! is the confusion this whole module tree exists to avoid.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::dataset::IngestionCounters;
use super::promotion::{PromotionDecision, PromotionVerdict};
use super::serving::{ActiveModelAction, ActiveModelAuditEntry, MlRouterCounts};
use super::shadow_analysis::ShadowAnalysis;
use super::traces::TraceCounters;
use crate::observation::ObservationStore;

/// A read-only view of one promotion, as an operator sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotedModelStatus {
    pub model_id: String,
    pub commit_id: String,
    /// The gate verdict that authorised it.
    pub verdict: PromotionVerdict,
    /// The body of evidence it was judged on.
    pub dataset_fingerprint: String,
    /// The partition it was fitted on, which is smaller.
    pub fitted_partition_fingerprint: String,
    /// Identity of the gate configuration in force at the time.
    pub gate_config_identity: String,
    /// The baseline it was required to beat, by name.
    pub required_baseline: String,
    /// Paired requests the decision rested on.
    pub paired_requests: usize,
    /// The out-of-sample loss the decision recorded.
    pub holdout_loss: f64,
    /// Unix seconds.
    pub promoted_at: i64,
}

/// One entry in the promotion history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionHistoryEntry {
    pub action: ActiveModelAction,
    pub commit_id: String,
    pub gate_identity: String,
    pub verdict: PromotionVerdict,
    pub at: i64,
    pub note: String,
}

/// The shadow engine's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowStatus {
    pub enabled: bool,
    pub decisions_recorded: usize,
    /// Evaluation faults it absorbed, which is the number that says the fault
    /// containment is doing its job.
    pub faults: u64,
}

/// A configured candidate the router holds no evidence about.
///
/// # Why this needs naming
///
/// `ml_routing.exploration_probability` ships at `0.0`, and `explore` returns
/// `Exploit` at zero *before it draws*. So a candidate the deterministic plan
/// never picks receives traffic from no source at all: not from the model,
/// because the model has no observation of it, and not from exploration,
/// because exploration is off. It cannot accumulate evidence, so it can never be
/// discovered to be better than the candidate that shadows it.
///
/// Adding a provider to the configuration therefore looks like adding a
/// capability and is in fact inert, and nothing anywhere said so. This type is
/// the report: it names the affected candidates, and
/// [`MlStatus::blind_spots_are_permanent`] says whether the configuration in
/// force can change that.
///
/// # What "no evidence" means precisely
///
/// `total_requests == 0`, which the store reports for a key it has never
/// written. Note this is *not* "no successful requests": a candidate tried
/// three times and failed every time is observed and is correctly absent here.
/// Conversely a candidate whose only traffic was a neutral failure — one that
/// `affects_observation` rejects, such as a 4xx — never reaches the store at
/// all, and is correctly present. In both cases the reading is the same one the
/// ranking features get: nothing to rank it with.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlindCandidate {
    /// The exposed model id — the same key the router records outcomes under.
    pub model_id: String,
    /// The provider that would serve it.
    pub provider_id: String,
}

impl BlindCandidate {
    /// The configured candidates the observation store holds nothing for.
    ///
    /// Takes pairs rather than an `AppConfig` so this stays a pure function of
    /// what it was handed, and so that deciding what counts as "configured" —
    /// enabled provider, enabled model entry — stays with the caller that owns
    /// the configuration, instead of being restated here where it would drift
    /// out of step with the classifier's own view.
    ///
    /// Sorted by `(model_id, provider_id)` so consecutive reads diff cleanly.
    /// The configuration's order is not meaningful to this question.
    pub fn unobserved<'a, I>(candidates: I, observations: &ObservationStore) -> Vec<Self>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut blind: Vec<Self> = candidates
            .into_iter()
            .filter(|(model_id, provider_id)| {
                observations.get(model_id, provider_id).health.total_requests == 0
            })
            .map(|(model_id, provider_id)| Self {
                model_id: model_id.to_string(),
                provider_id: provider_id.to_string(),
            })
            .collect();
        blind.sort();
        blind.dedup();
        blind
    }
}

/// The operator-facing result of running one promotion round.
///
/// Split from `ShadowAnalysisStatus` because the two answer different questions and
/// only one of them is about the live router. A shadow analysis is a measurement
/// over history and can always be produced. A promotion round *changes* the model
/// that will serve traffic, so its result carries the routing state it left behind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionRoundStatus {
    /// Traces the round read.
    pub traces_read: usize,
    /// The commit the round fitted, when it got that far.
    pub candidate_commit: Option<String>,
    /// The gate's verdict in full, when there was one.
    pub decision: Option<crate::ml::promotion::PromotionDecision>,
    /// What the model would have done with the same traffic.
    pub analysis: Option<crate::ml::shadow_analysis::ShadowAnalysis>,
    /// How often the learned policy named each candidate.
    ///
    /// The one number from the round that is worth having at a glance: a learned
    /// ranking that named a single candidate on every request has not learned a
    /// ranking, whatever the gate said.
    pub policy_choices: BTreeMap<String, usize>,
    /// Whether the model was installed, when installation was asked for.
    pub installed: Option<String>,
    /// The routing state after the round.
    pub reload: ReloadOutcome,
    /// Why no round could be run, when none was.
    pub reason: Option<String>,
    /// A fault that did not prevent the round from being reported.
    pub error: Option<String>,
}

impl PromotionRoundStatus {
    /// A round that never got as far as a verdict, and why.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            traces_read: 0,
            candidate_commit: None,
            decision: None,
            analysis: None,
            policy_choices: BTreeMap::new(),
            installed: None,
            reload: ReloadOutcome::ok(false, None),
            reason: Some(reason.into()),
            error: None,
        }
    }

    /// A round that ran, whether or not anything was installed.
    pub fn from_round(round: &crate::ml::round::PromotionRound, reload: ReloadOutcome) -> Self {
        let installed = match &reload.error {
            None => reload.commit_id.clone(),
            Some(_) => None,
        };
        let mut choices: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(arm) = round.comparison.arm("ml.candidate") {
            for (candidate, count) in &arm.selections {
                *choices.entry(candidate.clone()).or_insert(0) += count;
            }
        }
        Self {
            traces_read: round.traces_read,
            candidate_commit: Some(round.training.commit_id.as_str().to_string()),
            decision: Some(round.decision.clone()),
            analysis: Some(round.analysis.clone()),
            policy_choices: choices,
            installed,
            reload,
            reason: None,
            error: None,
        }
    }

    /// Whether the gate authorised a model, regardless of whether it was installed.
    pub fn gate_authorised(&self) -> bool {
        self.decision
            .as_ref()
            .is_some_and(|decision| decision.verdict == crate::ml::promotion::PromotionVerdict::Promoted)
    }

    /// Whether a model is serving as a result of this round.
    pub fn is_serving(&self) -> bool {
        self.installed.is_some()
    }
}

/// The whole operator-facing document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MlStatus {
    /// Whether a promoted model may re-order the provider plan.
    pub routing_enabled: bool,
    /// Whether a durable state directory is configured, and therefore whether
    /// history and promotion can work at all.
    pub durable_state: bool,
    /// Whether the durable log is currently open.
    pub traces_open: bool,
    /// Whether an active-model store is currently open.
    pub model_store_open: bool,
    /// The serving model, or `None` when routing is deterministic.
    pub active: Option<PromotedModelStatus>,
    /// The gate decision behind `active`, in full, so a reader can see which
    /// criteria held and which did not.
    pub active_decision: Option<PromotionDecision>,
    /// The most recent promotions and rollbacks, newest last.
    pub history: Vec<PromotionHistoryEntry>,
    /// Counters for the serving path.
    pub routing: MlRouterCounts,
    /// Exploration in force right now.
    pub exploration_probability: f64,
    pub exploration_seed: u64,
    /// Configured candidates the router holds no evidence about.
    ///
    /// Empty on a fresh installation with one provider, and non-empty the
    /// moment a second provider is added and exploration is off. See
    /// [`BlindCandidate`] for why that is a defect rather than a policy.
    pub blind_candidates: Vec<BlindCandidate>,
    /// Counters for collection. Not quality metrics.
    pub dataset: IngestionCounters,
    /// Counters for durability. Not quality metrics.
    pub traces: Option<TraceCounters>,
    /// Counters for the shadow engine.
    pub shadow: ShadowStatus,
    /// Unix seconds when this snapshot was read.
    pub read_at: i64,
}

impl MlStatus {
    /// Whether a model is currently able to change a served request.
    ///
    /// Distinct from `active.is_some()`: a model can be promoted and stored
    /// while `routing_enabled` is false, in which case it is installed and
    /// deliberately inert.
    pub fn is_routing_with_a_model(&self) -> bool {
        self.routing_enabled && self.active.is_some()
    }

    /// A one-line summary for a log or a tray tooltip.
    pub fn headline(&self) -> String {
        match (&self.active, self.routing_enabled) {
            (Some(model), true) => format!(
                "ML routing on: {} at commit {} (promoted by gate {} over body {})",
                model.model_id,
                model.commit_id,
                &model.gate_config_identity[..8],
                &model.dataset_fingerprint[..8]
            ),
            (Some(model), false) => format!(
                "ML model {} is promoted but ml_routing.enabled is false; \
                 routing is deterministic",
                model.commit_id
            ),
            (None, true) => {
                "ml_routing.enabled is set but no model is attached; \
                 routing is deterministic"
                    .to_string()
            }
            (None, false) => "ML routing off; routing is deterministic".to_string(),
        }
    }

    /// Whether the blind spots in this document can ever be closed.
    ///
    /// True whenever exploration is off, which is the shipped default. This is
    /// the distinction that carries the whole finding: an unobserved candidate
    /// *with* exploration is a cold start that traffic resolves, and an
    /// unobserved candidate *without* it is permanent — the configuration cannot
    /// produce the evidence that would change the answer.
    ///
    /// The linkage to `explore` is asserted empirically rather than assumed;
    /// see `blind_spot_warning_matches_what_exploration_actually_does` in
    /// `crates/zroutery-core/tests/blind_spot_test.rs`.
    pub fn blind_spots_are_permanent(&self) -> bool {
        !self.blind_candidates.is_empty() && self.exploration_probability <= 0.0
    }

    /// The consequence, in a sentence an operator can act on.
    ///
    /// `None` when there is nothing to say: no blind candidates, or exploration
    /// on, in which case an unobserved candidate is a cold start that traffic
    /// resolves on its own and calling it a problem would be noise.
    pub fn blind_spot_warning(&self) -> Option<String> {
        if !self.blind_spots_are_permanent() {
            return None;
        }
        // `exposed_id` is already `provider-model`, so the provider is not
        // repeated: naming it twice reads as two different things to an operator
        // looking for a configuration entry.
        let named: Vec<&str> = self
            .blind_candidates
            .iter()
            .map(|blind| blind.model_id.as_str())
            .collect();
        // One plural decision, reused, rather than a sentence with agreement to
        // get wrong in three places.
        let them = if self.blind_candidates.len() == 1 {
            "it"
        } else {
            "them"
        };
        Some(format!(
            "{} — configured but never tried, and exploration is off, so no traffic \
             will ever reach {them}. Raising ml_routing.exploration_probability above 0 \
             does reach {them}, but it blocks promotion of any learned model: the gate \
             needs the model and the baseline compared on the same requests, and \
             exploration makes them disagree. Measured, not assumed — paired evidence \
             collapses by at least 6x and doubling the traffic does not recover it. So \
             routing would stay deterministic either way. Give {them} top priority \
             instead: that reaches {them} and keeps promotion possible.",
            named.join(", ")
        ))
    }
}

/// What re-reading the durable pointer did to the serving model.
///
/// A fault here is reported as `error` with the attached model left alone,
/// because the one thing an operator must not lose to a transient read error is
/// a model they chose. `attached` therefore describes the state after the call,
/// not the state that was requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadOutcome {
    /// Whether a model is attached to the router now.
    pub attached: bool,
    /// The commit that is attached, if any.
    pub commit_id: Option<String>,
    /// Why nothing was changed, when nothing was.
    pub error: Option<String>,
}

impl ReloadOutcome {
    pub fn ok(attached: bool, commit_id: Option<String>) -> Self {
        Self {
            attached,
            commit_id,
            error: None,
        }
    }

    pub fn fault(attached: bool, error: impl Into<String>) -> Self {
        Self {
            attached,
            commit_id: None,
            error: Some(error.into()),
        }
    }

    /// Whether the pointer was read and applied.
    pub fn is_clean(&self) -> bool {
        self.error.is_none()
    }
}

/// The outcome of asking "what would the model serving right now have done with
/// the traffic I actually served?".
///
/// The states are kept apart because they are different facts and an operator who
/// conflates them draws the wrong conclusion from each:
///
/// * `analysis` set — the model was replayed over real history;
/// * `reason` set with no analysis — it could not be, and the reason says which
///   precondition was missing;
/// * neither — it ran and found nothing evaluable, which is a real answer about
///   a thin history rather than a failure.
///
/// Running it costs a bounded read of the tail of the trace log, so it is an
/// action an operator takes rather than a field that is always populated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowAnalysisStatus {
    /// How many records were read, which is the bound applied and not a claim
    /// that this is all the history there is.
    pub traces_read: usize,
    /// The commit that was replayed, when one is attached.
    pub commit_id: Option<String>,
    /// Why no analysis could be produced.
    pub reason: Option<String>,
    pub analysis: Option<ShadowAnalysis>,
}

impl ShadowAnalysisStatus {
    /// An analysis that could not be produced, and why.
    pub fn unavailable(traces_read: usize, reason: impl Into<String>) -> Self {
        Self {
            traces_read,
            commit_id: None,
            reason: Some(reason.into()),
            analysis: None,
        }
    }

    /// Whether an analysis is present.
    pub fn is_analysed(&self) -> bool {
        self.analysis.is_some()
    }
}

impl From<&ActiveModelAuditEntry> for PromotionHistoryEntry {
    fn from(entry: &ActiveModelAuditEntry) -> Self {
        Self {
            action: entry.action,
            commit_id: entry.commit_id.clone(),
            gate_identity: entry.gate_identity.clone(),
            verdict: entry.verdict,
            at: entry.at,
            note: entry.note.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(active: Option<PromotedModelStatus>, enabled: bool) -> MlStatus {
        MlStatus {
            routing_enabled: enabled,
            durable_state: false,
            traces_open: false,
            model_store_open: false,
            active,
            active_decision: None,
            history: Vec::new(),
            blind_candidates: Vec::new(),
            routing: MlRouterCounts {
                rankings: 0,
                fallbacks: 0,
                explorations: 0,
                blind_explorations: 0,
                attached: false,
            },
            exploration_probability: 0.0,
            exploration_seed: 0,
            dataset: IngestionCounters::default(),
            traces: None,
            shadow: ShadowStatus {
                enabled: false,
                decisions_recorded: 0,
                faults: 0,
            },
            read_at: 0,
        }
    }

    fn promoted() -> PromotedModelStatus {
        PromotedModelStatus {
            model_id: "shadow".to_string(),
            commit_id: "0123456789abcdef".to_string(),
            verdict: PromotionVerdict::Promoted,
            dataset_fingerprint: "fedcba9876543210".to_string(),
            fitted_partition_fingerprint: "0011223344556677".to_string(),
            gate_config_identity: "8899aabbccddeeff".to_string(),
            required_baseline: "baseline.priority".to_string(),
            paired_requests: 120,
            holdout_loss: 0.024,
            promoted_at: 1_700_000_000,
        }
    }

    #[test]
    fn a_fresh_installation_says_so_plainly() {
        let snapshot = status(None, false);
        assert!(!snapshot.is_routing_with_a_model());
        assert_eq!(snapshot.headline(), "ML routing off; routing is deterministic");
    }

    #[test]
    fn enabled_without_a_model_is_reported_as_enabled_not_as_working() {
        // The state a misconfiguration produces: the switch is on and nothing
        // is attached. Reporting it as "ML routing on" would be a lie an
        // operator would act on.
        let snapshot = status(None, true);
        assert!(!snapshot.is_routing_with_a_model());
        assert!(snapshot.headline().contains("no model is attached"));
    }

    #[test]
    fn a_promoted_but_disabled_model_is_reported_as_inert() {
        let snapshot = status(Some(promoted()), false);
        assert!(!snapshot.is_routing_with_a_model());
        assert!(snapshot.headline().contains("deterministic"));
        assert!(snapshot.headline().contains("0123456789abcdef"));
    }

    #[test]
    fn an_attached_enabled_model_names_its_gate_and_body() {
        let snapshot = status(Some(promoted()), true);
        assert!(snapshot.is_routing_with_a_model());
        let headline = snapshot.headline();
        assert!(headline.contains("ML routing on"));
        assert!(headline.contains("0123456789abcdef"));
        assert!(headline.contains("8899aabb"));
        assert!(headline.contains("fedcba98"));
    }

    #[test]
    fn a_status_document_survives_a_round_trip() {
        // It is read over HTTP and over IPC; a status that cannot be
        // serialised is a status nobody can read.
        let snapshot = status(Some(promoted()), true);
        let encoded = serde_json::to_string(&snapshot).expect("encode");
        let decoded: MlStatus = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, snapshot);
    }
}