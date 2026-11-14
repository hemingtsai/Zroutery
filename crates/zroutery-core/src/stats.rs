//! Request accounting with a bounded recent view and optional durable history.
//! Only accounting metadata is persisted; no prompt or completion is recorded.
//!
//! All mutable state lives behind a single `Mutex`.  For the current
//! desktop-proxy use case (one user, moderate request rate) this is
//! unlikely to be a bottleneck.  If contention ever matters, the
//! per-model and per-kind maps could be sharded behind `RwLock`s or
//! moved to a lock-free structure.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::billing::{Cost, CostTotals};
use crate::client::{ClientFingerprint, ClientId, Confidence, MAX_TRACKED_CLIENTS};
use crate::ir::{Dialect, Usage};
use crate::outcome::Outcome;
use crate::policy::{DecisionReason, RouteDecision};
use crate::query::RequestKind;
use crate::runtime_store::UsageSnapshot;

mod history;
pub use history::{UsageHistoryPage, UsageHistoryQuery};

/// One completed (or failed) client request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub id: String,
    pub at: DateTime<Utc>,
    /// Which API the client used.
    pub ingress: Dialect,
    /// What the request was *for*: the main conversation or a side query such
    /// as the Auto Mode classifier. Orthogonal to the model.
    #[serde(default)]
    pub kind: RequestKind,
    /// What the client asked for, e.g. `sonnet-class`.
    pub requested_model: String,
    /// Which concrete model answered, if any.
    pub resolved_model: Option<String>,
    pub provider_name: Option<String>,
    pub stream: bool,
    pub status: u16,
    pub ok: bool,
    pub error: Option<String>,
    pub latency_ms: u64,
    /// Time to first streamed token.
    pub ttft_ms: Option<u64>,
    pub usage: Usage,
    /// What it cost, when the model has a price. `None` means unpriced, not free.
    pub cost: Option<Cost>,
    /// How many upstream attempts it took (>1 means failover happened).
    pub attempts: u32,
    /// The tier this request was escalated *away from*.
    ///
    /// The client named a tier, every member of that tier was unusable, and the
    /// configured fallback policy escalated instead of answering from what was
    /// left. The answer therefore came from a tier the caller did not ask for,
    /// which is a billing event as much as a routing one.
    ///
    /// Projected from [`RouteDecision::reason`] at the moment the decision is
    /// recorded rather than decided again, so the two cannot disagree — there is
    /// no way to set one without the other.
    ///
    /// `None` for every request served from the tier it asked for, for every
    /// direct model-id request, and for every request that never reached a
    /// routing decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated_from: Option<String>,
    /// Routing decision trace (for diagnostics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_decision: Option<RouteDecision>,
    /// Who sent this request, and how sure we are.
    ///
    /// `#[serde(default)]` because records written before client attribution
    /// existed have no such field, and a missing one must load as
    /// [`ClientFingerprint::unknown`] rather than failing: the record log is
    /// the thing that has to keep working across a version bump. A record that
    /// deserialises into a *known* client because of a default would be worse,
    /// which is why the default is the explicit unknown bucket and not `None`.
    ///
    /// The id stored here is the one the [`crate::client::ClientRegistry`]
    /// actually filed the fingerprint under, including the overflow bucket. If
    /// the two could differ, the request list and the per-client aggregate
    /// would show different clients for the same traffic.
    #[serde(default)]
    pub client: ClientFingerprint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// What the upstream's structured response declared it ran, for the
    /// attempt that settled this request.
    ///
    /// `None` means the record predates the field (or the request never
    /// opened an attempt, so there was no response to read). A present value
    /// is the full declaration: the model axis and the version axis, each
    /// either a specific name, a placeholder (`auto` is not a model), a
    /// conflict, or an explicit absence. It is a provider's claim about its
    /// own routing, not a verification of the backend, and it is never
    /// back-filled from the requested or configured model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_declared: Option<crate::ir::UpstreamModelDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting: Option<crate::accounting::RequestAccounting>,
}

impl RequestRecord {
    /// All observed attempts, when available; legacy archives only know the
    /// final attempt. Reading statistics never rewrites either historical fact.
    fn observed_usage(&self) -> crate::accounting::UsageTotals {
        self.accounting
            .as_ref()
            .map(|accounting| accounting.observed_usage)
            .unwrap_or_else(|| self.usage.into())
    }

    fn add_known_cost(&self, totals: &mut CostTotals) {
        if let Some(accounting) = &self.accounting {
            for (currency, amount) in &accounting.known_cost.0 {
                *totals.0.entry(currency.clone()).or_default() += amount;
            }
        } else if let Some(cost) = &self.cost {
            totals.add(cost);
        }
    }

    fn unpriced(&self) -> bool {
        self.accounting
            .as_ref()
            .map(|accounting| accounting.unknown_cost_attempts > 0)
            .unwrap_or_else(|| self.cost.is_none())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelTotals {
    /// Request-level grouping by the final resolved model. Attempt totals can
    /// include failed predecessors; actual charge identities remain in the
    /// accounting attempt facts, not inferred from this grouping label.
    pub model_id: String,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    /// Prompt tokens served from the cache. **Read only** — cache *writes* are
    /// counted separately in [`ClientTotals::cache_write_tokens`] and were not
    /// tracked per model before client attribution existed.
    pub cached_tokens: u64,
    /// Spend per currency. Empty while the model has no price.
    pub cost: CostTotals,
    /// Average end to end latency in milliseconds.
    pub avg_latency_ms: f64,
}

/// Counters for one client.
///
/// The identity is carried on every row rather than looked up, so a reader of
/// `StatsSummary` never has to join against a table that may have been trimmed
/// or folded into the overflow bucket.
///
/// # The token columns
///
/// `input_tokens` is the cache-**inclusive** total: it already contains
/// `cache_read_tokens` and `cache_write_tokens`, which is the convention
/// [`crate::ir::Usage`] documents. They are broken out for display and are
/// **not** added to the input total again — summing the three would triple-count
/// every cached prompt. See [`ClientTotals::total_input_tokens`] for the
/// arithmetic that stays correct.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientTotals {
    /// The canonical client id this row aggregates.
    pub client_id: String,
    /// The name to show. A legacy row whose name repeats its raw id is named
    /// from the shipped user-agent rules when one of them recognises it; see
    /// `client_display_name`.
    pub client_name: String,
    /// The application family.
    pub family: String,
    /// How the id was arrived at.
    pub source: String,
    /// How much the answer can be trusted.
    pub confidence: String,
    pub requests: u64,
    pub failures: u64,
    /// Total prompt tokens, cache-inclusive. See the note above.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens served from the prompt cache; a subset of `input_tokens`.
    pub cache_read_tokens: u64,
    /// Prompt tokens written to the prompt cache; a subset of `input_tokens`.
    pub cache_write_tokens: u64,
    /// Reasoning tokens, already counted inside `output_tokens`.
    pub reasoning_tokens: u64,
    /// Spend per currency. Never summed across currencies.
    pub cost: CostTotals,
    /// Average end to end latency in milliseconds.
    pub avg_latency_ms: f64,
    /// Average time to first streamed token, over the requests that streamed.
    ///
    /// Averaged over *streamed* requests only, because a non-streamed request
    /// has no first token and folding its `None` in as a zero would drag the
    /// average toward zero and make the number look better than the client is.
    pub avg_ttft_ms: Option<f64>,
    /// How many of this client's requests actually streamed, and therefore how
    /// many rows `avg_ttft_ms` is averaged over.
    pub streamed: u64,
}

/// The label a client bucket shows.
///
/// The bucket's id is what the record stored, because a legacy row keeps the
/// identity it was written with. A row written before user-agent attribution
/// also stored its raw agent string as the *name*, though, and printing that
/// twice tells a reader nothing. When the stored name is the stored id, the
/// same string is looked up in the shipped user-agent rules and the product's
/// readable name is used instead. Nothing else changes: the id, family,
/// source and confidence stay exactly as recorded, so a legacy row is named,
/// not reclassified.
fn client_display_name(client: &ClientFingerprint) -> String {
    let stored = client.client_name.trim();
    if stored == client.client_id.as_str() {
        if let Some((_, name)) = crate::client::name_for_user_agent(stored) {
            return name.to_string();
        }
    }
    stored.to_string()
}

impl ClientTotals {
    fn observe(&mut self, record: &RequestRecord) {
        self.requests += 1;
        if !record.ok {
            self.failures += 1;
        }
        let usage = record.observed_usage();
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_read_tokens += usage.cache_read_tokens;
        self.cache_write_tokens += usage.cache_write_tokens;
        self.reasoning_tokens += usage.reasoning_tokens;
        record.add_known_cost(&mut self.cost);
        let n = self.requests as f64;
        self.avg_latency_ms += (record.latency_ms as f64 - self.avg_latency_ms) / n;
        if let Some(ttft) = record.ttft_ms {
            let seen = self.streamed;
            self.avg_ttft_ms = Some(match self.avg_ttft_ms {
                None => ttft as f64,
                Some(current) => current + (ttft as f64 - current) / (seen + 1) as f64,
            });
            self.streamed += 1;
        }
    }

    /// Prompt tokens that were neither read from nor written to the cache.
    ///
    /// The one figure that can be compared against a model's *fresh input* price
    /// without double-counting the cache columns.
    pub fn fresh_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cache_read_tokens)
            .saturating_sub(self.cache_write_tokens)
    }

    /// Prompt plus completion tokens.
    ///
    /// Reasoning is already inside `output_tokens`, so it is not added again.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Requests that succeeded, out of all of them.
    pub fn success_rate(&self) -> f64 {
        if self.requests == 0 {
            0.0
        } else {
            (self.requests - self.failures) as f64 / self.requests as f64
        }
    }
}

/// Counters for one client kind (`main` / `auto_mode`).
///
/// Model health says whether a *model* is responsive; this says how a kind of
/// traffic is doing. The same GLM endpoint can carry the main conversation and
/// the classifier, and without this split the dashboard cannot tell whether
/// "GLM is slow" means coding is slow or approvals are slow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KindTotals {
    pub kind: String,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Average end to end latency in milliseconds.
    pub avg_latency_ms: f64,
}

impl KindTotals {
    fn observe(&mut self, record: &RequestRecord) {
        self.requests += 1;
        if !record.ok {
            self.failures += 1;
        }
        let usage = record.observed_usage();
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        let n = self.requests as f64;
        self.avg_latency_ms += (record.latency_ms as f64 - self.avg_latency_ms) / n;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummary {
    pub since: DateTime<Utc>,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cache reads/writes are subsets of the cache-inclusive input total.
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Requests with wholly or partly unknown cost, not zero-cost requests.
    #[serde(default)]
    pub unpriced_requests: u64,
    /// Spend per currency; never summed across currencies.
    pub cost: CostTotals,
    pub per_model: Vec<ModelTotals>,
    /// Counters per request kind, sorted by kind name.
    pub per_kind: Vec<KindTotals>,
    /// Counters per client, sorted by client id.
    ///
    /// Sums to the same totals as the top-level fields for the same window, and
    /// to `per_model` as well. That equality is the point of the table: a
    /// dashboard that shows per-client spend which does not add up to the
    /// headline number is worse than one that shows no split at all.
    pub per_client: Vec<ClientTotals>,
    /// How well traffic is being attributed, as a share of `requests`.
    ///
    /// Three buckets rather than one percentage, because "we recognised 90%"
    /// is a very different claim depending on whether the 10% was
    /// `python-requests` — which we identified as a library — or a private
    /// script we have never heard of.
    ///
    /// These are shares of `requests`, not of tokens: a few large requests from
    /// an unrecognised client would dominate a token-weighted ratio and make
    /// the identified share look far worse than the request count justifies.
    pub attribution: AttributionSummary,
}

impl StatsSummary {
    /// Fold imported history into this device's headline figures.
    ///
    /// Only quantities the imported store measures are added: requests,
    /// failures, tokens (with cache kept as a subset of input), unpriced rows
    /// and known spend per currency. The breakdowns and the attribution shares
    /// stay this device's own ledger — the imported store has no request kind
    /// and no attribution evidence, and a split that silently disagreed with
    /// the headline would be worse than one that says what it covers.
    ///
    /// `since` moves back to the imported store's first counted day so the
    /// period matches the numbers it is printed beside. A missing first day
    /// leaves the device's own start alone.
    pub fn add_imported(
        &mut self,
        totals: &crate::history_import::ImportTotals,
        first_day: Option<NaiveDate>,
    ) {
        self.requests = self.requests.saturating_add(totals.requests);
        self.failures = self.failures.saturating_add(totals.failures);
        self.input_tokens = self.input_tokens.saturating_add(totals.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(totals.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(totals.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(totals.cache_write_tokens);
        self.unpriced_requests = self
            .unpriced_requests
            .saturating_add(totals.unpriced_requests);
        for (currency, amount) in &totals.cost.0 {
            self.cost.add(&Cost {
                currency: currency.clone(),
                amount: *amount,
            });
        }
        if let Some(first) = first_day {
            if let Some(start) =
                crate::history_import::day_start_utc(&first.format("%Y-%m-%d").to_string())
            {
                self.since = self.since.min(start);
            }
        }
    }
}

/// One local calendar day of usage, for the Activity page's charts.
///
/// The calendar is the operator's, not UTC: a request at 00:30 local time
/// belongs to the day they remember making it. History stays in UTC, so only
/// the bucket boundary moves here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageDay {
    /// Local day, `YYYY-MM-DD`.
    pub day: String,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub unpriced_requests: u64,
    /// Known subtotals only, always separated by currency.
    #[serde(default)]
    pub cost: CostTotals,
}

impl UsageDay {
    pub(crate) fn observe(&mut self, record: &RequestRecord) {
        let usage = record.observed_usage();
        self.requests += 1;
        self.failures += u64::from(!record.ok);
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_read_tokens += usage.cache_read_tokens;
        self.cache_write_tokens += usage.cache_write_tokens;
        self.unpriced_requests += u64::from(record.unpriced());
        record.add_known_cost(&mut self.cost);
    }

    /// Add another store's totals for the same calendar day.
    ///
    /// Every field is a count or a known amount, so the sum is exact; no
    /// average is derived here and no missing figure becomes zero.
    pub fn add_imported(&mut self, other: &UsageDay) {
        self.requests = self.requests.saturating_add(other.requests);
        self.failures = self.failures.saturating_add(other.failures);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.unpriced_requests = self
            .unpriced_requests
            .saturating_add(other.unpriced_requests);
        for (currency, amount) in &other.cost.0 {
            self.cost.add(&Cost {
                currency: currency.clone(),
                amount: *amount,
            });
        }
    }
}

impl From<&DurableTotals> for StatsSummary {
    fn from(totals: &DurableTotals) -> Self {
        StatsSummary {
            since: totals.since,
            requests: totals.requests,
            failures: totals.failures,
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
            cache_read_tokens: totals.cache_read_tokens,
            cache_write_tokens: totals.cache_write_tokens,
            unpriced_requests: totals.unpriced_requests,
            cost: totals.cost.clone(),
            per_model: totals.per_model.values().cloned().collect(),
            per_kind: totals.per_kind.values().cloned().collect(),
            per_client: totals.per_client.values().cloned().collect(),
            attribution: totals.attribution.clone(),
        }
    }
}

/// Add imported days to this device's series, one row per local day.
///
/// A day the imported store never covered is not created: silence from another
/// tool is not a measured zero. A day both stores know about is summed, because
/// both describe requests made that day; repeated imports of the same source are
/// already deduplicated by source, instance and entity id when they are stored,
/// so they cannot be added twice here.
pub fn merge_imported_days(mut local: Vec<UsageDay>, imported: Vec<UsageDay>) -> Vec<UsageDay> {
    for day in imported {
        match local.iter_mut().find(|row| row.day == day.day) {
            Some(row) => row.add_imported(&day),
            None => local.push(day),
        }
    }
    local.sort_by(|left, right| left.day.cmp(&right.day));
    local
}

/// The durable aggregate of every committed usage row.
///
/// [`crate::runtime_store::RuntimeStore`] keeps one serialized copy of this
/// value in the `usage_summary` table and updates it inside the same
/// transaction as every usage insert, settlement replace and clear. A refresh
/// therefore reads one row plus the recent window instead of re-reading the
/// whole usage table, whose size grows with the life of the process. The
/// in-memory projection adopts this value wholesale and re-adds the records
/// that are not durable yet, which is what a rebuild from scratch would have
/// produced.
///
/// [`Self::unobserve`] is the exact inverse of [`Self::observe`]: every field
/// is a counter or a running average carried with its count, so a settlement
/// that replaces a row can subtract the row it replaces. Floating-point
/// averages may drift by an ulp from a from-scratch rebuild; integer counters
/// never do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DurableTotals {
    pub since: DateTime<Utc>,
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub unpriced_requests: u64,
    pub cost: CostTotals,
    pub per_model: BTreeMap<String, ModelTotals>,
    pub per_kind: BTreeMap<String, KindTotals>,
    pub per_client: BTreeMap<String, ClientTotals>,
    pub attribution: AttributionSummary,
}

impl Default for DurableTotals {
    fn default() -> Self {
        Self::empty()
    }
}

impl DurableTotals {
    /// An empty aggregate whose window opens now, the same anchor a fresh
    /// [`Stats`] and a projection reset use.
    pub(crate) fn empty() -> Self {
        Self {
            since: Utc::now(),
            requests: 0,
            failures: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            unpriced_requests: 0,
            cost: CostTotals::default(),
            per_model: BTreeMap::new(),
            per_kind: BTreeMap::new(),
            per_client: BTreeMap::new(),
            attribution: AttributionSummary::default(),
        }
    }

    /// Fold one record into the aggregate. The same arithmetic
    /// [`Stats::accumulate`] has always applied, minus the request ring.
    pub(crate) fn observe(&mut self, record: &RequestRecord) {
        self.since = self.since.min(record.at);
        self.requests += 1;
        if !record.ok {
            self.failures += 1;
        }
        let usage = record.observed_usage();
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_read_tokens += usage.cache_read_tokens;
        self.cache_write_tokens += usage.cache_write_tokens;
        self.unpriced_requests += u64::from(record.unpriced());
        record.add_known_cost(&mut self.cost);

        // Only a model that actually resolved earns its own permanent
        // dimension. An unresolved request still counts towards the totals and
        // failures below, but it shares one bucket: the requested name is
        // caller-chosen text, not a bounded set this table can grow into.
        let key = record
            .resolved_model
            .clone()
            .unwrap_or_else(|| UNRESOLVED_MODEL_KEY.to_string());
        let totals = self
            .per_model
            .entry(key.clone())
            .or_insert_with(|| ModelTotals {
                model_id: key,
                ..ModelTotals::default()
            });
        totals.requests += 1;
        if !record.ok {
            totals.failures += 1;
        }
        totals.input_tokens += usage.input_tokens;
        totals.output_tokens += usage.output_tokens;
        totals.reasoning_tokens += usage.reasoning_tokens;
        totals.cached_tokens += usage.cache_read_tokens;
        record.add_known_cost(&mut totals.cost);
        let n = totals.requests as f64;
        totals.avg_latency_ms += (record.latency_ms as f64 - totals.avg_latency_ms) / n;

        self.per_kind
            .entry(record.kind.as_str().to_string())
            .or_insert_with(|| KindTotals {
                kind: record.kind.as_str().to_string(),
                ..KindTotals::default()
            })
            .observe(record);

        // The client bucket is the id actually stored on the record, not the
        // registry's canonical form re-derived here. They agree by
        // construction, and re-deriving would break the moment a caller folded
        // a high-cardinality id into the overflow bucket: the record would say
        // `unknown-agent-4417` while the aggregate said `custom-other`, and the
        // two totals would look like they did not match.
        let client_key = record.client.client_id.as_str().to_string();
        self.per_client
            .entry(client_key.clone())
            .or_insert_with(|| ClientTotals {
                client_id: client_key,
                client_name: client_display_name(&record.client),
                family: record.client.family.as_str().to_string(),
                source: record.client.source.as_str().to_string(),
                confidence: record.client.confidence.as_str().to_string(),
                ..ClientTotals::default()
            })
            .observe(record);

        match record.client.confidence {
            Confidence::Strong | Confidence::Probable => self.attribution.recognised += 1,
            Confidence::Weak => self.attribution.weak += 1,
            Confidence::Unknown => self.attribution.unknown += 1,
        }
    }

    /// Remove one record's contribution: the exact inverse of [`Self::observe`].
    ///
    /// Used when a durable settlement replaces a row the summary already
    /// counted. Sums and counters subtract; running averages reverse through
    /// their counts. `since` is left alone: a replacement carries its row's own
    /// timestamp unchanged, so the floor survives, and a real inconsistency
    /// self-corrects at the next rebuild from the table. A currency or
    /// dimension that falls back to zero is dropped, keeping the shape a
    /// from-scratch rebuild would have produced.
    pub(crate) fn unobserve(&mut self, record: &RequestRecord) {
        self.requests = self.requests.saturating_sub(1);
        if !record.ok {
            self.failures = self.failures.saturating_sub(1);
        }
        let usage = record.observed_usage();
        self.input_tokens = self.input_tokens.saturating_sub(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_sub(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_sub(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_sub(usage.cache_write_tokens);
        self.unpriced_requests = self
            .unpriced_requests
            .saturating_sub(u64::from(record.unpriced()));
        subtract_known_cost(&mut self.cost, record);

        let key = record
            .resolved_model
            .clone()
            .unwrap_or_else(|| UNRESOLVED_MODEL_KEY.to_string());
        let drop_model = self
            .per_model
            .get_mut(&key)
            .is_none_or(|totals| unobserve_model_totals(totals, record));
        if drop_model {
            self.per_model.remove(&key);
        }
        let drop_kind = self
            .per_kind
            .get_mut(record.kind.as_str())
            .is_none_or(|totals| unobserve_kind_totals(totals, record));
        if drop_kind {
            self.per_kind.remove(record.kind.as_str());
        }
        let client_key = record.client.client_id.as_str().to_string();
        let drop_client = self
            .per_client
            .get_mut(&client_key)
            .is_none_or(|totals| unobserve_client_totals(totals, record));
        if drop_client {
            self.per_client.remove(&client_key);
        }

        match record.client.confidence {
            Confidence::Strong | Confidence::Probable => {
                self.attribution.recognised = self.attribution.recognised.saturating_sub(1)
            }
            Confidence::Weak => self.attribution.weak = self.attribution.weak.saturating_sub(1),
            Confidence::Unknown => {
                self.attribution.unknown = self.attribution.unknown.saturating_sub(1)
            }
        }
    }
}

/// Mirror of [`RequestRecord::add_known_cost`] for taking a row back out.
fn subtract_known_cost(totals: &mut CostTotals, record: &RequestRecord) {
    let mut subtract = |currency: &str, amount: f64| {
        let remaining = totals.0.get(currency).copied().unwrap_or(0.0) - amount;
        // Floating-point residue from many add/subtract pairs can leave dust
        // behind where a from-scratch rebuild would have no row at all.
        if remaining == 0.0 || remaining.abs() < 1e-9 {
            totals.0.remove(currency);
        } else {
            totals.0.insert(currency.to_string(), remaining);
        }
    };
    if let Some(accounting) = &record.accounting {
        for (currency, amount) in &accounting.known_cost.0 {
            subtract(currency, *amount);
        }
    } else if let Some(cost) = &record.cost {
        subtract(&cost.currency, cost.amount);
    }
}

/// Fold one record out of a model bucket. Returns true when the bucket is
/// empty and must be dropped, the way a from-scratch rebuild would.
fn unobserve_model_totals(totals: &mut ModelTotals, record: &RequestRecord) -> bool {
    let usage = record.observed_usage();
    if totals.requests > 0 {
        let n = totals.requests as f64;
        if totals.requests >= 2 {
            totals.avg_latency_ms =
                (totals.avg_latency_ms * n - record.latency_ms as f64) / (n - 1.0);
        } else {
            totals.avg_latency_ms = 0.0;
        }
        totals.requests -= 1;
    }
    totals.failures = totals.failures.saturating_sub(u64::from(!record.ok));
    totals.input_tokens = totals.input_tokens.saturating_sub(usage.input_tokens);
    totals.output_tokens = totals.output_tokens.saturating_sub(usage.output_tokens);
    totals.reasoning_tokens = totals
        .reasoning_tokens
        .saturating_sub(usage.reasoning_tokens);
    totals.cached_tokens = totals.cached_tokens.saturating_sub(usage.cache_read_tokens);
    subtract_known_cost(&mut totals.cost, record);
    totals.requests == 0
}

/// Fold one record out of a kind bucket, as [`KindTotals::observe`] built it.
fn unobserve_kind_totals(totals: &mut KindTotals, record: &RequestRecord) -> bool {
    let usage = record.observed_usage();
    if totals.requests > 0 {
        let n = totals.requests as f64;
        if totals.requests >= 2 {
            totals.avg_latency_ms =
                (totals.avg_latency_ms * n - record.latency_ms as f64) / (n - 1.0);
        } else {
            totals.avg_latency_ms = 0.0;
        }
        totals.requests -= 1;
    }
    totals.failures = totals.failures.saturating_sub(u64::from(!record.ok));
    totals.input_tokens = totals.input_tokens.saturating_sub(usage.input_tokens);
    totals.output_tokens = totals.output_tokens.saturating_sub(usage.output_tokens);
    totals.requests == 0
}

/// Fold one record out of a client bucket, as [`ClientTotals::observe`] built
/// it. Returns true when the bucket is empty and must be dropped.
fn unobserve_client_totals(totals: &mut ClientTotals, record: &RequestRecord) -> bool {
    let usage = record.observed_usage();
    if totals.requests > 0 {
        let n = totals.requests as f64;
        if totals.requests >= 2 {
            totals.avg_latency_ms =
                (totals.avg_latency_ms * n - record.latency_ms as f64) / (n - 1.0);
        } else {
            totals.avg_latency_ms = 0.0;
        }
        totals.requests -= 1;
    }
    totals.failures = totals.failures.saturating_sub(u64::from(!record.ok));
    totals.input_tokens = totals.input_tokens.saturating_sub(usage.input_tokens);
    totals.output_tokens = totals.output_tokens.saturating_sub(usage.output_tokens);
    totals.cache_read_tokens = totals
        .cache_read_tokens
        .saturating_sub(usage.cache_read_tokens);
    totals.cache_write_tokens = totals
        .cache_write_tokens
        .saturating_sub(usage.cache_write_tokens);
    totals.reasoning_tokens = totals
        .reasoning_tokens
        .saturating_sub(usage.reasoning_tokens);
    subtract_known_cost(&mut totals.cost, record);
    if let Some(ttft) = record.ttft_ms {
        // observe() folded the first token in as the seen-th streamed request;
        // take it back out through the same running average.
        let seen = totals.streamed;
        if seen > 0 {
            totals.streamed -= 1;
            totals.avg_ttft_ms = if seen == 1 {
                None
            } else {
                Some(
                    (totals.avg_ttft_ms.unwrap_or(0.0) * seen as f64 - ttft as f64)
                        / (seen as f64 - 1.0),
                )
            };
        }
    }
    totals.requests == 0
}

/// How much of the traffic could be attributed to a client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AttributionSummary {
    /// Requests attributed to a named client with strong or probable evidence.
    pub recognised: u64,
    /// Requests attributed to a runtime or library but not to a product.
    pub weak: u64,
    /// Requests nothing could be attributed to.
    pub unknown: u64,
}

impl AttributionSummary {
    /// Share of `requests` that reached strong or probable, `0.0..=1.0`.
    ///
    /// `0.0` when there is no traffic at all, so a dashboard shows 0% rather
    /// than `NaN` from `0/0`.
    pub fn recognised_share(&self) -> f64 {
        self.share(self.recognised)
    }

    /// Share of `requests` that reached a weak runtime family, `0.0..=1.0`.
    pub fn weak_share(&self) -> f64 {
        self.share(self.weak)
    }

    /// Share of `requests` that could not be attributed, `0.0..=1.0`.
    pub fn unknown_share(&self) -> f64 {
        self.share(self.unknown)
    }

    /// Total requests the three buckets account for.
    pub fn total(&self) -> u64 {
        self.recognised + self.weak + self.unknown
    }

    fn share(&self, part: u64) -> f64 {
        let total = self.total();
        if total == 0 {
            0.0
        } else {
            part as f64 / total as f64
        }
    }
}

#[derive(Debug)]
pub struct Stats {
    inner: Mutex<Inner>,
    // Authenticated chat arrival, independent of completion, log eviction and
    // "clear activity". Contains only client ids and timestamps, never payloads.
    observed_clients: Mutex<BTreeMap<ClientId, DateTime<Utc>>>,
}

/// The single `per_model` key every unresolved request shares.
///
/// A request whose model could not be resolved still has to be counted, but its
/// name came from the caller and a caller can invent a new one on every call.
/// Keeping those names as permanent dimensions would let the summary — which the
/// dashboard polls and which is not bounded by the request log — grow without
/// limit, so unresolved requests land in one bucket instead.
const UNRESOLVED_MODEL_KEY: &str = "unresolved";

#[derive(Debug)]
struct Inner {
    limit: usize,
    records: VecDeque<RequestRecord>,
    /// Every aggregate the summary reports. Held as one value so a refresh can
    /// adopt a durable aggregate wholesale, exactly as a rebuild would leave it.
    totals: DurableTotals,
    store: Option<Arc<crate::runtime_store::RuntimeStore>>,
    pending: Vec<RequestRecord>,
    settlement_pending: BTreeMap<String, RequestRecord>,
    revision: Option<i64>,
    synced: Option<Instant>,
    local_revision: i64,
}

impl Stats {
    pub fn new(limit: usize) -> Self {
        Stats {
            inner: Mutex::new(Inner {
                limit: limit.max(1),
                records: VecDeque::new(),
                totals: DurableTotals::default(),
                store: None,
                pending: Vec::new(),
                settlement_pending: BTreeMap::new(),
                revision: None,
                synced: None,
                local_revision: 0,
            }),
            observed_clients: Mutex::new(BTreeMap::new()),
        }
    }

    /// Note an authenticated chat request at ingress, even if its upstream
    /// fails or its stream is still open. Health/model-list probes do not call
    /// this and therefore cannot confirm a takeover.
    pub fn observe_client(&self, client: &ClientFingerprint, at: DateTime<Utc>) {
        if !client.confidence.is_recognised() {
            return;
        }
        let mut observed = crate::sync::lock(&self.observed_clients);
        if let Some(previous) = observed.get_mut(&client.client_id) {
            *previous = (*previous).max(at);
        } else if observed.len() < MAX_TRACKED_CLIENTS {
            observed.insert(client.client_id.clone(), at);
        }
    }

    /// Whether this process saw a recognised client after a particular adopt.
    /// This is traffic attribution, not client identity authentication: header
    /// evidence remains probable, not strong.
    pub fn client_seen_since(&self, client_id: &str, since: DateTime<Utc>) -> bool {
        crate::sync::lock(&self.observed_clients)
            .get(&ClientId::trusted(client_id))
            .is_some_and(|at| *at > since)
    }

    /// A new adopt must not borrow earlier traffic, even if it happened in the
    /// same second as the manifest's backwards-compatible timestamp.
    pub fn forget_client_observation(&self, client_id: &str) {
        crate::sync::lock(&self.observed_clients).remove(&ClientId::trusted(client_id));
    }

    pub fn set_limit(&self, limit: usize) {
        let mut inner = crate::sync::lock(&self.inner);
        inner.limit = limit.max(1);
        while inner.records.len() > inner.limit {
            inner.records.pop_front();
            inner.local_revision = inner.local_revision.saturating_add(1);
        }
    }

    pub fn record(&self, record: RequestRecord) {
        let mut inner = crate::sync::lock(&self.inner);
        if inner.store.is_some() {
            inner.pending.push(record.clone());
        }
        Self::accumulate(&mut inner, record);
    }

    /// A transaction already committed usage **and** spend. Do not queue a
    /// second independent usage write. Normal local commits update in O(1).
    ///
    /// `inserted` means the settlement (and therefore its usage row) was newly
    /// committed by this call; a re-settled request changed nothing durable.
    /// The durable summary carries the committed row, so the projection only
    /// has to keep its own copy current: the record's pending copy, if the
    /// completion queued one, is dropped because the summary now counts it.
    /// When the revision chain holds, the in-memory aggregate is already
    /// right; when it does not, the next refresh adopts the summary in O(1).
    pub(crate) fn record_committed(
        &self,
        record: RequestRecord,
        before: i64,
        after: i64,
        inserted: bool,
    ) {
        let mut inner = crate::sync::lock(&self.inner);
        let was_pending = inner.settlement_pending.remove(&record.id).is_some();
        if inserted {
            inner.pending.retain(|queued| queued.id != record.id);
        }
        if inserted && !was_pending && inner.revision == Some(before) {
            Self::accumulate(&mut inner, record);
            inner.revision = Some(after);
            inner.synced = Some(Instant::now());
        } else {
            if let Some(live) = inner.records.iter_mut().find(|live| live.id == record.id) {
                *live = record;
            }
            inner.revision = None;
            if let Err(error) = Self::refresh_inner(&mut inner) {
                tracing::error!(%error, "committed accounting projection could not be refreshed");
            }
        }
    }

    pub(crate) fn record_settlement_pending(&self, record: RequestRecord) {
        let mut inner = crate::sync::lock(&self.inner);
        if inner
            .settlement_pending
            .insert(record.id.clone(), record.clone())
            .is_none()
        {
            Self::accumulate(&mut inner, record);
        }
        inner.revision = None;
    }

    fn accumulate(inner: &mut Inner, record: RequestRecord) {
        inner.local_revision = inner.local_revision.saturating_add(1);
        inner.totals.observe(&record);
        let limit = inner.limit;
        inner.records.push_back(record);
        while inner.records.len() > limit {
            inner.records.pop_front();
        }
    }

    /// Place a durable record in the ring without re-counting it.
    ///
    /// A refresh that adopted the durable summary already carries every
    /// committed row's contribution; only the recent view needs the record
    /// itself. The local revision still moves, so a non-durable history reader
    /// sees the same change signal a rebuild would have given it.
    fn project(inner: &mut Inner, record: RequestRecord) {
        inner.local_revision = inner.local_revision.saturating_add(1);
        let limit = inner.limit;
        inner.records.push_back(record);
        while inner.records.len() > limit {
            inner.records.pop_front();
        }
    }

    /// Most recent requests first.
    pub fn recent(&self, limit: usize) -> Vec<RequestRecord> {
        let mut inner = crate::sync::lock(&self.inner);
        Self::refresh_if_due(&mut inner);
        inner.records.iter().rev().take(limit).cloned().collect()
    }

    /// Bounded process-local summaries for lightweight surfaces. No SQLite
    /// refresh or keychain work is initiated while opening a tray panel.
    pub fn recent_local(&self, limit: usize) -> Vec<RequestRecord> {
        crate::sync::lock(&self.inner)
            .records
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn summary(&self) -> StatsSummary {
        let mut inner = crate::sync::lock(&self.inner);
        Self::refresh_if_due(&mut inner);
        StatsSummary::from(&inner.totals)
    }

    /// Refresh the durable projection immediately rather than waiting for its poll interval.
    pub fn refresh_history(&self) -> Result<(), String> {
        let mut inner = crate::sync::lock(&self.inner);
        Self::refresh_inner(&mut inner)
    }

    /// The last `days` local calendar days, oldest first.
    ///
    /// Every day in the window is present, including days with no traffic: the
    /// chart reads a zero as a quiet day and a missing day as a broken series.
    /// The window always ends today, so "the last 30 days" does not depend on
    /// when the page happened to load.
    ///
    /// The cap keeps a bad request from turning into an unbounded scan; the
    /// heatmap asks for 371, one year of weeks and a day.
    pub fn usage_days(&self, days: u32) -> Result<Vec<UsageDay>, String> {
        const MAX_DAYS: u32 = 400;
        let days = days.clamp(1, MAX_DAYS);
        let mut inner = crate::sync::lock(&self.inner);
        let today = chrono::Local::now().date_naive();
        let first = today - chrono::Duration::days(i64::from(days) - 1);
        Self::flush_pending(&mut inner)?;
        if inner.store.is_some() && !inner.settlement_pending.is_empty() {
            return Err("usage settlement is pending; retry after accounting recovery".into());
        }
        let records = if let Some(store) = inner.store.clone() {
            drop(inner);
            return store.usage_days(first, today);
        } else {
            inner.records.iter().cloned().collect::<Vec<_>>()
        };
        let mut buckets: BTreeMap<NaiveDate, UsageDay> = BTreeMap::new();
        let mut day = first;
        while day <= today {
            buckets.insert(
                day,
                UsageDay {
                    day: day.format("%Y-%m-%d").to_string(),
                    ..UsageDay::default()
                },
            );
            day += chrono::Duration::days(1);
        }

        for record in records {
            let local = record.at.with_timezone(&chrono::Local).date_naive();
            let Some(bucket) = buckets.get_mut(&local) else {
                continue;
            };
            bucket.observe(&record);
        }

        Ok(buckets.into_values().collect())
    }

    /// Attach before serving and restore totals beyond the bounded recent view.
    pub fn attach_store(
        &self,
        store: Arc<crate::runtime_store::RuntimeStore>,
    ) -> Result<(), String> {
        let limit = {
            let inner = crate::sync::lock(&self.inner);
            if inner.totals.requests != 0 {
                return Err("usage storage must be attached before serving".into());
            }
            inner.limit
        };
        let snapshot = store.usage_snapshot(None, limit)?;
        let mut inner = crate::sync::lock(&self.inner);
        if inner.totals.requests != 0 {
            return Err("usage storage must be attached before serving".into());
        }
        match snapshot {
            UsageSnapshot::Unchanged => {}
            UsageSnapshot::Summary {
                revision,
                totals,
                recent,
            } => {
                // The durable aggregate describes the whole table; only the
                // recent view has to be materialised.
                inner.totals = totals;
                for record in recent {
                    Self::project(&mut inner, record);
                }
                inner.revision = Some(revision);
                inner.synced = Some(Instant::now());
            }
            UsageSnapshot::Full { revision, records } => {
                let mut durable = DurableTotals::default();
                for record in &records {
                    durable.observe(record);
                }
                // Best effort: a failed write leaves the summary absent and the
                // next revision change rebuilds it again.
                if let Err(error) = store.save_usage_summary(revision, &durable) {
                    tracing::warn!(%error, "durable usage summary could not be written back");
                }
                for record in records {
                    Self::accumulate(&mut inner, record);
                }
                inner.revision = Some(revision);
                inner.synced = Some(Instant::now());
            }
        }
        inner.store = Some(store);
        Ok(())
    }

    fn flush_inner(inner: &mut Inner) -> Result<(), String> {
        Self::flush_pending(inner)?;
        Self::refresh_inner(inner)
    }

    fn flush_pending(inner: &mut Inner) -> Result<(), String> {
        if let Some(store) = &inner.store {
            if !inner.pending.is_empty() {
                let (before, after, inserted) = store.append_usage_versioned(&inner.pending)?;
                if inner.revision == Some(before) && inserted == inner.pending.len() {
                    inner.revision = Some(after);
                } else {
                    // Duplicate ids or another process changed history. The
                    // next refresh re-adopts the durable summary instead of
                    // trusting the in-memory revision chain.
                    inner.revision = None;
                    inner.synced = None;
                }
            }
            inner.pending.clear();
        }
        Ok(())
    }

    fn refresh_if_due(inner: &mut Inner) {
        if inner.store.is_none()
            || inner
                .synced
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
        {
            return;
        }
        if let Err(error) = Self::refresh_inner(inner) {
            tracing::warn!(%error, "shared usage history could not be refreshed");
            inner.synced = Some(Instant::now());
        }
    }

    fn refresh_inner(inner: &mut Inner) -> Result<(), String> {
        let Some(store) = &inner.store else {
            return Ok(());
        };
        let limit = inner.limit;
        match store.usage_snapshot(inner.revision, limit)? {
            UsageSnapshot::Unchanged => {}
            UsageSnapshot::Summary {
                revision,
                totals,
                recent,
            } => {
                // Keep live diagnostics in memory while durable accounting remains
                // redacted. Pending requests are not on disk yet and must survive.
                let mut live: std::collections::HashMap<_, _> = inner
                    .records
                    .iter()
                    .cloned()
                    .map(|record| (record.id.clone(), record))
                    .collect();
                let durable: std::collections::HashSet<_> =
                    recent.iter().map(|record| record.id.clone()).collect();
                let pending = inner.pending.clone();
                let unsettled: Vec<_> = inner.settlement_pending.values().cloned().collect();
                Self::reset_totals(inner);
                inner.totals = totals;
                for mut record in recent {
                    // Only ephemeral diagnostics come from the in-memory copy.
                    // A revised durable fact must replace old usage/cost/identity.
                    if let Some(previous) = live.remove(&record.id) {
                        record.error = previous.error;
                        record.routing_decision = previous.routing_decision;
                    }
                    Self::project(inner, record);
                }
                for record in pending {
                    if !durable.contains(&record.id) {
                        Self::accumulate(inner, record);
                    }
                }
                for record in unsettled {
                    if !durable.contains(&record.id) {
                        Self::accumulate(inner, record);
                    }
                }
                inner.revision = Some(revision);
                inner.synced = Some(Instant::now());
            }
            UsageSnapshot::Full { revision, records } => {
                /*
                 * The summary row is missing or stale: a database written
                 * before it existed, an older binary that writes usage without
                 * it, or a clear that dropped it. Rebuild once from the table
                 * and store the durable aggregate back, so every read after
                 * this one is a row read rather than a table scan.
                 */
                let mut durable = DurableTotals::default();
                for record in &records {
                    durable.observe(record);
                }
                if let Err(error) = store.save_usage_summary(revision, &durable) {
                    tracing::warn!(%error, "durable usage summary could not be written back");
                }
                // Keep live diagnostics in memory while durable accounting remains
                // redacted. Pending requests are not on disk yet and must survive.
                let mut live: std::collections::HashMap<_, _> = inner
                    .records
                    .iter()
                    .cloned()
                    .map(|record| (record.id.clone(), record))
                    .collect();
                let ids: std::collections::HashSet<_> =
                    records.iter().map(|record| record.id.clone()).collect();
                let pending = inner.pending.clone();
                let unsettled: Vec<_> = inner.settlement_pending.values().cloned().collect();
                Self::reset_totals(inner);
                for mut record in records {
                    // Only ephemeral diagnostics come from the in-memory copy.
                    // A revised durable fact must replace old usage/cost/identity.
                    if let Some(previous) = live.remove(&record.id) {
                        record.error = previous.error;
                        record.routing_decision = previous.routing_decision;
                    }
                    Self::accumulate(inner, record);
                }
                for record in pending {
                    if !ids.contains(&record.id) {
                        Self::accumulate(inner, record);
                    }
                }
                for record in unsettled {
                    if !ids.contains(&record.id) {
                        Self::accumulate(inner, record);
                    }
                }
                inner.revision = Some(revision);
                inner.synced = Some(Instant::now());
            }
        }
        Ok(())
    }

    /// A failed write keeps the queue intact for the next tick or shutdown.
    pub fn flush(&self) -> Result<(), String> {
        Self::flush_inner(&mut crate::sync::lock(&self.inner))
    }

    pub fn export_history(&self) -> Result<Vec<RequestRecord>, String> {
        let mut inner = crate::sync::lock(&self.inner);
        Self::flush_pending(&mut inner)?;
        if inner.store.is_some() && !inner.settlement_pending.is_empty() {
            return Err("usage settlement is pending; retry after accounting recovery".into());
        }
        if let Some(store) = inner.store.clone() {
            drop(inner);
            store.usage()
        } else {
            Ok(inner.records.iter().cloned().collect())
        }
    }

    /// Imported accounting does not change budgets, router health or ML training.
    pub fn import_history(&self, records: Vec<RequestRecord>) -> Result<usize, String> {
        if records.len() > 100_000 {
            return Err("import at most 100000 usage records at a time".into());
        }
        for record in &records {
            if record.id.is_empty()
                || record.id.len() > 256
                || record.client.client_id.as_str().len() > 256
                || record.requested_model.len() > 512
                || record.cost.as_ref().is_some_and(|cost| {
                    !cost.amount.is_finite()
                        || cost.amount < 0.0
                        || cost.currency.trim().is_empty()
                        || cost.currency != cost.currency.trim().to_ascii_uppercase()
                })
            {
                return Err("the usage archive contains an invalid record".into());
            }
            if let Some(accounting) = &record.accounting {
                if accounting.schema_version != crate::accounting::SETTLEMENT_SCHEMA_VERSION
                    || accounting.known_cost.0.iter().any(|(currency, amount)| {
                        currency.trim().is_empty()
                            || *currency != currency.trim().to_ascii_uppercase()
                            || !amount.is_finite()
                            || *amount < 0.0
                    })
                    || accounting
                        .observed_usage
                        .cache_read_tokens
                        .checked_add(accounting.observed_usage.cache_write_tokens)
                        .is_none_or(|cached| cached > accounting.observed_usage.input_tokens)
                    || accounting.observed_usage.reasoning_tokens
                        > accounting.observed_usage.output_tokens
                    || [
                        accounting.observed_usage.input_tokens,
                        accounting.observed_usage.output_tokens,
                        accounting.observed_usage.cache_read_tokens,
                        accounting.observed_usage.cache_write_tokens,
                        accounting.observed_usage.reasoning_tokens,
                    ]
                    .into_iter()
                    .any(|tokens| tokens > u64::from(u32::MAX) * u64::from(record.attempts.max(1)))
                {
                    return Err("the usage archive contains invalid accounting totals".into());
                }
            }
        }
        let mut inner = crate::sync::lock(&self.inner);
        Self::flush_inner(&mut inner)?;
        let store = inner
            .store
            .clone()
            .ok_or("durable usage storage is unavailable")?;
        let count = store.append_usage(&records)?;
        Self::refresh_inner(&mut inner)?;
        Ok(count)
    }

    pub fn clear_history(&self, client: Option<&str>) -> Result<(), String> {
        let mut inner = crate::sync::lock(&self.inner);
        Self::flush_inner(&mut inner)?;
        if inner
            .settlement_pending
            .values()
            .any(|record| client.is_none_or(|client| record.client.client_id.as_str() == client))
        {
            return Err(
                "usage settlement is pending; retry clearing after accounting recovery".into(),
            );
        }
        let retained: Vec<RequestRecord> = match &inner.store {
            Some(store) => {
                store.clear_usage(client)?;
                Self::refresh_inner(&mut inner)?;
                return Ok(());
            }
            None => inner
                .records
                .iter()
                .filter(|r| client.is_some_and(|id| r.client.client_id.as_str() != id))
                .cloned()
                .collect(),
        };
        Self::reset_totals(&mut inner);
        for record in retained {
            Self::accumulate(&mut inner, record);
        }
        Ok(())
    }

    fn reset_totals(inner: &mut Inner) {
        inner.local_revision = inner.local_revision.saturating_add(1);
        inner.records.clear();
        inner.totals = DurableTotals::default();
    }

    pub fn clear(&self) {
        if let Err(error) = self.clear_history(None) {
            tracing::warn!(%error, "usage history could not be cleared");
        }
    }
}

impl Default for Stats {
    fn default() -> Self {
        Stats::new(500)
    }
}

/// Bounded log of the terminal [`Outcome`] built for each request.
///
/// The request log above answers "what happened to this request"; this keeps
/// the accepted outcome the lifecycle built for it, so the planned /
/// last-attempted / served identity distinction and the classified terminal
/// state stay available without a second accounting path. It is a ring buffer
/// like the request log: same reason, same bound, same in-memory-only promise.
#[derive(Debug)]
pub struct OutcomeLog {
    inner: Mutex<OutcomeLogInner>,
}

#[derive(Debug)]
struct OutcomeLogInner {
    limit: usize,
    outcomes: VecDeque<Outcome>,
}

impl OutcomeLog {
    pub fn new(limit: usize) -> Self {
        OutcomeLog {
            inner: Mutex::new(OutcomeLogInner {
                limit: limit.max(1),
                outcomes: VecDeque::new(),
            }),
        }
    }

    /// Append one request's terminal outcome. The lifecycle guarantees it calls
    /// this exactly once per request; the log does not second-guess that.
    pub fn record(&self, outcome: Outcome) {
        let mut inner = crate::sync::lock(&self.inner);
        let limit = inner.limit;
        inner.outcomes.push_back(outcome);
        while inner.outcomes.len() > limit {
            inner.outcomes.pop_front();
        }
    }

    /// Most recent outcomes first, newest at the head of the returned list.
    pub fn recent(&self, limit: usize) -> Vec<Outcome> {
        let inner = crate::sync::lock(&self.inner);
        inner.outcomes.iter().rev().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        crate::sync::lock(&self.inner).outcomes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        crate::sync::lock(&self.inner).outcomes.clear();
    }
}

impl Default for OutcomeLog {
    fn default() -> Self {
        OutcomeLog::new(500)
    }
}

/// Accumulates the facts about one in-flight request.
pub struct RecordBuilder {
    record: RequestRecord,
}

impl RecordBuilder {
    pub fn new(ingress: Dialect, requested_model: &str, stream: bool) -> Self {
        RecordBuilder {
            record: RequestRecord {
                id: format!("req_{}", uuid::Uuid::new_v4().simple()),
                at: Utc::now(),
                ingress,
                kind: RequestKind::Main,
                requested_model: requested_model.to_string(),
                resolved_model: None,
                provider_name: None,
                stream,
                status: 200,
                ok: true,
                error: None,
                latency_ms: 0,
                ttft_ms: None,
                usage: Usage::default(),
                cost: None,
                attempts: 0,
                escalated_from: None,
                routing_decision: None,
                client: ClientFingerprint::unknown(),
                account_id: None,
                inference_key_id: None,
                provider_id: None,
                model_name: None,
                upstream_declared: None,
                accounting: None,
            },
        }
    }

    /// Stamp the request's client fingerprint. Set once, before the record is
    /// finished.
    pub fn client(&mut self, client: ClientFingerprint) -> &mut Self {
        self.record.client = client;
        self
    }

    pub fn inference_credential(
        &mut self,
        credential: Option<&crate::router::InferenceCredential>,
    ) {
        self.record.account_id = credential.map(|key| key.account_id.clone());
        self.record.inference_key_id = credential.map(|key| key.key_id.clone());
    }

    /// Stamp the request's kind. Set once, before the record is finished.
    pub fn kind(&mut self, kind: RequestKind) -> &mut Self {
        self.record.kind = kind;
        self
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn resolved(&mut self, model_id: &str, provider_name: &str) -> &mut Self {
        self.record.resolved_model = Some(model_id.to_string());
        self.record.provider_name = Some(provider_name.to_string());
        self
    }

    pub fn execution(&mut self, snapshot: &crate::accounting::ExecutionSnapshot) {
        self.record.provider_id = Some(snapshot.provider_id.clone());
        self.record.model_name = Some(snapshot.model_name.clone());
    }

    /// Record what the upstream's structured response declared it ran.
    ///
    /// Taken from the attempt that settled the request; per-attempt facts keep
    /// every other attempt's own declaration. `None` is honest for a record
    /// that predates the capture or never reached a response — it is never a
    /// stand-in for "the provider declared nothing".
    pub fn upstream_declared(
        &mut self,
        declared: Option<crate::ir::UpstreamModelDeclaration>,
    ) -> &mut Self {
        self.record.upstream_declared = declared;
        self
    }

    pub fn attempt(&mut self) -> &mut Self {
        self.record.attempts += 1;
        self
    }

    pub fn usage(&mut self, usage: Usage) -> &mut Self {
        self.record.usage = usage;
        self
    }

    /// Price the recorded usage. Called once the answering model is known, so a
    /// later price change never rewrites history.
    pub fn priced_with(&mut self, pricing: Option<&crate::billing::Pricing>) -> &mut Self {
        self.record.cost = pricing.map(|p| p.cost_of(&self.record.usage));
        self
    }

    pub fn ttft(&mut self, ms: u64) -> &mut Self {
        if self.record.ttft_ms.is_none() {
            self.record.ttft_ms = Some(ms);
        }
        self
    }

    pub fn ok(&mut self) -> &mut Self {
        self.record.ok = true;
        self.record.status = 200;
        self.record.error = None;
        self
    }

    /// Record the failure message shown in Activity.
    ///
    /// Prefer [`crate::error::Error::safe_message`] as the argument: it redacts
    /// a structured error without having to recognise the provider's body
    /// format. This method still guards itself, but only against this proxy's
    /// own `upstream … returned … : <body>` prefix, never against the shape of
    /// the body — see [`redact_upstream_body`].
    pub fn fail(&mut self, status: u16, error: String) -> &mut Self {
        self.record.ok = false;
        self.record.status = status;
        self.record.error = Some(redact_upstream_body(&error));
        self
    }

    /// Close the record and hand it over.
    ///
    /// Takes `&mut self` like every other builder step, so the request
    /// lifecycle can finish the record it owns without giving up the request
    /// state around it.
    pub fn finish(&mut self, latency_ms: u64) -> RequestRecord {
        self.record.latency_ms = latency_ms;
        self.record.clone()
    }

    /// Attach a routing decision trace for diagnostics.
    ///
    /// This is also where the record's own `escalated_from` comes from. The two
    /// are set together or not at all: a caller that hands over a decision
    /// cannot produce a record whose escalation field disagrees with it, and a
    /// record with an escalation field always carries the decision that explains
    /// it.
    pub fn routing_decision(&mut self, decision: RouteDecision) -> &mut Self {
        if let DecisionReason::Escalated { from_tier } = &decision.reason {
            self.record.escalated_from = Some(from_tier.clone());
        }
        self.record.routing_decision = Some(decision);
        self
    }
}

/// Strip an upstream response body out of an error string.
///
/// The `Display` form of an upstream error is
/// `upstream <provider> returned <status>: <body>`, and the body can be HTML,
/// JSON or plain text, with or without leading whitespace. Nothing here
/// inspects that body: the cut is made at the `": "` that this proxy itself
/// writes after `returned `, so every body shape is handled by the same rule.
/// `Error::safe_message` is still the primary redaction; this is the fallback
/// for a message that arrived as text.
fn redact_upstream_body(error: &str) -> String {
    if !error.starts_with("upstream ") {
        return error.to_string();
    }
    let Some(returned) = error.find("returned ") else {
        return error.to_string();
    };
    let status_start = returned + "returned ".len();
    match error[status_start..].find(": ") {
        Some(colon) => error[..status_start + colon].to_string(),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(model: &str, ok: bool, latency: u64, usage: Usage) -> RequestRecord {
        let mut b = RecordBuilder::new(Dialect::Anthropic, "sonnet-class", false);
        b.resolved(model, "DeepSeek").usage(usage).attempt();
        if !ok {
            b.fail(502, "boom".into());
        }
        b.finish(latency)
    }

    fn priced_rec(model: &str, usage: Usage, pricing: &crate::billing::Pricing) -> RequestRecord {
        let mut b = RecordBuilder::new(Dialect::OpenAI, "sonnet-class", false);
        b.resolved(model, "DeepSeek")
            .usage(usage)
            .priced_with(Some(pricing))
            .attempt();
        b.finish(100)
    }

    fn usd(amount: f64) -> CostTotals {
        let mut totals = CostTotals::default();
        totals.add(&Cost {
            currency: "USD".into(),
            amount,
        });
        totals
    }

    #[test]
    fn imported_history_adds_to_the_headline_without_inventing_a_split() {
        let mut summary = Stats::new(10).summary();
        summary.since = "2026-10-27T10:22:00Z".parse().unwrap();
        let mut totals = crate::history_import::ImportTotals {
            requests: 3,
            failures: 1,
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 10,
            unpriced_requests: 2,
            aggregate_requests: 1,
            suspected_requests: 4,
            cost: usd(1.5),
        };
        totals.cost.add(&Cost {
            currency: "CNY".into(),
            amount: 2.0,
        });

        summary.add_imported(&totals, NaiveDate::from_ymd_opt(2026, 5, 29));

        let mut expected = usd(1.5);
        expected.add(&Cost {
            currency: "CNY".into(),
            amount: 2.0,
        });

        assert_eq!(summary.requests, 3);
        assert_eq!(summary.failures, 1);
        assert_eq!(summary.input_tokens, 100);
        assert_eq!(summary.cache_read_tokens, 30, "cache stays inside input");
        assert_eq!(summary.unpriced_requests, 2);
        assert_eq!(summary.cost, expected, "currencies stay apart");
        assert_eq!(
            summary.since,
            crate::history_import::day_start_utc("2026-05-29").unwrap(),
            "the period moves back to the imported store's first counted day"
        );
        assert_eq!(
            totals.suspected_requests, 4,
            "suspected duplicates are not in the headline but are not hidden"
        );
        assert!(
            summary.per_client.is_empty(),
            "the imported store has no attribution evidence, so no client split is invented"
        );
    }

    #[test]
    fn imported_days_are_added_and_do_not_create_quiet_days() {
        let local = vec![UsageDay {
            day: "2026-01-02".into(),
            requests: 2,
            input_tokens: 10,
            cost: usd(1.0),
            ..UsageDay::default()
        }];
        let imported = vec![
            UsageDay {
                day: "2026-01-02".into(),
                requests: 5,
                failures: 1,
                input_tokens: 40,
                output_tokens: 8,
                cache_read_tokens: 4,
                cache_write_tokens: 2,
                unpriced_requests: 3,
                cost: usd(2.0),
            },
            // The imported store's first day; the local series has no row for it.
            UsageDay {
                day: "2026-01-01".into(),
                requests: 1,
                input_tokens: 3,
                ..UsageDay::default()
            },
        ];

        let merged = merge_imported_days(local, imported);

        assert_eq!(
            merged
                .iter()
                .map(|day| day.day.as_str())
                .collect::<Vec<_>>(),
            ["2026-01-01", "2026-01-02"]
        );
        let shared = &merged[1];
        assert_eq!(shared.requests, 7);
        assert_eq!(shared.failures, 1);
        assert_eq!(shared.input_tokens, 50);
        assert_eq!(shared.output_tokens, 8);
        assert_eq!(shared.cache_read_tokens, 4);
        assert_eq!(shared.unpriced_requests, 3);
        assert_eq!(shared.cost.get("USD"), 3.0);
        assert_eq!(
            merged[0].requests, 1,
            "a day only the imported store knows is the imported count, not a zero"
        );
    }

    #[test]
    fn cost_is_recorded_per_request_and_summed_per_currency() {
        let stats = Stats::new(10);
        let usd = crate::billing::Pricing::new("USD", 3.0, 15.0);
        let cny = crate::billing::Pricing::new("CNY", 2.0, 8.0);
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Usage::default()
        };

        stats.record(priced_rec("gpt", usage, &usd));
        stats.record(priced_rec("gpt", usage, &usd));
        stats.record(priced_rec("deepseek", usage, &cny));
        // An unpriced model contributes nothing rather than zero-cost noise.
        stats.record(rec("mystery", true, 10, usage));

        let summary = stats.summary();
        assert!((summary.cost.get("USD") - 6.0).abs() < 1e-9);
        assert!((summary.cost.get("CNY") - 2.0).abs() < 1e-9);
        assert_eq!(summary.cost.0.len(), 2, "currencies stay apart");

        let gpt = summary
            .per_model
            .iter()
            .find(|m| m.model_id == "gpt")
            .unwrap();
        assert!((gpt.cost.get("USD") - 6.0).abs() < 1e-9);
        let mystery = summary
            .per_model
            .iter()
            .find(|m| m.model_id == "mystery")
            .unwrap();
        assert!(mystery.cost.is_empty());
        assert!(stats.recent(1)[0].cost.is_none());
    }

    #[test]
    fn observe_then_unobserve_restores_the_empty_aggregate() {
        let pricing = crate::billing::Pricing::new("USD", 3.0, 15.0);
        let mut builder = RecordBuilder::new(Dialect::OpenAI, "sonnet-class", false);
        builder
            .resolved("m", "DeepSeek")
            .usage(Usage {
                input_tokens: 1_000,
                output_tokens: 40,
                cache_read_tokens: 900,
                cache_write_tokens: 10,
                reasoning_tokens: 5,
            })
            .priced_with(Some(&pricing))
            .attempt()
            .ttft(120);
        let streamed = builder.finish(800);
        let failed = rec("m", false, 320, Usage::default());
        let mut other = rec("other", true, 40, Usage::default());
        other.kind = RequestKind::Side(crate::query::SideQueryKind::AutoMode);

        let mut totals = DurableTotals::default();
        for record in [&streamed, &failed, &other] {
            totals.observe(record);
        }
        assert_eq!(totals.requests, 3);
        assert_eq!(totals.failures, 1);
        assert!(!totals.cost.is_empty());
        assert_eq!(totals.per_model.len(), 2);
        assert_eq!(totals.per_kind.len(), 2);
        assert_eq!(totals.per_client.len(), 1);
        assert_eq!(totals.attribution.unknown, 3);

        for record in [&streamed, &failed, &other] {
            totals.unobserve(record);
        }
        assert_eq!(totals.requests, 0);
        assert_eq!(totals.failures, 0);
        assert_eq!(totals.input_tokens, 0);
        assert_eq!(totals.output_tokens, 0);
        assert_eq!(totals.cache_read_tokens, 0);
        assert_eq!(totals.cache_write_tokens, 0);
        assert_eq!(totals.unpriced_requests, 0);
        assert!(totals.cost.is_empty(), "reversed spend leaves no dust rows");
        assert!(
            totals.per_model.is_empty(),
            "a bucket with no requests does not survive"
        );
        assert!(totals.per_kind.is_empty());
        assert!(totals.per_client.is_empty());
        assert_eq!(totals.attribution, AttributionSummary::default());
    }

    #[test]
    fn unobserving_a_replacement_reflects_only_the_settled_row() {
        // The settle path replaces a provisional row: out with the old
        // contribution, in with the new, in one summary step.
        let before = rec(
            "m",
            true,
            100,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
        );
        let after = rec(
            "m",
            true,
            300,
            Usage {
                input_tokens: 1_000,
                output_tokens: 50,
                ..Usage::default()
            },
        );
        let mut totals = DurableTotals::default();
        totals.observe(&before);
        totals.unobserve(&before);
        totals.observe(&after);
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.input_tokens, 1_000);
        assert_eq!(totals.output_tokens, 50);
        let model = &totals.per_model["m"];
        assert_eq!(model.requests, 1);
        assert!((model.avg_latency_ms - 300.0).abs() < 0.001);
    }

    #[test]
    fn refresh_adopts_the_durable_summary_and_matches_the_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.sqlite3");
        let store = Arc::new(crate::runtime_store::RuntimeStore::open(&path).unwrap());

        let stats = Stats::new(10);
        stats.attach_store(Arc::clone(&store)).unwrap();

        // The completion queues the record; the durable write publishes it.
        // The append folds the row into the durable summary in its own
        // transaction, and the flush's revision chain goes stale because the
        // summary moved underneath it — the situation every poll runs into.
        let record = rec(
            "settled",
            true,
            100,
            Usage {
                input_tokens: 42,
                output_tokens: 7,
                ..Usage::default()
            },
        );
        stats.record(record.clone());
        store.append_usage(std::slice::from_ref(&record)).unwrap();
        stats.flush().unwrap();
        let fast = stats.summary();
        assert_eq!(fast.requests, 1);

        // A refresh that cannot trust its revision adopts the durable summary
        // (one row plus the recent window) instead of re-reading the table.
        // The totals and the recent view must not move.
        {
            let mut inner = crate::sync::lock(&stats.inner);
            inner.revision = None;
            Stats::refresh_inner(&mut inner).unwrap();
        }
        let refreshed = stats.summary();
        assert_eq!(refreshed.requests, fast.requests);
        assert_eq!(refreshed.input_tokens, fast.input_tokens);
        assert_eq!(refreshed.output_tokens, fast.output_tokens);
        let model = refreshed
            .per_model
            .iter()
            .find(|m| m.model_id == "settled")
            .unwrap();
        assert!((model.avg_latency_ms - 100.0).abs() < 0.001);
        assert_eq!(stats.recent(10).len(), 1);
        assert_eq!(stats.recent(10)[0].id, record.id);
    }

    #[test]
    fn a_committed_settlement_replaces_the_pending_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.sqlite3");
        let store = Arc::new(crate::runtime_store::RuntimeStore::open(&path).unwrap());
        let stats = Stats::new(10);
        stats.attach_store(Arc::clone(&store)).unwrap();

        let record = rec(
            "mixed",
            true,
            50,
            Usage {
                input_tokens: 20,
                ..Usage::default()
            },
        );
        stats.record(record.clone());
        {
            let inner = crate::sync::lock(&stats.inner);
            assert_eq!(inner.pending.len(), 1);
        }
        // The durable write publishes the row and its summary contribution;
        // the settlement commit callback then drops the queued copy, because
        // the summary counts the durable row now.
        store.append_usage(std::slice::from_ref(&record)).unwrap();
        let after = store.usage_revision().unwrap();
        stats.record_committed(record, after - 1, after, true);
        {
            let inner = crate::sync::lock(&stats.inner);
            assert!(
                inner.pending.is_empty(),
                "a committed row must not stay queued for the next flush"
            );
        }
        {
            let mut inner = crate::sync::lock(&stats.inner);
            inner.revision = None;
            Stats::refresh_inner(&mut inner).unwrap();
        }
        assert_eq!(stats.summary().requests, 1);
    }

    #[test]
    fn a_legacy_raw_user_agent_is_named_without_rewriting_the_record() {
        let legacy = "codex_desktop_0.166.0-alpha.5.1_windows_10.0.26200_x86_64_unknown";
        let stats = Stats::new(10);
        let mut builder = RecordBuilder::new(Dialect::OpenAI, "gpt-5", false);
        builder.client(ClientFingerprint {
            client_id: ClientId::trusted(legacy),
            client_name: legacy.to_string(),
            family: crate::client::ClientFamily::Unknown,
            source: crate::client::AttributionSource::RuntimeFamily,
            confidence: Confidence::Weak,
        });
        let record = builder.finish(10);
        // Persistence keeps what was written; naming happens at aggregation.
        assert_eq!(record.client.client_name, legacy);
        stats.record(record);

        let totals = &stats.summary().per_client[0];
        assert_eq!(
            totals.client_id, legacy,
            "the bucket keeps the identity stored on the record"
        );
        assert_eq!(
            totals.client_name, "Codex Desktop",
            "a raw user agent a shipped rule knows is shown by its product name"
        );
        assert_eq!(totals.family, "unknown", "the family is not invented");
        assert_eq!(totals.confidence, "weak", "the confidence is not raised");
    }

    #[test]
    fn a_named_client_keeps_the_name_it_recorded() {
        let stats = Stats::new(10);
        let mut builder = RecordBuilder::new(Dialect::Anthropic, "sonnet-class", false);
        builder.client(ClientFingerprint {
            client_id: ClientId::trusted("claude_code"),
            client_name: "Claude Code".into(),
            family: crate::client::ClientFamily::Cli,
            source: crate::client::AttributionSource::UserAgent,
            confidence: Confidence::Probable,
        });
        stats.record(builder.finish(10));
        let totals = &stats.summary().per_client[0];
        assert_eq!(totals.client_name, "Claude Code");
        assert_eq!(totals.client_id, "claude_code");
    }

    #[test]
    fn unresolved_model_names_share_one_bounded_dimension() {
        // The request log holds one record, so nothing about the table may grow
        // with the number of distinct names a caller invents.
        let stats = Stats::new(1);
        for i in 0..5_000 {
            let mut b =
                RecordBuilder::new(Dialect::OpenAI, &format!("unknown-client-model-{i}"), false);
            b.fail(404, "no candidate for this model".into());
            stats.record(b.finish(5));
        }

        let summary = stats.summary();
        // The totals still count every request...
        assert_eq!(summary.requests, 5_000);
        assert_eq!(summary.failures, 5_000);
        // ...but the dimensions are bounded, and the log limit still holds.
        assert_eq!(
            summary.per_model.len(),
            1,
            "unresolved names must not become 5000 permanent dimensions"
        );
        assert_eq!(summary.per_model[0].model_id, "unresolved");
        assert_eq!(summary.per_model[0].requests, 5_000);
        assert_eq!(summary.per_model[0].failures, 5_000);
        assert_eq!(stats.recent(10).len(), 1);

        // A model that did resolve keeps its own dimension.
        stats.record(rec("resolved-model", true, 10, Usage::default()));
        let summary = stats.summary();
        assert_eq!(summary.per_model.len(), 2);
        assert!(summary
            .per_model
            .iter()
            .any(|m| m.model_id == "resolved-model"));
        assert!(summary.per_model.iter().any(|m| m.model_id == "unresolved"));
    }

    #[test]
    fn clearing_resets_the_spend_too() {
        let stats = Stats::new(4);
        let pricing = crate::billing::Pricing::new("USD", 3.0, 15.0);
        stats.record(priced_rec(
            "m",
            Usage {
                input_tokens: 1_000_000,
                ..Usage::default()
            },
            &pricing,
        ));
        assert!(!stats.summary().cost.is_empty());
        stats.clear();
        assert!(stats.summary().cost.is_empty());
    }

    #[test]
    fn shared_history_reaches_every_instance_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.sqlite3");
        let first = Stats::new(10);
        let second = Stats::new(10);
        let open = || std::sync::Arc::new(crate::runtime_store::RuntimeStore::open(&path).unwrap());
        first.attach_store(open()).unwrap();
        second.attach_store(open()).unwrap();

        first.record(rec("shared", true, 10, Usage::default()));
        first.flush().unwrap();
        {
            let mut inner = crate::sync::lock(&second.inner);
            Stats::refresh_inner(&mut inner).unwrap();
        }
        assert_eq!(second.summary().requests, 1);

        // The same durable id is a no-op, not a second request.
        let duplicate = first.recent(1);
        assert_eq!(second.import_history(duplicate).unwrap(), 0);
        assert_eq!(second.summary().requests, 1);
    }

    #[test]
    fn ring_buffer_is_bounded_and_newest_first() {
        let stats = Stats::new(2);
        for i in 0..5 {
            stats.record(rec(&format!("m{i}"), true, 10, Usage::default()));
        }
        let recent = stats.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].resolved_model.as_deref(), Some("m4"));
        assert_eq!(recent[1].resolved_model.as_deref(), Some("m3"));
        // aggregates still count every request
        assert_eq!(stats.summary().requests, 5);
    }

    #[test]
    fn aggregates_tokens_failures_and_latency() {
        let stats = Stats::new(10);
        stats.record(rec(
            "deepseek-v4-pro",
            true,
            100,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                reasoning_tokens: 2,
                ..Usage::default()
            },
        ));
        stats.record(rec(
            "deepseek-v4-pro",
            false,
            300,
            Usage {
                input_tokens: 1,
                output_tokens: 0,
                ..Usage::default()
            },
        ));
        let s = stats.summary();
        assert_eq!(s.requests, 2);
        assert_eq!(s.failures, 1);
        assert_eq!(s.input_tokens, 11);
        assert_eq!(s.output_tokens, 5);
        let m = &s.per_model[0];
        assert_eq!(m.model_id, "deepseek-v4-pro");
        assert_eq!(m.requests, 2);
        assert_eq!(m.failures, 1);
        assert_eq!(m.reasoning_tokens, 2);
        assert!((m.avg_latency_ms - 200.0).abs() < 0.001);
    }

    #[test]
    fn clear_resets_everything() {
        let stats = Stats::new(4);
        stats.record(rec("m", true, 1, Usage::default()));
        stats.clear();
        assert!(stats.recent(4).is_empty());
        assert_eq!(stats.summary().requests, 0);
        assert!(stats.summary().per_model.is_empty());
    }

    #[test]
    fn kind_totals_separate_main_from_side_queries() {
        let stats = Stats::new(10);
        // Same model, two kinds: the per-model row must not be able to say
        // which traffic was which, the per-kind rows must.
        let main_rec = {
            let mut b = RecordBuilder::new(Dialect::Anthropic, "claude-opus-4-8[1m]", false);
            b.kind(RequestKind::Main);
            b.resolved("zai-glm", "Z.ai").attempt();
            b.usage(Usage {
                input_tokens: 100,
                output_tokens: 5,
                ..Usage::default()
            });
            b.finish(600)
        };
        let classifier_rec = {
            let mut b = RecordBuilder::new(Dialect::Anthropic, "claude-opus-4-8[1m]", false);
            b.kind(RequestKind::Side(crate::query::SideQueryKind::AutoMode));
            b.resolved("zai-glm", "Z.ai").attempt();
            b.usage(Usage {
                input_tokens: 400,
                output_tokens: 20,
                ..Usage::default()
            });
            b.finish(100)
        };
        stats.record(main_rec);
        stats.record(classifier_rec.clone());
        stats.record(classifier_rec);

        let summary = stats.summary();
        let main = summary.per_kind.iter().find(|k| k.kind == "main").unwrap();
        let auto = summary
            .per_kind
            .iter()
            .find(|k| k.kind == "auto_mode")
            .unwrap();
        assert_eq!(main.requests, 1);
        assert_eq!(main.input_tokens, 100);
        assert!((main.avg_latency_ms - 600.0).abs() < 0.001);
        assert_eq!(auto.requests, 2);
        assert_eq!(auto.input_tokens, 800);
        assert!((auto.avg_latency_ms - 100.0).abs() < 0.001);
        // Model totals stay whole: both kinds hit the same model row.
        let model = summary
            .per_model
            .iter()
            .find(|m| m.model_id == "zai-glm")
            .unwrap();
        assert_eq!(model.requests, 3);

        stats.clear();
        assert!(stats.summary().per_kind.is_empty());
    }

    #[test]
    fn old_records_without_a_kind_deserialize_as_main() {
        // A record serialized before the field existed still loads.
        let legacy = serde_json::json!({
            "id": "req_x", "at": "2026-01-01T00:00:00Z", "ingress": "anthropic",
            "requested_model": "m", "resolved_model": null, "provider_name": null,
            "stream": false, "status": 200, "ok": true, "error": null,
            "latency_ms": 10, "ttft_ms": null,
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_read_tokens": 0,
                      "cache_write_tokens": 0, "reasoning_tokens": 0},
            "cost": null, "attempts": 1
        });
        let rec: RequestRecord = serde_json::from_value(legacy).unwrap();
        assert_eq!(rec.kind, RequestKind::Main);
    }

    #[test]
    fn upstream_bodies_are_redacted_from_activity_errors() {
        let raw = "upstream Xiaomi MiMo returned 404: <html> <head><title>404 Not Found</title></head> <body>openresty</body></html>";
        assert_eq!(
            redact_upstream_body(raw),
            "upstream Xiaomi MiMo returned 404"
        );
        // The same rule has to hold whatever the body is written in, because a
        // provider is free to answer a JSON or plain-text error page instead.
        for body in [
            "{\"error\":{\"message\":\"invalid key sk-SYNTHETIC-TEST-ONLY\"}}",
            "invalid key sk-SYNTHETIC-TEST-ONLY",
            "\n\t  <html><body>openresty</body></html>",
            "   ",
        ] {
            let raw = format!("upstream test-provider returned 401: {body}");
            assert_eq!(
                redact_upstream_body(&raw),
                "upstream test-provider returned 401",
                "a {body:?} body must not survive into Activity"
            );
        }
        // `BadUpstreamPayload` has no status, but its payload is a body too.
        assert_eq!(
            redact_upstream_body(
                "upstream returned malformed data: {\"access_token\":\"sk-SYNTHETIC\"}"
            ),
            "upstream returned malformed data"
        );
        // Non-upstream messages pass through untouched, colons included.
        assert_eq!(
            redact_upstream_body("stopped by a budget: the today limit is used up"),
            "stopped by a budget: the today limit is used up"
        );
        assert_eq!(redact_upstream_body("plain error"), "plain error");
    }

    #[test]
    fn builder_marks_failures_and_ttft() {
        let mut b = RecordBuilder::new(Dialect::OpenAI, "gpt-5.3-sol", true);
        b.attempt();
        b.attempt();
        b.ttft(42);
        b.ttft(99);
        b.fail(429, "rate limited".into());
        let r = b.finish(1234);
        assert_eq!(r.attempts, 2);
        assert_eq!(r.ttft_ms, Some(42));
        assert!(!r.ok);
        assert_eq!(r.status, 429);
        assert_eq!(r.latency_ms, 1234);
        assert!(r.id.starts_with("req_"));
    }

    #[test]
    fn usage_days_bucket_by_local_day_and_keep_quiet_days() {
        use chrono::TimeZone;

        let stats = Stats::new(100);
        let today = chrono::Local::now().date_naive();
        let at = |day: NaiveDate| {
            chrono::Local
                .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
                .single()
                .expect("noon exists on every local day")
                .with_timezone(&Utc)
        };

        let mut ok = rec(
            "m",
            true,
            10,
            Usage {
                input_tokens: 100,
                output_tokens: 10,
                ..Usage::default()
            },
        );
        ok.at = at(today);
        stats.record(ok);

        let mut failed = rec(
            "m",
            false,
            10,
            Usage {
                input_tokens: 5,
                output_tokens: 1,
                ..Usage::default()
            },
        );
        failed.at = at(today);
        stats.record(failed);

        let mut yesterday = rec(
            "m",
            true,
            10,
            Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            },
        );
        yesterday.at = at(today - chrono::Duration::days(1));
        stats.record(yesterday);

        // Outer than the window, so no bucket may count it.
        let mut old = rec("m", true, 10, Usage::default());
        old.at = at(today - chrono::Duration::days(10));
        stats.record(old);

        let days = stats.usage_days(3).unwrap();
        assert_eq!(days.len(), 3);
        assert_eq!(
            days[0].day,
            (today - chrono::Duration::days(2))
                .format("%Y-%m-%d")
                .to_string()
        );
        assert_eq!(days[0].requests, 0, "a quiet day is a zero, not a gap");
        assert_eq!(days[1].requests, 1);
        assert_eq!(days[1].input_tokens, 7);
        assert_eq!(days[2].day, today.format("%Y-%m-%d").to_string());
        assert_eq!(days[2].requests, 2);
        assert_eq!(days[2].failures, 1);
        assert_eq!(days[2].input_tokens, 105);
        assert_eq!(days[2].output_tokens, 11);
    }

    #[test]
    fn usage_days_answers_one_row_per_day_whatever_is_asked() {
        let stats = Stats::new(10);
        assert_eq!(stats.usage_days(0).unwrap().len(), 1);
        assert_eq!(stats.usage_days(5).unwrap().len(), 5);
        assert_eq!(stats.usage_days(u32::MAX).unwrap().len(), 400);
    }

    #[test]
    fn durable_views_report_pending_settlement_instead_of_silently_omitting_it() {
        let directory = tempfile::tempdir().unwrap();
        let stats = Stats::new(2);
        stats
            .attach_store(Arc::new(
                crate::runtime_store::RuntimeStore::open(&directory.path().join("usage.db"))
                    .unwrap(),
            ))
            .unwrap();
        stats.record_settlement_pending(rec("pending", false, 1, Usage::default()));
        assert_eq!(stats.summary().requests, 1);
        assert!(stats.usage_days(1).unwrap_err().contains("pending"));
        assert!(stats
            .query_history(&Default::default())
            .unwrap_err()
            .contains("pending"));
        assert!(stats.export_history().unwrap_err().contains("pending"));
        assert!(stats.clear_history(None).unwrap_err().contains("pending"));
        assert!(stats.clear_history(Some("another-client")).is_ok());
    }
}
