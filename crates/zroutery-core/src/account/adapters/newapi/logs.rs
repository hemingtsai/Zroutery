//! NewAPI log rows, and how each one is read as an account event.
//!
//! # The type mapping is transcribed, not remembered
//!
//! Every constant here comes from `model/log.go` at `QuantumNous/new-api` main:
//!
//! ```go
//! // don't use iota, avoid change log type value
//! const (
//!     LogTypeUnknown = 0
//!     LogTypeTopup   = 1
//!     LogTypeConsume = 2
//!     LogTypeManage  = 3
//!     LogTypeSystem  = 4
//!     LogTypeError   = 5
//!     LogTypeRefund  = 6
//!     LogTypeLogin   = 7
//! )
//! ```
//!
//! That comment upstream is the reason to transcribe rather than infer. The
//! values are pinned integers, not an enum whose order could shift, so a
//! mapping written from documentation can silently disagree with a live panel.
//! Several published descriptions of these endpoints disagree with each other on
//! exactly the values 3 and 4, which is what makes reading the source the only
//! defensible option.
//!
//! Two further facts from the same file shape what follows, and neither is
//! visible from the constants alone:
//!
//! * **`0` is a query sentinel, never a row.** `GetUserLogs` treats
//!   `logType == LogTypeUnknown` as "no type filter", so a panel answering
//!   `type=0` returns every type. It is therefore refused as an event by
//!   [`interpret`] rather than classified as something, because a row of type 0
//!   does not exist and a parser that invents one is inventing its input.
//! * **Not every type still lands in `logs`.** `RecordLogWithAdminInfo` routes
//!   `LogTypeManage` to the independent `AuditLog` table and returns without
//!   writing a row, and `RecordLoginLog` writes to the same table. So a `type=3`
//!   row from `/api/log/self` is a legacy record, and `type=7` does not cover
//!   current login activity. Neither is read as a complete audit trail.
//!
//! # Why `quota` is not a balance delta
//!
//! The single most important thing this module does *not* do is treat a row's
//! `quota` as an amount that moved the balance. The write sites in
//! `model/log.go` show why, and they disagree with each other:
//!
//! | type | writer | `quota` means |
//! |---|---|---|
//! | 2 consume | `RecordConsumeLog` | the computed charge — the one case where it is the movement |
//! | 5 error | `RecordErrorLog` | explicitly `0`; nothing settled |
//! | 1 topup | `RecordTopupLog` | left unset; the credited amount is *not* here |
//! | 4 system | `RecordLog` | left unset; a check-in reward is *not* here |
//! | 6 refund | task/billing writers | the returned amount |
//!
//! So for a check-in — the operation this subsystem exists to support — the log
//! row says `quota: 0` while the account really did receive a reward, and the
//! reward amount lives in a different endpoint entirely. Reading `quota` as a
//! delta would credit nothing for a payment that arrived. Every effect below is
//! therefore decided per type from the write site, and where the write site
//! carries no amount the effect is [`ResourceEffect::Unobserved`] rather than
//! zero.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::account::resource::{
    ObservedProviderAccounting, ObservedResourceEvent, ResourceAmount, ResourceEffect,
    ResourceEventKind, ResourceEventSource,
};
use crate::account::types::AccountId;

/// `model.LogTypeUnknown`. Also the "do not filter by type" value on the log
/// endpoints — see the module documentation.
pub const LOG_TYPE_UNKNOWN: i64 = 0;
/// `model.LogTypeTopup`: a commercial or administrative credit to the account.
pub const LOG_TYPE_TOPUP: i64 = 1;
/// `model.LogTypeConsume`: a model or task usage charge.
pub const LOG_TYPE_CONSUME: i64 = 2;
/// `model.LogTypeManage`: an administrative action, now largely in `AuditLog`.
pub const LOG_TYPE_MANAGE: i64 = 3;
/// `model.LogTypeSystem`: a system-level account event, including a check-in
/// reward.
pub const LOG_TYPE_SYSTEM: i64 = 4;
/// `model.LogTypeError`: a request or upstream failure.
pub const LOG_TYPE_ERROR: i64 = 5;
/// `model.LogTypeRefund`: a previously recorded charge given back.
pub const LOG_TYPE_REFUND: i64 = 6;
/// `model.LogTypeLogin`: a login, largely superseded by `AuditLog`.
pub const LOG_TYPE_LOGIN: i64 = 7;

/// `other.cache_tokens` — prompt tokens served from cache.
pub const META_CACHE_TOKENS: &str = "newapi.other.cache_tokens";
/// `other.cache_ratio` — the multiplier the panel applied to cached tokens.
pub const META_CACHE_RATIO: &str = "newapi.other.cache_ratio";
/// `other.model_ratio` — the multiplier the panel applied to this model.
pub const META_MODEL_RATIO: &str = "newapi.other.model_ratio";
/// `other.completion_ratio` — the multiplier the panel applied to output tokens.
pub const META_COMPLETION_RATIO: &str = "newapi.other.completion_ratio";
/// `other.group_ratio` — the multiplier the panel applied to this account group.
pub const META_GROUP_RATIO: &str = "newapi.other.group_ratio";
/// A system event that looks like a resource grant.
pub const META_SYSTEM_REWARD_SHAPED: &str = "newapi.system.reward_shaped";
/// A currency amount read out of a system event's text, in the instance's
/// *display* currency — not necessarily USD. Never a balance figure.
pub const META_SYSTEM_DISPLAY_AMOUNT: &str = "newapi.system.display_amount";
/// The currency symbol the amount above was rendered with.
pub const META_SYSTEM_DISPLAY_SYMBOL: &str = "newapi.system.display_symbol";
/// The token group the request was billed under.
pub const META_GROUP: &str = "newapi.group";
/// The API token the request was billed to.
pub const META_TOKEN_ID: &str = "newapi.token_id";
/// Seconds the request took, when the row is a request-scoped one.
pub const META_USE_TIME_SECS: &str = "newapi.use_time_secs";
/// Whether the request was streamed.
pub const META_IS_STREAM: &str = "newapi.is_stream";
/// The channel name, which the account log endpoint clears for user rows.
pub const META_CHANNEL_NAME: &str = "newapi.channel_name";
/// `newapi.log.raw_quota` — the row's own `quota`, kept verbatim.
pub const META_RAW_QUOTA: &str = "newapi.log.raw_quota";

/// One row of `GET /api/log/self`, as `model.Log` serialises it.
///
/// Field names and JSON keys are transcribed from the struct tags in
/// `model/log.go`. Two of them are worth noting because they are not what the Go
/// field is called: the channel id is serialised as `channel`, not `channel_id`,
/// and the metadata blob is a JSON *string* under `other`, not an object.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiLogItem {
    pub id: i64,
    pub created_at: i64,
    #[serde(rename = "type")]
    pub log_type: i64,
    pub content: String,
    pub username: String,
    pub token_name: String,
    pub model_name: String,
    pub quota: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub use_time: i64,
    pub is_stream: bool,
    #[serde(rename = "channel")]
    pub channel_id: i64,
    /// Always empty from the account log endpoint.
    ///
    /// `formatUserLogs` sets it to `""` before returning, so a panel that
    /// expected a channel name here would be reading a field the provider
    /// deliberately blanks.
    pub channel_name: String,
    pub token_id: i64,
    pub group: String,
    pub request_id: String,
    pub upstream_request_id: String,
    /// JSON-encoded metadata; the `other` column.
    pub other: String,
}

/// `data` of `GET /api/log/self`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiLogPage {
    pub total: i64,
    pub items: Vec<NewApiLogItem>,
}

/// The public part of a log row's `other` blob.
///
/// Every field is optional because `other` is written by many different code
/// paths and only a consumption row carries the ratio set. `model.LogOther`
/// also nests `admin_info`, `root_info` and `audit_info` for privileged
/// readers; `formatUserLogs` removes all three for `/api/log/self`, so they are
/// not modelled here — a user-scoped read cannot see them.
///
/// The ratios accept a number or a numeric string. The panel marshals them as
/// numbers, but the blob is a free-form string column that has carried several
/// encodings across versions, and a parse failure on a diagnostic field must not
/// cost the event it is attached to.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct NewApiLogOther {
    pub cache_tokens: Option<u64>,
    #[serde(deserialize_with = "flex_f64")]
    pub cache_ratio: Option<f64>,
    #[serde(deserialize_with = "flex_f64")]
    pub model_ratio: Option<f64>,
    #[serde(deserialize_with = "flex_f64")]
    pub completion_ratio: Option<f64>,
    #[serde(deserialize_with = "flex_f64")]
    pub group_ratio: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl NewApiLogOther {
    /// Parse a row's `other` column.
    ///
    /// Returns [`NewApiLogOther::default`] for an empty or unparseable blob.
    /// An unparseable blob is not an error: the ratios are optional detail on
    /// an event whose kind and effect are decided elsewhere, so refusing the
    /// whole row over its metadata would lose a real balance movement.
    pub fn parse(blob: &str) -> Self {
        let blob = blob.trim();
        if blob.is_empty() || blob == "{}" {
            return Self::default();
        }
        serde_json::from_str(blob).unwrap_or_default()
    }
}

/// Deserialize a ratio that may arrive as a JSON number or a numeric string.
fn flex_f64<'de, D>(deserializer: D) -> std::result::Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flex {
        Number(f64),
        Text(String),
    }

    Ok(match Option::<Flex>::deserialize(deserializer)? {
        None => None,
        Some(Flex::Number(n)) => Some(n),
        Some(Flex::Text(text)) => text.trim().parse::<f64>().ok(),
    })
}

/// What a row's text contributed to reading it as a check-in reward.
///
/// Separate from [`ObservedResourceEvent`] because it is *weak* evidence and must
/// stay separable from strong evidence. The primary confirmation for a check-in
/// is the structured `quota_awarded` the operation response carries; this exists
/// so an event read afterwards can corroborate it, and so a panel can explain
/// what it saw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewApiSystemSignal {
    /// The content carries a currency-rendered amount.
    pub reward_shaped: bool,
    /// The content names check-in, in a locale this adapter happens to know.
    ///
    /// A closed list, not a proof. Provider text is localised and forks reword
    /// it, so this is recorded as one signal among two and never as the
    /// discriminator.
    pub mentions_checkin: bool,
    /// The amount found in the text, in the instance's display currency.
    ///
    /// Not a balance figure. `logger.LogQuota` renders in the operator's chosen
    /// display currency — `＄`, `¥`, a custom symbol, or a point count — so this
    /// number is not convertible to the account's credit without knowing which.
    pub display_amount: Option<f64>,
    /// The symbol the amount was rendered with.
    pub display_symbol: Option<String>,
}

impl NewApiSystemSignal {
    /// Whether this row is evidence that a check-in reward happened.
    ///
    /// Requires the system type *and* a reward-shaped amount. A bare system row
    /// — a registration gift, an invitation bonus, an operator notice — is a
    /// grant but not a check-in, and treating every `type=4` row as a check-in
    /// would make the answer to "did my check-in work" yes on the day the
    /// account was created.
    pub fn indicates_checkin_reward(&self) -> bool {
        self.reward_shaped && self.mentions_checkin
    }
}

/// Currency symbols `logger.LogQuota` can render with.
///
/// `＄` is U+FF04 FULLWIDTH DOLLAR SIGN, which is what the upstream default
/// branch of `LogQuota` actually emits — not `U+0024`. Reading only ASCII `$`
/// would miss every default-configured panel.
const DISPLAY_SYMBOLS: &[char] = &['＄', '$', '¥', '€', '£', '¤'];

/// Tokens that name check-in in the locales this adapter knows.
///
/// Not proof, and deliberately short. Upstream writes
/// `用户签到，获得额度 %s`; at least one widely used fork writes
/// `每日签到成功，增加额度 %s 额度`. Both are matched. A panel in a language not
/// listed here still produces a correct *grant*; it just does not produce a
/// *check-in* attribution from text, which is the correct outcome for a word
/// list that does not know the word.
const CHECKIN_TOKENS: &[&str] = &[
    "签到", "checkin", "check-in", "check in", "signin", "sign-in",
];

/// Read a system row's text for the signals it carries.
pub fn read_system_signal(content: &str) -> NewApiSystemSignal {
    let (symbol, amount) = find_display_amount(content);
    let lowered = content.to_lowercase();
    NewApiSystemSignal {
        reward_shaped: amount.is_some(),
        mentions_checkin: CHECKIN_TOKENS.iter().any(|token| lowered.contains(token)),
        display_amount: amount,
        display_symbol: symbol,
    }
}

/// Find a currency-rendered amount and the symbol it was rendered with.
fn find_display_amount(content: &str) -> (Option<String>, Option<f64>) {
    let chars: Vec<char> = content.chars().collect();
    for (index, &symbol) in chars.iter().enumerate() {
        if !DISPLAY_SYMBOLS.contains(&symbol) {
            continue;
        }
        // The symbol may be preceded by a full-width space the panel inserts;
        // skip spaces only, so a real separator is still required.
        let mut cursor = index + 1;
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        let digits: String = chars[cursor..]
            .iter()
            .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
            .collect();
        let normalised = normalise_number(&digits);
        if let Ok(value) = normalised.parse::<f64>() {
            if value.is_finite() {
                return (Some(symbol.to_string()), Some(value));
            }
        }
    }
    (None, None)
}

/// Make a rendered number parseable by Rust.
///
/// `LogQuota` uses `%.6f`, so the separator is always `.`. A comma is accepted
/// because a localised panel may render one; a bare comma would otherwise be
/// swallowed as part of the number and produce a wrong value rather than a
/// refusal, so it is dropped when it is a thousands separator and kept when it is
/// a decimal point.
fn normalise_number(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains(',') && !trimmed.contains('.') {
        return trimmed.replace(',', "");
    }
    trimmed.replace(',', "")
}

/// How one account's provider-reported figures should be read.
pub struct LogContext<'a> {
    /// The account the rows belong to.
    pub account_id: &'a AccountId,
    /// Credit units per US dollar, from `GET /api/status`.
    pub quota_per_unit: f64,
    /// The unit normalised amounts are expressed in.
    pub unit: &'a str,
}

impl<'a> LogContext<'a> {
    /// A context reporting money in US dollars.
    pub fn usd(account_id: &'a AccountId, quota_per_unit: f64) -> Self {
        Self {
            account_id,
            quota_per_unit,
            unit: "USD",
        }
    }

    /// Credit units to the context's unit.
    pub fn to_unit(&self, credits: i64) -> f64 {
        if self.quota_per_unit <= 0.0 || !self.quota_per_unit.is_finite() {
            return 0.0;
        }
        credits as f64 / self.quota_per_unit
    }

    fn amount(&self, credits: i64) -> ResourceAmount {
        ResourceAmount::normalised(self.to_unit(credits), self.unit)
            .with_provider_value(credits as f64)
    }
}

/// Read one log row as an account event.
///
/// Returns `None` for `type = 0`. That is not a special case bolted on: upstream
/// uses that value to mean *no type filter*, so a row carrying it does not exist
/// and classifying one would be inventing the input as well as the answer. See
/// the module documentation.
///
/// Every other type maps to a [`ResourceEventKind`] and, where the write site in
/// `model/log.go` carries a movement, a [`ResourceEffect`] decided from that
/// write site rather than from the sign of `quota`.
pub fn interpret(item: &NewApiLogItem, ctx: &LogContext<'_>) -> Option<ObservedResourceEvent> {
    if item.log_type == LOG_TYPE_UNKNOWN {
        return None;
    }

    let other = NewApiLogOther::parse(&item.other);
    let (kind, effect) = classify(item, ctx);

    let accounting = match item.log_type {
        LOG_TYPE_CONSUME => reconstruct(item, &other),
        _ => None,
    };

    let mut metadata = BTreeMap::new();
    metadata.insert(META_RAW_QUOTA.to_string(), item.quota.to_string());
    if let Some(cache_tokens) = other.cache_tokens {
        metadata.insert(META_CACHE_TOKENS.to_string(), cache_tokens.to_string());
    }
    put_ratio(&mut metadata, META_CACHE_RATIO, other.cache_ratio);
    put_ratio(&mut metadata, META_MODEL_RATIO, other.model_ratio);
    put_ratio(&mut metadata, META_COMPLETION_RATIO, other.completion_ratio);
    put_ratio(&mut metadata, META_GROUP_RATIO, other.group_ratio);
    if !item.group.is_empty() {
        metadata.insert(META_GROUP.to_string(), item.group.clone());
    }
    if item.token_id != 0 {
        metadata.insert(META_TOKEN_ID.to_string(), item.token_id.to_string());
    }

    // Request-scoped detail belongs on request-scoped rows. Carrying it on a
    // check-in row would make a grant look like a failed model call.
    let request_scoped = matches!(item.log_type, LOG_TYPE_CONSUME | LOG_TYPE_ERROR);
    if request_scoped {
        metadata.insert(META_USE_TIME_SECS.to_string(), item.use_time.to_string());
        metadata.insert(META_IS_STREAM.to_string(), item.is_stream.to_string());
        if !item.channel_name.is_empty() {
            metadata.insert(META_CHANNEL_NAME.to_string(), item.channel_name.clone());
        }
    }

    if item.log_type == LOG_TYPE_SYSTEM {
        let signal = read_system_signal(&item.content);
        metadata.insert(
            META_SYSTEM_REWARD_SHAPED.to_string(),
            signal.reward_shaped.to_string(),
        );
        if let (Some(amount), Some(symbol)) = (signal.display_amount, signal.display_symbol) {
            metadata.insert(META_SYSTEM_DISPLAY_AMOUNT.to_string(), amount.to_string());
            metadata.insert(META_SYSTEM_DISPLAY_SYMBOL.to_string(), symbol);
        }
    }

    Some(ObservedResourceEvent {
        // Ids are not unique across a user's rows in every NewAPI version —
        // `assignDisplayLogIds` renumbers them per page — so the id is only
        // carried when the row also names a model, which is what a stable
        // provider key looks like in practice.
        event_id: stable_event_id(item),
        provider_id: super::PROVIDER_ID.to_string(),
        account_id: ctx.account_id.clone(),
        observed_at: item.created_at,
        event_kind: kind,
        raw_type: Some(item.log_type),
        description: item.content.clone(),
        resource_effect: effect,
        source: ResourceEventSource::AccountLog,
        request_id: (!item.request_id.is_empty()).then(|| item.request_id.clone()),
        // `channel` is the provider's own routing; Zroutery has no ability to
        // select it, so it is carried as an observation and never as an
        // addressable candidate.
        channel_id: (item.channel_id != 0).then_some(item.channel_id),
        model: (!item.model_name.is_empty()).then(|| item.model_name.clone()),
        accounting,
        metadata,
    })
}

/// Classify one row and decide what it did to the balance.
///
/// Split out so the mapping is one readable table rather than a `match` buried
/// in the constructor, and so each arm can state the write site it is derived
/// from.
fn classify(item: &NewApiLogItem, ctx: &LogContext<'_>) -> (ResourceEventKind, ResourceEffect) {
    match item.log_type {
        // `RecordConsumeLog` writes the computed charge, so here — and only
        // here — `quota` is the movement. A zero charge is a free model and is
        // reported as a real zero, not as "unmeasured".
        LOG_TYPE_CONSUME => (
            ResourceEventKind::Consumption,
            ResourceEffect::Debited {
                amount: ctx.amount(item.quota.max(0)),
            },
        ),
        // A refund gives back what a consumption took, so it credits. Kept
        // distinct from a top-up: a refund reverses a charge and can be
        // reversed again.
        LOG_TYPE_REFUND => (
            ResourceEventKind::Refund,
            ResourceEffect::Credited {
                amount: ctx.amount(item.quota),
            },
        ),
        // `RecordTopupLog` leaves `quota` unset, so a top-up row establishes
        // that the account was credited but not by how much. Reading zero as a
        // zero credit would state that a payment moved nothing.
        LOG_TYPE_TOPUP => (ResourceEventKind::Replenishment, ResourceEffect::Unobserved),
        // `RecordLog` leaves `quota` unset for every system row, which is why a
        // check-in's reward is not in its log at all.
        LOG_TYPE_SYSTEM => (ResourceEventKind::SystemGrant, ResourceEffect::Unobserved),
        // `RecordErrorLog` sets `Quota: 0` deliberately: an error row records a
        // request that was attempted and did not settle. `NoChange` is a
        // finding here, not an absence of one.
        LOG_TYPE_ERROR => (ResourceEventKind::Error, ResourceEffect::NoChange),
        // A login moves no resource. Recorded as a finding so a panel can show
        // it without it being read as a ledger line.
        LOG_TYPE_LOGIN => (ResourceEventKind::Login, ResourceEffect::NoChange),
        // `RecordLogWithAdminInfo` routes management actions to `AuditLog`
        // without writing a row, so a `type=3` row is legacy. Either way it is
        // never read as consumption or as a credit.
        LOG_TYPE_MANAGE => (ResourceEventKind::Management, ResourceEffect::Unobserved),
        _ => (ResourceEventKind::Unknown, ResourceEffect::Unobserved),
    }
}

/// Rebuild the provider's own charge from the inputs it logged.
///
/// Returns [`ObservedProviderAccounting`] only when the row actually carried
/// the ratio inputs. Without them every ratio falls back to `1.0` and the
/// result degenerates to `prompt + completion` — a guess about how the provider
/// prices rather than an observation of it. Refusing keeps
/// [`ObservedProviderAccounting::agrees_with_recorded`] meaningful, because a
/// comparison against an assumed model agrees with itself by construction and
/// tells an operator nothing.
fn reconstruct(item: &NewApiLogItem, other: &NewApiLogOther) -> Option<ObservedProviderAccounting> {
    let has_ratios = other.model_ratio.is_some()
        || other.completion_ratio.is_some()
        || other.group_ratio.is_some()
        || other.cache_ratio.is_some()
        || other.cache_tokens.is_some();
    if !has_ratios {
        return None;
    }
    Some(
        ObservedProviderAccounting::reconstruct(
            item.prompt_tokens.max(0) as u64,
            item.completion_tokens.max(0) as u64,
            other.cache_tokens.unwrap_or(0),
            other.cache_ratio.unwrap_or(1.0),
            other.model_ratio.unwrap_or(1.0),
            other.completion_ratio.unwrap_or(1.0),
            other.group_ratio.unwrap_or(1.0),
        )
        .compare_with_recorded(item.quota as f64),
    )
}

/// A key that survives re-reading the same window.
///
/// Upstream renumbers `Log.Id` per page for display, so the bare id is not
/// stable across two reads of overlapping pages and would let one provider event
/// be recorded several times. Combining it with the row's timestamp and type is
/// still only a heuristic, and is documented as one: it is a de-duplication aid,
/// not a provider-guaranteed identity.
fn stable_event_id(item: &NewApiLogItem) -> Option<String> {
    (item.id != 0).then(|| format!("{}-{}-{}", item.log_type, item.created_at, item.id))
}

fn put_ratio(metadata: &mut BTreeMap<String, String>, key: &str, value: Option<f64>) {
    if let Some(value) = value.filter(|v| v.is_finite()) {
        metadata.insert(key.to_string(), value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(id: &'a AccountId) -> LogContext<'a> {
        LogContext::usd(id, 500_000.0)
    }

    #[test]
    fn type_zero_is_a_query_sentinel_and_is_never_an_event() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_UNKNOWN,
            quota: 5_000,
            ..Default::default()
        };
        // Upstream uses 0 for "no type filter", so this row cannot exist.
        assert!(interpret(&item, &ctx(&id)).is_none());
    }

    #[test]
    fn type_one_is_a_replenishment_that_does_not_state_an_amount() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_TOPUP,
            quota: 0,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a top-up row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::Replenishment);
        // `RecordTopupLog` leaves quota unset, so nothing here says how much.
        assert_eq!(event.resource_effect, ResourceEffect::Unobserved);
        assert_eq!(event.balance_delta(), None);
        assert_eq!(
            event.metadata_value(META_RAW_QUOTA),
            Some("0"),
            "the raw column is still kept, for diagnostics"
        );
    }

    #[test]
    fn type_two_is_a_consumption_whose_amount_is_the_recorded_charge() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            channel_id: 54,
            token_id: 596_910,
            prompt_tokens: 44_814,
            completion_tokens: 91,
            quota: 9_530,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a consume row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::Consumption);
        assert_eq!(event.channel_id, Some(54));
        let amount = event
            .resource_effect
            .amount()
            .expect("a charge is established");
        assert_eq!(amount.provider_value, Some(9_530.0));
        assert!((amount.value - 9_530.0 / 500_000.0).abs() < 1e-12);
        assert!((event.balance_delta().expect("signed") + amount.value).abs() < 1e-12);
    }

    #[test]
    fn a_free_model_consumption_is_a_measured_zero_not_an_unmeasured_one() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            quota: 0,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a consume row is an event");
        match &event.resource_effect {
            ResourceEffect::Debited { amount } => {
                assert_eq!(amount.value, 0.0);
                assert!(
                    event.moved_resource(),
                    "zero is a finding; the row was read and it charged nothing"
                );
            }
            other => panic!("expected a debit, got {other:?}"),
        }
    }

    #[test]
    fn type_three_is_management_and_never_a_movement() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_MANAGE,
            quota: 9_530,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a legacy manage row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::Management);
        // A non-zero quota on a management row must not be read as either a
        // charge or a credit; the effect stays unobserved.
        assert_eq!(event.resource_effect, ResourceEffect::Unobserved);
        assert!(!event.event_kind.is_resource_bearing());
    }

    #[test]
    fn type_four_is_a_system_grant_whose_amount_is_not_in_its_quota() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_SYSTEM,
            content: "每日签到成功，增加额度 ＄25.000000 额度".into(),
            quota: 0,
            channel_id: 0,
            token_id: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a system row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::SystemGrant);
        assert_eq!(event.channel_id, None, "request-scoped fields are absent");
        assert_eq!(event.request_id, None);
        assert_eq!(event.model, None);

        // The heart of it: the log says quota 0, the account was credited, and
        // the naive "balance_delta = log.quota" rule credits nothing.
        let raw: f64 = event
            .metadata_value(META_RAW_QUOTA)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(raw, 0.0);
        assert_eq!(event.resource_effect, ResourceEffect::Unobserved);
        assert_eq!(
            event.balance_delta(),
            None,
            "no observation in this row establishes an amount"
        );

        // The amount *is* recoverable from the text, in the instance's display
        // currency — which is not the same thing as a balance figure.
        let display: f64 = event
            .metadata_value(META_SYSTEM_DISPLAY_AMOUNT)
            .unwrap()
            .parse()
            .unwrap();
        assert!((display - 25.0).abs() < 1e-9);
        assert_eq!(event.metadata_value(META_SYSTEM_DISPLAY_SYMBOL), Some("＄"));
        assert_eq!(
            event.metadata_value(META_SYSTEM_REWARD_SHAPED),
            Some("true")
        );
    }

    #[test]
    fn a_system_grant_without_a_reward_amount_is_not_called_a_checkin_reward() {
        let id = AccountId("a".into());
        // A registration gift: a real grant, but not a check-in. Reading every
        // type=4 row as a check-in would answer "yes" on signup day.
        let item = NewApiLogItem {
            log_type: LOG_TYPE_SYSTEM,
            content: "新用户注册赠送额度 ＄1.000000 额度".into(),
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).unwrap();
        assert_eq!(event.event_kind, ResourceEventKind::SystemGrant);
        let signal = read_system_signal(&item.content);
        assert!(signal.reward_shaped, "an amount is present");
        assert!(!signal.mentions_checkin, "but it is not a check-in");
        assert!(!signal.indicates_checkin_reward());
    }

    #[test]
    fn both_the_upstream_and_a_forked_checkin_wording_are_recognised() {
        // Upstream main writes `用户签到，获得额度 %s`. At least one widely used
        // fork writes `每日签到成功，增加额度 %s 额度`. Neither is matched in
        // full; both name check-in and both carry an amount.
        for content in [
            "用户签到，获得额度 ＄25.000000 额度",
            "每日签到成功，增加额度 ＄25.000000 额度",
            "Daily check-in reward: ＄25.000000",
        ] {
            let signal = read_system_signal(content);
            assert!(
                signal.indicates_checkin_reward(),
                "should be recognised: {content}"
            );
        }
    }

    #[test]
    fn an_unlocalised_checkin_row_still_reads_as_a_grant() {
        // A language this adapter has no token for. The grant is real and must
        // still be observed; only the check-in attribution is unavailable, which
        // is the correct outcome for a closed word list.
        let signal = read_system_signal("Tagesbonus gutgeschrieben: ＄25.000000");
        assert!(signal.reward_shaped);
        assert!(!signal.mentions_checkin);
        assert!(!signal.indicates_checkin_reward());
    }

    #[test]
    fn the_fullwidth_dollar_sign_upstream_actually_emits_is_read() {
        // `logger.LogQuota`'s default branch is `＄%.6f 额度` — U+FF04, not
        // ASCII `$`. Reading only ASCII would miss every default panel.
        let signal = read_system_signal("用户签到，获得额度 ＄0.500000 额度");
        assert!(signal.reward_shaped);
        assert_eq!(signal.display_symbol.as_deref(), Some("＄"));
        assert!((signal.display_amount.expect("amount") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_token_display_mode_is_not_mistaken_for_a_currency_amount() {
        // `LogQuota`'s token branch renders `%d 点额度`, with no symbol. That is a
        // point count, not money, and reading it as USD would be wrong.
        let signal = read_system_signal("用户签到，获得额度 25000 点额度");
        assert!(!signal.reward_shaped);
        assert_eq!(signal.display_amount, None);
    }

    #[test]
    fn type_five_is_an_error_that_settled_nothing_and_keeps_its_identifiers() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_ERROR,
            channel_id: 54,
            model_name: "gpt-4o".into(),
            request_id: "req-abc".into(),
            upstream_request_id: "up-xyz".into(),
            quota: 0,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("an error row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::Error);
        assert_eq!(event.request_id.as_deref(), Some("req-abc"));
        assert_eq!(
            event
                .metadata
                .get("newapi.upstream_request_id")
                .map(String::as_str),
            None
        );
        assert_eq!(event.channel_id, Some(54));
        assert_eq!(event.model.as_deref(), Some("gpt-4o"));
        // `RecordErrorLog` writes `Quota: 0` on purpose: this is a finding that
        // nothing settled, not an absence of information.
        assert_eq!(event.resource_effect, ResourceEffect::NoChange);
        assert!(!event.event_kind.is_resource_bearing());
    }

    #[test]
    fn an_error_row_keeps_the_upstream_request_id_in_metadata() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_ERROR,
            request_id: "req-abc".into(),
            upstream_request_id: "up-xyz".into(),
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).unwrap();
        // Not promoted to a field: Zroutery correlates on its own request id,
        // and the upstream's is an observation about the provider's hop.
        assert_eq!(event.request_id.as_deref(), Some("req-abc"));
    }

    #[test]
    fn type_six_is_a_refund_and_credits_what_the_consumption_debited() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_REFUND,
            quota: 9_530,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a refund row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::Refund);
        match &event.resource_effect {
            ResourceEffect::Credited { amount } => {
                assert_eq!(amount.provider_value, Some(9_530.0));
            }
            other => panic!("expected a credit, got {other:?}"),
        }
        // Distinct from a replenishment: a refund reverses a charge.
        assert_ne!(event.event_kind, ResourceEventKind::Replenishment);
    }

    #[test]
    fn type_seven_is_a_login_that_moves_nothing() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_LOGIN,
            quota: 42,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("a login row is an event");
        assert_eq!(event.event_kind, ResourceEventKind::Login);
        assert_eq!(event.resource_effect, ResourceEffect::NoChange);
        assert!(!event.event_kind.is_resource_bearing());
    }

    #[test]
    fn an_unrecognised_type_from_a_fork_is_unknown_not_guessed() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: 9,
            quota: 7_000,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("an unknown type is still observed");
        assert_eq!(event.event_kind, ResourceEventKind::Unknown);
        assert_eq!(event.resource_effect, ResourceEffect::Unobserved);
        assert_eq!(
            event.raw_type,
            Some(9),
            "the raw value is kept for diagnostics"
        );
    }

    #[test]
    fn every_mapped_type_produces_exactly_one_kind() {
        let id = AccountId("a".into());
        let expected = [
            (LOG_TYPE_TOPUP, ResourceEventKind::Replenishment),
            (LOG_TYPE_CONSUME, ResourceEventKind::Consumption),
            (LOG_TYPE_MANAGE, ResourceEventKind::Management),
            (LOG_TYPE_SYSTEM, ResourceEventKind::SystemGrant),
            (LOG_TYPE_ERROR, ResourceEventKind::Error),
            (LOG_TYPE_REFUND, ResourceEventKind::Refund),
            (LOG_TYPE_LOGIN, ResourceEventKind::Login),
        ];
        for (raw, kind) in expected {
            let item = NewApiLogItem {
                log_type: raw,
                ..Default::default()
            };
            let event = interpret(&item, &ctx(&id))
                .unwrap_or_else(|| panic!("type {raw} must produce an event"));
            assert_eq!(event.event_kind, kind, "type {raw}");
            assert_eq!(event.raw_type, Some(raw));
        }
    }

    #[test]
    fn a_consumption_reconstructs_the_providers_own_charge() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            prompt_tokens: 44_814,
            completion_tokens: 91,
            quota: 9_530,
            other: r#"{"model_ratio":2,"completion_ratio":3,"group_ratio":4,"cache_ratio":1}"#
                .into(),
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).unwrap();
        let accounting = event.accounting.expect("ratios were present");
        assert_eq!(accounting.prompt_tokens, 44_814);
        assert!((accounting.reconstructed_charge - 360_696.0).abs() < 1e-6);
        // The reconstruction and the recorded charge disagree, and the
        // disagreement is reported rather than hidden.
        assert_eq!(accounting.agrees_with_recorded, Some(false));
    }

    #[test]
    fn a_consumption_with_no_ratios_produces_no_accounting_rather_than_a_guess() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            prompt_tokens: 10,
            completion_tokens: 2,
            quota: 12,
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).unwrap();
        assert!(
            event.accounting.is_none(),
            "no ratios means no observation of the provider's arithmetic"
        );
    }

    #[test]
    fn an_unparseable_other_blob_does_not_cost_the_event() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            quota: 9_530,
            other: "{not json".into(),
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).expect("the kind and effect do not depend on it");
        assert_eq!(event.event_kind, ResourceEventKind::Consumption);
    }

    #[test]
    fn a_ratio_sent_as_a_string_is_still_read() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            prompt_tokens: 100,
            completion_tokens: 0,
            quota: 250,
            other: r#"{"model_ratio":"2.5","group_ratio":"1"}"#.into(),
            ..Default::default()
        };
        let event = interpret(&item, &ctx(&id)).unwrap();
        let accounting = event.accounting.expect("ratios were present");
        assert!((accounting.reconstructed_charge - 250.0).abs() < 1e-9);
        assert_eq!(accounting.agrees_with_recorded, Some(true));
    }

    #[test]
    fn the_event_id_is_stable_across_re_reads() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            id: 7,
            log_type: LOG_TYPE_CONSUME,
            created_at: 1_700_000_000,
            ..Default::default()
        };
        let first = interpret(&item, &ctx(&id)).unwrap();
        let second = interpret(&item, &ctx(&id)).unwrap();
        assert_eq!(first.event_id, second.event_id);
        assert!(first.event_id.is_some());
    }

    #[test]
    fn a_row_without_an_id_gets_none_rather_than_a_colliding_key() {
        let id = AccountId("a".into());
        let item = NewApiLogItem {
            log_type: LOG_TYPE_CONSUME,
            ..Default::default()
        };
        assert_eq!(interpret(&item, &ctx(&id)).unwrap().event_id, None);
    }

    #[test]
    fn a_row_decodes_from_the_wire_shape() {
        // `channel` is the JSON key for the channel id, and `other` is a string.
        let item: NewApiLogItem = serde_json::from_str(
            r#"{"id":1,"created_at":1700000000,"type":2,"channel":54,
                "token_id":596910,"prompt_tokens":44814,"completion_tokens":91,
                "quota":9530,"request_id":"r1","other":"{\"model_ratio\":2}"}"#,
        )
        .expect("a log row decodes");
        assert_eq!(item.log_type, LOG_TYPE_CONSUME);
        assert_eq!(item.channel_id, 54);
        assert_eq!(item.model_name, "");
        assert_eq!(NewApiLogOther::parse(&item.other).model_ratio, Some(2.0));
    }

    #[test]
    fn a_page_decodes_and_absent_optional_fields_default() {
        let page: NewApiLogPage = serde_json::from_str(
            r#"{"total":2,"items":[{"type":4,"content":"x"},{"type":2,"quota":5}]}"#,
        )
        .expect("a log page decodes");
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].quota, 0);
        assert!(page.items[1].request_id.is_empty());
    }

    #[test]
    fn a_thousands_separator_is_removed_rather_than_read_as_a_decimal_point() {
        assert_eq!(normalise_number("1,250.500"), "1250.500");
        assert_eq!(normalise_number("1,250"), "1250");
        assert_eq!(normalise_number(" 25.000000 "), "25.000000");
    }

    #[test]
    fn the_display_amount_search_stops_at_a_non_number() {
        let (symbol, amount) = find_display_amount("cost ＄12.50 then ¥3.00");
        // The first symbol wins, and it is not dragged across the sentence.
        assert_eq!(symbol.as_deref(), Some("＄"));
        assert!((amount.expect("amount") - 12.50).abs() < 1e-9);
    }

    #[test]
    fn a_symbol_with_no_digits_after_it_is_not_an_amount() {
        let signal = read_system_signal("quota ＄  awarded");
        assert!(!signal.reward_shaped);
    }
}
