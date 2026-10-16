//! What was decided, and the trace that lets someone check the reasoning after the fact.

use super::capabilities::{bounded_trace_text, canonical_capabilities, capability_label};
use super::profile::TaskProfile;
use super::scoring::ScoreBreakdown;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------- Decision Trace

/// Serializable summary of a [`TaskProfile`], suitable for embedding in
/// decision traces.  All fields are owned (no borrows).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskProfileSummary {
    pub complexity: String,
    pub task_type: String,
    pub context_tokens: u64,
    pub estimated_output_tokens: u64,
    pub streaming: bool,
    pub has_tools: bool,
    pub has_vision: bool,
    pub required_capabilities: Vec<String>,
}

impl From<&TaskProfile> for TaskProfileSummary {
    fn from(p: &TaskProfile) -> Self {
        TaskProfileSummary {
            complexity: format!("{:?}", p.complexity).to_lowercase(),
            task_type: format!("{:?}", p.task_type).to_lowercase(),
            context_tokens: p.context_tokens,
            estimated_output_tokens: p.estimated_output_tokens,
            streaming: p.streaming,
            has_tools: p.has_tools,
            has_vision: p.has_vision,
            required_capabilities: canonical_capabilities(&p.required_capabilities)
                .into_iter()
                .map(capability_label)
                .map(str::to_string)
                .collect(),
        }
    }
}

/// Per-candidate evaluation in a routing decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateDecision {
    pub model_id: String,
    pub provider_id: String,
    pub tier: Option<String>,
    pub eligible: bool,
    /// Why this candidate was rejected (if not eligible).
    pub rejection: Option<String>,
    /// Score breakdown (if eligible).
    pub score: Option<ScoreBreakdown>,
    pub final_score: Option<f64>,
}

impl CandidateDecision {
    /// A bounded, deterministic reason token for this candidate.
    ///
    /// Eligible candidates use the fixed `eligible` token rather than leaving
    /// evidence implicit; rejected candidates retain their fixed policy
    /// reasons. No provider response body or request content is ever included.
    pub fn trace_reason(&self) -> String {
        self.rejection
            .as_deref()
            .map(bounded_trace_text)
            .unwrap_or_else(|| "eligible".to_string())
    }
}

/// The identity selected before any provider attempt is made.
///
/// This is deliberately separate from last-attempted and final-served
/// identities. The latter belong to the later request lifecycle owner and are
/// not inferred here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlannedIdentity {
    pub model_id: String,
    pub provider_id: String,
    pub tier: Option<String>,
}

impl PlannedIdentity {
    pub fn new(
        model_id: impl Into<String>,
        provider_id: impl Into<String>,
        tier: Option<String>,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            provider_id: provider_id.into(),
            tier,
        }
    }
}

/// Why a specific model was selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionReason {
    /// Direct model resolution (client specified exact model).
    Direct,
    /// Policy selected the highest-scoring eligible candidate.
    PolicySelected,
    /// Fallback after primary candidate failed.
    Fallback { from: String, reason: String },
    /// Escalation to higher tier.
    Escalated { from_tier: String },
    /// Degradation to lower tier.
    Degraded { from_tier: String },
    /// No eligible candidate found.
    NoCandidate,
}

/// What a learned model did to a routing decision, when one was consulted.
///
/// Additive and optional: a decision this router produced without a model has
/// none, which is the state a fresh installation and every non-`ml` build are in.
///
/// It exists because `DecisionReason` is a closed vocabulary of *why policy
/// chose what it chose*, and "a model re-ordered this afterwards" is not one of
/// those things. Overwriting `reason` would have been the easy option and would
/// have destroyed the policy's own account of itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MlRankingTrace {
    /// The commit that produced the ranking.
    pub commit_id: String,
    /// The candidate ML would serve first.
    pub selected: String,
    /// The engine's own reason, verbatim.
    pub reason: String,
    /// Whether an exploration draw moved the selection off the exploitation pick.
    pub explored: bool,
    /// Whether a model produced this trace at all.
    ///
    /// `false` means the router deliberately tried a different eligible candidate
    /// to gather evidence about it, with no learned basis for the choice and no
    /// model attached. Recorded rather than inferred from an empty `commit_id`,
    /// because "the model chose this" and "the router went to find out" are
    /// different facts and every consumer of this trace has to be able to tell
    /// them apart.
    ///
    /// Defaults to `true`, which is right for every trace written before this
    /// field existed: those were all model rankings.
    #[serde(default = "modelled_by_default")]
    pub modelled: bool,
    /// Candidate identities in the order ML would try them.
    pub order: Vec<String>,
    /// The order the router produced, for comparison.
    pub baseline_order: Vec<String>,
}

/// The value [`MlRankingTrace::modelled`] takes when it is absent from a stored
/// document. See the field: every trace predating it was a model ranking.
fn modelled_by_default() -> bool {
    true
}

impl MlRankingTrace {
    /// Whether the model changed the first candidate.
    pub fn changed_selection(&self) -> bool {
        self.baseline_order.first().map(String::as_str) != self.order.first().map(String::as_str)
    }
}

/// Identifies the exact policy configuration used for a routing decision.
///
/// Captures the policy id, enabled state, and hashes of the requirements and
/// preference structs so that decisions can be correlated with the exact
/// configuration version that produced them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRevision {
    /// The id of the policy that was applied.
    pub policy_id: String,
    /// Whether the policy was enabled at decision time.
    pub policy_enabled: bool,
    /// Hash of the [`PolicyRequirements`] struct.
    pub requirements_hash: u64,
    /// Hash of the [`PolicyPreference`] struct.
    pub preference_hash: u64,
}

/// Compute a deterministic `u64` hash of any [`Hash`](std::hash::Hash)-able value.
pub fn hash_to_u64<T: std::hash::Hash>(item: &T) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    item.hash(&mut hasher);
    hasher.finish()
}

/// Complete routing decision for a single request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDecision {
    /// Unique id for this decision (for correlation).
    pub decision_id: String,
    /// Timestamp when the decision was made.
    pub timestamp: i64,
    /// The task profile derived from the request.
    pub task: TaskProfileSummary,
    /// The policy that was applied.
    pub policy_id: String,
    /// Client profile that matched (if any).
    pub client_id: Option<String>,
    /// Per-candidate evaluation.
    pub candidates: Vec<CandidateDecision>,
    /// The selected candidate (model id). This is the initial planned
    /// selection, before any runtime attempt or fallback.
    pub selected: Option<String>,
    /// Fallback chain: models tried before the final selection.
    pub fallback_chain: Vec<String>,
    /// Why this decision was made.
    pub reason: DecisionReason,
    /// Identifies the exact policy/config version used.
    pub policy_revision: PolicyRevision,
    /// What a learned model did to this decision, when one was consulted.
    ///
    /// `None` for every decision this router made on its own, which is every
    /// decision in a build without the `ml` feature and every decision in a
    /// deployment that has not enabled `ml_routing`. Present means a model was
    /// consulted, not that it changed anything — a model that agreed with the
    /// router is recorded with the same fidelity as one that did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ml_ranking: Option<MlRankingTrace>,
}

impl RouteDecision {
    /// Return the identity selected during planning, if it is present in the
    /// candidate evidence. This is not a last-attempted or served identity.
    pub fn planned_identity(&self) -> Option<PlannedIdentity> {
        let selected = self.selected.as_deref()?;
        self.candidates
            .iter()
            .find(|candidate| candidate.model_id == selected)
            .map(|candidate| {
                PlannedIdentity::new(
                    candidate.model_id.clone(),
                    candidate.provider_id.clone(),
                    candidate.tier.clone(),
                )
            })
    }

    /// Explicit alias for callers that need to document the lifecycle
    /// boundary without changing the legacy `selected` field.
    pub fn planned_selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
}
