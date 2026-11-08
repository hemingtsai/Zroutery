# Upgrade execution

Implements the agreed 2026-10-30 master plan. The canonical plan is the
OpenChamber Markdown file named in [upgrade-contracts.md](upgrade-contracts.md);
only its coarse checkboxes are changed when the corresponding gates close.
This record does not replace existing node evidence or claim a release.

## Status overview — 2026-11-01

A read-only source/status reconciliation day. The canonical plan's coarse
checkboxes are unchanged by this overview: **M0 is done; M1 through M5 are
partial; M6's first-batch exit is blocked** by the open M1/M2 gates. M3
(supplier expansion), M4 (tray/themes/performance) and M5 (reliability/ML) are
subsequent batches, not prerequisites of the first Windows batch.

Git state at the reconciliation: branch `dev`, HEAD and `origin/dev` both at
`4075a54`; the index is empty and a large pre-existing unstaged and untracked
working set is present, including the uncommitted history-import slices below.
That set belongs to the user and other sessions, its composition moves as
parallel work lands, and no file count is recorded here. The M0 baseline's
ownership, no-reset and no-rewrite rules still apply.

The same day's detailed reconciliation — P0 items, history import, the currency
qualification, client evidence boundaries and the commit-contract finding — is
the **Source review reconciliation — 2026-11-01 (E-204)** section at the end of
this file, together with the synchronized plan/Todo/node state.

## M0 baseline

- Started on 2026-10-30, Windows, `E:\Zroutery`, branch `dev`,
  `a2fa14557af4a885d639b7b8ce8be1023ce24e71`.
- Existing staged work: durable-usage/chart additions, desktop command/ACL
  registration and Traditional Chinese/error-boundary updates. Existing
  unstaged/untracked work: Codex tools/Auto rectifier/protocol changes, its tests
  and license/documentation, associated dependencies and routing controls.
  **All are pre-existing work, not authored or submitted by this execution.**
  No reset, unstaging, history rewrite or bulk commit is permitted.
- Binary staged/unstaged diff snapshots are saved outside the checkout at
  `C:\Users\InfinityNeko\AppData\Local\Temp\opencode\zroutery-upgrade-baseline-{index,working}.diff`.
- `pnpm --dir ui test`: 3 files / 13 tests passed, 2026-10-30 16:31 local.
- `uv run --python 3.12 --no-project python -B scripts/toolchain_gate.py`:
  **DRIFT**; local rustc 1.99.0 (`b940084d7`), CI 1.99.1 (`58cf8f9c3`).
  Install 1.99.1 independently with clippy/rustfmt; never kill another build
  holding the shared stable toolchain. MSRV remains 1.90, independently tested.
- Rust 1.99.1/clippy/rustfmt are installed; the rustup command's final failure
  was its independent self-update cleanup, not a failed compiler installation.
  `cargo +1.99.1 --version` succeeds. A repeated online toolchain-gate fetch
  timed out and is **INCONCLUSIVE**, not reported as a pass.
- `cargo +1.99.1 test -p zroutery-core --all-features --lib` through the quiet
  test gate completed without failure before the first code slice was applied.
- Hardware: Intel Xeon E5-2666 v3, 10 cores / 20 logical processors; throughput
  and UI timing measurements have not yet been run and are not inferred.
- User requested development standby on this execution. The existing process
  at 8788 runs from `%LOCALAPPDATA%\Zroutery\standby-8788\zroutery-headless.exe`,
  outside Cargo outputs. `/health` is OK and authenticated `/v1/models` offers
  the current model (47 catalogue entries). `.opencode/opencode.jsonc` overrides
  only this project's gateway URL to 8788; global credentials/models stay intact.
  No additional inference was sent to establish connectivity.
- Existing repo plan copy is the earlier unapproved draft. Its contents are
  retained as historical provenance, with an explicit pointer to the agreed
  source and these contracts to prevent two execution authorities.

## Review decisions

- Existing dirty work is treated as user/other-session work. Continue in this
  checkout as requested, compose small reviewed additions around it, and do not
  infer permission to submit unrelated hunks.
- Six task doors preserve the established route IDs where possible and use
  sub-areas for legacy links. This avoids needlessly changing every backend
  deep-link name while meeting the approved information architecture.
- The opening `spend.json` balance is a baseline, not replayed history. This
  preserves existing budget totals without fabricating per-request attribution.
- Native client capability/menu observations and genuine upstream evidence are
  external gates. Mock fixtures can validate implementation, not close those
  evidence requirements.

## Delivery state

Snapshot note, added 2026-11-01: the table below is a historical rolling delivery
record: its original rows date from 2026-10-30/31, while the history row was
updated through the early 2026-11-01 import slices. It is not a current Git or
aggregate acceptance matrix. Current milestone state is in the overview above
and the source-review reconciliation at the end of this file.

| Slice | Code / docs | Local verification | Submitted | Released |
|---|---|---|---|---|
| M0 | Task architecture, authority/versions, failure/migration and acceptance contracts frozen; baseline recorded | Rust 1.99.1 confirmed, initial UI tests; online recheck timed out, performance not yet measured | No | No |
| M2a + P-ACCOUNT | Versioned SQLite settlement/attempt facts, opening ledger, binding publication journal/CAS, compensation and identity review implemented; recovery hardening continues | 1519 Core unit tests + 6 durable-accounting cases passed before later protocol edits; 3 desktop binding cases and 2 binding-CAS cases passed | No | No |
| P-PRICE / P-EFFORT + model/routing | Readable names, reasoning request enum/exact filtering and cross-relay effort isolation implemented; effective pricing not yet implemented | Core/desktop all-target check passed; legacy-effort test expectations updated, full behavior revalidation pending | No | No |
| Client four-class setup | Claude native picker/four aliases, explicit model/env default; Codex owned catalogue; OpenCode common capability/variants; preview/apply UI implemented | Native client menus/request observations and full restore regression pending | No | No |
| History/query/charts | Imported-history contract, schema v5 and read-only cc-switch and Claude Code adapters are implemented in Core, with desktop commands, an Activity confirmation flow, and history sources found or remembered so the picker is no longer the only way in; the Codex and OpenCode adapters, chart/export parity and cross-source duplicate accounting are not implemented yet | 14 focused Core history-import tests, 8 desktop source-discovery tests, the workspace `--lib` suites (Core 1601, desktop 267), clippy/fmt, UI build and 41 tests, 30 interaction cases, layout/overflow and the size budget pass | No | No |
| M3 / M4 / M5 / M6 | Not implemented by this execution yet | External gates remain explicit | No | No |

## Steering and bounded fixes

- Operator reported `ps.air-outer.com/login` redirecting to
  `agentrouter.org/console/token`. The shared panel root now maps only the
  exact confirmed HTTPS relay to that panel, with explicit account override
  winning. Capture verifies the observed origin before reading credentials;
  other hosts, lookalikes and downgrades pause. Browser identity evidence is
  the accepted token-authenticated `/api/user/self` payload, not localStorage
  hints. Twelve capture cases passed before the latest evidence-payload edit;
  revalidate the page script and metadata publication together.
- Claude 2.1.281 rejected mid-conversation `role: system`. Consulted current
  official gateway compatibility docs: these entries are native for recent
  models and must retain their position/cache markers. IR gained `Role::System`
  and a same-protocol Anthropic body; text-only cross-protocol handling is
  explicit. No auto-approval fallback was introduced.
- Claude menu showed `tierflow_pro` while its default was a class. Read-only
  configuration checks found stale family mappings and later a still-present
  `env.ANTHROPIC_MODEL=tierflow_pro[1M]`, which outranks settings.model.
  Native four-class synchronization now handles aliases and, only with an
  explicit default choice, that model environment override. These settings
  changed during operator testing; this execution did not overwrite them by
  shell or discard the operator's edits. Existing sessions may retain selection.
- Operator requested more tolerant 503 recovery. Added six bounded availability
  replays (1/2/4/8/15/20 seconds, max upstream floor 30 seconds, pipeline total
  window 120 seconds). A configured fallback remains preferred; in-band or
  delivered output/tool actions are never transparently replayed. Pipeline owns
  each replay's distinct attempt ID; staged Codex availability has its existing
  bounded retry budget. Dedicated recovery and catalogue tests are running.
- Current UI: six task doors with legacy sub-area deep links, readable model
  titles, usage-owned budget editor, page lazy chunks, visibility/overlap-aware
  activity polling, refresh/save epoch protection and toast timer cleanup.
  `pnpm --dir ui test`: 4 files / 15 tests passed at 18:17 local; latest UI
   build/command-ACL checks are recorded separately from native menu evidence.

- Operator requested global standby handoff: backed up Claude/OpenCode settings
  under `%LOCALAPPDATA%\Temp\opencode\zroutery-standby-global-20261030-1925`;
  changed only Claude's gateway/model-related fields and OpenCode's Zroutery URL.
  Removed `ANTHROPIC_MODEL` and the legacy subagent pin, repaired four family
  display names and set the native picker to replace built-in/gateway rows.
  Preserved the operator's subsequent `fast-class` selection. Native Claude
  2.1.281 menu was observed in an isolated owned PTY: all four labels, no
  `tierflow_pro` or environment-override warning. Safe mode avoided hooks/plugins;
  no model prompt, permission bypass or paid connectivity probe was submitted.
  A pre-existing Claude process still needs a normal restart to drop its loaded
  environment; no user process was killed. CLI-on-PATH OpenCode is V1 1.18.31
  and rejects this existing V2 config; the actual OpenChamber V2 2.2.0 was read
  via its authenticated API and confirmed the global URL at 8788.
- Replaced only the development standby via verified Windows console Ctrl-C
  drain (35876 → 28844), with its binary/config/spend and consistent SQLite
  backup at `standby-8788\handoff-20261030-192425`. New independent executable
  SHA256 `0aa1e7bd9708f0f1ea65979a01ffc3ef6fb4296124ed989a065dd8ffb490d861`.
  The shared database was already schema v3, so this handoff did not migrate it.
  `/health` and authenticated status/models succeeded (47 entries, four classes).
  Cargo outputs were rebuilt concurrently afterward; the standby remains the
  named independent image, not whatever binary currently occupies target/debug.
- A health-row name edit used the wrong TS export (`readableModelName`); corrected
  it to existing `modelName`. UI build and 4 files / 15 tests passed at 19:16.
  Rust 1.99.1 now MATCHES current CI hashes. Core all-feature unit tests and
  Claude role/recovery/catalogue/durable-accounting/eligibility suites passed
  before the subsequent managed-environment removal hardening.
- Ordinary 429s now close the default-credential model window at the first
  confirmed rate limit; explicit Keys keep independent gates. Planning/send
  checks honor the window and upstream Retry-After; raw quota does not poison
  the fault circuit. Transport and staged Codex paths no longer replay an
  already rate-limited scope. Three scoped-window integration cases now pass:
  plain/stream direct requests stop after one send and return Retry-After,
  healthy sibling Keys remain usable, and staged Codex does not replay 429.
- Operator's Claude `frontier-class` failures after that handoff had zero
  upstream attempts, while exact OpenCode `agentrouter-gpt-6.1-sol` calls
  completed. The Claude process was launched at 19:07 before environment cleanup
  and still carried the old override; its saved settings now correctly track
  the operator's frontier selection and retain `max`. Found a separate native
  bridge bug: Anthropic effort was filtered unless an artificial token-budget
  mapping existed. Native Anthropic/OpenAI shared effort identifiers now map
  directly into output_config / reasoning_effort / reasoning.effort, preserving
  the requested level and requiring declared target support. Unknown levels and
  genuinely unrepresentable numeric controls remain fail-closed. Unsupported
  reasoning yields non-retryable 422 rather than a misleading transient 503.
  Genuine upstream effort evidence is still a separate P-EFFORT gate.
  `claude_routing_test` verifies eight actual loopback sends over four classes,
  buffered/live SSE, preserving `max` before an attempt cap of one; an effort
  gap gets 422 with `x-should-retry: false` and zero sends. Together with four
  system-role cases, three catalogue cases and the bounded-503 recovery case,
  all 13 targeted test cases passed before the second standby handoff.
  The first new-fixture runs failed at compile time because their test-only
  import named a non-reexported `ApiSurface`; corrected fixtures to deserialize
  the configured surface, then reran the actual tests. No earlier failed run
  is counted as passing. All-target Core clippy initially found that import
  and an item-after-test-module placement; its corrected rerun is separate.
- Corrected Core all-feature/all-target clippy passed with `-D warnings`.
  The second standby Ctrl-C drain timed out without replacement; no force was
  inferred. The operator explicitly authorized interrupting PID 28844 only.
  It was replaced with PID 46416 at 20:02, backed up at
  `standby-8788\handoff-20261030-200215`, SHA256
  `52f199bac180228d06b0e438777d9843a210bbd837855e70e37e7e787739f8f5`.
  Saved desktop declarations were copied with file CAS, retaining port 8788
  and standby ML directory, keeping elections/ML/exploration off. Existing
  account binding versions and durable wallet/history were retained; incomplete
  attempts were not invented as completed or replayed. The new child has a hidden
  independent console without the previous Ctrl-C-ignoring process-group flag.
  Health/status passed; an authenticated `system`-role request to a deliberately
  nonexistent fixture model reached 404 resolution, not the old decoder 400,
  and submitted no upstream inference.
- Wider regression: Codex staging 10 cases, durable accounting 6 and eligibility
  18 passed; lifecycle had 6 failures and later test binaries had not run.
  Five lifecycle fixtures relied on now-forbidden numeric budget halving;
  changed repair fixtures to explicit rejected signatures/media so per-send
  lifecycle coverage stays intact without lowering requested reasoning. A source
  assertion still named the old independent stats/ledger write; it now asserts
  the unique durable settlement and forbids pipeline-side duplicate charges.
  One client status preview expected only two fields and omitted the new native
  picker; updated the exact field expectation. Reruns are recorded separately.
  Frontend now has 5 files / 17 passing tests, including the shared name export.
- Subsequent wider rerun: client ownership/status 19/14 cases and lifecycle 49
  passed. Protocol golden still had two obsolete expectations (normalizing a
  native system string and treating minimal as disabled); corrected them while
  preserving native shapes and none/minimal separation. E2E exposed analogous
  old effort-dropping/budget-halving expectations; updated those fixtures to
  fail closed on unsupported effort and to use rejected signatures for the
  shadow retry A/B comparison. No check is reported green before its rerun.
- Operator surfaced the new Claude no-charge classifier notice. Official docs
  require native `safeguards` / `safeguard_results`, extension headers and tool
  IDs, not a zero-price override. Added native reply/stream preservation, with
  terminal review fields retained through message_stop, request-local safe reply
  headers and no fabricated server verdicts or upstream eligibility. Non-native
  providers cannot be represented as supporting Anthropic server review.
- Operator's standalone classifier requests were recorded as main traffic and
  resolved to Sage. Confirmed the local Claude 2.1.281 static implementation
  defaults monitor temperature to 1, varies caps with stage/budget, and also
  uses the 0..100 severity contract; the previous signatures fixed temperature
  to 0 and caps to 64/4096. Added specific monitor+verdict-contract detection,
  kept embedded safeguards review as main traffic, and kept typed Jev decision
  endpoints out of generative effort filtering. Buffered/live Jev replies adapt
  the binary decision to the requested contract without claiming a calibrated
  severity or approving unknown confidence. Added isolated durable-record and
  native server-review regression fixtures; their verification/deployment is
  still in progress. No user prompt/body or secret was saved for diagnosis.

## Backend optimization follow-up — 2026-10-31

The user assigned the backend-only optimization slice, with API/mock contract
synchronization authorized separately and subagents pinned to AgentRouter GPT
5.6 SOL. Scope, current evidence and frontend handoff are recorded in
[backend-optimization-execution.md](backend-optimization-execution.md) and node
`BACKEND-OPTIMIZATION` (E-192). B1/B3/B4/B5/B6 and the subsequently added B7
backend code landed; T's Rust shell is ready for the frontend panel. The B3
verified image was observed running on the authorized 8788 standby after drain.
This follow-up does not mark the aggregate M1–M6 Steps complete or claim a release.

## Local commit finalization — 2026-10-31

The user requested functional backend commits, followed by one combined commit
for the remaining frontend and development records. Backend commits
`8f36192..46c0f52` contain the frozen backend work and local OpenCode ignore rule.
The earlier delivery table records the implementation session's state at that
time; it is not the current Git submission state.

- Final backend tree: Rust 1.99.1 workspace/all-feature tests passed **3,238**
  cases with **22 existing ignored**; fmt, strict Clippy, Rust 1.90 all-target
  check, **850** Core minimal tests, desktop minimal check, all **40** feature
  combinations, command ACL and loopback headless smoke passed. The first test
  build hit the running desktop executable's Windows file lock; the passing
  rerun used separate final artifacts and shared intermediate dependencies.
- The frontend remainder connects cache totals/charts and native file export,
  retains the MD3/Fluent v2 skin work, and completes the per-client menu drawer.
  Preview/apply retain the selected client, options and fingerprint; changing
  options invalidates preview and late replies cannot populate a reopened drawer.
  The budget add action now shares the input baseline.
- Node **22.23.4**, pnpm **11.22.0**: build and **35** unit tests passed; all
  **26** interaction cases and **10 surfaces / 64 controls** passed. The first
  layout run reproduced the budget row's 18px spread; the corrected rerun
  reports 0px. Python's local HTTP fixture emitted connection-reset traces
  during browser shutdown; every interaction assertion passed.
- Bundle budget passed: **683,039 B JavaScript**, **142,801 B CSS** and
  **189,604 B largest chunk**. Command-surface and orchestration mutation
  self-tests passed. Frontend dark-accent evidence was renumbered to **E-195**
  to resolve the duplicate E-192; backend evidence remains E-192, with the
  frontend references and DAG synchronized.
- The three previously untracked upgrade records are included with this
  finalization, closing their missing references. Public class effort still
  differs from the accepted intersection contract; effective-rate execution,
  full history adapters, daily cost UI/SQL paging and remaining native-client
  acceptance keep their prior unfinished status. No deployment or release is
  implied by these local commits.

## Follow-up fixes — 2026-11-01

- This follow-up starts from `4075a54` (`dev` initially matched `origin/dev`).
  The aggregate M1–M6 checkboxes remain open; local implementation and tests
  do not close native-client, external-accounting, platform or release gates.
- Usage now offers separate **This device / Development standby** views. The
  standby runtime database is opened with SQLite read-only flags and
  `query_only`; schema migration and writes are not performed. A selected path
  is stored in a separate file. Recent rows identify their source. Clear,
  import/export and local accounting/budget status are suppressed in standby
  view. The databases are not summed because there is no cross-instance ID
  deduplication contract. The focused Core test proves read and write refusal.
  UI-28 drives the switch: the standby pane labels its source, offers the
  chosen file, and carries neither the per-client Clear column nor the local
  export/import actions, both of which return with the local view.
- Explicit inference Keys are expanded by ascending numeric `priority` after
  model ranking. A new loopback E2E test reverses ID/declaration order and
  priority, confirming priority 0 is the first send. The UI displays keys in
  that order and supports pointer drag plus keyboard up/down reordering; each
  reorder writes spaced priorities serially without changing key references or
  sending secret fields. UI-27 interaction coverage passed with delayed IPC.
- Skin/theme choices are saved synchronously on selection and initialized
  before React's first render. The UI now calls the native accent command and
  populates CSS variables, refreshes on theme/visibility changes, and retains
  fallback values when native accent is unavailable. This follow-up has UI
  token tests; no native accent claim is added beyond E-192's platform scope.
  UI-29 selects the dark theme and the MD3 skin, checks the accent CSS variable,
  then reloads the real document and re-reads all three values.
- Fluent contrast decision: interactive control boundaries must meet 3:1;
  purely decorative separators may retain the official weak stroke. A separate
  `--control-line` token strengthens control frames while decorative lines
  remain weak. MD3/Fluent popover shapes are implemented. Full six-state browser
  comparison remains open. Takeover proposal/conflict tables now have their own
  horizontal scrollers; the 820x560 forced-width containment check passes.
- Font CSS is loaded by skin (Industrial Noto Serif SC, MD3 Roboto, Fluent
  system Segoe/Yahei). The unused `@fontsource/ibm-plex-sans` dependency was
  removed. UI build/tests pass; current budget is **691,485 B JavaScript** and
  **148,589 B CSS**, within configured limits. Real first-paint transfer has
  not been measured.
- Focused verification: UI build and **41** tests, Core read-only usage test,
  explicit-Key priority E2E, command-surface self-test and orchestration-docs
  self-test pass. Full layout measured **11 surfaces / 70 controls**, including
  the Simple-mode workbench; every control aligned. Overflow measured all **9**
  task areas at 820×560, including the forced-width table containment probe.
  Interaction coverage passes **29/29**: the existing cases plus drag and
  keyboard Key order (UI-27), the separate read-only standby source (UI-28) and
  theme/skin/accent persistence through a real document reload (UI-29).
  The Python fixture reports connection resets while Chromium shuts down; every
  interaction assertion passed. Evidence: **E-196**.
- Read-only Oxionai/401 trace: fresh requests after a completed provider
  removal cannot route to it because the new registry/classifier pool excludes
  it. An in-flight request may retain its pre-removal registry snapshot.
  Separately, a 401 from a still-configured provider using its default
  credential is non-fallbackable; explicit inference-Key 401 is fallbackable.
  The reported re-authentication text is absent from the repository and the
  `Oxionai` provider identity is not established. No credentials were read and
  no upstream request was sent; the incident needs the triggering client and
  provider/config context to confirm.
- Additional uncommitted live-preview work appeared concurrently under
  `src-tauri/src/state/{budget,migration,takeover}.rs`, `src-tauri/examples/`,
  `ui/live-bridge.ts` and `ui/preview.html`. Their behavior was not authored,
  overwritten or cleaned here; `cargo fmt --all` normalized formatting in the
  concurrent Rust additions and in Rust files changed for this follow-up.

## History import, slice 1 — 2026-11-01

Scope of this slice: the Core contract, its storage and the **cc-switch** adapter.
The Claude Code, Codex and OpenCode adapters, the IPC commands, the Activity
import flow and chart parity are not part of it and remain open.

- `history_import` keeps imported facts apart from this instance's own usage.
  Every fact carries its source family, source instance, the source's own entity
  id and its granularity, so a day total can never be read as one request and a
  re-import recognises what it already holds. Absent latency, status or price
  stay absent; a source that reported no price is `unpriced`, never zero.
- Token counting keeps the source's own declaration. cc-switch records an
  `input_token_semantics` value per row (0 legacy, 1 total, 2 fresh); the
  importer converts each into this repository's invariant that cache reads and
  writes are inside `input_tokens`, and legacy cache-inclusive apps (codex,
  gemini, grokbuild) add only their writes, matching cc-switch's own read rule.
  The raw counts stay on the stored fact so the conversion can be re-checked.
- Duplicates are evidence, not identity. cc-switch suppresses a session row when
  a proxy row within ten minutes carries the same app and token counts; the same
  row is kept here as `Duplicate::Suspected` with `duplicate_of` naming its twin,
  and it is excluded from canonical totals while staying visible.
- `usage_daily_rollups` is treated as a replacement for pruned detail: a rollup
  day that still has request detail is skipped and counted as `covered_totals`,
  so detail and total are never added together.
- The source database is opened `SQLITE_OPEN_READ_ONLY` with `query_only` and a
  busy timeout; a source older than schema 13 is refused because its input
  counts have no declared meaning. Reading leaves the file byte-identical.
- Schema v5 adds `imported_fact` keyed by `(source, instance, entity_id)`,
  `imported_revision` with insert/update/delete triggers, and time, day, client,
  provider and model indexes. The store computes an FNV-1a content fingerprint
  so an unchanged row counts as `unchanged` instead of being rewritten, and a
  changed one becomes an `updated` that advances the revision. Validation runs
  before the transaction, so a rejected batch stores nothing.
- Verification: 9 contract/adapter tests and 8 store tests pass, including a
  legacy codex row whose input already contained its cache reads, the ±600 s
  suspected duplicate, a rollup day covered by detail, an unpriced zero cost, a
  late source correction, a clear that advances the revision, and two instances
  of the same source that do not overwrite each other. The full Core suite
  passes with **1596 tests** (1 ignored); `clippy -D warnings` over all targets
  and features and `cargo fmt --all -- --check` are clean. Evidence: **E-197**.
- Not claimed: no real cc-switch database was read (fixtures only); this slice had
  no IPC or UI path (the next slice adds them), and nothing is committed,
  deployed or released. Raising the runtime schema to 5 first made a standby
  database still at schema 4 refused by the read-only opener; E-203 later
  established that schema 4 stays readable, because v5 only added the imported
  tables and the usage side is unchanged. Only layouts older than 4 still need
  their owning build to open them once.

## History import, slice 2 — desktop surface

- Five commands wire the adapter to the dashboard: `pick_usage_history_source`
  (native dialog, path only), `preview_usage_history_import`, `commit_usage_history_import`,
  `get_imported_usage` and `clear_imported_usage`. They are registered in
  `lib.rs`, declared in `build.rs` and granted in `capabilities/default.json`;
  `command_surface_test.py` reports 82 interface commands, 242 registered and an
  exact capability match.
- The Activity page gains one entry point, "Import from another tool…", which
  opens the same drawer grammar as export/import. The drawer shows what would be
  stored (request rows, day totals, suspected duplicates, unpriced rows, days
  covered) with the source's copy label, and writes only when the confirmation
  button is pressed. The report states inserted, updated and already-stored
  rows; the standing row states the imported total and keeps it visibly separate
  from this device's own figures. Clearing asks first, then removes imported
  facts only.
- The import is deliberately not folded into the KPI row or the traffic chart
  yet. Imported history and local usage are different provenances, and a chart
  that added them without a way to tell them apart would be the double-count the
  contract forbids. Per-source filtering and the shared window/unit query are
  the next slice.
- Verification: UI build, **41** unit tests, **30** interaction cases (UI-30 is
  new: preview counts, no write before confirmation, report, standing total,
  a clear that requires its dialog), layout **11 surfaces / 70 controls**,
  overflow **9 pages** at 820×560, and the bundle budget (**699,141 B**
  JavaScript of 700,000, **148,589 B** CSS). Desktop all-target/all-feature
  check, clippy with `-D warnings` and `cargo fmt --check` are clean. Evidence:
  **E-198**.
- Not claimed: the flow is proven against the injected mock bridge, not on
  WebView2 with the real native dialog and a real cc-switch database; the other
  three source adapters, chart/export parity, range selection and instance
  relabelling remain open. The JavaScript budget now has only ~850 B of
  headroom, which the next UI addition will have to earn.

## History import, slice 3 — Claude Code transcripts — 2026-11-01

Scope of this slice: a second source family, and the picker change it needs.
Codex and OpenCode remain open.

- The adapter is chosen by what the chosen path **is**, not by a menu: a
  directory is read as client transcripts, a file has to be a SQLite database a
  known adapter recognises. A wrong guess in the interface would import nothing
  or another tool's history; a property of the path cannot be guessed wrongly.
  `Source` is the dispatch point for preview and commit, and reports its kind so
  the confirmation screen can name the reader.
- `claudecode` reads `projects/**/*.jsonl` to a fixed depth of six (the real
  layout is five), ignores non-JSONL files and does not follow symlinks. Only
  `type == "assistant"` lines carry usage. A message streams in more than once,
  so the file is deduped by `message.id`: a line with `stop_reason` wins over an
  intermediate one, and between equally complete renderings the larger
  `output_tokens` wins. A line with no trailing newline is a write in progress
  and is left for the re-import. An assistant line whose counts are all zero is
  bookkeeping, not a call, and is dropped.
- Tokens follow the Anthropic convention at the source: `input_tokens` is the
  fresh prompt, with cache reads and writes in their own fields. They are added
  into `input_tokens` to satisfy this repository's cache-inside-input invariant
  and the fact records `InputSemantics::Fresh`. Entity ids are
  `claude:{sessionId}:{messageId}` (or `claude:{messageId}` when the line carries
  no session), the family is `session_log`, and one request is one message.
- Price, latency and status stay absent. A transcript records completed
  assistant messages, so failed calls are not in the file at all; every stored
  fact is `priced = false` and counted as `unpriced`, and `status`/`ok` are
  `None` rather than invented success.
- Desktop: `pick_usage_history_source` gained a `kind` (`folder` opens a
  directory picker, otherwise the database filter); the default instance label
  is the directory name for a folder and the file stem for a file; preview and
  commit open `Source` instead of the cc-switch adapter directly.
- UI: the drawer's empty state offers **Choose database file…** and **Choose
  transcripts folder…**; the confirmation line names both the copy and the
  adapter that read it (`{name} ({source})`), so a person can see that the folder
  was read as Claude Code. All three locales are updated.
- Verification: 14 focused Core history-import tests (5 new Claude Code cases:
  streamed duplicate, cache-inside-input, subagent file, bad timestamp and
  zero-use lines, byte-identical re-read), the full Core suite, desktop
  all-target/all-feature check and clippy with `-D warnings`, `cargo fmt --check`,
  UI build and 41 tests, 30 interaction cases (UI-30 covers both entry buttons
  and asserts the file dialog is not opened for a listed source), layout
  11 surfaces / 70 controls, overflow 9 pages at 820×560. Evidence: **E-199**.
- Not claimed: no real Claude Code transcript was read (temporary fixtures
  only); Codex sessions and OpenCode storage are not implemented, chart/export
  parity is not wired, and the same call recorded by two sources is still
  counted once per source because cross-source identity has no contract yet.

## History import, slice 4 — sources found and remembered — 2026-11-01

An operator pointed out that importing always began with a file dialog, and that
the dialog did not remember where the source was last time. This slice answers
both without weakening the read-before-write rule.

- `discover_usage_history_sources` returns paths that already exist: CC Switch's
  `cc-switch.db` under `ZROUTERY_CC_SWITCH_DIR` (when set), `~/.cc-switch`,
  `%APPDATA%\com.ccswitch.desktop`, the macOS and Linux application directories;
  and Claude Code's `~/.claude/projects` when a bounded walk finds at least one
  `.jsonl`. Discovery only asks the filesystem whether a path exists — it never
  opens a database or reads a transcript, so a listed source is an offer, not a
  claim that it is readable.
- Every successful preview, and every commit, records the path in
  `import-sources.json` under the application's config directory, through the
  existing atomic writer. The file keeps the five newest paths, deduplicated,
  newest first. A missing or unparsable file is an empty list, and a failed write
  is a warning: remembering a path is a convenience, never a precondition for an
  import. Only paths are stored — no counts, no credentials, no source content.
- The drawer lists what was found and what was used before, with the adapter
  label and a "used before" marker; one click previews a listed path without any
  dialog. A remembered path that no longer exists is not listed. The native
  dialog also opens in the directory last used for that kind of source.
- Verification: 8 new desktop unit tests (conventional locations, a home with
  neither, one database found through two roots, a hand-chosen copy offered
  again and marked recent, a vanished path dropped, a detected-and-remembered
  path listed once, the five-path cap with reuse moving a path up, a corrupt
  memory file treated as empty); desktop `--lib` **267 passed** (1 ignored);
  workspace all-target/all-feature check clean; clippy `-D warnings` and
  `cargo fmt --check` clean; command surface **83 interface / 246 registered**
  with an exact capability match; UI build, 41 tests, 30 interaction cases,
  layout 11 surfaces / 70 controls, overflow 9 pages; bundle **700,790 B**
  JavaScript and **148,589 B** CSS. The JavaScript ceiling was raised from
  700,000 B to 706,000 B by this slice, and the gate records that reason next to
  the number. Evidence: **E-200**.
- Not claimed: discovery was exercised against temporary roots, not a real
  installation of either tool; no native dialog was driven on WebView2; the
  Codex and OpenCode adapters, chart/export parity, range selection and
  cross-source duplicate accounting remain open. Nothing is committed, deployed
  or released.

## History import, slice 5 — what the preview exposed — 2026-11-01

The first run against a real cc-switch library failed, and the read-only preview
that was built to check the import had its own visible faults. This slice is the
repair pass: one wrong column type in the adapter, one legacy display name, and
six interface shapes that promised something the screen did not show.

- `avg_latency_ms` is read as a number, not an integer. cc-switch stores the day
  rollup's `AVG(latency_ms)`, which SQLite returns as REAL, and the reader
  demanded INTEGER; the import aborted on the first imported aggregate with
  `Invalid column type Real at index: 14`. `numeric` now accepts INTEGER or REAL
  and the stored rollup keeps its REAL type. The regression fixture is a rollup
  whose average is `1234.5`, so a future narrowing to integers fails by name.
- A client name recorded before the desktop rule existed is raw user-agent text,
  and the per-client ledger printed it verbatim. `client::name_for_user_agent`
  is a lookup at aggregation time (`stats::client_display_name`): a string a
  shipped rule knows is shown as its product name, and the stored record keeps
  what it wrote. Family and confidence are untouched — naming is not
  attribution, and an unknown string stays unnamed rather than becoming a
  product.
- The heatmap is a year view again, and it is sized to the panel. It draws the
  loaded series whatever the range selector says (the selector is hidden while
  it is on), cells are measured from the container's width and clamped to
  7–22 px so a young year is not five giant tiles, month names sit under the
  grid, and the weekday row labels are gone. The legend moved to the right end
  of the toolbar. A reading line (`requests · 1 year · total 4,485`) was added
  because three heatmaps shaded from their own quartiles cannot show that the
  measure selector did anything.
- A measure with no data is not offered. When the loaded year has no cache
  tokens the 缓存 tier is absent, and a selected cache metric falls back to
  requests; the check runs over the whole year rather than the window, so
  switching ranges never reorders the segment under a pointer.
- The Activity lede names the imported period. After an import the page covers
  more than this device's ledger, and the earliest imported day is stated
  (`This device since …; imported history starts …`), or `No requests on this
  device yet; imported history starts …` when the local ledger is empty. The
  imported counts still stay in their own row.
- A wallet hold is a property of a provider, so it is marked on that provider's
  row (red dot, `Balance empty — requests paused`) and explained in its drawer,
  which carries the release button. The separate page-level list that repeated
  the same providers is gone. A compatibility toggle whose explanation needs the
  whole line now spans the grid instead of wrapping in one column.
- The source strip is a flex row with a real gap, so the standby-database link
  and its hint no longer run together as one sentence.
- Verification: cc-switch focused suite **15 passed** including the REAL rollup
  regression; three new Core tests for the display-name lookup and its two
  aggregation cases; UI build clean; **41** UI unit tests; **30** interaction
  cases (UI-30 now asserts the imported period appears and disappears with the
  import); layout **11 surfaces / 70 controls**; overflow **9 pages** at
  820×560; bundle **702,801 B** JavaScript of 706,000 and **148,825 B** CSS,
  with the reason for the increase recorded next to the ceiling. Evidence:
  **E-201** and **E-202**.
- Not claimed: the running `zroutery.exe` is an earlier build, so the REAL
  tolerance, discovery and the display-name lookup were verified by tests, not
  by the live preview; the native dialog, a real cc-switch library and a real
  Claude Code transcript remain unexercised on WebView2; Codex/OpenCode
  adapters, chart/export parity and cross-source duplicate identity remain open.
  Nothing is committed, deployed or released.

## History import, slice 6 — the headline and the curve, together — 2026-11-01

The preview said `本机自 … 起；导入历史自 2026年5月29日 起` while the chart above
still counted only this device. The sentence was true; the figures beside it were
not the ones the sentence described. This slice makes the imported history part
of what the page counts, and lets a schema-4 standby database be read.

- `StatsSummary::add_imported` folds the imported store's counted totals into
  this device's headline: requests, failures, input/output tokens (cache stays a
  subset of input), unpriced rows and known spend per currency. `since` moves
  back to the imported store's first counted day, so the period printed beside
  the figures is the period those figures cover.
- The breakdowns are deliberately not merged. The imported store carries no
  request kind and no attribution evidence, and building `per_client` rows from
  it would claim a family and a confidence it never had. The ledger says so in
  one line when imports exist, instead of letting a reader add its rows and
  conclude the page disagrees with itself.
- `Stats::merge_imported_days` adds imported day rows onto the local series by
  local day. A day only the imported store knows is that store's count — not a
  zero the other store measured — and a day both describe is summed. The series
  stays sorted by day.
- Deduplication is the import's own: facts are keyed by source, instance and the
  source's entity id, and an unchanged fingerprint is recognised instead of
  rewritten, so importing the same copy twice cannot make the figures grow.
  Cross-source identity is not guessed from a nearby timestamp and a model name;
  when two stores hold the same call with no strong id between them, both are
  counted rather than one being silently dropped.
- `get_usage_sources` merges only for the local source: a standby database is a
  different installation's history, not this device's, so it is shown on its own.
- `RuntimeStore::open_read_only` accepts schemas 4 through 5. Version 5 only
  added the imported-fact tables; the usage tables and indexes the reader queries
  are unchanged in 4, so a colleague's database no longer has to be opened (and
  migrated) by the owning build before it can be read. Older layouts still
  refused: before 4 the timestamps were not normalized and the expected indexes
  did not exist.
- Verification: two new Core tests (headline adds without inventing a split;
  imported days add without creating quiet days), the `runtime_store` suite at
  **14 passed** including a schema-4 file read without migrating it, desktop
  all-target/all-feature check clean, UI build and **41** tests, interaction
  **30 of 30** (UI-30 now checks the traffic hint and the ledger's local note
  alongside the imported period), layout **11 surfaces / 70 controls**, overflow
  **9 pages**, bundle **704,010 B** JavaScript of 706,000. Evidence: **E-203**.
- Not claimed: no real cc-switch library was read; the merge is proven against
  fixtures, and the running application must be rebuilt for the desktop command
  to return merged figures. The client/model split still describes the live
  ledger only, and per-request imported rows are not added to the request
  stream. Codex/OpenCode adapters and cross-source duplicate identity remain
  open. Nothing is committed, deployed or released.

## Source review reconciliation — 2026-11-01 (E-204)

The current tree was reconciled against the accepted contracts. This entry
records the source findings and the resulting plan/Todo/node updates, with
implementation evidence retaining its original dates and scopes.

- Milestones: M0 done; M1–M5 partial; M6 first-batch exit blocked, with M3–M5
  outside the first batch. `dev` and `origin/dev` at `4075a54`; empty index;
  pre-existing unstaged/untracked work untouched in ownership.
- P-ACCOUNT: binding staging/journal/publish/discard/recover, CAS-checked token
  capture, explicit identity-change confirmation, fresh generation per
  declaration and per-currency panel aggregation are implemented and committed.
  The real two-account AgentRouter identity observation remains an external gate.
- P-PRICE: `resolve_rate` is defined and re-exported with **no production
  caller**; `effective_rates` has no writer; candidate and pipeline rate quotes
  stay `None`; cost ranking still reads model-level pricing. NewAPI import still
  defaults the group to `default` and writes the shared `model.pricing` with a
  hardcoded USD currency, so account-scoped effective-rate execution, budget and
  ML cost inputs remain open.
- P-EFFORT: requested-effort preservation, full-set candidate filtering with an
  explicit no-candidate error, none/minimal separation and the cross-provider
  catalogue effort isolation are implemented. The desktop public class still
  publishes the ordered **union** of member effort levels against the contract's
  intersection — the divergence already noted on 2026-10-31 remains open. The
  per-surface reasoning-evidence structure has no production writer, and genuine
  AgentRouter effort evidence stays external.
- History import (slices 1–6, uncommitted): exactly two of the four contracted
  source adapters exist (cc-switch, Claude Code transcripts). The desktop commit
  fixes the range at its default and the instance label at the path default;
  import commits 500-fact chunks as it goes, so an interrupted run leaves earlier
  chunks published and resumes through idempotent re-run — there is no
  whole-import atomic publish, no cancellation channel and no cursor. The store
  supports per-source clears while the desktop command removes everything
  imported. Imported-history filtering/pagination/revision-delta queries and
  export parity are open; B5's SQL paging for local live history already exists
  in the backend. Cross-source duplicate identity remains open.
- Currency qualification: the imported aggregates are **USD-only** today — a
  fact's amount reaches the `cost_usd` column only when its currency is USD, and
  the totals and day-bucket queries sum that column as USD; the full fact,
  including any other currency, survives in the stored record but enters no
  aggregate. The cc-switch adapter emits USD charges and Claude Code leaves cost
  unknown; neither exercises a non-USD aggregate. Earlier increments
  that said "known spend per currency" are qualified by this paragraph; those run
  records are left exactly as written.
- Client evidence boundary: the per-client menu drawer and native picker evidence
  is E-192 and the 2026-10-31 finalization section, together with the earlier
  isolated-PTY Claude menu observation. E-198 is a history-import slice and is
  not client-menu evidence. WebView2 native dialogs, real source libraries and
  transcripts, and Codex/OpenCode native acceptance remain unexercised.
- Gates actually run on 2026-11-01 by the coordinating session, before this
  section landed: `scripts/orch_docs_test.py --self-test` PASS (84 nodes / 122
  edges / 195 evidence), `scripts/command_surface_test.py --self-test` and its
  no-argument run PASS (83 interface / 246 registered, exact capability match),
  and `git diff --check` PASS. The final docs/ACL/whitespace reruns after this
  section are recorded separately by the coordinating session.
- Commit-contract finding: `scripts/commit_contract_test.py --base a2fa145
  --head HEAD` FAILS on the existing committed range — `1f31a88` uses the
  disallowed scope `docs(development)` and an 83-character subject, and
  `72f5b1e` leads with the non-allowlisted word `dual`. This is a real defect in
  already committed history. No rewrite was performed and none is proposed here;
  the M6 commit-contract gate stays open, and no pass is manufactured for it.
- Test scope: no Rust or UI suite was re-run for this reconciliation. The
  recorded verifications under E-197–E-203 keep their stated scopes (workspace
  `--lib`/`--tests` where recorded; example targets not fully linked), and
  nothing here extends or re-runs them.
- Synchronized records: E-204 (documentation/source-review)
  was registered; UI-UX-M3 moved DONE→REVALIDATE with full six-state and native
  acceptance unfinished and the DAG synchronized; BACKEND-OPTIMIZATION stays
  PARTIAL; UI-UX-M2 stays DONE with the E-196 link added.
  TEST-COMMIT-CONTRACT retains its validator/fixtures DONE state and records
  the failed current range separately. The canonical plan's
  2026-11-01 header increments and its knowledge todo set (new P-ACCOUNT,
  client-mapping, performance and M6 entries; the plan stays 1/7 coarse steps
  complete) were updated outside this repository by the coordinating session.
- Not claimed by this reconciliation: no feature work, no commits, no deployment,
  no release, and no closure of native-client, external-accounting, platform or
  release gates.

### Post-edit verification

- `uv run --python 3.12 --no-project python -B scripts/orch_docs_test.py --self-test`:
  **PASS**, zero self-test failures; **84 nodes / 122 edges / 196 evidence IDs**,
  11 regressions, 8 ADRs and 41 development links.
- `uv run --python 3.12 --no-project python -B scripts/command_surface_test.py`:
  **PASS**, 83 interface commands / 246 registered, exact capabilities match;
  the command-surface mutation self-test also passed earlier in this audit.
- `git diff --check`: **PASS**. The index remains empty and HEAD remains
  `4075a54`; existing implementation changes retain their ownership.
- External plan readback: **1/7** master, **0/4** original U, **0/5** original T,
  **2/9** original R; all 12 numbered master sections remain. Project knowledge
  readback confirms that the existing 19 Todos were updated and four missing
  entries added, all associated with the master plan. The old U plan has no
  duplicate linked Todos. U13 → U14 is retained; P-ACCOUNT → P-PRICE is added.
- The earlier **FAIL** for commit range `a2fa145..4075a54` remains open in M6;
  successful documentation/ACL checks do not override it.

## Handoff implementation — 2026-11-06 (validation in progress)

The user transferred the whole task to a new primary session. The canonical plan
was read in full before any repository operation. `dev`, HEAD and local
`origin/dev` were still `4075a54`, with an empty index and the prior upgrade work
retained. No commit, history rewrite, deployment or runtime/client reconfiguration
was requested or performed here. Rust 1.99.1 matches the freshly resolved CI
compiler/Cargo hashes; MSRV remains a separate gate.

### Review decisions and implementation scope

- Prepared history is now staged **during preview**. Confirmation publishes the
  exact staged metadata, source version and half-open UTC range, with a saved
  fingerprint and history-revision CAS. It never reopens a changing source after
  confirmation. Publication iterates bounded SQL pages inside one transaction;
  discard/clear revoke pending previews atomically. Cancellation has an explicit
  atomic commit boundary and never claims cancellation after publication won.
- Complete previews survive restart and can be confirmed from the drawer.
  Incomplete source scans are restarted from a fresh snapshot, not described as
  cursor recovery: resuming a previous scan of a changed source could retain
  deleted/stale staged rows. True versioned per-file/source cursor recovery is
  still open. Schema 7 retires only unsafe schema-6 unpublished stages; existing
  imported history, accounting and the user's source files are retained.
- Schema-5 currency migration uses the stored currency/amount rather than a
  USD-only aggregate. Read totals and their currency groups share a transaction.
  Historical migration fixtures now build the actual schema (rather than merely
  lowering `user_version` on a current database). Counts/overflow and unknown
  token-semantics markers are rejected rather than silently converted to zero.
- CC Switch scans hold a read-only WAL-aware transaction; Claude file scans
  reject changing file membership/metadata and cancellation is checked while
  reading lines. A CC Switch duplicate-search margin no longer leaks rows into
  the requested window. Arbitrary source error text is not imported.
- OpenCode V1/V2 accounting adapter added, using table/column shape and metadata
  projection, including V2 compaction, migrated-ID precedence and late updates.
  The official V2 API/schema was consulted on 2026-11-06; source-format comparisons
  also used `E:\zr-research\cc-switch` read-only. Fixtures are not native-source
  acceptance. Codex, cross-source canonical association/conflict resolution,
  unified query/export and complete bounded/resumable scans remain open.
- The user explicitly delegated response-model identification to
  `zroutery/lightvela-auto`. The first child-session call was refused without
  starting work; the authorized independent session then implemented only the
  agreed IR/protocol/stats/accounting/lifecycle scope. Main orchestration,
  SystemOne, Codex staging/compact and UI integration remain primary-owned.
  Structured `model` and `modelVersion` are separate bounded observations;
  auto-only/missing/conflicting responses do not become verified backend labels.
  Transport observations additionally retain preface/error/failed-stage metadata
  before a parser can discard it; retries reset the handle per send.
- JEV investigation: read-only allowlisted desktop declarations show two
  `decision` providers, not Anthropic providers. Local mock requests use
  SystemOne for both buffered and SSE Claude Messages ingress. Confirmed defects
  were doubled `/v1` for a versioned base and a UI `API` row showing only ingress;
  full-endpoint and gateway-prefix URL cases, tool state, usage and explicit
  response declarations now have loopback coverage. This does not assert the
  reported live incident is fully reproduced or authorize replacing the live app.
- P-EFFORT's public intersection now uses the exact same provider/model/surface
  evidence as request filtering. Manual effort changes publish this evidence;
  catalogue refresh cannot replace a manual empty set. Actual AgentRouter effort
  evidence and the three native menu-to-request acceptances remain external.
- P-PRICE now resolves executable account/key options before explicit policy
  cost scoring, preserves the selected credential through expansion, and retains
  policy order after scoring. The implicit default retains existing escalation,
  decision traces and shadow correlation while the configured global strategy
  owns its final order; only an explicit policy enables price-dependent ranking.
  Unknown/cross-currency costs cannot
  enter cost ranking; pre-send quote/binding validation and a scoped-budget
  missing-rate rejection were added. Public NewAPI group prices are explicitly
  adopted as **estimated account/key fallbacks**, including per-call prices,
  rather than overwriting sibling/shared model prices or claiming verified
  effective billing. Actual group/rate evidence, key-binding publication,
  election/ML feature freshness and the full price failure matrix remain open.
- UI steering: the user reported worse usability than CC Switch and Magpie.
  Review found the default Simple mode hid the three basic setup doors. Both
  modes now retain the accepted six-door task architecture; advanced controls
  remain at their task locations. Home provides the setup path and readable
  usage tiers without promising price/speed/model strength. Import has source,
  review and result states, optional range/copy labels, cancellation, saved
  previews and inline feedback. Read-only drawers no longer show unrelated
  configuration-save success. Per-attempt diagnostics name inbound and outbound
  protocols separately. This is task-path repair, not full UI acceptance.
- The first UI budget run failed at 714,476 B against 706,000 B. The actual new
  workflow/diagnostic/effort surfaces and three locales are budgeted at 720,000 B
  with the reason beside the ceiling. A subsequent build measured 716,230 B JS,
  150,703 B CSS and 189,604 B maximum chunk. No performance gain is inferred.
- A patch reported `ENOSPC` after module registration and before writing the
  OpenCode adapter. Drive inspection found free space (E: about 18 GB, C: about
  68 GB); a smaller patch restored the empty new file. No cache/work/user file
  was deleted to manufacture a successful write.

### Verification so far (not final acceptance)

- Rust 1.99.1: history adapters 18 passed before the next two CC Switch cases;
  runtime store 25 passed; Core all-feature library 1,647 passed / 1 ignored;
  SystemOne contract 2 passed (including six endpoint-shape sends); executable
  pricing 3 passed; public-price scope desktop case 1 passed; earlier gateway
  target subset 4 passed. Later transport/auxiliary changes require reruns.
- The delegated model-declaration suite reported 10 all-feature and 10 minimal
  behavior passes plus owned-file formatting/lib clippy. Main all-target clippy
  found a nested format in its fixture; corrected here, rerun pending.
- Routing regressions exposed old capability fixtures with nonzero cost weights
  but no tariff. They now declare a common explicit fallback so they still test
  their capability/ordering contract; the production unknown-rate refusal stays.
  Removing the implicit default entirely initially broke escalation/traces/shadow
  in ten workspace targets. It was restored as a traced fallback with explicit
  ordering authority; the corrected ten-target rerun passed (10 dataset, 63 E2E,
  7 identity, 11 closed-loop, 8 multi-provider, 49 lifecycle, 14 real-shadow,
  15 shadow, 25 tier reporting, 12 vision; 6 ignored). These are fixture runs,
  not permission to train or promote a user's model. Latest Codex 15, eligibility
  18, routing 88 and declared-model 11 pass. A scoped-budget check briefly applied
  tier limits to classifier traffic; the existing classifier tier exemption is
  retained. The vision budget fixture now declares its deliberately free main
  tariff instead of treating an absent price as free.
- UI 41 unit tests, all 30 interaction cases, and layout 12 surfaces / 76 controls
  passed at their respective snapshots. Layout first failed because its Simple
  assertion required hidden navigation, then its new import selector assumed an
  English locale; it now checks mode state/six doors and a stable import marker.
  The intermediate failures are not counted as successful executions.
- Command surface and mutation self-test pass (86 UI calls / 250 registered at
  that snapshot). Complete workspace, minimal behavior, feature matrix, MSRV,
  final UI/whitespace/docs, native platform and installation gates remain to run.

- Latest strict workspace/all-target/all-feature clippy passed before subsequent
  read-scan cancellation tightening; Rust 1.90 locked workspace/all-target check
  passed. UI overflow passed all 9 areas at 820×560 plus contained-wide-table
  probe; layout passed 12 surfaces / 76 controls; UI unit tests remain 41 pass.
  No full-workspace pass is claimed from the earlier `--no-fail-fast` failure.
- The user additionally requested an independent UI research session using
  `zroutery/agentrouter-gpt-6.1-sol` with the confirmed `max` variant. It is
  restricted to a new review report and read-only reference/mock research. No
  further UI design conclusions are inferred before that report returns.

The source plan remains M0 checked and M1–M6 open. This work is uncommitted.

### Delegated correctness slices and integration — 2026-11-06

The user requested small implementation tasks on `zroutery/lightvela-auto`.
Independent sessions received exclusive files for credential revalidation,
usage observation, auxiliary settlement, election invalidation and the preview
example's feature guards. A subsequent inference-key publication slice is in
progress. The primary session owns cross-module integration and final gates.
The separate SOL Max reference-UI review was stopped, then the user reported
it resumed; no unfinished review is treated as an accepted design.

- Credential expansion now reads current account/key declarations, preserves
  the scored key's original reference and quote, and refuses stale identity,
  price, model-control or outbound settings. No `Some`-only loophole permits
  an unknown quote to gain a new tariff between decision and send. A removed
  selected key cannot silently become a different quoted key. Runtime wallet
  and cooldown checks run again before dispatch, including retries.
- `RequestLifecycle::begin_send` places the final check before a sent fact.
  OAuth preparation and retry waits occur before this check; refusals produce
  unsent preparation evidence. A handler/handshake drop now finalizes received
  metadata even before an SSE body takes ownership. Terminal attempt facts
  are no longer changed by repeated transport capture.
- Auxiliary description encoding now precedes the durable send. After OAuth
  resolution, identity/rate/availability are rechecked. Auxiliary failures use
  the concrete candidate's account/key failure adapter. A repeated `finish`
  preserves the original fact and still reports unresolved persistence; local
  persistence failure prevents a main or repair send through accounting readiness.
- Wire usage is strictly checked separately from successful answer decoding.
  `AttemptFact.usage_evidence` retains absent/partial/measured/invalid/conflicting
  provenance. Invalid or conflicting counters do not enter token totals or
  token-derived charges; partial failed usage keeps its known cost and flags
  the unknown remainder. Gemini candidate plus thought tokens now form the IR
  output total, with the inverse encoding. SystemOne error usage has its own
  cache/reasoning reader instead of the Responses detail shape. Cost headers
  read the final attempt's accounting, so a decoder's default cannot report a
  false zero cost. The Activity attempt view and all three locales show usage
  provenance. Explicit all-zero usage is still conservatively unknown.
- Election invalidation is paired with the registry swap under its lock;
  a late election can install only against the exact registry it measured.
  Provider billing/credential/outbound changes and existing price/priority or
  scoring changes invalidate the order. Pure model additions retain the
  existing measured primary. Primary review moved the change comparison under
  the lock too, so it compares the configuration actually being replaced.
  Election probes still use model-level price/default credentials: that wider
  effective-price integration remains an explicit P-PRICE gap.
- Feature-matrix run `sh_1b07f6285001dcGNFAvOLNBFwI` failed in 18 desktop
  combinations: the untracked `src-tauri/examples/preview.rs` called ML/account
  methods without their feature guards. Core combinations passed. The example
  now uses the real method guards and explicit unavailable errors. The worker
  reports focused example checks and formatting passed; the complete matrix
  is being rerun, not assumed green from default-feature checks.

Integration evidence at this snapshot (Rust 1.99.1, isolated stores/fake upstreams):

- `sh_1b0c87455001m734wCNfEcej81`: no-default Core library **965 passed**;
  auxiliary accounting 4, SystemOne contract 3, Codex staging 15, durable
  accounting 6, executable pricing 4, election invalidation 11, execution
  revalidation 4, lifecycle 49, recovery retry 1, quote-boundary 3, model
  declaration 11, usage query 3, vision 12, rejected-wire accounting 2 passed.
  Subsequent selected-reference/control checks require the final rerun.
- The first integration compile observed other workers' incomplete helper
  imports plus a primary test's nonexistent `ledger_snapshot`; the helper
  imports and test's `ledger()` call were fixed before the pass above. A source
  fence expected the older model-only failure adapter; it now checks the
  concrete-candidate adapter and includes the auxiliary/credential modules.
- UI build and **41 unit tests**, **30 interaction cases**, **12 layout surfaces /
  76 controls** and **9 pages at 820×560** passed. Interaction logged local HTTP
  server `ConnectionResetError` during browser replacement but every case
  completed and the process exited zero. The latest provenance-label build
  measures **717,629 B JS**, **150,703 B CSS**, **189,604 B largest chunk**, inside
  unchanged 720,000 / 180,000 / 320,000 ceilings. Those labels were added after
  the earlier full interaction run; final UI checks are kept distinct.
- Command self-test and real ACL/registration check pass: **86 interface calls /
  250 registered**. A manual browser-mock source→preview→cancel interaction
  displayed the saved snapshot and cancellation result; it used fixtures only.
- Final workspace tests, strict clippy and the full feature matrix have been
  started after integration. Their eventual results are still required. Native
  clients, actual NewAPI billing/identity/effort, Codex source positives and
  platform/installation acceptance remain open; no milestone is checked here.

### Inference key publication slice — 2026-11-06

A delegated slice closed the desktop inference-key replacement hole: the
command used to overwrite the secret behind the existing reference before
saving the configuration, so a failed save left the running state serving an
unpublished secret, and two racing IPC reads could revert each other's fields.

- `src-tauri/src/inference_commands.rs` is now a thin command layer over a new
  `inference_commands/publication.rs`; IPC signatures, parameter names, return
  types and registrations are unchanged, so no UI/api.ts/mock-bridge change was
  needed. `inference_commands/tests.rs` adds the regressions.
- A secret-bearing change is staged on a fresh reference
  (`...:inference:{key}:{uuid}` suffix, checked against every reference the
  document names), the secret is written to that reference, the document is
  normalized, validated and saved, and only then does the running state move to
  the new reference. Failure compensation deletes only the staging entry this
  call created and only while nothing names it; the previous document, its
  reference and shared siblings stand untouched.
- The whole read–modify–save holds the migration lock instead of a
  configuration-revision CAS (same precedent as `clear_account_key`), so a
  concurrent metadata edit cannot be lost. Key-scoped `effective_rates` rows die
  with the replaced secret, because `resolve_rate` matches by binding version
  and key id — not by reference — and would otherwise keep pricing the new
  secret with the old credential's evidence.
- Worker verification: `cargo +1.99.1 test -p zroutery --lib inference_commands
  --locked` **9 passed**; `cargo +1.99.1 clippy -p zroutery --lib --tests
  --locked -- -D warnings` exit 0; owned-file rustfmt and whitespace clean. The
  primary session reviewed the diff: staging uniqueness, compensation guard,
  lock discipline and post-publication retirement read correct.
- Honest boundaries left open: a process death between the staging write and
  the save can strand an unreferenced credential-store entry (no common
  transaction; a durable journal plus startup sweep is separate recovery
  wiring, mirroring `account_binding.rs::recover_bindings`), and key-scoped
  router cooldowns keyed by key id still apply to the replacement secret after
  a rekey. Both are recorded as follow-up work, not claimed as done.

### UI task-experience batch A — 2026-11-06

The accepted reference-experience review
(`ui-reference-experience-review-2026-11-06.md`) was read in full and its
batch A ("action trustworthiness and reachability") was implemented as five
delegated slices; the primary session owned shared-file sequencing
(i18n, `components.tsx`, `App.tsx`/`saveQueue.ts`, `Providers.tsx`,
`Models.tsx` were dispatched so no two workers shared a write scope at once)
and final integration.

- **A1** (`Providers.tsx`): the add-provider form now awaits its own save;
  a refused or stale write keeps the draft retryable with the error beside
  the form, a duplicate name never opens a stale entity, an in-flight second
  intent is inert, and only a real `added` while mounted clears the form and
  opens the new provider's drawer. `providers.add_failed` added in all three
  locales; six vitest regressions.
- **A2** (`components.tsx`): the four commit inputs share one
  `commitInputKeys` contract — one Enter is one commit, the Enter-caused blur
  is consumed by a mark (never by moving the baseline, so a refused save stays
  retryable), IME composition (including keyCode 229) neither commits nor
  blurs, a dirty Escape restores the field and is consumed, and a clean
  Escape bubbles to close. `numberDraftCommit` consolidates the number
  commit rules; the Drawer's close handling moved from capture to bubble so
  fields answer first (`drawerEscapeCloses`), with the Tab trap kept in
  capture. Twenty unit tests.
- **A3** (`saveQueue.ts`, `App.tsx`, call sites): tasks are `write` or
  `observe`; only a write may claim saving/saved/error. Startup snapshots,
  balance refreshes, probes, token-capture re-reads, gateway toggles and
  usage-library operations no longer produce a saved claim; a refresh
  failure keeps the last valid values and says so; `SaveFeedbackReset` gives
  every newly opened drawer a clean verdict so one surface's success never
  answers for another. Write ordering, queue-drop notification and
  CAS/revision discipline are unchanged (existing `saveQueue` tests untouched
  and passing). `toast.balances_refreshed` / `toast.balances_refresh_failed`
  in three locales; seven vitest regressions.
- **A4** (`Models.tsx`, `Providers.tsx`): the header add action now mounts
  its form directly under the page head — where the reader is — scrolls it
  into view and focuses the first field; folding again returns to the
  trigger and reopening re-focuses. No virtualization, no list/grouping
  change. Three vitest regressions.
- **A5** (`Models.tsx`, `styles.css`, i18n): the price column is gone; the
  price is a secondary line under the model name ("input X · output Y ·
  USD/1M", the unit stated once), with a formatting helper that shaves IEEE
  tails (1.4700000000000002 → 1.47) and never prints exponent notation,
  a readable "no price configured" label instead of a blank, known-zero kept
  distinguishable from unset sides, and the raw values on the title
  attribute. The variant (subtitle row) was chosen by the user from three
  rendered proposals before implementation. Nine vitest regressions.

**Integration findings.** The browser gate suite caught a real A2 defect the
unit suite could not see: `drawerEscapeCloses` had lost the
`key === "Escape"` check in the refactor, so *every* unprevented keydown
bubbling from a drawer field closed the drawer — the harness's select-all
Ctrl+A inside `replace_text` killed the drawer mid-edit (UI-1), the rename
Enter closed it after the commit (UI-23), and a later veil click failed
(UI-3). A2's tests had only asserted non-top, composing and
prevented-event refusals and never the plain-key negative. The primary
session fixed the check, added `key: "a"` / `key: "Enter"` do-not-close
regressions, and updated the existing tests to name the key. The first
four-gate batch was killed externally before any result; nothing was
recorded from it, and all gates were rerun on the fixed tree.

**Batch A final gates** (fresh build including all five slices and the fix):
`tsc` clean; vitest **12 files / 86 tests**; interaction **31/31** including
the new **UI-31** ("the add form lands in view and takes focus", run at
`?stress=600`, asserting focus plus viewport intersection plus the form
mounting ahead of the list, with no harness scrolling); layout **12
surfaces / 76 controls**; overflow **9 pages** at 820×560; budget
**722,796 B JS of 726,000** — the JS ceiling was raised from 720,000 with
the user's approval, with the batch A justification recorded in
`scripts/ui_budget_test.py` for the commit that ships it; CSS and
largest-chunk ceilings unchanged. Known remaining UX gaps from the review
(batch B/C/D) are not claimed here.

### Upstream failure classification — NewAPI pre-charge refusal — 2026-11-06

A live upstream (`xiatianai`, a NewAPI-family relay) answered an exhausted
wallet with **403** and `{"type":"new_api_error","code":"insufficient_user_quota"}`
("预扣费额度失败, 用户剩余额度: …, 需要预扣费额度: …"). The classifier's
balance markers did not match (`insufficient_user_quota` does not contain
`insufficient_quota`, and the message says 额度**失败** not 额度不足), so the
error fell through to the status table and became **Authentication**: the key
took an authentication cooldown, the wallet was never latched, model health
took the hit for a wallet fact, and routing treated it as the wrong class.

Fix (classification only, no pipeline change): `is_balance_exhausted_message`
now recognizes the relay's stable `insufficient_user_quota` code and the
pre-charge message shape. The error classifies as `BalanceExhausted`, which
already carries the right behaviour end to end: the wallet latch holds the
account (`hold_empty_wallet`), the failure is fallbackable to the next
candidate, never retried against the same wallet, and does not poison the
model circuit.

Verification (Rust 1.99.1, `--no-default-features`): Core library **968
passed** (one new regression classifying the live 403 body and its
message-only form as `BalanceExhausted`, with a plain-403 negative staying
`Authentication`), `failure_authority_test` 10 (the canonical classification
fence accepts the markers), `pipeline_lifecycle_test` 49,
`rate_limit_cooldown_test` 4, `retry_quote_boundary_test` 3;
`rustfmt --check` clean.

### Abandoned tool rounds are closed, not refused — 2026-11-07

A live client hit `invalid request: tool-call history is missing results before
the next message; restore the complete tool round` after a cancelled tool call:
the assistant turn carried `tool_calls` with no `tool` results before the next
user message. The strict send-boundary validator refused the whole turn.

`protocol::openai::close_abandoned_tool_rounds` now runs before
`validate_tool_history` in the pipeline's encode step. For an unanswered call
it inserts an explicit result — "Tool call not executed: the conversation
continued without its result." — in call order, preserving the round rather
than inventing an output or deleting the call. A complete round is untouched,
and an orphan result or duplicate id still fails closed because an interrupted
round cannot explain those.

Verification: `protocol::openai` 42 passed (two new regressions pin the repair
and the still-refused orphan), `pipeline_lifecycle_test` 49,
`codex_auto_e2e_test` 15, `recovery_retry_test` 1; `rustfmt` clean. The
Anthropic dialect has its own pairing rules and was not changed here: no
evidence of the same failure exists yet.

### Repository gates — 2026-11-07, final tree

All four workspace gates were rerun on the final tree (batch A, the drawer
Escape fix, the inference-key publication slice, the failure-classification
markers and every earlier slice included), after the first externally
killed batch produced no results:

- `cargo +1.99.1 test --workspace --all-features --locked --no-fail-fast`:
  every target green, zero failures — Core suites, src-tauri library
  **277 passed** (including the nine inference-publication tests), headless,
  doc-tests empty.
- `cargo +1.99.1 clippy --workspace --all-targets --all-features --locked
  -- -D warnings`: clean after one test-only `type_complexity` fix (a
  `ProviderMutations` alias in `credentials.rs`'s outbound-change test).
- `feature_matrix_gate.py --cargo cargo +1.99.1`: **40/40 combinations** —
  the desktop manifest compiles in every combination a person can type,
  which also verifies the preview example's feature guards on the final
  tree.
- `cargo +1.90.0 check --workspace --all-targets --locked`: clean (MSRV
  remains a separate compatibility gate from the CI toolchain, which
  resolves 1.99.1).
- `cargo +1.99.1 fmt --all -- --check` and `git diff --check`: clean.
- The UI gates are recorded in the batch A section above and were the last
  runs before the Rust gates started; no UI source changed afterwards.
- Two changes landed after this gate run: the OpenCode free-tier refusal
  classification and the abandoned-tool-round repair (both Core only, both
  recorded above). They were verified with focused suites — Core lib 974,
  `failure_authority` 10, `protocol::openai` 42, `pipeline_lifecycle` 49,
  `codex_auto_e2e` 15, `recovery_retry` 1 — but the workspace-wide gates
  listed here predate them and must be rerun before this tree can claim them.

Total-plan checkboxes remain M0 only. Still open and not claimed here:
real NewAPI billing/identity/effort evidence, native client menu acceptance,
Codex history positives, M3 OAuth/subscriptions, M5 small production and M6
installation/signing; the inference-key crash-recovery journal and
rekey-cooldown invalidation noted above. Nothing is committed or pushed.

## 2026-11-08 — plan/status reconciliation (source review, non-gate)

- Reconciled the canonical OpenChamber plan's stale M1 contract-gap claims
  against source: `billing::resolve_rate` has production call sites in
  `crates/zroutery-core/src/router/execution.rs` and
  `crates/zroutery-core/src/router/selection.rs`; `src-tauri/src/state/takeover.rs`
  `gateway_target` publishes the member effort **intersection** (with tests);
  `crates/zroutery-core/src/models_dev.rs` cross-provider lookup merges
  capabilities conservatively and does not advertise effort levels for
  cross-provider matches. The plan text was corrected to describe current
  behavior and keep remaining M1/M2 exits explicit (real
  P-ACCOUNT/P-PRICE/P-EFFORT evidence, three-client menu→request evidence,
  import multi-currency/query/paging/export/clear/atomic batch gaps). No
  milestone checkbox changed (M0 only remains checked).
- Verified untracked follow-up test
  `crates/zroutery-core/tests/rekey_invalidation_test.rs` on the current tree:
  `cargo +1.99.1 test -p zroutery-core --test rekey_invalidation_test --all-features --locked`
  → 3 passed, 0 failed. The rekey credential-gate invalidation hook it
  exercises is present in `server/config_state.rs` (`rekeyed_inference_keys`,
  `invalidate_key_cooldowns`) and router APIs. The test file was left
  untracked/not staged (provenance/worktree ownership not claimed here).
- Isolated ZR→Zen sidecar connectivity check was run earlier in temp
  directories/processes and fully cleaned up; no running ZR (40620:8787) or
  user runtime config was modified. Those artifacts are out of repo and not
  evidence for the plan.
