//! One streaming call, end to end.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use super::errors::{capture_failed_body, truncate};
use super::headers::{anonymous_identity, build_headers_for_identity};
use super::{EventStream, Upstream};
use crate::config::{ClientIdentity, ProviderConfig};
use crate::error::{Error, Result};
use crate::ir::StreamEvent;
use crate::protocol::{self, SseDecoder, StreamParser};
impl Upstream {
    /// Streaming call, yielding canonical events.
    pub async fn stream(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
        upstream_model: &str,
    ) -> Result<EventStream> {
        let identity = anonymous_identity(provider);
        self.stream_with_identity(provider, api_key, body, upstream_model, &identity)
            .await
    }

    /// Streaming call for one already resolved client identity.
    ///
    /// The identity is resolved once, before the handshake retry loop, so every
    /// replayed attempt carries the same fingerprint and the same forwarded
    /// client headers — a retry must not look like a different client.
    ///
    /// Retries are allowed here: a caller using this form has no plan to fail
    /// over to.
    pub async fn stream_with_identity(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
        upstream_model: &str,
        client: &ClientIdentity,
    ) -> Result<EventStream> {
        self.stream_with_identity_policy(provider, api_key, body, upstream_model, client, false)
            .await
    }

    /// Streaming call whose handshake knows whether a fallback exists.
    ///
    /// `can_fail_over` turns a retryable failure into a handover: with another
    /// candidate behind this one, moving on is faster than waiting out a rate
    /// limit, and the router already owns that decision.
    pub async fn stream_with_identity_policy(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
        upstream_model: &str,
        client: &ClientIdentity,
        can_fail_over: bool,
    ) -> Result<EventStream> {
        // The handshake (DNS, connect, TLS, request upload, response status) is
        // retried by the shared policy: transient failures and gateway blips
        // happen before any bytes of the answer exist, and a rate limit is a
        // window that reopens. Every replay carries the same fingerprint.
        let mut retries = 0u32;
        let response = loop {
            let request = self
                .client()
                .post(provider.chat_url())
                .headers(build_headers_for_identity(provider, api_key, client)?)
                .json(body);
            let result =
                tokio::time::timeout(Duration::from_secs(provider.timeout_secs), request.send())
                    .await
                    .map_err(|_| Error::Timeout(provider.timeout_secs))
                    .and_then(|r| {
                        r.map_err(|source| Error::Transport {
                            provider: provider.name.clone(),
                            source,
                        })
                    });
            let response = match result {
                Ok(response) => response,
                Err(err) => match super::retry_advice(&err, retries, can_fail_over, 2) {
                    Some(delay) => {
                        tracing::warn!(
                            provider = %provider.name,
                            retry = retries + 1,
                            wait_ms = delay.as_millis(),
                            error = %err,
                            "retryable handshake failure, retrying the same candidate"
                        );
                        tokio::time::sleep(delay).await;
                        retries += 1;
                        continue;
                    }
                    None => return Err(err),
                },
            };

            let status = response.status();
            if status.is_success() {
                break response;
            }
            let retry_after = super::retry_after(response.headers());
            // Error bodies are small, but a stalled upstream must not hold the
            // request forever on this path — non-stream calls time this read.
            let text = match tokio::time::timeout(
                Duration::from_secs(provider.timeout_secs),
                response.text(),
            )
            .await
            {
                Ok(Ok(text)) => text,
                _ => String::new(),
            };
            let err = Error::Upstream {
                provider: provider.name.clone(),
                status: status.as_u16(),
                body: truncate(&text, 2000),
                retry_after,
            };
            if let Some(delay) = super::retry_advice(&err, retries, can_fail_over, 2) {
                tracing::warn!(
                    provider = %provider.name,
                    retry = retries + 1,
                    wait_ms = delay.as_millis(),
                    status = status.as_u16(),
                    "retryable handshake status, retrying the same candidate"
                );
                tokio::time::sleep(delay).await;
                retries += 1;
                continue;
            }
            // DEBUG (opt-in via ZROUTERY_CAPTURE_FAILED_BODIES=1): capture the
            // request body so relay-side rejections can be bisected offline.
            capture_failed_body(provider, body, status.as_u16(), &text);
            return Err(err);
        };

        let state = StreamState {
            body: Box::pin(response.bytes_stream()),
            decoder: SseDecoder::new(),
            parser: protocol::stream_parser(provider.dialect(), upstream_model),
            pending: VecDeque::new(),
            provider: provider.name.clone(),
            idle: Duration::from_secs(provider.timeout_secs),
            done: false,
            terminal_error: None,
        };

        let mut stream: EventStream = Box::pin(futures_util::stream::unfold(
            state,
            |mut state| async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }
                    if state.done {
                        // A body that ended without a normal terminal still
                        // hands the client the events its parser closed out;
                        // the truncation follows as the stream's last word.
                        if let Some(err) = state.terminal_error.take() {
                            return Some((Err(err), state));
                        }
                        return None;
                    }
                    match tokio::time::timeout(state.idle, state.body.next()).await {
                        Err(_) => {
                            state.done = true;
                            return Some((Err(Error::Timeout(state.idle.as_secs())), state));
                        }
                        Ok(None) => {
                            state.done = true;
                            // `finish()` closes dangling blocks, but it is not
                            // proof the answer finished: only a real terminal
                            // event is, and its absence is reported as an
                            // interruption rather than a success.
                            let normal = state.parser.saw_normal_terminal();
                            let usage = state.parser.reported_usage();
                            state.pending.extend(state.parser.finish());
                            if !normal {
                                state.terminal_error = Some(Error::InterruptedStream { usage });
                            }
                        }
                        Ok(Some(Err(source))) => {
                            state.done = true;
                            let provider = state.provider.clone();
                            return Some((Err(Error::Transport { provider, source }), state));
                        }
                        Ok(Some(Ok(chunk))) => {
                            for frame in state.decoder.push(&chunk) {
                                match state.parser.push(&frame) {
                                    Ok(events) => state.pending.extend(events),
                                    Err(err) => {
                                        tracing::warn!(provider = %state.provider, error = %err,
                                            "upstream SSE parser rejected an event");
                                        state.done = true;
                                        // Preserve deltas parsed earlier in this
                                        // same network chunk before reporting a
                                        // later error. Otherwise first-byte
                                        // inspection could replay partial output.
                                        state.terminal_error = Some(err);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            },
        ));
        // HTTP 200 is not sufficient proof of a successful SSE handshake:
        // relays can put an upstream 429/402 in response.failed before any
        // answer. Watch only the opening of the stream — a relay that rejects a
        // request refuses it before doing any model work — so a slow first
        // token is released rather than held or failed. Nothing buffered here
        // has been published, so an error may still fail over safely; a silent
        // or cut opening stays the client's terminal error, as it always was.
        const ERROR_PREFACE: Duration = Duration::from_secs(3);
        let mut prefix = Vec::new();
        let mut tail_error = None;
        let deadline = ERROR_PREFACE.min(Duration::from_secs(provider.timeout_secs));
        loop {
            match tokio::time::timeout(deadline, stream.next()).await {
                // A slow model with no output yet is not a failed handshake.
                Err(_) | Ok(None) => break,
                Ok(Some(Ok(event))) => {
                    let output = matches!(
                        event,
                        StreamEvent::TextDelta { .. }
                            | StreamEvent::ThinkingDelta { .. }
                            | StreamEvent::ToolUseStart { .. }
                            | StreamEvent::RedactedThinking { .. }
                            | StreamEvent::Stop { .. }
                    );
                    prefix.push(Ok(event));
                    if output {
                        break;
                    }
                    if prefix.len() >= 256 {
                        return Err(Error::BadUpstreamPayload(
                            "upstream sent too many lifecycle events before any output".into(),
                        ));
                    }
                }
                Ok(Some(Err(err))) => {
                    if matches!(err, Error::InterruptedStream { .. }) {
                        // The body ended before the answer started. That is the
                        // stream's own last word, delivered at the status line
                        // the caller already holds, not a second 502.
                        tail_error = Some(err);
                        break;
                    }
                    return Err(err);
                }
            }
        }
        let tail = futures_util::stream::iter(tail_error.map(Err));
        Ok(Box::pin(
            futures_util::stream::iter(prefix).chain(stream).chain(tail),
        ))
    }
}

struct StreamState {
    body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    decoder: SseDecoder,
    parser: Box<dyn StreamParser>,
    pending: VecDeque<StreamEvent>,
    provider: String,
    idle: Duration,
    done: bool,
    /// Set when the body ended without a normal terminal: the error is yielded
    /// after the parser's closing events, so the client keeps everything it
    /// already received before the cut is reported.
    terminal_error: Option<Error>,
}
