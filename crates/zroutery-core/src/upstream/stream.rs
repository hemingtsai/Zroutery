//! One streaming call, end to end.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use super::errors::{capture_failed_body, is_transient_handshake_failure, truncate};
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
    pub async fn stream_with_identity(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
        upstream_model: &str,
        client: &ClientIdentity,
    ) -> Result<EventStream> {
        // The handshake (DNS, connect, TLS, request upload, response status) is
        // retried a couple of times: transient transport failures and gateway
        // blips (408/502/503/504) happen before any bytes of the answer exist,
        // so replaying the request is safe and masks proxy/node flaps.
        const HANDSHAKE_ATTEMPTS: u32 = 3;
        let mut attempt = 0;
        let response = loop {
            attempt += 1;
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
                Err(err) => {
                    if attempt < HANDSHAKE_ATTEMPTS && is_transient_handshake_failure(&err) {
                        tracing::warn!(
                            provider = %provider.name,
                            attempt,
                            error = %err,
                            "transient handshake failure, retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(750 * u64::from(attempt))).await;
                        continue;
                    }
                    return Err(err);
                }
            };

            let status = response.status();
            if status.is_success() {
                break response;
            }
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
            };
            if attempt < HANDSHAKE_ATTEMPTS && is_transient_handshake_failure(&err) {
                tracing::warn!(
                    provider = %provider.name,
                    attempt,
                    status = status.as_u16(),
                    "transient handshake status, retrying"
                );
                tokio::time::sleep(Duration::from_millis(750 * u64::from(attempt))).await;
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
            parser: protocol::stream_parser(provider.kind.dialect(), upstream_model),
            pending: VecDeque::new(),
            provider: provider.name.clone(),
            idle: Duration::from_secs(provider.timeout_secs),
            done: false,
            terminal_error: None,
        };

        Ok(Box::pin(futures_util::stream::unfold(
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
                                        state.done = true;
                                        return Some((Err(err), state));
                                    }
                                }
                            }
                        }
                    }
                }
            },
        )))
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
