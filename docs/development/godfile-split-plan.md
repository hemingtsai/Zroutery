# Splitting the god-files

Baseline: `dev` @ `5566915` plus the Phase 1 takeover/attribution work.

## What "god-file" means here

A file that grew by accretion: several unrelated responsibilities, no module
boundary between them, and a test block large enough to dominate the source it
tests. Measured by physical lines in the working tree.

| File | Lines | Inline tests | Author section markers? |
|---|---|---|---|
| `agent_takeover.rs` | 5028 | 17 banners, tests 82 | **yes** — 17 |
| `server/pipeline.rs` | 3900 | 249 (6%) | **no — none at all** |
| `account/adapters/newapi.rs` | 3265 | 1632 (50%) | yes (`// ── … ──`) |
| `config.rs` | 2749 | 600 (22%) + a second `mod` at 1457 | **no — none at all** |
| `router.rs` | 2655 | 976 (37%) | no |
| `src-tauri/src/state.rs` | 2700 | 1183 (44%) | no |
| `server/mod.rs` | 2587 | 462 (18%) | one (`// ------- handlers -------`) |
| `protocol/openai.rs` | 2854 | 1032 (36%) | **yes** — 5 |
| `migration.rs` | 2547 | 1418 (56%) | **yes** — 6 |
| `protocol/responses.rs` | 2372 | **0** | **yes** — 5 |
| `policy.rs` | 2134 | 940 (44%) | **yes** — 12 |

`pipeline.rs` and `config.rs` are the worst case precisely because they have no
internal structure to lean on: nothing marks where one concern ends and the next
begins, so any split is a judgement call rather than a mechanical cut.

## Off limits

The user has work in flight under `crates/zroutery-core/src/ml/`,
`crates/zroutery-headless/src/evaluate.rs` and
`docs/development/routing-learning-closure-audit.md`. **None of it is touched by
this plan.** That is also 5 of the 15 largest files in the repo
(`ml/calibration.rs` 3398, `ml/bandit.rs` 2909, `ml/shadow.rs` 2465,
`ml/activation.rs` 2461, `ml/offline_gate.rs` 2281), so this plan deliberately
leaves a large part of the total on the table.

## The house pattern to follow

It already exists, twice, in this repository.

**`account/adapters/newapi.rs` → `newapi/` — the closest precedent**
```rust
// newapi.rs, 95-106
pub mod checkin;
pub mod logs;
pub use checkin::{ CheckinAttempt, CheckinContext, /* … */ };
pub use logs::{ interpret as interpret_log, read_system_signal, /* … */ };
```

**`ir/mod.rs` → `ir/response.rs`**
```rust
pub mod response;
pub use response::{ ResponseStatus, ResponseStore, StoredResponse };
```

So the rule is: **`foo.rs` becomes a barrel** — `pub mod` every child, then
`pub use` whatever must stay reachable at the old path. `lib.rs` keeps its
existing `pub use foo::{…}` blocks untouched, because a `pub use` inside
`foo.rs` re-publishes at the same path. **No public path changes.**

Each child gets a `//!` header in the house voice, which explains *why it was
carved out* — see `server/projection_log.rs:1-34` for the model.

## Ranked by value ÷ risk

### 1. `protocol/responses.rs` — do first
2372 lines, **zero inline tests**, and the author already wrote five section
headers. Only three items cross a module boundary (`StoragePolicy`,
`input_items`, `decode_history`, all used by `server/pipeline.rs`).
Splits into `request_in`, `request_out`, `response`, `stream_in`, `stream_out`
— the same five names `openai.rs` uses, so the two dialects become symmetric.

### 2. `protocol/openai.rs`
Same five headers, 1032 lines of tests that travel with their cluster. Only
`wants_stream_usage` crosses out of `protocol/` (`server/pipeline.rs:48`).

### 3. `server/pipeline.rs` — the biggest prize
3900 lines and **already fully encapsulated**: of 47 top-level items, exactly
one is visible outside (`pub(super) fn handle_chat`, `pipeline.rs:42`, used by
`server/mod.rs:52`). Nothing is at risk from a split; the work is deciding where
the cuts go. Natural clusters, largest first:

| Cluster | Lines | Note |
|---|---|---|
| Lifecycle + SSE | 2509–3649 | `RequestLifecycle` alone is a 747-line `impl` |
| Attempt loops | 1404–2056 | `buffered_chat` 367 + `stream_chat` 245 |
| Response emission | 2058–2507 | headers, identity, policy, terminal kinds |
| Rectifier + vision fallback | 876–1402 | |
| Continuation + budgets | 601–874 | the inline tests cover only this cluster |
| Ingress + planning | 42–419 | `handle_chat` |
| ML ranking | 421–599 | feature-gated, self-delimited |

Also worth recording: `try_rectify_buffered` (1117) and `try_rectify_stream`
(1246) are near-identical, as are `buffered_chat`/`stream_chat`. A split makes
that duplication visible; it does not remove it.

### 4. `agent_takeover.rs`
5028 lines, my file, and it already has 17 section markers. The split is
mechanical: ownership types → store → status machine → adapters →
`takeover_fields` → **TOML/JSONC support (~900 lines, wholly self-contained)**.

### 5. `account/adapters/newapi.rs`
Half the file is tests, and the mock-panel harness (~460 lines) is its own
concern. The `// ── … ──` dividers the author left mark six clusters.

### 6. `config.rs`
Seven clean clusters, but the 600-line inline test module spans all of them
(14 of 31 tests exercise `validate`). Splitting means deciding whether tests
travel with their subject or stay whole against `crate::config::*`.

### 7–10. `router.rs`, `server/mod.rs`, `src-tauri/src/state.rs`, `policy.rs`
Same treatment, later.

## Why this is lower-risk than it looks

Three findings from the audit make it so:

- **`pipeline.rs` has no external surface.** Nothing outside it can break.
- **`responses.rs` has no inline tests.** Splitting moves zero test code.
- **`lib.rs` re-exports by module path**, so a barrel split is invisible to
  every consumer, including 197 test files.

The things that *will* break, and are known in advance:

| Risk | Handling |
|---|---|
| `pipeline.rs:20` `use super::{error_response, AdmissionGuard, AppState}` | every child imports these explicitly |
| `config.rs:1762` calls `crate::server::is_valid_origin` | a **config → server** dependency; leaves it alone |
| `config.rs:20` `pub use crate::protocol::ProviderQuirks` | re-export of a foreign type; the line moves with its subject |
| `router.rs:2646` test calls the private `score_and_sort` | that test moves with the method, or the method becomes `pub(super)` |
| `pipeline.rs:42` `pub(super)` | must stay reachable as `crate::server::pipeline::handle_chat` |
| `src-tauri/src/state.rs` is consumed by 6 sibling files | `pub` there is crate-public; re-exports keep every path |

## Definition of done, per file

1. `cargo fmt --check`
2. `cargo check --workspace --all-targets`
3. `cargo test -p zroutery-core --features ml` — currently 2367 passing, must stay 2367
4. `pnpm --dir ui build`
5. `git diff --stat` shows **no change to any `pub` path** — verified by diffing
   the public surface before and after with a grep of every `pub` item's path.