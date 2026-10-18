//! One entry point that answers "is this candidate actually better?", or says why it cannot.
//!
//! # What this is for
//!
//! The comparison machinery in `ml/comparison.rs` computes routing metrics over a body
//! of traces. What was missing is the layer above it: the audit of whether the data can
//! support a claim at all, the explicit statement of what is unknown, and a verdict that
//! is allowed to be "no".
//!
//! The central discipline is that **absence of evidence is reported as absence**. Three
//! specific ways this module refuses to flatter a candidate:
//!
//! 1. **Coverage before improvement.** A candidate that picks a different candidate on
//!    5% of requests and is unmeasured on 95% of them can show a large delta over its
//!    few paired samples. [`RoutingArmReport::coverage`] and the block on
//!    [`RoutingVerdict::Supported`] exist so a large delta on tiny coverage reads as
//!    `BLOCKED`, not as a win.
//! 2. **Unobserved fields stay unobserved.** A trace with no declared model, no cost, or
//!    no task evidence produces a coverage number, never a zero. See [`DataQualityReport`].
//! 3. **Prediction quality is reported separately from routing quality.** A better log
//!    loss is supporting evidence about a model, not evidence that routing improved, and
//!    the two are never combined into one score.
//!
//! # Determinism
//!
//! Given the same traces, policy and configuration the output is identical. The only
//! non-determinism anywhere in the ML statistics layer is absent by construction
//! (`ml/statistics.rs` documents that it uses closed-form tests and no resampling), and
//! this module adds none. `evaluated_at` is the one wall-clock field and it is excluded
//! from the fingerprint for exactly that reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ml::comparison::{
    run_comparison_with_records, ArmMetrics, BaselinePairing, ComparisonError, MlPolicy,
    ReplayBaseline, ReplayPolicy,
};
use crate::ml::promotion::{PromotionConfig, PromotionDecision, PromotionGate, PromotionVerdict};
use crate::ml::reward::RewardPolicy;
use crate::ml::task_evidence::EvidenceCoverage;
use crate::ml::temporal_planner::TemporalConfig;
use crate::ml::traces::{DatasetFingerprint, RequestTrace, TRACE_SCHEMA_VERSION};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What to evaluate, and what counts as enough.
///
/// Every threshold is explicit and recorded in the report. A verdict that depends on a
/// constant nobody can name is not reproducible, and the existing
/// `PromotionConfig::identity()` already establishes that convention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationConfig {
    /// Baselines to compare against. Always includes the production replay.
    pub baselines: Vec<ReplayBaseline>,
    pub reward_policy: RewardPolicy,
    pub promotion: PromotionConfig,
    /// Time-window configuration for the temporal resource replay.
    pub temporal: TemporalConfig,
    /// Minimum share of requests whose counterfactual choice must have a measured
    /// outcome. Below this the comparison is reported as blocked.
    pub min_coverage: f64,
    /// Minimum number of traces before any comparison is attempted at all.
    pub min_traces: usize,
    /// Statistical bar for the measured effect.
    pub evidence: crate::ml::eval_stats::EvidenceConfig,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            baselines: ReplayBaseline::ALL.to_vec(),
            reward_policy: RewardPolicy::default(),
            promotion: PromotionConfig::default(),
            temporal: TemporalConfig::default(),
            // Deliberately high. A comparison in which most counterfactual choices were
            // never executed is a comparison over a handful of lucky pairs.
            min_coverage: 0.30,
            min_traces: 50,
            evidence: crate::ml::eval_stats::EvidenceConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Data quality
// ---------------------------------------------------------------------------

/// How complete a body of traces is, field by field.
///
/// Every ratio here is over the traces that exist. Nothing is imputed: a missing cost
/// is a missing cost, and this report says how many so a reader can discount the metrics
/// that depend on it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataQualityReport {
    pub traces: usize,
    pub samples: usize,
    pub attempts: usize,
    /// Traces whose provider declared a model.
    pub declared_model: usize,
    /// Traces carrying a provider-declared model that disagrees with the request.
    pub declared_model_mismatch: usize,
    /// Traces whose samples carry a cost.
    pub with_cost: usize,
    /// Traces whose samples carry token usage.
    pub with_usage: usize,
    /// Traces carrying at least one attempt.
    pub with_fallback_chain: usize,
    /// Timestamps present, for the time-window report.
    pub earliest_recorded_at: Option<i64>,
    pub latest_recorded_at: Option<i64>,
    /// Task-evidence label coverage over the whole body.
    pub task_evidence: EvidenceCoverage,
    /// Distinct trace schema versions seen. More than one means the body spans an
    /// upgrade and the older records carry fewer fields.
    pub schema_versions: Vec<u32>,
}

impl DataQualityReport {
    /// Audit a body of traces without evaluating anything.
    pub fn of(traces: &[RequestTrace]) -> Self {
        let mut report = Self {
            traces: traces.len(),
            schema_versions: Vec::new(),
            ..Self::default()
        };
        let mut versions = std::collections::BTreeSet::new();
        let mut evidence = Vec::new();

        for trace in traces {
            versions.insert(trace.schema_version);
            if let Some(declared) = trace.execution_identity.provider_declared_model.value() {
                report.declared_model += 1;
                if declared != trace.execution_identity.requested_model {
                    report.declared_model_mismatch += 1;
                }
            }
            report.samples += trace.samples.len();
            report.attempts += trace
                .samples
                .iter()
                .map(|sample| sample.attempts.len())
                .sum::<usize>();
            if trace.samples.len() > 1 {
                report.with_fallback_chain += 1;
            }
            if trace
                .samples
                .iter()
                .any(|sample| sample.targets.cost.is_some())
            {
                report.with_cost += 1;
            }
            if trace.samples.iter().any(|sample| sample.usage.is_some()) {
                report.with_usage += 1;
            }
            let recorded = trace.recorded_at;
            report.earliest_recorded_at = Some(match report.earliest_recorded_at {
                Some(current) => current.min(recorded),
                None => recorded,
            });
            report.latest_recorded_at = Some(match report.latest_recorded_at {
                Some(current) => current.max(recorded),
                None => recorded,
            });
            evidence.extend(trace.task_evidence.iter().cloned());
        }

        report.schema_versions = versions.into_iter().collect();
        report.task_evidence = EvidenceCoverage::of(&evidence);
        report
    }

    fn ratio(count: usize, total: usize) -> f64 {
        if total == 0 {
            return 0.0;
        }
        count as f64 / total as f64
    }

    pub fn cost_coverage(&self) -> f64 {
        Self::ratio(self.with_cost, self.traces)
    }

    pub fn usage_coverage(&self) -> f64 {
        Self::ratio(self.with_usage, self.traces)
    }

    pub fn execution_identity_coverage(&self) -> f64 {
        Self::ratio(self.declared_model, self.traces)
    }

    /// Blocking findings, in the order an operator should fix them.
    pub fn blockers(&self) -> Vec<String> {
        let mut blockers = Vec::new();
        if self.traces == 0 {
            blockers.push("no traces on disk: nothing has been collected to evaluate".to_string());
            return blockers;
        }
        if self.task_evidence.observed == 0 {
            blockers.push(format!(
                "no task-quality evidence in {} traces: only API success is measurable, so no \
                 task-completion claim can be made",
                self.traces
            ));
        }
        if self.cost_coverage() < 0.5 {
            blockers.push(format!(
                "cost coverage is {:.0}% ({} of {} traces): cost metrics rest on a minority of samples",
                self.cost_coverage() * 100.0,
                self.with_cost,
                self.traces
            ));
        }
        if self.execution_identity_coverage() < 0.5 {
            blockers.push(format!(
                "execution-identity coverage is {:.0}%: which backend served a request is unknown for \
                 most traffic, so per-model quality figures may be measuring a relay's routing",
                self.execution_identity_coverage() * 100.0
            ));
        }
        blockers
    }
}

/// What the temporal resource policy would have done, across a body of traces.
///
/// A disagreement count is a **blast radius**, not an effect: it says where a live
/// rollout would first change behaviour, and nothing at all about whether the
/// alternative would have been better.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceShadowSummary {
    /// Traces carrying a temporal shadow record.
    pub recorded: usize,
    /// Records where the policy preferred an eligible candidate.
    pub with_preference: usize,
    /// Records where that preference differed from production's choice.
    pub disagreements: usize,
}

impl ResourceShadowSummary {
    /// Share of records that disagreed, with its denominator.
    ///
    /// The pair, because a rate without its denominator invites a reader to assume it
    /// covers every request. Traces carrying no record are **not** agreement — they are
    /// absence, and they are excluded from the denominator rather than counted as
    /// agreement.
    pub fn disagreement_rate(&self) -> (f64, usize) {
        if self.with_preference == 0 {
            return (0.0, 0);
        }
        (
            self.disagreements as f64 / self.with_preference as f64,
            self.with_preference,
        )
    }
}

/// Whether task-completion quality could be judged at all.
///
/// Kept separate from the routing verdict rather than folded into it. A body can
/// support a confident routing claim — this policy made requests fail less often —
/// while saying nothing whatsoever about whether the tasks got done. Reporting one
/// verdict for both is how "fewer 500s" quietly becomes "better answers".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum TaskQualityVerdict {
    /// Task verdicts exist and are attributable to a candidate.
    Available {
        /// Success rate over observed verdicts.
        rate: f64,
        /// The denominator the rate was computed over.
        denominator: usize,
    },
    /// No usable task evidence.
    Unavailable { reason: String },
}

impl TaskQualityVerdict {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

// ---------------------------------------------------------------------------
// Per-arm reporting
// ---------------------------------------------------------------------------

/// One policy's result, with its coverage stated alongside its metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingArmReport {
    pub policy: String,
    pub metrics: ArmMetrics,
    /// Share of requests where this policy's choice had a measured outcome.
    pub coverage: f64,
    /// Task-quality success rate over evidence attributable to this arm, with its
    /// denominator. `None` when no attributable evidence exists — never `Some(0.0)`.
    pub task_success_rate: Option<(f64, usize)>,
}

impl RoutingArmReport {
    /// Whether this arm has enough measured outcomes to be compared at all.
    pub fn is_measurable(&self) -> bool {
        self.metrics.requests_measured > 0
    }
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// The outcome of an evaluation, including the option of declining to conclude.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum EvaluationVerdict {
    /// The candidate was measured and beaten the production baseline.
    Supported {
        /// The measured improvement in observed utility, over paired requests only.
        utility_delta: f64,
        /// Paired requests the delta was computed over.
        paired_requests: usize,
    },
    /// Measured, and the candidate did not improve on the baseline.
    NotImproved { reason: String },
    /// Measured well enough to judge, and judged worse.
    Regressed { reason: String },
    /// The evidence cannot support a claim either way.
    ///
    /// The important variant. It is what a thin dataset produces, and reporting it as
    /// `NotImproved` would be a lie: "we cannot tell" and "it does not help" are
    /// different findings, and only the second is about the candidate.
    Blocked { reasons: Vec<String> },
}

impl EvaluationVerdict {
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    /// Whether this verdict may contribute to a promotion decision.
    pub fn is_promotable(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }

    /// A single line for a human reader.
    pub fn headline(&self) -> String {
        match self {
            Self::Supported {
                utility_delta,
                paired_requests,
            } => format!(
                "improved observed utility by {utility_delta:+.4} over {paired_requests} paired requests"
            ),
            Self::NotImproved { reason } => format!("no improvement: {reason}"),
            Self::Regressed { reason } => format!("regressed: {reason}"),
            Self::Blocked { reasons } => format!(
                "blocked: {}",
                reasons.join("; ")
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// Everything one evaluation produced.
///
/// Serialises to the machine-readable artifact; [`EvaluationReport::to_markdown`]
/// renders the same facts for a person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub schema_version: u32,
    /// Identity of the exact trace body every number was computed over.
    pub dataset_fingerprint: String,
    pub trace_schema_version: u32,
    pub data_quality: DataQualityReport,
    pub arms: Vec<RoutingArmReport>,
    pub verdict: EvaluationVerdict,
    /// Whether task quality could be judged, reported separately from routing so an
    /// improvement in API success is never read as an improvement in output quality.
    pub task_quality: TaskQualityVerdict,
    /// The production-baseline pairing, kept for the promotion gate.
    pub production_pairing: Option<BaselinePairing>,
    /// Whether the measured effect survives its own uncertainty.
    pub evidence: crate::ml::eval_stats::RoutingEvidence,
    /// What the temporal resource policy would have chosen, across the body.
    pub resource_shadow: ResourceShadowSummary,
    pub promotion_verdict: Option<PromotionVerdict>,
    /// Per-criterion promotion findings, when the gate ran.
    pub promotion_decision: Option<PromotionDecision>,
    /// Baselines actually run, with the definition of each recorded. A replay
    /// heuristic is not a production strategy and the report must not imply it is.
    pub baselines: Vec<String>,
    /// Fingerprint of the config, so a verdict can be traced to its thresholds.
    pub config_identity: u64,
    pub temporal_config_fingerprint: u64,
    /// Excluded from the fingerprint: the one wall-clock field.
    pub evaluated_at: i64,
}

impl EvaluationReport {
    pub fn arm(&self, policy: &str) -> Option<&RoutingArmReport> {
        self.arms.iter().find(|arm| arm.policy == policy)
    }

    /// Everything that stopped this evaluation reaching a conclusion.
    ///
    /// Deduplicated: a verdict's reasons already incorporate the data-quality
    /// findings, and listing both would have an operator chasing the same blocker
    /// twice and wondering whether it counted double.
    pub fn blockers(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        let mut blockers = Vec::new();
        let mut push = |reason: String| {
            if seen.insert(reason.clone()) {
                blockers.push(reason);
            }
        };
        for reason in self.data_quality.blockers() {
            push(reason);
        }
        if let EvaluationVerdict::Blocked { reasons } = &self.verdict {
            for reason in reasons {
                push(reason.clone());
            }
        }
        if let TaskQualityVerdict::Unavailable { reason } = &self.task_quality {
            push(reason.clone());
        }
        blockers
    }

    /// Render the report for a person. Every number keeps its denominator.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Routing evaluation\n\n");
        out.push_str(&format!("**Verdict:** {}\n\n", self.verdict.headline()));

        out.push_str("## Provenance\n\n");
        out.push_str(&format!(
            "- Dataset fingerprint: `{}`\n",
            self.dataset_fingerprint
        ));
        out.push_str(&format!("- Trace schema: v{}\n", self.trace_schema_version));
        out.push_str(&format!(
            "- Schema versions present: {:?}\n",
            self.data_quality.schema_versions
        ));
        out.push_str(&format!(
            "- Config identity: `{:#018x}`\n",
            self.config_identity
        ));
        out.push_str(&format!(
            "- Temporal config: `{:#018x}`\n",
            self.temporal_config_fingerprint
        ));
        out.push_str(&format!("- Traces: {}\n", self.data_quality.traces));

        out.push_str("\n## Data quality\n\n");
        out.push_str("| Field | Count | Coverage |\n|---|---|---|\n");
        out.push_str(&format!(
            "| Provider-declared model | {} | {:.0}% |\n",
            self.data_quality.declared_model,
            self.data_quality.execution_identity_coverage() * 100.0
        ));
        out.push_str(&format!(
            "| Declared model disagrees with request | {} | — |\n",
            self.data_quality.declared_model_mismatch
        ));
        out.push_str(&format!(
            "| Cost present | {} | {:.0}% |\n",
            self.data_quality.with_cost,
            self.data_quality.cost_coverage() * 100.0
        ));
        out.push_str(&format!(
            "| Usage present | {} | {:.0}% |\n",
            self.data_quality.with_usage,
            self.data_quality.usage_coverage() * 100.0
        ));
        out.push_str(&format!(
            "| Multi-attempt (fallback) traces | {} | {:.0}% |\n",
            self.data_quality.with_fallback_chain,
            DataQualityReport::ratio(
                self.data_quality.with_fallback_chain,
                self.data_quality.traces
            ) * 100.0
        ));

        out.push_str("\n### Task-quality label coverage\n\n");
        let coverage = &self.data_quality.task_evidence;
        if coverage.total == 0 {
            out.push_str("- No task verifier has run against any request.\n");
        } else {
            out.push_str(&format!(
                "- Total evidence records: {}\n- With a real verdict: {} ({:.0}%)\n- Unverified: {} ({:.0}%)\n- Mechanical verifiers: {}\n- Model-judged: {}\n",
                coverage.total,
                coverage.observed,
                coverage.observed_rate() * 100.0,
                coverage.total - coverage.observed,
                (1.0 - coverage.observed_rate()) * 100.0,
                coverage.mechanical,
                coverage.model_judged,
            ));
        }
        out.push_str(
            "\n> An unverified result is neither a success nor a failure. Every task-quality rate below \
             states the denominator it was computed over.\n",
        );

        out.push_str("\n## Task quality\n\n");
        match &self.task_quality {
            TaskQualityVerdict::Available { rate, denominator } => {
                out.push_str(&format!(
                    "- Task completion rate: {:.1}% over {denominator} verified verdict(s)\n",
                    rate * 100.0
                ));
            }
            TaskQualityVerdict::Unavailable { reason } => {
                out.push_str(&format!(
                    "- **Unavailable** — {reason}\n- No claim is made about output quality. Fewer \
                     failures is not the same as better answers.\n"
                ));
            }
        }

        out.push_str("\n## Statistical evidence\n\n");
        let evidence = &self.evidence;
        match evidence.utility {
            Some(interval) => {
                out.push_str(&format!(
                    "- Effect on observed utility: **{:+.4}** (95% interval {:+.4} to {:+.4})\n",
                    interval.mean, interval.lower, interval.upper
                ));
                out.push_str(&format!(
                    "- Paired requests: {} in {} time cluster(s)\n",
                    evidence.pairs, evidence.clusters
                ));
                out.push_str(&format!(
                    "- Minimum effect worth acting on: {:+.4}\n",
                    evidence.minimum_effect
                ));
                if let Some(required) = evidence.required_pairs {
                    out.push_str(&format!(
                        "- Pairs needed to detect that effect: {required}\n"
                    ));
                }
                if interval.is_zero_dispersion {
                    out.push_str(
                        "- Dispersion was exactly zero, so this interval has no width. Treat the \
                         precision as an artefact of the data rather than a finding.\n",
                    );
                }
            }
            None => out.push_str("- No interval could be computed.\n"),
        }
        if !evidence.blockers.is_empty() {
            for blocker in &evidence.blockers {
                out.push_str(&format!("- Blocked: {blocker}\n"));
            }
        }
        for caveat in &evidence.caveats {
            out.push_str(&format!("- Caveat: {caveat}\n"));
        }

        out.push_str("\n## Temporal resource shadow\n\n");
        let shadow = &self.resource_shadow;
        let (rate, denominator) = shadow.disagreement_rate();
        out.push_str(&format!(
            "- Traces carrying a temporal shadow record: {}\n- Of those, with a preference: {}\n",
            shadow.recorded, shadow.with_preference
        ));
        if denominator == 0 {
            out.push_str(
                "- No preference was expressed: there were no configured resource buckets to plan \
                 over. This is an absence of evidence, not agreement with production.\n",
            );
        } else {
            out.push_str(&format!(
                "- Would have disagreed with production: {:.0}% of {denominator}\n",
                rate * 100.0
            ));
        }
        out.push_str(
            "\n> A disagreement is a blast radius, not an effect. It marks where a live rollout \
             would first change behaviour; it says nothing about whether the alternative would \
             have performed better, because no alternative was executed.\n",
        );

        out.push_str("\n## Routing comparison\n\n");
        out.push_str(
            "| Policy | Considered | Measured | Coverage | Success | P95 ms | Mean cost |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|\n");
        for arm in &self.arms {
            out.push_str(&format!(
                "| {} | {} | {} | {:.0}% | {:.1}% | {:.0} | {:.4} |\n",
                arm.policy,
                arm.metrics.requests_considered,
                arm.metrics.requests_measured,
                arm.coverage * 100.0,
                arm.metrics.success_rate * 100.0,
                arm.metrics.p95_latency_ms,
                arm.metrics.mean_cost,
            ));
        }

        out.push_str("\n## Baselines run\n\n");
        for baseline in &self.baselines {
            out.push_str(&format!("- {baseline}\n"));
        }

        if let Some(pairing) = &self.production_pairing {
            out.push_str("\n## Paired against production\n\n");
            let d = &pairing.deltas;
            out.push_str(&format!("- Paired requests: {}\n", d.paired_requests));
            out.push_str(&format!(
                "- Observed utility delta: {:+.4}\n",
                d.mean_observed_utility_delta
            ));
            out.push_str(&format!(
                "- Success rate delta: {:+.4}\n",
                d.success_rate_delta
            ));
            out.push_str(&format!("- Mean cost delta: {:+.4}\n", d.mean_cost_delta));
            out.push_str(&format!(
                "- Mean latency delta: {:+.1} ms\n",
                d.mean_latency_delta_ms
            ));
            out.push_str(&format!(
                "- Candidate regressions: {}\n",
                d.candidate_regressions
            ));
        }

        if let Some(promotion) = self.promotion_verdict {
            out.push_str(&format!(
                "\n**Promotion verdict:** {}\n",
                promotion_wire(promotion)
            ));
        }

        let blockers = self.blockers();
        if !blockers.is_empty() {
            out.push_str("\n## Why this is blocked\n\n");
            for blocker in blockers {
                out.push_str(&format!("- {blocker}\n"));
            }
        }
        out
    }
}

fn promotion_wire(verdict: PromotionVerdict) -> &'static str {
    match verdict {
        PromotionVerdict::Promoted => "PROMOTED",
        PromotionVerdict::Rejected => "REJECTED",
        PromotionVerdict::Blocked => "BLOCKED",
    }
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Errors distinct from a verdict.
///
/// A failure to *run* is not a verdict of "blocked": it means the report does not
/// exist, and conflating the two would let a bug present itself as insufficient data.
#[derive(Debug, thiserror::Error)]
pub enum EvaluationError {
    #[error(transparent)]
    Comparison(#[from] ComparisonError),
}

/// Run one evaluation over a body of traces.
///
/// Never contacts an upstream and never sends a model request. It reads only the
/// traces it is given, which is what makes it safe to run on a schedule and what makes
/// "this did not affect production" true by construction rather than by policy.
pub fn evaluate(
    traces: &[RequestTrace],
    candidate: &MlPolicy,
    config: &EvaluationConfig,
    training: Option<&crate::ml::learning::TrainingReport>,
    evaluated_at: i64,
) -> Result<EvaluationReport, EvaluationError> {
    let quality = DataQualityReport::of(traces);
    let config_identity = config_identity(config);

    // Short-circuit before any comparison runs. Running the comparison over a body too
    // small to support it and then reporting the metrics would invite a reader to
    // quote them.
    if traces.len() < config.min_traces {
        return Ok(blocked_report(
            &quality,
            config,
            config_identity,
            evaluated_at,
            vec![format!(
                "{} traces is below the {} needed for a comparison to mean anything",
                traces.len(),
                config.min_traces
            )],
            &quality.blockers(),
        ));
    }

    let run =
        run_comparison_with_records(traces, candidate, &config.baselines, &config.reward_policy)?;
    let comparison = run.comparison.clone();

    // Task-quality attribution is computed per arm over evidence that names the
    // attempt that served, so a request-level verdict is never credited to a model.
    let task_by_model: BTreeMap<String, EvidenceCoverage> = task_coverage_by_model(traces);

    let arms: Vec<RoutingArmReport> = comparison
        .arms
        .iter()
        .map(|metrics| {
            let coverage = task_by_model
                .get(&metrics.policy)
                .cloned()
                .unwrap_or_default();
            let (rate, denominator) = coverage.success_rate();
            RoutingArmReport {
                policy: metrics.policy.clone(),
                coverage: metrics.coverage,
                metrics: metrics.clone(),
                task_success_rate: if denominator > 0 {
                    Some((rate, denominator))
                } else {
                    None
                },
            }
        })
        .collect();

    let production_pairing = comparison
        .pairing("policy.production")
        .or_else(|| comparison.paired.first())
        .cloned();

    let mut reasons = Vec::new();
    if let Some(arm) = arms.iter().find(|arm| arm.policy == candidate.name()) {
        // Coverage gate, applied before the delta is even read.
        if arm.coverage < config.min_coverage {
            reasons.push(format!(
                "counterfactual coverage is {:.0}%, below the {:.0}% floor: the candidate's \
                 alternative choices were executed and measured too rarely to compare",
                arm.coverage * 100.0,
                config.min_coverage * 100.0
            ));
        }
    } else {
        reasons.push(format!(
            "the candidate arm `{}` produced no metrics",
            candidate.name()
        ));
    }

    // Hard-constraint violations are never a quality question.
    if let Some(arm) = arms.iter().find(|arm| arm.policy == candidate.name()) {
        if arm.metrics.ineligible_selections > 0 {
            reasons.push(format!(
                "candidate made {} selection(s) that violate hard constraints",
                arm.metrics.ineligible_selections
            ));
        }
    }

    let pairing = production_pairing.as_ref();

    // -- Statistical evidence -----------------------------------------------
    //
    // The pair floor above says "enough requests to look at". This says whether the
    // effect survives its own uncertainty, clustered by time so correlated requests
    // are not counted as independent evidence. A candidate whose interval does not
    // clear the minimum detectable effect is `BLOCKED`, not `Supported` — a precise
    // measurement of an effect too small to act on is not a win.
    let evidence = routing_evidence(&run, traces, &config.evidence);
    if !evidence.is_sufficient() {
        reasons.extend(evidence.blockers.iter().cloned());
    }
    match pairing {
        None => reasons.push("no baseline pairing was produced".to_string()),
        Some(pairing) if reasons.is_empty() => {
            if pairing.deltas.paired_requests < config.promotion.min_paired_requests {
                reasons.push(format!(
                    "{} paired requests is below the {} required",
                    pairing.deltas.paired_requests, config.promotion.min_paired_requests
                ));
            }
        }
        Some(_) => {}
    }

    let verdict = if !reasons.is_empty() {
        // Data-quality blockers and coverage blockers combine: either alone is enough.
        let mut all = reasons;
        all.extend(quality.blockers());
        EvaluationVerdict::Blocked { reasons: all }
    } else {
        let pairing = pairing.expect("checked above");
        let d = &pairing.deltas;
        if d.candidate_regressions > config.promotion.max_success_regressions {
            EvaluationVerdict::Regressed {
                reason: format!(
                    "{} paired success regression(s) exceed the allowed {}",
                    d.candidate_regressions, config.promotion.max_success_regressions
                ),
            }
        } else if d.mean_observed_utility_delta < config.promotion.min_utility_delta {
            EvaluationVerdict::NotImproved {
                reason: format!(
                    "observed utility delta {:+.4} is below the required {:+.4}",
                    d.mean_observed_utility_delta, config.promotion.min_utility_delta
                ),
            }
        } else {
            EvaluationVerdict::Supported {
                utility_delta: d.mean_observed_utility_delta,
                paired_requests: d.paired_requests,
            }
        }
    };

    // The promotion gate is only consulted when the comparison itself reached a
    // verdict, and only when a training report was supplied. The gate refuses a
    // comparison whose dataset fingerprint does not match the one the model was
    // fitted on — a real protection, but it means the gate genuinely cannot be
    // evaluated without the training side, so its absence is reported as absent
    // rather than guessed at.
    let promotion = match (&verdict, training) {
        (EvaluationVerdict::Supported { .. }, Some(training)) => {
            Some(PromotionGate::new(config.promotion.clone()).evaluate(training, &comparison, None))
        }
        _ => None,
    };
    let promotion_verdict = promotion.as_ref().map(|decision| decision.verdict);

    // Task quality is judged independently of the routing verdict. A measured
    // improvement in API success rate is a real finding and is reported as one; what
    // is withheld is any claim that outputs got better.
    let attributable: usize = task_by_model
        .values()
        .map(|coverage| coverage.observed)
        .sum();
    let task_quality = match attributable {
        0 => TaskQualityVerdict::Unavailable {
            reason: format!(
                "no attempt-attributable task verdicts in {} traces: API success is measurable, \
                 task completion is not",
                quality.traces
            ),
        },
        _ => {
            let total_observed: usize = task_by_model
                .values()
                .map(|coverage| coverage.observed)
                .sum();
            let total_succeeded: usize = task_by_model
                .values()
                .map(|coverage| coverage.succeeded)
                .sum();
            let total_partial: usize = task_by_model.values().map(|c| c.partial).sum();
            TaskQualityVerdict::Available {
                rate: (total_succeeded as f64 + total_partial as f64 * 0.5) / total_observed as f64,
                denominator: total_observed,
            }
        }
    };

    Ok(EvaluationReport {
        schema_version: TRACE_SCHEMA_VERSION,
        dataset_fingerprint: comparison.dataset_fingerprint.to_string(),
        trace_schema_version: TRACE_SCHEMA_VERSION,
        data_quality: quality,
        arms,
        verdict,
        task_quality,
        evidence: evidence.clone(),
        resource_shadow: resource_shadow_summary(traces),
        production_pairing,
        promotion_verdict,
        promotion_decision: promotion,
        baselines: config
            .baselines
            .iter()
            .map(|baseline| baseline_label(*baseline))
            .collect(),
        config_identity,
        temporal_config_fingerprint: config.temporal.fingerprint(),
        evaluated_at,
    })
}

fn blocked_report(
    quality: &DataQualityReport,
    config: &EvaluationConfig,
    config_identity: u64,
    evaluated_at: i64,
    reasons: Vec<String>,
    _quality_blockers: &[String],
) -> EvaluationReport {
    let mut all = reasons;
    all.extend(quality.blockers());
    EvaluationReport {
        schema_version: TRACE_SCHEMA_VERSION,
        dataset_fingerprint: DatasetFingerprint::of(&[]).to_string(),
        trace_schema_version: TRACE_SCHEMA_VERSION,
        data_quality: quality.clone(),
        arms: Vec::new(),
        verdict: EvaluationVerdict::Blocked { reasons: all },
        evidence: crate::ml::eval_stats::measure(
            &[],
            &crate::ml::eval_stats::EvidenceConfig::default(),
        ),
        task_quality: TaskQualityVerdict::Unavailable {
            reason: "no evaluation ran".to_string(),
        },
        production_pairing: None,
        promotion_verdict: None,
        promotion_decision: None,
        baselines: config
            .baselines
            .iter()
            .map(|baseline| baseline_label(*baseline))
            .collect(),
        config_identity,
        temporal_config_fingerprint: config.temporal.fingerprint(),
        evaluated_at,
        resource_shadow: ResourceShadowSummary::default(),
    }
}

/// Turn one policy's arm records into paired observations for the interval.
///
/// `PairedDeltas` carries sums and counts, which gives a mean but not a variance, so
/// the per-request values have to be rebuilt from the records. Only requests where
/// **both** arms were measured contribute — the same set `pair_against_baseline` uses,
/// so the interval and the reported mean describe the same population.
fn routing_evidence(
    run: &crate::ml::comparison::ComparisonRun,
    traces: &[RequestTrace],
    config: &crate::ml::eval_stats::EvidenceConfig,
) -> crate::ml::eval_stats::RoutingEvidence {
    // Timestamps only enter to form time-block clusters.
    let recorded: BTreeMap<&str, i64> = traces
        .iter()
        .map(|trace| (trace.request_id.as_str(), trace.recorded_at))
        .collect();

    let baseline_name = crate::ml::promotion::PromotionConfig::default().required_baseline;
    let Some(baseline) = run.records.get(baseline_name.as_str()) else {
        return crate::ml::eval_stats::measure(&[], config);
    };
    let Some(candidate) = run.records.get(
        run.comparison
            .arms
            .last()
            .map(|arm| arm.policy.as_str())
            .unwrap_or_default(),
    ) else {
        return crate::ml::eval_stats::measure(&[], config);
    };

    let candidate_by_request: BTreeMap<&str, &crate::ml::comparison::ArmRecord> = candidate
        .iter()
        .filter(|record| record.measured)
        .map(|record| (record.choice.request_id.as_str(), record))
        .collect();

    let observations: Vec<crate::ml::eval_stats::PairedObservation> = baseline
        .iter()
        .filter(|record| record.measured)
        .filter_map(|base| {
            let other = candidate_by_request.get(base.choice.request_id.as_str())?;
            let (base_outcome, other_outcome) = (base.outcome?, other.outcome?);
            let base_utility = crate::ml::comparison::observed_utility(
                &base_outcome,
                &run.comparison.reward_policy,
                base.fell_back,
            );
            let other_utility = crate::ml::comparison::observed_utility(
                &other_outcome,
                &run.comparison.reward_policy,
                other.fell_back,
            );
            Some(crate::ml::eval_stats::PairedObservation {
                recorded_at: recorded
                    .get(base.choice.request_id.as_str())
                    .copied()
                    .unwrap_or_default(),
                utility_delta: other_utility - base_utility,
                candidate_only_success: !base_outcome.success && other_outcome.success,
                baseline_only_success: base_outcome.success && !other_outcome.success,
            })
        })
        .collect();

    crate::ml::eval_stats::measure(&observations, config)
}

/// Summarise the temporal resource shadow over a body of traces.
///
/// Traces carrying no record are excluded rather than counted as agreement: a trace
/// with nothing to plan over is an absence of evidence, and folding it into the
/// denominator would understate the disagreement rate for a body that mostly never
/// produced one.
fn resource_shadow_summary(traces: &[RequestTrace]) -> ResourceShadowSummary {
    let mut summary = ResourceShadowSummary::default();
    for trace in traces {
        let Some(shadow) = trace.resource_shadow.as_ref() else {
            continue;
        };
        summary.recorded += 1;
        if shadow.preferred.is_some() {
            summary.with_preference += 1;
            if shadow.is_disagreement() {
                summary.disagreements += 1;
            }
        }
    }
    summary
}

/// Group task evidence by the candidate that actually served it.
fn task_coverage_by_model(traces: &[RequestTrace]) -> BTreeMap<String, EvidenceCoverage> {
    let mut out: BTreeMap<String, EvidenceCoverage> = BTreeMap::new();
    for trace in traces {
        for evidence in trace.attributable_task_evidence() {
            let Some(model) = evidence.served_model.as_deref() else {
                continue;
            };
            out.entry(model.to_string()).or_default().observe(evidence);
        }
    }
    out
}

/// The definition of each baseline, recorded so a heuristic is not read as production.
fn baseline_label(baseline: ReplayBaseline) -> String {
    match baseline {
        ReplayBaseline::Priority => {
            "priority (static priority from the decision-time feature vector)".to_string()
        }
        ReplayBaseline::RoundRobin => {
            "round_robin (eligible candidates in rotating order)".to_string()
        }
        ReplayBaseline::LowestLatency => {
            "lowest_latency (observed latency EWMA from the trace)".to_string()
        }
        ReplayBaseline::Balanced => {
            "balanced (latency/health replay heuristic — NOT the production Balanced strategy, \
             which is a per-tier latency/price election this trace does not record)"
                .to_string()
        }
    }
}

fn config_identity(config: &EvaluationConfig) -> u64 {
    // FNV-1a over every threshold, matching the convention already used by
    // `PromotionConfig::identity()`. A verdict is only reproducible if the thresholds
    // that produced it can be named.
    let mut hash = 0xcbf29ce484222325u64;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for baseline in &config.baselines {
        mix(baseline.as_str().as_bytes());
    }
    mix(&config.min_coverage.to_bits().to_le_bytes());
    mix(config.min_traces.to_le_bytes().as_slice());
    mix(config.temporal.fingerprint().to_le_bytes().as_slice());
    hash
}

/// Whether an evaluation reached a conclusion the promotion path may act on.
///
/// Three things must all hold, and the task-quality requirement is not negotiable:
/// an evaluation that measured only API success has not shown that routing makes
/// anything *better*, only that it makes requests fail less often.
pub fn is_promotable(report: &EvaluationReport) -> bool {
    report.verdict.is_promotable()
        && report.task_quality.is_available()
        && matches!(report.promotion_verdict, Some(PromotionVerdict::Promoted))
}

/// A one-line summary of what this body of data can currently support.
pub fn evaluability(quality: &DataQualityReport, config: &EvaluationConfig) -> Vec<String> {
    let mut notes = Vec::new();
    if quality.traces == 0 {
        return vec!["no traces collected yet: serve real traffic, then re-run".to_string()];
    }
    if quality.traces < config.min_traces {
        notes.push(format!(
            "{} of {} traces needed for a comparison",
            config.min_traces.saturating_sub(quality.traces),
            config.min_traces
        ));
    }
    if quality.task_evidence.observed == 0 {
        notes.push(
            "no task-quality evidence: an API-success-only router is all that can be evaluated"
                .to_string(),
        );
    }
    if notes.is_empty() {
        notes.push("data is sufficient for a routing comparison".to_string());
    }
    notes
}
