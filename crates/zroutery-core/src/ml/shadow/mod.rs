//! Shadow decision infrastructure for ML routing (Stage 7E-1).
//!
//! Records what the ML routing stack *would have done* for policy-routed
//! main traffic — without influencing production routing. Every
//! [`ShadowDecision`] correlates a [`ProductionDecisionRef`] (what production
//! actually did) with a [`ShadowVerdict`] (the hypothetical ML decision) plus
//! the per-candidate [`ShadowCandidate`] evidence that produced it.
//!
//! Design invariants:
//!
//! - The engine never touches runtime stores: all inputs arrive as an
//!   immutable [`ShadowInput`] snapshot.
//! - A shadow verdict is evidence only — it can never become a routing
//!   action.
//! - Faults are contained: a panicking predictor or failing step increments
//!   the fault counter, logs one non-sensitive diagnostic line, and returns
//!   `None`; production continues either way.
//! - The shadow ensemble is content-addressed: [`ShadowEngine::train`]
//!   advances a deterministic commit chain equivalent to replaying all
//!   samples from genesis.

mod engine;
mod predictor;
mod records;
mod store;

pub use engine::*;
pub use predictor::*;
pub use records::*;
pub use store::*;

#[cfg(test)]
mod tests;
