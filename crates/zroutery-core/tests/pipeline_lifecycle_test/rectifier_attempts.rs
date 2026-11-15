use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::json;
use zroutery_core::config::{AppConfig, MemorySecretStore, ModelTier};
use zroutery_core::failure::FailureClass;
use zroutery_core::outcome::FinalStatus;

use super::support::*;

// --------------------------------------------------- per-send attempt accounting

impl Harness {
    /// An Anthropic request that enables extended thinking.
    async fn ask_thinking(&self, model: &str, budget: u64, stream: bool) -> reqwest::Response {
        self.post("/v1/messages")
            .json(&json!({
                "model": model,
                "max_tokens": 4096,
                "stream": stream,
                "thinking": {"type": "enabled", "budget_tokens": budget},
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap()
    }

    /// Signature repair is permitted after an explicit upstream rejection;
    /// unlike a token budget or native effort, no reasoning control is lowered.
    async fn ask_signature_repair(
        &self,
        model: &str,
        with_image: bool,
        stream: bool,
    ) -> reqwest::Response {
        let mut content = vec![json!({"type":"text","text":"fixture"})];
        if with_image {
            content.push(json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}));
        }
        self.post("/v1/messages").json(&json!({"model":model,"max_tokens":4096,"stream":stream,"messages":[
            {"role":"user","content":content},
            {"role":"assistant","content":[{"type":"thinking","thinking":"fixture reasoning","signature":"stale-signature"},
                {"type":"text","text":"prior answer"}]},
            {"role":"user","content":"continue fixture"}
        ]})).send().await.unwrap()
    }
}

/// The Anthropic provider the rectifier scenarios talk to.
fn repair_config(mock: SocketAddr) -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.auth_token = TOKEN.into();
    cfg.providers = vec![anthropic_provider("delta", "Delta", mock)];
    // The requests under test declare thinking (and one carries an image), and
    // request-derived capabilities are a hard gate, so the candidates have to
    // declare them.
    let capable = |upstream: &str| {
        let mut entry = model("delta", upstream, 10, ModelTier::Standard);
        entry.capabilities.thinking = true;
        entry.capabilities.vision = true;
        entry
    };
    cfg.models = vec![
        capable("repair-once-model"),
        capable("repair-twice-model"),
        capable("repair-never-model"),
        capable("repair-multi-model"),
        capable("repair-stream-model"),
    ];
    cfg
}

fn delta_secrets() -> Arc<MemorySecretStore> {
    Arc::new(MemorySecretStore::new().with("provider:delta", "sk-delta"))
}

async fn repair_harness() -> Harness {
    let (addr, mock) = start_mock().await;
    Harness::start_with_secrets(repair_config(addr), mock, delta_secrets()).await
}

/// One candidate whose repair is refused, and a healthy candidate behind it.
fn repair_fallback_config(mock: SocketAddr) -> AppConfig {
    let mut cfg = repair_config(mock);
    cfg.providers.push(provider("beta", "Beta", mock));
    let capable = |provider: &str, upstream: &str, priority: i32| {
        let mut entry = model(provider, upstream, priority, ModelTier::Standard);
        // The request declares thinking, and request-derived capabilities are
        // a hard gate, so both candidates have to declare it.
        entry.capabilities.thinking = true;
        entry
    };
    cfg.models = vec![
        capable("delta", "repair-fail-model", 0),
        capable("beta", "good-model", 20),
    ];
    cfg
}

fn repair_fallback_secrets() -> Arc<MemorySecretStore> {
    Arc::new(
        MemorySecretStore::new()
            .with("provider:delta", "sk-delta")
            .with("provider:beta", "sk-beta"),
    )
}

/// A repaired request tells the truth about every send: the first attempt is a
/// complete classified failure, the repair is its own attempt, and the request's
/// single Outcome passes the schema. A repairable request shape is not punished
/// in provider health.
#[tokio::test]
async fn a_repaired_request_records_every_send_as_a_complete_attempt() {
    let h = repair_harness().await;

    let response = h
        .ask_signature_repair("delta-repair-once-model", false, false)
        .await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(outcome.attempts.len(), 2, "one attempt per upstream send");
    assert!(!outcome.attempts[0].success);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::ProviderRejected)
    );
    assert!(!outcome.attempts[0].rectified);
    assert!(outcome.attempts[1].is_terminal_success());
    assert!(
        outcome.attempts[1].rectified,
        "the repair is labelled as one"
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), 2, "both sends left the proxy");

    // A fixable request shape is not evidence against the model.
    let health = h.state.router().health_snapshot();
    let delta = health
        .iter()
        .find(|m| m.model_id == "delta-repair-once-model");
    assert_eq!(
        delta.map(|row| row.total_failure).unwrap_or(0),
        0,
        "the repair is not punished"
    );
    assert!(!h.state.router().is_cooling("delta-repair-once-model"));

    h.shutdown().await;
}

/// A repair that is itself refused falls over to the next candidate.
///
/// The first send is a repairable 400, so the rectifier sends again and the
/// provider answers 503 — a complete attempt of its own. That failure is
/// fallbackable, so the request has to reach the healthy candidate behind it
/// instead of ending on the first provider. The record must keep all three
/// sends, in order, and still produce exactly one outcome.
#[tokio::test]
async fn a_repaired_request_that_is_still_refused_fails_over() {
    let (addr, mock) = start_mock().await;
    let h = Harness::start_with_secrets(
        repair_fallback_config(addr),
        mock,
        repair_fallback_secrets(),
    )
    .await;

    let response = h.ask_signature_repair("standard-class", false, false).await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(
        h.mock.count(),
        3,
        "the first send, its repair, and the fallback candidate"
    );
    assert_eq!(outcome.attempts.len(), 3);
    assert_eq!(
        outcome.attempts[0].candidate_model,
        "delta-repair-fail-model"
    );
    assert!(!outcome.attempts[0].success);
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::ProviderRejected)
    );
    assert!(!outcome.attempts[0].rectified);
    assert_eq!(
        outcome.attempts[1].candidate_model,
        "delta-repair-fail-model"
    );
    assert!(!outcome.attempts[1].success);
    assert!(outcome.attempts[1].rectified);
    assert_eq!(
        outcome.attempts[1].failure_class,
        Some(FailureClass::ProviderUnavailable)
    );
    assert_eq!(outcome.attempts[2].candidate_model, "beta-good-model");
    assert!(outcome.attempts[2].is_terminal_success());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());

    // The repairable 400 is not punished: only the failed repair (503) counts
    // as evidence against the refusing candidate. The serving candidate is
    // healthy and was never blamed for the first provider's failure.
    let health = h.state.router().health_snapshot();
    let delta = health
        .iter()
        .find(|m| m.model_id == "delta-repair-fail-model")
        .expect("the refusing candidate has health");
    assert_eq!(delta.total_failure, 1);
    let beta = health
        .iter()
        .find(|m| m.model_id == "beta-good-model")
        .expect("the serving candidate has health");
    assert_eq!(beta.total_success, 1);

    h.shutdown().await;
}

/// A repair that needs more than one round still produces one complete attempt
/// per send, in order, and the final success settles the request.
#[tokio::test]
async fn a_multi_round_repair_records_each_round() {
    let h = repair_harness().await;

    let response = h
        .ask_signature_repair("delta-repair-twice-model", true, false)
        .await;
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.attempts.len(), 3, "initial send plus two repairs");
    assert!(!outcome.attempts[0].success);
    assert!(!outcome.attempts[1].success);
    assert!(outcome.attempts[1].rectified);
    assert!(outcome.attempts[2].is_terminal_success());
    assert!(outcome.attempts[2].rectified);
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), 3);

    h.shutdown().await;
}

/// When two different rectifiers each send, each send is its own attempt — the
/// cascade is not collapsed into one invisible retry.
#[tokio::test]
async fn multiple_rectifier_candidates_each_record_their_own_attempt() {
    let h = repair_harness().await;

    let response = h
        .post("/v1/messages")
        .json(&json!({
            "model": "delta-repair-multi-model",
            "max_tokens": 4096,
            "messages": [
                {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64",
                     "media_type": "image/png", "data": "AAAA"}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                    {"type": "text", "text": "looking"}
                ]},
                {"role": "user", "content": "what is this"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.attempts.len(), 3, "one attempt per cascade send");
    assert_eq!(
        outcome.attempts[0].failure_class,
        Some(FailureClass::ProviderRejected)
    );
    assert_eq!(
        outcome.attempts[1].failure_class,
        Some(FailureClass::Capability),
        "the media rejection is its own classified attempt"
    );
    assert!(outcome.attempts[2].is_terminal_success());
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), 3);

    h.shutdown().await;
}

/// When every repair fails, every send is still a complete settled attempt and
/// the request's Outcome is a valid failure.
#[tokio::test]
async fn every_failed_repair_is_a_complete_attempt() {
    let h = repair_harness().await;

    let response = h
        .ask_signature_repair("delta-repair-never-model", false, false)
        .await;
    assert_eq!(response.status(), 400);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert!(
        outcome.attempts.len() >= 2,
        "the initial send and each repair round: {}",
        outcome.attempts.len()
    );
    assert!(
        outcome
            .attempts
            .iter()
            .all(|attempt| !attempt.success && attempt.failure_class.is_some()),
        "no attempt is left unsuccessful and unclassified: {:?}",
        outcome.attempts
    );
    assert!(outcome
        .attempts
        .iter()
        .skip(1)
        .all(|attempt| attempt.rectified));
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), outcome.attempts.len());

    h.shutdown().await;
}

/// A streamed repair is a second handshake with its own attempt, and the stream
/// terminal settles it.
#[tokio::test]
async fn a_streamed_repair_records_both_handshakes() {
    let h = repair_harness().await;

    let response = h
        .ask_signature_repair("delta-repair-stream-model", false, true)
        .await;
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    assert!(body.contains("repaired"), "{body}");
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Success);
    assert_eq!(
        outcome.attempts.len(),
        2,
        "the failed handshake and the repair"
    );
    assert!(!outcome.attempts[0].success);
    assert!(outcome.attempts[1].is_terminal_success());
    assert!(outcome.attempts[1].rectified);
    assert_eq!(
        outcome.served_identity().map(|id| id.model().to_string()),
        Some("delta-repair-stream-model".to_string())
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), 2);

    h.shutdown().await;
}

/// A stream whose repairs all fail at the handshake is a valid failure with one
/// complete attempt per send — no invisible retry, no unclassified attempt.
#[tokio::test]
async fn a_failed_streamed_repair_is_a_complete_attempt() {
    let h = repair_harness().await;

    let response = h.ask_thinking("delta-repair-never-model", 4096, true).await;
    assert_eq!(response.status(), 400);
    let _ = response.text().await.unwrap();
    wait_for_outcomes(&h, 1).await;

    let outcome = h.outcome();
    assert_eq!(outcome.final_status, FinalStatus::Failed);
    assert!(
        outcome
            .attempts
            .iter()
            .all(|attempt| !attempt.success && attempt.failure_class.is_some()),
        "every send is settled: {:?}",
        outcome.attempts
    );
    assert!(outcome.validate().is_ok(), "{:?}", outcome.validate());
    assert_eq!(h.mock.count(), outcome.attempts.len());
    assert_eq!(
        outcome.attempts.len(),
        1,
        "an explicit budget rejection must not trigger a lower-budget replay"
    );
    assert_eq!(h.mock.bodies()[0]["thinking"]["budget_tokens"], 4096);

    h.shutdown().await;
}
