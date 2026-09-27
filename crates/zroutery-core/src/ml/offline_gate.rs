//! The offline release gate: the evidence that decides whether a trained model
//! may be **considered** at all.
//!
//! This is not an activation, a shadow window, or a statistical test. It is the
//! question that has to be answered *before* any of those are worth doing: is
//! this artifact the one that produced the recorded decisions, does replaying
//! those decisions from their retained inputs reproduce them exactly, does the
//! evidence that would justify a look exist at all, and is it intact? A model
//! that cannot answer "yes" to all four is not a model to be argued about.
//!
//! # The one rule everything else serves
//!
//! **Replay equivalence is bit-exact, and a near miss is a refusal.**
//!
//! The replay re-derives the decision through
//! [`ShadowEngine::evaluate_with`](crate::ml::shadow::ShadowEngine::evaluate_with)
//! — the *same* accepted path the original decision took — and compares every
//! component against the recorded decision. Floats compare through
//! [`f64::to_bits`], never through `==`, never through an epsilon, and never
//! after rounding. There is no tolerance anywhere in this module, and there is
//! no "close enough" path to fall into: the first non-identical component ends
//! the comparison and is carried out as
//! [`OfflineGateError::ReplayDivergence`].
//!
//! Reusing the original's code path is what makes the claim worth anything. If
//! the gate carried its own re-implementation of the decision, a divergence
//! could only ever mean "the two implementations differ", which is a fact about
//! the gate. Running the accepted path means a divergence can only mean the
//! retained input or the commit is not the one the original used — a fact about
//! the evidence.
//!
//! # The replay is driven by the retained input, and that is shown, not asserted
//!
//! This module reads candidate features from exactly one place:
//! [`ShadowDecision::input`], the immutable decision-time snapshot. It does not
//! import `extract_features`, an `ObservationStore` or a `StatsStore`, so
//! re-deriving a feature vector is not a choice this module declines to make —
//! it is not reachable from here.
//!
//! Structure alone is still a claim about the code, so the gate also *measures*
//! it: [`RetentionAblation`] perturbs retained feature positions one at a time
//! and replays. A position whose perturbation changes a prediction, a utility,
//! the selection, the action, the reason or the ranking is **load-bearing**;
//! the retained bytes reach the decision arithmetic. A position whose
//! perturbation changes nothing is reported as inert, not hidden. The gate
//! requires at least one load-bearing position before it will say the retention
//! is what made the equivalence possible, because "the replay reads the retained
//! input" is a claim about a function, and a function that ignored its input
//! would satisfy every other gate here.
//!
//! Drift in the retained input is caught separately and earlier: the replay
//! recomputes `decision_input_checksum`, and a retained input whose checksum
//! differs from the recorded one did not produce the recorded decision
//! ([`OfflineGateError::RetainedInputDrifted`]).
//!
//! # Floats: what this workspace's JSON does, and what this gate does about it
//!
//! `serde_json` is used here **without** its `float_roundtrip` feature, so its
//! float parsing is a multiply/divide-by-a-power-of-ten fast path rather than a
//! correctly-rounded parse. A canonical sample carries 32 `f32` features plus
//! several `f64` targets (`Targets::{latency_ms, ttft_ms, cost}`, the per
//! attempt `latency_ms`/`ttft_ms`, and the cost facts), and the `f64` targets
//! can come back **one ULP different** after a round trip. The parent measured
//! this and recorded it as E-089 against the journal node.
//!
//! That hazard lands in three separate places here, and each gets its own
//! answer rather than a single blanket tolerance:
//!
//! 1. **The model artifact.** A checkpoint cannot survive plain JSON, because
//!    the accepted verification hashes `f64::to_bits`. 7E-2F already solved
//!    the transport with a bit-exact wire form. This gate does **not** reach
//!    into that module: an accepted boundary test requires that nothing else in
//!    `ml` may name it, which is the right constraint, because the installer
//!    must stay unreachable from the ML stack. What the gate does instead is
//!    verify the commit with the accepted check and *measure*, on every run,
//!    whether plain JSON would have carried it ([`CommitTransport`]). A trained
//!    commit is expected to fail that measurement, and the report says so
//!    rather than leaving the caller to discover it as an unexplainable
//!    verification failure later. The gate does not enable `float_roundtrip` in
//!    the workspace manifest either: 7E-2E and 7E-2F both recorded that as a
//!    global change outside any single node's ownership.
//!
//! 2. **The compared decision.** No journal-carried `f64` is ever a compared
//!    component of replay equivalence. The compared floats are model *outputs*,
//!    recomputed from the retained `f32` feature vector through the bit-exact
//!    commit, and the `f32` round trip is a single correctly-rounded
//!    `f64`->`f32` cast from a shortest-form decimal rather than the
//!    multiple-rounding path that bites arbitrary `f64`. This exclusion is the
//!    reason the gate stays decidable at all under E-089, and it is stated here
//!    rather than left as an accident of which fields happen to be compared.
//!
//! 3. **The measurements that do consume those targets.** The holdout and
//!    calibration path reads exactly those drifted `f64`s as regression and
//!    loss targets, so there the drift is genuine measurement error. The gate
//!    does not absorb it. [`JournalFloatFidelity`] re-derives every `f64`
//!    field's bits through the same `to_vec`/`from_slice` path the journal
//!    uses, on every run, and reports how many fields moved and by how many
//!    ULPs. It is a constituent of the release verdict, and a non-exact
//!    fidelity is a refusal. Its honest limit is written on the constant
//!    [`JOURNAL_FLOAT_NOTE`]: it measures the *transport*, so a value that
//!    arrived already drifted is invisible to it and can only be prevented, not
//!    detected, by sourcing it bit-exactly.
//!
//! A consequence worth stating plainly: if a caller persists a
//! [`ShadowDecision`] as plain JSON and hands the gate the re-parsed record,
//! the gate **must refuse** on the first differing component. That is the
//! correct outcome. `JournalFloatFidelity` is how the caller finds out why,
//! rather than discovering an unexplained mismatch.
//!
//! # What this gate consumes rather than reimplements
//!
//! * Calibration measurement and its `passes` rule are 7E-2D's. The gate runs
//!   [`run_calibration`] over a holdout proven disjoint from the fit set and
//!   carries [`CalibrationVerdict::from_measurements`] verbatim. A degenerate
//!   holdout refuses through 7E-2D's own
//!   [`DegeneracyReason`]; the gate does not classify degeneracy itself.
//! * Drift, PSI and the priced marginal route are 7E-2D's and are read, never
//!   reinterpreted.
//! * The failure vocabulary and the impact table are `FailureClass::impact` and
//!   `FailureFacts::classified`. The gate reads them. It never re-derives a
//!   failure class from a message or a status code.
//! * The commit, its checkpoint and its checkpoint verification are
//!   `ModelCommit`'s. The gate never re-implements content hashing.
//!
//! # What this gate deliberately does not answer
//!
//! [`RELEASE_SCOPE`] says it in one line, and it is repeated in the release
//! verdict's own type because a verdict is exactly the place a reader is most
//! likely to over-read it:
//!
//! * **Statistical support.** No confidence interval, no power analysis, no
//!   significance test, no minimum detectable effect, no multiple-comparison
//!   control, no bootstrap. [`EvidenceFloors`] is a *presence* floor: it
//!   refuses when there is too little evidence to make the claim at all. It is
//!   not a test of whether the claim is supported. Attempt-level attribution
//!   and the statistical release methodology belong to node 7D, sequenced after
//!   this one (E-092).
//! * **Reachability.** Nothing here says a model can be served, will be
//!   reached, or is safe to activate. A `Considerable` verdict means the
//!   evidence survived being checked. 7F owns real-traffic shadowing.
//! * **Any of the forbidden operations.** This module starts no thread, runs no
//!   timer and installs nothing. The [`ShadowEngine`] it builds is local to the
//!   call, is never registered with a runtime, and is never `swap`ped or
//!   `try_train`ed.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::failure::{FailureClass, FailureImpact};
use crate::feedback::DataOrigin;
use crate::ml::calibration::{
    project_cohorts, run_calibration, CalibrationConfig, CalibrationError, CalibrationOutcome,
    CalibrationReport, CalibrationVerdict, DegeneracyReason, PartitionKind,
};
use crate::ml::coordinator::{CoordinatorConfig, RoutingAction};
use crate::ml::dataset::{canonical_samples_from_decision_time, OutcomeTrainingSample, SampleScope};
use crate::ml::decision_engine::DecisionEngine;
use crate::ml::evaluation::{
    f32_identical, f64_identical, find_nonfinite_f64, ulp_distance, Divergence, Exactness,
    NonFiniteComponent,
};
use crate::ml::features::FEATURE_DIMENSION;
use crate::ml::model_identity::{CommitId, ModelCheckpoint, ModelCommit, ModelEnsemble, ReplayError};
use crate::ml::reward::RewardPolicy;
use crate::ml::shadow::{
    ModelEnsemblePredictor, ShadowCandidate, ShadowDecision, ShadowInput, ShadowEngine,
};
use crate::ml::statistics::{
    measure_release_evidence, StatisticalConfig, StatisticalInput, StatisticalRefusal,
    StatisticalRelease,
};
use crate::outcome::{FinalStatus, Outcome};

/// What a release verdict from this gate is, and what it is not.
///
/// Carried on the report and repeated in the verdict's own documentation,
/// because a verdict is the one artifact a reader is most likely to over-read.
pub const RELEASE_SCOPE: &str = "whether the recorded evidence for this model is intact, \
replayable and sufficient to be worth looking at; it is NOT a statistical claim, NOT a claim \
that the model is better than the baseline, and NOT a claim that the model can be served";

/// What 7D's statistical constituent of that verdict is, and what it is not.
///
/// Carried beside [`RELEASE_SCOPE`] rather than folded into it. 7E-3's string
/// describes the replay and integrity constituents, which are unchanged; this
/// one describes the statistical constituent added alongside them. A reader
/// holding both can see which part of the verdict is evidence of integrity and
/// which part is evidence of effect, and neither string over-reads the other.
pub use crate::ml::statistics::STATISTICAL_SCOPE;

/// The honest limit of [`JournalFloatFidelity`].
pub const JOURNAL_FLOAT_NOTE: &str = "measured by re-serialising and re-parsing the canonical \
samples this run actually consumed, on every run; this measures the TRANSPORT, so a value that \
arrived already drifted is invisible to it and must be prevented at the source rather than \
detected here";

// ---------------------------------------------------------------------------
// OfflineGateError
// ---------------------------------------------------------------------------

/// Every way the offline gate refuses.
///
/// A refusal is a typed value carrying the numbers or the names that caused it.
/// There is no boolean returned alongside a partially-successful run, no
/// "close enough" variant, and no path that reports a measurement it could not
/// support. The variants are grouped in the order the gate reaches them, which
/// is also the order of the claims.
///
/// Payloads are boxed where a variant would otherwise dominate the enum's
/// size: a large `Err` is expensive to construct and expensive to move, and
/// this type is returned from every fallible entry point here.
#[derive(Debug, Clone, thiserror::Error)]
pub enum OfflineGateError {
    // -- nothing to decide --

    /// No recorded decision was supplied. A release verdict computed from no
    /// evidence is not a verdict.
    #[error("no recorded decision to gate; a release verdict computed from nothing is not a verdict")]
    NothingToGate,

    // -- the artifact --

    /// The commit is absent, or absent in a form that cannot name itself.
    #[error("the model commit is missing or unusable: {reason}")]
    MissingCommit { reason: String },

    /// The commit failed its own identity or checkpoint verification, or did
    /// not survive the bit-exact wire form this workspace needs.
    #[error("model commit {commit_id} could not be verified: {reason}")]
    UnverifiableCommit { commit_id: String, reason: String },

    /// The retained root-to-current lineage did not verify: incomplete,
    /// out of order, duplicated, cyclic, or ending somewhere other than the
    /// held commit.
    #[error("the retained lineage for commit {commit_id} does not verify: {reason}")]
    LineageRejected { commit_id: String, reason: String },

    // -- the retained decision-time input --

    /// The recorded decision carries no retained input to replay from.
    #[error("decision {decision_id} has no retained decision-time input: {reason}")]
    RetainedInputAbsent { decision_id: String, reason: String },

    /// The retained input is not the one the recorded decision was made from.
    /// Its recomputed input checksum disagrees with the recorded one, so the
    /// replay would be re-deriving something other than the original decision.
    #[error(
        "the retained input of decision {decision_id} is not the input the decision was made \
         from: recorded input checksum {recorded} but the retained input re-derives to {replayed}"
    )]
    RetainedInputDrifted {
        decision_id: String,
        recorded: u64,
        replayed: u64,
    },

    /// The retained input does not carry a usable feature vector for every
    /// candidate the Outcome touched. 7E-2D's retention-completeness refusal,
    /// surfaced rather than unwrapped.
    #[error("the retained input of decision {decision_id} is incomplete: {reason}")]
    RetainedInputIncomplete { decision_id: String, reason: String },

    // -- the replay --

    /// The replay produced no decision at all. The accepted seam answers `None`
    /// both when it is disabled and when it contained a fault, so the gate
    /// cannot tell those apart and will not guess which happened.
    #[error("replaying decision {decision_id} produced no decision at all")]
    ReplayProducedNoDecision { decision_id: String },

    /// The accepted seam refused the replay with its own reason.
    #[error("replaying decision {decision_id} was refused: {reason}")]
    ReplayRejected { decision_id: String, reason: String },

    /// **The headline refusal.** The replay reproduced the decision up to a
    /// component and no further. Carries the first differing component in the
    /// documented comparison order, with the recorded and replayed values in
    /// the lossless `{:?}` spelling. There is no tolerance on this variant and
    /// no amount of it would produce a different outcome.
    #[error("decision {decision_id} does not replay: {divergence}")]
    ReplayDivergence {
        decision_id: String,
        divergence: Box<Divergence>,
    },

    /// A value on the replay path is `NaN` or infinite, so no equality verdict
    /// about it would mean anything. Checked *before* any comparison: under
    /// `to_bits` two `NaN`s would otherwise compare equal and be scored a
    /// match, and under `==` they would compare unequal and hide a broken model.
    #[error("decision {decision_id} carries {component}")]
    NonFiniteMeasurement {
        decision_id: String,
        component: Box<NonFiniteComponent>,
    },

    // -- outcome authority --

    /// The replayed decision cannot be reconciled with the canonical Outcome:
    /// either the replay selected a candidate the request never attempted, or
    /// the recorded terminal state contradicts the replay.
    #[error("the replayed decision for {decision_id} disagrees with the canonical Outcome: {detail}")]
    TerminalStateDisagreement { decision_id: String, detail: String },

    /// The Outcome records no identity that served. The evaluation must key on
    /// the identity that actually served, and planned identity is not a
    /// substitute for it, so a decision with no served identity cannot
    /// contribute to a served-keyed evaluation and the gate says so rather
    /// than filling the gap.
    #[error(
        "Outcome {outcome_id} for decision {decision_id} records no served identity; planned \
         identity is not a substitute for the identity that served"
    )]
    ServedIdentityAbsent {
        decision_id: String,
        outcome_id: String,
    },

    // -- the holdout --

    /// The holdout shares a sample or an Outcome with the set the model was
    /// fitted on. A holdout that overlaps the fit set measures the fit.
    #[error("holdout {holdout_index} shares {kind} '{sample_id}' with the model's fit set")]
    HoldoutOverlap {
        kind: &'static str,
        sample_id: String,
        holdout_index: usize,
    },

    /// The holdout cannot support a calibration claim. 7E-2D classifies this;
    /// the gate carries the classification without re-deriving it.
    #[error("the holdout is degenerate: 7E-2D reports {reason} on the {partition} partition ({detail})")]
    DegenerateHoldout {
        partition: PartitionKind,
        reason: DegeneracyReason,
        detail: String,
    },

    /// There is not enough evidence present to make the claim this gate makes.
    ///
    /// This is a **presence** floor, not a significance test. Refusing here
    /// means "we cannot show this"; it never means "this is shown to be
    /// inadequate". Whether the evidence supports a release claim is node 7D's
    /// statistical methodology, not this floor's arithmetic.
    #[error("insufficient evidence to make this claim: {context} is {observed}, at least {required} is required")]
    InsufficientEvidence {
        context: &'static str,
        observed: usize,
        required: usize,
    },

    /// 7E-2D refused the calibration run for a reason of its own. Held whole,
    /// with its own message, so no refusal is flattened into a generic error.
    #[error("7E-2D refused the calibration run: {error}")]
    CalibrationRefused { error: Box<CalibrationError> },
}

impl OfflineGateError {
    /// The commit this refusal names, when the refusal is about a commit.
    ///
    /// Lets a caller attribute a refusal to an artifact without matching on
    /// every variant, and without this gate having to invent an id for the
    /// refusals that are not about one.
    pub fn commit_id(&self) -> Option<String> {
        match self {
            Self::UnverifiableCommit { commit_id, .. }
            | Self::LineageRejected { commit_id, .. } => Some(commit_id.clone()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// One recorded decision paired with the canonical Outcome that judged it.
///
/// The pairing is not a convenience. A decision without its Outcome cannot be
/// judged against anything, and an Outcome without its decision has no
/// counterfactual to replay; the gate refuses both rather than measuring one
/// and ignoring the other.
#[derive(Debug, Clone)]
pub struct RecordedDecision {
    /// The decision-time record, including its retained input.
    pub decision: ShadowDecision,
    /// The canonical Outcome for the same request.
    pub outcome: Outcome,
}

impl RecordedDecision {
    /// The retained decision-time input, borrowed from the record.
    pub fn input(&self) -> &ShadowInput {
        self.decision.input()
    }

    /// The identity that actually served, from the Outcome — never the planned
    /// identity, and never inferred from the legacy `initial_*`/`final_*`
    /// compatibility projections.
    pub fn served_identity(&self) -> Option<(&str, &str)> {
        match (
            self.outcome.served_model.as_deref(),
            self.outcome.served_provider.as_deref(),
        ) {
            (Some(model), Some(provider)) => Some((provider, model)),
            _ => None,
        }
    }
}

/// The presence floors this gate enforces before it will measure anything.
///
/// Deliberately only counts. Deciding whether an amount of evidence *supports*
/// a release claim is a statistical question, and node 7D owns that
/// methodology; a floor that pretended to answer it would be a significance
/// test wearing a different hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EvidenceFloors {
    /// Fewest recorded decisions the replay-equivalence claim may be made over.
    pub min_replayed_decisions: usize,
    /// Fewest decisions the holdout must hold.
    pub min_holdout_decisions: usize,
}

impl Default for EvidenceFloors {
    fn default() -> Self {
        Self {
            min_replayed_decisions: 1,
            min_holdout_decisions: 12,
        }
    }
}

/// Everything one gate run is configured with.
#[derive(Debug, Clone)]
pub struct GateConfig {
    /// 7E-2D's configuration, consumed and never reinterpreted.
    pub calibration: CalibrationConfig,
    /// The presence floors.
    pub floors: EvidenceFloors,
    /// How many retained feature positions each decision's retention ablation
    /// perturbs. Zero disables the ablation, and a run with it disabled cannot
    /// claim the retention is load-bearing.
    pub retention_probes: usize,
    /// The decision engine's configuration at decision time.
    ///
    /// The engine's utility weights reach `UtilityBreakdown`, and the
    /// breakdown reaches the action and the reason, so a wrong claim here
    /// surfaces as a replay divergence rather than as a silent pass. It is a
    /// caller's claim, not something the record carries.
    pub engine: CoordinatorConfig,
    /// The reward policy at decision time, for the same reason.
    pub reward_policy: RewardPolicy,
    /// 7D's claim specification: the level, the power and the minimum effect the
    /// statistical constituent is held to.
    ///
    /// Required and never defaulted. There is no value of
    /// [`StatisticalConfig`] that disables the gate, and
    /// [`StatisticalConfig::checked`] refuses a specification that would make
    /// the claim vacuous. A caller may demand *more* evidence — a smaller
    /// alpha, a higher power, a smaller minimum effect — and demanding more
    /// withholds the verdict more often, never less.
    pub statistics: StatisticalConfig,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            calibration: CalibrationConfig::default(),
            floors: EvidenceFloors::default(),
            retention_probes: 8,
            engine: CoordinatorConfig::default(),
            reward_policy: RewardPolicy::default(),
            statistics: StatisticalConfig::default(),
        }
    }
}

/// The complete input to one gate run.
#[derive(Debug, Clone)]
pub struct GateInput {
    /// The commit under consideration.
    ///
    /// It must have been sourced through 7E-2F's bit-exact wire form; the gate
    /// re-measures that rather than assuming it, and refuses if it does not
    /// hold.
    pub commit: ModelCommit,
    /// The complete root-to-current lineage of [`GateInput::commit`].
    pub lineage: Vec<ModelCommit>,
    /// The recorded decisions, each with its canonical Outcome.
    pub recorded: Vec<RecordedDecision>,
    /// The sample ids the model was fitted on. The holdout is proven disjoint
    /// from this set, and from its Outcome ids as well.
    pub fit_sample_ids: BTreeSet<String>,
    /// How the run is configured.
    pub config: GateConfig,
}

// ---------------------------------------------------------------------------
// Measurements
// ---------------------------------------------------------------------------

/// How this commit behaves when it is stored as ordinary JSON in this
/// workspace.
///
/// Recomputed on every run by pushing the commit through the same
/// `to_vec`/`from_slice` pair a plain store would use and comparing every
/// `f64` parameter in bits. This is a **reported** measurement, not a verdict
/// constituent, and the reason is worth stating plainly:
///
/// * a trained commit is *expected* to fail it. The accepted verification
///   hashes `f64::to_bits`, so a parameter that comes back one ULP different
///   makes the content hash disagree and the commit refuses to verify itself —
///   7E-2F recorded exactly that finding;
/// * the supported transport for such a commit is 7E-2F's bit-exact wire form,
///   which carries each parameter as 16 hex digits of its bits. This gate does
///   **not** reach for that module itself, because an accepted boundary test
///   requires that nothing else in `ml` may name it — which is the right
///   constraint, since the installer must stay unreachable from the ML stack;
/// * so the gate verifies the commit with the accepted check, reports whether
///   plain JSON would have carried it, and leaves the transport decision to
///   the node that owns the wire form.
///
/// A caller who stores the commit as plain JSON and reads it back will get a
/// commit that fails [`ModelCommit::verify`], and this measurement is where they
/// find out why before that happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitTransport {
    /// The commit's content-addressed id.
    pub commit_id: String,
    /// `f64` parameters compared across the commit's four model states.
    pub f64_parameters: usize,
    /// How many of them came back with different bits.
    pub moved_parameters: usize,
    /// `moved_parameters == 0`, derived here rather than supplied.
    pub plain_json_exact: bool,
}

/// The float fidelity of the canonical samples this run consumed, across this
/// workspace's JSON read path.
///
/// Recomputed every run by pushing the samples through the same
/// `to_vec`/`from_slice` pair `ml/journal.rs` uses. See
/// [`JOURNAL_FLOAT_NOTE`] for what this does and does not see.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JournalFloatFidelity {
    /// Samples measured.
    pub samples: usize,
    /// `f64` fields compared across all samples.
    pub f64_fields: usize,
    /// How many of those fields came back with different bits.
    pub moved_fields: usize,
    /// The largest ULP distance observed. Zero when nothing moved.
    pub max_ulp: u64,
    /// The worst offender, so a non-zero count is actionable.
    pub worst: Option<FloatDrift>,
    /// `moved_fields == 0`, derived here rather than supplied.
    pub round_trip_exact: bool,
}

/// One `f64` field that did not survive the round trip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FloatDrift {
    /// The sample the field belongs to.
    pub sample_id: String,
    /// Dotted path of the field.
    pub component: String,
    /// The value as authored.
    pub before: f64,
    /// The value as re-parsed.
    pub after: f64,
    /// How far apart they are, in representable doubles.
    pub ulp: u64,
}

impl JournalFloatFidelity {
    /// Measure the transport by pushing the samples through it.
    ///
    /// The samples are re-parsed into the accepted
    /// [`OutcomeTrainingSample`], not into a loose struct, so what is measured
    /// is the fidelity of the type the journal actually reads back.
    pub fn measure(samples: &[OutcomeTrainingSample]) -> Self {
        let mut f64_fields = 0usize;
        let mut moved_fields = 0usize;
        let mut max_ulp = 0u64;
        let mut worst: Option<FloatDrift> = None;

        for sample in samples {
            // A sample that cannot be written at all cannot be read back
            // either; there is no round trip to measure, and pretending
            // otherwise would report fidelity for a value nothing ever stored.
            let Ok(bytes) = serde_json::to_vec(sample) else {
                continue;
            };
            let Ok(reparsed) = serde_json::from_slice::<OutcomeTrainingSample>(&bytes) else {
                continue;
            };

            for (component, before, after) in float_fields(sample, &reparsed) {
                f64_fields += 1;
                if f64_identical(before, after) {
                    continue;
                }
                moved_fields += 1;
                let ulp = ulp_distance(before, after);
                if ulp > max_ulp {
                    max_ulp = ulp;
                }
                let drift = FloatDrift {
                    sample_id: sample.sample_id.clone(),
                    component,
                    before,
                    after,
                    ulp,
                };
                // The workspace MSRV predates `Option::is_none_or`, and the
                // comparison is written out rather than lifted into a closure
                // so it reads as "keep whichever drift is worse".
                let replace = match worst.as_ref() {
                    Some(current) => drift.ulp > current.ulp,
                    None => true,
                };
                if replace {
                    worst = Some(drift);
                }
            }
        }

        Self {
            samples: samples.len(),
            f64_fields,
            moved_fields,
            max_ulp,
            worst,
            round_trip_exact: moved_fields == 0,
        }
    }
}

/// Every `f64` a canonical sample carries, paired with the re-parsed value.
///
/// The set is exactly the `f64` surface of a canonical sample: the three
/// regression/classification targets, the two cost facts, and each attempt's
/// latency and time-to-first-token. The 32 `f32` feature values are
/// deliberately **not** in this list — see the module header on why the feature
/// round trip is a different question from the target round trip.
fn float_fields(
    original: &OutcomeTrainingSample,
    reparsed: &OutcomeTrainingSample,
) -> Vec<(String, f64, f64)> {
    let mut fields: Vec<(String, f64, f64)> = Vec::with_capacity(6 + original.attempts.len() * 2);
    let mut push = |name: String, before: Option<f64>, after: Option<f64>| {
        // An optional field is compared only when the transport kept it. A
        // field that appeared or vanished is a schema change, which the
        // accepted sample validator owns, not a ULP story.
        if let (Some(before), Some(after)) = (before, after) {
            fields.push((name, before, after));
        }
    };
    push(
        "targets.latency_ms".to_string(),
        original.targets.latency_ms,
        reparsed.targets.latency_ms,
    );
    push(
        "targets.ttft_ms".to_string(),
        original.targets.ttft_ms,
        reparsed.targets.ttft_ms,
    );
    push(
        "targets.cost".to_string(),
        original.targets.cost,
        reparsed.targets.cost,
    );
    push(
        "estimated_cost".to_string(),
        original.estimated_cost,
        reparsed.estimated_cost,
    );
    push(
        "actual_cost".to_string(),
        original.actual_cost,
        reparsed.actual_cost,
    );
    for (index, (before, after)) in original
        .attempts
        .iter()
        .zip(reparsed.attempts.iter())
        .enumerate()
    {
        fields.push((
            format!("attempts[{index}].latency_ms"),
            before.latency_ms,
            after.latency_ms,
        ));
        if let (Some(before_ttft), Some(after_ttft)) = (before.ttft_ms, after.ttft_ms) {
            fields.push((
                format!("attempts[{index}].ttft_ms"),
                before_ttft,
                after_ttft,
            ));
        }
    }
    fields
}

/// How the replayed decision's terminal state sits against the recorded one.
///
/// A [`TerminalAgreement`] is only ever produced for a replay that already
/// passed equivalence; the two situations that *cannot* be reconciled are
/// refusals ([`OfflineGateError::TerminalStateDisagreement`] and
/// [`OfflineGateError::ServedIdentityAbsent`]), not variants here, so no value
/// of this enum means "we could not tell".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgreement {
    /// The replay selected the identity that served.
    SelectionServed,
    /// The replay selected a candidate the request did attempt, and a
    /// different identity served after failover. The counterfactual and the
    /// record disagree about which candidate won, which is the entire point of
    /// a counterfactual.
    SelectionSupersededByFailover,
    /// The request failed, and the replay selected the candidate the request
    /// ended on.
    SelectionEndedTheFailure,
    /// The request failed, and the replay selected a candidate the request did
    /// attempt before the one it ended on.
    SelectionAttemptedNotFinal,
}

/// The failure classification of a replayed decision, read rather than derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureAuthority {
    /// The accepted class, read from the Outcome's own terminal facts.
    pub class: Option<FailureClass>,
    /// The accepted impact, read from the class's own table.
    pub impact: Option<FailureImpact>,
}

impl Serialize for FailureAuthority {
    /// The class and the accepted table's five bits.
    ///
    /// Hand-written because `FailureImpact` is a routing type and deliberately
    /// carries no wire form of its own — it is read out of a table, not
    /// exchanged. Spelling its bits here means the report records what the
    /// table said without this gate becoming a second place the table is
    /// written down, and without inventing a serialization for a type this node
    /// does not own.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("FailureAuthority", 7)?;
        state.serialize_field("class", &self.class)?;
        let impact = self.impact;
        state.serialize_field("affects_observation", &impact.map(|i| i.affects_observation))?;
        state.serialize_field("affects_circuit", &impact.map(|i| i.affects_circuit))?;
        state.serialize_field("retryable", &impact.map(|i| i.retryable))?;
        state.serialize_field("fallbackable", &impact.map(|i| i.fallbackable))?;
        state.serialize_field("provider_fault", &impact.map(|i| i.provider_fault))?;
        state.end()
    }
}

impl FailureAuthority {
    /// Read the class and its impact from the accepted table.
    ///
    /// [`FailureFacts::classified`](crate::outcome::FailureFacts::classified)
    /// rehydrates the accepted impact for a class; it does not classify
    /// anything, and neither does this. There is no path here that inspects a
    /// failure message or an HTTP status to work out a class, because a second
    /// classification policy is exactly the thing this gate must not have.
    pub fn read_from(outcome: &Outcome) -> Self {
        match outcome.terminal_failure_facts() {
            Some(facts) => {
                let classified = facts.classified();
                Self {
                    class: Some(classified.class),
                    impact: Some(classified.impact),
                }
            }
            None => Self {
                class: None,
                impact: None,
            },
        }
    }
}

/// Whether the retained input is load-bearing for the replayed decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetentionAblation {
    /// Retained feature positions perturbed.
    pub probed: usize,
    /// How many of those changed the replayed decision before the decision
    /// checksum was even reached.
    pub load_bearing: usize,
}

impl RetentionAblation {
    /// Whether the ablation ran at all.
    pub fn is_proof(&self) -> bool {
        self.probed > 0 && self.load_bearing > 0
    }
}

/// The evidence that the retained input is the input the decision was made
/// from, and that it is load-bearing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionProof {
    /// The input checksum the record carries.
    pub recorded_input_checksum: u64,
    /// The input checksum the retained input re-derives to. Equal by
    /// construction here, because a disagreement is a refusal rather than a
    /// measurement; it is carried so the report shows the value that was
    /// checked.
    pub replayed_input_checksum: u64,
    /// Candidates retained in the decision-time input.
    pub retained_candidates: usize,
    /// How many of those the policy left eligible.
    pub eligible_candidates: usize,
    /// How many distinct candidates the canonical Outcome touched.
    pub outcome_candidates: usize,
    /// The retention-completeness result: every candidate the Outcome touched
    /// had a retained, eligible, finite, schema-matched vector. 7E-2D's
    /// function produced it, so this gate inherits its refusal vocabulary
    /// rather than inventing a weaker one.
    pub complete_for_outcome: bool,
    /// The ablation.
    pub ablation: RetentionAblation,
}

/// The identity an evaluation is keyed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServedIdentity {
    /// The model that produced a successful terminal response.
    pub model: String,
    /// Its provider.
    pub provider: String,
}

impl ServedIdentity {
    /// Read the served identity from the Outcome, or `None` when the Outcome
    /// records none.
    ///
    /// Deliberately refuses to fall back to `planned_model`/`planned_provider`,
    /// to `last_attempted_*`, and to the legacy `final_model` compatibility
    /// projection. Every one of those is a different claim from "this is what
    /// served", and an evaluation keyed on the wrong one is an evaluation of
    /// something that did not happen.
    pub fn from_outcome(outcome: &Outcome) -> Option<Self> {
        match (
            outcome.served_model.as_deref(),
            outcome.served_provider.as_deref(),
        ) {
            (Some(model), Some(provider)) => Some(Self {
                model: model.to_string(),
                provider: provider.to_string(),
            }),
            _ => None,
        }
    }
}

/// Everything one replay established about one recorded decision.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayEvidence {
    /// The recorded decision's own id.
    pub shadow_id: String,
    /// The decision the input belongs to.
    pub decision_id: String,
    /// The commit the replay ran against.
    pub model_commit: CommitId,
    /// The feature schema the replay validated against.
    pub feature_schema: u32,
    /// The replayed selection.
    pub selected: String,
    /// The replayed action. The gate produces no `Explore` action; it compares
    /// whatever was recorded.
    pub action: RoutingAction,
    /// How many scalar components the comparison walked.
    pub components_compared: usize,
    /// The retention evidence.
    pub retention: RetentionProof,
    /// How the replayed decision's terminal state sits against the Outcome's.
    pub terminal: TerminalAgreement,
    /// The Outcome's recorded terminal state.
    pub recorded_terminal: FinalStatus,
    /// The failure classification, read from the accepted table.
    pub failure: FailureAuthority,
    /// The identity the evaluation is keyed on. `None` only when the gate
    /// refused before reaching this point.
    pub served: Option<ServedIdentity>,
}

/// The holdout the calibration gate was run over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldoutSummary {
    /// Canonical request-scope samples in the holdout.
    pub samples: usize,
    /// Decisions the holdout covers.
    pub decisions: usize,
    /// Holdout decisions with an attributed Outcome.
    pub attributed: usize,
    /// Distinct served identities the holdout is keyed on.
    pub served_identities: usize,
    /// Sample ids shared with the fit set. Always zero on a run that produced a
    /// verdict; a non-zero count is a refusal, not a warning.
    pub overlap_with_fit_set: usize,
}

// ---------------------------------------------------------------------------
// ReleaseVerdict
// ---------------------------------------------------------------------------

/// Every constituent measurement the release verdict is recomputed from.
///
/// The verdict is a pure function of this struct, and the struct is a
/// measurement — there is no boolean here that a caller could set and have the
/// verdict believe. A run that cannot measure a constituent refuses instead of
/// reporting a default.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReleaseMeasurements {
    /// The commit passed the accepted identity and artifact verification. A
    /// run that produced a verdict always has this true, because a commit that
    /// fails it is a refusal; it is carried so the report shows the check that
    /// ran rather than implying it.
    pub commit_verified: bool,
    /// The retained lineage verified.
    pub lineage_verified: bool,
    /// Records in the verified lineage.
    pub lineage_len: usize,
    /// Recorded decisions replayed.
    pub decisions_replayed: usize,
    /// Replays that were bit-identical to the record. A run that produced a
    /// verdict has this equal to [`ReleaseMeasurements::decisions_replayed`];
    /// the field is separate so the report can show the arithmetic rather than
    /// a bare claim.
    pub decisions_equivalent: usize,
    /// Replays whose terminal state sat consistently with the Outcome.
    pub terminal_agreements: usize,
    /// The retention ablation.
    pub retention: RetentionAblation,
    /// Decisions the evaluation keyed on a served identity.
    pub served_identities: usize,
    /// Decisions with no served identity in the Outcome. A run that produced a
    /// verdict has this at zero, because the gate refuses such a decision; it
    /// is carried so the report can state the count rather than imply it.
    pub absent_served_identities: usize,
    /// The holdout.
    pub holdout: HoldoutSummary,
    /// 7E-2D's verdict, carried verbatim.
    pub calibration: CalibrationVerdict,
    /// The float fidelity of the holdout across this workspace's read path.
    pub float_fidelity: JournalFloatFidelity,
    /// 7D's statistical constituent, carried whole.
    ///
    /// Required, never optional, and never defaulted: there is no value of
    /// [`StatisticalRelease`] that means "not measured, carry on". A run that
    /// cannot measure the statistical claim records
    /// [`StatisticalRelease::Refused`] with a typed reason, and that refusal is
    /// a blocker. The alternative — omitting the field — would make the
    /// statistical gate something a caller could switch off by not supplying it.
    pub statistics: StatisticalRelease,
}

/// The one computed value that says whether the evidence supports considering
/// this model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseVerdict {
    /// The evidence survived being checked, and there is enough of it to be
    /// worth looking at.
    ///
    /// Read this as its name says: *considerable for review*. It is not a
    /// recommendation to activate, not a statement that the model beats the
    /// baseline, and not a statistical claim of any kind. See
    /// [`RELEASE_SCOPE`].
    Considerable,
    /// The evidence was produced and does not support considering this model.
    ///
    /// This is the honest default. A measurement that cannot be judged is
    /// reported here rather than as the other value, because "we cannot show
    /// it" and "we have shown it" are different claims and only one is
    /// supported.
    NotConsiderable,
}

impl ReleaseVerdict {
    /// The whole acceptance rule, as a pure function of the measurements.
    ///
    /// Reads the measurements and nothing else. There is no stored boolean
    /// anywhere on the report that this could disagree with, and no path that
    /// reaches `Considerable` without every constituent below having been
    /// measured.
    pub fn from_measurements(measurements: &ReleaseMeasurements) -> Self {
        if Self::blockers(measurements).is_empty() {
            Self::Considerable
        } else {
            Self::NotConsiderable
        }
    }

    /// Every constituent that withholds the verdict, in a fixed order.
    ///
    /// Exposed so a reader can see *which* measurement refused, rather than
    /// being handed a bare verdict and left to guess. An empty list is the
    /// `Considerable` case, and the equality between "empty" and
    /// "considerable" is what
    /// [`ReleaseVerdict::from_measurements`] asserts.
    pub fn blockers(measurements: &ReleaseMeasurements) -> Vec<&'static str> {
        let mut blockers = Vec::new();
        if !measurements.commit_verified {
            blockers.push("the commit did not pass the accepted verification");
        }
        if !measurements.lineage_verified {
            blockers.push("the retained lineage did not verify");
        }
        if measurements.decisions_replayed == 0 {
            blockers.push("no decision was replayed");
        }
        if measurements.decisions_equivalent != measurements.decisions_replayed {
            blockers.push("at least one replay was not bit-identical to its record");
        }
        if measurements.terminal_agreements != measurements.decisions_replayed {
            blockers.push("at least one replay disagreed with its Outcome");
        }
        if !measurements.retention.is_proof() {
            blockers.push(
                "no retained feature position was shown to be load-bearing for the replayed decision",
            );
        }
        if measurements.absent_served_identities > 0 {
            blockers.push("at least one Outcome records no served identity");
        }
        if measurements.holdout.overlap_with_fit_set > 0 {
            blockers.push("the holdout overlaps the model's fit set");
        }
        if measurements.holdout.decisions == 0 {
            blockers.push("the holdout is empty");
        }
        if !measurements.calibration.is_calibrated() {
            blockers.push("7E-2D reports the emitted vector as miscalibrated");
        }
        if !measurements.float_fidelity.round_trip_exact {
            blockers.push("canonical sample floats do not survive this workspace's JSON read path");
        }
        // -- 7D's statistical constituent, appended, never substituted --
        //
        // These are appended after every check above and they short-circuit
        // nothing: each one is evaluated on the same measurements as before and
        // is reported alongside the eleven above. A run that withholds for any
        // of the reasons above still withholds for them.
        //
        // One blocker per unmet criterion, in a fixed order, so a reader can see
        // *which* of adequacy / interval / family-wise significance failed
        // rather than being handed a single opaque reason. The numbers behind
        // each are on [`ReleaseReport::statistical_reasons`].
        blockers.extend(measurements.statistics.blockers());
        blockers
    }

    /// Every constituent that withholds the verdict, with 7D's numbers spelled
    /// out.
    ///
    /// The labels are the same list [`ReleaseVerdict::blockers`] returns, in
    /// the same order, so the two cannot disagree. This is the one to read when
    /// a statistical constituent is the reason: a label says *which* requirement
    /// failed, and a reason says by how much and against what.
    pub fn blocker_details(measurements: &ReleaseMeasurements) -> Vec<String> {
        let mut details: Vec<String> = ReleaseVerdict::blockers(measurements)
            .into_iter()
            .map(str::to_string)
            .collect();
        // The labels are already in `details`, in order; the reasons are keyed
        // by criterion and appended so neither list is a re-derivation of the
        // other.
        details.extend(measurements.statistics.reasons());
        details
    }

    /// Whether this verdict says the model may be considered.
    pub const fn is_considerable(self) -> bool {
        matches!(self, Self::Considerable)
    }
}

impl std::fmt::Display for ReleaseVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Considerable => f.write_str("considerable for review"),
            Self::NotConsiderable => f.write_str("not considerable"),
        }
    }
}

// ---------------------------------------------------------------------------
// ReleaseReport
// ---------------------------------------------------------------------------

/// The honest report of one offline gate run.
///
/// No timestamp, no duration, no run counter: two runs of the same input
/// produce two identical reports, because a gate whose own output moves is a
/// gate nobody can reason about.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseReport {
    /// The commit under consideration.
    pub model_commit: CommitId,
    /// The model lineage.
    pub model_id: String,
    /// The feature schema every comparison validated against.
    pub feature_schema: u32,
    /// The artifact's transport.
    pub transport: CommitTransport,
    /// One entry per recorded decision.
    pub replay: Vec<ReplayEvidence>,
    /// The holdout.
    pub holdout: HoldoutSummary,
    /// 7E-2D's report, carried whole.
    pub calibration: CalibrationReport,
    /// The float fidelity of the holdout.
    pub float_fidelity: JournalFloatFidelity,
    /// Every constituent measurement.
    pub measurements: ReleaseMeasurements,
    /// 7D's statistical constituent, carried whole beside the measurements it
    /// was derived from.
    pub statistics: StatisticalRelease,
    /// The recorded verdict. Recompute it with
    /// [`ReleaseReport::recomputed_verdict`] — a test asserts the two agree.
    pub verdict: ReleaseVerdict,
    /// What 7E-3's constituents say this verdict is and is not.
    pub scope: &'static str,
    /// What 7D's statistical constituent says it is and is not.
    pub statistical_scope: &'static str,
}

impl ReleaseReport {
    /// The verdict, recomputed from the stored measurements.
    pub fn recomputed_verdict(&self) -> ReleaseVerdict {
        ReleaseVerdict::from_measurements(&self.measurements)
    }

    /// Whether the evidence supports considering the model, recomputed.
    pub fn is_considerable(&self) -> bool {
        self.recomputed_verdict().is_considerable()
    }

    /// Every constituent that withholds the verdict.
    pub fn blockers(&self) -> Vec<&'static str> {
        ReleaseVerdict::blockers(&self.measurements)
    }

    /// Every constituent that withholds, with 7D's numbers spelled out.
    pub fn blocker_details(&self) -> Vec<String> {
        ReleaseVerdict::blocker_details(&self.measurements)
    }

    /// 7D's unmet statistical requirements as sentences carrying the numbers.
    ///
    /// A label says which requirement failed; this says by how much and against
    /// what. The measurement itself is on [`ReleaseReport::statistics`].
    pub fn statistical_reasons(&self) -> Vec<String> {
        self.measurements.statistics.reasons()
    }

    /// The headline, in one line.
    pub fn headline(&self) -> String {
        format!(
            "verdict={}; replay {}/{} bit-identical; terminal agreements {}/{}; retention \
             load-bearing {}/{}; served identities {}; holdout {} decisions ({} attributed, {} \
             overlap); calibration={}; commit plain-JSON transport exact={} ({} of {} commit \
             parameters moved); float round trip exact={} ({} of {} f64 fields moved, max {} \
             ulp); {}; scope: {}; statistical scope: {}",
            self.recomputed_verdict(),
            self.measurements.decisions_equivalent,
            self.measurements.decisions_replayed,
            self.measurements.terminal_agreements,
            self.measurements.decisions_replayed,
            self.measurements.retention.load_bearing,
            self.measurements.retention.probed,
            self.measurements.served_identities,
            self.measurements.holdout.decisions,
            self.measurements.holdout.attributed,
            self.measurements.holdout.overlap_with_fit_set,
            self.measurements.calibration,
            self.transport.plain_json_exact,
            self.transport.moved_parameters,
            self.transport.f64_parameters,
            self.float_fidelity.round_trip_exact,
            self.float_fidelity.moved_fields,
            self.float_fidelity.f64_fields,
            self.float_fidelity.max_ulp,
            self.statistics.headline(),
            RELEASE_SCOPE,
            self.statistical_scope,
        )
    }
}

// ---------------------------------------------------------------------------
// 7D's measurement, carried from its own module
// ---------------------------------------------------------------------------

/// Measure 7D's statistical constituent over the holdout 7E-2D already
/// reserved, consuming only 7E-2D's own outputs.
///
/// Three things are taken, none of them recomputed:
///
/// * the holdout partition, from the accepted `project_cohorts`, split at the
///   same trailing count 7E-2D's `run_calibration` documents. The K axis comes
///   from attempt-scope rows, so a partition built from request-scope rows
///   alone cannot produce a one-candidate axis here: it produces the refusal
///   7E-2D raises first;
/// * the emitted distributions, from the accepted [`CalibrationOutcome`], whose
///   per-row fingerprint is checked against the partition's, so a misaligned
///   pair is refused rather than cross-attributed;
/// * the unconditional base rate, from the accepted marginal route's
///   per-candidate table, whose counts are cross-checked against the counts this
///   walk measures.
fn measure_statistics(
    holdout_samples: &[OutcomeTrainingSample],
    ensemble: &ModelEnsemble,
    calibration: &CalibrationOutcome,
    calibration_config: &CalibrationConfig,
    config: StatisticalConfig,
) -> StatisticalRelease {
    let cohorts = match project_cohorts(
        holdout_samples,
        &ensemble.success,
        calibration_config.probability_floor,
    ) {
        Ok(cohorts) => cohorts,
        Err(error) => {
            return StatisticalRelease::Refused(StatisticalRefusal {
                code: "calibration_projection_refused",
                reason: format!("7E-2D's projection could not be repeated: {error}"),
            });
        }
    };
    // 7E-2D split at `len - holdout_cohorts` and its own report carries the
    // numbers. If the two disagree, the partition 7D measured is not the one
    // 7E-2D's verdict was about, so it is refused rather than assumed.
    let report = calibration.report();
    let split = cohorts.len().saturating_sub(calibration_config.holdout.holdout_cohorts);
    if report.holdout_cohorts != cohorts.len() - split
        || report.fit_cohorts != split
        || report.cohorts_total != cohorts.len()
    {
        return StatisticalRelease::Refused(StatisticalRefusal {
            code: "partition_disagrees_with_calibration",
            reason: format!(
                "7E-2D reported {} cohorts split {}/{} but the projection yields {} split {split}/{}",
                report.cohorts_total,
                report.fit_cohorts,
                report.holdout_cohorts,
                cohorts.len(),
                cohorts.len() - split
            ),
        });
    }
    let partition = &cohorts[split..];
    match measure_release_evidence(&StatisticalInput {
        partition,
        emitted: calibration.holdout_distributions(),
        marginal: &report.marginal_calibrated.per_candidate,
        config,
    }) {
        Ok(measured) => measured,
        Err(error) => StatisticalRelease::Refused(StatisticalRefusal::from(&error)),
    }
}

// ---------------------------------------------------------------------------
// run_offline_gate
// ---------------------------------------------------------------------------

/// Everything one offline gate run produced.
pub struct GateOutcome {
    predictor: ModelEnsemblePredictor,
    report: ReleaseReport,
}

impl std::fmt::Debug for GateOutcome {
    /// Structural, without dumping every replayed scalar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateOutcome")
            .field("model_commit", &self.report.model_commit)
            .field("verdict", &self.report.recomputed_verdict())
            .field("scope", &RELEASE_SCOPE)
            .finish()
    }
}

impl GateOutcome {
    /// The honest report.
    pub fn report(&self) -> &ReleaseReport {
        &self.report
    }

    /// The predictor the replay ran against, pinned to the verified commit.
    pub fn predictor(&self) -> &ModelEnsemblePredictor {
        &self.predictor
    }

    /// Whether the evidence supports considering the model, recomputed.
    pub fn is_considerable(&self) -> bool {
        self.report.is_considerable()
    }
}

/// Run the offline release gate.
///
/// The order of the gates is the order of the claims:
///
/// 1. something to decide, or there is no verdict;
/// 2. the artifact is present and transports bit-exactly, and its retained
///    lineage verifies — because a decision attributed to a commit nobody can
///    reproduce is not evidence about that commit;
/// 3. each recorded decision replays **bit-identically** from its retained
///    input, and the retention is shown to be load-bearing;
/// 4. each replay sits consistently with its canonical Outcome, is classified
///    through the accepted impact table, and is keyed on the identity that
///    actually served;
/// 5. the holdout is proven disjoint from the fit set and large enough to be
///    measured at all;
/// 6. the holdout's float fidelity across this workspace's read path is
///    measured;
/// 7. 7E-2D's calibration measurement runs over that holdout and its verdict
///    is carried whole;
/// 8. the release verdict is recomputed from every constituent above.
///
/// Pure with respect to routing state. Nothing is installed, no live engine is
/// touched, no thread is started, and the [`ShadowEngine`] this builds is local
/// to the call and discarded when it returns.
pub fn run_offline_gate(input: &GateInput) -> Result<GateOutcome, OfflineGateError> {
    if input.recorded.is_empty() {
        return Err(OfflineGateError::NothingToGate);
    }
    if input.commit.commit_id.as_str().is_empty() {
        return Err(OfflineGateError::MissingCommit {
            reason: "the supplied commit does not name itself".to_string(),
        });
    }

    // -- 2. the artifact --
    let transport = measure_commit_transport(&input.commit)?;
    let commit = input.commit.clone();
    let predictor = build_predictor(&commit, &input.lineage)?;

    // -- 3/4. per decision --
    let engine = ShadowEngine::new(
        DecisionEngine::new(input.config.engine.clone(), input.config.reward_policy.clone()),
        true,
    );
    let mut replay = Vec::with_capacity(input.recorded.len());
    for recorded in &input.recorded {
        replay.push(replay_one(
            &engine,
            &predictor,
            recorded,
            &input.config,
        )?);
    }

    // -- 5. the holdout --
    let holdout_samples = build_holdout(&input.recorded, &replay, &input.fit_sample_ids)?;
    if holdout_samples.len() < input.config.floors.min_holdout_decisions {
        return Err(OfflineGateError::InsufficientEvidence {
            context: "the holdout sample count",
            observed: holdout_samples.len(),
            required: input.config.floors.min_holdout_decisions,
        });
    }
    if input.recorded.len() < input.config.floors.min_replayed_decisions {
        return Err(OfflineGateError::InsufficientEvidence {
            context: "the replayed decision count",
            observed: input.recorded.len(),
            required: input.config.floors.min_replayed_decisions,
        });
    }

    let holdout = summarise_holdout(&input.recorded, &replay, holdout_samples.len());
    // Measured over exactly the rows the calibration gate is about to consume,
    // so the fidelity number describes the measurements rather than a parallel
    // set that happens to look similar.
    let float_fidelity = JournalFloatFidelity::measure(&holdout_samples);

    // -- 6/7. 7E-2D's calibration gate, consumed whole --
    let ensemble = ModelEnsemble::load_all(&commit.checkpoint).map_err(|error| {
        OfflineGateError::UnverifiableCommit {
            commit_id: commit.commit_id.as_str().to_string(),
            reason: format!("the verified checkpoint did not load: {error}"),
        }
    })?;
    let calibration = run_calibration(
        &holdout_samples,
        &ensemble.success,
        &input.config.calibration,
    )
    .map_err(map_calibration_refusal)?;
    let calibration_verdict = calibration.report().recomputed_verdict();

    // -- 7D's statistical constituent, measured over the same partition --
    //
    // Everything here is 7E-2D's: the cohorts come from the accepted
    // `project_cohorts`, the emitted distributions come from the accepted
    // `CalibrationOutcome`, the unconditional base rate is the accepted
    // marginal route's per-candidate table, and the partition is the same
    // trailing holdout 7E-2D already reserved, recomputed by the same
    // arithmetic it documents. Nothing about the measurement is re-derived here.
    //
    // A refusal is recorded rather than propagated. This gate has to keep
    // producing its report when the statistical claim cannot be measured — a
    // thin holdout is a finding, not a crash — and recording the refusal is
    // what makes it a blocker instead of a silence. The refusal is carried
    // whole, with 7E-2D's own error nested inside it if that is what happened,
    // so nothing is flattened into a generic error.
    let statistics = measure_statistics(
        &holdout_samples,
        &ensemble,
        &calibration,
        &input.config.calibration,
        input.config.statistics,
    );

    let measurements = ReleaseMeasurements {
        commit_verified: true,
        lineage_verified: true,
        lineage_len: predictor.lineage().len(),
        decisions_replayed: replay.len(),
        decisions_equivalent: replay.len(),
        terminal_agreements: replay.len(),
        retention: RetentionAblation {
            probed: replay
                .iter()
                .map(|evidence| evidence.retention.ablation.probed)
                .sum(),
            load_bearing: replay
                .iter()
                .map(|evidence| evidence.retention.ablation.load_bearing)
                .sum(),
        },
        served_identities: replay
            .iter()
            .filter(|evidence| evidence.served.is_some())
            .count(),
        absent_served_identities: 0,
        holdout,
        calibration: calibration_verdict,
        float_fidelity: float_fidelity.clone(),
        statistics: statistics.clone(),
    };
    let verdict = ReleaseVerdict::from_measurements(&measurements);

    let report = ReleaseReport {
        model_commit: commit.commit_id.clone(),
        model_id: commit.model_id.as_str().to_string(),
        feature_schema: input.recorded[0].input().feature_schema,
        transport,        replay,
        holdout: measurements.holdout.clone(),
        calibration: calibration.report().clone(),
        float_fidelity,
        statistics,
        measurements,
        verdict,
        scope: RELEASE_SCOPE,
        statistical_scope: STATISTICAL_SCOPE,
    };

    Ok(GateOutcome { predictor, report })
}

// ---------------------------------------------------------------------------
// 2. the artifact
// ---------------------------------------------------------------------------

/// Verify the commit with the accepted check and measure its plain-JSON
/// transport.
///
/// The verification is the gate: a commit whose checkpoint parameters do not
/// hash to the content hash its identity was derived from is refused, and there
/// is no tolerance on that either — the accepted check hashes `f64::to_bits`.
fn measure_commit_transport(commit: &ModelCommit) -> Result<CommitTransport, OfflineGateError> {
    if !commit.verify() {
        return Err(OfflineGateError::UnverifiableCommit {
            commit_id: commit.commit_id.as_str().to_string(),
            reason: "the commit failed its own identity and artifact verification".to_string(),
        });
    }
    let mut f64_parameters = 0usize;
    let mut moved_parameters = 0usize;
    if let Ok(bytes) = serde_json::to_vec(commit) {
        if let Ok(reparsed) = serde_json::from_slice::<ModelCommit>(&bytes) {
            for (original, round_tripped) in
                checkpoint_states(&commit.checkpoint)
                    .into_iter()
                    .zip(checkpoint_states(&reparsed.checkpoint))
            {
                for (before, after) in original.iter().zip(round_tripped.iter()) {
                    f64_parameters += 1;
                    if !f64_identical(*before, *after) {
                        moved_parameters += 1;
                    }
                }
            }
        }
    }
    Ok(CommitTransport {
        commit_id: commit.commit_id.as_str().to_string(),
        f64_parameters,
        moved_parameters,
        plain_json_exact: moved_parameters == 0,
    })
}

/// A checkpoint's four model-state parameter vectors, in a fixed order, so the
/// transport measurement is a function of the checkpoint alone.
fn checkpoint_states(checkpoint: &ModelCheckpoint) -> [&[f64]; 4] {
    [
        &checkpoint.success.parameters,
        &checkpoint.latency.parameters,
        &checkpoint.ttft.parameters,
        &checkpoint.cost.parameters,
    ]
}

/// Build the predictor, mapping the accepted artifact refusals onto typed ones.
fn build_predictor(
    commit: &ModelCommit,
    lineage: &[ModelCommit],
) -> Result<ModelEnsemblePredictor, OfflineGateError> {
    ModelEnsemblePredictor::from_model_commit_with_lineage(commit, lineage).map_err(|error| {
        let reason = error.to_string();
        match error {
            ReplayError::InvalidCommit { .. } => OfflineGateError::UnverifiableCommit {
                commit_id: commit.commit_id.as_str().to_string(),
                reason,
            },
            _ => OfflineGateError::LineageRejected {
                commit_id: commit.commit_id.as_str().to_string(),
                reason,
            },
        }
    })
}

// ---------------------------------------------------------------------------
// 3/4. one decision
// ---------------------------------------------------------------------------

/// Replay one recorded decision and establish everything the gate claims about
/// it.
fn replay_one(
    engine: &ShadowEngine,
    predictor: &ModelEnsemblePredictor,
    recorded: &RecordedDecision,
    config: &GateConfig,
) -> Result<ReplayEvidence, OfflineGateError> {
    let decision_id = recorded.input().decision_id.clone();
    let recorded_decision = &recorded.decision;

    if recorded_decision.input().candidates.is_empty() {
        return Err(OfflineGateError::RetainedInputAbsent {
            decision_id,
            reason: "the retained decision-time input carries no candidates".to_string(),
        });
    }

    // A non-finite value on the recorded side is refused before any comparison,
    // because under `to_bits` it could otherwise be scored a match.
    require_finite_recorded(&decision_id, recorded_decision)?;

    let replayed = replay(engine, predictor, recorded)?;

    // The retained input must be the input the decision was made from, and
    // that is checked before the components: a retained input whose checksum
    // disagrees is a different input, and comparing components across two
    // different inputs would answer the wrong question.
    if recorded_decision.decision_input_checksum != replayed.decision_input_checksum {
        return Err(OfflineGateError::RetainedInputDrifted {
            decision_id,
            recorded: recorded_decision.decision_input_checksum,
            replayed: replayed.decision_input_checksum,
        });
    }

    let exactness = compare_decisions(recorded_decision, &replayed);
    let components_compared = component_count(recorded_decision);
    exactness.into_divergence().map_err(|divergence| {
        OfflineGateError::ReplayDivergence {
            decision_id: decision_id.clone(),
            divergence: Box::new(divergence),
        }
    })?;

    // Retention completeness, through the accepted function, so the refusal
    // vocabulary for an incomplete retained input is 7E-2D's.
    let complete_for_outcome =
        canonical_samples_from_decision_time(&recorded.outcome, recorded.input(), DataOrigin::Native)
            .is_ok();

    let retention = RetentionProof {
        recorded_input_checksum: recorded_decision.decision_input_checksum,
        replayed_input_checksum: replayed.decision_input_checksum,
        retained_candidates: recorded.input().candidates.len(),
        eligible_candidates: recorded
            .input()
            .candidates
            .iter()
            .filter(|candidate| candidate.eligible)
            .count(),
        outcome_candidates: outcome_candidate_count(&recorded.outcome),
        complete_for_outcome,
        ablation: ablate_retention(engine, predictor, recorded, config),
    };

    let served = ServedIdentity::from_outcome(&recorded.outcome);
    let terminal = agree_with_outcome(&decision_id, &replayed, &recorded.outcome)?;
    let failure = FailureAuthority::read_from(&recorded.outcome);

    Ok(ReplayEvidence {
        shadow_id: recorded_decision.shadow_id.clone(),
        decision_id,
        model_commit: replayed.shadow.model_commit.clone(),
        feature_schema: replayed.shadow.feature_schema,
        selected: replayed.shadow.selected.clone(),
        action: replayed.shadow.action,
        components_compared,
        retention,
        terminal,
        recorded_terminal: recorded.outcome.final_status,
        failure,
        served,
    })
}

/// Re-derive the decision from the retained input alone.
fn replay(
    engine: &ShadowEngine,
    predictor: &ModelEnsemblePredictor,
    recorded: &RecordedDecision,
) -> Result<ShadowDecision, OfflineGateError> {
    engine
        .evaluate_with(
            &recorded.decision.actual.request_id,
            recorded.input(),
            predictor,
        )
        .ok_or_else(|| OfflineGateError::ReplayProducedNoDecision {
            decision_id: recorded.input().decision_id.clone(),
        })
}

/// The comparison, in its documented order.
///
/// The order is the contract: the refusal names *the first* component that
/// differs, so it has to be a fixed one rather than a consequence of field
/// layout. Every compared scalar is a bit comparison, and the two checksums
/// come last because a checksum difference is a *consequence* of a component
/// difference, not a component in its own right.
fn compare_decisions(recorded: &ShadowDecision, replayed: &ShadowDecision) -> Exactness {
    let mut exactness = Exactness::new();

    exactness.expect_str(
        "shadow.model_commit",
        None,
        recorded.shadow.model_commit.as_str(),
        replayed.shadow.model_commit.as_str(),
    );
    exactness.expect_u64(
        "shadow.feature_schema",
        None,
        u64::from(recorded.shadow.feature_schema),
        u64::from(replayed.shadow.feature_schema),
    );
    exactness.expect_str(
        "shadow.selected",
        None,
        &recorded.shadow.selected,
        &replayed.shadow.selected,
    );
    exactness.expect_debug_eq(
        "shadow.action",
        &recorded.shadow.action,
        &replayed.shadow.action,
    );
    exactness.expect_str(
        "shadow.reason",
        None,
        &recorded.shadow.reason,
        &replayed.shadow.reason,
    );
    exactness.expect_len(
        "shadow.ranked_candidates",
        recorded.shadow.ranked_candidates.len(),
        replayed.shadow.ranked_candidates.len(),
    );
    for (index, (expected, actual)) in recorded
        .shadow
        .ranked_candidates
        .iter()
        .zip(replayed.shadow.ranked_candidates.iter())
        .enumerate()
    {
        exactness.expect_str(
            &format!("shadow.ranked_candidates[{index}]"),
            None,
            expected,
            actual,
        );
    }

    compare_candidate_evidence(&mut exactness, &recorded.candidates, &replayed.candidates);
    compare_candidate_predictions(
        &mut exactness,
        &recorded.candidates,
        &replayed.candidates,
    );
    compare_candidate_utilities(
        &mut exactness,
        &recorded.candidates,
        &replayed.candidates,
    );

    exactness.expect_u64(
        "decision_checksum",
        None,
        recorded.decision_checksum,
        replayed.decision_checksum,
    );
    exactness
}

fn compare_candidate_evidence(
    exactness: &mut Exactness,
    recorded: &[ShadowCandidate],
    replayed: &[ShadowCandidate],
) {
    exactness.expect_len("candidates", recorded.len(), replayed.len());
    for (index, (expected, actual)) in recorded.iter().zip(replayed.iter()).enumerate() {
        exactness.expect_str(
            &format!("candidates[{index}].candidate_id"),
            Some(index),
            &expected.candidate_id,
            &actual.candidate_id,
        );
        exactness.expect_str(
            &format!("candidates[{index}].provider_id"),
            Some(index),
            &expected.provider_id,
            &actual.provider_id,
        );
        exactness.expect_debug_eq(
            &format!("candidates[{index}].tier"),
            &expected.tier,
            &actual.tier,
        );
        exactness.expect_bool(
            &format!("candidates[{index}].eligible"),
            Some(index),
            expected.eligible,
            actual.eligible,
        );
        exactness.expect_bool(
            &format!("candidates[{index}].valid"),
            Some(index),
            expected.valid,
            actual.valid,
        );
        exactness.expect_opt_str(
            &format!("candidates[{index}].rejection_reason"),
            Some(index),
            expected.rejection_reason.as_deref(),
            actual.rejection_reason.as_deref(),
        );
    }
}

fn compare_candidate_predictions(
    exactness: &mut Exactness,
    recorded: &[ShadowCandidate],
    replayed: &[ShadowCandidate],
) {
    for (index, (expected, actual)) in recorded.iter().zip(replayed.iter()).enumerate() {
        let (expected, actual) = (&expected.prediction, &actual.prediction);
        exactness.expect_str(
            &format!("candidates[{index}].prediction.candidate_model"),
            Some(index),
            &expected.candidate_model,
            &actual.candidate_model,
        );
        exactness.expect_str(
            &format!("candidates[{index}].prediction.candidate_provider"),
            Some(index),
            &expected.candidate_provider,
            &actual.candidate_provider,
        );
        for (name, expected_head, actual_head) in [
            ("success", &expected.success, &actual.success),
            ("latency", &expected.latency, &actual.latency),
            ("ttft", &expected.ttft, &actual.ttft),
            ("cost", &expected.cost, &actual.cost),
        ] {
            // Each field is named separately rather than the head as a whole:
            // a refusal that says "the success prediction differs" has left the
            // reader to guess whether the value, the confidence, the sample
            // count or the cold flag moved.
            exactness.expect_f64(
                &format!("candidates[{index}].prediction.{name}.value"),
                Some(index),
                expected_head.value,
                actual_head.value,
            );
            exactness.expect_f64(
                &format!("candidates[{index}].prediction.{name}.confidence"),
                Some(index),
                expected_head.confidence,
                actual_head.confidence,
            );
            exactness.expect_u64(
                &format!("candidates[{index}].prediction.{name}.sample_count"),
                Some(index),
                expected_head.sample_count,
                actual_head.sample_count,
            );
            exactness.expect_bool(
                &format!("candidates[{index}].prediction.{name}.cold"),
                Some(index),
                expected_head.cold,
                actual_head.cold,
            );
        }
    }
}

fn compare_candidate_utilities(
    exactness: &mut Exactness,
    recorded: &[ShadowCandidate],
    replayed: &[ShadowCandidate],
) {
    for (index, (expected, actual)) in recorded.iter().zip(replayed.iter()).enumerate() {
        let (expected, actual) = (&expected.utility, &actual.utility);
        for (name, expected_value, actual_value) in [
            ("success", expected.success, actual.success),
            ("latency", expected.latency, actual.latency),
            ("ttft", expected.ttft, actual.ttft),
            ("cost", expected.cost, actual.cost),
            ("fallback", expected.fallback, actual.fallback),
            ("uncertainty", expected.uncertainty, actual.uncertainty),
            ("switch_cost", expected.switch_cost, actual.switch_cost),
            ("total", expected.total, actual.total),
        ] {
            exactness.expect_f64(
                &format!("candidates[{index}].utility.{name}"),
                Some(index),
                expected_value,
                actual_value,
            );
        }
    }
}

/// How many scalar components one comparison walks.
///
/// A count of what was checked, so "bit-identical" on the report is a claim
/// about a known number of values rather than an adjective.
fn component_count(decision: &ShadowDecision) -> usize {
    let mut count = 5 // model_commit, feature_schema, selected, action, reason
        + 1 // ranked_candidates length
        + decision.shadow.ranked_candidates.len()
        + 1 // decision_checksum
        + 1; // candidates length
    for _ in &decision.candidates {
        count += 6; // id, provider, tier, eligible, valid, rejection_reason
        count += 2; // prediction model and provider
        count += 4 * 4; // four heads, each value/confidence/sample_count/cold
        count += 8; // eight utility terms
    }
    count
}

/// Refuse a recorded decision that carries a value no equality verdict could be
/// about.
fn require_finite_recorded(
    decision_id: &str,
    decision: &ShadowDecision,
) -> Result<(), OfflineGateError> {
    let mut owned: Vec<(String, f64)> = Vec::new();
    for (index, candidate) in decision.candidates.iter().enumerate() {
        for (name, value) in [
            ("prediction.success.value", candidate.prediction.success.value),
            (
                "prediction.success.confidence",
                candidate.prediction.success.confidence,
            ),
            (
                "prediction.latency.value",
                candidate.prediction.latency.value,
            ),
            (
                "prediction.latency.confidence",
                candidate.prediction.latency.confidence,
            ),
            ("prediction.ttft.value", candidate.prediction.ttft.value),
            (
                "prediction.ttft.confidence",
                candidate.prediction.ttft.confidence,
            ),
            ("prediction.cost.value", candidate.prediction.cost.value),
            (
                "prediction.cost.confidence",
                candidate.prediction.cost.confidence,
            ),
            ("utility.success", candidate.utility.success),
            ("utility.latency", candidate.utility.latency),
            ("utility.ttft", candidate.utility.ttft),
            ("utility.cost", candidate.utility.cost),
            ("utility.fallback", candidate.utility.fallback),
            ("utility.uncertainty", candidate.utility.uncertainty),
            ("utility.switch_cost", candidate.utility.switch_cost),
            ("utility.total", candidate.utility.total),
        ] {
            owned.push((format!("candidates[{index}].{name}"), value));
        }
    }
    let borrowed: Vec<(&str, f64)> = owned
        .iter()
        .map(|(component, value)| (component.as_str(), *value))
        .collect();
    find_nonfinite_f64(&borrowed).map_err(|component| OfflineGateError::NonFiniteMeasurement {
        decision_id: decision_id.to_string(),
        component: Box::new(component),
    })
}

/// How the replayed decision sits against the recorded Outcome.
///
/// Two situations cannot be reconciled and are refusals rather than values of
/// [`TerminalAgreement`], so no value of that enum means "we could not tell":
///
/// * the replay selected a candidate the request never attempted — the
///   retained input and the Outcome describe different requests, so the replay
///   is not evidence about this Outcome at all;
/// * the Outcome records a terminal success with no served identity — the
///   evaluation cannot be keyed on what served, and planned identity is not a
///   substitute for it.
fn agree_with_outcome(
    decision_id: &str,
    replayed: &ShadowDecision,
    outcome: &Outcome,
) -> Result<TerminalAgreement, OfflineGateError> {
    let selected = &replayed.shadow.selected;
    let attempted: BTreeSet<(&str, &str)> = outcome
        .attempts
        .iter()
        .map(|attempt| {
            (
                attempt.candidate_provider.as_str(),
                attempt.candidate_model.as_str(),
            )
        })
        .collect();

    // The replay's selection is a candidate key; the Outcome records model and
    // provider. The key is resolved through the retained input, which is the
    // only place the two vocabularies meet.
    let selected_provider = selected_provider(replayed);
    let was_attempted = selected_provider.is_some_and(|provider| {
        attempted
            .iter()
            .any(|(attempted_provider, attempted_model)| {
                *attempted_model == selected.as_str() && *attempted_provider == provider
            })
    });
    if !was_attempted {
        return Err(OfflineGateError::TerminalStateDisagreement {
            decision_id: decision_id.to_string(),
            detail: format!(
                "the replay selected '{selected}', which Outcome {} never attempted (attempts: [{}])",
                outcome.outcome_id,
                outcome
                    .attempts
                    .iter()
                    .map(|attempt| format!(
                        "{}/{}",
                        attempt.candidate_provider, attempt.candidate_model
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    if outcome.final_status.is_success() {
        let Some(served) = ServedIdentity::from_outcome(outcome) else {
            return Err(OfflineGateError::ServedIdentityAbsent {
                decision_id: decision_id.to_string(),
                outcome_id: outcome.outcome_id.clone(),
            });
        };
        return Ok(if served.model == *selected {
            TerminalAgreement::SelectionServed
        } else {
            TerminalAgreement::SelectionSupersededByFailover
        });
    }

    // A failed request has no served identity by definition, so the last
    // attempt is the closest thing the record has to a terminal state.
    Ok(
        if outcome
            .attempts
            .last()
            .is_some_and(|attempt| attempt.candidate_model == *selected)
        {
            TerminalAgreement::SelectionEndedTheFailure
        } else {
            TerminalAgreement::SelectionAttemptedNotFinal
        },
    )
}

/// The provider of the candidate the replay selected, from the retained input.
fn selected_provider(replayed: &ShadowDecision) -> Option<&str> {
    replayed
        .observation
        .input
        .candidates
        .iter()
        .find(|candidate| candidate.candidate_id == replayed.shadow.selected)
        .map(|candidate| candidate.provider_id.as_str())
}

/// How many distinct candidates the canonical Outcome touched.
fn outcome_candidate_count(outcome: &Outcome) -> usize {
    let mut identities: BTreeSet<(&str, &str)> = BTreeSet::new();
    for attempt in &outcome.attempts {
        identities.insert((
            attempt.candidate_provider.as_str(),
            attempt.candidate_model.as_str(),
        ));
    }
    identities.len()
}

// ---------------------------------------------------------------------------
// the retention ablation
// ---------------------------------------------------------------------------

/// Perturb retained feature positions one at a time and replay, so the claim
/// "the replay is driven by the retained input" is a measurement.
///
/// A position whose perturbation changes a prediction, a utility, the
/// selection, the action, the reason or the ranking is **load-bearing**: the
/// retained bytes reach the decision arithmetic. Because the comparison order
/// puts every component before the decision checksum, and the decision checksum
/// mixes the input checksum, a load-bearing position's *first* divergence is
/// always a semantic component. A position that produces no divergence at all
/// is inert at this commit, and is reported as such rather than dropped.
///
/// This does not claim the equivalence would fail without retention in
/// production — there is no production path to fail. It claims the weaker,
/// checkable thing: the replay is a function of the retained input, and not a
/// function that ignores it.
fn ablate_retention(
    engine: &ShadowEngine,
    predictor: &ModelEnsemblePredictor,
    recorded: &RecordedDecision,
    config: &GateConfig,
) -> RetentionAblation {
    let mut probed = 0usize;
    let mut load_bearing = 0usize;
    if config.retention_probes == 0 {
        return RetentionAblation {
            probed,
            load_bearing,
        };
    }
    let Some(candidate) = recorded
        .input()
        .candidates
        .iter()
        .find(|candidate| candidate.eligible)
    else {
        return RetentionAblation {
            probed,
            load_bearing,
        };
    };
    let target = candidate.candidate_id.clone();

    for position in 0..FEATURE_DIMENSION.min(config.retention_probes) {
        let mut perturbed = recorded.input().clone();
        let Some(entry) = perturbed
            .candidates
            .iter_mut()
            .find(|entry| entry.candidate_id == target)
        else {
            break;
        };
        let before = entry.features.values[position];
        let after = perturbed_value(before);
        if f32_identical(before, after) {
            continue;
        }
        entry.features.values[position] = after;

        probed += 1;
        // A perturbation the accepted seam refuses is itself evidence that the
        // retained value was load-bearing: the value changed what the replay
        // was willing to decide.
        let Some(replayed) = engine.evaluate_with(
            &recorded.decision.actual.request_id,
            &perturbed,
            predictor,
        ) else {
            load_bearing += 1;
            continue;
        };
        let divergence = compare_decisions(&recorded.decision, &replayed)
            .divergence()
            .map(|divergence| divergence.component.clone());
        match divergence {
            // Every component precedes the checksum, so a divergence here is a
            // semantic one.
            Some(component) if component != "decision_checksum" => load_bearing += 1,
            // Only the checksum moved: the input changed but the decision did
            // not. Inert at this commit, and reported as such.
            Some(_) => {}
            None => {}
        }
    }

    RetentionAblation {
        probed,
        load_bearing,
    }
}

/// A finite, distinct replacement for one feature value.
fn perturbed_value(value: f32) -> f32 {
    if value.is_finite() && value.abs() < 1.0e6 {
        let shifted = value + 0.5;
        if shifted.is_finite() && !f32_identical(shifted, value) {
            return shifted;
        }
    }
    if f32_identical(value, 0.25) {
        0.75
    } else {
        0.25
    }
}

// ---------------------------------------------------------------------------
// 5/6/7. the holdout
// ---------------------------------------------------------------------------

/// The canonical samples for the holdout, keyed on the identity that served.
///
/// Every row the accepted conversion produces is kept. That is not an
/// attempt-level attribution claim — attributing credit or blame per attempt
/// is node 7D's, sequenced after this one (E-092) — it is the *candidate set
/// the decision compared*. 7E-2D builds its K axis out of the attempt-scope
/// rows and ignores the request-scope one, so handing it request rows only
/// would give it a one-candidate axis and a trivially perfect vector, and
/// "calibrated" would mean nothing at all. The K axis has to be the axis.
///
/// The decision as a whole must still have a served identity, and that is a
/// refusal rather than a row with a substituted planned identity: an
/// evaluation keyed on the wrong identity is an evaluation of something that
/// did not happen.
fn build_holdout(
    recorded: &[RecordedDecision],
    evidence: &[ReplayEvidence],
    fit_sample_ids: &BTreeSet<String>,
) -> Result<Vec<OutcomeTrainingSample>, OfflineGateError> {
    let mut samples = Vec::new();
    for (index, entry) in recorded.iter().enumerate() {
        let decision_id = entry.input().decision_id.clone();
        if ServedIdentity::from_outcome(&entry.outcome).is_none() {
            return Err(OfflineGateError::ServedIdentityAbsent {
                decision_id,
                outcome_id: entry.outcome.outcome_id.clone(),
            });
        }
        let replay = evidence.get(index).ok_or_else(|| {
            OfflineGateError::RetainedInputAbsent {
                decision_id: decision_id.clone(),
                reason: "the replay evidence for this decision is missing".to_string(),
            }
        })?;
        if replay.served.is_none() {
            return Err(OfflineGateError::ServedIdentityAbsent {
                decision_id: decision_id.clone(),
                outcome_id: entry.outcome.outcome_id.clone(),
            });
        }

        let candidates =
            canonical_samples_from_decision_time(&entry.outcome, entry.input(), DataOrigin::Native)
                .map_err(|reason| OfflineGateError::RetainedInputIncomplete {
                    decision_id: decision_id.clone(),
                    reason,
                })?;

        // The request-scope row must be keyed on the identity that served. It
        // is built by the accepted resolver, which falls back to
        // last-attempted and then planned; a served identity is present, so
        // anything else is a contradiction rather than a fallback.
        if let Some(served) = &replay.served {
            let request = candidates
                .iter()
                .find(|sample| matches!(sample.scope, SampleScope::Request))
                .ok_or_else(|| OfflineGateError::RetainedInputIncomplete {
                    decision_id: decision_id.clone(),
                    reason: "the canonical conversion produced no request-scope sample".to_string(),
                })?;
            if request.model_id != served.model || request.provider_id != served.provider {
                return Err(OfflineGateError::TerminalStateDisagreement {
                    decision_id,
                    detail: format!(
                        "the request-scope sample is keyed on '{}/{}' but the Outcome records \
                         '{}/{}' as served",
                        request.provider_id, request.model_id, served.provider, served.model
                    ),
                });
            }
        }

        for sample in candidates {
            if let Some(overlap) = fit_sample_ids.get(&sample.sample_id) {
                return Err(OfflineGateError::HoldoutOverlap {
                    kind: "sample id",
                    sample_id: overlap.clone(),
                    holdout_index: samples.len(),
                });
            }
            samples.push(sample);
        }
    }
    Ok(samples)
}

fn summarise_holdout(
    recorded: &[RecordedDecision],
    evidence: &[ReplayEvidence],
    samples: usize,
) -> HoldoutSummary {
    let mut served_identities: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in recorded {
        if let Some(served) = ServedIdentity::from_outcome(&entry.outcome) {
            served_identities.insert((served.provider, served.model));
        }
    }
    HoldoutSummary {
        samples,
        decisions: recorded.len(),
        attributed: evidence
            .iter()
            .filter(|entry| entry.served.is_some() && entry.recorded_terminal.is_success())
            .count(),
        served_identities: served_identities.len(),
        overlap_with_fit_set: 0,
    }
}

// ---------------------------------------------------------------------------
// 7E-2D's refusals, carried rather than re-derived
// ---------------------------------------------------------------------------

/// Carry 7E-2D's refusal, singling out the two this gate has its own reason for.
///
/// The two mapped cases keep 7E-2D's own classification — the gate does not
/// decide what "degenerate" means — and the rest is held whole so no refusal
/// is flattened into a generic error. The catch-all is deliberate: this gate
/// is sequenced before 7D and before any later revision of 7E-2D, and it must
/// not be the thing that breaks when a refusal is added upstream.
fn map_calibration_refusal(error: CalibrationError) -> OfflineGateError {
    match error {
        CalibrationError::DegeneratePartition {
            partition,
            reason,
            detail,
        } => OfflineGateError::DegenerateHoldout {
            partition,
            reason,
            detail,
        },
        other => OfflineGateError::CalibrationRefused {
            error: Box::new(other),
        },
    }
}
