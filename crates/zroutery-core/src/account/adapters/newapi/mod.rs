//! NewAPI account adapter.
//!
//! [NewAPI] is a multi-provider LLM relay that also ships a dashboard HTTP API
//! for accounts: balance, consumption logs, check-in and a public status
//! endpoint. This adapter maps that API onto the provider-agnostic
//! [`AccountProvider`] trait. Everything NewAPI-specific — endpoint paths,
//! quota units, error codes, credential shapes — stays inside this module; the
//! rest of the core only sees [`AccountRuntime`], [`AccountQuota`] and friends.
//!
//! [NewAPI]: https://github.com/Calcium-Ion/new-api
//!
//! # Endpoints
//!
//! Verified against `Calcium-Ion/new-api` at commit `c2b7a9a` (2026-09-25):
//!
//! | Endpoint | Auth | Used for |
//! |---|---|---|
//! | `GET  /api/status` | none | reachability, `quota_per_unit`, check-in and Turnstile flags |
//! | `GET  /api/user/self` | bearer | quota, used quota, request count, account status |
//! | `GET  /api/log/self/stat` | bearer | consumed quota for a window, plus `rpm`/`tpm` |
//! | `GET  /api/log/self` | bearer | paged consume logs, summed for token counts |
//! | `POST /api/user/auth/refresh` | refresh cookie | rotate a dashboard access token |
//! | `GET/POST /api/user/checkin` | bearer | check-in status and daily check-in |
//! | `GET  /api/subscription/self` | bearer | whether a zero wallet is still funded by a plan |
//!
//! # Credentials
//!
//! Modern NewAPI dropped cookie sessions: the dashboard authenticates with
//! `Authorization: Bearer <token>`, where the token is either a personal
//! access token (`GET /api/user/token`) or a short-lived login JWT. A login
//! JWT can be renewed with the `new_api_refresh` cookie through
//! `POST /api/user/auth/refresh`; this adapter performs that renewal once when
//! a call comes back `401`, and stores the rotated cookie. The generation of
//! panels after that re-introduced the `New-Api-User` id header on the log and
//! check-in surfaces — and AgentRouter's on `/api/user/self` too — so every
//! authenticated call carries the id once the session knows it (see
//! `http.rs`).
//!
//! # Quota units
//!
//! NewAPI keeps a synthetic credit where `quota_per_unit` units equal one US
//! dollar (`common.QuotaPerUnit`, default `500_000`); the panel's own
//! `logger.LogQuota` converts with `usd = quota / quota_per_unit`. This adapter
//! reports every money value in USD, reads the live `quota_per_unit` from
//! `GET /api/status`, and keeps the raw credit numbers in
//! `AccountRuntime::metadata` under `newapi.*` keys.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`types`] | what a caller configures and gets back — credential, panel status, usage report |
//! | [`wire`] | the shapes NewAPI puts on the wire, and the in-memory session. None of these leave the module |
//! | [`auth`] | constructing an adapter and turning a credential into a verified session |
//! | [`checkin_ops`] | running the daily check-in and observing what it did |
//! | [`session`] | the rotating refresh cookie and the single-flight guard around it |
//! | [`http`] | one authenticated call, the one-shot token renewal, the status cache |
//! | [`panel`] | reading the account, the rate limits, and the metadata carried forward |
//! | [`provider`] | the provider-agnostic surface the rest of the core sees |
//! | [`helpers`] | the small pure functions the calls share |
//!
//! The five impl modules carry the methods of one type. Rust allows a single
//! inherent `impl` to be split by concern and the compiler merges them; the
//! cost is that finding `NewApiAdapter::authenticate` means knowing which file
//! to open. This table is the map, and the order is the order a request takes.
//!
//! # What is deliberately not supported
//!
//! * Check-in on an instance that requires a Turnstile challenge and no
//!   challenge response is supplied — a headless client cannot solve it, so
//!   [`NewApiAdapter::checkin_with_turnstile`] reports "not supported" instead
//!   of burning a doomed request.
//! * Rate-limit *limits*: NewAPI exposes current `rpm`/`tpm` usage but no
//!   per-account ceiling, so [`RateLimitState`] carries usage with unknown
//!   limits (its `pressure()` stays `0.0`).

use wire::InstalledSession;

/// The upstream routes this adapter depends on, as `(method, path, guard)`.
///
/// Transcribed from `router/api-router.go` at `Calcium-Ion/new-api` commit
/// `c2b7a9a9e0b548c2051a949fceabb59029adcb49` (2026-09-25), which is the commit
/// the module documentation above cites. It is public and `const` so that
/// `newapi_upstream_routes_test.rs` can check the adapter's calls against it.
///
/// Two properties make it worth having as data rather than as prose:
///
/// * **The OpenAPI spec is not a usable source for it.** `docs/openapi/api.json`
///   at the same commit documents 136 paths and includes neither
///   `/api/subscription/self` nor `/api/user/checkin` -- the endpoints behind the
///   subscription/quota rules and the entire check-in path. Both are present in
///   the Go router. Treating "absent from the spec" as "absent upstream" would
///   have deleted two working endpoints.
/// * **The guard is part of the entry.** Whether a route sits behind
///   `UserAuth` decides if a request is answered or refused, so a transcription
///   that dropped it would make an unauthenticated probe look correct.
pub const UPSTREAM_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/api/status", "none"),
    ("GET", "/api/user/self", "UserAuth"),
    (
        "POST",
        "/api/user/auth/refresh",
        "SessionCookieOriginGuard, CriticalRateLimit",
    ),
    ("GET", "/api/log/self", "UserAuth"),
    ("GET", "/api/log/self/stat", "UserAuth"),
    ("GET", "/api/subscription/self", "UserAuth"),
    ("GET", "/api/user/checkin", "UserAuth"),
    ("POST", "/api/user/checkin", "UserAuth, TurnstileCheck"),
];

/// `provider_id()` reported by this adapter.
const PROVIDER_ID: &str = "newapi";
/// Refresh cookie name (`service.RefreshCookieName`).
const REFRESH_COOKIE_NAME: &str = "new_api_refresh";
/// Quota units per US dollar, as a fallback when `/api/status` cannot tell us
/// (`common.QuotaPerUnit` in NewAPI).
///
/// Public because a caller that reads an observation has to name a unit for the
/// figure before it has an instance to ask: `common.QuotaPerUnit` is the
/// documented default and using it is an assumption, which is why the adapter
/// still prefers the live `quota_per_unit` from `/api/status` wherever it can.
pub const DEFAULT_QUOTA_PER_UNIT: f64 = 500_000.0;
/// `common.UserStatusEnabled`.
const USER_STATUS_ENABLED: i64 = 1;
/// `common.UserStatusDisabled`.
const USER_STATUS_DISABLED: i64 = 2;
/// NewAPI clamps `page_size` to 100 (`common.GetPageQuery`).
const LOG_PAGE_SIZE: u32 = 100;
/// Default usage window for [`AccountProvider::fetch_usage`].
const DEFAULT_USAGE_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;
/// `/api/status` is public and changes rarely; caching it keeps `refresh()`
/// down to the calls that actually carry account state.
const STATUS_CACHE_TTL: Duration = Duration::from_secs(300);

/// NewAPI account adapter.
pub struct NewApiAdapter {
    config: NewApiConfig,
    client: reqwest::Client,
    /// API root, already normalised by [`normalize_base_url`].
    base: String,
    session: RwLock<Option<InstalledSession>>,
    /// Mints the generation for each installed session. Only ever moves forward.
    session_generation: AtomicU64,
    /// Single-flight guard for refresh: one task redeems the rotating cookie
    /// while the others wait and then reuse the session it installed.
    refresh: tokio::sync::Mutex<()>,
    status_cache: RwLock<Option<(Instant, NewApiStatus)>>,
}

use std::sync::atomic::AtomicU64;
use std::sync::RwLock;
use std::time::{Duration, Instant};

pub mod checkin;
pub mod logs;

pub use checkin::{
    CheckinAttempt, CheckinContext, CheckinObservation, NewApiCheckinRecord, NewApiCheckinSnapshot,
    NewApiCheckinStats, NewApiCheckinStatus, NewApiDoCheckin,
};
pub use logs::{
    interpret as interpret_log, read_system_signal, LogContext, NewApiLogItem, NewApiLogOther,
    NewApiLogPage, NewApiSystemSignal, LOG_TYPE_CONSUME, LOG_TYPE_ERROR, LOG_TYPE_LOGIN,
    LOG_TYPE_MANAGE, LOG_TYPE_REFUND, LOG_TYPE_SYSTEM, LOG_TYPE_TOPUP, LOG_TYPE_UNKNOWN,
};

mod auth;
mod checkin_ops;
mod helpers;
mod http;
mod panel;
mod provider;
mod session;
mod types;
mod wire;

#[cfg(test)]
mod tests;

pub use types::{AuthenticatedSession, NewApiAuth, NewApiConfig, NewApiStatus, NewApiUsageReport};
