//! Rectifier for providers that reject a thinking/reasoning budget.
//!
//! The fix halves the requested budget (with a floor of 1024) so the request
//! fits inside the provider's allowance.

use serde_json::Value;

use super::{error_text, Rectifier, RectifyResult};
use crate::error::Error;

pub struct ThinkingBudgetRectifier;

const MIN_BUDGET: u64 = 1024;

impl Rectifier for ThinkingBudgetRectifier {
    fn should_apply(&self, error: &Error, _body: &Value) -> bool {
        let msg = error_text(error);
        (msg.contains("budget") && (msg.contains("thinking") || msg.contains("reasoning")))
            || msg.contains("maximum thinking budget")
            || msg.contains("thinking budget must")
    }

    fn rectify(&self, body: &mut Value) -> RectifyResult {
        let Some(thinking) = body.get_mut("thinking").and_then(Value::as_object_mut) else {
            return RectifyResult {
                applied: false,
                details: "no top-level thinking configuration".to_string(),
            };
        };
        let Some(budget) = thinking.get("budget_tokens").and_then(Value::as_u64) else {
            return RectifyResult {
                applied: false,
                details: "no budget_tokens to reduce".to_string(),
            };
        };
        let reduced = (budget / 2).max(MIN_BUDGET);
        if reduced == budget {
            return RectifyResult {
                applied: false,
                details: "budget is already at the minimum".to_string(),
            };
        }
        thinking.insert("budget_tokens".into(), Value::from(reduced));
        RectifyResult {
            applied: true,
            details: format!("reduced thinking budget from {budget} to {reduced}"),
        }
    }

    fn name(&self) -> &'static str {
        "thinking_budget"
    }
}
