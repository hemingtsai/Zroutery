//! Per-provider/model/surface reasoning evidence. Manual corrections are explicit
//! and discovery cannot merge another relay's vocabulary into this contract.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningCapability {
    pub schema_version: u32,
    pub source: String,
    pub observed_at: i64,
    pub provider_id: String,
    pub upstream_model: String,
    pub dialect: crate::ir::Dialect,
    pub effort_levels: Vec<String>,
    pub token_budget: bool,
    /// Explicitly verified per-effort bridge to numeric budget (never a guess).
    #[serde(default)]
    pub effort_budgets: BTreeMap<String, u32>,
    #[serde(default)]
    pub manual: bool,
}
