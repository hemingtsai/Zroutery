//! Task-completion evidence, kept separate from API success.
//!
//! # Why this is not another boolean
//!
//! `Outcome::success` answers "did the transport and protocol layers complete". That
//! is a fact about the exchange, and it is the right label for learning *reliability*.
//! It is the wrong label for learning *quality*, because a 200 carrying a syntactically
//! perfect answer to the wrong thing is a success by every field the outcome records.
//!
//! Nothing in the tree currently distinguishes these. `Feedback` models user reactions
//! but is keyed on `outcome_id` and is never consumed by training or evaluation
//! (`ml/bandit.rs` states outright that no user-preference signal exists in the data).
//! So the practical consequence today is that a model can be shown to be better at
//! *returning responses* while being worse at *completing tasks*, and nothing in the
//! pipeline would notice.
//!
//! # The `Unknown` requirement
//!
//! [`TaskStatus::Unknown`] is the most important variant and is not a failure to
//! implement. Three arguments make it load-bearing:
//!
//! 1. Most requests have no objective verifier. A chat turn is not compile-testable.
//! 2. Collapsing `Unknown` into `Failed` biases every statistic downward and would
//!    make unverified traffic look like evidence of poor models.
//! 3. Collapsing it into `Succeeded` manufactures quality from absence.
//!
//! Every quality figure derived from this module therefore reports its denominator
//! explicitly (see [`EvidenceCoverage`]) rather than letting an unknown silently
//! vanish from a rate.
//!
//! # Verifier authority
//!
//! Evidence is not equally trustworthy, and [`VerifierKind`] records which is which. A
//! compile-and-test run is close to ground truth for a code task; a model judging its
//! own output is not, and [`VerifierKind::ModelJudge`] is explicitly marked as a
//! possibly-biased source that must be evaluated for its own reliability rather than
//! folded silently into the same pool as a test run.

use serde::{Deserialize, Serialize};

use crate::policy::TaskType;

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Whether the task the request was for actually got done.
///
/// Distinct from `Outcome::success`, which reports only that the exchange completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Verified complete against a stated criterion.
    Succeeded,
    /// Verified not complete, or verification found the goal unmet.
    Failed,
    /// Partially complete. Kept separate from both because averaging it into either
    /// end invents a result nobody observed.
    Partial,
    /// No verdict. Not a failure and not a success.
    Unknown,
}

impl TaskStatus {
    /// Whether this status is a real observation.
    pub fn is_observed(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Numeric encoding for a scalar aggregate.
    ///
    /// `Unknown` is deliberately *absent* rather than encoded. Callers wanting a mean
    /// over all rows must use [`EvidenceCoverage::observed_rate`] to state the
    /// denominator; [`observed_fraction`] exists only for rows already known observed.
    pub fn observed_fraction(self) -> Option<f64> {
        match self {
            Self::Succeeded => Some(1.0),
            Self::Failed => Some(0.0),
            Self::Partial => Some(0.5),
            Self::Unknown => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

/// What kind of check produced a verdict.
///
/// The ordering of authority is the point of this enum: `Compilation` and friends are
/// mechanical and reproducible, `ModelJudge` is a judgement call by another model and
/// carries known biases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierKind {
    /// Build/test/static-analysis result for a code task.
    Compilation,
    /// Output parsed and validated against a schema.
    SchemaValidation,
    /// A tool was executed and its real effect checked against the expectation.
    ToolExecution,
    /// The requested post-condition of a file/config change was checked.
    StateVerification,
    /// A check with a known expected answer was reproduced.
    AnswerCheck,
    /// Explicit user rating.
    ExplicitRating,
    /// Heuristic from later user behaviour (retry, rewrite, abandon).
    BehaviouralSignal,
    /// Another model judged the output.
    ///
    /// Recorded separately rather than merged into the mechanical kinds because its
    /// verdicts are fallible and correlated with the thing being judged.
    ModelJudge,
}

impl VerifierKind {
    /// Mechanical, reproducible checks, whose verdict is close to ground truth.
    pub fn is_mechanical(self) -> bool {
        matches!(
            self,
            Self::Compilation
                | Self::SchemaValidation
                | Self::ToolExecution
                | Self::StateVerification
                | Self::AnswerCheck
        )
    }

    /// Whether the verdict rests on a *person* rather than a check.
    pub fn is_human(self) -> bool {
        matches!(self, Self::ExplicitRating)
    }

    /// Whether the verdict is a weak signal that may not be equivalent to a rating.
    ///
    /// A retry is evidence the user was not satisfied; it is not a negative score and
    /// must not be averaged in as one.
    pub fn is_weak(self) -> bool {
        matches!(self, Self::BehaviouralSignal)
    }

    /// Default confidence in a verdict of this kind, before per-evidence adjustment.
    ///
    /// Mechanical checks are trusted; a model judge is not, because it is a biased
    /// estimator whose bias has not been measured in this data.
    pub fn base_confidence(self) -> f64 {
        match self {
            Self::Compilation
            | Self::SchemaValidation
            | Self::ToolExecution
            | Self::StateVerification
            | Self::AnswerCheck => 0.95,
            Self::ExplicitRating => 0.85,
            Self::BehaviouralSignal => 0.40,
            Self::ModelJudge => 0.50,
        }
    }

    /// Whether this kind of evidence may settle a quality gate on its own.
    ///
    /// Weak signals and model judges may inform, but must not be the sole basis for
    /// declaring a candidate task-quality-inferior, so a promotion criterion cannot
    /// rest on them alone.
    pub fn can_settle_alone(self) -> bool {
        self.is_mechanical() || self.is_human()
    }
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// Where a piece of task evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    /// A verifier Zroutery ran itself in-process (schema check, tool result).
    RuntimeVerifier,
    /// Reported by the client.
    Client,
    /// Produced by an external evaluation harness.
    ExternalHarness,
    /// Imported from elsewhere (CC Switch, a user dataset).
    Imported,
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// One verified claim about whether a task completed.
///
/// Attaches to the **attempt that produced the content**, not to the request. This is
/// the attribution rule that matters in a fallback chain: if candidate A failed and
/// candidate B served, a test pass belongs to B, and filing it against A teaches the
/// model that A succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcomeEvidence {
    pub evidence_id: String,
    /// The request this belongs to.
    pub request_id: String,
    /// The outcome of that request.
    pub outcome_id: String,
    /// The attempt whose output was judged. `None` when the verifier judged the
    /// request as a whole (only correct for verifiers that cannot be
    /// attempt-scoped).
    #[serde(default)]
    pub attempt_id: Option<String>,
    /// The candidate that actually served the judged content.
    #[serde(default)]
    pub served_model: Option<String>,
    #[serde(default)]
    pub served_provider: Option<String>,
    pub task_type: TaskType,
    pub verifier: VerifierKind,
    pub status: TaskStatus,
    /// Reliability of this verdict in `[0, 1]`. Seeded from
    /// [`VerifierKind::base_confidence`] and lowered for weak or contested evidence.
    pub confidence: f64,
    /// Unix seconds when the verdict was reached.
    pub observed_at: i64,
    /// Short human-readable account of what was checked. A summary of the *check*,
    /// never the content.
    #[serde(default)]
    pub summary: String,
    pub origin: EvidenceOrigin,
}

impl TaskOutcomeEvidence {
    /// Build evidence for one attempt, inheriting the verifier's baseline confidence.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        evidence_id: impl Into<String>,
        request_id: impl Into<String>,
        outcome_id: impl Into<String>,
        attempt_id: Option<String>,
        served_model: Option<String>,
        served_provider: Option<String>,
        task_type: TaskType,
        verifier: VerifierKind,
        status: TaskStatus,
        observed_at: i64,
        origin: EvidenceOrigin,
    ) -> Self {
        Self {
            evidence_id: evidence_id.into(),
            request_id: request_id.into(),
            outcome_id: outcome_id.into(),
            attempt_id,
            served_model,
            served_provider,
            task_type,
            verifier,
            status,
            confidence: if status.is_observed() {
                verifier.base_confidence()
            } else {
                0.0
            },
            observed_at,
            summary: String::new(),
            origin,
        }
    }

    /// Evidence recording that no verdict was reached.
    ///
    /// Deliberately constructible rather than represented by absence: "we looked and
    /// could not tell" and "this row predates evidence collection" are different, and
    /// only the first should count against label coverage.
    pub fn unknown(
        evidence_id: impl Into<String>,
        request_id: impl Into<String>,
        outcome_id: impl Into<String>,
        task_type: TaskType,
        verifier: VerifierKind,
        observed_at: i64,
        origin: EvidenceOrigin,
    ) -> Self {
        Self::new(
            evidence_id,
            request_id,
            outcome_id,
            None,
            None,
            None,
            task_type,
            verifier,
            TaskStatus::Unknown,
            observed_at,
            origin,
        )
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Whether this evidence may decide a quality gate by itself.
    pub fn can_settle_alone(&self) -> bool {
        self.status.is_observed() && self.verifier.can_settle_alone()
    }

    /// Whether this verdict is attributable to a specific attempted candidate.
    ///
    /// Evidence without an attempt id cannot be assigned to a candidate on a fallback
    /// chain, so it must not be used in per-candidate quality statistics.
    pub fn is_attempt_attributable(&self) -> bool {
        self.attempt_id.is_some() && self.served_model.is_some()
    }

    /// Refresh confidence for a verdict whose verifier turned out to be wrong.
    ///
    /// Used when a later check contradicts an earlier one: a model judge that called a
    /// failing build correct should not keep its baseline confidence.
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = if status_observed(self.status) {
            confidence.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self
    }
}

/// `Unknown` can never carry confidence, whatever the caller asked for.
fn status_observed(status: TaskStatus) -> bool {
    status.is_observed()
}

// ---------------------------------------------------------------------------
// Coverage — denominators made explicit
// ---------------------------------------------------------------------------

/// Label coverage for a body of task evidence.
///
/// Every quality rate in this module is reported against this, because a rate whose
/// denominator is hidden is the single easiest way to overstate a model: "90% success"
/// over 10 verified of 10,000 attempts is not the same claim as 90% over 10,000.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceCoverage {
    /// Every evidence record considered, including `Unknown` ones.
    pub total: usize,
    /// Records carrying a real verdict.
    pub observed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub partial: usize,
    /// Records naming the attempt they judge.
    pub attempt_attributable: usize,
    /// Records from a mechanical verifier.
    pub mechanical: usize,
    /// Records from a model judge, held apart so its reliability can be assessed.
    pub model_judged: usize,
}

impl EvidenceCoverage {
    /// Fold one record in.
    pub fn observe(&mut self, evidence: &TaskOutcomeEvidence) {
        self.total += 1;
        match evidence.status {
            TaskStatus::Succeeded => {
                self.observed += 1;
                self.succeeded += 1;
            }
            TaskStatus::Failed => {
                self.observed += 1;
                self.failed += 1;
            }
            TaskStatus::Partial => {
                self.observed += 1;
                self.partial += 1;
            }
            // Deliberately counted in `total` and nowhere else.
            TaskStatus::Unknown => {}
        }
        if evidence.is_attempt_attributable() {
            self.attempt_attributable += 1;
        }
        if evidence.verifier.is_mechanical() {
            self.mechanical += 1;
        }
        if matches!(evidence.verifier, VerifierKind::ModelJudge) {
            self.model_judged += 1;
        }
    }

    pub fn of(evidence: &[TaskOutcomeEvidence]) -> Self {
        let mut coverage = Self::default();
        for item in evidence {
            coverage.observe(item);
        }
        coverage
    }

    /// Share of records that carry a real verdict.
    pub fn observed_rate(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.observed as f64 / self.total as f64
    }

    /// Success rate **over observed verdicts only**, with the denominator returned
    /// alongside it.
    ///
    /// The pair is the honest unit. Returning the rate alone invites a reader to
    /// assume the denominator was every request.
    pub fn success_rate(&self) -> (f64, usize) {
        if self.observed == 0 {
            return (0.0, 0);
        }
        (
            (self.succeeded as f64 + self.partial as f64 * 0.5) / self.observed as f64,
            self.observed,
        )
    }

    /// Whether there is enough mechanical evidence to support a quality claim.
    ///
    /// Without this a report can present a number derived entirely from weak signals
    /// and model judges with the same typography as a test-suite result.
    pub fn has_settleable_evidence(&self) -> bool {
        self.mechanical > 0 || self.observed > self.model_judged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(verifier: VerifierKind, status: TaskStatus) -> TaskOutcomeEvidence {
        TaskOutcomeEvidence::new(
            "ev-1",
            "req-1",
            "out-1",
            Some("att-1".into()),
            Some("m-1".into()),
            Some("p-1".into()),
            TaskType::Code,
            verifier,
            status,
            1_700_000_000,
            EvidenceOrigin::RuntimeVerifier,
        )
    }

    #[test]
    fn api_success_and_task_failure_are_independent_records() {
        // The whole reason this module exists: a clean 200 with a broken build.
        let request_ok = true;
        let task = evidence(VerifierKind::Compilation, TaskStatus::Failed);
        assert!(request_ok);
        assert_eq!(task.status, TaskStatus::Failed);
        assert_ne!(task.status.observed_fraction(), Some(1.0));
    }

    #[test]
    fn unknown_is_neither_success_nor_failure() {
        let unknown = evidence(VerifierKind::BehaviouralSignal, TaskStatus::Unknown);
        assert!(!unknown.status.is_observed());
        assert_eq!(unknown.status.observed_fraction(), None);
        // An unknown verdict must not carry confidence, or it would be weighted in.
        assert_eq!(unknown.confidence, 0.0);
    }

    #[test]
    fn an_unknown_cannot_gain_confidence() {
        let boosted = evidence(VerifierKind::ModelJudge, TaskStatus::Unknown).with_confidence(1.0);
        assert_eq!(boosted.confidence, 0.0);
    }

    #[test]
    fn evidence_without_an_attempt_is_not_candidate_attributable() {
        let mut unattributed = evidence(VerifierKind::Compilation, TaskStatus::Succeeded);
        unattributed.attempt_id = None;
        assert!(!unattributed.is_attempt_attributable());
        assert!(
            evidence(VerifierKind::Compilation, TaskStatus::Succeeded).is_attempt_attributable()
        );
    }

    #[test]
    fn weak_signals_and_model_judges_cannot_settle_a_gate_alone() {
        assert!(evidence(VerifierKind::Compilation, TaskStatus::Succeeded).can_settle_alone());
        assert!(evidence(VerifierKind::ExplicitRating, TaskStatus::Succeeded).can_settle_alone());
        assert!(!evidence(VerifierKind::BehaviouralSignal, TaskStatus::Failed).can_settle_alone());
        assert!(!evidence(VerifierKind::ModelJudge, TaskStatus::Failed).can_settle_alone());
    }

    #[test]
    fn coverage_reports_its_denominator_rather_than_hiding_unknowns() {
        let body = vec![
            evidence(VerifierKind::Compilation, TaskStatus::Succeeded),
            evidence(VerifierKind::Compilation, TaskStatus::Failed),
            evidence(VerifierKind::BehaviouralSignal, TaskStatus::Unknown),
            evidence(VerifierKind::BehaviouralSignal, TaskStatus::Unknown),
        ];
        let coverage = EvidenceCoverage::of(&body);
        assert_eq!(coverage.total, 4);
        assert_eq!(coverage.observed, 2);
        assert_eq!(coverage.observed_rate(), 0.5);
        let (rate, denominator) = coverage.success_rate();
        assert_eq!(rate, 0.5);
        // The rate is over 2 observed, not over 4 total.
        assert_eq!(denominator, 2);
    }

    #[test]
    fn coverage_survives_an_empty_body() {
        let coverage = EvidenceCoverage::of(&[]);
        assert_eq!(coverage.observed_rate(), 0.0);
        assert_eq!(coverage.success_rate(), (0.0, 0));
        assert!(!coverage.has_settleable_evidence());
    }

    #[test]
    fn coverage_separates_model_judged_records_for_reliability_checks() {
        let body = vec![
            evidence(VerifierKind::Compilation, TaskStatus::Succeeded),
            evidence(VerifierKind::ModelJudge, TaskStatus::Failed),
            evidence(VerifierKind::ModelJudge, TaskStatus::Failed),
        ];
        let coverage = EvidenceCoverage::of(&body);
        assert_eq!(coverage.mechanical, 1);
        assert_eq!(coverage.model_judged, 2);
        assert!(coverage.has_settleable_evidence());
    }

    #[test]
    fn confidence_ranks_mechanical_above_weak_signals() {
        assert!(
            VerifierKind::Compilation.base_confidence()
                > VerifierKind::BehaviouralSignal.base_confidence()
        );
        assert!(VerifierKind::BehaviouralSignal.is_weak());
    }

    #[test]
    fn evidence_round_trips_through_json() {
        let item = evidence(VerifierKind::SchemaValidation, TaskStatus::Partial)
            .with_summary("required field missing");
        let json = serde_json::to_string(&item).expect("serialize");
        let back: TaskOutcomeEvidence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(item, back);
    }
}
