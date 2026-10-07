# OpenCode Zen providers

Two opt-in upstream providers for OpenCode's gateway, kept **independent of each
other** — neither is a prerequisite of the other:

- **`OPENCODE-ZEN-FREE`** — the anonymous free tier: `Bearer public` against
  `/zen/v1`, no login. Serves the free model catalog.
- **`OPENCODE-CONSOLE`** — the authenticated Console tier: device-code OAuth →
  access token (`+ refresh`, `org_id`) against `/inference/openai/v1`. Serves the
  Console/OAuth catalog.

Both speak the OpenAI Chat Completions wire format, so they reuse Zroutery's
existing OpenAI egress and add only the pieces OpenCode's gateway demands.

This note records what the upstream actually checks (field-verified, not
inferred), how the two providers map onto Zroutery, what is reused, and what is
deliberately not implemented.

## Status

| Piece | State |
| --- | --- |
| Upstream behaviour | field-verified 2026-10 (reverse-proxy capture + per-item bisection) |
| Design | this document; nodes `OPENCODE-ZEN-FREE` / `OPENCODE-CONSOLE` are `READY` |
| Code | not started |

The DAG records added with this design do not yet pass
`scripts/orch_docs_test.py` on their own: `CLIENT-IDENTITY` cites `E-120`, which
is not defined in `evidence-registry.md` (the registry tops out at `E-119`).
That is a pre-existing gap in uncommitted work, not introduced here; the two
`OPENCODE-*` records themselves validate.

## Upstream reference

- **Client storage / identity** — `anomalyco/opencode` at `dev`:
  `packages/opencode/src/auth/index.ts` (auth file shape),
  `packages/core/src/global.ts` (data path). The Console OAuth token a real
  client writes lives at `<data>/opencode/auth.json`, keyed by provider id
  (`"opencode"`), value `{ type: "oauth", access, refresh, expires, accountId? }`.
- **Reference implementations** — `jasonxu114514/opencode2api` (Go) and
  `shadow3aaa/DaatLocus` (Rust, `src/providers.rs`, `src/providers/opencode_oauth.rs`).
- **Gateway endpoints** — Zen `https://opencode.ai/zen`; Console inference
  `https://opencode.ai/inference/openai/v1`; capability catalog
  `https://models.opencode.ai/api.json`.

The field evidence below was produced by pointing the OpenCode client at a local
reverse proxy and bisecting one header/body field at a time.

## The free-tier gate (field-verified)

A request reaches the free tier only when **all** of these hold; otherwise the
gateway answers `403 FreeTierError`:

1. body `stream: true`.
2. `tools` contains function definitions named exactly **`bash` and `read`**
   (case-sensitive; descriptions/parameters may be fake; removing either fails).
3. `User-Agent` contains `opencode/<ver>` with `<ver> >= 1.18.0` (a lower version
   answers `426 UpgradeRequired`). Substring match — embedding in a longer UA is
   fine.
4. `x-opencode-session` present, shape `ses_` + 12 lowercase hex + 14 base62
   (30 chars). `x-opencode-client`/`project`/`request` are optional.
5. `Authorization` is not required: the free models answer with no key. This
   provider sends `Bearer public` anyway, matching the reference implementations.

Two further observed facts:

- **The gate is shape-only; the usage limit is per-IP.** A flagged IP still
  passes the shape check, but the free tier then answers `429 FreeUsageLimitError`
  ("Error from provider (Console): Rate limit exceeded"). Switching the egress IP
  returned `200` immediately. So a proxy pool is a real throughput lever for this
  provider, not a necessity for the gate.
- **The reply is standard OpenAI Chat Completions SSE**, with `reasoning_content`
  deltas, a `usage` object (sometimes on every chunk, sometimes only on the last),
  a terminal `[DONE]`, and one OpenCode-specific trailer
  `data: {"choices":[],"cost":"0"}`.

The real client also sends `User-Agent: opencode/1.18.31 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14`,
`x-opencode-client: cli`, `x-opencode-project: global`, `x-opencode-request: msg_…`,
`tool_choice: "auto"`, and `stream_options: {include_usage: true}`.

## Provider A — `OPENCODE-ZEN-FREE`

| | |
| --- | --- |
| Egress | OpenAI Chat Completions (`/v1/chat/completions`) |
| Base | `https://opencode.ai/zen` |
| Credential | `Bearer public` (constant; no login) |
| Shaping | required (the gate above) |
| Discovery | `GET /zen/v1/models` (ungated) + capability catalog |
| Rate limit | per egress IP (`429 FreeUsageLimitError`) |

Verified reachable anonymously (200 on a spoofed request): `fledge-alpha-free`,
`ling-3.1-flash-free`, `longcat-2.5-preview-free`, `space-bunny-free`,
`mimo-v2.6-flash-free`, `nemotron-3.5-lightning-free`, `nemotron-3-ultra-free`,
`big-pickle`. (`muse-spark-1.3-free` is **not** a real id — the catalog id is
`muse-spark-1.3-contributor-free`; derive no ids by hand.)

## Provider B — `OPENCODE-CONSOLE`

| | |
| --- | --- |
| Egress | OpenAI Chat Completions |
| Base | `https://opencode.ai/inference/openai/v1` |
| Credential | device-code OAuth → access token; `x-opencode-org-id: <org_id>` |
| Shaping | same identity as A (the gateway is the same host) |
| Discovery | `GET {server}/api/config` → `config.provider.*.models` |

Device-code flow (from the reference implementations): start at
`POST {console}/auth/device/code` (`client_id: "opencode-cli"`), poll
`POST {console}/auth/device/token` (`grant_type=…device_code`), refresh with
`grant_type=refresh_token`. The Console origin is `https://opencode.ai/console`.
The token response need not carry the billing org: resolve it with
`GET {console}/api/orgs` (Bearer) and take the first org id, sent back as
`x-opencode-org-id` (DaatLocus `opencode_fetch_org_id`).

## Config surface

Two variants on the existing fieldless `ProviderKind` (it selects the dialect and
the default base only; everything else stays on `ProviderConfig`):

- **`ProviderKind::OpenCodeZenFree`** — dialect `OpenAI`, default base
  `https://opencode.ai/zen`, credential `public` (constant), discovery
  `/zen/v1/models`.
- **`ProviderKind::OpenCodeConsole`** — dialect `OpenAI`, default base
  `https://opencode.ai/inference/openai/v1`, credential = the stored OAuth token,
  discovery `GET {console}/api/config`.

Why a kind and not a pile of orthogonal flags: B's **credential lifecycle**
differs (a rotating OAuth token, not a static key), and both differ from a plain
`OpenAICompatible` provider in discovery and shaping. A kind keeps the router,
the UI and the discovery switch readable. `ProviderClientProfile` stays the
shaping carrier: both kinds force it to `OpenCodeCli` (a plain provider can also
select it, but the kinds are the supported way). Validation must reject an
`OpenCodeZenFree`/`OpenCodeConsole` provider whose `client_profile` is not
`OpenCodeCli`.

The OAuth token for B is stored through the existing secret path
(`key_ref` → `SecretStore`/keychain), not in `AppConfig` plaintext: the same
place, and the same private-get/private-set discipline, that every other provider
credential uses. `ModelEntry::exposed_id()` stays `<provider_id>-<upstream_model>`.

## Shared component: the `opencode` shaping

Both providers select the **same** identity profile, `ProviderClientProfile::OpenCodeCli`,
and share one shaping module. That module owns only OpenCode-specific knowledge:

- **Identifier / session** — port `opencode_identifier` from DaatLocus
  (`src/providers.rs`): `hex48(ts_ms * 0x1000 + counter)` (12 lowercase hex) + 14
  **random** base62, bit-inverted when descending; prefix `ses_` (session) or
  `msg_` (request). The 14 random characters never form a sequential run, which
  is why this sidesteps an unexplained Console rule that rejects hand-made
  sequential ids. Keep one stable `ses_` per conversation (reuse is safe and
  preserves the upstream prompt cache — the free reply reports `cached_tokens`).
- **Headers** — a locked, non-configurable fingerprint: the `opencode/<ver>`
  User-Agent plus `x-opencode-client: cli`, `x-opencode-project: global`,
  `x-opencode-session`, `x-opencode-request`.
- **Body shaping** — force `stream: true`; ensure the tool list contains a
  function named exactly `bash` and one named exactly `read`. A client tool that
  already carries one of those names is left **verbatim** — the gate checks only
  the names, so its description and parameters are never rewritten. Only a
  *missing* name is filled with a minimal placeholder.
  `stream_options.include_usage` comes from the existing `ProviderQuirks::stream_usage`
  (default on). `tool_choice` is left to the caller, **except** that when the
  client declared no tools of its own it is set to `"none"` — otherwise the
  injected placeholders invite a tool call in place of an answer. Whether the
  gateway accepts `tool_choice: "none"` is not yet tested.
- **Tool reconciliation** — injected placeholders exist only to satisfy the gate.
  On the way back, drop tool calls that name a placeholder the client did not
  declare (computed from the request's own tool list).

Everything *generic* stays in the modules being reused — the HTTP client, the
OpenAI encoder/decoder, the SSE machinery.

## Session and header wiring

`ClientIdentity` (`config.rs`) today carries only `{profile, passthrough}` — it
has no conversation, so `build_headers_for_identity` currently cannot emit a
`ses_`. Add an `Option<String> session` to `ClientIdentity`, resolved once per
attempt by the pipeline (the same place that builds the identity today), from, in
order:

1. an inbound `x-opencode-session` that is **already canonical** (worth
   preserving: it carries the client's upstream prompt-cache affinity);
2. the conversation identity Zroutery already computes for session pinning
   (`session.rs`);
3. the first user message (the reference implementations' fallback seed).

The chosen signal is canonicalised with `opencode_identifier` into `ses_…`; the
transform is idempotent, so case 1 is a no-op. The `OpenCodeCli` arm of
`build_headers_for_identity` then emits the fingerprint plus that session plus a
fresh `msg_…` per attempt.

`x-opencode-session` must **not** travel through the generic passthrough
allowlist: passthrough is verbatim and would forward a non-canonical client value
straight into a `403`. The profile arm owns the header. Auxiliary calls with no
conversation (discovery, a "test connection") carry no session; they are GETs or
excluded, so they never face the gate.

## Non-streaming compatibility

The gate forces `stream: true` upstream, so a client that asked for a
non-streaming reply still receives SSE. The response side therefore has two paths:

- client wanted `stream` → pass the SSE through (existing stream path);
- client wanted JSON → **collapse** the SSE into a `ChatResponse`.

Wiring: in the pipeline's non-stream branch, when the resolved provider is an
`OPENCODE-*` kind, call `stream_with_identity` (the shaped body already forces
`stream: true`) and fold the `EventStream` through a new `StreamCollector` into a
`ChatResponse`, then continue the existing non-stream response path
(`protocol::encode_response`). A mid-stream SSE **error** event must surface as an
`Error` — a JSON client must never receive an empty success because the failure
arrived inside the stream.

The collapse is a fold over the existing canonical `StreamEvent` (`ir/mod.rs`)
into `ChatResponse`, sitting beside `StreamParser`/`StreamEncoder` in `protocol`:

| `StreamEvent` | → `ChatResponse` |
| --- | --- |
| `Start{id, model, usage}` | `id` / `model` / initial `usage` |
| `TextDelta{index, text}` | accumulate → `ContentBlock::Text` |
| `ThinkingDelta{index, text}` | accumulate → `ContentBlock::Thinking` |
| `ThinkingSignature` / `RedactedThinking` | attach to the block |
| `ToolUseStart` + `ToolUseDelta` | assemble `ContentBlock::ToolUse` |
| `Stop{stop_reason, stop_sequence, usage}` | terminal fields (last usage wins) |
| `BlockStop` / `Ping` | ignored |

Parser robustness required by the observed streams: usage on any chunk (last
wins), `null` delta fields, a `choices: []` usage-only chunk,
text-empty-but-thinking-present, and the `{"choices":[],"cost":"0"}` trailer. The
existing OpenAI parser already handles the last two shapes (`openai.rs` skips an
empty-`choices` chunk and stops at `[DONE]`), and already decodes
`reasoning_content` into a thinking block — so both are reuse, not new work.

## Discovery

Two steps; neither needs a fingerprint:

1. **Ids / enabled set** — `GET /zen/v1/models` (A) or `GET {server}/api/config`
   (B). `/zen/v1/models` is ungated and returns a standard OpenAI list.
2. **Capabilities / windows / free flag** — the capability catalog
   `https://models.opencode.ai/api.json` (models.dev shape; provider `opencode`);
   ~5.3 MB, fetched compressed and cached.

Free detection is **`cost.input == 0 && cost.output == 0`**, not name matching —
`big-pickle` has no `free` in its id yet is free.

Capabilities land on `ModelCapabilities`; the windows land on the model entry's
capacity fields (`context_window_tokens`, `max_output_tokens`):

| target | catalog source |
| --- | --- |
| `ModelCapabilities::vision` | `modalities.input` ∋ `"image"` |
| `ModelCapabilities::video` | `modalities.input` ∋ `"video"` |
| `ModelCapabilities::audio` | `modalities.input` ∋ `"audio"` |
| `ModelCapabilities::files` | `attachment` |
| `ModelCapabilities::tools` | `tool_call` |
| `ModelCapabilities::thinking` | `reasoning` |
| `ModelCapabilities::structured_output` | `structured_output` |
| context window | `limit.context` |
| max output | `limit.output` |

The catalog also supplies `reasoning_options[].values` (the thinking-budget
levels), `temperature: bool` (drives the existing `drop_temperature` quirk), and
`interleaved.field: "reasoning_content"`. No active capability probing is needed;
unknown/new ids fall back to conservative defaults. The catalog carries no
protocol field, so A always uses `chat/completions`.

## Error mapping

The gateway's own error shapes must map onto Zroutery's failure handling so
failover behaves:

| Upstream answer | Meaning | Handling |
| --- | --- | --- |
| `403 FreeTierError` | the shape gate rejected the request | deterministic — every attempt would fail; surface as an upstream error, **do not** rotate |
| `429 FreeUsageLimitError` | per-IP free-tier rate limit | **retryable** — fail over to another candidate / back off |
| `401 ModelError` | the model is not served for this account/tier | **terminal for the model** — drop it from the route, do not treat as a gateway fault |
| `400 "Endpoint is unavailable"` | transient provider-side failure | **retryable** — fail over |
| SSE **error** frame mid-stream | failure after headers were sent | surface to the client as an error |

Individual free models also come and go (`ling-3.0-flash-fin-free` returned a
reproducible `400` while its neighbours answered `200`): a single model's 4xx is
not evidence the provider is down, so it must not cool the whole provider.

## Reuse map

| Need | Reused |
| --- | --- |
| OpenAI wire format + SSE | `protocol/openai.rs`, `protocol::{encode_request, decode_response, stream_parser, encode_response}` |
| `reasoning_content` → thinking block | `protocol/openai.rs` (already decodes it) |
| `stream_options.include_usage` | `ProviderQuirks::stream_usage` (default on) |
| Outbound header assembly | `upstream::build_headers_for_identity` (add one `OpenCodeCli` arm after the profile tier; feed it `ClientIdentity.session`) |
| Outbound body encode | `upstream::encode_for_mode` (add the shaping tail) |
| HTTP transport | `upstream::{send_with_identity, stream_with_identity}` |
| Identity/profile + passthrough | `config::{ProviderClientProfile, ClientIdentity, resolve_client_identity}` |
| Per-provider wire deviations | `protocol::ProviderQuirks` (`stream_usage`, `drop_temperature`) |
| Model listing / "Fetch models" | `upstream::list_models`, `ProviderConfig::{chat_url, models_url}` |
| OAuth token refresh discipline (B) | the pattern in `account/adapters/newapi.rs` (single-flight refresh, generation guard, rotating credential) |
| Registry / routing | `registry`, `router` (a provider is data + a `ModelEntry` set) |

## Deliberately not implemented

- **Auto Mode classifier and election probes for these providers.** The
  classifier wants a deterministic `<block>…</block>` verdict; the gate forces an
  agent-shaped streaming request, and the classifier is fail-closed, so a
  misbehaving free model would over-block the user. The election probe is a
  non-streaming call, which the gate's forced `stream: true` cannot satisfy.
  Enforcement sits at the two planning seams: `Router::plan_classifier`
  (`router.rs`) drops any candidate whose provider is an `OPENCODE-*` kind (an
  opencode-only deployment then has an empty classifier pool, which
  `plan_classifier` already reports as an error), and the election's per-class
  candidate set applies the same filter. `OPENCODE-*` providers serve **main
  traffic only**.
- **The `opencode-go` tier** (`/zen/go`) — a separate lane, not wired here.
- **Native `responses`/`messages` protocols** — A uses `chat/completions` for every
  model.
- **Importing a token from a locally-installed OpenCode client** (matching
  DaatLocus's Codex import). B logs in with its own device-code flow; reading a
  local `auth.json` is a possible later convenience, not a dependency.

## Tests (planned)

- Unit: identifier shape (`^ses_[0-9a-f]{12}[0-9A-Za-z]{14}$`) and no sequential
  run; canonicalisation is idempotent; locked header set with a session; tool
  injection for the 0/1/many client-tool cases and the `tool_choice: "none"`
  branch; collapse folds each `StreamEvent` variant; a mid-stream error frame
  becomes an error, not an empty success.
- Integration: an in-process fake gateway (axum, ephemeral port, mirroring the
  captured SSE) asserting the shaped request and the collapsed response, in the
  style of the NewAPI fake panel.
- Live (`#[ignore]`, opt-in): one anonymous smoke against `/zen/v1` and one
  device-code smoke against the Console, both asserting a non-empty stream.
- Pair with `scripts/orch_docs_test.py` for the DAG records.

## Open items

1. Does the **authenticated Console** tier also require the free-tier shape gate?
   If not, side queries could run over B unshaped; if it does, B stays main-only.
   Test: an authenticated request with no tools and `stream: false`.
2. Does the gateway accept `tool_choice: "none"`? If it rejects or ignores it, a
   pure-chat client on the free tier can lose its answer to an injected
   placeholder call; the response-side filter still strips the call.
3. Whether to add the local `auth.json` import as a convenience login for B.
