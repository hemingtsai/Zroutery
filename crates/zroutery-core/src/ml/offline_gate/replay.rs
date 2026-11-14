use std::collections::BTreeSet;

use crate::feedback::DataOrigin;
use crate::ml::dataset::{
    canonical_samples_from_decision_time, OutcomeTrainingSample, SampleScope,
};
use crate::ml::evaluation::{f32_identical, find_nonfinite_f64, Exactness};
use crate::ml::features::FEATURE_DIMENSION;
use crate::ml::shadow::{ModelEnsemblePredictor, ShadowCandidate, ShadowDecision, ShadowEngine};
use crate::outcome::Outcome;

use super::error::OfflineGateError;
use super::report::{
    FailureAuthority, GateConfig, HoldoutSummary, RecordedDecision, ReplayEvidence,
    RetentionAblation, RetentionProof, ServedIdentity, TerminalAgreement,
};

// ---------------------------------------------------------------------------
// 3/4. one decision
// ---------------------------------------------------------------------------

/// Refuse a decision/Outcome pair whose identities disagree.
///
/// The gate converts the Outcome into canonical samples itself rather than
/// through [`DatasetStore::ingest`](crate::ml::dataset::DatasetStore::ingest),
/// so it does not inherit that path's correlation guard. This is that guard,
/// applied to the retained pair before the replay and before any label exists.
///
/// The *request* id is always present on both sides, so it is compared
/// unconditionally: an Outcome for another request would be attributed to this
/// decision in full. The *decision* id is optional on the Outcome — a request
/// that bypassed routing records none — so it is compared only when the record
/// names one; an Outcome that names a different decision is foreign even if the
/// request id happens to match.
fn require_outcome_identity(
    recorded: &RecordedDecision,
    decision_id: &str,
) -> Result<(), OfflineGateError> {
    let outcome = &recorded.outcome;
    let request_id = recorded.decision.actual.request_id.as_str();
    if outcome.request_id != request_id {
        return Err(OfflineGateError::OutcomeDecisionMismatch {
            decision_id: decision_id.to_string(),
            outcome_id: outcome.outcome_id.clone(),
            detail: format!(
                "it is correlated to request '{}' but the decision being replayed is request \
                 '{request_id}'",
                outcome.request_id
            ),
        });
    }
    if let Some(outcome_decision_id) = outcome.decision_id.as_deref() {
        if outcome_decision_id != decision_id {
            return Err(OfflineGateError::OutcomeDecisionMismatch {
                decision_id: decision_id.to_string(),
                outcome_id: outcome.outcome_id.clone(),
                detail: format!(
                    "it names decision '{outcome_decision_id}' but the decision being replayed is \
                     '{decision_id}'"
                ),
            });
        }
    }
    Ok(())
}

/// Replay one recorded decision and establish everything the gate claims about
/// it.
pub(super) fn replay_one(
    engine: &ShadowEngine,
    predictor: &ModelEnsemblePredictor,
    recorded: &RecordedDecision,
    config: &GateConfig,
) -> Result<ReplayEvidence, OfflineGateError> {
    let decision_id = recorded.input().decision_id.clone();
    let recorded_decision = &recorded.decision;

    // The pair is checked before anything else, because every claim below
    // treats the Outcome as the terminal fact about *this* decision.
    require_outcome_identity(recorded, &decision_id)?;

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
    exactness
        .into_divergence()
        .map_err(|divergence| OfflineGateError::ReplayDivergence {
            decision_id: decision_id.clone(),
            divergence: Box::new(divergence),
        })?;

    // Retention completeness, through the accepted function, so the refusal
    // vocabulary for an incomplete retained input is 7E-2D's.
    let complete_for_outcome = canonical_samples_from_decision_time(
        &recorded.outcome,
        recorded.input(),
        DataOrigin::Native,
    )
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
    compare_candidate_predictions(&mut exactness, &recorded.candidates, &replayed.candidates);
    compare_candidate_utilities(&mut exactness, &recorded.candidates, &replayed.candidates);

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
            (
                "prediction.success.value",
                candidate.prediction.success.value,
            ),
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
        let Some(replayed) =
            engine.evaluate_with(&recorded.decision.actual.request_id, &perturbed, predictor)
        else {
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
pub(super) fn build_holdout(
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
        let replay = evidence
            .get(index)
            .ok_or_else(|| OfflineGateError::RetainedInputAbsent {
                decision_id: decision_id.clone(),
                reason: "the replay evidence for this decision is missing".to_string(),
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

/// How many independent decisions the holdout covers.
///
/// The identity is the decision's request plus its decision id, both read from
/// the retained pair, and the count is a set: a decision that appears twice in
/// the recorded set is the same decision counted twice, not twice the evidence.
/// Canonical rows are deliberately not the unit — one decision projects to a
/// request row plus one row per attempt, so a row count overstates the
/// independent decisions by that factor.
pub(super) fn holdout_decision_count(recorded: &[RecordedDecision]) -> usize {
    recorded
        .iter()
        .map(|entry| {
            (
                entry.decision.actual.request_id.as_str(),
                entry.input().decision_id.as_str(),
            )
        })
        .collect::<BTreeSet<_>>()
        .len()
}

pub(super) fn summarise_holdout(
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
        decisions: holdout_decision_count(recorded),
        attributed: evidence
            .iter()
            .filter(|entry| entry.served.is_some() && entry.recorded_terminal.is_success())
            .count(),
        served_identities: served_identities.len(),
        overlap_with_fit_set: 0,
    }
}
