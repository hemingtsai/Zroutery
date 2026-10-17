# Client Attribution & Agent Takeover — Phase 0 audit and Phase 1 contracts

Baseline: `dev` @ `5566915`, working tree clean.

Every claim below is backed by a `file:line` in this repository at that commit.
Nothing here is inferred from `README.md` or from a module's name.

## 1. Baseline gates

| Gate | Before this work | Note |
|---|---|---|
| `cargo fmt --check` | pass | |
| `cargo check --workspace` | pass | |
| `cargo check --workspace --all-targets` | **fail** | pre-existing: `crates/zroutery-core/tests/e2e.rs:1925` matched `ProviderKind` non-exhaustively after the `opencode-ui-p5` merge added `OpenCodeZenFree` / `OpenCodeConsole`. Fixed in this work (see §8). |

## 2. What `agent_takeover` actually is

A real library (`crates/zroutery-core/src/agent_takeover.rs`, 4013 lines) with
**zero production callers**.

Genuinely implemented and covered by `crates/zroutery-core/tests/agent_takeover_test.rs`:

- `TakeoverStore` with an optional durable manifest (`TakeoverStore::open`,
  `:403`), refusing a corrupt manifest rather than reading it as empty (`:296`).
- `OwnershipManifest` (`:283`) carrying `field_snapshots`, `absent_fields`,
  `last_applied` and a `generation` counter.
- `absent_fields` (`:298`) — a field that did not exist before adoption is
  **deleted** on release, not restored to `""`/`null`.
- Compare-and-swap on release via `generation` plus a re-read of the durable
  manifest (`release_refuses_when_another_process_moved_the_manifest`).
- `plan_adopt` / `confirm_adopt` (`:965`, `:1015`) bound by
  `TakeoverProposal::fingerprint` (`:229`), so a proposal held open while the
  file moves underneath is refused.
- `write_config_atomic`, with permission preservation.

Gaps against the task's B1/B2 requirements:

- `OwnershipState` has **four** states (`Verified`, `Adopted`, `Releasing`,
  `Released`, `:30-40`). The task's state model needs nine
  (`unsupported` … `error`). There is no `active`, `conflict`, `degraded`,
  `not_detected`, `available` or `ready` — so a caller cannot distinguish
  "written to disk" from "the client actually reloaded it".
- No verification step exists at all. Nothing ever checks that the client
  process picked the config up.

## 3. Adapters as they stand

| Adapter | Config path | Verdict |
|---|---|---|
| `ClaudeAdapter` (`:1282`) | `~/.claude.json` | **Wrong file.** `~/.claude.json` is the state file Claude Code writes for itself (session, MCP servers, per-project trust, `/config` writes) and is **not** on its settings precedence stack. The `env` block lives in `~/.claude/settings.json`. Writing here succeeds and changes nothing. |
| `CodexAdapter` (`:1323`) | `$CODEX_HOME/config.toml` | Path correct; honours `CODEX_HOME`. |
| `GeminiAdapter` (`:1373`) | `$GEMINI_CLI_HOME/.gemini/settings.json` | To be removed from the takeover set (B6). The Gemini *protocol* code is separate and stays. |
| `OpenCodeAdapter` | **does not exist** | `crates/zroutery-core/src/opencode/` is Zen/Console **upstream provider** code (`console.rs`, `oauth.rs`), unrelated to client takeover. |

Adapters take a caller-supplied `&[ManagedField]` (`AgentAdapter` trait, `:1243`).
**There is no function that says which fields a takeover should manage** — the
caller invents the paths. That is the gap `takeover_fields` closes.

## 4. Production wiring — the honest answer

`src-tauri/src/lib.rs:47-93` registers **34** Tauri commands. None is
takeover-shaped. A case-insensitive grep for `takeover|AgentType|AgentAdapter|agent_`
across `src-tauri/` returns 4 hits, all incidental Claude Code *env var names*
in `ccswitch.rs`.

UI: six routes (`ui/src/App.tsx:18-27`), none for takeover.
`ui/src/i18n.tsx` has **no** key containing `agent`, `takeover` or `gemini`.

The repository records this itself: `README.md:132` — *"目前只有库和测试在调用"*.

## 5. Stats and attribution as they stand

`RequestRecord` (`crates/zroutery-core/src/stats.rs:24-71`) has **no client
field of any kind**. Grep for `per_client|client_totals|by_client` across the
repo: no matches.

`Stats` (`stats.rs:132`) aggregates exactly two dimensions: `per_model`
(`:151`) and `per_kind` (`:152`). No `per_client`, no `per_provider`.

Client identity *is* computed on every request —
`resolve_client_identity` (`config.rs:450`) called from `pipeline.rs:2201` — and
is then **discarded**. `RouteDecision.client_id` exists (`policy.rs:1145`) but is
hardcoded `None` at the only production construction site (`router.rs:610`).

So the missing piece is not detection logic; it is plumbing a fingerprint from
the request into the record and from the record into an aggregate.

### Arithmetic facts that constrain the new aggregate

- `Usage::input_tokens` is **cache-inclusive**; `cache_read`/`cache_write` are
  **subsets**, not additions (`ir/mod.rs:661-716`). An aggregate must not add
  them again.
- `CostTotals` is a `BTreeMap<String, f64>` keyed by currency
  (`billing.rs:124`) and must stay split. There is no conversion anywhere.
- `cache_write_tokens` is currently aggregated **nowhere** (`stats.rs:217-220`
  adds input/output/reasoning/cache_read only).
- `ModelTotals.cached_tokens` is cache-**read** only, under a name that does not
  say so.
- `attempts: u32` counts plan-candidates, not upstream sends — the rectifier
  cascade makes those differ.

## 6. Serialization constraints

- `RequestRecord.kind` is `#[serde(default)]` (`stats.rs:33`); `Usage`'s five
  fields are **not**. A new client field must be `#[serde(default)]` so records
  written before it exist still load (D5, F2).
- `Stats` is `#[derive(Debug)]` only and is never persisted — it is a bounded
  in-memory ring buffer (`stats.rs:1-4`, `limit` from `config.server.log_limit`,
  default 500). It must not be described as history.

## 7. Dependency DAG

```text
Phase 0  audit + shared interface freeze
            |
            +--> Track A  adapters (Claude settings.json, Codex TOML, OpenCode)
            |              needs: takeover_fields(), AgentType::OpenCode
            |
            +--> Track B  client attribution + per_client aggregation
            |              needs: ClientFingerprint, RequestRecord.client
            |
            +--> Track C  Tauri commands + UI
                           needs: both tracks' DTOs frozen
                                    |
Phase 4  real-client end-to-end acceptance
```

The two Phase 1 contracts are genuinely independent: A owns
`agent_takeover.rs`, B owns `stats.rs` + a new `client.rs`. They meet only at
`RequestRecord.client`, so that one field is the interface to freeze first.

## 8. File ownership for parallel work

| Owner | Files |
|---|---|
| Takeover core | `agent_takeover.rs`, `agent_takeover_test.rs` |
| Client adapters | adapter code + their fixtures (inside `agent_takeover.rs`; see note) |
| Attribution | new `client.rs`, `stats.rs`, `ir/mod.rs`, `client_test.rs` |
| UI integration | `src-tauri/src/{lib,commands,state}.rs`, `ui/src/{api.ts,i18n.tsx,App.tsx,pages/}` |

Note: all three adapters live in the single `agent_takeover.rs`. Splitting them
into `adapters/` is a prerequisite for two agents editing that file
concurrently, or the adapter work stays serial.

## 9. Changes made in this phase

### Phase 0 — audit

1. `crates/zroutery-core/tests/e2e.rs` — made the `ProviderKind` match in
   `the_sub2api_preset_reads_a_relay_of_either_dialect` exhaustive. The fixture
   (`config_for`, `e2e.rs:499`) only builds `OpenAICompatible` and `Anthropic`
   providers, so the new arm cannot be reached today; it is written out rather
   than folded into a wildcard so that adding a provider kind fails the test
   instead of leaving the preset's credential-header claim unverified.
2. This document.

### Phase 1 — the two frozen contracts

**Contract A — takeover status.** `crates/zroutery-core/src/agent_takeover.rs`:

- `TakeoverStatus` — the nine-state surface machine (`unsupported`,
  `not_detected`, `available`, `ready`, `active`, `conflict`, `degraded`,
  `released`, `error`), derived by `TakeoverStore::inspect` from the manifest,
  the live config on disk, and a caller-supplied `VerificationReport`.
  `OwnershipState` is untouched and still answers "what may this store do next";
  the two are never collapsed.
- `VerificationReport` with a **three-valued** `request_confirmed`. `None`
  ("nobody looked") must not read the same as `Some(false)` ("somebody looked and
  it failed"), or an environment that cannot observe clients could claim
  `Active`.
- `holds_ownership()` covers `active`, `conflict` **and** `degraded`, so the
  release path stays reachable in exactly the states where a user needs it.
- `takeover_fields(agent, &GatewayTarget)` — the function that says which fields
  a takeover manages. It did not exist before; callers invented the paths.
- `OpenCodeAdapter`, plus `XDG_CONFIG_HOME` / `OPENCODE_CONFIG` handling.
- `ClaudeAdapter` moved from `~/.claude.json` to `~/.claude/settings.json`, with
  `CLAUDE_CONFIG_DIR` support.
- `GeminiAdapter` kept but deprecated and removed from `adapter_for`, so an older
  manifest naming it can still be released. The Gemini *protocol* code is
  untouched.

**Contract B — client attribution.** `crates/zroutery-core/src/client.rs` (new):

- `identify(headers, binding)` — one ladder, shared by every ingress: credential
  binding → `x-zroutery-client` → client header pair → `User-Agent` prefix →
  runtime family → unknown.
- `ClientFamily`, `AttributionSource`, `Confidence`, `ClientFingerprint`,
  `ClientId` (sanitised, length-capped, separator-collapsing), `ClientRegistry`
  (bounded).
- `RuleProvenance` on every rule — `Observed` / `Documented` / `Inferred` — so
  "which clients can we recognise, and on what basis" has one answer.
- `RequestRecord.client` (`#[serde(default)]`), `ClientTotals`, `StatsSummary::per_client`,
  `AttributionSummary`, wired through `RequestLifecycle::new` and `reject`.

**Three pre-existing defects these contracts uncovered**, each found by a fixture
that asserted a negative rather than re-asserting the implementation:

| Defect | Location | Why it mattered |
|---|---|---|
| `conflicts()` iterated `field_snapshots`, not `managed_fields` | `agent_takeover.rs` `conflicts` | A field Zroutery **created** has no snapshot, so it was never conflict-checked. A user editing a base URL Zroutery had just added was reported as a clean, verified takeover. |
| `apply_resolution` kept a path in `absent_fields` after giving it a restore value | `agent_takeover.rs` `apply_resolution` | `restore_fields` checks `absent_fields` first, so `KeepExternal` **deleted** the user's edit — for exactly the fields a takeover creates. Silent data loss on release. |
| Four fixtures wrote `~/.claude.json` and read `~/.claude/settings.json` | `agent_takeover.rs` tests | They were green because the adapter was reading a file nobody wrote. `apply_patch_refuses_stale_snapshot` and two others asserted `unwrap_err()` and were getting `Ok`. |

### Phase 1b — official-documentation verification

Every client-format assumption was checked against vendor documentation and, for
Claude Code, against a locally installed binary. **Four assumptions were wrong
and four were right for the wrong reason**; the code was corrected for all of
them.

#### Verified correct

| Assumption | Verdict |
|---|---|
| `~/.claude/settings.json` is Claude Code's user settings file | **VERIFIED** — and `CLAUDE_CONFIG_DIR` genuinely relocates it |
| `env.ANTHROPIC_BASE_URL` / `env.ANTHROPIC_AUTH_TOKEN` redirect the client's own traffic | **VERIFIED** — and the docs explicitly say *"If you weren't told which kind, use `ANTHROPIC_AUTH_TOKEN`"*, which is the field this adapter writes |
| Not writing `ANTHROPIC_DEFAULT_*_MODEL` | **VERIFIED as the right call** — they are alias pins, and writing them would move a routing decision into a file the manifest then owns |
| `~/.claude.json` is not a settings file | **VERIFIED** — it is sign-in session, MCP, per-project trust and `/config` state. (Nuance: a bounded "Global config" key scope is read from it, but it is not on the settings precedence stack and must not be written.) |
| `$CODEX_HOME/config.toml`, default `~/.codex` on all three platforms | **VERIFIED** — and the code confirms no per-platform branching |
| `model_provider` is the selector; a provider table without it is dead config | **VERIFIED** |
| Codex `model` is a bare id, not `provider/model`; there is no per-provider model table | **VERIFIED** — the whole repo contains no model-id slash splitting |
| OpenCode `model` is `provider/model` and the prefix is stripped before sending | **VERIFIED** — the schema *requires* the slash |
| `apiKey` becomes `Authorization: Bearer <key>` | **VERIFIED** for `@ai-sdk/openai-compatible` |
| `$XDG_CONFIG_HOME/opencode/opencode.json` | **VERIFIED** |
| `OPENCODE_CONFIG` names a single file | **VERIFIED** — `path.resolve`d straight to the file loader; pointing it at a directory does nothing |
| `zroutery` is a safe provider id | **VERIFIED** — not among the 230 ids in `models.dev/api.json` |

#### Corrected

| Was | Actually | Fix |
|---|---|---|
| "`experimental_bearer_token` may not be supported by the installed Codex; a takeover writing it should be reported `degraded`" | **VERIFIED present, documented, and runtime-wired** (`bearer_auth_for_provider` emits `Authorization: Bearer <token>`). *Discouraged* upstream, not deprecated. `env_key` wins if both are set. | Comment rewritten to record the verified position and the `env_key` precedence caveat. Still the right field: it is the only one of the three that takes a literal token, and `env_key`/`auth` cannot be restored from a manifest. |
| "OpenCode's JSONC refusal covers a `.jsonc` file" | OpenCode parses **every** config as JSONC — the extension is cosmetic, so `opencode.json` carries comments too | Split into `refuse_before_parse` (text-level: `.jsonc` sibling, and comments *anywhere* in a `.json`) and `refuse_after_parse` (shape-level: the v2 `providers` key). The text check now runs **before** parsing, because `serde_json` otherwise reports "invalid JSON: key must be a string", which tells the user their config is broken when a tool merely declined. |
| "A project `opencode.json` can shadow the global config" | There are **two**: `<project>/opencode.json` *and* `<project>/.opencode/opencode.json`, merged in that order, so the dotted directory is the effective source | `project_shadow` checks both and reports the **stronger** one. |
| "OpenCode identifies as `opencode/<version>`" | It is `opencode/<channel>/<version>/<name>`, **and only on built-in provider paths**. A custom `openai-compatible` provider — precisely what a takeover creates — sends `ai-sdk/openai-compatible/…` instead. | Added an `x-opencode-client` + `x-opencode-session` header-pair rule at rung 3, which sits on the shared request path every provider uses. A `User-Agent` rule alone would have failed exactly when the feature is in use. |
| "Claude Code sends `claude-cli/<version>`" | True, but it also sends a **separate** `claude-code/<version>` for metadata/token requests | Rule already covered both prefixes; pinned by a test. |

#### Also established, and relevant to Phase 2

- **Codex `deny_unknown_fields`.** `ModelProviderInfo` rejects unknown keys as a
  hard parse error, so the field set must stay exact. Confirmed absent:
  `api_key`, `token`, `key`, `headers` (it is `http_headers`), any per-provider
  `models` table.
- **Codex ignores `model_provider`/`model_providers` in project-local
  `.codex/config.toml`** and prints a startup warning. The adapter writes the
  user-level file, and a test now pins that.
- **Codex `name` must be non-empty** or the whole `config.toml` fails to load.
  Pinned by a test.
- **Codex `[profiles]` and `profile =` are no longer supported** (0.134.0+);
  profiles are separate files now. The adapter writes neither, correctly.
- **Codex TOML comments survive** — it uses `toml_edit` and preserves decor, and
  only rewrites the file when a value actually changes. Zroutery's own TOML
  subset writer does **not** promise this and is a Phase 2 risk.
- **All three clients hot-reload config**, so the UI must not say "restart
  required". Claude Code's documented exceptions are that *removing* an `env`
  entry needs a relaunch and that a malformed file is only reported at the next
  start — which is the real reason to prompt for a new session.
- **OpenCode V1 `provider` is marked `deprecated: true`** in the live schema, and
  `@opencode/cli@2.0.30` is the current major. V1-shaped writes still work, but
  this is the single biggest open design question for Phase 2.
- **Claude Code settings are strict JSON** — a trailing comma or `//` is a hard
  syntax error that disables the whole file. Zroutery emits strict JSON.

## 10. Known limitations at this baseline

### Blocking, and not caused by this work

`ml/resource_buckets.rs` was mid-edit in the working tree and failed to compile
for part of this phase (a `&self` borrow returned where a `buckets` borrow was
owed, plus two `E0716`s). It was **reproduced with every change in this branch
removed**, so it was not touched here. It now compiles and the full gate is
green — the work in progress finished alongside this one.

`crates/zroutery-core/tests/e2e.rs:1925` — the non-exhaustive `ProviderKind`
match — **was fixed here** (§9). It is listed so the two failures are not
conflated.

One side effect to disclose: `cargo fmt --all` was run during this phase, and
the project enforces `cargo fmt --check`, so it also reformatted the in-flight
`ml/` files that were unformatted at the time. That is whitespace only and no
semantic change was made to anyone else's work, but those files are not as they
were before this phase started.

### What was actually run

| Gate | Result |
|---|---|
| `cargo fmt --check` | **pass** — whole workspace clean |
| `cargo check --workspace --all-targets` | **pass** |
| `cargo test -p zroutery-core --features ml` | **2367 passed, 0 failed** across 69 targets |
| `pnpm --dir ui build` | **pass** |
| `cargo clippy --workspace --all-targets` | one pre-existing warning, no errors |

New tests added by this phase: **73** — 14 status-machine fixtures
(`agent_takeover_status_test.rs`), 42 attribution fixtures
(`client_attribution_test.rs`), and 17 adapter / field-set fixtures inside
`agent_takeover.rs` (82 in-crate tests there in total, up from 60).

`migration::tests::verify_endpoint_unreachable_errors` is environment-flaky:
it dials `192.0.2.1:1` (TEST-NET-1) and asserts the error says "unreachable".
It fails or passes depending on how this machine's network stack reports an
unroutable address. It is unrelated — `migration.rs` is untouched — and it
passed on the final run.

### Behavioural limits

- No credential binding is wired into the gateway yet.
  `identify()` takes `Option<&CredentialBinding>` and the pipeline passes `None`,
  so nothing can currently reach `Confidence::Strong`. The rung exists and is
  tested; the authentication side is Phase 3.
- No takeover command, UI page or i18n key exists yet. `agent_takeover` is still
  a library with no production caller.
- Every adapter test uses a temp directory. **No test has run against a real
  Claude Code, Codex or OpenCode install.** The configuration *schemas* are now
  verified against vendor documentation, but "the client reads these fields" is
  a separate claim and remains unproven. Expect the first real-client run to
  surface gaps the docs do not cover — most likely around Codex's
  `experimental_bearer_token` when a user also has an `env_key` set.
- Zroutery's TOML subset writer does not preserve comments the way Codex's own
  `toml_edit` path does. A takeover that rewrites `config.toml` may drop
  comments. Untested against a real Codex; a Phase 2 fixture.
- The OpenCode adapter writes the **V1** provider shape (`provider`/`npm`/
  `options`). That still works and the adapter refuses V2 configs outright, but
  V1 is marked `deprecated` in the live schema and OpenCode 2 is the current
  major. Choosing V2 instead is a Phase 2 decision with a real trade-off:
  V2 breaks OpenCode 1 users, V1 is on a deprecated path.
- `Usage` still carries no provenance field (D6), and the Gemini
  `reasoning_tokens` / `output_tokens` disjointness noted in §5 is still
  unfixed.
- `per_provider` aggregation still does not exist.
- `clippy --workspace --all-targets` reports one pre-existing warning
  (`buffered_chat` has 11 parameters; unchanged by this work).