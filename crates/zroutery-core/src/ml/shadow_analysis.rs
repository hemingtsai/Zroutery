//! Shadow analysis: what a candidate model would have done, and whether that
//! would have been better.
//!
//! # Reachability is not evidence
//!
//! The existing shadow path records, per policy-routed request, the decision
//! production made and a full counterfactual decision from the model. That is
//! real capability and it answers no question on its own: proving the code ran
//! says the plumbing works, not that the model is worth serving.
//!
//! This module is the layer that turns those records into a measurement. It
//! answers four questions, in increasing order of how much they should worry
//! you:
//!
//! 1. **Agreement** — how often would the model have chosen what production
//!    chose? A model that always agrees has changed nothing.
//! 2. **Alternative rate** — how often would it have chosen something else, and
//!    what would it have chosen?
//! 3. **Estimated utility difference** — on those disagreements, how much
//!    utility did the model *think* it was gaining?
//! 4. **Observed utility difference and regret** — on the disagreements where
//!    the alternative was actually attempted, how much utility did it really
//!    gain?
//!
//! Questions 3 and 4 are kept apart on purpose. An estimated gain with no
//! observed counterpart is a prediction, and reporting it next to observed
//! numbers without saying which is which is how a model that only sounds good
//! gets promoted.
//!
//! # Two shadow kinds
//!
//! [`ShadowKind::PredictionOnly`] means the model scored candidates and nothing
//! selected on the result. [`ShadowKind::Decision`] means a full alternative
//! decision was computed. Only the second can answer questions 2 to 4, so this
//! module reports how much of the body was decision-shaped rather than quietly
//! averaging the two together.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::comparison::{observed_utility, MeasuredOutcome};
use super::reward::RewardPolicy;
use super::traces::{DatasetFingerprint, RequestTrace};

/// How much of a shadow record is a decision and how much is only a prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowKind {
    /// Candidates were scored; nothing selected on the result.
    PredictionOnly,
    /// A full alternative decision was computed.
    Decision,
}

impl ShadowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowKind::PredictionOnly => "prediction_only",
            ShadowKind::Decision => "decision",
        }
    }
}

/// Why a shadow record contributed nothing to the quality measurement.
///
/// Reported rather than dropped: a body that is 90% unmeasurable and a body that
/// is 100% measurable with a bad model look identical if the missing 90% is
/// silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowGap {
    /// The candidate the shadow chose was not among those actually attempted,
    /// so nothing is known about how it would have behaved.
    AlternativeUnmeasured,
    /// Production and the shadow chose the same candidate.
    Agreement,
    /// The record named no alternative selection.
    NoAlternative,
    /// The record carried no candidate set.
    NoCandidates,
}

impl ShadowGap {
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowGap::AlternativeUnmeasured => "alternative_unmeasured",
            ShadowGap::Agreement => "agreement",
            ShadowGap::NoAlternative => "no_alternative",
            ShadowGap::NoCandidates => "no_candidates",
        }
    }
}

// ---------------------------------------------------------------------------
// ShadowVerdict — one request
// ---------------------------------------------------------------------------

/// One request's shadow comparison, with both the estimated and the observed
/// half kept apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowVerdictRecord {
    pub request_id: String,
    pub kind: ShadowKind,
    /// What production planned.
    pub production_candidate: String,
    /// What the model would have chosen.
    pub shadow_candidate: String,
    /// Whether the two agree.
    pub agrees: bool,
    /// Utility the model estimated for each side. `None` when the model made no
    /// prediction for that candidate.
    pub estimated_production_utility: Option<f64>,
    pub estimated_shadow_utility: Option<f64>,
    /// Utility actually observed for each side, where the router attempted it.
    pub observed_production: Option<MeasuredOutcome>,
    pub observed_shadow: Option<MeasuredOutcome>,
    /// Observed utility difference, shadow minus production. `None` when
    /// either side was not measured.
    pub observed_utility_delta: Option<f64>,
    /// What the shadow record itself reports as the reason for its choice.
    pub reason: String,
}

// ---------------------------------------------------------------------------
// ShadowAnalysis
// ---------------------------------------------------------------------------

/// The measurement over a body of shadow records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowAnalysis {
    /// Identity of the trace body this was computed over.
    pub dataset_fingerprint: DatasetFingerprint,
    pub records: usize,
    pub decision_records: usize,
    pub prediction_only_records: usize,
    /// Records where production and the model chose the same candidate.
    pub agreements: usize,
    /// Records where they chose differently.
    pub disagreements: usize,
    /// Disagreements where the alternative had a recorded outcome.
    pub disagreements_measured: usize,
    /// `agreements / records`, over records that named a candidate.
    pub agreement_rate: f64,
    /// `disagreements_measured / disagreements`.
    pub measured_alternative_rate: f64,
    /// How often each alternative candidate was the one the model wanted.
    pub alternative_selections: BTreeMap<String, usize>,
    /// Mean estimated utility difference over disagreements where both sides
    /// were predicted.
    pub mean_estimated_utility_delta: Option<f64>,
    /// Mean observed utility difference over disagreements where both sides
    /// were attempted.
    pub mean_observed_utility_delta: Option<f64>,
    /// Mean regret: observed utility of the production choice minus observed
    /// utility of the best measured alternative. Zero when the model chose
    /// production's candidate.
    pub mean_regret: Option<f64>,
    /// Disagreements where the alternative would have been *worse*.
    pub harmful_alternatives: usize,
    /// Disagreements where the alternative would have been better.
    pub helpful_alternatives: usize,
    /// Records that contributed nothing, and why.
    pub gaps: BTreeMap<String, usize>,
    pub reward_policy: RewardPolicy,
    pub produced_at: i64,
}

impl ShadowAnalysis {
    /// Records that named no candidate at all, so nothing could be compared.
    pub fn unusable(&self) -> usize {
        self.gaps
            .get(ShadowGap::NoCandidates.as_str())
            .copied()
            .unwrap_or(0)
    }

    /// Whether this analysis can support a claim about model quality.
    ///
    /// False unless the body is mostly decision-shaped, the model actually
    /// disagrees often enough to be worth measuring, and enough of those
    /// disagreements have a recorded outcome to compare.
    pub fn is_evaluable(&self) -> bool {
        self.records > 0
            && self.decision_records * 2 >= self.records
            && self.disagreements >= super::comparison::MIN_PAIRED_REQUESTS
            && self.disagreements_measured >= super::comparison::MIN_PAIRED_REQUESTS
    }
}

// ---------------------------------------------------------------------------
// analyse
// ---------------------------------------------------------------------------

/// The shadow-side evidence for a body of traces, gathered from whatever ran
/// alongside production.
///
/// Grouped into one value rather than four positional maps because these four
/// always travel together, and a caller that supplied three of them by accident
/// would otherwise compile.
#[derive(Debug, Clone, Default)]
pub struct ShadowEvidence {
    /// Request id to the candidate a shadow decision selected.
    pub selections: BTreeMap<String, String>,
    /// Request id to how much of that record was a decision.
    pub kinds: BTreeMap<String, ShadowKind>,
    /// Request id to the reason the shadow recorded.
    pub reasons: BTreeMap<String, String>,
    /// Request id to `(production estimate, shadow estimate)` predicted utility.
    ///
    /// Separate from the observed half and optional: a shadow that predicted but
    /// did not decide still produces estimates, and a body with no estimates
    /// must report `mean_estimated_utility_delta: None` rather than zero.
    pub estimated_utilities: BTreeMap<String, (f64, f64)>,
}

impl ShadowEvidence {
    /// Gather evidence by replaying `policy` over the recorded candidate sets.
    ///
    /// This is the bridge from "we have traces" to "we know what the model would
    /// have done": the same [`ReplayPolicy`](super::comparison::ReplayPolicy)
    /// machinery the routing comparison uses, applied to the recorded
    /// decision-time inputs.
    pub fn from_policy(
        traces: &[RequestTrace],
        policy: &dyn super::comparison::ReplayPolicy,
        estimated_utilities: BTreeMap<String, (f64, f64)>,
    ) -> Self {
        let mut state = super::comparison::ReplayState::default();
        let mut evidence = Self {
            estimated_utilities,
            ..Self::default()
        };
        for trace in traces {
            let choice = policy.select(trace, &mut state);
            evidence
                .selections
                .insert(trace.request_id.clone(), choice.candidate_id);
            evidence
                .kinds
                .insert(trace.request_id.clone(), ShadowKind::Decision);
            evidence
                .reasons
                .insert(trace.request_id.clone(), choice.reason);
        }
        evidence
    }
}

/// Analyse a body of traces as shadow records.
///
/// Each trace is read as one shadow record: `production_selected` is what
/// production planned, and `evidence.selections` is what a [`ShadowEngine`]
/// recorded for the same request.
pub fn analyse(
    traces: &[RequestTrace],
    evidence: &ShadowEvidence,
    reward_policy: &RewardPolicy,
) -> ShadowAnalysis {
    let mut records = 0usize;
    let mut decision_records = 0usize;
    let mut prediction_only_records = 0usize;
    let mut agreements = 0usize;
    let mut disagreements = 0usize;
    let mut disagreements_measured = 0usize;
    let mut alternative_selections: BTreeMap<String, usize> = BTreeMap::new();
    let mut estimated_deltas: Vec<f64> = Vec::new();
    let mut observed_deltas: Vec<f64> = Vec::new();
    let mut regrets: Vec<f64> = Vec::new();
    let mut harmful = 0usize;
    let mut helpful = 0usize;
    let mut gaps: BTreeMap<String, usize> = BTreeMap::new();

    fn note_gap(gaps: &mut BTreeMap<String, usize>, gap: ShadowGap) {
        *gaps.entry(gap.as_str().to_string()).or_insert(0) += 1;
    }

    for trace in traces {
        if trace.input.candidates.is_empty() {
            note_gap(&mut gaps, ShadowGap::NoCandidates);
            continue;
        }
        records += 1;

        let request_id = trace.request_id.clone();
        let production = trace.input.production_selected.clone();
        let Some(shadow) = evidence.selections.get(&request_id) else {
            note_gap(&mut gaps, ShadowGap::NoAlternative);
            continue;
        };
        if shadow.is_empty() {
            note_gap(&mut gaps, ShadowGap::NoAlternative);
            continue;
        }

        match evidence.kinds.get(&request_id).copied() {
            Some(ShadowKind::Decision) | None => decision_records += 1,
            Some(ShadowKind::PredictionOnly) => prediction_only_records += 1,
        }

        let observed = observed_outcomes(trace);

        if shadow == &production {
            agreements += 1;
            note_gap(&mut gaps, ShadowGap::Agreement);
            continue;
        }

        disagreements += 1;
        *alternative_selections.entry(shadow.clone()).or_insert(0) += 1;

        if let Some((production_estimate, shadow_estimate)) =
            evidence.estimated_utilities.get(&request_id)
        {
            if production_estimate.is_finite() && shadow_estimate.is_finite() {
                estimated_deltas.push(shadow_estimate - production_estimate);
            }
        }

        let production_observed = lookup(&observed, trace, &production);
        let shadow_observed = lookup(&observed, trace, shadow);

        let (Some(production_outcome), Some(shadow_outcome)) =
            (production_observed, shadow_observed)
        else {
            note_gap(&mut gaps, ShadowGap::AlternativeUnmeasured);
            continue;
        };

        disagreements_measured += 1;
        let production_utility = observed_utility(
            &production_outcome,
            reward_policy,
            !production_outcome.success,
        );
        let shadow_utility =
            observed_utility(&shadow_outcome, reward_policy, !shadow_outcome.success);
        let delta = shadow_utility - production_utility;
        observed_deltas.push(delta);
        regrets.push(-delta);
        if delta > 0.0 {
            helpful += 1;
        } else if delta < 0.0 {
            harmful += 1;
        }
    }

    let comparable = records;
    ShadowAnalysis {
        dataset_fingerprint: DatasetFingerprint::of(&super::traces::samples_from(traces)),
        records,
        decision_records,
        prediction_only_records,
        agreements,
        disagreements,
        disagreements_measured,
        agreement_rate: if comparable == 0 {
            0.0
        } else {
            agreements as f64 / comparable as f64
        },
        measured_alternative_rate: if disagreements == 0 {
            0.0
        } else {
            disagreements_measured as f64 / disagreements as f64
        },
        alternative_selections,
        mean_estimated_utility_delta: mean_of(&estimated_deltas),
        mean_observed_utility_delta: mean_of(&observed_deltas),
        mean_regret: mean_of(&regrets),
        harmful_alternatives: harmful,
        helpful_alternatives: helpful,
        gaps,
        reward_policy: reward_policy.clone(),
        produced_at: now_seconds(),
    }
}

/// Index a trace's measured outcomes by candidate id, through the candidate set
/// so a model id served by two providers still resolves.
fn observed_outcomes(trace: &RequestTrace) -> BTreeMap<String, MeasuredOutcome> {
    let mut index = BTreeMap::new();
    for sample in trace.attempt_samples() {
        for candidate in &trace.input.candidates {
            if candidate.candidate_id == sample.model_id
                && candidate.provider_id == sample.provider_id
            {
                index.insert(
                    sample.model_id.clone(),
                    MeasuredOutcome {
                        success: sample.success,
                        latency_ms: sample.targets.latency_ms,
                        ttft_ms: sample.targets.ttft_ms,
                        cost: sample.targets.cost,
                    },
                );
            }
        }
    }
    index
}

fn lookup(
    observed: &BTreeMap<String, MeasuredOutcome>,
    _trace: &RequestTrace,
    candidate_id: &str,
) -> Option<MeasuredOutcome> {
    observed.get(candidate_id).copied()
}

fn mean_of(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::comparison::ReplayBaseline;
    use crate::ml::dataset::{SampleScope, Targets};
    use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION, UNKNOWN};
    use crate::ml::shadow::{ShadowCandidateInput, ShadowInput};

    /// `fast` succeeds quickly and cheaply; `steady` fails, slowly and
    /// dearly. A shadow that wants `fast` where production took `steady` is
    /// therefore making a measurable improvement, and the reverse is a
    /// measurable harm — both directions are exercised so the analysis cannot
    /// pass by always reporting a positive number.
    fn candidate(id: &str, provider: &str) -> ShadowCandidateInput {
        let mut values = [UNKNOWN; crate::ml::features::FEATURE_DIMENSION];
        if id == "fast" {
            values[crate::ml::features::F_OBS_LATENCY_EWMA] = 0.9;
        } else {
            values[crate::ml::features::F_OBS_LATENCY_EWMA] = 0.1;
        }
        ShadowCandidateInput {
            candidate_id: id.to_string(),
            provider_id: provider.to_string(),
            tier: None,
            eligible: true,
            features: RoutingFeatures {
                schema_version: FEATURE_SCHEMA_VERSION,
                values,
            },
            rejection_reason: None,
        }
    }

    fn sample(
        id: &str,
        model: &str,
        provider: &str,
        success: bool,
        latency: f64,
    ) -> crate::ml::dataset::OutcomeTrainingSample {
        crate::ml::dataset::OutcomeTrainingSample {
            sample_id: format!("s-{id}-{model}"),
            schema_version: 1,
            timestamp: 1_700_000_000,
            streaming: false,
            dialect: "anthropic".to_string(),
            features: RoutingFeatures::default(),
            targets: Targets {
                success,
                latency_ms: Some(latency),
                ttft_ms: Some(50.0),
                cost: Some(if success { 0.001 } else { 0.02 }),
                failure_class: None,
                fallback_count: 0,
            },
            provider_id: provider.to_string(),
            model_id: model.to_string(),
            origin: crate::feedback::DataOrigin::Native,
            outcome_id: format!("out-{id}"),
            request_id: format!("r{id}"),
            decision_id: Some(format!("d{id}")),
            response_id: None,
            final_status: if success {
                crate::outcome::FinalStatus::Success
            } else {
                crate::outcome::FinalStatus::Failed
            },
            success,
            identity: crate::outcome::OutcomeIdentity::default(),
            scope: SampleScope::Attempt {
                index: 0,
                attempt_id: format!("a-{id}-{model}"),
            },
            attempt_id: Some(format!("a-{id}-{model}")),
            rectified: false,
            attempts: Vec::new(),
            usage: None,
            estimated_cost: None,
            actual_cost: None,
            terminal_error: None,
            feedback: None,
        }
    }

    /// One trace. `measured` names the candidates the router actually attempted;
    /// the candidate set always names both, so an absent measurement is a real
    /// gap rather than an absent candidate.
    fn trace(id: &str, production: &str, measured: &[&str]) -> RequestTrace {
        let input = ShadowInput {
            decision_id: format!("d{id}"),
            production_selected: production.to_string(),
            candidates: vec![candidate("fast", "alpha"), candidate("steady", "beta")],
            ..ShadowInput::default()
        };
        let samples: Vec<crate::ml::dataset::OutcomeTrainingSample> = measured
            .iter()
            .map(|model| {
                let (provider, success, latency) = match *model {
                    "fast" => ("alpha", true, 120.0),
                    _ => ("beta", false, 4000.0),
                };
                sample(id, model, provider, success, latency)
            })
            .collect();
        RequestTrace::new(format!("r{id}"), 1_700_000_000, input, samples)
    }

    fn policy() -> ReplayBaseline {
        ReplayBaseline::LowestLatency
    }

    /// Production takes `steady`, the shadow wants `fast`, both were attempted.
    fn disagreement_body(count: usize) -> Vec<RequestTrace> {
        (0..count)
            .map(|index| {
                let id = format!("{index:04}");
                trace(&id, "steady", &["fast", "steady"])
            })
            .collect()
    }

    fn analyse_with(traces: &[RequestTrace]) -> ShadowAnalysis {
        analyse(
            traces,
            &ShadowEvidence::from_policy(traces, &policy(), BTreeMap::new()),
            &RewardPolicy::default(),
        )
    }

    #[test]
    fn a_model_that_always_agrees_shows_agreement_and_no_measurement() {
        let traces: Vec<RequestTrace> = (0..50)
            .map(|index| {
                let id = format!("{index:04}");
                trace(&id, "fast", &["fast", "steady"])
            })
            .collect();
        let analysis = analyse_with(&traces);
        assert_eq!(analysis.records, 50);
        assert_eq!(analysis.agreements, 50);
        assert_eq!(analysis.disagreements, 0);
        assert!((analysis.agreement_rate - 1.0).abs() < 1e-9);
        assert_eq!(analysis.mean_observed_utility_delta, None);
        assert_eq!(analysis.mean_regret, None);
        assert!(!analysis.is_evaluable());
        assert_eq!(analysis.gaps.get("agreement").copied(), Some(50));
    }

    #[test]
    fn a_measurable_disagreement_reports_observed_utility_and_regret() {
        let analysis = analyse_with(&disagreement_body(50));
        assert_eq!(analysis.records, 50);
        assert_eq!(analysis.disagreements, 50);
        assert_eq!(analysis.agreements, 0);
        assert_eq!(analysis.disagreements_measured, 50);
        assert!((analysis.agreement_rate - 0.0).abs() < 1e-9);
        assert_eq!(analysis.decision_records, 50);
        assert_eq!(
            analysis.alternative_selections.get("fast").copied(),
            Some(50)
        );

        // `fast` succeeds fast and cheap, `steady` fails slow and dear, so
        // switching is the right call and regret is negative.
        let delta = analysis
            .mean_observed_utility_delta
            .expect("a measured body produces an observed delta");
        assert!(
            delta > 0.0,
            "expected the shadow pick to be better: {delta}"
        );
        assert_eq!(analysis.helpful_alternatives, 50);
        assert_eq!(analysis.harmful_alternatives, 0);
        assert!(analysis.mean_regret.expect("regret") < 0.0);
        assert!(analysis.is_evaluable());
    }

    #[test]
    fn a_harmful_disagreement_is_reported_as_harm() {
        // Production took `fast`; the shadow insists on `steady`, which failed.
        let traces: Vec<RequestTrace> = (0..40)
            .map(|index| {
                let id = format!("{index:04}");
                trace(&id, "fast", &["fast", "steady"])
            })
            .collect();
        let mut evidence = ShadowEvidence::from_policy(&traces, &policy(), BTreeMap::new());
        for selection in evidence.selections.values_mut() {
            *selection = "steady".to_string();
        }
        let analysis = analyse(&traces, &evidence, &RewardPolicy::default());
        assert_eq!(analysis.disagreements, 40);
        assert_eq!(analysis.disagreements_measured, 40);
        assert_eq!(analysis.harmful_alternatives, 40);
        assert_eq!(analysis.helpful_alternatives, 0);
        assert!(analysis.mean_observed_utility_delta.expect("measured") < 0.0);
        assert!(analysis.mean_regret.expect("regret") > 0.0);
        // Harmful but fully measured: still not a reason to promote, and the
        // analysis says so with numbers rather than with a verdict.
        assert!(analysis.is_evaluable());
    }

    #[test]
    fn an_unmeasured_alternative_is_a_reported_gap_not_a_zero() {
        // Only `steady` was attempted, so the model's wish for `fast` cannot be
        // scored.
        let traces: Vec<RequestTrace> = (0..40)
            .map(|index| {
                let id = format!("{index:04}");
                trace(&id, "steady", &["steady"])
            })
            .collect();
        let analysis = analyse_with(&traces);
        assert_eq!(analysis.disagreements, 40);
        assert_eq!(analysis.disagreements_measured, 0);
        assert_eq!(
            analysis.gaps.get("alternative_unmeasured").copied(),
            Some(40)
        );
        assert_eq!(analysis.mean_observed_utility_delta, None);
        assert!((analysis.measured_alternative_rate - 0.0).abs() < 1e-9);
        // Disagreement without measurement is not evidence, so this body cannot
        // support a claim even though it disagrees on every record.
        assert!(!analysis.is_evaluable());
    }

    #[test]
    fn estimated_and_observed_deltas_are_reported_separately() {
        let traces = disagreement_body(40);
        let mut estimates = BTreeMap::new();
        for trace in &traces {
            // The model believed it would gain far more than it really did.
            estimates.insert(trace.request_id.clone(), (0.10, 9.0));
        }
        let evidence = ShadowEvidence::from_policy(&traces, &policy(), estimates);
        let analysis = analyse(&traces, &evidence, &RewardPolicy::default());
        let estimated = analysis
            .mean_estimated_utility_delta
            .expect("estimates were supplied");
        let observed = analysis
            .mean_observed_utility_delta
            .expect("the body was measured");
        assert!((estimated - 8.9).abs() < 1e-9);
        // The estimate is kept apart from the observation rather than averaged
        // into it, and it is larger - which is exactly the overconfidence this
        // separation exists to make visible.
        assert!(
            estimated > observed,
            "expected the estimate to exceed the observation: {estimated} vs {observed}"
        );
    }

    #[test]
    fn a_body_with_no_estimates_reports_no_estimate_rather_than_zero() {
        let analysis = analyse_with(&disagreement_body(40));
        assert_eq!(analysis.mean_estimated_utility_delta, None);
        assert!(analysis.mean_observed_utility_delta.is_some());
    }

    #[test]
    fn prediction_only_records_are_counted_separately() {
        let traces = disagreement_body(50);
        let mut evidence = ShadowEvidence::from_policy(&traces, &policy(), BTreeMap::new());
        for (index, kind) in evidence.kinds.values_mut().enumerate() {
            if index % 4 != 0 {
                *kind = ShadowKind::PredictionOnly;
            }
        }
        let analysis = analyse(&traces, &evidence, &RewardPolicy::default());
        assert_eq!(analysis.records, 50);
        assert_eq!(analysis.decision_records, 13);
        assert_eq!(analysis.prediction_only_records, 37);
        // Most of the body is not decision-shaped, so it cannot support a claim.
        assert!(!analysis.is_evaluable());
    }

    #[test]
    fn a_body_below_the_evidence_floor_is_not_evaluable() {
        let analysis = analyse_with(&disagreement_body(10));
        assert_eq!(analysis.disagreements_measured, 10);
        assert!(!analysis.is_evaluable());
    }

    #[test]
    fn a_missing_shadow_selection_is_reported_rather_than_treated_as_agreement() {
        let traces = disagreement_body(10);
        let mut evidence = ShadowEvidence::from_policy(&traces, &policy(), BTreeMap::new());
        evidence.selections.remove("r0000");
        let analysis = analyse(&traces, &evidence, &RewardPolicy::default());
        assert_eq!(analysis.records, 10);
        assert_eq!(analysis.gaps.get("no_alternative").copied(), Some(1));
        assert_eq!(analysis.disagreements, 9);
        // The agreement rate is over records that named a candidate, so the
        // missing one neither inflates agreement nor counts as agreement.
        assert_eq!(analysis.agreements, 0);
    }

    #[test]
    fn the_analysis_names_the_body_it_was_computed_over() {
        let traces = disagreement_body(40);
        let analysis = analyse_with(&traces);
        assert_eq!(
            analysis.dataset_fingerprint,
            DatasetFingerprint::of(&super::super::traces::deduped_samples_from(&traces))
        );
    }

    #[test]
    fn an_empty_body_reports_nothing_rather_than_a_clean_result() {
        let analysis = analyse_with(&[]);
        assert_eq!(analysis.records, 0);
        assert_eq!(analysis.agreement_rate, 0.0);
        assert!(!analysis.is_evaluable());
    }
}
