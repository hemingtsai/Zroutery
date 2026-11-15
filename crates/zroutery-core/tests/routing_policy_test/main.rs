//! Stage 3 routing conformance tests.
//!
//! These tests exercise the full routing policy surface area:
//! - TaskProfile derivation from ChatRequest
//! - Complexity -> suggested_tier mapping
//! - Policy matcher evaluation with multiple conditions
//! - Eligibility (PolicyRequirements) checks
//! - Scoring (PolicyPreference) ranking
//! - Client profile resolution
//! - PolicyConfig serde round-trip

mod client_profile;
mod decision_trace;
mod e2e_decision;
mod eligibility;
mod fallback;
mod matrix;
mod policy_config;
mod policy_match;
mod scoring;
mod support;
mod task_profile;
