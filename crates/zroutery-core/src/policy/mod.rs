//! Routing policy engine.
//!
//! A [`RoutingPolicy`] defines *why* a request should land on a particular
//! model.  Policies sit between request analysis and candidate selection:
//!
//! ```text
//! Request → TaskProfile → PolicyMatcher → PolicyRequirements → Candidate
//! ```
//!
//! Each policy has:
//! - **Matchers**: conditions that determine whether the policy applies to a
//!   given request.
//! - **Requirements**: hard constraints that candidates must satisfy.
//! - **Preference**: soft scoring weights for ranking eligible candidates.
//! - **Fallback**: what to do when no candidate is eligible.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`profile`] | [`TaskProfile`] — what Zroutery thinks the task is, from its complexity and kind |
//! | [`policy`] | [`RoutingPolicy`], its matchers, requirements, preferences, fallbacks, and the [`MatchContext`] they are matched against |
//! | [`eligibility`] | [`RejectionReason`] — whether a candidate may be used at all, and the one reason it may not |
//! | [`scoring`] | ranking the eligible candidates, plus [`default_policy`] |
//! | [`clients`] | [`resolve_client`] — the per-client profile override, and the config that carries it |
//! | [`decision`] | [`RouteDecision`] and the trace that lets someone check the reasoning afterwards |
//! | [`capabilities`] | capability normalization and trace-text bounding, the two small jobs the other modules call into |
//!
//! The order above is the order a request travels: profile, then the policy
//! matched against it, then eligibility, then scoring. `decision` is the only
//! module that knows all four, which is why it is the only one that does.
//!
//! # What is public, and why
//!
//! Everything is re-exported here, so `policy::X` resolves exactly as it did
//! when this was one file and no caller moved.

mod capabilities;
mod clients;
mod decision;
mod eligibility;
// `policy::policy` reads oddly, and the alternative is worse: the file is named
// for its subject, `RoutingPolicy`, and calling it `rule` or `types` would name
// it after nothing. The parent module is the engine, the child is the policy.
#[allow(clippy::module_inception)]
mod policy;
mod profile;
mod scoring;

#[cfg(test)]
mod tests;

pub use capabilities::{canonical_capabilities, capability_label};

/// Bounded so a caller cannot put an unbounded string in a stored trace.
pub(crate) use capabilities::bounded_trace_text;
pub use clients::{resolve_client, ClientContext, ClientMatcher, ClientProfile, PolicyConfig};
pub use decision::{
    hash_to_u64, CandidateDecision, DecisionReason, MlRankingTrace, PlannedIdentity,
    PolicyRevision, RouteDecision, TaskProfileSummary,
};
pub use eligibility::{EligibilityCheck, RejectionReason};
pub use policy::{
    MatchContext, PolicyFallback, PolicyMatcher, PolicyPreference, PolicyRequirements,
    RoutingPolicy,
};
pub use profile::{Complexity, TaskProfile, TaskType};
pub use scoring::{
    default_policy, score_candidate, ScoreBreakdown, ScoredCandidate, ScoringContext,
};
