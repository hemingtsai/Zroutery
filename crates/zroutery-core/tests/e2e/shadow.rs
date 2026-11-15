use serde_json::{json, Value};

use zroutery_core::billing::Pricing;

use super::support::*;

// ------------------------------------------------------- shadow evaluation (ml)
//
// Stage 7E-1: the ML stack records what it *would have done* for policy-routed
// main traffic. Shadow is record-only by construction, so every test here also
// proves a negative: that production behaves identically with it on.

#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_decisions_record_only_for_policy_routed_requests() {
    use zroutery_core::ml::{ShadowScope, FEATURE_SCHEMA_VERSION};

    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.shadow.enabled = true;
    let h = Harness::start(cfg, mock).await;

    // Policy-routed: the tier virtual id resolves through the policy engine.
    let resp = post_policy_routed(&h, false, 0).await;
    assert_eq!(resp.status(), 200);

    let store = h.state.shadow().store();
    assert_eq!(
        store.len(),
        1,
        "one shadow decision per policy-routed request"
    );
    let shadow = &store.decisions()[0];

    // Correlated with the request record: same request id, same decision id.
    let record = h.state.stats().recent(10).remove(0);
    assert_eq!(shadow.actual.request_id, record.id);
    let decision = record
        .routing_decision
        .as_ref()
        .expect("policy-routed request record carries its routing decision");
    assert_eq!(shadow.actual.decision_id, decision.decision_id);

    // The record itself is well-formed evidence.
    assert_eq!(shadow.scope, ShadowScope::PolicyRouted);
    assert!(shadow.shadow_id.starts_with("shadow-"));
    assert_eq!(shadow.shadow.feature_schema, FEATURE_SCHEMA_VERSION);
    assert_eq!(shadow.actual.selected, "deepseek-deepseek-v4-pro");
    // Single-candidate plan: the engine sees the current candidate first, so
    // its hypothetical verdict keeps production's choice.
    assert_eq!(shadow.shadow.selected, "deepseek-deepseek-v4-pro");
    assert!(!shadow.candidates.is_empty());
    assert_eq!(
        shadow.candidates[0].candidate_id,
        "deepseek-deepseek-v4-pro"
    );
    assert!(shadow.candidates[0].eligible);
    assert_eq!(h.state.shadow().fault_count(), 0);

    // Direct resolution (exact model id) is out of shadow scope.
    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "anthropic-claude-native",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        store.len(),
        1,
        "direct-resolution requests are not recorded"
    );

    h.shutdown().await;
}

/// A/B purity: shadow on must not change what reaches providers or how
/// requests are counted.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_causes_no_provider_requests() {
    async fn run(shadow: bool) -> (usize, u64, usize) {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        let h = Harness::start(cfg, mock).await;
        for round in 0..3 {
            let resp = post_policy_routed(&h, false, round).await;
            assert_eq!(resp.status(), 200);
        }
        let upstream_calls = h.mock.count();
        let requests = h.state.stats().summary().requests;
        let shadow_records = h.state.shadow().store().len();
        h.shutdown().await;
        (upstream_calls, requests, shadow_records)
    }

    let off = run(false).await;
    let on = run(true).await;
    // Identical upstream traffic and request accounting...
    assert_eq!(off.0, on.0, "upstream call count must not change");
    assert_eq!(off.1, on.1, "request accounting must not change");
    // ...while only the ON side actually recorded shadow decisions.
    assert_eq!(off.2, 0);
    assert_eq!(on.2, 3);
}

/// A/B with expected delta: shadow on must not mutate router health, spend,
/// per-request routing outcomes or the bytes the client receives.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_causes_no_runtime_mutation() {
    use zroutery_core::budget::Ledger;

    /// A request record with the volatile fields masked: what was asked, which
    /// model answered on which provider, ok flag, status, attempt count.
    /// (Ids, timestamps and latencies differ per run by construction.)
    #[derive(Debug, PartialEq)]
    struct MaskedRecord {
        requested_model: String,
        resolved_model: Option<String>,
        provider_name: Option<String>,
        ok: bool,
        status: u16,
        attempts: u32,
    }

    /// Health rows with the timing-derived fields (`avg_latency_ms`,
    /// `cooldown_remaining_secs`) masked: latency is measured wall clock,
    /// so identical scripts still differ by scheduling noise. What must
    /// not diverge is the structural state — breaker state, counters and
    /// the last error.
    #[derive(Debug, PartialEq)]
    struct MaskedHealth {
        model_id: String,
        state: zroutery_core::circuit_breaker::CircuitState,
        consecutive_failures: u32,
        total_success: u64,
        total_failure: u64,
        last_error: Option<String>,
    }

    struct Outcome {
        health: Vec<MaskedHealth>,
        ledger: Ledger,
        records: Vec<MaskedRecord>,
        bodies: Vec<Value>,
    }

    async fn run(shadow: bool) -> Outcome {
        let (addr, mock) = start_mock().await;
        let mut cfg = config_for(addr);
        cfg.shadow.enabled = shadow;
        // Price the model that answers so the ledger comparison is over real
        // spends, not two empty ledgers.
        cfg.models[1].pricing = Some(Pricing::new("USD", 0.5, 2.0));
        let h = Harness::start(cfg, mock).await;

        let mut bodies = Vec::new();
        for round in 0..3 {
            let resp = post_policy_routed(&h, false, round).await;
            assert_eq!(resp.status(), 200);
            bodies.push(resp.json::<Value>().await.unwrap());
        }

        let records = h
            .state
            .stats()
            .recent(10)
            .into_iter()
            .map(|r| MaskedRecord {
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
            .map(|mh| MaskedHealth {
                model_id: mh.model_id,
                state: mh.state,
                consecutive_failures: mh.consecutive_failures,
                total_success: mh.total_success,
                total_failure: mh.total_failure,
                last_error: mh.last_error,
            })
            .collect();
        let outcome = Outcome {
            health,
            ledger: h.state.ledger(),
            records,
            bodies,
        };
        h.shutdown().await;
        outcome
    }

    let off = run(false).await;
    let on = run(true).await;

    // Router health identical: no shadow-driven failures, recoveries or probes.
    assert_eq!(off.health, on.health, "health must not diverge");
    // Identical spend for identical deterministic traffic.
    assert_eq!(off.ledger, on.ledger, "ledger must not diverge");
    // Identical per-request routing outcomes.
    assert_eq!(off.records, on.records, "records must not diverge");
    // Byte-identical answers to the client.
    assert_eq!(off.bodies, on.bodies, "response bodies must not diverge");
    // The SessionStore is not wired into the production pipeline (it has no
    // reader yet), and the shadow path holds no session handle, so it is
    // untouched on both sides by construction.
}

/// Production must be unaffected with shadow on, and a healthy predictor runs
/// fault-free. (Poisoned-predictor isolation is covered at engine level in
/// the ml shadow tests; AppState wires the real predictor.)
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_survives_predictor_faults() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.shadow.enabled = true;
    let h = Harness::start(cfg, mock).await;

    // Two buffered and one streaming policy-routed request, so both evaluate
    // hooks (buffered_chat and stream_chat) run with shadow on.
    let resp = post_policy_routed(&h, false, 0).await;
    assert_eq!(resp.status(), 200);
    let resp = post_policy_routed(&h, false, 1).await;
    assert_eq!(resp.status(), 200);
    let resp = post_policy_routed(&h, true, 2).await;
    assert_eq!(resp.status(), 200);
    // Drain the stream so its record finalizes.
    let _ = resp.text().await.unwrap();

    // Every request succeeded and every one was shadow-evaluated, fault-free.
    assert_eq!(h.state.stats().summary().requests, 3);
    assert_eq!(h.state.stats().summary().failures, 0);
    assert_eq!(h.state.shadow().store().len(), 3);
    assert_eq!(h.state.shadow().fault_count(), 0);

    h.shutdown().await;
}

#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_disabled_by_default() {
    let h = Harness::new().await;
    assert!(!h.state.shadow().enabled());

    // Policy-routed traffic while disabled: nothing recorded, nothing faulted.
    let resp = post_policy_routed(&h, false, 0).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(h.state.shadow().store().len(), 0);
    assert_eq!(h.state.shadow().fault_count(), 0);

    h.shutdown().await;
}

/// GATE 7E-1 (model mutation = 0): production traffic never trains the
/// shadow ensemble. Across a burst of policy-routed requests — buffered and
/// streaming, so both evaluate hooks run — every recorded decision carries
/// one and the same model commit, and that commit is the genesis commit the
/// engine started from. The commit id is derived from the cold-start
/// ensemble's content hash, so any training during the burst would have
/// advanced it.
#[cfg(feature = "ml")]
#[tokio::test]
async fn shadow_model_commit_stable_across_traffic() {
    use zroutery_core::ml::model_identity::CommitId;
    use zroutery_core::ml::ModelEnsemblePredictor;

    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.shadow.enabled = true;
    let h = Harness::start(cfg, mock).await;

    for round in 0..4 {
        let stream = round % 2 == 0;
        let resp = post_policy_routed(&h, stream, round).await;
        assert_eq!(resp.status(), 200);
        if stream {
            // Drain the stream so its record finalizes.
            let _ = resp.text().await.unwrap();
        }
    }

    let decisions = h.state.shadow().store().decisions();
    assert_eq!(decisions.len(), 4, "one shadow decision per request");
    assert_eq!(h.state.shadow().fault_count(), 0);

    // One identical commit across the whole burst: nothing trained mid-flight.
    let observed = decisions[0].shadow.model_commit.clone();
    assert!(
        decisions.iter().all(|d| d.shadow.model_commit == observed),
        "every decision must carry the same model commit"
    );

    // ...and that commit is the one AppState wired at startup, unchanged.
    //
    // This witness used to be `ModelEnsemblePredictor::genesis().commit()`, which
    // was correct only while the server always wired the cold-start root. Node
    // 7F attaches a verified candidate at `AppState::new`, so the invariant this
    // gate exists for — production traffic never trains the ensemble — is now
    // witnessed against whatever was wired at startup: the candidate when one is
    // attached, and genesis when none is.
    //
    // The comparison stays honest because that value is read from the server's
    // configuration, not from the recorded decisions. Attachment is one-way and
    // there is no setter that can attach, so this cannot drift toward the
    // observed value and become a tautology.
    let wired = h
        .state
        .shadow_candidate_commit_id()
        .map_or_else(|| ModelEnsemblePredictor::genesis().commit(), CommitId::new);
    assert_eq!(
        observed, wired,
        "production traffic must not advance the shadow model"
    );

    h.shutdown().await;
}
