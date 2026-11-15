use std::time::Duration;

use chrono::Local;

use serde_json::{json, Value};

use zroutery_core::billing::{Cost, Pricing};
use zroutery_core::budget::{Budget, BudgetPeriod, BudgetScope, Ledger};
use zroutery_core::config::ModelTier;

use super::support::*;

#[tokio::test]
async fn a_budget_stops_spending_once_it_is_used_up() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    // The mock reports 11 + 7 tokens, so one request costs 0.018 USD.
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg, mock).await;

    let ask = || {
        h.post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
    };

    // The first request fits, and the one that crosses the line still completes.
    assert_eq!(ask().send().await.unwrap().status(), 200);

    // The next is refused, naming the limit that stopped it.
    let resp = ask().send().await.unwrap();
    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "budget_exceeded");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("today") && message.contains("everything"),
        "{message}"
    );

    // Nothing reached the provider for the refused request.
    assert_eq!(h.mock.count(), 1);
    // A direct call to the same model is stopped too: a global budget is global.
    assert_eq!(
        h.post("/v1/chat/completions")
            .json(&json!({"model": "deepseek-deepseek-v4-pro",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        402
    );

    h.shutdown().await;
}

#[tokio::test]
async fn a_class_budget_degrades_instead_of_refusing() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[2].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Tier {
            tier: ModelTier::Reasoning,
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )
    .degrading_to(ModelTier::Fast)];
    let h = Harness::start(cfg, mock).await;

    // The first opus request goes to opus and spends past the limit.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "opus-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-5.3-sol");

    // The next is served by the cheap class rather than refused.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "opus-class", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn a_class_budget_also_gates_direct_id_requests() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[2].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Tier {
            tier: ModelTier::Reasoning,
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )
    .degrading_to(ModelTier::Fast)];
    let h = Harness::start(cfg, mock).await;

    // The direct call spends past the opus budget…
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "openai-gpt-5.3-sol", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-zroutery-model"], "openai-gpt-5.3-sol");

    // …so the next direct call to the same id degrades to the cheap class
    // rather than spending on: charging already bills direct calls against
    // the model's class, so gating must match.
    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "openai-gpt-5.3-sol", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["x-zroutery-model"],
        "deepseek-deepseek-v4-flash"
    );

    h.shutdown().await;
}

#[tokio::test]
async fn spend_survives_a_restart() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg.clone(), mock.clone()).await;

    assert_eq!(
        h.post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let ledger = h.state.ledger();
    assert!(!ledger.is_empty(), "the spend was recorded");
    h.shutdown().await;

    // A fresh process that adopts the ledger is already over its limit, which is the
    // whole point: a guardrail that forgets on restart is not a guardrail.
    let restarted = Harness::start(cfg, mock).await;
    restarted.state.set_ledger(ledger);
    assert_eq!(
        restarted
            .post("/v1/messages")
            .json(&json!({"model": "sonnet-class", "max_tokens": 8,
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .status(),
        402
    );

    restarted.shutdown().await;
}

#[tokio::test]
async fn per_provider_budget_blocks_only_that_provider() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // Price the deepseek-sonnet model so the budget can measure spend.
    cfg.models[1].pricing = Some(Pricing::new("USD", 1000.0, 1000.0));
    // Budget scoped to the deepseek provider, exhausted after one request.
    cfg.budgets = vec![Budget::new(
        BudgetScope::Provider {
            id: "deepseek".into(),
        },
        BudgetPeriod::Day,
        "USD",
        0.01,
    )];
    let h = Harness::start(cfg, mock).await;

    // First deepseek request succeeds and spends past the limit.
    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Second deepseek request is refused by the provider budget.
    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "deepseek-deepseek-v4-pro", "max_tokens": 8,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 402);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "budget_exceeded");

    // OpenAI (different provider) should still work.
    let resp = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "openai-gpt-5.3-sol",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    h.shutdown().await;
}

// --------------------------------------------------- concurrent budget admission

/// The report's scenario, through the real server: a $1 limit with $0.99
/// already spent, and 32 concurrent $0.10 requests.
///
/// Before admission they all read the same historical total and all passed, so
/// the proxy spent $4.19 against a $1 limit. Now each request holds a permit for
/// every budgeted scope it occupies until it has been charged, so exactly one
/// gets through and the rest are refused — the documented overshoot of at most
/// one request.
#[tokio::test]
async fn concurrent_requests_cannot_all_pass_the_same_budget() {
    let (addr, mock) = start_hold_mock().await;
    let mut cfg = config_for(addr);
    // 1000 prompt tokens at $100/Mtok is exactly $0.10 a request.
    cfg.models[1].pricing = Some(Pricing::new("USD", 100.0, 0.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        1.0,
    )];

    // $0.99 already spent, in the bucket the check will read.
    let mut ledger = Ledger::new();
    ledger.charge(
        Local::now(),
        "deepseek",
        Some(ModelTier::Standard),
        &Cost {
            currency: "USD".into(),
            amount: 0.99,
        },
    );
    let h = Harness::start_with_ledger(cfg, ledger).await;

    let ask = || {
        h.post("/v1/chat/completions")
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
    };
    let sent: Vec<_> = (0..32).map(|_| ask().send()).collect();
    let responses = futures_util::future::join_all(sent).await;

    let mut admitted = 0;
    let mut refused = 0;
    for response in responses {
        match response.unwrap().status().as_u16() {
            200 => admitted += 1,
            402 => refused += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(admitted + refused, 32, "every request got an answer");
    assert_eq!(
        admitted, 1,
        "at most one request may pass the check while the others wait"
    );
    assert_eq!(refused, 31);

    // The permit really was exclusive: one request reached the provider, and
    // never more than one was in flight there.
    assert_eq!(
        mock.requests(),
        1,
        "the refused requests must be stopped before the provider"
    );
    assert_eq!(
        mock.max_in_flight(),
        1,
        "at most one request held the permit"
    );

    // The overshoot is bounded by the one request that crossed the line:
    // 0.99 + 0.10, not 0.99 + 32 x 0.10.
    let spent = global_day_spend(&h.state);
    assert!(
        (spent - 1.09).abs() < 1e-9,
        "expected the historical total plus one request, got {spent}"
    );
    assert!(
        spent <= 1.0 + 0.1 + 1e-9,
        "spend went past the limit by more than one request: {spent}"
    );

    h.shutdown().await;
}

/// A limit on a scope these requests cannot reach must not serialise them: the
/// proxy must not become single-file for traffic no budget covers.
#[tokio::test]
async fn requests_whose_scopes_have_no_budget_stay_concurrent() {
    let (addr, mock) = start_hold_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 100.0, 0.0));
    // A provider budget for a provider `sonnet-class` cannot land on.
    cfg.budgets = vec![Budget::new(
        BudgetScope::Provider {
            id: "anthropic".into(),
        },
        BudgetPeriod::Day,
        "USD",
        1.0,
    )];
    let h = Harness::start_with_ledger(cfg, Ledger::new()).await;

    let ask = || {
        h.post("/v1/chat/completions")
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
    };
    let (first, second) = tokio::join!(ask().send(), ask().send());
    assert_eq!(first.unwrap().status(), 200);
    assert_eq!(second.unwrap().status(), 200);
    assert_eq!(
        mock.max_in_flight(),
        2,
        "two requests with no finite budget must be in flight at once"
    );

    h.shutdown().await;
}

/// A client that walks away mid-stream must give its permit back, or every
/// later request for that scope would wait for a stream that will never end.
#[tokio::test]
async fn a_disconnected_stream_releases_its_permit() {
    let (addr, mock) = start_hold_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 100.0, 0.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        1.0,
    )];
    let h = Harness::start_with_ledger(cfg, Ledger::new()).await;

    let mut held = h
        .post("/v1/chat/completions")
        .json(&json!({"model": "sonnet-class", "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(held.status(), 200);
    let first = held.chunk().await.unwrap().expect("some output");
    assert!(!first.is_empty(), "the stream really started");
    assert_eq!(mock.requests(), 1, "the stream is in flight");

    // The client walks away rather than reading the answer.
    drop(held);
    wait_for_outcomes(&h, 1).await;

    let second = tokio::time::timeout(
        Duration::from_secs(5),
        h.post("/v1/chat/completions")
            .json(&json!({"model": "sonnet-class",
                          "messages": [{"role": "user", "content": "hi"}]}))
            .send(),
    )
    .await
    .expect("the disconnected stream must release its permit")
    .unwrap();
    assert_eq!(second.status(), 200);

    h.shutdown().await;
}

/// A normal settlement releases the permit too, so requests that fit the budget
/// keep flowing one after another.
#[tokio::test]
async fn a_normal_settlement_releases_the_permit() {
    let (addr, _mock) = start_hold_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("USD", 100.0, 0.0));
    cfg.budgets = vec![Budget::new(
        BudgetScope::Global,
        BudgetPeriod::Day,
        "USD",
        1.0,
    )];
    let h = Harness::start_with_ledger(cfg, Ledger::new()).await;

    // Three sequential requests that all fit: each one may only start because
    // the previous settlement gave the scope back.
    for expected in [0.1, 0.2, 0.3] {
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            h.post("/v1/chat/completions")
                .json(&json!({"model": "sonnet-class",
                              "messages": [{"role": "user", "content": "hi"}]}))
                .send(),
        )
        .await
        .expect("the previous settlement must have released the permit")
        .unwrap();
        assert_eq!(response.status(), 200);
        let spent = global_day_spend(&h.state);
        assert!(
            (spent - expected).abs() < 1e-9,
            "expected {expected} charged, got {spent}"
        );
    }

    h.shutdown().await;
}
