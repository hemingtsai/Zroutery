# Upgrade execution

Implements the agreed 2026-10-30 master plan. The canonical plan is the
OpenChamber Markdown file named in [upgrade-contracts.md](upgrade-contracts.md);
only its coarse checkboxes are changed when the corresponding gates close.
This record does not replace existing node evidence or claim a release.

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

| Slice | Code / docs | Local verification | Submitted | Released |
|---|---|---|---|---|
| M0 | Task architecture, authority/versions, failure/migration and acceptance contracts frozen; baseline recorded | Rust 1.99.1 confirmed, initial UI tests; online recheck timed out, performance not yet measured | No | No |
| M2a + P-ACCOUNT | Versioned SQLite settlement/attempt facts, opening ledger, binding publication journal/CAS, compensation and identity review implemented; recovery hardening continues | 1519 Core unit tests + 6 durable-accounting cases passed before later protocol edits; 3 desktop binding cases and 2 binding-CAS cases passed | No | No |
| P-PRICE / P-EFFORT + model/routing | Readable names, reasoning request enum/exact filtering and cross-relay effort isolation implemented; effective pricing not yet implemented | Core/desktop all-target check passed; legacy-effort test expectations updated, full behavior revalidation pending | No | No |
| Client four-class setup | Claude native picker/four aliases, explicit model/env default; Codex owned catalogue; OpenCode common capability/variants; preview/apply UI implemented | Native client menus/request observations and full restore regression pending | No | No |
| History/query/charts | Existing graph work; new adapters not implemented yet | Not run | No | No |
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
