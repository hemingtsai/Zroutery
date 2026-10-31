//! Durable accounting facts. These contain neither request bodies nor secrets.
//! Identity and prices are captured before a send; observations cannot rename
//! an earlier attempt when configuration, credentials or rates change later.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::billing::{Cost, CostTotals, Pricing};
use crate::config::ModelTier;
use crate::ir::{Dialect, Usage};
use crate::router::Candidate;
use crate::stats::RequestRecord;

pub const SETTLEMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub upstream_model: String,
    pub dialect: Dialect,
    pub account_id: Option<String>,
    pub inference_key_id: Option<String>,
    /// Unknown for legacy bindings, never inferred from an account name.
    pub binding_version: Option<String>,
    pub tier: Option<ModelTier>,
    pub pricing: Option<Pricing>,
    pub rate_quote: Option<crate::billing::RateQuote>,
}

impl ExecutionSnapshot {
    pub fn from_candidate(candidate: &Candidate) -> Self {
        Self {
            provider_id: candidate.provider.id.clone(),
            provider_name: candidate.provider.name.clone(),
            model_id: candidate.model_id().to_string(),
            model_name: candidate
                .entry
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&candidate.entry.upstream_model)
                .to_string(),
            upstream_model: candidate.entry.upstream_model.clone(),
            dialect: candidate.provider.dialect(),
            account_id: candidate
                .credential
                .as_ref()
                .map(|key| key.account_id.clone()),
            inference_key_id: candidate.credential.as_ref().map(|key| key.key_id.clone()),
            binding_version: candidate.credential.as_ref().and_then(|credential| {
                candidate
                    .provider
                    .accounts
                    .iter()
                    .find(|account| account.account_id == credential.account_id)
                    .and_then(|account| account.binding.as_ref())
                    .map(|binding| binding.version())
            }),
            tier: candidate.entry.tier,
            pricing: candidate
                .rate_quote
                .as_ref()
                .map(|quote| quote.pricing.clone())
                .or_else(|| candidate.entry.pricing.clone()),
            rate_quote: candidate.rate_quote.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    InFlight,
    Served,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptFact {
    pub id: String,
    /// Auxiliary calls can be correlated without inventing a client request.
    pub request_id: Option<String>,
    pub execution: ExecutionSnapshot,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub state: AttemptState,
    /// A preparation failure never reached an upstream and has no remote cost.
    pub sent: bool,
    pub rectified: bool,
    pub usage: Option<Usage>,
    pub cost: Option<Cost>,
    pub failure_class: Option<crate::failure::FailureClass>,
    /// Freeze budget windows when observed, not when a disk retry happens.
    pub budget_day: String,
    pub budget_month: String,
}

impl AttemptFact {
    pub fn new(id: String, request_id: Option<String>, candidate: &Candidate, sent: bool) -> Self {
        let now = Local::now();
        Self {
            id,
            request_id,
            execution: ExecutionSnapshot::from_candidate(candidate),
            started_at: now.with_timezone(&Utc),
            finished_at: None,
            state: AttemptState::InFlight,
            sent,
            rectified: false,
            usage: None,
            cost: None,
            failure_class: None,
            budget_day: now.format("%Y-%m-%d").to_string(),
            budget_month: now.format("%Y-%m").to_string(),
        }
    }

    pub fn observe_usage(&mut self, usage: Usage) {
        // Existing decoders use an all-zero default for absent usage. Until a
        // source explicitly proves zero, it cannot prove a free executed call.
        if usage != Usage::default() {
            self.usage = Some(usage);
            self.cost = self
                .execution
                .pricing
                .as_ref()
                .map(|price| price.cost_of(&usage));
        }
        if self.sent {
            if let Some(price) = self
                .execution
                .pricing
                .as_ref()
                .filter(|price| price.per_request.is_some())
            {
                self.cost = Some(price.cost_of(&usage));
            }
        }
    }

    pub fn finish(&mut self, state: AttemptState) {
        let now = Local::now();
        self.state = state;
        self.finished_at = Some(now.with_timezone(&Utc));
        self.budget_day = now.format("%Y-%m-%d").to_string();
        self.budget_month = now.format("%Y-%m").to_string();
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.id.len() > 256 || self.execution.provider_id.is_empty() {
            return Err("the accounting attempt has no stable identity".into());
        }
        if self.state == AttemptState::InFlight
            && (self.finished_at.is_some() || self.cost.is_some())
        {
            return Err("an in-flight attempt cannot have a final charge".into());
        }
        if self.state != AttemptState::InFlight && self.finished_at.is_none() {
            return Err("a completed attempt must carry its observation time".into());
        }
        if let Some(cost) = &self.cost {
            if !self.sent
                || cost.currency.trim().is_empty()
                || !cost.amount.is_finite()
                || cost.amount < 0.0
            {
                return Err("the accounting attempt has an invalid charge".into());
            }
        }
        if chrono::NaiveDate::parse_from_str(&self.budget_day, "%Y-%m-%d").is_err()
            || self.budget_day.get(..7) != Some(self.budget_month.as_str())
        {
            return Err("the accounting attempt has invalid budget windows".into());
        }
        Ok(())
    }

    /// Fields that must never move when late observations complete an attempt.
    pub(crate) fn same_execution(&self, other: &Self) -> bool {
        self.id == other.id
            && self.request_id == other.request_id
            && self.execution == other.execution
            && self.started_at == other.started_at
            && self.sent == other.sent
            && self.rectified == other.rectified
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

impl UsageTotals {
    pub fn add(&mut self, usage: Usage) {
        self.input_tokens += u64::from(usage.input_tokens);
        self.output_tokens += u64::from(usage.output_tokens);
        self.cache_read_tokens += u64::from(usage.cache_read_tokens);
        self.cache_write_tokens += u64::from(usage.cache_write_tokens);
        self.reasoning_tokens += u64::from(usage.reasoning_tokens);
    }
}

impl From<Usage> for UsageTotals {
    fn from(usage: Usage) -> Self {
        let mut totals = Self::default();
        totals.add(usage);
        totals
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceState {
    Durable,
    Pending,
    /// Explicit Core/test mode; never the desktop's implicit fallback.
    Volatile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestAccounting {
    pub schema_version: u32,
    pub persistence: PersistenceState,
    pub attempt_ids: Vec<String>,
    pub known_cost: CostTotals,
    pub unknown_cost_attempts: u32,
    pub observed_usage: UsageTotals,
    pub unknown_usage_attempts: u32,
}

impl RequestAccounting {
    pub fn from_attempts(attempts: &[AttemptFact], persistence: PersistenceState) -> Self {
        let mut result = Self {
            schema_version: SETTLEMENT_SCHEMA_VERSION,
            persistence,
            attempt_ids: attempts.iter().map(|attempt| attempt.id.clone()).collect(),
            known_cost: CostTotals::default(),
            unknown_cost_attempts: 0,
            observed_usage: UsageTotals::default(),
            unknown_usage_attempts: 0,
        };
        for attempt in attempts.iter().filter(|attempt| attempt.sent) {
            match &attempt.cost {
                Some(cost) => result.known_cost.add(cost),
                None => result.unknown_cost_attempts += 1,
            }
            match attempt.usage {
                Some(usage) => result.observed_usage.add(usage),
                None => result.unknown_usage_attempts += 1,
            }
        }
        result
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettlementFact {
    pub schema_version: u32,
    pub request: RequestRecord,
    pub attempts: Vec<AttemptFact>,
}

impl SettlementFact {
    pub fn safe(&self) -> Self {
        let mut safe = self.clone();
        safe.request.error = None;
        safe.request.routing_decision = None;
        safe
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingStatus {
    pub required: bool,
    pub durable: bool,
    pub available: bool,
    pub pending_settlements: usize,
    pub pending_attempts: usize,
    pub unreconciled_attempts: usize,
    pub error: Option<String>,
    pub commits: u64,
    pub last_commit_wait_ms: Option<f64>,
}
