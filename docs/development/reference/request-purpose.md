# Request Purpose Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

A request has two independent dimensions: which model it asks for
(`Resolution`) and why it exists (`RequestKind`). This page covers the second.
It does not cover model resolution; see [routing-policy.md](routing-policy.md).

## Kinds

`RequestKind` is `Main` or `Side(SideQueryKind)`
(`crates/zroutery-core/src/query.rs::RequestKind`):

| Kind | Stable name | What it is |
|---|---|---|
| `Main` | `main` | the client's conversation |
| `Side(AutoMode)` | `auto_mode` | Claude Code's permission classifier |
| `Side(CodexGuardian)` | `codex_guardian` | Codex Guardian auto-review |
| `Side(CodexGuardianRisk)` | `codex_guardian_risk` | Codex Guardian risk scorer |

`SideQueryKind` also records the verdict contract: `AutoMode` uses
`<block>yes|no</block>`, `CodexGuardian` uses JSON, and `CodexGuardianRisk`
answers a bare `low`/`high` token (low allows, high blocks; anything else is
unparseable and is spoken as high)
(`crates/zroutery-core/src/query.rs::SideQueryKind`,
`crates/zroutery-core/src/query.rs::SideQueryKind::uses_block_verdict`,
`crates/zroutery-core/src/classifier.rs::codex_guardian_kind`).

The kind is a routing intent, not a field on the protocol IR. The same model id
can arrive as a main request or a classifier request; the model id's `[1m]`
modifier can only nudge an existing match, never carry one
(`crates/zroutery-core/src/classifier.rs::detect`).

## Detection

`detect(headers, body, config)` is the single entry point
(`crates/zroutery-core/src/classifier.rs::detect`). It is pure: it does not
mutate the request, and a detection failure is not an error; the request is
treated as `Main`.

Detection has three layers, in order of trust
(`crates/zroutery-core/src/classifier.rs::Detection`):

1. an explicit `anthropic-beta` marker (`auto-mode-classifier`);
2. built-in structural signatures;
3. operator-configured fingerprints.

Two shapes short-circuit that order: `CodexGuardian` is detected by the Responses
policy before structural scoring
(`crates/zroutery-core/src/classifier.rs::codex_guardian_kind`), and the Auto
Mode built-ins require the "security monitor for autonomous AI coding agents"
system prompt plus a verdict contract, not only a parameter shape; the
64/4,096-token signatures are the second level, not the whole decision
(`crates/zroutery-core/src/classifier.rs::DetectionConfig::builtin_signatures`).

A signature matches only when every present field matches
(`crates/zroutery-core/src/classifier.rs::ClassifierSignature`):

- `max_tokens` or `max_output_tokens`;
- `temperature`;
- `stop_sequence`;
- `system_contains` substrings, all required.

A mismatch on any present field scores zero; a signature is not a weak match for
a different request shape. More checks score higher; parameters alone cannot
cross the default confidence.

`DetectionConfig` defaults to enabled, minimum confidence `0.85`, all built-ins
on, and no custom signatures
(`crates/zroutery-core/src/classifier.rs::DetectionConfig`). The built-in
signatures are the known Auto Mode stages: 64 tokens / 0.0 temperature /
`</block>` stop, and the 4,096-token follow-up
(`crates/zroutery-core/src/classifier.rs::DetectionConfig::builtin_signatures`).

A request carrying `safeguards` is always `Main`: embedded server review belongs
to the conversation, and a beta header must not redirect it into the paid
side-query pool (`crates/zroutery-core/src/classifier.rs::detect`).

## Verdicts

An unparseable answer is **not** an Allow. The gate is fail-closed.

- `parse_verdict` scans for a `<block>`, `<block yes>` or `<block no>` tag pair
  and accepts only an exact `no` (allow) or `yes` (block) between the tags;
  surrounding prose is ignored and never mapped to a verdict
  (`crates/zroutery-core/src/classifier.rs::parse_verdict`).
- `parse_verdict_for` dispatches to the JSON contract for Guardian: only
  `{"outcome":"allow"}` or `{"outcome":"deny"}` count
  (`crates/zroutery-core/src/classifier.rs::parse_verdict_for`).
- `ClassifierVerdict` is `Allow`, `Block` or `Unparseable`; the last one never
  approves (`crates/zroutery-core/src/classifier.rs::ClassifierVerdict`).
- A declared `</severity>` contract is answered with `0` or `100`, not a
  fabricated probability (`crates/zroutery-core/src/classifier.rs::decision_text_for_request`).
- A decision model's probability is thresholded; the uncertain band is
  `Unparseable`, not Allow
  (`crates/zroutery-core/src/classifier.rs::verdict_from_decision`).

## Routing

`ClassifierConfig` has its own pool, strategy, failover switch and attempt cap
(`crates/zroutery-core/src/config/routing.rs::ClassifierConfig`):

- `enabled` is off by default; when off every request is `Main`;
- `max_attempts` defaults to 2;
- `candidates` is the shared review pool; the strategy orders it;
- `detection` is the `DetectionConfig` above.

The classifier pool is shared by Auto Mode, Guardian and the risk scorer: they are the same
question ("may this action run without asking?") in different protocols. The
verdict parser is told which question was asked, so a `<block>` tag in a
Guardian answer is not a verdict.

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| a main request is misread as a side query | false positive | lower confidence; the threshold is 0.85 (`crates/zroutery-core/src/classifier.rs::detect`) |
| a side query stays main | false negative | add a configured signature; detection failure is not an error |
| the verdict is prose | unparseable | fail closed (`crates/zroutery-core/src/classifier.rs::parse_verdict`) |

## Not implemented

- No detection by model name alone; the `[1m]` modifier can only nudge an
  existing structural match.
- No natural-language verdict mapping; prose is unparseable and closes the gate.
- No silent approval when detection is uncertain.

## Sources

- `crates/zroutery-core/src/query.rs::RequestKind`
- `crates/zroutery-core/src/query.rs::SideQueryKind`
- `crates/zroutery-core/src/query.rs::SideQueryKind::uses_block_verdict`
- `crates/zroutery-core/src/query.rs::SideQueryKind::CodexGuardianRisk`
- `crates/zroutery-core/src/classifier.rs::codex_guardian_kind`
- `crates/zroutery-core/src/classifier.rs::detect`
- `crates/zroutery-core/src/classifier.rs::Detection`
- `crates/zroutery-core/src/classifier.rs::ClassifierSignature`
- `crates/zroutery-core/src/classifier.rs::DetectionConfig`
- `crates/zroutery-core/src/classifier.rs::DetectionConfig::builtin_signatures`
- `crates/zroutery-core/src/classifier.rs::ClassifierVerdict`
- `crates/zroutery-core/src/classifier.rs::parse_verdict`
- `crates/zroutery-core/src/classifier.rs::parse_verdict_for`
- `crates/zroutery-core/src/classifier.rs::decision_text_for_request`
- `crates/zroutery-core/src/classifier.rs::verdict_from_decision`
- `crates/zroutery-core/src/config/routing.rs::ClassifierConfig`
