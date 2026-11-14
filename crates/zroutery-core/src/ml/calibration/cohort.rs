use serde::Serialize;

use super::errors::CalibrationError;
use super::types::CandidateInput;
use super::{FINGERPRINT_OFFSET, FINGERPRINT_PRIME};
use crate::ml::dataset::{validate_outcome_sample, OutcomeTrainingSample, SampleScope};
use crate::ml::features::{FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION};
use crate::ml::model::RoutingModel;
use crate::outcome::{CandidateIdentity, FinalStatus};

// ---------------------------------------------------------------------------
// Cohort ordering
// ---------------------------------------------------------------------------

/// Request-level facts about one decision, used only for ordering and for the
/// report. None of them is an identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CohortContext {
    /// Unix seconds, as recorded. Second resolution, so this alone ties.
    pub timestamp: i64,
    /// The dialect the request arrived in.
    pub dialect: String,
    /// Whether the request was streaming.
    pub streaming: bool,
    /// The terminal status, as a stable rank.
    pub final_status_rank: u8,
    /// The terminal failure class, as a stable rank. `None` on success.
    pub failure_class_rank: Option<u8>,
}

/// The rank of a terminal status, declared here so the ordering never depends
/// on a derive or on a discriminant layout.
const fn final_status_rank(status: FinalStatus) -> u8 {
    match status {
        FinalStatus::Success => 0,
        FinalStatus::Failed => 1,
        FinalStatus::Cancelled => 2,
        FinalStatus::Interrupted => 3,
    }
}

/// One candidate's contribution to the cohort ordering key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AxisKey {
    /// `false` for ranked so ranked candidates sort first and the axis order is
    /// readable in a diagnostic dump.
    ranked: bool,
    provider: String,
    model: String,
    /// The raw success probability as exact bits, so two cohorts tie only when
    /// they would contribute the same gradient.
    probability_bits: u64,
}

/// The canonical content order of a cohort.
///
/// Carries every input the computation consumes, and nothing that came from a
/// UUID. See the module documentation for why a tie here is an equivalence
/// class rather than an arbitrary choice.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CohortOrderKey {
    context: CohortContext,
    served: Option<(String, String)>,
    axis: Vec<AxisKey>,
}

impl CohortOrderKey {
    fn of(
        candidates: &[CandidateInput],
        served: Option<&CandidateIdentity>,
        context: CohortContext,
    ) -> Self {
        Self {
            context,
            served: served.map(|identity| {
                (
                    identity.provider().to_string(),
                    identity.model().to_string(),
                )
            }),
            axis: candidates
                .iter()
                .map(|input| AxisKey {
                    ranked: input.is_ranked(),
                    provider: input.candidate().provider().to_string(),
                    model: input.candidate().model().to_string(),
                    probability_bits: input
                        .raw_success_probability()
                        .map_or(0, |value| value.to_bits()),
                })
                .collect(),
        }
    }

    /// A stable content fingerprint, as lowercase hex.
    ///
    /// FNV-1a over a canonical byte encoding. It is a label, not a security
    /// boundary, and it is computed from the ordering key rather than from any
    /// identifier, so the same logical data fingerprints the same way no
    /// matter what UUIDs the rows carry.
    fn fingerprint(&self) -> String {
        let mut bytes: Vec<u8> = Vec::with_capacity(96);
        bytes.extend_from_slice(&self.context.timestamp.to_le_bytes());
        encode_string(&mut bytes, &self.context.dialect);
        bytes.push(u8::from(self.context.streaming));
        bytes.push(self.context.final_status_rank);
        bytes.push(self.context.failure_class_rank.unwrap_or(u8::MAX));
        match &self.served {
            Some((provider, model)) => {
                bytes.push(1);
                encode_string(&mut bytes, provider);
                encode_string(&mut bytes, model);
            }
            None => bytes.push(0),
        }
        bytes.extend_from_slice(&(self.axis.len() as u64).to_le_bytes());
        for entry in &self.axis {
            bytes.push(u8::from(entry.ranked));
            encode_string(&mut bytes, &entry.provider);
            encode_string(&mut bytes, &entry.model);
            bytes.extend_from_slice(&entry.probability_bits.to_le_bytes());
        }
        format!("{:016x}", fnv1a(&bytes))
    }
}

fn encode_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FINGERPRINT_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FINGERPRINT_PRIME);
    }
    hash
}

// ---------------------------------------------------------------------------
// DecisionCohort
// ---------------------------------------------------------------------------

/// One decision's candidate set, its axis order, and what actually happened.
///
/// Built either by [`project_cohorts`] from a dataset snapshot, or directly by
/// a caller that already has a candidate set. Either way the axis is validated
/// on the way in: a cohort that cannot form a distribution is refused here, not
/// at emit time.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionCohort {
    fingerprint: String,
    pub(super) order_key: CohortOrderKey,
    subject: CandidateIdentity,
    candidates: Vec<CandidateInput>,
    served: Option<CandidateIdentity>,
}

impl DecisionCohort {
    /// Validate and build one cohort.
    ///
    /// `subject_hint` is the identity the decision was planned around. The
    /// 7E-2A type requires the subject to be one of the outcomes, so a hint
    /// that is not in the axis is replaced by the first ranked candidate rather
    /// than emitted as a distribution about a decision that never considered
    /// its own first choice.
    pub fn try_new(
        context: CohortContext,
        subject_hint: Option<&CandidateIdentity>,
        candidates: Vec<CandidateInput>,
        served: Option<CandidateIdentity>,
    ) -> Result<Self, CalibrationError> {
        let fingerprint =
            CohortOrderKey::of(&candidates, served.as_ref(), context.clone()).fingerprint();

        if candidates.is_empty() {
            return Err(CalibrationError::CohortWithoutCandidates {
                cohort: fingerprint,
            });
        }
        let mut seen: Vec<&CandidateIdentity> = Vec::with_capacity(candidates.len());
        for input in &candidates {
            if seen.contains(&input.candidate()) {
                return Err(CalibrationError::DuplicateCohortCandidate {
                    cohort: fingerprint.clone(),
                    candidate: format!(
                        "{}/{}",
                        input.candidate().provider(),
                        input.candidate().model()
                    ),
                });
            }
            seen.push(input.candidate());
        }

        if let Some(served) = served.as_ref() {
            if !candidates.iter().any(|input| input.candidate() == served) {
                return Err(CalibrationError::ServedCandidateNotInCohort {
                    cohort: fingerprint.clone(),
                    served: format!("{}/{}", served.provider(), served.model()),
                });
            }
        }

        let first_ranked = candidates
            .iter()
            .find(|input| input.is_ranked())
            .ok_or_else(|| CalibrationError::CohortWithoutRankedCandidate {
                cohort: fingerprint.clone(),
            })?;

        let subject = subject_hint
            .filter(|hint| candidates.iter().any(|input| input.candidate() == *hint))
            .cloned()
            .unwrap_or_else(|| first_ranked.candidate().clone());

        Ok(Self {
            fingerprint,
            order_key: CohortOrderKey::of(&candidates, served.as_ref(), context),
            subject,
            candidates,
            served,
        })
    }

    /// The content fingerprint. Two cohorts with the same fingerprint are
    /// indistinguishable to this module's computation.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The request-level facts.
    pub fn context(&self) -> &CohortContext {
        &self.order_key.context
    }

    /// The axis, in order.
    pub fn candidates(&self) -> &[CandidateInput] {
        &self.candidates
    }

    /// The decision this distribution is about.
    pub fn subject(&self) -> &CandidateIdentity {
        &self.subject
    }

    /// The candidate that actually served, if any.
    pub fn served(&self) -> Option<&CandidateIdentity> {
        self.served.as_ref()
    }

    /// The arity K.
    pub fn arity(&self) -> usize {
        self.candidates.len()
    }

    /// How many candidates carry a prediction.
    pub fn ranked_count(&self) -> usize {
        self.candidates
            .iter()
            .filter(|input| input.is_ranked())
            .count()
    }

    /// Whether this decision was served.
    pub fn is_attributed(&self) -> bool {
        self.served.is_some()
    }
}

/// Whether a set of cohorts is in the canonical content order.
///
/// Exposed so a consumer that assembles its own cohort list can check the
/// invariant the determinism claim rests on, without the ordering key itself
/// becoming public surface.
///
/// [`run_calibration`] guarantees it **within each partition**, which is where
/// the computation needs it: two cohorts that tie on the key are
/// indistinguishable to every sum it performs, so exchanging them cannot change an
/// answer. It deliberately does *not* hold across the fit/holdout boundary —
/// ordering the whole vector by content and then taking a contiguous suffix would
/// make the holdout a content cluster rather than a later slice of history.
pub fn canonical_order_holds(cohorts: &[DecisionCohort]) -> bool {
    unordered_at(cohorts).is_none()
}

// ---------------------------------------------------------------------------
// project_cohorts
// ---------------------------------------------------------------------------

/// Reconstruct the decision cohorts of a dataset snapshot.
///
/// Only attempt-scope rows are candidate evidence: a request-scope row is the
/// request as a whole, not a candidate, and treating it as one would put a
/// twelfth "candidate" in the axis that no decision ever compared.
///
/// The returned cohorts are in **arrival order** — the snapshot's row order, which
/// is a deterministic function of the snapshot. That is the order in which a
/// prefix and a suffix mean "earlier" and "later", and it is the order
/// [`run_calibration`] takes its temporal split from.
///
/// It is deliberately *not* in [`CohortOrderKey`] order. That key makes every
/// computation in this module invariant to permuting cohorts that tie on it, but
/// a partition is not a computation: which cohorts end up in the holdout is
/// decided by position, and position in a content-sorted vector is a function of
/// tie-breaking rather than of history. Ordering by content therefore belongs
/// after the split, and [`run_calibration`] does it there.
pub fn project_cohorts(
    snapshot: &[OutcomeTrainingSample],
    model: &dyn RoutingModel,
    probability_floor: f64,
) -> Result<Vec<DecisionCohort>, CalibrationError> {
    if snapshot.is_empty() {
        return Err(CalibrationError::EmptySnapshot);
    }
    if !probability_floor.is_finite() || probability_floor <= 0.0 || probability_floor >= 0.5 {
        return Err(CalibrationError::InvalidProbabilityFloor {
            floor: probability_floor,
        });
    }

    let attempt_rows = attempt_row_indexes(snapshot)?;
    let groups = group_attempt_rows(snapshot, &attempt_rows)?;

    let mut cohorts = Vec::with_capacity(groups.len());
    for group in &groups {
        cohorts.push(build_cohort(snapshot, group, model)?);
    }

    // Arrival order, deliberately. `group_attempt_rows` walks the snapshot's rows
    // in index order, so this vector is already a deterministic function of the
    // snapshot and it is the *only* order in which a prefix and a suffix mean
    // "earlier" and "later".
    //
    // It used to be sorted by [`CohortOrderKey`] here, on the reasoning that a
    // content key makes the computation invariant to tie permutation. That
    // reasoning is sound and it was applied in the wrong place: the *statistics*
    // of a partition are invariant to permuting cohorts that tie on the key, but
    // **which cohorts are in the partition is not**. `run_calibration` takes a
    // contiguous suffix as a frozen holdout, and a contiguous suffix of a
    // content-sorted vector is whichever content cluster sorted last — not a later
    // slice of history.
    //
    // The cost was a holdout that was silently a function of tie-breaking. Because
    // `sort_by` is stable, cohorts that tie keep the order `group_attempt_rows`
    // produced, so the degenerate case appeared only when the ties fell
    // differently: measured on a window whose recorded decisions alternate two
    // served identities perfectly, the holdout held one identity in five runs out
    // of six and six in the other, and the gate correctly refused the degenerate
    // minority as `SingleServedCandidate`.
    //
    // Ordering now happens per partition, after the split, where invariance to
    // tie permutation is the property actually needed.
    Ok(cohorts)
}

/// Indexes of the attempt-scope rows, after rejecting a repeated sample id.
fn attempt_row_indexes(snapshot: &[OutcomeTrainingSample]) -> Result<Vec<usize>, CalibrationError> {
    let mut rows = Vec::with_capacity(snapshot.len());
    let mut seen: Vec<(&str, usize)> = Vec::with_capacity(snapshot.len());
    for (index, sample) in snapshot.iter().enumerate() {
        if matches!(sample.scope, SampleScope::Request) {
            continue;
        }
        if let Some((_, first)) = seen.iter().find(|(id, _)| *id == sample.sample_id.as_str()) {
            return Err(CalibrationError::DuplicateSampleId {
                sample_id: sample.sample_id.clone(),
                first: *first,
                second: index,
            });
        }
        seen.push((sample.sample_id.as_str(), index));
        rows.push(index);
    }
    if rows.is_empty() {
        return Err(CalibrationError::EmptySnapshot);
    }
    Ok(rows)
}

/// A group of attempt rows that belong to one decision.
struct CohortGroup {
    outcome_id: String,
    decision_id: Option<String>,
    context: CohortContext,
    subject_hint: Option<CandidateIdentity>,
    served: Option<CandidateIdentity>,
    /// Row indexes, in the order the decision attempted them.
    rows: Vec<usize>,
}

impl CohortGroup {
    /// A content-derived label for a cohort that has not been built yet.
    ///
    /// Used only in the per-row refusals below, which fire before the cohort
    /// exists and therefore before it has a fingerprint. Derived from the
    /// group's own contents, never from its identifier.
    fn pending_label(&self) -> String {
        format!(
            "{}/{}/{}",
            self.context.dialect,
            self.context.timestamp,
            self.rows.len()
        )
    }
}

/// Group attempt rows into decisions, refusing an incoherent decision scope.
fn group_attempt_rows(
    snapshot: &[OutcomeTrainingSample],
    rows: &[usize],
) -> Result<Vec<CohortGroup>, CalibrationError> {
    // First by request, so a request that disagrees with itself about carrying
    // a decision id is caught before it is split.
    let mut by_request: Vec<(&str, Vec<usize>)> = Vec::new();
    for &row in rows {
        let outcome_id = snapshot[row].outcome_id.as_str();
        match by_request.iter_mut().find(|(id, _)| *id == outcome_id) {
            Some((_, bucket)) => bucket.push(row),
            None => by_request.push((outcome_id, vec![row])),
        }
    }

    // Then by decision within the request, in attempt order.
    let mut scoped: Vec<CohortGroup> = Vec::new();
    for (outcome_id, request_rows) in &by_request {
        let with_decision = request_rows
            .iter()
            .filter(|row| snapshot[**row].decision_id.is_some())
            .count();
        if with_decision > 0 && with_decision != request_rows.len() {
            return Err(CalibrationError::MixedDecisionScope {
                request: (*outcome_id).to_string(),
                with: with_decision,
                without: request_rows.len() - with_decision,
            });
        }

        let mut ordered = request_rows.clone();
        ordered.sort_by_key(|row| attempt_index(&snapshot[*row].scope));
        let mut active: Option<(Option<String>, Vec<usize>)> = None;
        for row in ordered {
            let decision_id = snapshot[row].decision_id.clone();
            match &mut active {
                Some((current, bucket)) if *current == decision_id => bucket.push(row),
                _ => {
                    if let Some((decision_id, bucket)) = active.replace((decision_id, vec![row])) {
                        scoped.push(new_group(snapshot, outcome_id, decision_id, &bucket));
                    }
                }
            }
        }
        if let Some((decision_id, bucket)) = active {
            scoped.push(new_group(snapshot, outcome_id, decision_id, &bucket));
        }
    }

    // Finally, a decision id that names two requests is not one decision.
    let mut owners: Vec<(&str, &str)> = Vec::new();
    for group in &scoped {
        let Some(decision_id) = group.decision_id.as_deref() else {
            continue;
        };
        match owners.iter_mut().find(|(id, _)| *id == decision_id) {
            Some((_, owner)) => {
                if *owner != group.outcome_id {
                    return Err(CalibrationError::DecisionScopeSpansRequests {
                        decision: decision_id.to_string(),
                        requests: 2,
                    });
                }
            }
            None => owners.push((decision_id, group.outcome_id.as_str())),
        }
    }

    Ok(scoped)
}

fn new_group(
    snapshot: &[OutcomeTrainingSample],
    outcome_id: &str,
    decision_id: Option<String>,
    rows: &[usize],
) -> CohortGroup {
    let head = &snapshot[rows[0]];
    CohortGroup {
        outcome_id: outcome_id.to_string(),
        decision_id,
        context: CohortContext {
            timestamp: head.timestamp,
            dialect: head.dialect.clone(),
            streaming: head.streaming,
            final_status_rank: final_status_rank(head.final_status),
            failure_class_rank: head
                .terminal_error
                .as_ref()
                .map(|facts| failure_class_rank(facts.class)),
        },
        subject_hint: head.identity.planned.clone(),
        served: head.identity.served.clone(),
        rows: rows.to_vec(),
    }
}

fn failure_class_rank(class: crate::failure::FailureClass) -> u8 {
    crate::failure::FailureClass::ALL
        .iter()
        .position(|candidate| *candidate == class)
        .map_or(u8::MAX, |index| index as u8)
}

fn attempt_index(scope: &SampleScope) -> usize {
    match scope {
        SampleScope::Attempt { index, .. } => *index,
        SampleScope::Request => usize::MAX,
    }
}

/// Build one decision's cohort from its attempt rows.
///
/// The axis is the set of unique `(provider, model)` identities the decision
/// attempted, in first-attempt order - *not* one entry per attempt. The product
/// retries the same candidate after a rectifier repairs a request, and records
/// each attempt separately; treating every attempt as a new candidate would put
/// one identity on its own axis twice and make a legitimate retry fail the
/// whole projection as a duplicate-candidate error.
///
/// A retried candidate's raw prediction is taken from its **terminal** attempt,
/// the last row for that identity in attempt order. `served` is the decision's
/// terminal attribution, so the terminal attempt is the observation the
/// candidate's outcome belongs to; earlier attempts remain in the snapshot and
/// in every sample's retained `attempts` evidence, they simply do not become
/// separate axis entries.
fn build_cohort(
    snapshot: &[OutcomeTrainingSample],
    group: &CohortGroup,
    model: &dyn RoutingModel,
) -> Result<DecisionCohort, CalibrationError> {
    let mut candidates: Vec<CandidateInput> = Vec::with_capacity(group.rows.len());
    for &row in &group.rows {
        let sample = &snapshot[row];
        validate_calibration_sample(row, sample)?;
        let raw = model.predict(&sample.features).value;
        let candidate = format!("{}/{}", sample.provider_id, sample.model_id);
        if !raw.is_finite() {
            return Err(CalibrationError::NonFinitePrediction {
                cohort: group.pending_label(),
                candidate,
                value: raw,
            });
        }
        if !(0.0..=1.0).contains(&raw) {
            return Err(CalibrationError::PredictionOutOfRange {
                cohort: group.pending_label(),
                candidate,
                value: raw,
            });
        }
        let identity = CandidateIdentity::new(sample.model_id.clone(), sample.provider_id.clone());
        let ranked = CandidateInput::Ranked {
            candidate: identity,
            raw_success_probability: raw,
        };
        match candidates
            .iter()
            .position(|input| input.candidate() == ranked.candidate())
        {
            // Another attempt of a candidate already on the axis: the terminal
            // attempt replaces the raw prediction and the axis position is kept.
            Some(slot) => candidates[slot] = ranked,
            None => candidates.push(ranked),
        }
    }
    DecisionCohort::try_new(
        group.context.clone(),
        group.subject_hint.as_ref(),
        candidates,
        group.served.clone(),
    )
}

fn validate_calibration_sample(
    index: usize,
    sample: &OutcomeTrainingSample,
) -> Result<(), CalibrationError> {
    if sample.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(CalibrationError::SchemaMismatch {
            index,
            component: "sample",
            found: sample.schema_version,
            expected: FEATURE_SCHEMA_VERSION,
        });
    }
    if sample.features.schema_version != FEATURE_SCHEMA_VERSION {
        return Err(CalibrationError::SchemaMismatch {
            index,
            component: "feature",
            found: sample.features.schema_version,
            expected: FEATURE_SCHEMA_VERSION,
        });
    }
    if sample.features.values.len() != FEATURE_DIMENSION {
        return Err(CalibrationError::FeatureDimension {
            index,
            found: sample.features.values.len(),
            expected: FEATURE_DIMENSION,
        });
    }
    if let Some((position, value)) = sample
        .features
        .values
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        return Err(CalibrationError::NonFiniteFeature {
            index,
            position,
            value: *value,
        });
    }
    validate_outcome_sample(sample)
        .map_err(|reason| CalibrationError::InvalidSample { index, reason })
}

pub(super) fn unordered_at(cohorts: &[DecisionCohort]) -> Option<usize> {
    cohorts
        .windows(2)
        .position(|pair| pair[1].order_key < pair[0].order_key)
}
