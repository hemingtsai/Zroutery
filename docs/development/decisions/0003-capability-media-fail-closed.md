# ADR-0003: Core Capability and Media Fail-Closed Boundary

## Decision state

Accepted for the bounded Core P1 repair batch. This ADR does not activate ML or
change the production routing architecture.

## Context

The audit found incomplete capability derivation, silent unknown-content loss
in protocol decoders, and a soft router fallback that can reinsert candidates
without proving that request-derived requirements are satisfied. A capability
value of `Unknown` is not evidence of support.

## Decision

1. Capabilities derived from the request are hard Core eligibility inputs.
   Policy fallback may select a documented remediation or transform before
   planning, but it may not silently discard the requirement or reinsert a
   candidate that fails it.
2. The canonical derivation must cover every supported request content form,
   including documents, files, image-bearing tool results, audio, video,
   tools, and thinking. It must be deterministic and deduplicated.
3. Unknown or unsupported content/media is rejected by default with an
   explicit, non-sensitive error. It is never silently dropped.
4. A transform is successful only when it returns an explicit replacement. A
   `Drop` result is allowed only through an explicit, auditable policy and
   must be observable as a capability/rejection decision.
5. Provider capability `Unknown` is not equivalent to supported. Any soft
   fallback must mark the candidate as degraded/unknown and remain subject to
   the Core acceptance policy; it cannot be described as a capability pass.
6. Capability decisions must be represented in the canonical route decision
   and rejection evidence so later ML observation can consume facts without
   changing Core authority.

## Consequences

- Protocol and media decoders gain fail-closed tests for unknown blocks.
- Router eligibility cannot use a request-derived capability argument with
  filtering disabled.
- Stage 2 must revalidate media, capability, and direct-ID behavior before the
  eligibility/pipeline nodes are accepted.
- No provider execution, ML training, UI/Tauri, or real-provider E2E is part
  of this decision.

## Required gates

- Document and tool-result-image requirement derivation.
- Unknown Anthropic/OpenAI/Responses/Gemini content rejection.
- Explicit transform/drop observability tests.
- Existing protocol golden, media, vision, and Core all-feature tests remain
  green.
- Full Core clippy and `git diff --check`.

## Related records

- `docs/development/repository-audit-2026-09-25.md`
- `docs/development/node-status/core-p1-media-req.status.json`
- `docs/development/dependency-dag.md`
- `docs/development/regression-ledger.md`
