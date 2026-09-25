# ADR-0002: Production ML Boundary

## Decision state

Unresolved. The current repository has a safe provisional boundary, but the
future deployment shape of online learning is not accepted policy.

## Context

The Core crate is the production authority and is used by the Tauri shell.
ML is an optional feature in `crates/zroutery-core/Cargo.toml`; the Tauri
dependency does not enable it. Shadow evaluation is evidence-only and the
current audit records `7E-0 FAILED`, `7E-1 PARTIAL`, and `7E-2A BLOCKED`.
There is no accepted commit, atomic activation path, packaging gate, stable
real-traffic shadow window, takeover budget, or rollback proof.

## Options

### Option A — Keep ML as an optional in-process library

- **Advantages:** preserves the current Core-authoritative dependency direction;
  reuses existing feature/schema types; minimizes deployment changes.
- **Risks:** an eventual default feature or desktop enablement could blur the
  safety boundary; activation and resource isolation remain in-process concerns.

### Option B — Move online learning to a separately owned sidecar

- **Advantages:** isolates model state, durable journals, resource budgets, and
  rollback; makes read-only Core integration explicit.
- **Risks:** introduces IPC, version negotiation, availability, packaging, and
  operational ownership requirements that are not currently evidenced.

### Option C — Expose a read-only evidence service first

- **Advantages:** permits observability and offline evaluation without changing
  production routing; supports the current `OBSERVABILITY` `READY` track.
- **Risks:** does not itself close activation, takeover, or exploration gates;
  users may expect online behavior before the safety path is complete.

## Provisional boundary

Until this ADR is resolved, adopt Option A only as a library/test boundary and
Option C only as a possible read-only projection. Do not enable automatic
activation, online RL, exploration, or takeover in the production Tauri path.
A future decision must identify the exact IPC or feature contract, rollback
owner, packaging artifact, observability signals, and evidence gates.

## Consequences

- Core remains authoritative for policy, eligibility, failure, and served
  identity.
- ML may consume immutable decision-time facts and verified commits but cannot
  silently override them.
- A sidecar is a future architecture option, not a current implementation claim.
- The critical path remains 7E-1A → 7E-1B → 7E-2A → … → 7E-3 → 7F → 7G → 7H.

## Related records

- `docs/development/architecture.md`
- `docs/development/roadmap.md`
- `docs/development/dependency-dag.md`
- `docs/development/evidence-registry.md`
