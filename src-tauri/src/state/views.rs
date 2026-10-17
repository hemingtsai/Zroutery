//! The views the window layer reads. Plain data, serialised across the Tauri boundary, carrying no behaviour.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use zroutery_core::billing::{Balance, Cost};
use zroutery_core::budget::Budget;
use zroutery_core::config::{AppConfig, ConfigIssue};
use zroutery_core::election::Election;
use zroutery_core::router::ModelHealth;
use zroutery_core::stats::{RequestRecord, StatsSummary};

/// How the close button behaves, held where the window layer can read it
/// without going through the whole config document.
#[derive(Debug, Clone, Default)]
pub struct WindowRules {
    pub keep_in_tray: bool,
}

/// What the last balance check found, per provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceStatus {
    pub checked_at: DateTime<Utc>,
    pub balance: Option<Balance>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub address: Option<String>,
    pub base_url: Option<String>,
    pub host: String,
    pub port: u16,
    pub require_auth: bool,
    /// Enough of the token to recognise which one is in play, never the whole
    /// thing: snapshots are handed to the webview on every poll. Use the
    /// `reveal_token` or `copy_token` commands for the real value.
    pub token_hint: String,
    pub exposed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub config: AppConfig,
    /// The exposed id of every entry in `config.models`, in the same order.
    ///
    /// The dashboard renders these instead of deriving ids itself, so the
    /// `<provider>-<model>` rule has exactly one implementation.
    pub exposed_ids: Vec<String>,
    pub issues: Vec<ConfigIssue>,
    pub blocking: bool,
    pub server: ServerStatus,
    /// provider id -> whether an API key is stored for it.
    pub keys: std::collections::BTreeMap<String, bool>,
    pub health: Vec<ModelHealth>,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
    pub warning: Option<String>,
    pub config_path: String,
    pub version: String,
    /// provider id -> last balance check, for the providers that were asked.
    pub balances: BTreeMap<String, BalanceStatus>,
    /// The last election, when one has been held this run.
    pub election: Option<Election>,
    /// Every budget with what has been spent against it, for the dashboard.
    pub budgets: Vec<BudgetStatus>,
    /// Whether this build contains a learning stack at all.
    ///
    /// Declared here so the dashboard can ask before it calls, rather than
    /// discovering the absence from an error. A command that was never
    /// registered and a command that failed are the same event to the webview,
    /// and conflating them is how an operator ends up reading "no learning
    /// stack" out of what is actually a broken bridge.
    pub ml_available: bool,
    /// Whether this build contains account maintenance and browser check-in.
    ///
    /// Same reasoning as [`Self::ml_available`], and the same discipline: the
    /// dashboard asks rather than discovering by calling a command that does not
    /// exist.
    pub account_maintenance_available: bool,
    /// Whether this build contains the account subsystem at all.
    ///
    /// Distinct from [`Self::account_maintenance_available`] on purpose.
    /// `account-maint` implies `account`, so a build can have one without the
    /// other, and the two answer different questions: this one says whether
    /// declared accounts can be probed at all, the other whether they can be
    /// checked in through a browser. A dashboard that reads one for the other
    /// offers a control the build cannot serve.
    ///
    /// Same reasoning as [`Self::ml_available`]: a command that was never
    /// registered and a command that failed are the same event to the webview.
    pub account_available: bool,
    /// What each declared account is doing, newest observation last.
    #[cfg(feature = "account-maint")]
    pub checkin: Vec<CheckinView>,
    /// Observed resource events, newest first.
    ///
    /// Carried so an operator can see *why* a balance moved and, for each entry,
    /// how strongly that was established. The amount and the evidence travel
    /// together on purpose: a figure the provider reported in a structured field
    /// and one recovered out of log text must not read the same.
    #[cfg(feature = "account-maint")]
    pub account_events: Vec<AccountEventView>,
    /// Per-account balances, for providers that declare more than one account.
    #[cfg(feature = "newapi")]
    pub account_balances: Vec<AccountBalanceView>,
}

/// How much durable history the operator has, as bytes and as a record count.
///
/// `records` is `None` when the log could not be counted, which is a different
/// answer from zero records and must not render as one. `error` carries that
/// reason; the counters are reported either way, because "the log is not
/// readable" and "the log is not being written to" are separate faults and the
/// counters are what tells them apart.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, Serialize)]
pub struct MlTraceInfo {
    pub records: Option<usize>,
    pub bytes_on_disk: u64,
    pub path: Option<String>,
    pub counters: Option<zroutery_core::ml::TraceCounters>,
    pub error: Option<String>,
}

/// What discarding the durable history removed.
///
/// `cleared: false` with no error is a real answer rather than a failure:
/// deleting an empty log did exactly what was asked.
#[cfg(feature = "ml")]
#[derive(Debug, Clone, Serialize)]
pub struct MlTraceClear {
    pub cleared: bool,
    pub removed_bytes: u64,
    pub error: Option<String>,
}

/// One budget and how much of it is gone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub budget: Budget,
    pub spent: Cost,
    /// 0.0 to 1.0, and beyond: over 1.0 means the limit has been passed, which
    /// happens because a request is only charged once it has finished.
    pub used: f64,
}

/// The subset the Activity tab polls for.
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub health: Vec<ModelHealth>,
    pub summary: StatsSummary,
    pub recent: Vec<RequestRecord>,
}

/// What a panel needs to know about one account's check-in.
///
/// Deliberately a **view** and not the runtime types. The browser's phase is a
/// plain enum rather than `BrowserSession`, the reward is the core's
/// `CheckinReward`, and there is no field here for a cookie, a profile path or a
/// port — so this struct cannot leak a session even by accident, and it is the
/// only check-in shape that reaches the webview.
#[cfg(feature = "account-maint")]
#[derive(Debug, Clone, Serialize)]
pub struct CheckinView {
    pub provider_id: String,
    pub account_id: String,
    /// `"idle"`, `"running"`, `"waiting_for_user"`, `"succeeded"`,
    /// `"already_completed"`, `"not_supported"`, `"failed"` or `"cancelled"`.
    ///
    /// Named here rather than serialising the Rust enum so the webview has a
    /// stable wire vocabulary that a `#[serde(rename_all)]` change cannot move
    /// underneath it.
    pub phase: String,
    /// Whether a browser is held open for this account right now.
    pub browser_held: bool,
    /// Whether the user is being asked to finish a challenge in it.
    pub awaiting_user: bool,
    /// What stopped it, when it stopped, and why.
    pub failure: Option<String>,
    /// The reward, in the account's reported unit — only when one was observed.
    pub reward_amount: Option<f64>,
    pub reward_unit: Option<String>,
    /// Where the observed reward came from.
    ///
    /// Carried so a panel can render a figure recovered from provider log text as
    /// what it is, rather than as an authoritative number.
    pub reward_source: Option<String>,
    pub last_completed_at: Option<i64>,
    /// Why the scheduler did or did not act on this account, and when it next will.
    ///
    /// Carried per account rather than as a panel-wide summary because the six
    /// reasons are per-account facts: one account switched off, another missing a
    /// selector and a third simply not due yet all need different words, and a count
    /// per kind would tell an operator how many are broken without telling them which.
    ///
    /// `next_due_at` is set only when a decision names a time, so its absence is
    /// informative rather than a zero: there is no future time for an account that
    /// needs a first run, and rendering 0 as a date would be a lie about the epoch.
    pub schedule: CheckinScheduleView,
}

/// One observed resource event, as a panel receives it.
///
/// A projection rather than the core type itself, for the same reason the phase is
/// a named string rather than a serialised enum: the webview gets a vocabulary that
/// a `#[serde(rename_all)]` change cannot move underneath it.
#[derive(Debug, Clone, Serialize)]
pub struct AccountEventView {
    pub provider_id: String,
    pub account_id: String,
    /// When the provider says it happened, in unix seconds.
    pub observed_at: i64,
    /// What kind of thing it was: `system_grant`, `replenishment`, and so on.
    pub kind: String,
    /// What it did to the balance, in the layer's own words.
    ///
    /// `credited`, `debited`, `no_change` or `unobserved` — and the last two are
    /// different facts, not synonyms: one says nothing moved and the other says we
    /// could not find out.
    pub effect: String,
    /// The amount, only when one is established.
    pub amount: Option<f64>,
    pub unit: Option<String>,
    /// Where the figure came from: `operation_response`, `account_state` or
    /// `account_log`. The weakest of these is the log.
    pub source: String,
    pub description: String,
}

/// The scheduler's answer for one account.
#[derive(Debug, Clone, Serialize)]
pub struct CheckinScheduleView {
    /// `disabled`, `not_applicable`, `needs_first_observation`, `no_policy`,
    /// `not_due` or `due`.
    pub decision: String,
    /// When the next attempt is due, when the decision names one.
    pub next_due_at: Option<i64>,
    /// When the last attempt ended, which is what the interval is measured from.
    pub last_settled_at: Option<i64>,
}

/// How many observed events a snapshot carries.
///
/// Bounded so a long-running application cannot grow its snapshot without limit. The
/// log is bounded too; this is the smaller of the two bounds and is about the wire.
#[cfg(feature = "account-maint")]
pub const ACCOUNT_EVENT_SNAPSHOT_LIMIT: usize = 50;

/// One account's probed state, as a panel receives it.
///
/// Only carried for providers that declare more than one account, because with a
/// single account the provider-level balance is the same fact and two panels would
/// say it twice. With several, the provider figure is a total that hides which
/// account is empty and which is suspended — which is the whole reason to look.
#[cfg(feature = "newapi")]
#[derive(Debug, Clone, Serialize)]
pub struct AccountBalanceView {
    pub provider_id: String,
    pub account_id: String,
    /// `active`, `suspended`, `quota_exhausted`, `rate_limited`,
    /// `authentication_expired`, `unknown` or `unprobed`.
    pub status: String,
    /// Present only when the provider reported a quota at all.
    pub remaining: Option<f64>,
    pub total: Option<f64>,
    pub used: Option<f64>,
    pub unit: Option<String>,
    /// When the provider says this was true, in unix seconds.
    pub last_sync: Option<i64>,
    /// When the last attempt failed. Carried separately from `status` because a
    /// provider that stopped answering leaves the previous reading in place, and
    /// showing that reading as current is the failure this whole layer prevents.
    pub last_failure: Option<i64>,
}

impl CheckinScheduleView {
    /// The placeholder a stored report carries before it is read.
    ///
    /// The schedule is *derived* from the live configuration and the durable
    /// history, never stored, because a stored copy would be stale the moment the
    /// operator edits an interval. Both read paths overwrite this before returning,
    /// so it is never what a caller sees — it exists so the write paths do not each
    /// have to know how to compute it.
    pub fn unknown() -> Self {
        Self {
            decision: "unknown".into(),
            next_due_at: None,
            last_settled_at: None,
        }
    }
}

#[cfg(feature = "account-maint")]
impl CheckinView {
    /// A view for an account that has never been checked in.
    ///
    /// Phase `idle` rather than an error, so a freshly configured account renders
    /// as not-yet-done instead of as broken.
    pub fn idle(provider_id: &str, account_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            account_id: account_id.to_string(),
            phase: "idle".to_string(),
            browser_held: false,
            awaiting_user: false,
            failure: None,
            reward_amount: None,
            reward_unit: None,
            reward_source: None,
            last_completed_at: None,
            // Overwritten on every read; see `CheckinScheduleView::unknown`.
            schedule: CheckinScheduleView::unknown(),
        }
    }
}

/// The panel root of a relay's base URL, with any `/api` or `/v1` suffix removed.
///
/// Providers are configured with the *relay* base, and a panel is the same host
/// with a different suffix. Repeated from the adapter's own normalisation rather
/// than reusing it, because that function is private to the adapter and this
/// only needs an origin — and because a value the adapter later refuses is
/// caught by its validation, so a wrong guess here fails at the call rather than
/// silently.
// Gated on `newapi` for the same reason as `panel_adapter`: this normalises a
// base URL and nothing else, and gating it on the browser maintenance surface
// made `--features newapi` alone uncompilable — a build that ships the NewAPI
// adapter without the browser surface could not use the adapter at all.
#[cfg(feature = "newapi")]
pub(super) fn panel_root(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    for suffix in ["/api", "/v1"] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    trimmed.to_string()
}

/// `zr-1234abcd…` -> `zr-…abcd`: enough to tell two tokens apart, useless on its
/// own.
pub fn token_hint(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("zr-…{tail}")
}
