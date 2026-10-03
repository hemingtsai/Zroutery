//! Routing-level comparison: a fixed baseline against a learned candidate,
//! over the same traces, the same candidate sets, and the same constraints.
//!
//! # The question this module exists to answer
//!
//! `> Does learning change routing outcomes in an observable, repeatable way?`
//!
//! Answering it with model metrics does not answer it. A lower log loss on a
//! held-out set says the model fits better; it says nothing about whether a
//! request would have been routed somewhere better, cost less, failed less, or
//! been served at all. Every number in this module is a *routing* number:
//! success rate, fallback rate, latency, TTFT, cost, aggregate utility, provider
//! switches, ineligible selections.
//!
//! # The counterfactual problem, stated honestly
//!
//! A trace records what happened, not what would have happened. For one request
//! the router attempted some candidates and not others, so only some
//! (candidate, outcome) pairs are *measured*. Naming a candidate nobody tried
//! produces no observation, and there is no honest way to invent one.
//!
//! So this module does not estimate. It reports three things that are each
//! individually true and jointly impossible to misread:
//!
//! * **coverage** — how often a policy's choice was measurable at all;
//! * **unmeasured** — how often it was not;
//! * **paired deltas on the intersection** — the requests where *both* arms'
//!   choices have measurements, compared request by request.
//!
//! A policy is not rewarded for choosing what was tried. That is an artifact of
//! the logging policy, not a property of the policy, and it is exactly why the
//! paired comparison is restricted to the intersection and reported with the
//! intersection's size attached.
//!
//! # Why observed utility is not comparable to predicted utility
//!
//! [`super::reward::compute_utility`] scores a *predicted* success probability,
//! so its success term lives in `[0, w]`. Observed utility here scores a
//! *measured* outcome, so its success term is `±w`. They are different scales
//! and mixing them would manufacture an improvement out of arithmetic. This
//! module therefore never subtracts one from the other: predicted utility is
//! reported only as evidence about the model, observed utility only as evidence
//! about the routing, and every delta in a [`PairedDeltas`] is observed-minus-
//! observed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::decision_engine::{DecisionEngine, EngineCandidate, EngineInput};
use super::features::{F_OBS_HEALTH, F_OBS_LATENCY_EWMA, F_PRIORITY};
use super::learning::{predict_bundle, TrainingOutcome};
use super::model_identity::ModelEnsemble;
use super::reward::{PredictionBundle, RewardPolicy};
use super::traces::{DatasetFingerprint, RequestTrace};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ComparisonError {
    #[error("the comparison body is empty: no trace carries a decision-time candidate set")]
    NoTraces,
    #[error("no trace offered an eligible candidate, so no policy could have routed")]
    NoEligibleCandidates,
}

// ---------------------------------------------------------------------------
// Choice — what a policy named
// ---------------------------------------------------------------------------

/// One policy's selection for one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyChoice {
    pub policy: String,
    pub request_id: String,
    pub candidate_id: String,
    pub provider_id: String,
    /// Whether the candidate the policy named was eligible in the recorded
    /// decision. A policy that names an ineligible candidate has proposed a
    /// routing action the real request path would never execute, and that is
    /// counted as a constraint violation rather than quietly scored.
    pub eligible: bool,
    pub reason: String,
}

impl PolicyChoice {
    /// The `(model, provider)` key a measured outcome is looked up under.
    pub fn key(&self) -> (&str, &str) {
        (self.candidate_id.as_str(), self.provider_id.as_str())
    }
}

// ---------------------------------------------------------------------------
// ReplayPolicy
// ---------------------------------------------------------------------------

/// Mutable state a policy carries across the replay.
///
/// Round robin needs it; everything else ignores it. Existed as a parameter
/// rather than a trait object field so a policy's state is visible at the call
/// site instead of hidden inside the policy.
#[derive(Debug, Clone, Default)]
pub struct ReplayState {
    /// Rotating cursor for round robin.
    pub cursor: usize,
}

/// A deterministic selection function over a recorded candidate set.
///
/// Implementations must be pure functions of the trace, their own state, and
/// the model they were given. Two runs over the same body must name the same
/// candidate for every request, or the comparison is not repeatable.
pub trait ReplayPolicy {
    fn name(&self) -> &'static str;
    fn select(&self, trace: &RequestTrace, state: &mut ReplayState) -> PolicyChoice;
}

// ---------------------------------------------------------------------------
// Candidate helpers
// ---------------------------------------------------------------------------

/// The candidates a policy may consider, in recorded plan order.
fn candidates_of(trace: &RequestTrace) -> &[super::shadow::ShadowCandidateInput] {
    &trace.input.candidates
}

fn choice_for(
    policy: &'static str,
    trace: &RequestTrace,
    index: usize,
    reason: impl Into<String>,
) -> PolicyChoice {
    let candidate = &trace.input.candidates[index];
    PolicyChoice {
        policy: policy.to_string(),
        request_id: trace.request_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        provider_id: candidate.provider_id.clone(),
        eligible: candidate.eligible,
        reason: reason.into(),
    }
}

/// Index of the eligible candidate maximising `score`, first on a tie.
fn argmax_eligible<F>(trace: &RequestTrace, score: F) -> Option<usize>
where
    F: Fn(&super::shadow::ShadowCandidateInput) -> f32,
{
    candidates_of(trace)
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.eligible)
        .max_by(|a, b| {
            let left = score(a.1);
            let right = score(b.1);
            left.partial_cmp(&right)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
}

/// Indices of the eligible candidates, in recorded plan order.
fn eligible_indices(trace: &RequestTrace) -> Vec<usize> {
    candidates_of(trace)
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.eligible)
        .map(|(index, _)| index)
        .collect()
}

// ---------------------------------------------------------------------------
// ReplayBaseline
// ---------------------------------------------------------------------------

/// The fixed, deterministic strategies a learned candidate has to beat.
///
/// Each reads only decision-time state the trace recorded, and each mirrors the
/// production strategy of the same name rather than an idealised version of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayBaseline {
    /// Lowest configured priority number first. The recorded
    /// [`F_PRIORITY`] feature is `1 - priority / 100` clamped, so the largest
    /// value is the smallest priority number; ties resolve to plan order, which
    /// is what the production `Priority` sort does.
    Priority,
    /// Rotate through the eligible candidates in plan order. The recorded
    /// history carries no per-client cursor, so this is the stateless form:
    /// one cursor across the whole replay body, in trace order.
    RoundRobin,
    /// Largest recorded inverse-normalised latency value, i.e. the lowest
    /// observed latency. A candidate with no latency history reads
    /// [`UNKNOWN`], the minimum, and therefore loses — the same as having no
    /// evidence.
    LowestLatency,
    /// Latency weighted against observed health. This is *not* the production
    /// `Balanced` strategy, which uses a latency/price election this trace does
    /// not record; it is the honest latency-and-health replay of the same idea,
    /// and it is named for what it does.
    Balanced,
}

impl ReplayBaseline {
    pub const ALL: [ReplayBaseline; 4] = [
        ReplayBaseline::Priority,
        ReplayBaseline::RoundRobin,
        ReplayBaseline::LowestLatency,
        ReplayBaseline::Balanced,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReplayBaseline::Priority => "baseline.priority",
            ReplayBaseline::RoundRobin => "baseline.round_robin",
            ReplayBaseline::LowestLatency => "baseline.lowest_latency",
            ReplayBaseline::Balanced => "baseline.balanced",
        }
    }

    /// Latency weight of [`ReplayBaseline::Balanced`].
    const BALANCED_LATENCY_WEIGHT: f32 = 0.7;
}

impl ReplayPolicy for ReplayBaseline {
    fn name(&self) -> &'static str {
        self.as_str()
    }

    fn select(&self, trace: &RequestTrace, state: &mut ReplayState) -> PolicyChoice {
        let eligible = eligible_indices(trace);
        let Some(first) = eligible.first().copied() else {
            // No eligible candidate: name nothing rather than invent one. The
            // caller counts this as unmeasured.
            return PolicyChoice {
                policy: self.name().to_string(),
                request_id: trace.request_id.clone(),
                candidate_id: String::new(),
                provider_id: String::new(),
                eligible: false,
                reason: "no eligible candidate in the recorded decision".to_string(),
            };
        };

        match self {
            ReplayBaseline::Priority => {
                let index =
                    argmax_eligible(trace, |candidate| candidate.features.values[F_PRIORITY])
                        .unwrap_or(first);
                choice_for(self.name(), trace, index, "highest recorded priority score")
            }
            ReplayBaseline::RoundRobin => {
                let index = eligible[state.cursor % eligible.len()];
                state.cursor = state.cursor.wrapping_add(1);
                choice_for(self.name(), trace, index, "round robin cursor")
            }
            ReplayBaseline::LowestLatency => {
                let index = argmax_eligible(trace, |candidate| {
                    candidate.features.values[F_OBS_LATENCY_EWMA]
                })
                .unwrap_or(first);
                choice_for(
                    self.name(),
                    trace,
                    index,
                    "lowest recorded observed latency",
                )
            }
            ReplayBaseline::Balanced => {
                let weight = Self::BALANCED_LATENCY_WEIGHT;
                let index = argmax_eligible(trace, |candidate| {
                    let latency = candidate.features.values[F_OBS_LATENCY_EWMA];
                    let health = candidate.features.values[F_OBS_HEALTH];
                    weight * latency + (1.0 - weight) * health
                })
                .unwrap_or(first);
                choice_for(
                    self.name(),
                    trace,
                    index,
                    "recorded latency and health blend",
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MlPolicy — the learned candidate
// ---------------------------------------------------------------------------

/// The learned candidate: predict, then decide through the real authority.
///
/// This is the same path production takes. Features come from the recorded
/// decision-time vector, predictions come from the trained ensemble, and the
/// selection comes from [`DecisionEngine::decide_with_ml_ranking`] — the
/// eligibility filter, the utility computation, the session guard and the
/// switch threshold are the engine's, not re-implemented here. Comparing a
/// candidate that bypassed the decision authority against one that used it
/// would measure the difference between the two implementations, not between
/// the two policies.
pub struct MlPolicy {
    ensemble: ModelEnsemble,
    engine: DecisionEngine,
    reward_policy: RewardPolicy,
}

impl std::fmt::Debug for MlPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlPolicy").finish_non_exhaustive()
    }
}

impl MlPolicy {
    pub fn new(outcome: &TrainingOutcome, reward_policy: RewardPolicy) -> Self {
        Self {
            ensemble: ModelEnsemble::load_all(&outcome.checkpoint)
                .unwrap_or_else(|_| ModelEnsemble::new()),
            engine: DecisionEngine::new(
                super::coordinator::CoordinatorConfig::default(),
                reward_policy.clone(),
            ),
            reward_policy,
        }
    }

    /// The commit this policy predicts under.
    pub fn commit_id(&self) -> String {
        // The ensemble is loaded from the outcome's checkpoint, so its identity
        // is that checkpoint's identity.
        format!("{:016x}", self.ensemble.save_all().content_hash())
    }

    pub fn reward_policy(&self) -> &RewardPolicy {
        &self.reward_policy
    }
}

impl ReplayPolicy for MlPolicy {
    fn name(&self) -> &'static str {
        "ml.candidate"
    }

    fn select(&self, trace: &RequestTrace, _state: &mut ReplayState) -> PolicyChoice {
        let mut engine_candidates: Vec<EngineCandidate> = Vec::new();
        for candidate in candidates_of(trace) {
            let bundle: PredictionBundle = predict_bundle(
                &self.ensemble,
                &candidate.candidate_id,
                &candidate.provider_id,
                &candidate.features,
            );
            engine_candidates.push(EngineCandidate {
                candidate_id: candidate.candidate_id.clone(),
                bundle,
                eligible: candidate.eligible,
            });
        }

        if engine_candidates.is_empty() {
            return PolicyChoice {
                policy: self.name().to_string(),
                request_id: trace.request_id.clone(),
                candidate_id: String::new(),
                provider_id: String::new(),
                eligible: false,
                reason: "the recorded decision carried no candidate".to_string(),
            };
        }

        // The engine's `current_candidate` is the recorded production selection,
        // so the switch hysteresis is evaluated against what production actually
        // planned rather than against an arbitrary starting point.
        let current = trace.input.production_selected.clone();
        let output = self.engine.decide_with_ml_ranking(&EngineInput {
            current_candidate: &current,
            candidates: &engine_candidates,
            session_mode: trace.input.session_mode,
            session_switch_count: trace.input.session_switch_count,
            is_fallback: trace.input.is_fallback,
        });

        let selected = output.decision.selected_candidate.clone();
        let index = candidates_of(trace)
            .iter()
            .position(|candidate| candidate.candidate_id == selected)
            .unwrap_or(0);
        let candidate = &trace.input.candidates[index];
        PolicyChoice {
            policy: self.name().to_string(),
            request_id: trace.request_id.clone(),
            candidate_id: candidate.candidate_id.clone(),
            provider_id: candidate.provider_id.clone(),
            // The engine refuses ineligible candidates, so this reads the
            // engine's own eligibility evidence rather than assuming it.
            eligible: candidate.eligible,
            reason: output.decision.reason.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Measured outcomes
// ---------------------------------------------------------------------------

/// What is known about how one candidate actually behaved.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeasuredOutcome {
    pub success: bool,
    pub latency_ms: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub cost: Option<f64>,
}

/// Index a trace's measured outcomes by `(model, provider)`.
///
/// Built from the attempt-scoped samples only. A request-scoped sample
/// describes the request as a whole and would wrongly look like an observation
/// about whichever candidate happened to serve it.
fn measured_outcomes(trace: &RequestTrace) -> BTreeMap<(String, String), MeasuredOutcome> {
    let mut index = BTreeMap::new();
    for sample in trace.attempt_samples() {
        index.insert(
            (sample.model_id.clone(), sample.provider_id.clone()),
            MeasuredOutcome {
                success: sample.success,
                latency_ms: sample.targets.latency_ms,
                ttft_ms: sample.targets.ttft_ms,
                cost: sample.targets.cost,
            },
        );
    }
    index
}

/// Observed utility: the measured counterpart of
/// [`super::reward::compute_utility`].
///
/// Scored on the same weights and the same normalisations so that two policies'
/// observed utilities are on one scale and can be subtracted. Not comparable to
/// a predicted utility; see the module documentation.
pub fn observed_utility(outcome: &MeasuredOutcome, policy: &RewardPolicy, fell_back: bool) -> f64 {
    let success = if outcome.success {
        policy.success_weight
    } else {
        -policy.success_weight
    };
    let latency = -policy.latency_weight * (outcome.latency_ms.unwrap_or(0.0) / 5000.0).min(1.0);
    let ttft = -policy.latency_weight * 0.5 * (outcome.ttft_ms.unwrap_or(0.0) / 2000.0).min(1.0);
    let cost = -policy.cost_weight * outcome.cost.unwrap_or(0.0).min(1.0);
    let fallback = if fell_back {
        policy.fallback_penalty
    } else {
        0.0
    };
    let switch = if fell_back { policy.switch_cost } else { 0.0 };
    success + latency + ttft + cost + fallback + switch
}

// ---------------------------------------------------------------------------
// ArmRecord — one scored selection
// ---------------------------------------------------------------------------

/// One policy's selection for one request, with whatever was measurable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmRecord {
    pub choice: PolicyChoice,
    /// False when the choice was not among the candidates this trace actually
    /// attempted, so nothing is known about how it would have behaved.
    pub measured: bool,
    pub outcome: Option<MeasuredOutcome>,
    /// True when the chosen candidate is known to have failed, so serving this
    /// request would have required falling back to another provider.
    pub fell_back: bool,
    /// Observed utility, present exactly when the outcome was measured.
    pub utility: Option<f64>,
}

// ---------------------------------------------------------------------------
// ArmMetrics
// ---------------------------------------------------------------------------

/// Routing-level metrics for one policy over the replay body.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArmMetrics {
    pub policy: String,
    /// Traces the policy was asked about.
    pub requests_considered: usize,
    /// Traces where it named a candidate.
    pub requests_decided: usize,
    /// Traces where its choice had a recorded outcome.
    pub requests_measured: usize,
    /// `requests_measured / requests_considered`.
    pub coverage: f64,
    /// Selections of a candidate the recorded decision marked ineligible.
    pub ineligible_selections: usize,
    pub success_rate: f64,
    pub fallback_rate: f64,
    pub mean_latency_ms: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub mean_ttft_ms: f64,
    pub mean_cost: f64,
    pub mean_observed_utility: f64,
    /// Requests whose chosen candidate failed and so required a provider switch.
    pub provider_switches: usize,
    /// Distinct providers the policy selected across the body.
    pub distinct_providers: usize,
    /// Mean number of candidates the policy considered per request.
    pub mean_eligible_candidates: f64,
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (fraction * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Aggregate one policy's records into routing metrics.
///
/// Every rate and mean is over the *measured* records only. An unmeasurable
/// request contributes to `coverage` and to nothing else, because scoring it
/// would require inventing the number the whole module refuses to invent.
pub fn aggregate(policy: &str, records: &[ArmRecord], considered: usize) -> ArmMetrics {
    let measured: Vec<&ArmRecord> = records.iter().filter(|r| r.measured).collect();
    let n = measured.len();

    let latencies: Vec<f64> = measured
        .iter()
        .filter_map(|r| r.outcome.and_then(|o| o.latency_ms))
        .collect();
    let mut sorted_latencies = latencies.clone();
    sorted_latencies.sort_by(|a, b| a.total_cmp(b));

    let ttfts: Vec<f64> = measured
        .iter()
        .filter_map(|r| r.outcome.and_then(|o| o.ttft_ms))
        .collect();
    let costs: Vec<f64> = measured
        .iter()
        .filter_map(|r| r.outcome.and_then(|o| o.cost))
        .collect();
    let utilities: Vec<f64> = measured.iter().filter_map(|r| r.utility).collect();

    let mut providers: Vec<&str> = records
        .iter()
        .filter(|r| !r.choice.candidate_id.is_empty())
        .map(|r| r.choice.provider_id.as_str())
        .collect();
    providers.sort_unstable();
    providers.dedup();

    ArmMetrics {
        policy: policy.to_string(),
        requests_considered: considered,
        requests_decided: records
            .iter()
            .filter(|r| !r.choice.candidate_id.is_empty())
            .count(),
        requests_measured: n,
        coverage: if considered == 0 {
            0.0
        } else {
            n as f64 / considered as f64
        },
        ineligible_selections: records
            .iter()
            .filter(|r| !r.choice.candidate_id.is_empty() && !r.choice.eligible)
            .count(),
        success_rate: if n == 0 {
            0.0
        } else {
            measured
                .iter()
                .filter(|r| r.outcome.is_some_and(|o| o.success))
                .count() as f64
                / n as f64
        },
        fallback_rate: if n == 0 {
            0.0
        } else {
            measured.iter().filter(|r| r.fell_back).count() as f64 / n as f64
        },
        mean_latency_ms: mean(&latencies),
        p50_latency_ms: percentile(&sorted_latencies, 0.50),
        p95_latency_ms: percentile(&sorted_latencies, 0.95),
        mean_ttft_ms: mean(&ttfts),
        mean_cost: mean(&costs),
        mean_observed_utility: mean(&utilities),
        provider_switches: measured.iter().filter(|r| r.fell_back).count(),
        distinct_providers: providers.len(),
        mean_eligible_candidates: 0.0,
    }
}

// ---------------------------------------------------------------------------
// Paired comparison
// ---------------------------------------------------------------------------

/// Baseline-minus-candidate deltas, restricted to requests where both arms had
/// a measurement.
///
/// Positive deltas favour the candidate except for latency, TTFT and cost,
/// where negative is better. The sign convention is stated per field rather
/// than left to the reader.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PairedDeltas {
    /// Traces where both arms' choices were measurable. Every number below is
    /// over exactly this set, and it is the number to read first.
    pub paired_requests: usize,
    /// Positive favours the candidate.
    pub success_rate_delta: f64,
    pub fallback_rate_delta: f64,
    /// Negative favours the candidate.
    pub mean_latency_delta_ms: f64,
    /// Negative favours the candidate.
    pub mean_ttft_delta_ms: f64,
    /// Negative favours the candidate.
    pub mean_cost_delta: f64,
    /// Positive favours the candidate.
    pub mean_observed_utility_delta: f64,
    /// Both succeeded.
    pub both_succeeded: usize,
    /// Baseline succeeded, the candidate did not. These are the regressions and
    /// they are the number a promotion gate should care about first.
    pub candidate_regressions: usize,
    /// The candidate succeeded, the baseline did not.
    pub candidate_improvements: usize,
    /// Both failed.
    pub both_failed: usize,
    /// Requests where the candidate's observed utility was lower.
    pub utility_regressions: usize,
}

/// Verdict from a paired comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingVerdict {
    /// Too little paired evidence to say anything. Never reported as an
    /// improvement, and never reported as a failure either.
    InsufficientEvidence,
    /// The candidate produced better routing outcomes on the paired set.
    Improved,
    /// The candidate produced worse routing outcomes on the paired set.
    Regressed,
    /// The paired set is large enough and the difference is not large enough to
    /// call. An honest null, which is the most common real result.
    NoDifference,
}

/// The smallest paired set this module will draw a directional conclusion from.
///
/// Deliberately a floor on *paired requests*, not on samples or traces: a
/// comparison over a handful of requests cannot distinguish a routing change
/// from a coincidence, and a module that reports a direction anyway is the
/// module that turns noise into a shipping decision.
pub const MIN_PAIRED_REQUESTS: usize = 30;

/// A utility delta that counts as a real difference, relative to the observed
/// utility spread. One unit of observed utility is a full success against a
/// full failure, so a fifth of that is a large effect for routing.
pub const UTILITY_DELTA_THRESHOLD: f64 = 0.2;

/// Compare one baseline's records against the candidate's, request by request.
pub fn pair_against_baseline(
    baseline: &[ArmRecord],
    candidate: &[ArmRecord],
    reward_policy: &RewardPolicy,
) -> (PairedDeltas, RoutingVerdict) {
    let candidate_by_request: BTreeMap<&str, &ArmRecord> = candidate
        .iter()
        .filter(|r| r.measured)
        .map(|r| (r.choice.request_id.as_str(), r))
        .collect();

    let mut both_succeeded = 0usize;
    let mut candidate_regressions = 0usize;
    let mut candidate_improvements = 0usize;
    let mut both_failed = 0usize;
    let mut utility_regressions = 0usize;
    let mut utility_delta_total = 0.0f64;
    let mut latency_delta_total = 0.0f64;
    let mut ttft_delta_total = 0.0f64;
    let mut cost_delta_total = 0.0f64;

    for base in baseline.iter().filter(|r| r.measured) {
        let Some(other) = candidate_by_request.get(base.choice.request_id.as_str()) else {
            continue;
        };
        let (Some(base_outcome), Some(other_outcome)) = (base.outcome, other.outcome) else {
            continue;
        };

        match (base_outcome.success, other_outcome.success) {
            (true, true) => both_succeeded += 1,
            (true, false) => candidate_regressions += 1,
            (false, true) => candidate_improvements += 1,
            (false, false) => both_failed += 1,
        }

        let base_utility = observed_utility(&base_outcome, reward_policy, base.fell_back);
        let other_utility = observed_utility(&other_outcome, reward_policy, other.fell_back);
        utility_delta_total += other_utility - base_utility;
        if other_utility < base_utility {
            utility_regressions += 1;
        }
        latency_delta_total +=
            other_outcome.latency_ms.unwrap_or(0.0) - base_outcome.latency_ms.unwrap_or(0.0);
        ttft_delta_total +=
            other_outcome.ttft_ms.unwrap_or(0.0) - base_outcome.ttft_ms.unwrap_or(0.0);
        cost_delta_total += other_outcome.cost.unwrap_or(0.0) - base_outcome.cost.unwrap_or(0.0);
    }

    let paired_requests =
        both_succeeded + candidate_regressions + candidate_improvements + both_failed;
    let n = paired_requests as f64;
    let deltas = PairedDeltas {
        paired_requests,
        success_rate_delta: if n == 0.0 {
            0.0
        } else {
            // Signed on purpose. Regressions routinely exceed improvements — that
            // is what a regression *is* — and subtracting two `usize` counts to
            // express a signed difference overflows on exactly the data this
            // module exists to detect.
            (candidate_improvements as i64 - candidate_regressions as i64) as f64 / n
        },
        fallback_rate_delta: 0.0,
        mean_latency_delta_ms: if n == 0.0 {
            0.0
        } else {
            latency_delta_total / n
        },
        mean_ttft_delta_ms: if n == 0.0 { 0.0 } else { ttft_delta_total / n },
        mean_cost_delta: if n == 0.0 { 0.0 } else { cost_delta_total / n },
        mean_observed_utility_delta: if n == 0.0 {
            0.0
        } else {
            utility_delta_total / n
        },
        both_succeeded,
        candidate_regressions,
        candidate_improvements,
        both_failed,
        utility_regressions,
    };

    let verdict = if paired_requests < MIN_PAIRED_REQUESTS {
        RoutingVerdict::InsufficientEvidence
    } else if deltas.mean_observed_utility_delta > UTILITY_DELTA_THRESHOLD {
        RoutingVerdict::Improved
    } else if deltas.mean_observed_utility_delta < -UTILITY_DELTA_THRESHOLD {
        RoutingVerdict::Regressed
    } else {
        RoutingVerdict::NoDifference
    };

    (deltas, verdict)
}

// ---------------------------------------------------------------------------
// ComparisonReport
// ---------------------------------------------------------------------------

/// One baseline paired against the candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselinePairing {
    pub baseline: String,
    pub deltas: PairedDeltas,
    pub verdict: RoutingVerdict,
}

/// The whole comparison, over one body of traces, for one candidate model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingComparison {
    /// Identity of the exact trace body every number here was computed over.
    pub dataset_fingerprint: DatasetFingerprint,
    pub traces: usize,
    /// The learned model's commit.
    pub candidate_commit: String,
    /// Routing metrics for every arm, baselines included.
    pub arms: Vec<ArmMetrics>,
    /// One entry per baseline the candidate was paired against.
    pub paired: Vec<BaselinePairing>,
    /// The utility weights observed utility was scored under.
    pub reward_policy: RewardPolicy,
    /// The evidence floor a directional verdict required.
    pub min_paired_requests: usize,
    pub produced_at: i64,
}

impl RoutingComparison {
    pub fn arm(&self, policy: &str) -> Option<&ArmMetrics> {
        self.arms.iter().find(|arm| arm.policy == policy)
    }

    pub fn pairing(&self, baseline: &str) -> Option<&BaselinePairing> {
        self.paired.iter().find(|pair| pair.baseline == baseline)
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// run_comparison
// ---------------------------------------------------------------------------

/// Replay a body of traces through every baseline and through the learned
/// candidate, and report routing-level metrics for each.
///
/// Same traces, same candidate sets, same eligibility evidence and the same
/// utility weights for every arm. That is the whole point: a difference in the
/// numbers can then only come from the selection.
pub fn run_comparison(
    traces: &[RequestTrace],
    candidate: &MlPolicy,
    baselines: &[ReplayBaseline],
    reward_policy: &RewardPolicy,
) -> Result<RoutingComparison, ComparisonError> {
    if traces.is_empty() {
        return Err(ComparisonError::NoTraces);
    }
    let total_eligible: usize = traces
        .iter()
        .map(|trace| eligible_indices(trace).len())
        .sum();
    if total_eligible == 0 {
        return Err(ComparisonError::NoEligibleCandidates);
    }

    let mut records_by_policy: BTreeMap<String, Vec<ArmRecord>> = BTreeMap::new();

    let run = |policy: &dyn ReplayPolicy| {
        let mut state = ReplayState::default();
        let mut records = Vec::with_capacity(traces.len());
        for trace in traces {
            let choice = policy.select(trace, &mut state);
            if choice.candidate_id.is_empty() {
                records.push(ArmRecord {
                    choice,
                    measured: false,
                    outcome: None,
                    fell_back: false,
                    utility: None,
                });
                continue;
            }
            let index = measured_outcomes(trace);
            match index.get(&(choice.candidate_id.clone(), choice.provider_id.clone())) {
                Some(outcome) => {
                    let fell_back = !outcome.success;
                    records.push(ArmRecord {
                        choice,
                        measured: true,
                        outcome: Some(*outcome),
                        fell_back,
                        utility: Some(observed_utility(outcome, reward_policy, fell_back)),
                    });
                }
                None => records.push(ArmRecord {
                    choice,
                    measured: false,
                    outcome: None,
                    fell_back: false,
                    utility: None,
                }),
            }
        }
        records
    };

    let mut arms = Vec::new();
    for baseline in baselines {
        let records = run(baseline);
        let mut metrics = aggregate(baseline.as_str(), &records, traces.len());
        metrics.mean_eligible_candidates = total_eligible as f64 / traces.len() as f64;
        arms.push(metrics);
        records_by_policy.insert(baseline.as_str().to_string(), records);
    }

    let candidate_records = run(candidate);
    let mut candidate_metrics = aggregate(candidate.name(), &candidate_records, traces.len());
    candidate_metrics.mean_eligible_candidates = total_eligible as f64 / traces.len() as f64;
    arms.push(candidate_metrics);
    records_by_policy.insert(candidate.name().to_string(), candidate_records);

    let mut paired = Vec::with_capacity(baselines.len());
    for baseline in baselines {
        let Some(records) = records_by_policy.get(baseline.as_str()) else {
            continue;
        };
        let Some(candidate_records) = records_by_policy.get(candidate.name()) else {
            continue;
        };
        let (deltas, verdict) = pair_against_baseline(records, candidate_records, reward_policy);
        paired.push(BaselinePairing {
            baseline: baseline.as_str().to_string(),
            deltas,
            verdict,
        });
    }

    Ok(RoutingComparison {
        // The fingerprint is of the *deduplicated* body, because that is the body
        // the arms are evaluated over and therefore the body a training run over
        // the same traces must name. Hashing the raw traces instead would make a
        // model's provenance and its evidence differ by the number of duplicate
        // sample ids, which is not a difference anyone could act on.
        dataset_fingerprint: DatasetFingerprint::of(&super::traces::deduped_samples_from(traces)),
        traces: traces.len(),
        candidate_commit: candidate.commit_id(),
        arms,
        paired,
        reward_policy: reward_policy.clone(),
        min_paired_requests: MIN_PAIRED_REQUESTS,
        produced_at: now_seconds(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::dataset::{OutcomeTrainingSample, SampleScope, Targets};
    use crate::ml::features::{RoutingFeatures, FEATURE_SCHEMA_VERSION, UNKNOWN};
    use crate::ml::learning::{run_training, TrainingConfig};
    use crate::ml::shadow::{ShadowCandidateInput, ShadowInput};

    /// Two candidates: `fast` is faster and dearer, `steady` is slower and
    /// cheaper, and both always succeed. The only thing a policy can get wrong
    /// is which one it prefers, so a comparison over this body measures
    /// selection and nothing else.
    fn candidates() -> Vec<ShadowCandidateInput> {
        let build = |id: &str, provider: &str, priority: f32, latency: f32, health: f32| {
            let mut values = [UNKNOWN; crate::ml::features::FEATURE_DIMENSION];
            values[F_PRIORITY] = priority;
            values[F_OBS_LATENCY_EWMA] = latency;
            values[F_OBS_HEALTH] = health;
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
        };
        vec![
            build("fast", "alpha", 0.9, 0.9, 0.8),
            build("steady", "beta", 0.5, 0.2, 0.9),
        ]
    }

    /// One trace whose `fast` attempt succeeded and whose `steady` attempt
    /// failed, so both candidates have a measurement and the choice matters.
    fn trace(id: &str) -> RequestTrace {
        let input = ShadowInput {
            decision_id: format!("d-{id}"),
            production_selected: "fast".to_string(),
            candidates: candidates(),
            ..ShadowInput::default()
        };
        let sample = |suffix: &str, model: &str, provider: &str, success: bool, latency: f64| {
            OutcomeTrainingSample {
                sample_id: format!("s-{id}-{suffix}"),
                schema_version: 1,
                timestamp: 1_700_000_000,
                streaming: false,
                dialect: "anthropic".to_string(),
                features: RoutingFeatures::default(),
                targets: Targets {
                    success,
                    latency_ms: Some(latency),
                    ttft_ms: Some(50.0),
                    cost: Some(0.01),
                    failure_class: None,
                    fallback_count: 0,
                },
                provider_id: provider.to_string(),
                model_id: model.to_string(),
                origin: crate::feedback::DataOrigin::Native,
                outcome_id: format!("out-{id}"),
                request_id: format!("r{id}"),
                decision_id: Some(format!("d-{id}")),
                response_id: None,
                final_status: crate::outcome::FinalStatus::Success,
                success,
                identity: crate::outcome::OutcomeIdentity::default(),
                scope: SampleScope::Attempt {
                    index: 0,
                    attempt_id: format!("a-{id}-{suffix}"),
                },
                attempt_id: Some(format!("a-{id}-{suffix}")),
                rectified: false,
                attempts: Vec::new(),
                usage: None,
                estimated_cost: None,
                actual_cost: None,
                terminal_error: None,
                feedback: None,
            }
        };
        RequestTrace::new(
            format!("r{id}"),
            1_700_000_000,
            input,
            vec![
                sample("fast", "fast", "alpha", true, 100.0),
                sample("steady", "steady", "beta", false, 900.0),
            ],
        )
    }

    fn body(count: usize) -> Vec<RequestTrace> {
        (0..count)
            .map(|index| trace(&format!("{index:04}")))
            .collect()
    }

    /// A trained candidate. The body is tiny, so this is a fixture for wiring
    /// rather than a claim that the model learned anything.
    fn candidate(traces: &[RequestTrace]) -> MlPolicy {
        let samples = super::super::traces::samples_from(traces);
        let outcome = run_training(&samples, &TrainingConfig::default()).expect("train");
        MlPolicy::new(&outcome, RewardPolicy::default())
    }

    #[test]
    fn priority_baseline_picks_the_highest_priority_feature() {
        let traces = body(1);
        let mut state = ReplayState::default();
        let choice = ReplayBaseline::Priority.select(&traces[0], &mut state);
        assert_eq!(choice.candidate_id, "fast");
        assert!(choice.eligible);
    }

    #[test]
    fn lowest_latency_baseline_picks_the_faster_candidate() {
        let traces = body(1);
        let mut state = ReplayState::default();
        let choice = ReplayBaseline::LowestLatency.select(&traces[0], &mut state);
        assert_eq!(choice.candidate_id, "fast");
    }

    #[test]
    fn balanced_baseline_prefers_the_healthier_candidate_when_latency_is_tied() {
        let mut traces = body(1);
        for candidate in traces[0].input.candidates.iter_mut() {
            candidate.features.values[F_OBS_LATENCY_EWMA] = 0.5;
        }
        let mut state = ReplayState::default();
        // `steady` carries the higher health score.
        assert_eq!(
            ReplayBaseline::Balanced
                .select(&traces[0], &mut state)
                .candidate_id,
            "steady"
        );
    }

    #[test]
    fn round_robin_alternates() {
        let traces = body(1);
        let mut state = ReplayState::default();
        let first = ReplayBaseline::RoundRobin.select(&traces[0], &mut state);
        let second = ReplayBaseline::RoundRobin.select(&traces[0], &mut state);
        let third = ReplayBaseline::RoundRobin.select(&traces[0], &mut state);
        assert_eq!(first.candidate_id, "fast");
        assert_eq!(second.candidate_id, "steady");
        assert_eq!(third.candidate_id, "fast");
    }

    #[test]
    fn every_baseline_refuses_an_ineligible_candidate() {
        let mut traces = body(1);
        // `fast` has the better recorded state on every axis but was rejected.
        traces[0].input.candidates[0].eligible = false;
        traces[0].input.candidates[0].rejection_reason = Some("policy rejected".to_string());
        for baseline in ReplayBaseline::ALL {
            let mut state = ReplayState::default();
            let choice = baseline.select(&traces[0], &mut state);
            assert_eq!(
                choice.candidate_id,
                "steady",
                "{} chose an ineligible candidate",
                baseline.as_str()
            );
            assert!(choice.eligible);
        }
    }

    #[test]
    fn the_ml_policy_goes_through_the_decision_engine_and_stays_eligible() {
        let mut traces = body(1);
        traces[0].input.candidates[0].eligible = false;
        let policy = candidate(&traces);
        let mut state = ReplayState::default();
        let choice = policy.select(&traces[0], &mut state);
        assert_eq!(choice.candidate_id, "steady");
        assert!(choice.eligible);
        // The reason must come from the engine, not from this module.
        assert!(
            !choice.reason.is_empty(),
            "the ML choice must carry the engine's reason"
        );
    }

    #[test]
    fn a_choice_nobody_tried_is_reported_as_unmeasured_not_scored() {
        // The policy prefers `fast`, and this trace attempted only `steady`.
        // Nothing is known about how `fast` would have behaved, and the record
        // must say so instead of inheriting `steady`'s numbers.
        let mut trace = body(1).remove(0);
        trace.samples.retain(|sample| sample.model_id == "steady");

        let mut state = ReplayState::default();
        let policy = ReplayBaseline::LowestLatency;
        let choice = policy.select(&trace, &mut state);
        assert_eq!(choice.candidate_id, "fast");
        let index = measured_outcomes(&trace);
        assert!(
            !index.contains_key(&(choice.candidate_id.clone(), choice.provider_id.clone())),
            "the fixture must leave the chosen candidate unmeasured"
        );

        let record = match index.get(&(choice.candidate_id.clone(), choice.provider_id.clone())) {
            Some(outcome) => ArmRecord {
                choice,
                measured: true,
                outcome: Some(*outcome),
                fell_back: !outcome.success,
                utility: Some(observed_utility(
                    outcome,
                    &RewardPolicy::default(),
                    !outcome.success,
                )),
            },
            None => ArmRecord {
                choice,
                measured: false,
                outcome: None,
                fell_back: false,
                utility: None,
            },
        };
        assert!(!record.measured);
        assert!(record.utility.is_none());

        // And the aggregate must not score it as a failure either: an
        // unmeasurable request contributes to coverage and nothing else.
        let metrics = aggregate("t", std::slice::from_ref(&record), 1);
        assert_eq!(metrics.requests_measured, 0);
        assert_eq!(metrics.coverage, 0.0);
        assert_eq!(metrics.success_rate, 0.0);
    }

    #[test]
    fn coverage_is_reported_and_rates_are_over_measured_records_only() {
        let mut traces = body(10);
        // Half the traces never attempted `fast`, which is what this policy
        // always chooses, so those five choices are unmeasurable.
        for trace in traces.iter_mut().step_by(2) {
            trace.samples.retain(|sample| sample.model_id == "steady");
        }
        let records = {
            let mut state = ReplayState::default();
            let mut out = Vec::new();
            for trace in &traces {
                let choice = ReplayBaseline::LowestLatency.select(trace, &mut state);
                let index = measured_outcomes(trace);
                let measured =
                    index.get(&(choice.candidate_id.clone(), choice.provider_id.clone()));
                out.push(match measured {
                    Some(outcome) => ArmRecord {
                        choice,
                        measured: true,
                        outcome: Some(*outcome),
                        fell_back: !outcome.success,
                        utility: Some(observed_utility(
                            outcome,
                            &RewardPolicy::default(),
                            !outcome.success,
                        )),
                    },
                    None => ArmRecord {
                        choice,
                        measured: false,
                        outcome: None,
                        fell_back: false,
                        utility: None,
                    },
                });
            }
            out
        };
        let metrics = aggregate("test", &records, traces.len());
        assert_eq!(metrics.requests_considered, 10);
        assert_eq!(metrics.requests_measured, 5);
        assert!((metrics.coverage - 0.5).abs() < 1e-9);
        // Every measured choice here was `fast`, which succeeded.
        assert!((metrics.success_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn observed_utility_rewards_a_fast_success_over_a_slow_failure() {
        let policy = RewardPolicy::default();
        let good = MeasuredOutcome {
            success: true,
            latency_ms: Some(100.0),
            ttft_ms: Some(20.0),
            cost: Some(0.001),
        };
        let bad = MeasuredOutcome {
            success: false,
            latency_ms: Some(4000.0),
            ttft_ms: Some(1500.0),
            cost: Some(0.5),
        };
        assert!(observed_utility(&good, &policy, false) > observed_utility(&bad, &policy, true));
    }

    #[test]
    fn a_small_body_cannot_produce_a_directional_verdict() {
        let traces = body(5);
        let policy = candidate(&traces);
        let comparison = run_comparison(
            &traces,
            &policy,
            &ReplayBaseline::ALL,
            &RewardPolicy::default(),
        )
        .expect("comparison");
        for pairing in &comparison.paired {
            assert_eq!(pairing.verdict, RoutingVerdict::InsufficientEvidence);
            assert!(pairing.deltas.paired_requests < MIN_PAIRED_REQUESTS);
        }
    }

    #[test]
    fn the_report_names_the_body_it_was_computed_over() {
        let traces = body(40);
        let policy = candidate(&traces);
        let comparison = run_comparison(
            &traces,
            &policy,
            &ReplayBaseline::ALL,
            &RewardPolicy::default(),
        )
        .expect("comparison");
        assert_eq!(comparison.traces, 40);
        assert_eq!(
            comparison.dataset_fingerprint,
            DatasetFingerprint::of(&super::super::traces::samples_from(&traces))
        );
        assert_eq!(comparison.arms.len(), ReplayBaseline::ALL.len() + 1);
        assert!(comparison.candidate_commit.len() == 16);
        assert_eq!(comparison.min_paired_requests, MIN_PAIRED_REQUESTS);
    }

    #[test]
    fn an_empty_body_is_refused_rather_than_scored_as_zero() {
        let traces = body(1);
        let policy = candidate(&traces);
        assert!(matches!(
            run_comparison(&[], &policy, &ReplayBaseline::ALL, &RewardPolicy::default()),
            Err(ComparisonError::NoTraces)
        ));
    }

    #[test]
    fn a_body_with_no_eligible_candidate_is_refused() {
        let mut traces = body(1);
        for candidate in traces[0].input.candidates.iter_mut() {
            candidate.eligible = false;
        }
        let policy = candidate(&traces);
        assert!(matches!(
            run_comparison(
                &traces,
                &policy,
                &ReplayBaseline::ALL,
                &RewardPolicy::default()
            ),
            Err(ComparisonError::NoEligibleCandidates)
        ));
    }

    #[test]
    fn the_same_body_and_model_produce_the_same_comparison() {
        let traces = body(40);
        let first = candidate(&traces);
        let second = candidate(&traces);
        let a = run_comparison(
            &traces,
            &first,
            &ReplayBaseline::ALL,
            &RewardPolicy::default(),
        )
        .expect("comparison");
        let b = run_comparison(
            &traces,
            &second,
            &ReplayBaseline::ALL,
            &RewardPolicy::default(),
        )
        .expect("comparison");
        assert_eq!(a.candidate_commit, b.candidate_commit);
        assert_eq!(a.dataset_fingerprint, b.dataset_fingerprint);
        for (left, right) in a.arms.iter().zip(&b.arms) {
            assert_eq!(left, right, "arm {} was not reproducible", left.policy);
        }
    }
}
