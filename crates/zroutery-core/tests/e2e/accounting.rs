use serde_json::{json, Value};

use zroutery_core::billing::Pricing;

use super::support::*;

#[tokio::test]
async fn priced_requests_report_their_cost() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    // 3 USD per million in, 15 out: the shape of a frontier model's price list.
    cfg.models[1].pricing = Some(Pricing::new("USD", 3.0, 15.0));
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(&json!({"model": "sonnet-class", "max_tokens": 16,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // The mock reports 11 prompt and 7 completion tokens.
    let expected = 3.0 * 11.0 / 1e6 + 15.0 * 7.0 / 1e6;
    assert_eq!(
        resp.headers()["x-zroutery-cost"],
        format!("USD {expected:.6}")
    );

    let record = &h.state.stats().recent(1)[0];
    let cost = record.cost.as_ref().unwrap();
    assert_eq!(cost.currency, "USD");
    assert!((cost.amount - expected).abs() < 1e-12);

    let summary = h.state.stats().summary();
    assert!((summary.cost.get("USD") - expected).abs() < 1e-12);
    let per_model = summary
        .per_model
        .iter()
        .find(|m| m.model_id == "deepseek-deepseek-v4-pro")
        .unwrap();
    assert!((per_model.cost.get("USD") - expected).abs() < 1e-12);

    // And the estimate offered before sending uses the same price.
    let body: Value = h
        .post("/v1/messages/count_tokens")
        .json(&json!({"model": "sonnet-class",
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let estimate = &body["zroutery"]["estimated_input_cost"];
    assert_eq!(estimate["currency"], "USD");
    assert!(estimate["amount"].as_f64().unwrap() > 0.0);
    assert_eq!(body["zroutery"]["input_per_mtok"], 3.0);

    h.shutdown().await;
}

#[tokio::test]
async fn streamed_requests_are_priced_even_without_a_header() {
    let (addr, mock) = start_mock().await;
    let mut cfg = config_for(addr);
    cfg.models[1].pricing = Some(Pricing::new("CNY", 2.0, 8.0));
    let h = Harness::start(cfg, mock).await;

    let resp = h
        .post("/v1/messages")
        .json(
            &json!({"model": "sonnet-class", "max_tokens": 16, "stream": true,
                      "messages": [{"role": "user", "content": "hi"}]}),
        )
        .send()
        .await
        .unwrap();
    // Headers are already sent when the usage arrives, so there is nothing to put
    // in them; the record still gets the cost.
    assert!(resp.headers().get("x-zroutery-cost").is_none());
    let _ = resp.text().await.unwrap();

    let record = &h.state.stats().recent(1)[0];
    let cost = record.cost.as_ref().unwrap();
    // The mock's streaming trailer reports 5 prompt and 2 completion tokens.
    assert_eq!(cost.currency, "CNY");
    assert!((cost.amount - (2.0 * 5.0 / 1e6 + 8.0 * 2.0 / 1e6)).abs() < 1e-12);

    h.shutdown().await;
}
