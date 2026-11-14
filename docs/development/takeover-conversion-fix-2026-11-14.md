# Takeover model selection and thinking conversion

## Scope

- Client setup publishes exact enabled provider/model IDs alongside usage pools.
  Untiered models can be selected directly; disabled models, disabled providers
  and decision-only providers are excluded. Pool capability intersections remain
  conservative. Exact metadata and pools come from one configuration snapshot.
- `ClientSetupOptions.default_class` retains its persisted/IPC name and accepts
  either an offered class or an exact exposed ID. Existing preview fingerprints,
  file transactions, original-value ownership and release semantics remain in use.
- Claude and Codex accept dotted exact model IDs. OpenCode's existing dotted-path
  ownership representation cannot safely embed these IDs; its menu excludes them
  and explains the limitation. They remain callable through the gateway.
- Responses accepts both absent and empty thinking signatures as plain reasoning
  history. Existing nonempty signed-history bridge behavior is unchanged.
- Gemini represents thinking as thought parts instead of rejecting it. Native
  thought history retains its signature; foreign Anthropic signatures are not
  emitted as Gemini signatures. Buffered and streaming replies distinguish
  thinking from answer text. Opaque redacted history still follows the explicit
  unsupported-content policy rather than silently disappearing.
- Codex asynchronous Guardian risk classification (`high`/`low`) is distinct from
  synchronous JSON `outcome` review. Both use the configured review pool, preserve
  native answer formats and fail closed on unknown answers. Setup never modifies
  approval, permission or sandbox policies.

## Verification

Local Windows toolchain: Rust 1.100.0. `toolchain_gate.py` could not resolve the
current CI toolchain because DNS lookup of static.rust-lang.org failed; these
results are not CI toolchain acceptance.

Completed:

- Frontend build; 89 frontend tests; subsequent TypeScript check.
- 21 classifier unit tests.
- Four desktop takeover verification tests passed, including exact untiered
  model selection, disabled-provider exclusion and preview fingerprints covering
  desired values, catalogue metadata and ownership.
- All 103 Core takeover tests passed, including exact-model defaults without
  approval-policy ownership and restoration of the previous client default.
- Codex Guardian end-to-end regression passed with all Core features enabled:
  asynchronous high/low and synchronous JSON review retain their exact answer
  contracts for both buffered and streaming clients, with review-pool routing
  and request-kind accounting verified.
- 25 Responses interoperability tests and 10 Responses protocol tests.
- 36 Gemini protocol tests and 64 cross-protocol golden tests.
- Decision-model integration tests: 9 passed, 1 live-endpoint test ignored.
  Guardian buffered/streaming answers use high/low or JSON as requested;
  uncertain decisions remain high/deny, never implicit approvals.
- All five thinking-history regressions passed (empty/missing/nonempty signature,
  Gemini history, native signature, buffered/streaming thought separation and
  explicit policy for opaque redacted history).
- UI-26: exact-model selection, preview/apply options and stale reply isolation;
  UI-14 drawer/select focus regression.
- UI layout: all 12 declared surfaces; 15 layout-gate self-tests.
- Command surface and its self-test; whitespace check.
- Workspace format check at the checked snapshot.
- `cargo +1.90.0 check --workspace --all-targets --locked --offline -j 2`
  passed on Windows (18m46s), validating the declared minimum toolchain build.

- `cargo +1.100.0 test --workspace --all-features --locked --offline -j 2`
  completed successfully: 120 test-suite summaries, 3505 passed, 0 failed,
  22 ignored. Ignored tests remain unverified, not passing tests.

Observed gate failures outside the focused behavior checks:

- Full workspace Clippy with Rust 1.100.0 failed on an unchanged file:
  `crates/zroutery-core/src/agent_takeover/declarations.rs:136`,
  `clippy::needless_bool` in a platform-conditional file check. That file was not
  modified as part of this fix; the lint was not suppressed.
- UI JavaScript bundle: 764139 bytes against a 742000-byte budget. CSS and largest
  chunk are within budget. The budget was not raised.
- Orchestration documentation: self-tests pass, but the repository gate refers
  to missing `ui/src/pages/MlRouting.tsx` and `ui/src/components.tsx`.
- Initial Rust compilation timed out at 120 seconds, then a generated thiserror
  PDB was corrupt (LNK1285). Only that generated PDB was removed; the subsequent
  classifier run passed.

No commits, release, deployment, running-gateway changes or writes to real
Claude/Codex/OpenCode client configurations were performed. Repository changes
already present for plugin/UI work were retained. Native client and Linux/macOS
runtime acceptance are not established by mock/browser tests.
