//! Allowlisted per-inference usage. No prompts, outputs, signatures or tokens.

use serde::{Deserialize, Serialize};

use super::Usage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageIteration {
    pub kind: String,
    /// Omitted means the request's model, not an inferred response alias.
    pub model: Option<String>,
    pub usage: Usage,
    pub cache_write_5m_tokens: Option<u32>,
    pub cache_write_1h_tokens: Option<u32>,
    /// A documented refusal is not always billed; absent means unknown.
    pub billed: Option<bool>,
    /// Snapshot-derived estimates, never a provider-reported debit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<crate::billing::Cost>,
    #[serde(default)]
    pub cost_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_quote: Option<crate::billing::RateQuote>,
}

pub(crate) fn iteration_total(iterations: &[UsageIteration]) -> Option<Usage> {
    iterations
        .iter()
        .try_fold(Usage::default(), |mut total, iteration| {
            total.input_tokens = total
                .input_tokens
                .checked_add(iteration.usage.input_tokens)?;
            total.output_tokens = total
                .output_tokens
                .checked_add(iteration.usage.output_tokens)?;
            total.cache_read_tokens = total
                .cache_read_tokens
                .checked_add(iteration.usage.cache_read_tokens)?;
            total.cache_write_tokens = total
                .cache_write_tokens
                .checked_add(iteration.usage.cache_write_tokens)?;
            total.reasoning_tokens = total
                .reasoning_tokens
                .checked_add(iteration.usage.reasoning_tokens)?;
            Some(total)
        })
}
