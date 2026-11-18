# Routing and Policy Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-15 · Verified by: `cargo test --workspace --all-features --locked`

This page describes how a requested model becomes an ordered candidate plan:
resolution, policy, eligibility, the strategy and the circuit breaker. It does
not describe accounting; see [accounting.md](accounting.md).

## Resolution

A request names either a direct model id or a virtual tier:

- `Resolution::Direct(id)` resolves one configured entry;
- `Resolution::Tier(tier)` resolves the virtual `*-class` id to a tier
  (`crates/zroutery-core/src/registry.rs::Resolution`).

The registry's `resolve` method owns the mapping, including aliases and custom tier names
(`crates/zroutery-core/src/registry.rs::Registry::resolve`). A direct id does not
bypass capability or provider checks: it is planned through the same hard gate as
a tier, with one exception — a direct same-dialect request with an explicit
effort level drops only the automatic thinking requirement, never the media or
tool gates (`crates/zroutery-core/src/router/plan.rs::plan`,
`crates/zroutery-core/src/server/pipeline/effort.rs::direct_requirements`).

## Policy

A policy can narrow and order what a client is allowed to reach:

- `RoutingPolicy` carries matchers, preferences and requirements
  (`crates/zroutery-core/src/policy/policy.rs::RoutingPolicy`);
- an `EligibilityCheck` records why a candidate was rejected
  (`crates/zroutery-core/src/policy/eligibility.rs::EligibilityCheck`);
- a candidate decision records one candidate's eligibility, rejection reason and
  score (`crates/zroutery-core/src/policy/decision.rs::CandidateDecision`); the
  selected candidate and the planned identity live on the route decision
  (`crates/zroutery-core/src/policy/decision.rs::RouteDecision`).

Policy is a filter and an ordering input. It cannot reintroduce a candidate that
the capability or provider gate rejected.

## The plan

The router builds the ordered candidates: `plan` for a bare plan,
`plan_with_policy` and `plan_with_policy_order` when a policy applies
(`crates/zroutery-core/src/router/plan.rs::plan`,
`crates/zroutery-core/src/router/plan.rs::plan_with_policy`,
`crates/zroutery-core/src/router/mod.rs::Router`):

1. resolve the requested model;
2. reject a disabled provider;
3. apply the hard capability gate (tools, vision, thinking);
4. apply the policy's requirements, preferences and ordering
   (`crates/zroutery-core/src/router/plan.rs::plan_with_policy`);
5. drop candidates whose breaker refuses;
6. order the survivors by the configured strategy.

The pipeline passes the capabilities it derived from the request
(`crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`), so a direct id
and a tier see the same hard gate, except that a direct same-dialect request
with an explicit effort level drops only the automatic thinking requirement
(`crates/zroutery-core/src/server/pipeline/effort.rs::direct_requirements`;
media and tool requirements are unchanged).

## Strategies

`RoutingStrategy` defines the order
(`crates/zroutery-core/src/config/routing.rs::RoutingStrategy`):

| Strategy | Order |
|---|---|
| `Priority` | priority order; equal priorities are broken by exposed id, so the order is a function of the configuration alone (`crates/zroutery-core/src/router/helpers.rs::by_priority`) |
| `WeightedRandom` | weighted across healthy candidates |
| `RoundRobin` | rotation across healthy candidates |
| `LowestLatency` | lowest observed latency first |
| `Balanced` | the order an election decided from measured latency and price |

`Balanced` falls back to `Priority` until an election has been held. An election
scores measured latency and price with `ScoringConfig` and pins the resulting
order (`crates/zroutery-core/src/election.rs::Election`,
`crates/zroutery-core/src/election.rs::ScoringConfig`).

## Circuit breaker

Each model has a breaker with three states
(`crates/zroutery-core/src/circuit_breaker.rs::CircuitBreaker`,
`crates/zroutery-core/src/circuit_breaker.rs::CircuitState`):

- closed: candidates are eligible;
- open: the candidate is skipped until the cooldown expires;
- half-open: a probe is allowed; `can_probe` decides when
  (`crates/zroutery-core/src/circuit_breaker.rs::CircuitBreaker::can_probe`).

A breaker is runtime state: it starts closed on restart. The spend ledger is the
exception; it is durable.

## Failover

Messages-native requirements filter the request-local registry before scoring;
direct IDs cannot escape the protocol bridge gate. Server-only contexts keep
their native route instead of acquiring a lossy fallback
(`crates/zroutery-core/src/server/pipeline/anthropic_native.rs::validate_scope`,
`crates/zroutery-core/src/server/pipeline/anthropic_native.rs::registry_for`).
Thread continuation narrows to the original model/key before attempt capping,
so a priority change cannot hide its valid credential or switch its server
context (`crates/zroutery-core/src/server/pipeline/session_binding.rs::UpstreamSessions::thread_registry`,
`crates/zroutery-core/src/server/pipeline/session_binding.rs::UpstreamSessions::thread_plan`).
Embedded auto review uses the configured purpose pool and a separate
lifecycle, never the generator's tier selection or an invented permission
(`crates/zroutery-core/src/server/pipeline/safeguards/reviewer.rs::ReviewContext::classify`).

The plan is the failover order. An attempt fails, the failure is classified, and
the loop decides whether to retry the same candidate or move to the next one;
the order comes from the router, not from the caller. An empty candidate set is
the `NoCandidate` error, never an empty plan served by a rejected fallback
(`crates/zroutery-core/src/router/selection.rs::plan_candidates`). The weighted
shuffle that orders those candidates uses the random-range API
(`crates/zroutery-core/src/router/selection.rs::weighted_shuffle`).

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| the plan is empty | every candidate rejected | return no candidate; never invent one (`crates/zroutery-core/src/router/plan.rs::plan`) |
| a cooling model is skipped | breaker open | wait for the cooldown or probe (`crates/zroutery-core/src/circuit_breaker.rs::CircuitState`) |
| the elected order is unavailable | no election held | fall back to priority (`crates/zroutery-core/src/election.rs::Election`) |

## Not implemented

- No speculative execution across candidates; one attempt at a time.
- No global load balancer across instances.
- No policy that can widen the hard capability gate.

## Sources

- `crates/zroutery-core/src/registry.rs::Resolution`
- `crates/zroutery-core/src/registry.rs::Registry::resolve`
- `crates/zroutery-core/src/router/plan.rs::plan`
- `crates/zroutery-core/src/router/mod.rs::Router`
- `crates/zroutery-core/src/policy/policy.rs::RoutingPolicy`
- `crates/zroutery-core/src/policy/eligibility.rs::EligibilityCheck`
- `crates/zroutery-core/src/policy/decision.rs::CandidateDecision`
- `crates/zroutery-core/src/router/selection.rs::plan_candidates`
- `crates/zroutery-core/src/router/helpers.rs::by_priority`
- `crates/zroutery-core/src/router/plan.rs::plan_with_policy`
- `crates/zroutery-core/src/policy/decision.rs::CandidateDecision`
- `crates/zroutery-core/src/policy/decision.rs::RouteDecision`
- `crates/zroutery-core/src/config/routing.rs::RoutingStrategy`
- `crates/zroutery-core/src/election.rs::Election`
- `crates/zroutery-core/src/election.rs::ScoringConfig`
- `crates/zroutery-core/src/circuit_breaker.rs::CircuitBreaker`
- `crates/zroutery-core/src/circuit_breaker.rs::CircuitState`
- `crates/zroutery-core/src/circuit_breaker.rs::CircuitBreaker::can_probe`
- `crates/zroutery-core/src/server/pipeline/mod.rs::handle_chat`
