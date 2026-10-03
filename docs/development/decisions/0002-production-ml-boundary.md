# ADR-0002: Production ML Boundary

## Decision state

**Superseded by ADR-0006.** The boundary this record provisions no longer
exists. Kept because the reasoning it recorded is the reasoning ADR-0006 had to
answer, and a reader who has only seen the outcome deserves to see the question.

## What this decided, and why it was wrong

The boundary was: ML is an optional in-process library, shadow evaluation is
record-only, and "automatic activation, online RL, exploration, or takeover"
must not be enabled in the production Tauri path.

The reasoning was sound. An unproven model must not be able to route a real
request, and there was no accepted commit, no activation path, no rollback proof
and no stable traffic window to judge one by.

The remedy was wrong. A prohibition does not reduce the risk of an unproven
model; it removes the only thing that could. With ML fenced out of production,
the system could never accumulate the routing evidence that would make the risk
manageable, so the risk stayed exactly where it was while a very large amount of
infrastructure was built to keep the model away. The repository recorded this
honestly — `7F PARTIAL`, "blocked on VOLUME", a refusal citing twelve decisions
against a floor of thirty — without noticing that the volume was zero because
the collector's output was never made durable and no binary could read it.

Two artefacts made the fence self-enforcing, which is what turned a provisional
boundary into a permanent one:

- `scripts/desktop_artifact_test.py` failed the build if the shipped desktop
  executable contained any ML symbol.
- `tests/dataset_production_test.rs` and `tests/real_request_shadow_test.rs`
  asserted that the serving path must never call a training entry point, and
  that `router.rs` must not contain the string `dataset`.

A gate that proves a thing cannot happen is only worth its cost until the thing
becomes the requirement. At that point it is not a safety mechanism; it is the
thing standing in the way, and it will resist with exactly the rigour it was
built with.

## Where that reasoning was right, and was kept

The safety concern was real, so it became mechanism rather than prohibition.
`ml::serving` holds what the fence was standing in for:

- a model serves only after a `PromotionGate` promoted it, and the promotion's
  digest is stored beside it;
- every ranking is recomputed from immutable decision-time facts through the
  same `DecisionEngine` that decides everything else;
- any fault, missing model or selection outside the executable plan falls back
  to the deterministic plan the router already computed;
- promotion and rollback are durable and audited;
- `ml_routing.enabled` defaults to off, so an installation that has never
  promoted a model routes exactly as it did before.

## Consequences

- Core remains authoritative for policy, eligibility, failure and served
  identity. ML chooses among candidates those filters already admitted; it
  cannot widen them.
- A model's influence is recorded on the routing decision itself
  (`RouteDecision::ml_ranking`), not only in a log line.
- The learned model's usefulness is an empirical question with a defined
  evidence floor, answered by `ml::comparison` and gated by `ml::promotion`. It
  is not answered by the presence of infrastructure.

## Related records

- `docs/development/decisions/0006-ml-closed-loop.md`
- `docs/development/architecture.md`
- `docs/development/roadmap.md`
- `docs/development/ml-closed-loop-report.md`
