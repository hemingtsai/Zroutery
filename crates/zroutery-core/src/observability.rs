//! Read-only correlation projection over accepted Core types.
//!
//! This module answers one question: *given a terminal [`Outcome`], can an
//! operator or a downstream consumer find the decision that planned it, the
//! failure that ended it, and the usage it cost, in one place, without any of
//! them being silently missing or silently invented?* It adds no routing, no
//! activation, and no learning.
//!
//! # The unit
//!
//! **One record per request, and the unit of truth is
//! [`Outcome`](crate::outcome::Outcome).** [`RequestProjection`] is that unit,
//! addressable by exactly one `outcome_id`.
//!
//! Attempts are **not** a second top-level unit. They are nested inside the
//! record as [`AttemptProjection`] values, each keyed by its own `attempt_id`
//! and carrying an explicit `position`. The distinction matters: under a
//! per-attempt unit, "which decision does this attempt belong to" would be
//! unanswerable, because `attempt_id` alone does not lead back to a decision.
//!
//! # Correlation keys
//!
//! [`CorrelationKeys`] names the four UUID-derived identifiers Core mints:
//!
//! | kind     | prefix | minted at                       |
//! |----------|--------|---------------------------------|
//! | outcome  | `out_` | `outcome.rs` `Outcome::builder` |
//! | request  | `req_` | `stats.rs` `RecordBuilder::new`  |
//! | decision | `dec-` | `router.rs` `RouteDecision`      |
//! | attempt  | `att_` | `server/pipeline.rs`             |
//!
//! ## What is refused as a key
//!
//! **No wall-clock value is ever a correlation key, an ordering key, or a
//! tiebreaker.** Specifically refused: `Outcome::timestamp`,
//! `Attempt::started_at`, `Attempt::completed_at`, `RouteDecision::timestamp`
//! and every `observed_at`. All are second-granular or coarser, so two events
//! inside one second are indistinguishable by them; any grouping or ordering
//! that depends on them will look correct while being wrong.
//!
//! This is enforced structurally rather than by convention:
//!
//! * the clock is carried in [`SecondGranularTimestamp`], a newtype that
//!   deliberately implements **no** `Ord`/`PartialOrd`, so it cannot be sorted
//!   by, and trying fails to compile rather than failing silently;
//! * the only ordering this module offers is
//!   [`RequestProjection::content_order`], which compares identifiers only;
//! * the batch index is a `BTreeMap`, so every internal iteration is ordered by
//!   key content rather than by hash;
//! * refusals are ordered by their own canonical serialization, so even two
//!   refusals that share a locator cannot swap places between runs.
//!
//! Attempt order is the accepted `Vec` order, preserved verbatim and made
//! explicit through `AttemptProjection::position`. That order is the routing
//! sequence the lifecycle recorded, not a reading of a clock; sorting attempts
//! by `attempt_id` would destroy the failover narrative, which is the most
//! useful thing an operator reads.
//!
//! # A served identity is never substituted
//!
//! `Outcome::decision_id` is an `Option`. An absent decision id is a **typed
//! refusal** ([`RefusalReason::DecisionIdAbsent`]), never a projected record
//! with a filled-in id. The refusal still carries the record's locator, so the
//! record is reported rather than dropped, and it states the consequence:
//! decision correlation is unavailable for it. A planned identity is never
//! substituted for a missing served one, and no identifier is ever synthesised.
//!
//! # Purity
//!
//! [`project_request`] and [`project_batch`] are pure functions of their
//! arguments. There is no interior state, no accumulator, no clock read, no
//! environment read, no I/O, and no thread. Anything that accumulates belongs to
//! the caller. That is what makes the no-side-effect requirement provable
//! instead of asserted: the projection test file greps this module's own source
//! for each of those facilities.
//!
//! # What is deliberately not projected
//!
//! No free-form operator text is carried. In particular `FailureFacts::message`
//! and `Attempt::failure_message` are **never** projected, because an upstream
//! error message is the one field in this schema capable of carrying a provider
//! response body, an echoed request, or a credential. Only the closed
//! [`FailureClass`] vocabulary, its [`FailureImpact`] bits, and the HTTP status
//! cross the boundary. [`REQUEST_PROJECTION_FIELDS`] and its siblings declare
//! the complete projected field set and a test asserts the serialized record has
//! exactly those keys, so the redaction property is structural: a future
//! free-text field fails a test instead of leaking.

use std::cmp::Ordering;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::failure::{FailureClass, FailureImpact};
use crate::ir::Usage;
use crate::outcome::{
    failure_class_wire_name, Attempt, CandidateIdentity, FailureFacts, FinalStatus, Outcome,
};
use crate::policy::{DecisionReason, RouteDecision};

/// Wire version of the projected request record.
pub const PROJECTION_SCHEMA_VERSION: &str = "observability.request.v1";

/// Length of the UUID body Core mints behind every identifier prefix.
pub const IDENTITY_BODY_LEN: usize = 32;

/// Every field [`RequestProjection`] serializes. A test compares the serialized
/// key set against this list, so adding a field without declaring it here fails
/// the audit rather than silently widening what leaves Core.
pub const REQUEST_PROJECTION_FIELDS: &[&str] = &[
    "schema_version",
    "keys",
    "attempts",
    "decision",
    "success",
    "final_status",
    "failure",
    "planned_identity",
    "last_attempted_identity",
    "served_identity",
    "usage",
    "total_latency_ms",
    "ttft_ms",
    "estimated_cost",
    "actual_cost",
    "streaming",
    "dialect",
    "fallback_count",
    "recorded",
];

/// Every field [`AttemptProjection`] serializes.
pub const ATTEMPT_PROJECTION_FIELDS: &[&str] = &[
    "position",
    "attempt_id",
    "candidate",
    "success",
    "failure",
    "latency_ms",
    "ttft_ms",
    "http_status",
    "rectified",
];

/// Every field [`FailureProjection`] serializes. Note what is missing: there is
/// no message field, by design.
pub const FAILURE_PROJECTION_FIELDS: &[&str] = &["class", "http_status", "impact"];

/// Every field [`UsageProjection`] serializes.
pub const USAGE_PROJECTION_FIELDS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "total_tokens",
];

// ---------------------------------------------------------------------------
// Identities
// ---------------------------------------------------------------------------

/// Which UUID-derived identifier a value is, and therefore which prefix it must
/// carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    /// `out_` + 32 lowercase hex digits.
    Outcome,
    /// `req_` + 32 lowercase hex digits.
    Request,
    /// `dec-` + 32 lowercase hex digits.
    Decision,
    /// `att_` + 32 lowercase hex digits.
    Attempt,
}

impl IdentityKind {
    /// The prefix Core mints this identifier with.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Outcome => "out_",
            Self::Request => "req_",
            Self::Decision => "dec-",
            Self::Attempt => "att_",
        }
    }

    /// The field this identifier is carried in.
    pub const fn field(self) -> &'static str {
        match self {
            Self::Outcome => "outcome_id",
            Self::Request => "request_id",
            Self::Decision => "decision_id",
            Self::Attempt => "attempt_id",
        }
    }

    /// The minting site that defines this identifier's shape.
    pub const fn minted_at(self) -> &'static str {
        match self {
            Self::Outcome => "outcome.rs Outcome::builder",
            Self::Request => "stats.rs RecordBuilder::new",
            Self::Decision => "router.rs RouteDecision",
            Self::Attempt => "server/pipeline.rs attempt",
        }
    }
}

/// Why an identifier was refused. Carries the raw value verbatim: a malformed
/// identifier is reported, never repaired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityError {
    /// Which kind of identifier was expected.
    pub kind: IdentityKind,
    /// The value as it was found.
    pub raw: String,
    /// What the value should have looked like.
    pub detail: String,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({:?}) is not a well-formed identifier: {}",
            self.kind.field(),
            self.kind.prefix(),
            self.detail
        )
    }
}

impl std::error::Error for IdentityError {}

impl IdentityError {
    /// The refusal this parse failure becomes. An empty value is *absent*; any
    /// other wrong value is *unparsable*.
    fn into_reason(self) -> RefusalReason {
        if self.raw.trim().is_empty() {
            RefusalReason::IdentityAbsent { kind: self.kind }
        } else {
            RefusalReason::IdentityUnparsable {
                kind: self.kind,
                raw: self.raw,
                detail: self.detail,
            }
        }
    }
}

/// Check an identifier against the shape Core documents for it.
///
/// The rule is deliberately narrow: the documented prefix followed by exactly
/// [`IDENTITY_BODY_LEN`] lowercase hex digits. Core mints all four identifiers
/// from `uuid::Uuid::new_v4().simple()`, so that shape is verifiable; anything
/// else cannot be confirmed to *be* the identifier it claims to be, and
/// correlating on an unverifiable identifier is unsound. This is the one place
/// the rule lives, so a later change has to be made here, with evidence, rather
/// than by accident.
///
/// # Errors
///
/// Returns [`IdentityError`] when `raw` is empty, carries the wrong prefix, or
/// does not end in exactly 32 lowercase hex digits. On success the input is
/// returned unchanged.
pub fn parse_identity(kind: IdentityKind, raw: &str) -> Result<String, IdentityError> {
    let expected = || {
        format!(
            "expected {:?} followed by {IDENTITY_BODY_LEN} lowercase hex digits, as minted by {}",
            kind.prefix(),
            kind.minted_at()
        )
    };
    if raw.trim().is_empty() {
        return Err(IdentityError {
            kind,
            raw: raw.to_string(),
            detail: "the value is empty".to_string(),
        });
    }
    let Some(body) = raw.strip_prefix(kind.prefix()) else {
        return Err(IdentityError {
            kind,
            raw: raw.to_string(),
            detail: expected(),
        });
    };
    if body.len() != IDENTITY_BODY_LEN || !body.bytes().all(is_lowercase_hex) {
        return Err(IdentityError {
            kind,
            raw: raw.to_string(),
            detail: expected(),
        });
    }
    Ok(raw.to_string())
}

const fn is_lowercase_hex(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

/// The four identifiers one record is correlated by.
///
/// `Ord` is derived over `(outcome_id, request_id, decision_id)` and is the only
/// ordering this module performs on records. No timestamp participates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CorrelationKeys {
    /// `out_` + 32 lowercase hex digits. The record's own address.
    pub outcome_id: String,
    /// `req_` + 32 lowercase hex digits. Joins to the request log.
    pub request_id: String,
    /// `dec-` + 32 lowercase hex digits. Joins to the routing decision.
    ///
    /// Present on every projected record because an absent decision id refuses
    /// rather than projects; [`RefusalReason::DecisionIdAbsent`] is where the
    /// absent case is reported.
    pub decision_id: String,
}

// ---------------------------------------------------------------------------
// The clock, carried inertly
// ---------------------------------------------------------------------------

/// A wall-clock reading that may be displayed, and may be nothing else.
///
/// `Outcome::timestamp` is second-granular, so two events inside the same second
/// are indistinguishable by it. This type therefore implements **no** `Ord` and
/// **no** `PartialOrd`: there is no way to sort by it, and a compile error is the
/// intended response to trying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecondGranularTimestamp(i64);

impl SecondGranularTimestamp {
    /// Wrap a Unix timestamp in seconds, as Core records it.
    pub const fn from_unix_secs(secs: i64) -> Self {
        Self(secs)
    }

    /// The raw value, for display, or for a caller that owns the ordering.
    pub const fn as_unix_secs(self) -> i64 {
        self.0
    }
}

/// The one wall-clock reading a record carries, kept in one place so that its
/// granularity cannot be mistaken for an ordering key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecordedClock {
    /// When the request was first received. Second-granular; never a key.
    pub received: SecondGranularTimestamp,
}

// ---------------------------------------------------------------------------
// Leaves
// ---------------------------------------------------------------------------

/// A model/provider pair: a routing position, not a route plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RouteLabel {
    /// Model id as the router knows it.
    pub model: String,
    /// Provider id as the router knows it.
    pub provider: String,
}

impl RouteLabel {
    fn new(model: &str, provider: &str) -> Self {
        Self {
            model: model.to_string(),
            provider: provider.to_string(),
        }
    }

    fn of(identity: CandidateIdentity) -> Self {
        Self::new(identity.model(), identity.provider())
    }
}

/// Token usage, flattened for the wire. Built from [`Usage`], never recomputed
/// from a response body.
///
/// The token counts are `u32`, so a negative or non-finite usage count is
/// unrepresentable and cannot reach this projection: serde refuses such a value
/// at the deserialization boundary. The numeric measure that *can* be negative
/// or non-finite is money and latency, and those are checked and refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct UsageProjection {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
    /// `Usage::total()`: input plus output, with saturating arithmetic.
    pub total_tokens: u32,
}

impl From<&Usage> for UsageProjection {
    fn from(usage: &Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total(),
        }
    }
}

/// The accepted impact table, flattened. Consumed from [`FailureClass::impact`];
/// this module adds no policy of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FailureImpactProjection {
    pub affects_observation: bool,
    pub affects_circuit: bool,
    pub retryable: bool,
    pub fallbackable: bool,
    pub provider_fault: bool,
    pub records_stats: bool,
}

impl From<&FailureImpact> for FailureImpactProjection {
    fn from(impact: &FailureImpact) -> Self {
        Self {
            affects_observation: impact.affects_observation,
            affects_circuit: impact.affects_circuit,
            retryable: impact.retryable,
            fallbackable: impact.fallbackable,
            provider_fault: impact.provider_fault,
            records_stats: impact.records_stats(),
        }
    }
}

/// A classified failure, without its message.
///
/// The message is omitted on purpose. `FailureFacts::message` is free-form text
/// produced upstream, and it is the field in this schema most capable of carrying
/// a provider response body, an echoed request, or a credential. Only the closed
/// [`FailureClass`] vocabulary, its impact bits, and the status code are
/// projected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FailureProjection {
    /// The stable wire name from [`failure_class_wire_name`].
    pub class: String,
    /// The HTTP status the provider returned, when it returned one.
    pub http_status: Option<u16>,
    /// The accepted impact of this class.
    pub impact: FailureImpactProjection,
}

impl FailureProjection {
    /// Project facts Core already classified. Never reclassifies, and never
    /// inspects the message.
    pub fn from_facts(facts: &FailureFacts) -> Self {
        let impact = facts.class.impact();
        Self {
            class: failure_class_wire_name(facts.class).to_string(),
            http_status: facts.http_status,
            impact: FailureImpactProjection::from(&impact),
        }
    }

    /// The class this projection describes, read back from its wire name.
    pub fn failure_class(&self) -> Option<FailureClass> {
        FailureClass::ALL
            .into_iter()
            .find(|class| failure_class_wire_name(*class) == self.class)
    }
}

/// One attempt inside a record, keyed by its own `attempt_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AttemptProjection {
    /// Zero-based position in the accepted attempt order, which is the routing
    /// order. Made explicit so the order is stated rather than inherited.
    pub position: u32,
    /// `att_` + 32 lowercase hex digits.
    pub attempt_id: String,
    /// Which provider/model was tried.
    pub candidate: RouteLabel,
    pub success: bool,
    /// The accepted classification for this attempt, when it failed.
    pub failure: Option<FailureProjection>,
    pub latency_ms: f64,
    pub ttft_ms: Option<f64>,
    pub http_status: Option<u16>,
    /// Whether this was a rectifier retry of the same candidate.
    pub rectified: bool,
}

/// Why a decision selected what it selected, as a closed token.
///
/// The full [`DecisionReason`] carries free-form `from`/`reason`/`from_tier`
/// strings in some variants; only the discriminant crosses this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReasonKind {
    Direct,
    PolicySelected,
    Fallback,
    Escalated,
    Degraded,
    NoCandidate,
}

impl From<&DecisionReason> for DecisionReasonKind {
    fn from(reason: &DecisionReason) -> Self {
        match reason {
            DecisionReason::Direct => Self::Direct,
            DecisionReason::PolicySelected => Self::PolicySelected,
            DecisionReason::Fallback { .. } => Self::Fallback,
            DecisionReason::Escalated { .. } => Self::Escalated,
            DecisionReason::Degraded { .. } => Self::Degraded,
            DecisionReason::NoCandidate => Self::NoCandidate,
        }
    }
}

/// The decision evidence joined to a record by `decision_id`.
///
/// `None` on a record means "this decision id was not in the set you supplied".
/// That is a fact about the input and is deliberately different from a missing
/// decision id, which refuses. The join is never guessed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DecisionSummary {
    /// `dec-` + 32 lowercase hex digits, equal to the record's `decision_id`.
    pub decision_id: String,
    /// What planning selected, before any attempt.
    pub selected_model: Option<String>,
    /// Why it selected that.
    pub reason: DecisionReasonKind,
    pub policy_id: String,
    pub candidate_count: u32,
    pub eligible_count: u32,
    pub fallback_chain_len: u32,
    /// The identity planning chose, when it chose one. This is the decision's
    /// plan, not the outcome's served identity; the record keeps those two
    /// apart in `planned_identity` and `served_identity`.
    pub planned: Option<RouteLabel>,
}

/// The outcome of projecting one record.
// The refused arm is carried inline rather than boxed on purpose: a refusal is
// the expected outcome for corrupt input, and refusing must not allocate, while
// the projected arm is only paid for when a record really did project.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    Projected(RequestProjection),
    Refused(ProjectionRefusal),
}

impl Projection {
    /// The record, when it projected.
    pub fn projected(&self) -> Option<&RequestProjection> {
        match self {
            Self::Projected(record) => Some(record),
            Self::Refused(_) => None,
        }
    }

    /// The refusal, when it refused.
    pub fn refused(&self) -> Option<&ProjectionRefusal> {
        match self {
            Self::Projected(_) => None,
            Self::Refused(refusal) => Some(refusal),
        }
    }
}

/// One request's outcome, projected for correlation.
///
/// Built by [`project_request`] or [`project_batch`], both of which validate
/// every field before a value is produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RequestProjection {
    /// [`PROJECTION_SCHEMA_VERSION`] as of the projection that built this.
    pub schema_version: String,
    /// The identifiers this record is correlated by.
    pub keys: CorrelationKeys,
    /// Attempts in routing order. Not the unit of the record.
    pub attempts: Vec<AttemptProjection>,
    /// The joined decision, when one was supplied for `keys.decision_id`.
    pub decision: Option<DecisionSummary>,
    pub success: bool,
    pub final_status: FinalStatus,
    /// The terminal failure classification, when the request did not succeed.
    pub failure: Option<FailureProjection>,
    /// Selected before attempts were made. Never a substitute for a missing
    /// decision id.
    pub planned_identity: Option<RouteLabel>,
    /// Identity of the last attempt made.
    pub last_attempted_identity: Option<RouteLabel>,
    /// Identity that served a successful terminal response. `None` for every
    /// non-success terminal state, and never inferred from a plan.
    pub served_identity: Option<RouteLabel>,
    /// Token usage, when usage was recorded.
    pub usage: Option<UsageProjection>,
    pub total_latency_ms: f64,
    pub ttft_ms: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub actual_cost: Option<f64>,
    pub streaming: bool,
    /// The ingress dialect label the caller recorded.
    pub dialect: String,
    pub fallback_count: u32,
    /// The wall clock, carried inertly. Never a key, never an ordering input.
    pub recorded: RecordedClock,
}

impl RequestProjection {
    /// This record's address.
    pub fn outcome_id(&self) -> &str {
        &self.keys.outcome_id
    }

    /// The request this record belongs to.
    pub fn request_id(&self) -> &str {
        &self.keys.request_id
    }

    /// The decision this record is correlated with.
    pub fn decision_id(&self) -> &str {
        &self.keys.decision_id
    }

    /// Look up one attempt by its own identifier.
    pub fn attempt(&self, attempt_id: &str) -> Option<&AttemptProjection> {
        self.attempts
            .iter()
            .find(|attempt| attempt.attempt_id == attempt_id)
    }

    /// Every attempt identifier, in routing order.
    pub fn attempt_ids(&self) -> impl Iterator<Item = &str> {
        self.attempts
            .iter()
            .map(|attempt| attempt.attempt_id.as_str())
    }

    /// The total order this module offers.
    ///
    /// Identifiers only: `outcome_id`, then `request_id`, then `decision_id`. No
    /// timestamp appears in it, which is why two requests that arrived in the
    /// same second still have a stable, content-determined order.
    pub fn content_order(&self, other: &Self) -> Ordering {
        self.keys
            .outcome_id
            .cmp(&other.keys.outcome_id)
            .then_with(|| self.keys.request_id.cmp(&other.keys.request_id))
            .then_with(|| self.keys.decision_id.cmp(&other.keys.decision_id))
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Where a refused record says it is. Every field holds the value as it was
/// found, including a value that failed to parse: a malformed identifier is
/// reported, never repaired and never discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecordLocator {
    pub outcome_id: Option<String>,
    pub request_id: Option<String>,
    pub decision_id: Option<String>,
    pub attempt_id: Option<String>,
}

impl RecordLocator {
    /// The best identifier available for this record, for use as a sort key.
    pub fn identity_key(&self) -> &str {
        self.outcome_id
            .as_deref()
            .or(self.request_id.as_deref())
            .or(self.decision_id.as_deref())
            .or(self.attempt_id.as_deref())
            .unwrap_or("")
    }
}

/// The typed reason a record was not projected, and what that costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum RefusalReason {
    /// An identifier was empty.
    IdentityAbsent {
        /// Which identifier was empty.
        kind: IdentityKind,
    },
    /// An identifier did not match the shape Core documents for it.
    IdentityUnparsable {
        /// Which identifier was malformed.
        kind: IdentityKind,
        /// The value as found.
        raw: String,
        /// What it should have looked like.
        detail: String,
    },
    /// The record names no decision.
    DecisionIdAbsent,
    /// More than one record claims one outcome id.
    DuplicateOutcomeId {
        /// How many records claimed it.
        occurrences: usize,
    },
    /// More than one attempt in this record claims one attempt id.
    DuplicateAttemptId {
        /// The id claimed twice.
        attempt_id: String,
    },
    /// More than one supplied decision claims one decision id.
    DuplicateDecisionId {
        /// The id claimed more than once.
        decision_id: String,
        /// How many decisions claimed it.
        occurrences: usize,
    },
    /// The supplied decision is not the one the record names.
    DecisionIdentityMismatch {
        /// The decision id the caller actually passed.
        supplied_decision_id: String,
    },
    /// The decision id the record names is claimed by more than one decision.
    AmbiguousDecision {
        /// The contested decision id.
        decision_id: String,
    },
    /// A measure was not finite, or was negative.
    FigureNotFiniteOrNegative {
        /// The field that failed, named as `field` or `attempts[i].field`.
        field: String,
        /// The value as found.
        value: f64,
    },
    /// The outcome reports a non-success terminal state and carries no
    /// classification for it.
    FailureClassificationAbsent {
        /// The terminal state that lacked a classification.
        final_status: FinalStatus,
    },
    /// The accepted `Outcome` schema rejects the record.
    InvalidOutcome {
        /// `Outcome::validate`'s own words.
        detail: String,
    },
}

impl RefusalReason {
    /// A stable tag, used for ordering and for matching.
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::IdentityAbsent { .. } => "identity_absent",
            Self::IdentityUnparsable { .. } => "identity_unparsable",
            Self::DecisionIdAbsent => "decision_id_absent",
            Self::DuplicateOutcomeId { .. } => "duplicate_outcome_id",
            Self::DuplicateAttemptId { .. } => "duplicate_attempt_id",
            Self::DuplicateDecisionId { .. } => "duplicate_decision_id",
            Self::DecisionIdentityMismatch { .. } => "decision_identity_mismatch",
            Self::AmbiguousDecision { .. } => "ambiguous_decision",
            Self::FigureNotFiniteOrNegative { .. } => "figure_not_finite_or_negative",
            Self::FailureClassificationAbsent { .. } => "failure_classification_absent",
            Self::InvalidOutcome { .. } => "invalid_outcome",
        }
    }

    /// What refusing this record costs its consumer. Every refusal states a
    /// consequence; none of them states only a fact.
    pub const fn consequence(&self) -> &'static str {
        match self {
            Self::IdentityAbsent { .. } => {
                "an empty identifier cannot address this record, so it is reported instead of projected"
            }
            Self::IdentityUnparsable { .. } => {
                "an identifier that does not match its documented minting shape cannot be verified, and correlating on an unverifiable identifier is unsound"
            }
            Self::DecisionIdAbsent => {
                "this record names no served decision, so decision correlation is unavailable for it; a planned identity is not a substitute and no identifier is synthesised"
            }
            Self::DuplicateOutcomeId { .. } => {
                "several records claim one outcome id, so neither can be addressed exactly and all of them are refused"
            }
            Self::DuplicateAttemptId { .. } => {
                "two attempts in this record share one attempt id, so per-attempt correlation inside it is ambiguous"
            }
            Self::DuplicateDecisionId { .. } => {
                "two supplied decisions share one decision id, so the join target for it is ambiguous"
            }
            Self::DecisionIdentityMismatch { .. } => {
                "the supplied decision is not the one this record names, so joining them would assert a correlation the identifiers contradict"
            }
            Self::AmbiguousDecision { .. } => {
                "the decision id this record names is claimed by more than one decision, so the join is ambiguous and no decision is chosen"
            }
            Self::FigureNotFiniteOrNegative { .. } => {
                "a non-finite or negative measure makes this record's usage incomparable, so it is refused rather than carried silently"
            }
            Self::FailureClassificationAbsent { .. } => {
                "the outcome reports a non-success terminal state with no accepted classification, so failure correlation is unavailable for it"
            }
            Self::InvalidOutcome { .. } => {
                "the accepted Outcome schema rejects this record, so projecting it would carry evidence Core has already refused"
            }
        }
    }
}

/// A refusal: what went wrong, and which record it went wrong for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionRefusal {
    /// The typed reason.
    pub reason: RefusalReason,
    /// The record it applies to. Present on every refusal, so a refused record
    /// is reported rather than silently dropped.
    pub record: RecordLocator,
}

impl ProjectionRefusal {
    /// The record's best available identifier.
    pub fn identity_key(&self) -> &str {
        self.record.identity_key()
    }

    /// The typed reason.
    pub fn reason(&self) -> &RefusalReason {
        &self.reason
    }

    /// What this refusal costs.
    pub fn consequence(&self) -> &'static str {
        self.reason.consequence()
    }
}

/// A batch of projections, ordered by content rather than by arrival.
///
/// The batch is the whole of this module's output. There is no other state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionBatch {
    /// Projected records, ordered by `(outcome_id, request_id, decision_id)`.
    pub records: Vec<RequestProjection>,
    /// Refusals, ordered by their own canonical serialization.
    ///
    /// Accounting invariant: `records.len() + refusals.len()` equals the number
    /// of `Outcome` values handed to [`project_batch`]. Every input is either
    /// projected or refused — never both, never neither.
    pub refusals: Vec<ProjectionRefusal>,
}

impl ProjectionBatch {
    /// How many records projected.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether nothing projected.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// How many records refused.
    pub fn refused_count(&self) -> usize {
        self.refusals.len()
    }

    /// Look up a projected record by its own address.
    pub fn record(&self, outcome_id: &str) -> Option<&RequestProjection> {
        self.records
            .iter()
            .find(|record| record.outcome_id() == outcome_id)
    }

    /// Every refusal that applies to one outcome id.
    pub fn refusals_for(&self, outcome_id: &str) -> Vec<&ProjectionRefusal> {
        self.refusals
            .iter()
            .filter(|refusal| refusal.record.outcome_id.as_deref() == Some(outcome_id))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Project one record: a pure function of its two arguments.
///
/// # Refusal order
///
/// Checks run in a fixed order, so the reason a caller receives is
/// deterministic:
///
/// 1. `outcome_id`, then `request_id`, then `decision_id`, then each
///    `attempt_id`, parsed against their documented shapes;
/// 2. duplicate attempt identifiers inside this record;
/// 3. every numeric measure, for finiteness and sign;
/// 4. failure-classification presence against the terminal state;
/// 5. the supplied decision's identity against this record's;
/// 6. [`Outcome::validate`].
///
/// The specific refusals precede schema validation on purpose, so a caller
/// receives the typed reason rather than a validation string, and so a record
/// that is corrupt in one way is reported for that way.
///
/// A record with no decision id refuses. It is not projected with a filled-in id,
/// and it is not dropped: the refusal carries its locator.
pub fn project_request(outcome: &Outcome, decision: Option<&RouteDecision>) -> Projection {
    let mut locator = RecordLocator {
        outcome_id: Some(outcome.outcome_id.clone()),
        request_id: Some(outcome.request_id.clone()),
        decision_id: outcome.decision_id.clone(),
        attempt_id: None,
    };

    // 1. Identities. Nothing is correlated on a value that has not been checked.
    if let Err(error) = parse_identity(IdentityKind::Outcome, &outcome.outcome_id) {
        return refuse(&mut locator, error);
    }
    if let Err(error) = parse_identity(IdentityKind::Request, &outcome.request_id) {
        return refuse(&mut locator, error);
    }
    let Some(decision_id) = outcome.decision_id.clone() else {
        return Projection::Refused(ProjectionRefusal {
            reason: RefusalReason::DecisionIdAbsent,
            record: take(&mut locator),
        });
    };
    if let Err(error) = parse_identity(IdentityKind::Decision, &decision_id) {
        return refuse(&mut locator, error);
    }

    let mut seen: Vec<&str> = Vec::with_capacity(outcome.attempts.len());
    for attempt in &outcome.attempts {
        if let Err(error) = parse_identity(IdentityKind::Attempt, &attempt.attempt_id) {
            locator.attempt_id = Some(attempt.attempt_id.clone());
            return refuse(&mut locator, error);
        }
        if seen.contains(&attempt.attempt_id.as_str()) {
            locator.attempt_id = Some(attempt.attempt_id.clone());
            return Projection::Refused(ProjectionRefusal {
                reason: RefusalReason::DuplicateAttemptId {
                    attempt_id: attempt.attempt_id.clone(),
                },
                record: take(&mut locator),
            });
        }
        seen.push(attempt.attempt_id.as_str());
    }

    // 2. Measures. Every number a consumer would compare across records has to
    //    be comparable, so a non-finite or negative one refuses the record.
    let mut measures: Vec<(String, f64)> = Vec::with_capacity(4 + outcome.attempts.len() * 2);
    measures.push(("total_latency_ms".to_string(), outcome.total_latency_ms));
    if let Some(ttft) = outcome.ttft_ms {
        measures.push(("ttft_ms".to_string(), ttft));
    }
    if let Some(estimated) = outcome.estimated_cost {
        measures.push(("estimated_cost".to_string(), estimated));
    }
    if let Some(actual) = outcome.actual_cost {
        measures.push(("actual_cost".to_string(), actual));
    }
    for (index, attempt) in outcome.attempts.iter().enumerate() {
        measures.push((format!("attempts[{index}].latency_ms"), attempt.latency_ms));
        if let Some(ttft) = attempt.ttft_ms {
            measures.push((format!("attempts[{index}].ttft_ms"), ttft));
        }
    }
    for (field, value) in measures {
        if !value.is_finite() || value < 0.0 {
            return Projection::Refused(ProjectionRefusal {
                reason: RefusalReason::FigureNotFiniteOrNegative { field, value },
                record: take(&mut locator),
            });
        }
    }

    // 3. Failure evidence. A non-success terminal state with no classification is
    //    refused rather than projected with a blank failure.
    if !outcome.is_terminal_success() && outcome.terminal_failure_facts().is_none() {
        return Projection::Refused(ProjectionRefusal {
            reason: RefusalReason::FailureClassificationAbsent {
                final_status: outcome.final_status,
            },
            record: take(&mut locator),
        });
    }

    // 4. The decision join, when a decision was supplied.
    let summary = match decision {
        Some(decision) => {
            if let Err(error) = parse_identity(IdentityKind::Decision, &decision.decision_id) {
                return refuse(&mut locator, error);
            }
            if decision.decision_id != decision_id {
                return Projection::Refused(ProjectionRefusal {
                    reason: RefusalReason::DecisionIdentityMismatch {
                        supplied_decision_id: decision.decision_id.clone(),
                    },
                    record: take(&mut locator),
                });
            }
            Some(summarize(decision))
        }
        None => None,
    };

    // 5. The accepted schema has the last word.
    if let Err(detail) = outcome.validate() {
        return Projection::Refused(ProjectionRefusal {
            reason: RefusalReason::InvalidOutcome { detail },
            record: take(&mut locator),
        });
    }

    Projection::Projected(RequestProjection {
        schema_version: PROJECTION_SCHEMA_VERSION.to_string(),
        keys: CorrelationKeys {
            outcome_id: outcome.outcome_id.clone(),
            request_id: outcome.request_id.clone(),
            decision_id,
        },
        attempts: outcome
            .attempts
            .iter()
            .enumerate()
            .map(|(index, attempt)| project_attempt(index, attempt))
            .collect(),
        decision: summary,
        success: outcome.success,
        final_status: outcome.final_status,
        failure: outcome
            .terminal_failure_facts()
            .as_ref()
            .map(FailureProjection::from_facts),
        planned_identity: outcome.planned_identity().map(RouteLabel::of),
        last_attempted_identity: outcome.last_attempted_identity().map(RouteLabel::of),
        served_identity: outcome.served_identity().map(RouteLabel::of),
        usage: outcome.usage.as_ref().map(UsageProjection::from),
        total_latency_ms: outcome.total_latency_ms,
        ttft_ms: outcome.ttft_ms,
        estimated_cost: outcome.estimated_cost,
        actual_cost: outcome.actual_cost,
        streaming: outcome.streaming,
        dialect: outcome.dialect.clone(),
        fallback_count: outcome.fallback_count,
        recorded: RecordedClock {
            received: SecondGranularTimestamp::from_unix_secs(outcome.timestamp),
        },
    })
}

/// Project a batch of records against a batch of decisions.
///
/// Pure, and independent of the order of both arguments: `records` is ordered by
/// identifier and `refusals` by canonical serialization, so any permutation of
/// the inputs yields byte-identical output.
///
/// A record whose decision id was not supplied still projects, with
/// `decision: None`. That is "not in the set you gave me", which is a fact about
/// the input, and it is deliberately different from a missing decision id, which
/// refuses.
pub fn project_batch(outcomes: &[Outcome], decisions: &[RouteDecision]) -> ProjectionBatch {
    // Content-ordered index. A BTreeMap, never a HashMap: nothing in here may
    // depend on hash iteration order.
    let mut unique: BTreeMap<&str, &RouteDecision> = BTreeMap::new();
    let mut ambiguous: BTreeSet<&str> = BTreeSet::new();
    for decision in decisions {
        match unique.entry(decision.decision_id.as_str()) {
            Entry::Vacant(slot) => {
                slot.insert(decision);
            }
            Entry::Occupied(_) => {
                ambiguous.insert(decision.decision_id.as_str());
            }
        }
    }

    // How many records claim each outcome id. Records sharing an id cannot be
    // told apart by anything, so every one of them refuses.
    let mut occurrences: BTreeMap<&str, usize> = BTreeMap::new();
    for outcome in outcomes {
        *occurrences.entry(outcome.outcome_id.as_str()).or_insert(0) += 1;
    }

    let mut records: Vec<RequestProjection> = Vec::with_capacity(outcomes.len());
    let mut refusals: Vec<ProjectionRefusal> = Vec::with_capacity(outcomes.len());

    for outcome in outcomes {
        let mut locator = RecordLocator {
            outcome_id: Some(outcome.outcome_id.clone()),
            request_id: Some(outcome.request_id.clone()),
            decision_id: outcome.decision_id.clone(),
            attempt_id: None,
        };

        if let Some(count) = occurrences.get(outcome.outcome_id.as_str()).copied() {
            if count > 1 {
                refusals.push(ProjectionRefusal {
                    reason: RefusalReason::DuplicateOutcomeId { occurrences: count },
                    record: take(&mut locator),
                });
                continue;
            }
        }

        // An unresolvable decision id is a refusal; a merely unsupplied one is
        // not. Which is which is decided here, and never by guessing.
        let supplied = match outcome.decision_id.as_deref() {
            Some(id) if ambiguous.contains(id) => {
                refusals.push(ProjectionRefusal {
                    reason: RefusalReason::AmbiguousDecision {
                        decision_id: id.to_string(),
                    },
                    record: take(&mut locator),
                });
                continue;
            }
            Some(id) => unique.get(id).copied(),
            None => None,
        };

        match project_request(outcome, supplied) {
            Projection::Projected(record) => records.push(record),
            Projection::Refused(refusal) => refusals.push(refusal),
        }
    }

    records.sort_by(|left, right| left.content_order(right));

    // Ties on the identity key are broken by the refusal's own canonical
    // serialization, so two refusals that share a locator still cannot swap
    // places between runs. `to_string` on a struct is field-ordered, so this is
    // a content order and not an address order.
    refusals.sort_by_key(|refusal| {
        serde_json::to_string(refusal)
            .unwrap_or_else(|_| format!("{}:{}", refusal.reason.tag(), refusal.identity_key()))
    });

    debug_assert_eq!(
        records.len() + refusals.len(),
        outcomes.len(),
        "every input is either projected or refused, never both and never neither"
    );

    ProjectionBatch { records, refusals }
}

/// Problems in a supplied decision set itself, independent of any record.
///
/// A duplicate decision id makes the join target ambiguous even when no record
/// happens to reference it, so it is reported here rather than only when a
/// record trips over it. Returned in content order.
///
/// This is outside [`project_batch`]'s per-record accounting, which is why
/// `records.len() + refusals.len()` stays equal to the number of inputs.
pub fn decision_set_problems(decisions: &[RouteDecision]) -> Vec<ProjectionRefusal> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for decision in decisions {
        *counts.entry(decision.decision_id.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, count)| ProjectionRefusal {
            reason: RefusalReason::DuplicateDecisionId {
                decision_id: id.to_string(),
                occurrences: count,
            },
            record: RecordLocator {
                decision_id: Some(id.to_string()),
                ..RecordLocator::default()
            },
        })
        .collect()
}

fn project_attempt(index: usize, attempt: &Attempt) -> AttemptProjection {
    AttemptProjection {
        position: u32::try_from(index).unwrap_or(u32::MAX),
        attempt_id: attempt.attempt_id.clone(),
        candidate: RouteLabel::new(&attempt.candidate_model, &attempt.candidate_provider),
        success: attempt.success,
        failure: attempt
            .failure_facts()
            .as_ref()
            .map(FailureProjection::from_facts),
        latency_ms: attempt.latency_ms,
        ttft_ms: attempt.ttft_ms,
        http_status: attempt.http_status,
        rectified: attempt.rectified,
    }
}

fn summarize(decision: &RouteDecision) -> DecisionSummary {
    DecisionSummary {
        decision_id: decision.decision_id.clone(),
        selected_model: decision.selected.clone(),
        reason: DecisionReasonKind::from(&decision.reason),
        policy_id: decision.policy_id.clone(),
        candidate_count: u32::try_from(decision.candidates.len()).unwrap_or(u32::MAX),
        eligible_count: u32::try_from(
            decision
                .candidates
                .iter()
                .filter(|candidate| candidate.eligible)
                .count(),
        )
        .unwrap_or(u32::MAX),
        fallback_chain_len: u32::try_from(decision.fallback_chain.len()).unwrap_or(u32::MAX),
        planned: decision
            .planned_identity()
            .map(|planned| RouteLabel::new(&planned.model_id, &planned.provider_id)),
    }
}

fn refuse(locator: &mut RecordLocator, error: IdentityError) -> Projection {
    Projection::Refused(ProjectionRefusal {
        reason: error.into_reason(),
        record: take(locator),
    })
}

fn take(locator: &mut RecordLocator) -> RecordLocator {
    std::mem::take(locator)
}
