# NewAPI account adapter

`crates/zroutery-core/src/account/adapters/newapi.rs` maps a [NewAPI]
(formerly one-api lineage) dashboard onto the provider-agnostic
`AccountProvider` trait. This note records what the adapter talks to, why the
mapping looks the way it does, and what is still missing on the application
side.

[NewAPI]: https://github.com/Calcium-Ion/new-api

## Status

| Piece | State |
| --- | --- |
| Panel client, auth, quota, usage, health, check-in | implemented, `#[cfg(feature = "newapi")]` |
| Tests | 55 unit tests against an in-process fake panel, run by CI (`--all-features`) |
| Default build | untouched — `newapi = ["account"]`, `default = []` |
| Desktop shell (`src-tauri`) | **not wired**: nothing enables `account`/`newapi`, no config or GUI surface exists for accounts |

The earlier revision of this file was a stub: `refresh`/`health_check` returned
constants and `fetch_usage`/`fetch_quota` returned "not yet implemented". The
history treatment map's T33 entry ("the NewAPI adapter with quota auth")
described more than the code did; this document is the accurate record.

Upstream reference: `Calcium-Ion/new-api` at
`c2b7a9a9e0b548c2051a949fceabb59029adcb49` (2026-09-25), plus its
`docs/authentication.md` and `docs/openapi/api.json`.

## Authentication

Modern NewAPI dropped cookie sessions and the `New-Api-User` header:

- Dashboard calls authenticate with `Authorization: Bearer <token>`, where the
  token is either a personal access token from `GET /api/user/token` (stored in
  `users.access_token`, no expiry) or a 15-minute login JWT.
- A login JWT is renewed at `POST /api/user/auth/refresh`, which reads the
  `HttpOnly` `new_api_refresh` cookie and **rotates** it on every call.

Mapping:

| `NewApiAuth` | Behaviour |
| --- | --- |
| `ApiKey` | Personal access token; probed against `GET /api/user/self` before the session is installed. A pasted `Bearer ` prefix is tolerated. |
| `OAuth2` | `access_token` is the bearer; `refresh_token`, when given, is the `new_api_refresh` cookie value used for renewal. |
| `Cookie` | The cookie value is exchanged once for an access token; the rotated cookie from `Set-Cookie` replaces the one supplied. |

Any authenticated call that comes back `401` is retried **once** after a
renewal, but only when a refresh cookie is held; a personal access token cannot
be renewed and surfaces `Error::Unauthorized` directly. Sessions live in the
adapter behind an `RwLock`, so an `&self` call can renew the credential.

`NewApiConfig::api_key`, when set, is installed as the initial credential and
verified lazily by the first authenticated call. `NewApiAdapter::authenticate`
replaces it and verifies eagerly.

## Endpoint mapping

| Adapter | Endpoint | Notes |
| --- | --- | --- |
| `AccountProvider::refresh` | `GET /api/status`, `GET /api/user/self`, `GET /api/log/self/stat`, `GET /api/subscription/self` (conditional) | `/api/status` is cached for 5 minutes; the rate probe is best-effort |
| `AccountProvider::fetch_quota` | `GET /api/status`, `GET /api/user/self` | |
| `AccountProvider::fetch_usage` | `GET /api/log/self/stat`, `GET /api/log/self` | last 30 days by default; `usage_report` takes an explicit window |
| `AccountProvider::health_check` | `GET /api/user/self` when authenticated, else `GET /api/status` | |
| `AccountProvider::checkin` | `GET /api/status`, `GET/POST /api/user/checkin` | |
| `NewApiAdapter::probe_status` | `GET /api/status` | never cached, for a "test connection" action |

All log reads pass `type=2` (`model.LogTypeConsume`), because top-up, system and
error entries are not consumption. `page_size` is fixed at 100, which is what
NewAPI clamps it to (`common.GetPageQuery`).

## Semantics worth knowing

**Quota units.** NewAPI keeps a synthetic credit: `usd = quota / quota_per_unit`
(`common.QuotaPerUnit`, default `500_000`, reported by `GET /api/status`; the
panel's own `logger.LogQuota` converts the same way). The adapter reports every
money value in USD and keeps `newapi.quota_raw`, `newapi.used_quota_raw` and
`newapi.quota_per_unit` in `AccountRuntime::metadata`.

**Granted total.** NewAPI tracks a remaining balance plus lifetime consumption,
so `AccountQuota::total` is their sum. Top-ups, redemptions and refunds make
that an approximation of what was ever granted, not a column in the database.

**`QuotaExhausted` is a judgement, not a fact.** A zero or negative wallet is
only reported as exhausted when nothing else funds the account: self-use mode
(`self_use_mode_enabled`) ignores balances, and `GET /api/subscription/self` can
show an active plan paying for a wallet that reads empty. Instances old enough
to lack the subscription endpoint answer 404, which is treated as "no plan" —
the wallet is then the only funding source, which is the question being asked.

**Usage costs a sweep.** `GET /api/log/self/stat` aggregates consumed quota for
a window and reports `rpm`/`tpm` for the **last 60 seconds** (not for the
window), so the adapter uses it for two different things. Token counts are not
aggregated by NewAPI at all, so `fetch_usage` walks log pages up to
`usage_page_limit * 100` entries and sets `NewApiUsageReport::truncated` rather
than extrapolating. Per-model breakdowns are available in the same log entries
but are not surfaced yet.

**Rate limits.** NewAPI exposes current `rpm`/`tpm` usage but no per-account
ceiling. `RateLimitState` therefore carries usage with unknown limits; its
`pressure()` stays `0.0`, which is honest rather than invented.

**Check-in.** Check-in is disabled on many instances, and when Turnstile is
enabled `POST /api/user/checkin` needs a `turnstile` query parameter that only a
browser can produce. Both cases return `AccountOpResult::NotSupported` with no
request sent. Callers that do hold a challenge response use
`checkin_with_turnstile`. A repeat check-in on the same day is reported as
`Success`, since the day's reward is already granted; any other business refusal
becomes `AccountOpResult::Failed` carrying the panel's code and message.

**Transport hygiene.** The client sets a connect timeout, a whole-request
timeout, a user agent, and `redirect(Policy::none())` — a redirect would hand
the bearer to whatever host the panel names. Panel response bodies reach the
log through `Error::Upstream`, whose `safe_message()` redacts them for clients,
matching the rest of the core.

**`base_url` shapes.** Operators paste the panel root (`https://host`), the API
root (`https://host/api`) or the relay base (`https://host/v1`); all three
normalise to the same origin. Credentials embedded in the URL, a query string,
a fragment, or a non-HTTP scheme are rejected by `NewApiConfig::validate`.

**Header injection.** Cookie values and API keys are rejected when they contain
control characters or, for cookies, `;` — a user-supplied credential must not be
able to add headers or cookies.

## Tests

The adapter is tested against an in-process fake panel (`axum` on an ephemeral
port) whose handlers mirror the Go responses: the envelope
`{success, message, code, data}`, the `SelfData` shape, paginated logs, the
rotating refresh cookie, and HTTP 200 with `success: false` for business
failures. Coverage includes credential rejection, refresh-once-then-retry,
cookie rotation, quota/status mapping, subscription and self-use exceptions,
usage paging and truncation, check-in paths, transport failure, malformed
payloads and redaction.

```sh
cargo test -p zroutery-core --all-features
```

## Remaining application work

Enabling the adapter in the shipped binary is a separate change, because
nothing consumes `AccountProvider` yet:

1. Enable `zroutery-core/newapi` for `src-tauri` (and decide whether the
   desktop build ships the `account` subsystem at all).
2. Store the credential in the existing keychain path (`SecretStore` /
   keyring), not in `AppConfig` plaintext.
3. Decide where accounts live in `AppConfig` and expose a GUI surface (add /
   test / show quota and usage), reusing `probe_status` for the connection test.
4. Decide who polls `refresh` — on demand, or a background store keyed by
   `AccountId`.
