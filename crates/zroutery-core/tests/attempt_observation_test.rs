//! Per-attempt response metadata: what one upstream send declared and used.
//!
//! `AttemptObservation` is the evidence a failed attempt keeps: the model the
//! response's structured fields declared, and whatever usage the wire stated
//! before the send failed. These tests hold the observation's merge contract —
//! what a later reading may replace and what it may not — and drive the real
//! capture path against a loopback mock upstream, because the capture has to
//! have run before an error parser could discard the body.
//!
//! Everything here is JSON fixtures against a loopback socket: no real
//! credentials, no real upstream, no ML involvement — the observation types
//! are plain accounting evidence and depend on none.

use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use zroutery_core::config::{
    resolve_client_identity, ProviderClientProfile, ProviderConfig, ProviderKind,
};
use zroutery_core::ir::{
    AttemptObservation, DeclaredIdentity, ObservedResponse, UpstreamModelDeclaration, Usage,
    UsageEvidence,
};
use zroutery_core::upstream::Upstream;

// --------------------------------------------------------- merge contract

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        ..Usage::default()
    }
}

/// An all-zero total is structurally "nothing was reported": it is never
/// recorded as a measured zero.
#[test]
fn an_all_zero_usage_is_never_a_measured_zero() {
    let observation = AttemptObservation::default();
    observation.observe(&Default::default(), Some(Usage::default()));
    let seen = observation.snapshot();
    assert!(seen.usage.is_none());
    assert_eq!(seen.usage_evidence, UsageEvidence::Absent);
}

/// A zero restatement cannot clear a reading the wire already stated.
#[test]
fn a_recorded_usage_is_not_shadowed_by_an_all_zero_restatement() {
    let observation = AttemptObservation::default();
    observation.observe(&Default::default(), Some(usage(100, 50)));
    observation.observe(&Default::default(), Some(Usage::default()));
    let seen = observation.snapshot();
    assert_eq!(seen.usage, Some(usage(100, 50)));
    assert_eq!(seen.usage_evidence, UsageEvidence::Partial);
}

/// Restating frames are cumulative, so a dominant terminal replaces the
/// opening partial and confirms it.
#[test]
fn a_cumulative_restatement_replaces_the_opening_total() {
    let observation = AttemptObservation::default();
    observation.observe(&Default::default(), Some(usage(100, 1)));
    observation.observe_terminal_usage(&Default::default(), usage(100, 15));
    let seen = observation.snapshot();
    assert_eq!(seen.usage, Some(usage(100, 15)));
    assert_eq!(seen.usage_evidence, UsageEvidence::Measured);
}

/// A later reading that would shrink a retained total conflicts with it. The
/// retained reading stands — in particular this is not a per-field maximum,
/// which would dress a counter reset up as growth — and the conflict is a
/// visible state, not a silently billed account.
#[test]
fn a_later_total_that_shrinks_is_refused_not_field_maxed() {
    let observation = AttemptObservation::default();
    observation.observe(&Default::default(), Some(usage(500, 15)));
    observation.observe(&Default::default(), Some(usage(3, 50)));
    let seen = observation.snapshot();
    assert_eq!(seen.usage, Some(usage(500, 15)));
    assert_eq!(seen.usage_evidence, UsageEvidence::Conflicting);
}

/// Growth on one axis does not adopt a shrink on the other.
#[test]
fn growth_on_one_axis_does_not_adopt_a_shrink_on_the_other() {
    let observation = AttemptObservation::default();
    observation.observe(&Default::default(), Some(usage(100, 50)));
    observation.observe(&Default::default(), Some(usage(120, 10)));
    assert_eq!(observation.snapshot().usage, Some(usage(100, 50)));
}

/// The declaration is evidence in its own right: it survives an absent or
/// unusable usage and keeps its own conflicts across observations.
#[test]
fn the_declaration_survives_unusable_usage_and_keeps_conflicts() {
    let mut first = UpstreamModelDeclaration::default();
    first.observe_model(Some("relay-a"));
    let mut second = UpstreamModelDeclaration::default();
    second.observe_model(Some("relay-b"));

    let observation = AttemptObservation::default();
    observation.observe(&first, None);
    observation.observe(&second, None);
    observation.observe(&Default::default(), Some(Usage::default()));

    let seen = observation.snapshot();
    assert!(seen.usage.is_none(), "no usable usage was ever stated");
    assert!(
        matches!(seen.declaration.model, DeclaredIdentity::Conflicting(_)),
        "two different declarations stay a conflict: {:?}",
        seen.declaration.model
    );
}

// ------------------------------------------------------------- mock relay

async fn respond(model: &str) -> Response {
    match model {
        // A relay that answers a rate limit with the usage the request
        // consumed before it was refused.
        "fail-with-usage" => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": {"message": "insufficient quota", "type": "insufficient_quota"},
                "model": "gpt-x",
                "usage": {"input_tokens": 120, "output_tokens": 9}
            })),
        )
            .into_response(),
        // The same refusal, but the usage object cannot state its own totals.
        "fail-with-bad-usage" => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": {"message": "insufficient quota", "type": "insufficient_quota"},
                "model": "gpt-x",
                "usage": {"output_tokens": 5}
            })),
        )
            .into_response(),
        // A successful answer whose usage object cannot state its own totals:
        // the lenient decoder fills the missing input with zero, and exactly
        // that backfill must not become accounting.
        "ok-bad-usage" => Json(json!({
            "id": "resp-1",
            "object": "response",
            "status": "completed",
            "model": "gpt-x",
            "output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok", "annotations": []}]}],
            "usage": {"output_tokens": 5}
        }))
        .into_response(),
        // A successful answer whose cached detail contradicts itself; the
        // lenient decoder clamps it into a trusted-looking subset.
        "ok-bad-detail" => Json(json!({
            "id": "resp-1",
            "object": "response",
            "status": "completed",
            "model": "gpt-x",
            "output": [{"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok", "annotations": []}]}],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 5,
                "input_tokens_details": {"cached_tokens": -3}
            }
        }))
        .into_response(),
        // A successful answer whose usage carries the documented cache detail.
        "ok-openai" => Json(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-x",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                         "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 40}
            }
        }))
        .into_response(),
        // A relay that reports the Anthropic-style cache-write counter on an
        // OpenAI-shaped body. The capture merges it into the prompt total;
        // the lenient decoder ignores it, so the decoded restatement would
        // shrink the total the wire actually stated.
        "ok-cache-write" => Json(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 0,
            "model": "gpt-x",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                         "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "cache_creation_input_tokens": 20
            }
        }))
        .into_response(),
        other => panic!("the mock relay does not answer {other}"),
    }
}

async fn chat(Json(body): Json<Value>) -> Response {
    respond(body["model"].as_str().unwrap_or_default()).await
}

async fn responses(Json(body): Json<Value>) -> Response {
    respond(body["model"].as_str().unwrap_or_default()).await
}

async fn start_relay() -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

fn provider_for(base: &str, kind: ProviderKind, surface: Option<&str>) -> ProviderConfig {
    let mut provider = ProviderConfig::new("relay", "Relay", kind);
    provider.base_url = base.to_string();
    provider.timeout_secs = 5;
    provider.connect_timeout_secs = 5;
    if let Some(surface) = surface {
        provider.api_surface = serde_json::from_value(json!(surface)).unwrap();
    }
    provider
}

async fn send_observed(provider: &ProviderConfig, model: &str) -> ObservedResponse {
    let upstream = Upstream::new(true, 5);
    let identity = resolve_client_identity(ProviderClientProfile::Auto, &HeaderMap::new());
    let body = json!({"model": model, "messages": [{"role": "user", "content": "hi"}]});
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        upstream.send_with_identity(provider, None, &body, &identity),
    )
    .await
    .expect("the loopback relay must not hang");
    identity.observation.snapshot()
}

// ----------------------------------------------------- capture on the wire

/// A failed attempt keeps the usage its error body reported: the capture runs
/// before the status check discards the body, so a refused request's known
/// charge is not lost. The reading is wire evidence, not a confirmed account.
#[tokio::test]
async fn a_failed_attempt_keeps_the_usage_its_error_body_reported() {
    let base = start_relay().await;
    let provider = provider_for(&base, ProviderKind::OpenAICompatible, Some("responses"));
    let seen = send_observed(&provider, "fail-with-usage").await;
    let usage = seen.usage.expect("the refused request's usage is kept");
    assert_eq!(usage.input_tokens, 120);
    assert_eq!(usage.output_tokens, 9);
    assert_eq!(seen.usage_evidence, UsageEvidence::Partial);
    assert_eq!(
        seen.declaration.model.value(),
        Some("gpt-x"),
        "the failed answer still declares which model produced it"
    );
}

/// A usage object that cannot state its own totals is not a measured zero:
/// the reading stays unknown, flagged invalid, while the declaration lands.
#[tokio::test]
async fn a_malformed_error_usage_stays_unknown_but_the_declaration_lands() {
    let base = start_relay().await;
    let provider = provider_for(&base, ProviderKind::OpenAICompatible, Some("responses"));
    let seen = send_observed(&provider, "fail-with-bad-usage").await;
    assert!(
        seen.usage.is_none(),
        "a missing total is not filled with zero"
    );
    assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
    assert_eq!(seen.declaration.model.value(), Some("gpt-x"));
}

/// A successful answer reports one usage number, not two: what the raw body
/// capture extracted and what the decoded response states must agree.
#[tokio::test]
async fn a_valid_answer_reports_one_usage_raw_and_decoded() {
    let base = start_relay().await;
    let provider = provider_for(&base, ProviderKind::OpenAICompatible, None);
    let seen = send_observed(&provider, "ok-openai").await;
    let usage = seen.usage.expect("the answer's usage is captured");
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.cache_read_tokens, 40);
    assert_eq!(usage.cache_write_tokens, 0);
    assert_eq!(seen.usage_evidence, UsageEvidence::Measured);
}

/// A successful answer whose usage object cannot state its own totals must
/// stay unknown even though the lenient decoder fills the missing axis with
/// zero: the raw capture refused the wire usage, so the decoded backfill is
/// refused with it and the attempt reports unknown usage, not a measured
/// zero input.
#[tokio::test]
async fn a_decoder_backfill_cannot_resurrect_usage_the_wire_refused() {
    let base = start_relay().await;
    let provider = provider_for(&base, ProviderKind::OpenAICompatible, Some("responses"));
    let seen = send_observed(&provider, "ok-bad-usage").await;
    assert!(
        seen.usage.is_none(),
        "the decoded input_tokens=0 backfill must not become accounting"
    );
    assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
    assert_eq!(seen.declaration.model.value(), Some("gpt-x"));
}

/// The same refusal for a malformed detail counter: the decoder clamps it
/// into a trusted-looking subset; the raw refusal stands.
#[tokio::test]
async fn a_clamped_detail_backfill_cannot_resurrect_refused_usage() {
    let base = start_relay().await;
    let provider = provider_for(&base, ProviderKind::OpenAICompatible, Some("responses"));
    let seen = send_observed(&provider, "ok-bad-detail").await;
    assert!(
        seen.usage.is_none(),
        "cached_tokens -3 is not a measured zero"
    );
    assert_eq!(seen.usage_evidence, UsageEvidence::Invalid);
}

/// A relay that declares a cache-write subset on an OpenAI-shaped body keeps
/// the merged reading: the capture folded the declared subset into the prompt
/// total, and the same-source decoded restatement that would shrink that
/// total is not treated as a wire conflict — the richer reading stands, the
/// send is confirmed decoded.
#[tokio::test]
async fn a_relay_declared_cache_write_subset_stands_over_a_smaller_restatement() {
    let base = start_relay().await;
    let provider = provider_for(&base, ProviderKind::OpenAICompatible, None);
    let seen = send_observed(&provider, "ok-cache-write").await;
    let usage = seen.usage.expect("the answer's usage is captured");
    assert_eq!(usage.input_tokens, 120, "fresh 100 + declared write 20");
    assert_eq!(usage.cache_write_tokens, 20);
    assert_eq!(usage.fresh_input_tokens(), 100);
    assert_eq!(
        seen.usage_evidence,
        UsageEvidence::Measured,
        "a same-source re-extraction is not a wire conflict"
    );
}
