//! The ML serving path: an active model, a ranking, and a fallback that always
//! works.
//!
//! # What this reverses
//!
//! The repository previously provisioned ML *out* of the serving path. ADR-0002
//! recorded a provisional boundary that forbade online learning in production,
//! `ml::activation` was built to be "complete and unreachable at the same time",
//! and a binary-level gate scanned the shipped desktop executable for ML
//! symbols and failed if it found them. The reasoning was that an unproven model
//! must not be able to route a real request.
//!
//! That reasoning is right about the danger and wrong about the remedy. It
//! produced a system that could never produce the evidence that would make the
//! danger manageable, so the risk never went down; it just went unmeasured while
//! a very large amount of infrastructure was built to keep the model away from
//! production. The remedy for an unproven model is a gated one with a fallback
//! and a rollback, not an absent one.
//!
//! So the boundary is replaced by a mechanism:
//!
//! * a model serves only after a [`super::promotion::PromotionGate`] promoted
//!   it, and the promotion's digest is recorded beside it;
//! * every ranking is recomputed from immutable decision-time facts, through the
//!   same [`DecisionEngine`] that decides everything else;
//! * any fault, any missing model, any disagreement between the ranking and the
//!   executable plan falls back to the deterministic plan the router already
//!   computed;
//! * promotion and rollback are durable and auditable.
//!
//! # The fallback is not an error path
//!
//! [`MlRouter::rank`] returns `Err` and the caller keeps the router's own order.
//! A model that cannot load, a checkpoint that does not verify, a prediction that
//! is not finite, a decision that names a candidate the plan does not contain —
//! every one of these is a normal, expected condition that costs a request its
//! ML ranking and nothing else. None of them can fail a request, because the
//! deterministic order was computed before ML was consulted and is still there.

/// File holding the active model pointer and its checkpoint.
pub const ACTIVE_MODEL_FILE: &str = "active-model.json";
/// Append-only record of every promotion and rollback.
pub const ACTIVE_MODEL_AUDIT_FILE: &str = "active-model-audit.jsonl";

/// Pointer envelope version.
pub const ACTIVE_MODEL_SCHEMA_VERSION: u32 = 1;

mod active;
mod router;

pub use active::*;
pub use router::*;

#[cfg(test)]
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests;
