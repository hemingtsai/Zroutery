use serde::{Deserialize, Serialize};

use crate::config::ModelTier;
use crate::ml::coordinator::RoutingAction;
use crate::ml::features::{
    extract_features, FeatureContext, RoutingFeatures, FEATURE_SCHEMA_VERSION,
};
use crate::ml::model_identity::CommitId;
use crate::ml::reward::{PredictionBundle, UtilityBreakdown};
use crate::observation::ObservationStore;
use crate::policy::{PolicyRevision, RouteDecision, TaskProfile, TaskProfileSummary};
use crate::router::Candidate;
use crate::session::SessionRoutingMode;
use crate::stats_ext::StatsStore;

// ---------------------------------------------------------------------------
// FNV-1a checksum helpers (pattern from model_identity::content_hash)
// ---------------------------------------------------------------------------

/// FNV-1a offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
/// FNV-1a prime.
const FNV_PRIME: u64 = 0x100000001b3;

/// Mix `bytes` into `hash` following the FNV-1a pattern used by
/// [`ModelCheckpoint::content_hash`](super::model_identity::ModelCheckpoint::content_hash).
pub(crate) fn fnv_mix_u64(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= u64::from(*b);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// Mix a stable, quantized representation of an `f32` into `hash`.
///
/// Values in the normal ML range are quantized to 1e-12 before hashing. This
/// removes harmless last-bit differences introduced by serializing a computed
/// utility and parsing it again, without allocating on the evaluation hot
/// path. Values outside that range retain their exact bit pattern; the JSON
/// representation of those finite values is already round-trip exact, and
/// non-finite values are rejected before a record is stored.
fn fnv_mix_f32(hash: &mut u64, value: f32) {
    if value == 0.0 {
        fnv_mix_u64(hash, &0i64.to_le_bytes());
    } else if value.is_finite() && value.abs() <= 1_000_000.0 {
        let quantized = (f64::from(value) * 1e12).round() as i64;
        fnv_mix_u64(hash, &quantized.to_le_bytes());
    } else {
        fnv_mix_u64(hash, &value.to_bits().to_le_bytes());
    }
}

/// Mix a stable, quantized representation of an `f64` into `hash` using the
/// same replay precision as [`fnv_mix_f32`].
fn fnv_mix_f64(hash: &mut u64, value: f64) {
    if value == 0.0 {
        fnv_mix_u64(hash, &0i64.to_le_bytes());
    } else if value.is_finite() && value.abs() <= 1_000_000.0 {
        let quantized = (value * 1e12).round() as i64;
        fnv_mix_u64(hash, &quantized.to_le_bytes());
    } else {
        fnv_mix_u64(hash, &value.to_bits().to_le_bytes());
    }
}

/// Mix a length-delimited string into `hash`.
///
/// Length delimiting matters here because the input record contains several
/// adjacent identity strings. Without it, moving bytes from one identity into
/// the next could produce the same byte stream.
fn fnv_mix_string(hash: &mut u64, value: &str) {
    fnv_mix_u64(hash, &(value.len() as u64).to_le_bytes());
    fnv_mix_u64(hash, value.as_bytes());
}

/// Mix an optional string into `hash`, preserving the distinction between an
/// absent value and an empty value.
fn fnv_mix_optional_string(hash: &mut u64, value: Option<&str>) {
    match value {
        Some(value) => {
            fnv_mix_u64(hash, &[1]);
            fnv_mix_string(hash, value);
        }
        None => fnv_mix_u64(hash, &[0]),
    }
}

/// Mix the policy revision used to produce a production decision.
fn fnv_mix_policy_revision(hash: &mut u64, revision: &PolicyRevision) {
    fnv_mix_string(hash, &revision.policy_id);
    fnv_mix_u64(hash, &[u8::from(revision.policy_enabled)]);
    fnv_mix_u64(hash, &revision.requirements_hash.to_le_bytes());
    fnv_mix_u64(hash, &revision.preference_hash.to_le_bytes());
}

/// Mix the task identity/summary used to produce a production decision.
fn fnv_mix_task_summary(hash: &mut u64, task: &TaskProfileSummary) {
    fnv_mix_string(hash, &task.complexity);
    fnv_mix_string(hash, &task.task_type);
    fnv_mix_u64(hash, &task.context_tokens.to_le_bytes());
    fnv_mix_u64(hash, &task.estimated_output_tokens.to_le_bytes());
    fnv_mix_u64(hash, &[u8::from(task.streaming)]);
    fnv_mix_u64(hash, &[u8::from(task.has_tools)]);
    fnv_mix_u64(hash, &[u8::from(task.has_vision)]);
    fnv_mix_u64(
        hash,
        &(task.required_capabilities.len() as u64).to_le_bytes(),
    );
    for capability in &task.required_capabilities {
        fnv_mix_string(hash, capability);
    }
}

/// Discriminant of [`ModelTier`] for checksums: 0 = none, 1..=4 = tiers.
fn tier_discriminant(tier: Option<ModelTier>) -> u8 {
    match tier {
        None => 0,
        Some(ModelTier::Fast) => 1,
        Some(ModelTier::Standard) => 2,
        Some(ModelTier::Reasoning) => 3,
        Some(ModelTier::Frontier) => 4,
    }
}

/// Discriminant of [`SessionRoutingMode`] for checksums.
fn session_mode_discriminant(mode: SessionRoutingMode) -> u8 {
    match mode {
        SessionRoutingMode::Free => 0,
        SessionRoutingMode::Sticky => 1,
        SessionRoutingMode::Pinned => 2,
    }
}

/// Discriminant of [`RoutingAction`] for checksums.
fn action_discriminant(action: RoutingAction) -> u8 {
    match action {
        RoutingAction::Keep => 0,
        RoutingAction::Switch => 1,
        RoutingAction::Explore => 2,
    }
}

/// Identity of the INPUT snapshot a shadow decision was computed from.
///
/// Every decision-time field which can affect the prediction or the
/// counterfactual is included: the model commit, policy/task identity, the
/// planned production selection, and every candidate's ordered identity,
/// eligibility/rejection evidence, feature schema, and feature bits. Volatile
/// correlation fields (timestamp, shadow id, request id, and decision id) are
/// intentionally excluded: replaying the same decision-time input must produce
/// the same checksum.
pub(super) fn shadow_input_checksum(input: &ShadowInput, model_commit: &CommitId) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    fnv_mix_string(&mut hash, model_commit.as_str());
    fnv_mix_string(&mut hash, &input.policy_id);
    fnv_mix_optional_string(&mut hash, input.client_id.as_deref());
    fnv_mix_policy_revision(&mut hash, &input.policy_revision);
    fnv_mix_task_summary(&mut hash, &input.task);
    fnv_mix_u64(&mut hash, &input.feature_schema.to_le_bytes());
    fnv_mix_string(&mut hash, &input.production_selected);
    fnv_mix_u64(&mut hash, &(input.candidates.len() as u64).to_le_bytes());
    for candidate in &input.candidates {
        fnv_mix_string(&mut hash, &candidate.candidate_id);
        fnv_mix_string(&mut hash, &candidate.provider_id);
        fnv_mix_u64(&mut hash, &[tier_discriminant(candidate.tier)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.eligible)]);
        fnv_mix_optional_string(&mut hash, candidate.rejection_reason.as_deref());
        fnv_mix_u64(&mut hash, &candidate.features.schema_version.to_le_bytes());
        for value in &candidate.features.values {
            fnv_mix_f32(&mut hash, *value);
        }
    }
    fnv_mix_u64(&mut hash, &[session_mode_discriminant(input.session_mode)]);
    fnv_mix_u64(&mut hash, &input.session_switch_count.to_le_bytes());
    fnv_mix_u64(&mut hash, &[u8::from(input.is_fallback)]);
    hash
}

/// Identity of the resulting decision: the input checksum plus the ordered
/// candidate evidence, explicit ML ranking, and verdict. The served identity is
/// intentionally not mixed here: it is not known at decision time in this pure
/// seam and must not change the replayable counterfactual.
pub(super) fn shadow_decision_checksum(
    input_checksum: u64,
    candidates: &[ShadowCandidate],
    ranked_candidates: &[String],
    selected: &str,
    action: RoutingAction,
    reason: &str,
) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    fnv_mix_u64(&mut hash, &input_checksum.to_le_bytes());
    fnv_mix_u64(&mut hash, &(candidates.len() as u64).to_le_bytes());
    for candidate in candidates {
        fnv_mix_string(&mut hash, &candidate.candidate_id);
        fnv_mix_string(&mut hash, &candidate.provider_id);
        fnv_mix_u64(&mut hash, &[tier_discriminant(candidate.tier)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.eligible)]);
        fnv_mix_u64(&mut hash, &[u8::from(candidate.valid)]);
        fnv_mix_optional_string(&mut hash, candidate.rejection_reason.as_deref());
        let bundle = &candidate.prediction;
        for prediction in [&bundle.success, &bundle.latency, &bundle.ttft, &bundle.cost] {
            fnv_mix_f64(&mut hash, prediction.value);
            fnv_mix_f64(&mut hash, prediction.confidence);
            fnv_mix_u64(&mut hash, &prediction.sample_count.to_le_bytes());
            fnv_mix_u64(&mut hash, &[u8::from(prediction.cold)]);
        }
        let utility = &candidate.utility;
        for component in [
            utility.success,
            utility.latency,
            utility.ttft,
            utility.cost,
            utility.fallback,
            utility.uncertainty,
            utility.switch_cost,
            utility.total,
        ] {
            fnv_mix_f64(&mut hash, component);
        }
    }
    fnv_mix_u64(&mut hash, &(ranked_candidates.len() as u64).to_le_bytes());
    for candidate_id in ranked_candidates {
        fnv_mix_string(&mut hash, candidate_id);
    }
    fnv_mix_string(&mut hash, selected);
    fnv_mix_u64(&mut hash, &[action_discriminant(action)]);
    fnv_mix_string(&mut hash, reason);
    hash
}

// ---------------------------------------------------------------------------
// Shadow scope / decision records
// ---------------------------------------------------------------------------

/// Explicit scoping of shadow mode (7E-1 covers policy-routed main traffic
/// only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowScope {
    PolicyRouted,
}

/// What production planned and, separately, what actually served.
///
/// `selected` is the initial `RouteDecision.selected` identity. It is not a
/// claim about the final response after runtime fallback. The pure shadow seam
/// has no final outcome input, so `served` is explicitly `None` until the
/// production integration node supplies that identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductionDecisionRef {
    pub request_id: String,
    pub decision_id: String,
    /// Initial planned selection, before runtime failover.
    pub selected: String,
    /// Final served model identity, or `None` when not integrated/known.
    #[serde(default)]
    pub served: Option<String>,
}

impl ProductionDecisionRef {
    /// Explicit name for the legacy `selected` field.
    pub fn planned_selected(&self) -> &str {
        &self.selected
    }

    /// Whether a final served identity has been supplied by an integration
    /// boundary. The pure ML seam intentionally returns `false`.
    pub fn has_served_identity(&self) -> bool {
        self.served.is_some()
    }
}

/// What the ML stack would have done (hypothetical — never a real routing
/// action).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowVerdict {
    pub model_commit: CommitId,
    pub feature_schema: u32,
    pub selected: String,
    pub action: RoutingAction,
    pub reason: String,
    /// Valid candidates in the explicit ML utility ranking used for the
    /// counterfactual. This is independent of production plan order.
    #[serde(default)]
    pub ranked_candidates: Vec<String>,
}

/// Per-candidate evidence captured during a shadow evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowCandidate {
    pub candidate_id: String,
    pub provider_id: String,
    pub tier: Option<ModelTier>,
    /// Hard eligibility as reported by production.
    pub eligible: bool,
    /// Whether the predictions were finite (usable evidence).
    pub valid: bool,
    /// Why this candidate was not considered (`None` when it was).
    pub rejection_reason: Option<String>,
    pub prediction: PredictionBundle,
    pub utility: UtilityBreakdown,
}

/// The immutable, serializable decision-time observation used to produce a
/// shadow verdict.
///
/// Keeping this as a separate object makes the boundary explicit: a decision
/// record can be round-tripped and replayed without consulting router/session
/// state. The input contains the complete ordered candidate snapshot and the
/// policy/task identity; the commit is pinned beside it so the exact model
/// artifact is part of the observation identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowObservation {
    pub input: ShadowInput,
    pub model_commit: CommitId,
}

impl Default for ShadowObservation {
    fn default() -> Self {
        Self {
            input: ShadowInput::default(),
            model_commit: CommitId::new(""),
        }
    }
}

/// One recorded shadow decision: production's planned identity, the explicit
/// served-identity state, the exact observation, the hypothetical ML verdict,
/// and the evidence between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowDecision {
    /// `shadow-<uuid simple>`.
    pub shadow_id: String,
    pub timestamp: i64,
    pub scope: ShadowScope,
    pub actual: ProductionDecisionRef,
    #[serde(default)]
    pub observation: ShadowObservation,
    pub shadow: ShadowVerdict,
    pub candidates: Vec<ShadowCandidate>,
    /// Identity of the INPUT snapshot.
    pub decision_input_checksum: u64,
    /// Identity of the resulting decision.
    pub decision_checksum: u64,
}

impl ShadowDecision {
    /// Borrow the exact decision-time input retained by this record.
    pub fn input(&self) -> &ShadowInput {
        &self.observation.input
    }
}

// ---------------------------------------------------------------------------
// Shadow input — immutable snapshot
// ---------------------------------------------------------------------------

/// One candidate as seen at decision time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowCandidateInput {
    pub candidate_id: String,
    pub provider_id: String,
    pub tier: Option<ModelTier>,
    pub eligible: bool,
    /// Exact feature vector captured for this candidate. A policy-only
    /// candidate (present in `RouteDecision` but absent from the executable
    /// plan) uses the explicit UNKNOWN vector and carries its rejection reason;
    /// it is never made executable.
    pub features: RoutingFeatures,
    /// Policy or execution rejection evidence from the production decision.
    #[serde(default)]
    pub rejection_reason: Option<String>,
}

/// Immutable input snapshot — the engine NEVER touches runtime stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowInput {
    pub decision_id: String,
    /// Policy identity and exact revision used by production.
    #[serde(default)]
    pub policy_id: String,
    /// Client-profile identity which selected the policy, when present.
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub policy_revision: PolicyRevision,
    /// Task identity/summary used by production at decision time.
    #[serde(default)]
    pub task: TaskProfileSummary,
    /// Initial planned selection. This is not the final served identity.
    pub production_selected: String,
    pub feature_schema: u32,
    pub candidates: Vec<ShadowCandidateInput>,
    pub session_mode: SessionRoutingMode,
    pub session_switch_count: u32,
    pub is_fallback: bool,
}

impl Default for ShadowInput {
    fn default() -> Self {
        Self {
            decision_id: String::new(),
            policy_id: String::new(),
            client_id: None,
            policy_revision: PolicyRevision::default(),
            task: TaskProfileSummary::default(),
            production_selected: String::new(),
            feature_schema: FEATURE_SCHEMA_VERSION,
            candidates: Vec::new(),
            session_mode: SessionRoutingMode::Free,
            session_switch_count: 0,
            is_fallback: false,
        }
    }
}

impl ShadowInput {
    /// Build the immutable snapshot for one shadow evaluation of a
    /// policy-routed request.
    ///
    /// Pure with respect to routing state BY SIGNATURE: the only inputs are
    /// read-only store handles plus the plan and [`RouteDecision`]
    /// production already computed. No [`AppState`](crate::server::AppState)
    /// and no [`Router`](crate::router::Router) is reachable here, so no
    /// ranking path can be invoked from this module — the type system
    /// enforces it, and an identical request produces an identical snapshot.
    ///
    /// Eligibility keying mirrors the decision trace: a candidate counts as
    /// eligible when the trace marked its exposed id eligible.
    pub fn from_policy_plan(
        observations: &ObservationStore,
        stats: &StatsStore,
        plan: &[Candidate],
        decision: &RouteDecision,
        task_profile: &TaskProfile,
        message_count: usize,
    ) -> Self {
        let mut candidates: Vec<ShadowCandidateInput> = plan
            .iter()
            .map(|candidate| {
                let entry = &candidate.entry;
                // Production ranks by the exposed id, and the decision trace
                // keys its per-candidate eligibility on the same identity.
                // Keep the route evidence alongside the executable-plan entry;
                // a policy-rejected candidate must not silently become
                // executable merely because it was present in the plan.
                let evidence = decision.candidates.iter().find(|candidate_decision| {
                    candidate_decision.model_id == candidate.exposed_id
                        && candidate_decision.provider_id == entry.provider_id
                });
                let eligible =
                    evidence.is_some_and(|candidate_decision| candidate_decision.eligible);
                let rejection_reason = evidence
                    .and_then(|candidate_decision| candidate_decision.rejection.clone())
                    .or_else(|| {
                        if evidence.is_none() {
                            Some("missing policy evidence".to_string())
                        } else if !eligible {
                            Some("policy rejected".to_string())
                        } else {
                            None
                        }
                    });
                let observation = observations.get(&candidate.exposed_id, &entry.provider_id);
                let stats = stats.get(&candidate.exposed_id, &entry.provider_id);
                let features = extract_features(&FeatureContext {
                    task: Some(task_profile),
                    message_count: Some(message_count),
                    tier: entry.tier,
                    capabilities: Some(&entry.capabilities),
                    priority: entry.priority,
                    observation: Some(&observation),
                    stats: Some(&stats),
                    #[cfg(feature = "account")]
                    account: None,
                });
                ShadowCandidateInput {
                    candidate_id: candidate.exposed_id.clone(),
                    provider_id: entry.provider_id.clone(),
                    tier: entry.tier,
                    eligible,
                    features,
                    rejection_reason,
                }
            })
            .collect();

        // `RouteDecision.candidates` is the authoritative policy evidence. It
        // can contain candidates which policy rejected and which therefore
        // never made it into the executable plan. Retain those identities and
        // rejection reasons as non-executable observations. The full model
        // entry is not available through this pure boundary, so their feature
        // vector is the explicit UNKNOWN snapshot rather than fabricated data.
        for candidate_decision in &decision.candidates {
            let already_present = candidates.iter().any(|candidate| {
                candidate.candidate_id == candidate_decision.model_id
                    && candidate.provider_id == candidate_decision.provider_id
            });
            if already_present {
                continue;
            }
            candidates.push(ShadowCandidateInput {
                candidate_id: candidate_decision.model_id.clone(),
                provider_id: candidate_decision.provider_id.clone(),
                tier: candidate_decision.tier.as_deref().and_then(|tier| {
                    ModelTier::ALL
                        .into_iter()
                        .find(|known| known.as_str() == tier)
                }),
                eligible: false,
                features: RoutingFeatures::default(),
                rejection_reason: Some(
                    candidate_decision
                        .rejection
                        .clone()
                        .unwrap_or_else(|| "not in executable plan".to_string()),
                ),
            });
        }

        Self {
            decision_id: decision.decision_id.clone(),
            policy_id: decision.policy_id.clone(),
            client_id: decision.client_id.clone(),
            policy_revision: decision.policy_revision.clone(),
            task: decision.task.clone(),
            production_selected: decision.selected.clone().unwrap_or_default(),
            feature_schema: FEATURE_SCHEMA_VERSION,
            candidates,
            // The session store is not wired into the production pipeline
            // yet, so shadow evaluation always sees a fresh session: free
            // mode, no switches yet, and this is the initial decision (not
            // a fallback).
            session_mode: SessionRoutingMode::Free,
            session_switch_count: 0,
            is_fallback: false,
        }
    }
}
