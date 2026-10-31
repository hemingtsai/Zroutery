//! One effective tariff resolver for previews, routing, execution and accounting.
//! A public catalogue row is evidence, not a verified account/key invoice.

use serde::{Deserialize, Serialize};

use super::Pricing;
use crate::config::{AccountConfig, ModelEntry, ProviderConfig};
use crate::ir::Dialect;

pub const RATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateSource {
    PublicCatalogue,
    VerifiedEffective,
    ManualOverride,
    ManualFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveRate {
    pub schema_version: u32,
    pub id: String,
    pub upstream_model: String,
    pub dialect: Dialect,
    pub binding_version: String,
    pub inference_key_id: Option<String>,
    pub group: Option<String>,
    pub source: RateSource,
    pub observed_at: i64,
    pub expires_at: Option<i64>,
    pub evidence: String,
    pub pricing: Pricing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateQuote {
    pub schema_version: u32,
    pub rate_id: Option<String>,
    pub source: RateSource,
    pub estimated: bool,
    pub binding_version: Option<String>,
    pub group: Option<String>,
    pub pricing: Pricing,
    pub missing: Vec<String>,
}

pub fn resolve_rate(
    provider: &ProviderConfig,
    model: &ModelEntry,
    account: Option<&AccountConfig>,
    key: Option<&str>,
    now: i64,
) -> Option<RateQuote> {
    let version = account
        .and_then(|account| account.binding.as_ref())
        .map(|binding| binding.version());
    let rate = account
        .into_iter()
        .flat_map(|account| &account.effective_rates)
        .filter(|rate| {
            rate.schema_version == RATE_SCHEMA_VERSION
                && rate.upstream_model == model.upstream_model
                && rate.dialect == provider.dialect()
                && version.as_deref() == Some(rate.binding_version.as_str())
                && rate
                    .inference_key_id
                    .as_deref()
                    .is_none_or(|id| Some(id) == key)
                && rate.expires_at.is_none_or(|expires| now <= expires)
                && rate.pricing.problems().is_empty()
                && !matches!(rate.source, RateSource::PublicCatalogue)
                && (rate.group.is_some()
                    || matches!(
                        rate.source,
                        RateSource::ManualOverride | RateSource::ManualFallback
                    ))
        })
        .max_by_key(|rate| {
            (
                match rate.source {
                    RateSource::ManualOverride => 4,
                    RateSource::VerifiedEffective => 3,
                    RateSource::ManualFallback => 2,
                    RateSource::PublicCatalogue => 0,
                },
                rate.inference_key_id.is_some(),
                rate.observed_at,
            )
        });
    if let Some(rate) = rate {
        return Some(RateQuote {
            schema_version: RATE_SCHEMA_VERSION,
            rate_id: Some(rate.id.clone()),
            source: rate.source,
            estimated: rate.source == RateSource::ManualFallback,
            binding_version: version,
            group: rate.group.clone(),
            pricing: rate.pricing.clone(),
            missing: cache_missing(&rate.pricing),
        });
    }
    // The existing explicit model price is the operator's shared fallback,
    // never a verified account group rate. No price means unknown, not free.
    model
        .pricing
        .as_ref()
        .filter(|price| price.problems().is_empty())
        .map(|price| RateQuote {
            schema_version: RATE_SCHEMA_VERSION,
            rate_id: None,
            source: RateSource::ManualFallback,
            estimated: true,
            binding_version: version,
            group: None,
            pricing: price.clone(),
            missing: cache_missing(price),
        })
}

fn cache_missing(price: &Pricing) -> Vec<String> {
    let mut missing = Vec::new();
    if price.per_request.is_none() {
        if price.cache_read_per_mtok.is_none() {
            missing.push("cache read uses explicit model-input fallback (estimate)".into());
        }
        if price.cache_write_per_mtok.is_none() {
            missing
                .push("cache write uses explicit model-input fallback (estimate), not free".into());
        }
    }
    missing
}
