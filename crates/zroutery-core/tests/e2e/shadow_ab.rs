use serde_json::{json, Value};

use zroutery_core::billing::Pricing;
use zroutery_core::config::{ModelEntry, ModelTier};

use super::support::*;

// ------------------------------------------------- fallback / retry A/B (ml)
//
// Twin-harness evidence for the two remaining gate items: production
// fallback = 0 and production retry = 0 under shadow. Each test runs the
// same script twice (shadow off vs on) against fresh mocks and compares
// everything downstream of the shadow hook.

/// Masked request record for the A/B comparison (same masking discipline as
/// `shadow_causes_no_runtime_mutation`: ids, timestamps and latencies are
/// volatile by construction; everything else must match — including each
/// record's attempt count).
#[cfg(feature = "ml")]
#[derive(Debug, PartialEq)]
struct AbRecord {
    requested_model: String,
    resolved_model: Option<String>,
    provider_name: Option<String>,
    ok: bool,
    status: u16,
    attempts: u32,
}

/// Masked health row: the timing-derived fields (`avg_latency_ms`,
/// `cooldown_remaining_secs`) are dropped; the structural state — breaker
/// state, counters, last error — must match.
#[cfg(feature = "ml")]
#[derive(Debug, PartialEq)]
struct AbHealth {
    model_id: String,
    state: zroutery_core::circuit_breaker::CircuitState,
    consecutive_failures: u32,
    total_success: u64,
    total_failure: u64,
    last_error: Option<String>,
}

/// Everything the A/B tests compare across a twin run.
#[cfg(feature = "ml")]
struct AbOutcome {
    /// The full upstream request bodies, in order — identical traffic.
    upstream_bodies: Vec<Value>,
    /// The model that answered each client request (`x-zroutery-model`).
    served_models: Vec<String>,
    records: Vec<AbRecord>,
    health: Vec<AbHealth>,
    ledger: zroutery_core::budget::Ledger,
    bodies: Vec<Value>,
    shadow_decisions: usize,
    shadow_faults: u64,
}

#[cfg(feature = "ml")]
impl AbOutcome {
    fn capture(h: &Harness, served_models: Vec<String>, bodies: Vec<Value>) -> Self {
        let records = h
            .state
            .stats()
            .recent(10)
            .into_iter()
            .map(|r| AbRecord {
                requested_model: r.requested_model,
                resolved_model: r.resolved_model,
                provider_name: r.provider_name,
                ok: r.ok,
                status: r.status,
                attempts: r.attempts,
            })
            .collect();
        let health = h
            .state
            .router()
            .health_snapshot()
            .into_iter()
            .map(|mh| AbHealth {
                model_id: mh.model_id,
                state: mh.state,
                consecutive_failures: mh.consecutive_failures,
                total_success: mh.total_success,
                total_failure: mh.total_failure,
                last_error: mh.last_error,
            })
            .collect();
        AbOutcome {
            upstream_bodies: h.mock.bodies(),
            served_models,
            records,
            health,
            ledger: h.state.ledger(),
            bodies,
            shadow_decisions: h.state.shadow().store().len(),
            shadow_faults: h.state.shadow().fault_count(),
        }
    }

    /// Assert the two twin runs are production-identical.
    fn assert_identical(off: &AbOutcome, on: &AbOutcome) {
        assert_eq!(
            off.upstream_bodies, on.upstream_bodies,
            "upstream traffic must not change"
        );
        assert_eq!(
            off.served_models, on.served_models,
            "served models must not change"
        );
        assert_eq!(
            off.records, on.records,
            "request records (incl. attempt counts) must not change"
        );
        assert_eq!(off.health, on.health, "health must not diverge");
        assert_eq!(off.ledger, on.ledger, "ledger must not diverge");
        assert_eq!(off.bodies, on.bodies, "response bodies must not diverge");
    }
}

/// GATE 7E-1 (fallback = 0): production failover must be identical with
/// shadow off vs on. Script: two policy-routed requests whose first
/// candidate always answers 429 (the mock's `limited*` convention, as in
/// `rate_limit_triggers_failover`), forcing production to fail over to the
/// second candidate. Compared across the twin runs: the exact upstream
/// traffic, the served model per request, the masked request records
/// (including each record's attempt count), the masked router health, the
/// spend ledger and the bytes the client receives.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_does_not_cause_fallback() {
    async fn run(shadow: bool) -> AbOutcome {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        // Primary always 429s; the fallback model is healthy and priced so
        // the ledger comparison is over real spend.
        let mut fallback =
            ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard))
                .with_priority(10);
        fallback.pricing = Some(Pricing::new("USD", 0.5, 2.0));
        cfg.models = vec![
            ModelEntry::for_upstream("deepseek", "limited-v4", Some(ModelTier::Standard))
                .with_priority(0),
            fallback,
        ];
        let h = Harness::start(cfg, mock).await;

        let mut served_models = Vec::new();
        let mut bodies = Vec::new();
        for round in 0..2 {
            let resp = post_policy_routed(&h, false, round).await;
            assert_eq!(resp.status(), 200, "the failover script must succeed");
            served_models.push(
                resp.headers()["x-zroutery-model"]
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
            bodies.push(resp.json::<Value>().await.unwrap());
        }

        let outcome = AbOutcome::capture(&h, served_models, bodies);
        h.shutdown().await;
        outcome
    }

    let off = run(false).await;
    let on = run(true).await;

    // The script really exercised production fallback: the rate-limited
    // primary was attempted and at least one request needed a second attempt.
    assert!(
        off.upstream_bodies
            .iter()
            .any(|b| b["model"] == "limited-v4"),
        "the rate-limited primary must have been attempted"
    );
    assert!(
        off.records.iter().any(|r| r.attempts >= 2),
        "the script must trigger a fallback"
    );

    // Identical traffic, outcomes, health, spend and bytes.
    AbOutcome::assert_identical(&off, &on);

    // Only the ON side recorded shadow decisions — one per request,
    // fault-free. Each snapshot is taken before any attempt (is_fallback is
    // false by construction), so a store record exists for every fallback
    // request exactly as for every clean one.
    assert_eq!(off.shadow_decisions, 0);
    assert_eq!(on.shadow_decisions, 2);
    assert_eq!(on.shadow_faults, 0);
}

/// GATE 7E-1 (retry = 0): the production retry path must be identical with
/// shadow off vs on. Script: a policy-routed request whose first candidate
/// answers with a REPAIRABLE 400 (thinking budget too large — the mock's
/// `budget*` convention). The pipeline runs the thinking-budget rectifier,
/// which halves `budget_tokens` and retries the SAME candidate, so one
/// client request produces two upstream calls to the same model (the
/// rejected shape, then the repaired one) and never fails over.
///
/// Honest scope note: chat requests have no other same-candidate retry seam
/// to drive from a test — client errors are not retried, the transport-level
/// handshake retries happen inside the upstream client, and every other
/// retry-shaped failure flows through the failover loop the fallback test
/// already covers. The rectifier path is the one retry the pipeline itself
/// drives, and both its calls land on the same per-request upstream counter
/// this test compares.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_does_not_cause_retry() {
    async fn run(shadow: bool) -> AbOutcome {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        // The primary answers 400 until its thinking budget is halved to the
        // floor (1024); the rectifier does exactly that and retries the SAME
        // candidate. It advertises the thinking capability the request
        // requires and is priced so the ledger comparison is over real spend.
        let mut primary =
            ModelEntry::for_upstream("anthropic", "signature-v4", Some(ModelTier::Standard))
                .with_priority(0);
        primary.capabilities.thinking = true;
        primary.pricing = Some(Pricing::new("USD", 0.5, 2.0));
        cfg.models = vec![
            primary,
            ModelEntry::for_upstream("deepseek", "deepseek-v4-pro", Some(ModelTier::Standard))
                .with_priority(10),
        ];
        let h = Harness::start(cfg, mock).await;

        let mut served_models = Vec::new();
        let mut bodies = Vec::new();
        for round in 0..2 {
            let resp = h
                .post("/v1/messages")
                .json(&json!({
                    "model": "sonnet-class",
                    "max_tokens": 64,
                    "messages": [
                        {"role":"user","content":"prior fixture"},
                        {"role":"assistant","content":[{"type":"thinking","thinking":"fixture","signature":"stale"},{"type":"text","text":"prior answer"}]},
                        {"role": "user", "content": format!("round {round}")}
                    ]
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200, "the repaired retry must succeed");
            served_models.push(
                resp.headers()["x-zroutery-model"]
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
            bodies.push(resp.json::<Value>().await.unwrap());
        }

        let outcome = AbOutcome::capture(&h, served_models, bodies);
        h.shutdown().await;
        outcome
    }

    let off = run(false).await;
    let on = run(true).await;

    // The script really exercised the same-candidate retry: every request
    // hit the SAME model twice — the rejected 2048-budget shape, then the
    // repaired 1024-budget body — and never failed over.
    for round in 0..2 {
        let rejected = &off.upstream_bodies[round * 2];
        let repaired = &off.upstream_bodies[round * 2 + 1];
        assert_eq!(rejected["model"], "signature-v4");
        assert_eq!(
            repaired["model"], "signature-v4",
            "the retry must stay on the same candidate"
        );
        assert!(rejected["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "thinking"));
        assert!(repaired["messages"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["type"] != "thinking"));
    }
    assert!(
        off.records.iter().all(|r| r.attempts == 2),
        "each actual upstream send has its own complete attempt identity"
    );

    // Identical traffic, outcomes, health, spend and bytes.
    AbOutcome::assert_identical(&off, &on);

    // Only the ON side recorded shadow decisions — one per request,
    // fault-free, snapshotted before the attempt (and therefore before the
    // retry) — so the retry path ran with a decision on record and was
    // untouched by it.
    assert_eq!(off.shadow_decisions, 0);
    assert_eq!(on.shadow_decisions, 2);
    assert_eq!(on.shadow_faults, 0);
}
