# Upgrade contracts

Frozen for M0 on 2026-10-30. These contracts implement the agreed revision of
the **Zroutery 升级总计划（三份计划整合）**, not an alternative roadmap. The
source plan is `C:\Users\InfinityNeko\.config\openchamber\projects\path_RTovWnJvdXRlcnk\plans\1793340247479-zroutery.md`.
Implementation and verification are recorded separately in
[upgrade-execution.md](upgrade-execution.md). Unknown facts remain unknown.

## Task architecture and migration

| Door / stable route | Basic task | Existing surface / retained location |
|---|---|---|
| Overview / `overview` | Read readiness, traffic, actionable problems; resume setup | Overview; links to the responsible entity |
| Providers / `providers` | Connect, sign in, manage accounts/keys, prices, balances, plans, check-in, fetch models | Providers plus Accounts as adjacent configuration/observations |
| Models and routing / `models` | Browse by provider, confirm primary tiers, see effective rules and candidates | Models; `routing` maps to its rules tab; compatibility, Auto review and ML remain advanced areas |
| Client setup / `takeover` | Preview/apply four classes, choose default, inspect evidence, restore | Takeover; `cutover` maps to its migration tab |
| Usage and records / `activity` | Filter/query, chart, import/resolve conflicts, inspect failures and budgets | Activity; budget controls move from Settings; logs are available here on demand |
| Settings / `settings` | Appearance, language, window/startup, listener/network, app diagnostics | Settings without duplicated business-rule editors |

`accounts` maps to `providers/accounts`; legacy `models`, `routing`, `takeover`
and `cutover` links retain their sub-area. Alt+1..6 follows the six doors.
Navigation records the sub-area and restores focus to its heading, not merely
to the first page. A model's visible name is trimmed `display_name`, otherwise
the exact upstream name. The provider name disambiguates it. Search also sees
technical IDs/aliases; IDs and copying live in details. Editing a display name
never edits the upstream ID or stable references.

First-run steps are provider → models/four tiers → client setup → observed
mapping. Each has saved input, missing conditions and a next action. It can be
skipped/reopened; an HTTP health probe is not proof of a client model choice or
an upstream task success.

State words are independent: configured, enabled, verified, currently
routable, partial data, expired and needs user action. A failed observation
retains the last valid observation's timestamp and reports the latest attempt
separately. Unknown, uncovered and measured-zero are different states.

## Ownership and versions

| Contract | Owner / version | Compatibility and authority |
|---|---|---|
| Configuration | Core config, additive fields with serde defaults | Legacy single `tier`/global routing stays valid. Identity changes are explicit mutations, never display edits. |
| Account binding | Core declaration + durable binding journal, v1 | `(provider, local account, generation, credential revision)`; generation never reused after delete/recreate. Remote user ID/group are verified evidence, not local account IDs. |
| Effective rate | Core billing, v1 | `(provider, account, binding version, inference key, upstream model, API surface, effective group, source, validity)`; manual overrides survive refresh. |
| Settlement | Core runtime SQLite, schema-versioned migration | One request terminal plus stable attempt IDs, immutable identity/rate snapshots, known cost per currency/unit and missing-cost state. Budget is a projection of the same committed facts. |
| Imported history | Core history, v1 | Independent request/daily/session facts, source instance/entity/revision, coverage and correlation. Never invent status, latency or attempt counts. No budget/health/training side effects. |
| Client catalogue | Core takeover, v1 | Stable class IDs, common capability evidence and generation fingerprint; owned files and managed fields have CAS preview/apply/restore. |
| ML input | ML consumes Core-owned decision-time facts | A change in feature meaning increments the feature schema. Incompatible traces/models are refused or use baseline; import does not synthesize training inputs. |

Secrets live only in the system credential store. An account panel token is
never a provider inference key. Each new/updated account gets its own staging
reference; no update overwrites a shared legacy entry. The binding journal
contains references and evidence, never token bytes or browser session data.

### Credential publication and recovery

1. Serialize mutations against configuration saves; capture current binding CAS.
2. Create an independent staging keychain entry and durable pending journal.
3. Verify the panel identity through the account's own URL/token/header pairing.
   Network/WAF/429 failures mean pending verification, not identity rejection.
4. A different verified remote identity requires explicit confirmation. Preserve
   the old identity version for all historical requests and rates.
5. Save the new configuration/binding version before publishing runtime state.
   On save failure, compensate staging or retain a recoverable pending journal;
   never report fully bound. A stale CAS cannot publish.
6. Recover a pending journal by comparing the saved reference/version. Delete an
   orphan only when **all** remaining provider/account/key references permit it.
   Cached probes, profile/session results and check-in publication are versioned.

### Settlement and failures

SQLite uses WAL, `synchronous=FULL`, bounded busy timeout and transactional
`user_version` migrations. Refuse newer schemas before writing; retain legacy
files. An attachment/migration failure is visible and cannot silently switch a
production desktop instance to volatile accounting. Tests may explicitly use
memory-only Core state.

An attempt is recorded before moving to another attempt; observed failed or
cancelled usage is preserved. Terminal acknowledgement waits for the request,
attempt charges and corresponding budget projection to commit together. No
local disk failure retries an already executed upstream/tool action. Report a
local persistence failure, retain its pending evidence for retry, and deny
budget-dependent admission while accounting is unresolved. Cancellation drops
permits; accounting does not depend on the client continuing to read.

`spend.json` is imported once as an **opening ledger baseline**, with its exact
source digest and activation watermark in the same transaction. Existing usage
is historical and is not replayed onto that opening balance. Reopening or
clearing/importing usage cannot re-import or reset the baseline. New settlement
facts advance the ledger after that watermark. Legacy aggregate spend is not
rewritten into fictional request events. Unobserved remote charges after a
hard kill are pending reconciliation, not recoverable known costs.

Standby is a development handoff, not multi-active budget admission. Shared
SQLite supplies accounting/binding versions; the active request admission
permits remain process-scoped. Old binaries must not write a newer schema.

## Routing decision table

The model remains the candidate axis; executable credentials and their quotes
are bound **before cost decisions**, not multiplied into fake ML models.

| Order | Authority | Rule |
|---|---|---|
| 1 | Client resolution | Stable class/explicit alias or exact model. Naming style changes presentation, not class identity. |
| 2 | Effective shared rule | Global defaults → per-tier field override → explicitly matched advanced policy fields. Restore-inheritance clears an override. Record source and final values. |
| 3 | Hard eligibility | Provider/model enabled, original allowed route range, exact request capability/effort, binding, wallet and budget; preferences never relax these. |
| 4 | Executable quotes | Snapshot available account/key options and effective rates; manual fallback is explicitly estimated. Unknown rate with no fallback cannot enter cost ordering/budget-dependent paths. |
| 5 | Final base order | Priority uses manual model/key order. Explicit policy preference owns its scored order; do not silently re-sort it by a global strategy. Without a policy preference, the effective tier/global strategy owns ordering. Election only serves Balanced, with its evidence/version invalidated on affected changes. |
| 6 | ML | Only an explicitly enabled compatible model may reorder eligible model candidates, using the same decision-time executable quote. Otherwise retain baseline. |
| 7 | Execution | Choose the quote's credential, check its current binding/wallet/availability, enforce actual send cap, bind the immutable attempt snapshot. Fallback stays in the original allowed route range and rechecks full effort support. |

Four primary **usage tiers** are Fast, Standard, Reasoning, Frontier; they do
not set effort. Suggestions only use explicit catalogue evidence, old
configuration/import mappings or declared rules, show their reason/source and
require batch confirmation. Missing or conflicting evidence stays pending;
re-import never silently overwrites a manual tier. A model has one primary
tier; escalation/degradation is an explicit rule with recorded origin/reason.

Reasoning distinguishes unspecified, disabled, native effort (`none` is not
`minimal`) and numeric budget. Advertise only a provider/model/API-surface
specific declared set, never a cross-relay union. Preserve requested effort
and filter the full set, including gaps; no silent clamp/drop. Unknown native
effort may pass unchanged only on an exact same-protocol direct request; this
is not capability evidence and cannot advertise a variant or guess a bridge.

Rate evidence distinguishes public catalogue, verified effective rate, manual
fallback, pre-request estimate and source-reported charge/reconciliation. Token,
per-request and supported special rates are different shapes. Missing cache
rates cannot prove free cache writes. Money/credits/subscription allowance and
cash cost are independent units; no currency conversion is implicit.

## Four-class client contract

| Stable ID | Visible name | Claude native alias |
|---|---|---|
| `fast-class` | Zroutery · 快速 | Haiku |
| `standard-class` | Zroutery · 标准 | Sonnet |
| `reasoning-class` | Zroutery · 推理 | Opus |
| `frontier-class` | Zroutery · 前沿 | Fable |

Preview all four slots; empty/uncertain slots report why they are unavailable
and cannot become an arbitrary fallback model. Publish the actual supported
intersection of participating members' effort/tools/media/context limits,
with provenance and a generation fingerprint, not a fixed account price.

- Claude Code ≥2.1.242: user `modelPicker`, explicit supported alias mappings
  and selected default; restore every new managed field.
- Codex: owned JSON catalogue plus `model_catalog_json`, provider and optional
  default `model`; catalogue changes require restart, not thread hot reload.
- OpenCode V2: existing `providers.zroutery.models` path, accurate variants;
  update V1 only when a legacy block already exists. Project configuration and
  existing session selection may override the default and are not overwritten.

Evidence levels stay separate: written configuration, effective menu/catalogue,
reachable gateway, observed authenticated client request **for the selected
class**. Record actual CLI/IDE/shell entry and version. Owned catalogue files
participate in atomic write, fingerprint CAS and recovery/restore. A user-owned
catalogue or manually changed field is a visible conflict, not silently lost.

## History and query contract

Source detection uses table columns/record shape, not just client version.
CCSwitch v21 request details and daily rollups, Claude assistant message
updates, Codex rollout cumulative token events/session summaries, OpenCode
V1/V2 migrations have separate adapters and minimal redacted fixtures.

The stable key is `(source kind, source instance, entity kind, entity ID)`;
updates advance a revision/watermark and replace that fact, not `INSERT OR
IGNORE` it forever. Strong request/message/migration IDs prove duplicates.
Near time/model/token coincidences are only **suspected** duplicates, preserved
outside accurate totals until resolved. Rollup/detail coverage is explicit and
never summed twice; a rollup cannot expand into made-up request rows.

Source token semantics are identified before normalizing to cache-inclusive
input and reasoning-inclusive output. CCSwitch 0/1/2 are not guessed. Raw
accounting values and decimal monetary strings survive with unit/provenance;
untrusted zero cost remains unknown, not verified free. Partial last lines,
rotation, truncation, resets, overflow and late completion are resumable cases.

Manual flow: source/range → bounded read-only snapshot → staged preview →
mapping/conflict resolution → atomic publish → added/updated/deduplicated/failed
report. Cursor advances only with commit. No giant text-box/IPC archive, no
background importer. Cancellation/validation failure publishes nothing.

Index/query time, client, provider/model, account/key, source and completeness.
Use stable cursor pagination and revision/delta reads; modifications/deletions
advance revision. List, charts and export use one filter/window/timezone/unit.
All three chart styles render the same window with measured quiet, uncovered
and unknown states and visible query errors. Clearing a batch/client/history
never clears actual spend, wallet or health.

## Verification slices and explicit gates

1. M2a + account: schema/migration, one terminal/attempt idempotency,
   synchronous durable evidence, force-kill/reopen, interrupted-save recovery,
   two distinct mock identities and stale probe CAS.
2. Model/routing: different group/key rates, unknown/caches/per-call/currencies,
   priority vs cost execution, native effort gaps/none/minimal/direct unknown,
   readable names and confirmed tiers/overrides.
3. Clients: four-class/default/capability diff, owned-file CAS, restore,
   upgrades/empty/shrinking sets; native menu and observed request tests separate
   from browser mock IPC.
4. Usage: all source adapters, same-source updates, proven/suspected duplicates,
   rollup coverage, interrupted imports, indexed query and matching charts.
5. Windows: self-contained Tauri `custom-protocol` build, five user tasks at
   820×560/long names/600 models/500 activities/30 accounts, keyboard/focus/save
   races, bounded background IPC; NSIS/MSI install/reinstall/uninstall per batch.

Run `scripts/toolchain_gate.py` first. Rust gates use its resolved toolchain,
plus `cargo +1.90.0 check --workspace --locked --all-targets`; fmt, all-target
workspace clippy, workspace/all-feature/no-default behavior tests and manifest
feature matrix remain independent claims. UI tests/build, bundle budget,
layout/interaction/overflow, mock-upstream smoke, command ACL/registration,
orchestration and mutation self-tests keep their existing gates.

Measure before optimizing: local-only 50/200 QPS, 10k-token realtime vs Codex
staged SSE and 1k burst, latency percentiles/CPU/memory/locks/durable wait and
ledger invariants; UI data sizes above, queries/import memory, transferred fonts.
Historical targets are not measurements. No HTTP/2/lock swap simply to meet a
number. Coding Plan brands/platform versions, genuine AgentRouter identity/
rate/effort evidence, a real Codex usage positive, native menus, public signing
and ML small-production platform/model/budget/period are still explicit external
gates. Do not guess, send paid probes, enable training/exploration or promote.
