//! Money: what a request cost, and how much credit a provider still has.
//!
//! Prices are entered by the user, per model, in the currency the provider bills
//! in. Nothing is guessed and nothing is converted between currencies: totals are
//! reported per currency, because a mix of USD and CNY has no meaningful sum.
//!
//! Balances have no cross-vendor standard, so a provider carries a *probe*: a
//! path plus JSON pointers into the response. Presets fill those in for the
//! providers that do offer an endpoint.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ir::Usage;

/// Price of one model, per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    /// Currency code as the provider bills it, e.g. `USD` or `CNY`.
    pub currency: String,
    /// Price per 1M input (prompt) tokens.
    pub input_per_mtok: f64,
    /// Price per 1M output (completion) tokens.
    pub output_per_mtok: f64,
    /// Price per 1M tokens served from the prompt cache. Falls back to the input
    /// price when unset, which is the conservative direction.
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    /// Price per 1M tokens written to the prompt cache.
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
}

impl Pricing {
    pub fn new(currency: impl Into<String>, input_per_mtok: f64, output_per_mtok: f64) -> Self {
        Pricing {
            currency: currency.into(),
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
        }
    }

    /// What this usage costs.
    ///
    /// Cached input tokens are billed at the cache read price and are *not* also
    /// billed as fresh input: every provider that reports `cached_tokens` counts
    /// them inside the prompt total. Reasoning tokens are already part of the
    /// output count for the same reason.
    pub fn cost_of(&self, usage: &Usage) -> Cost {
        let per_token = |price: f64, tokens: u32| price * tokens as f64 / 1_000_000.0;
        let cached = usage.cache_read_tokens.min(usage.input_tokens);
        let fresh_input = usage.input_tokens - cached;

        let amount = per_token(self.input_per_mtok, fresh_input)
            + per_token(
                self.cache_read_per_mtok.unwrap_or(self.input_per_mtok),
                cached,
            )
            + per_token(
                self.cache_write_per_mtok.unwrap_or(0.0),
                usage.cache_write_tokens,
            )
            + per_token(self.output_per_mtok, usage.output_tokens);

        Cost {
            currency: self.currency.clone(),
            amount,
        }
    }

    /// Cost of a prompt that has not been sent yet, from an estimated token count.
    pub fn estimate_input(&self, input_tokens: u32) -> Cost {
        Cost {
            currency: self.currency.clone(),
            amount: self.input_per_mtok * input_tokens as f64 / 1_000_000.0,
        }
    }

    /// Complaints about a price the user typed.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.currency.trim().is_empty() {
            out.push("has no currency".to_string());
        }
        for (name, value) in [
            ("input", Some(self.input_per_mtok)),
            ("output", Some(self.output_per_mtok)),
            ("cache read", self.cache_read_per_mtok),
            ("cache write", self.cache_write_per_mtok),
        ] {
            if let Some(v) = value {
                if v < 0.0 || !v.is_finite() {
                    out.push(format!("has an impossible {name} price"));
                }
            }
        }
        out
    }
}

/// An amount in one currency. Never added to an amount in another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub currency: String,
    pub amount: f64,
}

/// Sums kept apart by currency.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CostTotals(pub BTreeMap<String, f64>);

/// Six decimal places is far below the smallest billed unit of any provider and
/// still exact enough that repeated addition cannot drift visibly.
fn round_cents(amount: f64) -> f64 {
    (amount * 1_000_000.0).round() / 1_000_000.0
}

impl CostTotals {
    pub fn add(&mut self, cost: &Cost) {
        let entry = self.0.entry(cost.currency.clone()).or_insert(0.0);
        *entry = round_cents(*entry + cost.amount);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, currency: &str) -> f64 {
        self.0.get(currency).copied().unwrap_or(0.0)
    }
}

// ------------------------------------------------------------------- balances

/// How deep a provider's base URL already reaches.
///
/// A balance path has to be appended to it, and the two dialects disagree about
/// where the base stops: an OpenAI compatible base usually already ends in
/// `/v1`, while an Anthropic base is the API root. Billing takes this as a plain
/// fact rather than importing the provider type, which keeps the money code
/// independent of how providers are configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseDepth {
    /// The base URL is the API root, e.g. `https://relay.example.com`.
    ApiRoot,
    /// The base URL already carries the version, e.g. `.../v1`.
    Versioned,
}

/// Providers that publish a balance, plus an escape hatch.
///
/// OpenAI and Anthropic have no such endpoint; their consoles are the only place
/// the number exists, which is why `None` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalancePreset {
    #[default]
    None,
    DeepSeek,
    Moonshot,
    SiliconFlow,
    OpenRouter,
    /// Sub2API relay (`GET /v1/usage`), which reports a wallet balance, a key
    /// quota or a subscription allowance depending on how the key was issued.
    #[serde(rename = "sub2api")]
    Sub2Api,
    Custom,
}

impl BalancePreset {
    pub const ALL: [BalancePreset; 7] = [
        BalancePreset::None,
        BalancePreset::DeepSeek,
        BalancePreset::Moonshot,
        BalancePreset::SiliconFlow,
        BalancePreset::OpenRouter,
        BalancePreset::Sub2Api,
        BalancePreset::Custom,
    ];

    /// The built-in probe, if this preset has one.
    ///
    /// The depth matters because the path is appended to the provider's base URL,
    /// which may or may not already carry the version.
    pub fn probe_for(&self, depth: BaseDepth) -> Option<BalanceProbe> {
        match self {
            BalancePreset::None | BalancePreset::Custom => None,
            BalancePreset::DeepSeek => Some(BalanceProbe {
                // Note the lack of `/v1`: this one hangs off the API root.
                path: "https://api.deepseek.com/user/balance".into(),
                remaining_pointer: Some("/balance_infos/0/total_balance".into()),
                total_pointer: None,
                used_pointer: None,
                currency_pointer: Some("/balance_infos/0/currency".into()),
                currency: Some("CNY".into()),
            }),
            BalancePreset::Moonshot => Some(BalanceProbe {
                path: "/users/me/balance".into(),
                remaining_pointer: Some("/data/available_balance".into()),
                total_pointer: None,
                used_pointer: None,
                currency_pointer: None,
                currency: Some("CNY".into()),
            }),
            BalancePreset::SiliconFlow => Some(BalanceProbe {
                path: "/user/info".into(),
                remaining_pointer: Some("/data/totalBalance".into()),
                total_pointer: None,
                used_pointer: None,
                currency_pointer: None,
                currency: Some("CNY".into()),
            }),
            BalancePreset::OpenRouter => Some(BalanceProbe {
                path: "/credits".into(),
                remaining_pointer: None,
                total_pointer: Some("/data/total_credits".into()),
                used_pointer: Some("/data/total_usage".into()),
                currency_pointer: None,
                currency: Some("USD".into()),
            }),
            BalancePreset::Sub2Api => Some(BalanceProbe {
                // Every branch of its `/v1/usage` answer carries `remaining` and
                // `unit`; a quota bound key adds the limit and the amount used.
                path: match depth {
                    BaseDepth::Versioned => "/usage".into(),
                    BaseDepth::ApiRoot => "/v1/usage".into(),
                },
                remaining_pointer: Some("/remaining".into()),
                total_pointer: Some("/quota/limit".into()),
                used_pointer: Some("/quota/used".into()),
                currency_pointer: Some("/unit".into()),
                currency: Some("USD".into()),
            }),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            BalancePreset::None => "not supported",
            BalancePreset::DeepSeek => "DeepSeek",
            BalancePreset::Moonshot => "Moonshot",
            BalancePreset::SiliconFlow => "SiliconFlow",
            BalancePreset::OpenRouter => "OpenRouter",
            BalancePreset::Sub2Api => "Sub2API",
            BalancePreset::Custom => "custom",
        }
    }
}

/// Where to ask for a balance and how to read the answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceProbe {
    /// Absolute URL, or a path appended to the provider's base url.
    pub path: String,
    /// JSON pointer to the remaining credit.
    #[serde(default)]
    pub remaining_pointer: Option<String>,
    /// Pointer to the granted total, when the payload reports total and used.
    #[serde(default)]
    pub total_pointer: Option<String>,
    #[serde(default)]
    pub used_pointer: Option<String>,
    /// Pointer to a currency code inside the payload.
    #[serde(default)]
    pub currency_pointer: Option<String>,
    /// Currency to report when the payload does not carry one.
    #[serde(default)]
    pub currency: Option<String>,
}

impl Default for BalanceProbe {
    fn default() -> Self {
        BalanceProbe {
            path: "/user/balance".into(),
            remaining_pointer: Some("/balance".into()),
            total_pointer: None,
            used_pointer: None,
            currency_pointer: None,
            currency: None,
        }
    }
}

/// Per provider balance settings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BalanceConfig {
    #[serde(default)]
    pub preset: BalancePreset,
    /// Used when `preset` is [`BalancePreset::Custom`].
    #[serde(default)]
    pub custom: Option<BalanceProbe>,
}

impl BalanceConfig {
    pub fn probe(&self, depth: BaseDepth) -> Option<BalanceProbe> {
        match self.preset {
            BalancePreset::Custom => self.custom.clone(),
            other => other.probe_for(depth),
        }
    }

    pub fn is_supported(&self, depth: BaseDepth) -> bool {
        self.probe(depth).is_some()
    }
}

/// What a provider answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    /// Credit left, when the provider says so.
    pub remaining: Option<f64>,
    pub total: Option<f64>,
    pub used: Option<f64>,
}

impl Balance {
    /// Read a payload according to a probe.
    pub fn from_payload(probe: &BalanceProbe, payload: &Value) -> Option<Balance> {
        let remaining = probe
            .remaining_pointer
            .as_deref()
            .and_then(|p| number_at(payload, p));
        let total = probe
            .total_pointer
            .as_deref()
            .and_then(|p| number_at(payload, p));
        let used = probe
            .used_pointer
            .as_deref()
            .and_then(|p| number_at(payload, p));

        let remaining = remaining.or(match (total, used) {
            (Some(t), Some(u)) => Some(t - u),
            _ => None,
        });
        if remaining.is_none() && total.is_none() && used.is_none() {
            return None;
        }

        let currency = probe
            .currency_pointer
            .as_deref()
            .and_then(|p| payload.pointer(p))
            .and_then(Value::as_str)
            .map(str::to_uppercase)
            .or_else(|| probe.currency.clone())
            .unwrap_or_else(|| "?".to_string());

        Some(Balance {
            currency,
            remaining,
            total,
            used,
        })
    }
}

/// Numbers arrive as JSON numbers or as decimal strings, depending on the vendor.
fn number_at(payload: &Value, pointer: &str) -> Option<f64> {
    let value = payload.pointer(pointer)?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u32, output: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    #[test]
    fn cost_is_per_million_tokens() {
        let p = Pricing::new("USD", 3.0, 15.0);
        let cost = p.cost_of(&usage(1_000_000, 1_000_000));
        assert_eq!(cost.currency, "USD");
        assert!((cost.amount - 18.0).abs() < 1e-9);

        let cost = p.cost_of(&usage(1_000, 500));
        assert!((cost.amount - (0.003 + 0.0075)).abs() < 1e-9);
    }

    #[test]
    fn cached_tokens_are_billed_at_the_cache_price_and_only_once() {
        let mut p = Pricing::new("CNY", 2.0, 8.0);
        p.cache_read_per_mtok = Some(0.5);
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 800_000,
            ..Usage::default()
        };
        // 200k fresh at 2.0 plus 800k cached at 0.5
        assert!((p.cost_of(&u).amount - (0.4 + 0.4)).abs() < 1e-9);

        // Without a cache price the input price applies, which never underbills.
        p.cache_read_per_mtok = None;
        assert!((p.cost_of(&u).amount - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_cache_read_count_larger_than_the_prompt_cannot_go_negative() {
        let p = Pricing::new("USD", 1.0, 1.0);
        let u = Usage {
            input_tokens: 100,
            cache_read_tokens: 5_000,
            ..Usage::default()
        };
        assert!(p.cost_of(&u).amount > 0.0);
    }

    #[test]
    fn cache_writes_are_added_when_priced() {
        let mut p = Pricing::new("USD", 3.0, 15.0);
        p.cache_write_per_mtok = Some(3.75);
        let u = Usage {
            input_tokens: 0,
            cache_write_tokens: 1_000_000,
            ..Usage::default()
        };
        assert!((p.cost_of(&u).amount - 3.75).abs() < 1e-9);
    }

    #[test]
    fn input_only_estimates_are_available_before_sending() {
        let p = Pricing::new("USD", 3.0, 15.0);
        let cost = p.estimate_input(500_000);
        assert!((cost.amount - 1.5).abs() < 1e-9);
    }

    #[test]
    fn impossible_prices_are_reported() {
        let mut p = Pricing::new("", 1.0, 2.0);
        assert!(p.problems().iter().any(|m| m.contains("currency")));
        p.currency = "USD".into();
        p.output_per_mtok = -1.0;
        assert!(p.problems().iter().any(|m| m.contains("output")));
        p.output_per_mtok = 2.0;
        p.cache_read_per_mtok = Some(f64::NAN);
        assert!(p.problems().iter().any(|m| m.contains("cache read")));
    }

    #[test]
    fn totals_stay_separated_by_currency() {
        let mut totals = CostTotals::default();
        totals.add(&Cost {
            currency: "USD".into(),
            amount: 1.5,
        });
        totals.add(&Cost {
            currency: "USD".into(),
            amount: 0.5,
        });
        totals.add(&Cost {
            currency: "CNY".into(),
            amount: 7.0,
        });
        assert_eq!(totals.get("USD"), 2.0);
        assert_eq!(totals.get("CNY"), 7.0);
        assert_eq!(totals.get("EUR"), 0.0);
        assert_eq!(totals.0.len(), 2);
    }

    /// Most presets hang off a versioned base; the exceptions say so.
    fn probe_of(preset: BalancePreset) -> BalanceProbe {
        preset.probe_for(BaseDepth::Versioned).unwrap()
    }

    #[test]
    fn deepseek_payload_is_understood() {
        let probe = probe_of(BalancePreset::DeepSeek);
        let payload = json!({
            "is_available": true,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "48.75",
                 "granted_balance": "0.00", "topped_up_balance": "48.75"}
            ]
        });
        let balance = Balance::from_payload(&probe, &payload).unwrap();
        assert_eq!(balance.currency, "CNY");
        assert_eq!(balance.remaining, Some(48.75));
    }

    #[test]
    fn openrouter_reports_total_and_usage() {
        let probe = probe_of(BalancePreset::OpenRouter);
        let payload = json!({"data": {"total_credits": 25.0, "total_usage": 4.25}});
        let balance = Balance::from_payload(&probe, &payload).unwrap();
        assert_eq!(balance.currency, "USD");
        assert_eq!(balance.total, Some(25.0));
        assert_eq!(balance.used, Some(4.25));
        assert_eq!(balance.remaining, Some(20.75));
    }

    #[test]
    fn moonshot_and_siliconflow_payloads_are_understood() {
        let probe = probe_of(BalancePreset::Moonshot);
        let balance = Balance::from_payload(
            &probe,
            &json!({"code": 0, "data": {"available_balance": 12.5, "cash_balance": 12.5}}),
        )
        .unwrap();
        assert_eq!(
            (balance.currency.as_str(), balance.remaining),
            ("CNY", Some(12.5))
        );

        let probe = probe_of(BalancePreset::SiliconFlow);
        let balance = Balance::from_payload(
            &probe,
            &json!({"data": {"balance": "1.00", "totalBalance": "3.50"}}),
        )
        .unwrap();
        assert_eq!(balance.remaining, Some(3.5));
    }

    #[test]
    fn a_custom_probe_reads_wherever_it_is_pointed() {
        let config = BalanceConfig {
            preset: BalancePreset::Custom,
            custom: Some(BalanceProbe {
                path: "/billing/state".into(),
                remaining_pointer: Some("/wallets/1/amount".into()),
                currency_pointer: Some("/wallets/1/unit".into()),
                ..BalanceProbe::default()
            }),
        };
        let probe = config.probe(BaseDepth::Versioned).unwrap();
        let balance = Balance::from_payload(
            &probe,
            &json!({"wallets": [{"amount": 1, "unit": "usd"}, {"amount": "9.99", "unit": "eur"}]}),
        )
        .unwrap();
        assert_eq!(balance.currency, "EUR");
        assert_eq!(balance.remaining, Some(9.99));
    }

    #[test]
    fn an_unreadable_payload_yields_nothing() {
        let probe = probe_of(BalancePreset::DeepSeek);
        assert!(Balance::from_payload(&probe, &json!({"error": "nope"})).is_none());
        assert!(Balance::from_payload(&probe, &json!({"balance_infos": []})).is_none());
    }

    #[test]
    fn providers_without_an_endpoint_are_explicit_about_it() {
        let kind = BaseDepth::Versioned;
        assert!(BalancePreset::None.probe_for(kind).is_none());
        assert!(!BalanceConfig::default().is_supported(kind));
        // Custom without a probe is also unsupported rather than a panic.
        let config = BalanceConfig {
            preset: BalancePreset::Custom,
            custom: None,
        };
        assert!(!config.is_supported(kind));
    }

    #[test]
    fn sub2api_reports_a_wallet_a_quota_or_a_subscription() {
        let probe = probe_of(BalancePreset::Sub2Api);

        // Wallet keys: `remaining` mirrors the account balance.
        let wallet = Balance::from_payload(
            &probe,
            &json!({"mode": "unrestricted", "isValid": true, "planName": "钱包余额",
                    "remaining": 12.5, "unit": "USD", "balance": 12.5}),
        )
        .unwrap();
        assert_eq!(wallet.currency, "USD");
        assert_eq!(wallet.remaining, Some(12.5));

        // Quota bound keys add the limit and what has been spent.
        let quota = Balance::from_payload(
            &probe,
            &json!({"mode": "quota_limited", "status": "active", "remaining": 7.25, "unit": "USD",
                    "quota": {"limit": 20.0, "used": 12.75, "remaining": 7.25, "unit": "USD"}}),
        )
        .unwrap();
        assert_eq!(quota.remaining, Some(7.25));
        assert_eq!(quota.total, Some(20.0));
        assert_eq!(quota.used, Some(12.75));

        // Subscription keys report the tightest window that is left.
        let subscription = Balance::from_payload(
            &probe,
            &json!({"mode": "unrestricted", "planName": "Max", "remaining": 3.0, "unit": "USD",
                    "subscription": {"daily_limit_usd": 5.0, "daily_usage_usd": 2.0}}),
        )
        .unwrap();
        assert_eq!(subscription.remaining, Some(3.0));

        // A key with only rate limits reports no amount at all, which is an honest
        // "cannot tell" rather than a zero.
        assert!(Balance::from_payload(
            &probe,
            &json!({"mode": "quota_limited", "rate_limits": [{"window": "5h", "limit": 10}]}),
        )
        .is_none());
    }

    #[test]
    fn sub2api_path_follows_the_dialect() {
        // A versioned base already ends in /v1; an API root does not.
        assert_eq!(
            BalancePreset::Sub2Api
                .probe_for(BaseDepth::Versioned)
                .unwrap()
                .path,
            "/usage"
        );
        assert_eq!(
            BalancePreset::Sub2Api
                .probe_for(BaseDepth::ApiRoot)
                .unwrap()
                .path,
            "/v1/usage"
        );
    }

    #[test]
    fn presets_round_trip_through_json() {
        for preset in BalancePreset::ALL {
            let json = serde_json::to_string(&preset).unwrap();
            assert_eq!(
                serde_json::from_str::<BalancePreset>(&json).unwrap(),
                preset
            );
        }
        assert_eq!(
            serde_json::to_string(&BalancePreset::SiliconFlow).unwrap(),
            "\"silicon_flow\""
        );
        // Spelled the way the project spells itself, not `sub2_api`.
        assert_eq!(
            serde_json::to_string(&BalancePreset::Sub2Api).unwrap(),
            "\"sub2api\""
        );
    }
}
