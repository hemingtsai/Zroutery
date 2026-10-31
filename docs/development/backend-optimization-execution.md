# Backend optimization execution

Implements the backend slice of [backend-optimization-plan.md](backend-optimization-plan.md).
The accepted master constraints remain in [upgrade-contracts.md](upgrade-contracts.md).
Frontend pages belong to ZCode; the user explicitly authorized this execution to
synchronize only `ui/src/api.ts` and `ui/mock-bridge.ts`. All delegated work uses
`zroutery/agentrouter-gpt-5.6-sol`, as requested.

## Baseline

- Windows, `E:\Zroutery`, branch `dev`, base `a2fa14557af4a885d639b7b8ce8be1023ce24e71`.
- Began with substantial staged/unstaged upgrade work. Existing account,
  protocol, pricing, client-catalogue and UI work is a dependency of this slice,
  not newly delivered by it. No commits or deployment are implied.
- Index/working diff snapshots:
  `C:\Users\InfinityNeko\AppData\Local\Temp\opencode\zroutery-backend-20261031-{index,working}.diff`.
- `toolchain_gate.py` with `RUSTUP_TOOLCHAIN=1.99.1`: MATCH with CI, rustc
  `58cf8f9c3`, Cargo `5f94df478`. Minimum Rust remains 1.90, independently checked.

## Contracts delivered by this slice

### B3 — Client attribution

The observed underscore Codex Desktop UA receives stable ID `codex_desktop` and
display name `Codex Desktop`. Reporting `family` retains its existing meaning
(application kind, hence `desktop`); upstream profile resolution separately uses
the Codex profile. Existing archive identities are never re-detected.

The read-only Magpie `internal/agent/agents.go` catalogue informed PI-Desktop,
Kimi Code Desktop, Pi, Magpie and curl rules. Raft has no distinctive UA: only an
explicit/credential identity can name it; Pi traffic remains Pi. Prefix boundaries,
readable explicit names and attribution precedence have regression coverage.

### B5 — Accounting views and bounded history

- `StatsSummary`: `cache_read_tokens`, `cache_write_tokens`, `unpriced_requests`.
- `UsageDay`: the same fields, plus `cost: CostTotals` (currency → known subtotal).
- Cache reads/writes are subsets of input, reasoning is inside output. The
  headline, day and grouping views read `accounting.observed_usage/known_cost`
  when available, falling back to the legacy final-attempt values otherwise.
  The two representations are never added together. A partly unknown request
  increments `unpriced_requests` without discarding known failed-attempt costs.
- Per-model grouping remains **requests grouped by final resolved model**;
  actual charge identities live in immutable attempt facts. It is not a claim
  that a preceding failed attempt used that final model.
- `query_usage_history({ query })` returns
  `{ records, page, page_size, total, revision }`. Pages start at 1, size defaults
  to 50 and caps at 200, out-of-range pages clamp to the last page. Filters:
  `client_id`, `provider_id`, `model_id`, `from`, `until`, `failures_only`.
  Bounds are UTC instants, inclusive start/exclusive end; ordering is
  `at DESC, id DESC`. Pass `expected_revision` for follow-up pages; mutation
  invalidates the page set and returns an error rather than silently skipping rows.
- Runtime schema 4 adds scoped/time indexes and canonical nanosecond UTC index
  keys. Migration is transactional and bounded in memory. Record JSON, IDs,
  client identity and settlement facts are retained. Older binaries refuse the
  new schema through their existing version guard.
- Day queries stream only their indexed window, convert to the operator's local
  date (including DST), and retain quiet days. SQLite work runs off Tauri's UI
  thread. Historical import/clear still does not alter actual budget spend.
- These are **request-day** statistics (`RequestRecord.at`), including observed
  attempts of the request. Budget windows remain the accounting attempt's
  completion-day contract; the request chart is not presented as a budget-window
  reconciliation. Per-attempt provider/day breakdown remains part of the master
  settlement/query contract rather than guessed from the final model here.

### B6 — Export to a selected file

`export_usage_history_file()` opens the native save dialog and returns the path
only after a successful fsynced atomic write; cancellation returns `null`.
No target path or archive-sized payload is accepted from the webview. The existing
schema-v1 JSON export stays readable by the current importer. Both export paths
redact live error bodies and routing diagnostics, including memory-only operation.
The file path streams one SQLite row at a time into the archive; legacy text
export/import/clear run on blocking workers. Pending settlement prevents a
partial archive or a clear that would silently reappear after recovery. Windows
replacement uses `ReplaceFileW` to retain the existing archive's DACL.
The dialog plugin is used from Rust; no general filesystem/plugin permission is
granted to the webview.

### T — Tray shell handoff

- Left click attaches/reuses the transparent, frameless `tray-panel` webview at
  `index.html?mode=tray-panel`. The work-area calculation accounts for DPI,
  negative monitor origins, taskbars and menu bars.
- The panel acknowledges `tray_panel_ready()` **after its own page is mounted**.
  It stays hidden before this handshake. A missing/older panel bundle falls back
  to the dashboard after two seconds; it cannot expose the full dashboard in a
  small transparent frame. Hidden fallback webviews are destroyed.
- Esc/blur/close hides only the panel; closing it does not quit the gateway when
  `keep_in_tray=false`. A short blur/click guard prevents reopening on the click
  that dismissed it. Native menu still has dashboard, start/stop, copy URL/token
  and quit direct actions.
- Panel ACL grants only `get_tray_snapshot`, `tray_set_provider_enabled`,
  `tray_set_gateway_running`, accent and panel navigation/events. The DTO exposes
  safe names/enabled flags/tiers plus eight redacted recent summaries; complete
  configuration, custom credential headers, key references and diagnostics never
  enter the panel. Targeted provider enable performs its
  read-modify-write under the configuration mutex; it never sends an entire
  stale configuration back from the panel.
- Configuration writes announce payload-free `zroutery://config-state-changed`.
  Pages should subscribe through `onConfigStateChanged`, refresh on show, and
  stop polling while hidden. Existing gateway-state notifications remain separate.
- `Snapshot.config_revision` binds full saves to the config they were edited
  from. The bridge carries it through `structuredClone` in a transient
  `__config_revision` marker, strips that marker before IPC, and sends
  `expectedRevision` separately. An unrelated later snapshot cannot authorize
  an older document; Rust compares the token under the same mutation lock.

ZCode handoff: implement the panel page, 50 ms hover/three-point pointer intent,
submenu search reset and in-panel feedback; call `trayPanelReady` after mount,
use `hideTrayPanel`/`showDashboard` for navigation, `traySnapshot` for data, and
`traySetProviderEnabled`/`traySetGatewayRunning` for mutations. The UI chart/export
controls still need to consume B5/B6. Backend/unit
evidence is distinct from native multi-monitor/pointer/keyboard acceptance.

### B1 — Configuration publication

Multi-file client setup stages config, catalogue, ownership and default selection
as one operation. Before the durable commit marker, abandonment discards staging;
after it, recovery rolls forward idempotently. Unexpected live bytes stop recovery
and retain evidence. A fresh preview binds desired fields, source fingerprint,
catalogue content and auxiliary ownership, so stale intent cannot publish.

Restore plans capture current live fields. Explicitly confirmed manual changes
are retained as restore values; unmanaged current values are preserved. Codex
catalogue restoration preserves original bytes; keeping an externally edited
catalogue detaches ownership so a later adoption can be previewed cleanly.

The standalone app config retains its fsynced single-file atomic replacement.
It must not acquire a recoverable, forward-publishing error after callers have
compensated a failed credential binding. Its temporary names are now unique,
created exclusively and cleaned on failure. The multi-file journal is integrated
where the caller supports recovering the whole set, rather than silently changing
the error contract of all single-file writers.

### B4 — Gemini CLI and declarative adapters

Reference: Gemini CLI v0.66.0, commit
`970413207732f5d4d8c50540c7fc359c44fdaaf2`; Magpie v0.1.2283,
`a6951cc63af6b992625a96d93aafe438c14ffab5`; CC Switch 4.0.22,
`ba3d80d87ce7e3fe1ed52a5d5cbe9fa79eb5898e`. Research reads these sources;
no active Gemini login/configuration or remote inference is used for acceptance.

The existing Gemini body codec alone was insufficient: the SDK supplies its
model/operation in `/v1beta/models/<model>:<operation>` and authenticates with
`x-goog-api-key`. Native generate/stream/count routes and a Gemini-shaped model
listing now share existing routing/accounting. URL model and stream mode win
over conflicting body extensions. Token count is explicitly an estimate and
does not create usage/confirmation. The route implementation captures the
whole resource and splits its operation because this locked Axum build rejects
the attempted `{model}:operation` path pattern.
Native `thinkingLevel` is retained as a native reasoning request, with complete
set filtering; unsupported cross-protocol levels are refused rather than silently
dropped. Token budgets are distinct controls and no automatic lowering is added.

Gemini's user settings and sibling `.env` are one logical two-file adapter:
explicit `gemini-api-key` authentication and selected class in settings, bare
gateway root and local token in `.env`, removal of the overriding `GEMINI_MODEL`.
An explicit default is necessary. Nearer env files, inherited variables,
workspace/system settings and `--model` may override user settings; preview
notes name locations/variables without exposing values. Whole-file migration
stays unavailable for Gemini, since its single-file cutover cannot represent
the composite setup. CLI launch/menu confirmation remains an external gate.
Private `.zroutery/gemini-origin.json` metadata joins that transaction and records
original file presence (no credentials). This distinguishes a missing `.env`
from an existing empty one; removing the latter would change dotenv's search
precedence. Legacy settings-only manifests restore without creating an env file.

Other adapters now use a declaration table for binary, environment override,
relative config path and JSON/TOML field preparation. Detection is file or binary
where valid; Gemini additionally requires its CLI binary because other apps also
use `.gemini`. No unrelated client catalogue was added.

## Verification record

### B7 — Native system accent (added to the plan during execution)

`get_system_accent()` returns `{ accent, accent_soft, accent_fg }` or `null` when
the native source is unavailable. Windows reads DWM's **AARRGGBB** value;
macOS reads `NSColor.controlAccentColor` on the main thread and converts to sRGB.
Linux reads KDE's explicit `AccentColor` or the GNOME 47+ named palette (libadwaita
1.6), with a bounded `gsettings` call. No preference is written. The frontend
keeps its CSS fallback on `null` and can re-read on activation/theme change.

The foreground uses the stronger black/white contrast calculated from relative
sRGB luminance, rather than the draft's YIQ threshold which can pick low-contrast
white for saturated red. `accent_soft` is the native RGB at 16% alpha so the
same result can composite over either light or dark skin. Semantic status colors
remain independent. Only Windows is natively exercised on this machine; macOS
and Linux paths are feature-gated with deterministic parser/color tests.

Backend implementation verification is complete for this slice. Frontend/native
acceptance remains explicitly open below; no code-presence statement is a claim
of release or of genuine upstream/client observation.

### Completed scoped gates

- Rust 1.99.1 toolchain gate matches CI. Rust 1.90 workspace/all-target locked
  check passed; its rustup command's final self-updater error did not prevent
  installation or the actual check. Later additions receive a final recheck.
- B3 attribution/profile: 48 + 14 tests passed.
- Multi-file journal: 17 fault/recovery tests passed, including a stale no-op
  auxiliary file, Windows replacement and cleanup interruption.
- Gemini adapter/status: 19 + 14 tests passed, including the multiline edge
  case; native Gemini route/auth/CORS and codec: 2 + 32 passed.
- Durable accounting/takeover/query integration: 6 + 19 + 14 + 2 + 3 passed in
  the relevant runs. A full workspace pass exposed three earlier upgrade
  regressions (account fields, no-replay 429 expectation, all-explicit-keys
  cooling reported as 503); all 18 tests in their three corrected targets pass.
- Native accent: three tests passed, including the read-only Windows DWM path.
- UI build and 35 unit tests pass; bundle budget passes (680,435 B JavaScript,
  142,570 B CSS, largest chunk 189,604 B in the final production build). The contract mock now
  derives summaries/day totals from the same records and carries document-bound
  revisions through clones without persisting bridge metadata.
- Command ACL/registration self-test passes. Orchestration checker and its
  mutation self-tests pass; repaired the missing reverse edges of existing UI
  nodes rather than weakening DAG validation. Commit-contract self-test passes.
  The concurrently added frontend evidence used E-191; this slice's evidence
  is E-192 after resolving that duplicate without replacing the frontend record.
- Mock-provider headless smoke passed every assertion (auth, streaming,
  routing, costs, budgets, history, shadow and balances); Python's mock server
  emitted connection-reset traces while its owned processes shut down. No
  external inference was used. This is the staged B3 image's smoke result.
- A later desktop lib run passed 259 tests (one existing ignored case); statistics
  module filter passed 25. Subsequent least-privilege tray/export changes are
  included in the final queued workspace revalidation, not retroactively in
  older test counts. Cargo invocations were serialized by the shared target
  directory; a build that read during active edits is recorded as inconclusive
  until its rerun, not hidden as a passing verification.
- Final Rust 1.90 locked workspace/all-target check passed after the native accent,
  narrow tray DTO, streaming export and Gemini additions, with no warnings.
- Final desktop no-default-feature all-target check passed without warnings.
- Final Core no-default-feature unit run passed **850 tests**.
- Final Rust 1.99.1 workspace/all-target/all-feature Clippy passed with
  `-D warnings` after the Gemini, tray and export review changes.
- Final file-export replacement/redaction/failure-cleanup tests: **2 passed**.
- Final `cargo +1.99.1 test --workspace --all-features --locked --no-fail-fast -- --quiet`:
  **3,238 passed, 0 failed, 22 existing ignored cases**, across 99 result groups
  including empty binary/doc-test groups. The earlier three failed targets all
  passed in this full rerun. The final export guard was additionally re-tested
  by the two export cases above.

### Frontend handoff / remaining native acceptance

- The synchronized interaction harness passes **25/25** cases. Layout harness
  self-test passes **15/15** and measures all **9 surfaces / 63 controls**. Its
  full run is correctly red for two existing page geometries: the client setup
  row in `ui/src/pages/Takeover.tsx` and budget-add row in `ui/src/pages/Budgets.tsx`
  have an 18px label/button top-edge spread. These page/style fixes remain with
  the frontend owner; no required coverage was removed to obtain a green result.
  A repeat after the concurrent frontend rebuild stopped earlier at “waiting
  for drawer to close”; that latest run is also a failure, not a replacement
  green claim. Re-run the page owner's layout gate after the frontend settles.
- Connect T's panel page/readiness, hover-intent/search/feedback, B5 graph metrics
  and history query source, B6 export-file action, B7 theme injection and
  `onConfigStateChanged` refresh in the owning frontend flow.
- Add Gemini to the setup selector and require an offered default (no “keep”
  default); verify the real CLI menu/request path and recovery in the user's
  chosen entrypoint. Command implementations and local mock requests do not
  prove a native CLI menu.
- Gemini setup accepts strict JSON settings; comments are refused with their
  location rather than stripped. Dotenv preserves unrelated multiline text,
  comments, duplicates and line endings. A held legacy settings-only takeover
  must be released before the new composite setup so original field ownership
  cannot be silently reinterpreted.
- Native tray mixed-DPI/multi-monitor behavior and macOS/Linux accent reads are
  separate acceptance items. Master P0 effective pricing, complete imported
  history adapters and per-attempt provider/day analysis retain their existing
  master-plan status; this backend optimization slice does not close M1–M6.

### B3 live handoff observation

The user explicitly requested a graceful 8788 handoff during this execution.
The first Ctrl-C attempt exceeded the three-minute wait and did **not** replace
the image. The old process subsequently logged `proxy stopped` at
2026-10-31 16:40:39 local. After the interruption, read-only verification found
PID **46568**, started 16:45:26, running the verified staged image:
`c0577b37772c75499df6d864a1d027b9f2594faa73c02cf631c4cffd0f5f44d0`.
This is an observed resumed handoff, not a claim that the timed-out command
completed successfully. Authenticated status/models pass with 47 entries.

The previous instance record was stale: active PID was 45156, not its recorded
4176, and its actual database was
`%LOCALAPPDATA%\Zroutery\standby-8788\runtime.sqlite3`, not the desktop DB.
Process-environment inspection confirmed the active path before any signal;
only paths left that inspection. Configuration SHA256 remained
`34d18705de3ae2dba69804e4c79d5ba0d39086125c03c0de84e18c89945e4f31`.
Schema 4 and newly arriving OpenCode history were observed afterward. No new
Codex Desktop request had arrived at that observation, so its native label is
verified by the deterministic UA fixture and loaded binary, not fabricated
live traffic. Historical client IDs remain untouched.

Backup: `%LOCALAPPDATA%\Zroutery\standby-8788\handoff-20261031-163435-backend-b3`
contains the prior executable, config/instance/spend and a consistent SQLite
snapshot made before draining. No after-drain snapshot was made by the timed-out
command. The later source changes in this session are not assumed to be deployed
merely because B3's verified image is running.
