use std::collections::BTreeSet;

use serde::Serialize;

use super::RELEASE_SCOPE;
use crate::failure::{FailureClass, FailureImpact};
use crate::ml::calibration::{CalibrationConfig, CalibrationReport, CalibrationVerdict};
use crate::ml::coordinator::{CoordinatorConfig, RoutingAction};
use crate::ml::dataset::OutcomeTrainingSample;
use crate::ml::evaluation::{f64_identical, ulp_distance};
use crate::ml::model_identity::{CommitId, ModelCommit};
use crate::ml::reward::RewardPolicy;
use crate::ml::shadow::{ShadowDecision, ShadowInput};
use crate::ml::statistics::{StatisticalConfig, StatisticalRelease};
use crate::outcome::{FinalStatus, Outcome};

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
    /// Fewest independent decisions the holdout must hold.
    ///
    /// Counted by the decision's request and decision identity and
    /// deduplicated, deliberately **not** by canonical sample rows: canonical
    /// data carries a request row plus one row per attempt for every decision,
    /// so a row floor is satisfied several times over by the same decision and
    /// would let a caller's floor pass on a fraction of the decisions it asked
    /// for.
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
/// * a trained commit was *expected* to fail it. The accepted verification
///   hashes `f64::to_bits`, so a parameter that comes back one ULP different
///   makes the content hash disagree and the commit refuses to verify itself —
///   7E-2F recorded exactly that finding. It no longer does: the workspace
///   enables `float_roundtrip`, so this measurement now reports exact. It is
///   kept as a working safety net, because a transport that silently loses a bit
///   again should withhold the verdict rather than pass;
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
        state.serialize_field(
            "affects_observation",
            &impact.map(|i| i.affects_observation),
        )?;
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
