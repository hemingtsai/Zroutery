//! Durable accounting facts. These contain neither request bodies nor secrets.
//! Identity and prices are captured before a send; observations cannot rename
//! an earlier attempt when configuration, credentials or rates change later.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::billing::{Cost, CostTotals, Pricing};
use crate::config::{ModelTier, ProviderConfig};
use crate::ir::{Dialect, ObservedResponse, Usage, UsageEvidence};
use crate::router::Candidate;
use crate::stats::RequestRecord;

pub const SETTLEMENT_SCHEMA_VERSION: u32 = 1;

/// The stable name of the wire protocol an upstream actually answered on.
///
/// Recorded per attempt so a reader can tell the API the *client* used from
/// the API the *upstream* answered on — the two differ whenever the proxy
/// bridges dialects, and collapsing them is how an inbound `/v1/messages`
/// request starts looking like an upstream Messages call. The decision bridge
/// is named before any dialect lookup: a decision endpoint's `dialect()` value
/// is the wire fallback nothing asks it to chat on, not the protocol it
/// speaks.
fn upstream_protocol_name(provider: &ProviderConfig) -> Option<String> {
    if provider.kind.is_decision() {
        return Some("systemone".to_string());
    }
    Some(
        match provider.dialect() {
            Dialect::Anthropic => "anthropic_messages",
            Dialect::OpenAI => "openai_chat",
            Dialect::OpenAIResponses => "openai_responses",
            Dialect::Gemini => "gemini",
        }
        .to_string(),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_origin: Option<PluginExecutionOrigin>,
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub model_name: String,
    pub upstream_model: String,
    pub dialect: Dialect,
    /// The wire protocol the upstream actually answered on, as a stable name
    /// (`systemone`, `anthropic_messages`, `openai_chat`, `openai_responses`,
    /// `gemini`). Distinct from the record's ingress dialect, which is the
    /// API the *client* spoke. `None` on records written before the field
    /// existed; it is never re-derived from current configuration, because a
    /// legacy attempt's protocol is a fact about that attempt, not about the
    /// provider as configured today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_protocol: Option<String>,
    pub account_id: Option<String>,
    pub inference_key_id: Option<String>,
    /// Unknown for legacy bindings, never inferred from an account name.
    pub binding_version: Option<String>,
    pub tier: Option<ModelTier>,
    pub pricing: Option<Pricing>,
    pub rate_quote: Option<crate::billing::RateQuote>,
    /// Tariffs for server-side sub-inferences on this same provider/account/key,
    /// frozen before the call. Unknown models never inherit the main tariff.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub iteration_pricing: std::collections::BTreeMap<String, Pricing>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub iteration_quotes: std::collections::BTreeMap<String, crate::billing::RateQuote>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginExecutionOrigin {
    pub installation_id: String,
    pub contribution_id: String,
    pub package_digest: String,
    pub grant_epoch: u64,
    pub transport_profile: Option<String>,
}

impl ExecutionSnapshot {
    pub fn from_candidate(candidate: &Candidate) -> Self {
        Self {
            plugin_origin: {
                #[cfg(feature = "plugin")]
                {
                    candidate.provider.plugin_binding.as_ref().map(|binding| {
                        PluginExecutionOrigin {
                            installation_id: binding.installation_id().into(),
                            contribution_id: binding.contribution_id().into(),
                            package_digest: binding.package_digest().into(),
                            grant_epoch: binding.grant_epoch(),
                            transport_profile: binding.profile().map(str::to_string),
                        }
                    })
                }
                #[cfg(not(feature = "plugin"))]
                {
                    None
                }
            },
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
            upstream_protocol: upstream_protocol_name(&candidate.provider),
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
            pricing: candidate.settled_pricing().cloned(),
            rate_quote: candidate.rate_quote.clone(),
            iteration_pricing: Default::default(),
            iteration_quotes: Default::default(),
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
    /// The generating request whose server-side safety review made this send.
    /// An opaque gateway request ID, never a tool input or conversation body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<String>,
    pub execution: ExecutionSnapshot,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub state: AttemptState,
    /// A preparation failure never reached an upstream and has no remote cost.
    pub sent: bool,
    pub rectified: bool,
    pub usage: Option<Usage>,
    /// Wire quality is separate from retained counters. Conflicting readings
    /// remain evidence but cannot become complete accounting or a known charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_evidence: Option<UsageEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_iterations: Option<Vec<crate::ir::UsageIteration>>,
    /// Unknown model rates/refusal billing must not turn a known subtotal into
    /// a claim that every upstream sub-inference has been fully priced.
    #[serde(default)]
    pub unknown_cost_parts: u32,
    pub cost: Option<Cost>,
    /// What the upstream's structured response declared it ran, when the
    /// attempt produced a response to read. `None` means the attempt predates
    /// the capture or has not settled yet — not "the provider declared
    /// nothing", which is [`crate::ir::DeclaredIdentity::Missing`]. The value
    /// is an observation that completes the attempt and is deliberately not
    /// part of `same_execution`: a late completion may add it without the
    /// pre-send identity having moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<crate::ir::UpstreamModelDeclaration>,
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
            parent_request_id: None,
            execution: ExecutionSnapshot::from_candidate(candidate),
            started_at: now.with_timezone(&Utc),
            finished_at: None,
            state: AttemptState::InFlight,
            sent,
            rectified: false,
            usage: None,
            usage_evidence: None,
            usage_iterations: None,
            unknown_cost_parts: 0,
            cost: None,
            declared: None,
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

    pub(crate) fn bind_iteration_pricing(
        &mut self,
        config: &crate::config::AppConfig,
        candidate: &Candidate,
    ) {
        if candidate.provider.dialect() != Dialect::Anthropic {
            return;
        }
        let account = candidate.credential.as_ref().and_then(|credential| {
            candidate
                .provider
                .accounts
                .iter()
                .find(|account| account.account_id == credential.account_id)
        });
        let key = candidate
            .credential
            .as_ref()
            .map(|credential| credential.key_id.as_str());
        for model in config
            .models
            .iter()
            .filter(|model| model.provider_id == candidate.provider.id)
        {
            if let Some(quote) = crate::billing::resolve_rate(
                &candidate.provider,
                model,
                account,
                key,
                chrono::Utc::now().timestamp(),
            ) {
                self.execution
                    .iteration_pricing
                    .insert(model.upstream_model.clone(), quote.pricing.clone());
                self.execution
                    .iteration_quotes
                    .insert(model.upstream_model.clone(), quote);
            }
        }
        if let Some(price) = &self.execution.pricing {
            self.execution
                .iteration_pricing
                .insert(self.execution.upstream_model.clone(), price.clone());
        }
        if let Some(quote) = &self.execution.rate_quote {
            self.execution
                .iteration_quotes
                .insert(self.execution.upstream_model.clone(), quote.clone());
        }
    }

    pub fn observe_response(&mut self, observed: &ObservedResponse) {
        if self.state != AttemptState::InFlight || !self.sent {
            return;
        }
        self.usage_evidence = Some(observed.usage_evidence);
        self.usage = observed.usage;
        self.usage_iterations = observed.usage_iterations.clone();
        self.cost = observed.accounting_usage().and_then(|usage| {
            self.execution
                .pricing
                .as_ref()
                .map(|price| price.cost_of(&usage))
        });
        if let Some(iterations) = &mut self.usage_iterations {
            self.cost = None;
            self.unknown_cost_parts = 0;
            let mut totals = CostTotals::default();
            if observed.accounting_usage().is_some() {
                for iteration in iterations {
                    iteration.cost = None;
                    iteration.cost_complete = false;
                    iteration.rate_quote = None;
                    if iteration.billed == Some(false) {
                        iteration.cost_complete = true;
                        continue;
                    }
                    let model = iteration
                        .model
                        .as_deref()
                        .unwrap_or(&self.execution.upstream_model);
                    let price = self.execution.iteration_pricing.get(model).or_else(|| {
                        (model == self.execution.upstream_model)
                            .then_some(self.execution.pricing.as_ref())
                            .flatten()
                    });
                    iteration.rate_quote = self.execution.iteration_quotes.get(model).cloned();
                    if iteration.billed == Some(true) {
                        if let Some(price) = price.filter(|price| price.per_request.is_none()) {
                            let cost = price.cost_of_iteration(iteration);
                            totals.add(&cost);
                            iteration.cost = Some(cost);
                            iteration.cost_complete = true;
                            continue;
                        }
                    }
                    self.unknown_cost_parts += 1;
                }
                if totals.0.len() == 1 {
                    let (currency, amount) = totals.0.into_iter().next().expect("one currency");
                    self.cost = Some(Cost { currency, amount });
                } else if totals.is_empty() && self.unknown_cost_parts == 0 {
                    if let Some(price) = &self.execution.pricing {
                        self.cost = Some(Cost {
                            currency: price.currency.clone(),
                            amount: 0.0,
                        });
                    }
                }
            } else {
                self.unknown_cost_parts = 1;
            }
        }
        self.declared
            .get_or_insert_with(Default::default)
            .merge(&observed.declaration);
    }

    /// Every known currency is retained independently. `cost` is the legacy
    /// single-currency view and must not lose a multi-model/currency subtotal.
    pub(crate) fn known_costs(&self) -> Vec<Cost> {
        if self
            .execution
            .pricing
            .as_ref()
            .is_some_and(|price| price.per_request.is_some())
        {
            return self.cost.iter().cloned().collect();
        }
        if let Some(iterations) = &self.usage_iterations {
            let mut totals = CostTotals::default();
            for cost in iterations
                .iter()
                .filter_map(|iteration| iteration.cost.as_ref())
            {
                totals.add(cost);
            }
            if !totals.is_empty() {
                return totals
                    .0
                    .into_iter()
                    .map(|(currency, amount)| Cost { currency, amount })
                    .collect();
            }
        }
        self.cost.iter().cloned().collect()
    }

    pub(crate) fn accounted_usage(&self) -> Option<Usage> {
        match self.usage_evidence {
            Some(UsageEvidence::Absent | UsageEvidence::Invalid | UsageEvidence::Conflicting) => {
                None
            }
            _ => self.usage,
        }
    }

    pub fn finish(&mut self, state: AttemptState) {
        let now = Local::now();
        // A completed per-call response has a tariff even if it omits token
        // counters. A cancellation without usage does not prove a charged call.
        if self.sent && state == AttemptState::Served {
            if let Some(price) = self
                .execution
                .pricing
                .as_ref()
                .filter(|price| price.per_request.is_some())
            {
                self.cost = Some(price.cost_of(&Usage::default()));
                self.unknown_cost_parts = 0;
            }
        }
        self.state = state;
        self.finished_at = Some(now.with_timezone(&Utc));
        self.budget_day = now.format("%Y-%m-%d").to_string();
        self.budget_month = now.format("%Y-%m").to_string();
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.id.len() > 256 || self.execution.provider_id.is_empty() {
            return Err("the accounting attempt has no stable identity".into());
        }
        if self.parent_request_id.as_ref().is_some_and(|id| {
            !id.starts_with("req_") || id.len() > 256 || id.chars().any(char::is_control)
        }) {
            return Err("the accounting review has an invalid parent request ID".into());
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
        if self.usage_iterations.as_ref().is_some_and(|iterations| {
            iterations
                .iter()
                .filter_map(|iteration| iteration.cost.as_ref())
                .any(|cost| {
                    cost.currency.trim().is_empty() || !cost.amount.is_finite() || cost.amount < 0.0
                })
        }) {
            return Err("the accounting iteration has an invalid charge".into());
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
            && self.parent_request_id == other.parent_request_id
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
            let costs = attempt.known_costs();
            for cost in &costs {
                result.known_cost.add(cost);
            }
            let usage = attempt.accounted_usage();
            if let Some(usage) = usage {
                result.observed_usage.add(usage);
            }
            // Keep the known part and disclose that no final counters arrived;
            // the same partial attempt is counted only once in each unknown set.
            let partial = attempt.usage_evidence == Some(UsageEvidence::Partial);
            result.unknown_usage_attempts += u32::from(usage.is_none() || partial);
            result.unknown_cost_attempts += u32::from(
                costs.is_empty()
                    || attempt.unknown_cost_parts > 0
                    || (partial
                        && attempt
                            .execution
                            .pricing
                            .as_ref()
                            .is_some_and(|price| price.per_request.is_none())),
            );
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

#[cfg(test)]
mod protocol_name_tests {
    use super::upstream_protocol_name;
    use crate::config::{ProviderConfig, ProviderKind};

    #[test]
    fn the_protocol_name_follows_the_dialect_and_names_the_decision_bridge_first() {
        let mut provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);
        assert_eq!(
            upstream_protocol_name(&provider).as_deref(),
            Some("openai_chat")
        );
        provider.kind = ProviderKind::Anthropic;
        assert_eq!(
            upstream_protocol_name(&provider).as_deref(),
            Some("anthropic_messages")
        );

        // A decision endpoint's `dialect()` value is the wire fallback nothing
        // chats on; the bridge it actually speaks is named before that
        // lookup, so an inbound dialect confusion cannot relabel it.
        let decision = ProviderConfig::new("jev", "Jev", ProviderKind::Decision);
        assert_eq!(
            upstream_protocol_name(&decision).as_deref(),
            Some("systemone")
        );
    }
}
