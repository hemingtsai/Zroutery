//! Repairing a request the upstream refused, and settling the attempt when the repair itself is refused.

use super::client::client_for;
use super::lifecycle::RequestLifecycle;
use super::{apply_vision_fallback, console_key, encode_mode_for};
use std::time::Instant;

use serde_json::Value;

use super::AppState;
use crate::config::ClientIdentity;
use crate::error::Error;
use crate::ir::ChatRequest;
use crate::rectifier::media_fallback::MediaFallbackRectifier;
use crate::rectifier::{self, Rectifier};
use crate::router::Candidate;

/// Try every enabled rectifier, allowing each rectifier up to three repair
/// rounds. A rectifier retry is deliberately not reported to the circuit
/// breaker: it is the same provider and same model, and the failure was a
/// fixable request-shape problem.
///
/// A media rejection is upgraded before the ordinary cascade runs: instead of
/// a placeholder, the images get described by the vision model (falling back
/// to the placeholder when it fails), and the repaired request retries the
/// same provider — the upstream rejected the *image*, not the question.
///
/// Every actual upstream send opens its own attempt before it leaves and
/// settles that attempt if it fails. A send that succeeds is left open
/// deliberately: the caller owns the response that follows (a classifier
/// verdict still has to be checked), so it settles the attempt with what the
/// answer turned out to be. Nothing is reported to router health here.
pub(super) async fn try_rectify_buffered(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    req: &mut ChatRequest,
    error: &Error,
    lifecycle: &mut RequestLifecycle,
) -> std::result::Result<Option<crate::ir::ChatResponse>, Error> {
    // Resolve the provider credential once; every retry below reuses it.
    let mut client = client_for(req, &candidate.provider);
    let key = match console_key(state, candidate, key.map(str::to_string), &mut client).await {
        Ok(key) => key,
        Err(e) => return Err(e),
    };
    let key = key.as_deref();

    // The reactive vision path: only when the upstream said "no images" and
    // the request still carries them.
    if state.config().vision.enabled
        && MediaFallbackRectifier.should_apply(error, &serde_json::Value::Null)
        && !crate::media::collect::collect(req).is_empty()
    {
        apply_vision_fallback(state, req, "upstream rejected media").await;
        let key_owned = key.map(str::to_string);
        if let Some(body) = reencode(candidate, req) {
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
            match state
                .upstream
                .send_with_identity(&candidate.provider, key_owned.as_deref(), &body, &client)
                .await
            {
                Ok(resp) => return Ok(Some(resp)),
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
                    tracing::warn!(
                        model = candidate.model_id(),
                        "vision-repaired retry also failed: {e}"
                    );
                }
            }
        }
    }

    let rectifiers = rectifier::from_config(&state.config().routing.rectifier);
    if rectifiers.is_empty() {
        return Ok(None);
    }

    // Rectifiers operate on the encoded body; re-encode the (possibly
    // vision-repaired) request for them.
    let base_body = match reencode(candidate, req) {
        Some(body) => body,
        None => return Ok(None),
    };
    let mut current_body = base_body.clone();
    let mut last_error: Option<Error> = None;
    let mut current_error: &Error = error;

    for rectifier in rectifiers {
        let mut rounds = 0;
        loop {
            if !rectifier.should_apply(current_error, &current_body) {
                break;
            }
            let mut modified = current_body.clone();
            let result = rectifier.rectify(&mut modified);
            if !result.applied {
                break;
            }
            tracing::info!(
                model = candidate.model_id(),
                provider = candidate.provider.name.as_str(),
                rectifier = rectifier.name(),
                "request repaired, retrying same provider without health accounting"
            );
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
            match state
                .upstream
                .send_with_identity(&candidate.provider, key, &modified, &client)
                .await
            {
                Ok(resp) => return Ok(Some(resp)),
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
                    tracing::warn!(
                        model = candidate.model_id(),
                        rectifier = rectifier.name(),
                        "rectifier retry also failed: {e}"
                    );
                    last_error = Some(e);
                    current_body = modified;
                    current_error = last_error.as_ref().expect("just set");
                    rounds += 1;
                    if rounds >= 3 {
                        break;
                    }
                }
            }
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// Re-encode a (possibly repaired) IR request for one candidate.
fn reencode(candidate: &Candidate, req: &ChatRequest) -> Option<Value> {
    crate::upstream::encode_for_mode(
        &candidate.provider,
        req,
        &candidate.entry.upstream_model,
        candidate.entry.max_output_tokens,
        encode_mode_for(crate::query::RequestKind::Main),
    )
    .ok()
}

/// The streaming counterpart of [`try_rectify_buffered`].
///
/// Each repair send opens its own attempt before the handshake leaves. A
/// handshake that fails settles that attempt here; a handshake that succeeds
/// leaves it open, because for a stream the attempt's verdict is the stream's
/// terminal state and only the body stream knows it. Nothing is reported to
/// router health: the repair is the same provider and model, and the original
/// failure was a fixable request shape rather than evidence about the model.
pub(super) async fn try_rectify_stream(
    state: &AppState,
    candidate: &Candidate,
    key: Option<&str>,
    req: &mut ChatRequest,
    error: &Error,
    lifecycle: &mut RequestLifecycle,
) -> std::result::Result<Option<crate::upstream::EventStream>, Error> {
    // Resolve the provider credential once; every retry below reuses it.
    let mut client = client_for(req, &candidate.provider);
    let key = match console_key(state, candidate, key.map(str::to_string), &mut client).await {
        Ok(key) => key,
        Err(e) => return Err(e),
    };
    let key = key.as_deref();

    // The reactive vision path: only when the upstream said "no images" and
    // the request still carries them.
    if state.config().vision.enabled
        && MediaFallbackRectifier.should_apply(error, &serde_json::Value::Null)
        && !crate::media::collect::collect(req).is_empty()
    {
        apply_vision_fallback(state, req, "upstream rejected media").await;
        let key_owned = key.map(str::to_string);
        if let Some(body) = reencode(candidate, req) {
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
            match state
                .upstream
                .stream_with_identity(
                    &candidate.provider,
                    key_owned.as_deref(),
                    &body,
                    &candidate.entry.upstream_model,
                    &client,
                )
                .await
            {
                Ok(events) => return Ok(Some(events)),
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
                    tracing::warn!(
                        model = candidate.model_id(),
                        "vision-repaired stream retry also failed: {e}"
                    );
                }
            }
        }
    }

    let rectifiers = rectifier::from_config(&state.config().routing.rectifier);
    if rectifiers.is_empty() {
        return Ok(None);
    }

    // Rectifiers operate on the encoded body; re-encode the (possibly
    // vision-repaired) request for them.
    let base_body = match reencode(candidate, req) {
        Some(body) => body,
        None => return Ok(None),
    };
    let mut current_body = base_body.clone();
    let mut last_error: Option<Error> = None;
    let mut current_error: &Error = error;

    for rectifier in rectifiers {
        let mut rounds = 0;
        loop {
            if !rectifier.should_apply(current_error, &current_body) {
                break;
            }
            let mut modified = current_body.clone();
            let result = rectifier.rectify(&mut modified);
            if !result.applied {
                break;
            }
            tracing::info!(
                model = candidate.model_id(),
                provider = candidate.provider.name.as_str(),
                rectifier = rectifier.name(),
                "stream request repaired, retrying same provider without health accounting"
            );
            let started = Instant::now();
            lifecycle.begin_attempt(candidate, true);
            match state
                .upstream
                .stream_with_identity(
                    &candidate.provider,
                    key,
                    &modified,
                    &candidate.entry.upstream_model,
                    &client,
                )
                .await
            {
                Ok(events) => return Ok(Some(events)),
                Err(e) => {
                    lifecycle.failed_attempt(started, &e);
                    tracing::warn!(
                        model = candidate.model_id(),
                        rectifier = rectifier.name(),
                        "rectifier stream retry also failed: {e}"
                    );
                    last_error = Some(e);
                    current_body = modified;
                    current_error = last_error.as_ref().expect("just set");
                    rounds += 1;
                    if rounds >= 3 {
                        break;
                    }
                }
            }
        }
    }

    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
/// Wrap a streaming upstream so a tool call to a placeholder injected only to
/// satisfy OpenCode's gate is not served to a client that never declared it.
#[allow(unused_variables)] // `client`/`req` are used only under the `opencode` feature
pub(super) fn maybe_filter_stream(
    events: crate::upstream::EventStream,
    client: &ClientIdentity,
    req: &ChatRequest,
) -> crate::upstream::EventStream {
    #[cfg(feature = "opencode")]
    if client.profile == crate::config::ProviderClientProfile::OpenCodeCli {
        let declared: std::collections::HashSet<String> =
            req.tools.iter().map(|tool| tool.name.clone()).collect();
        return crate::opencode::filter_injected_tool_calls(events, declared);
    }
    events
}
