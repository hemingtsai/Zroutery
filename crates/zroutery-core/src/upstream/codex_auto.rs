//! Stage a complete response before any executable output crosses the gateway.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;

use super::{build_headers_for_identity, retry_after, Upstream};
use crate::config::{ClientIdentity, ProviderConfig};
use crate::error::{Error, Result};
use crate::ir::{Dialect, StreamEvent, Usage};
use crate::protocol::{self, SseDecoder, SseFrame};
use crate::rectifier::codex_auto::{BUFFER_LIMIT, IDLE_TIMEOUT};

pub enum StagedResponse {
    Json(Value),
    Responses(Vec<SseFrame>),
    Events(Vec<StreamEvent>),
}

impl Upstream {
    /// Native compaction is not safe to replay after a partial attempt.
    pub async fn compact_codex(
        &self,
        provider: &ProviderConfig,
        key: Option<&str>,
        body: &Value,
        identity: &ClientIdentity,
    ) -> Result<Value> {
        let request = self
            .client_for(provider)
            .post(format!(
                "{}/compact",
                provider.chat_url().trim_end_matches('/')
            ))
            .headers(build_headers_for_identity(provider, key, identity)?)
            .json(body);
        let idle = IDLE_TIMEOUT.min(Duration::from_secs(provider.timeout_secs));
        let response = tokio::time::timeout(idle, request.send())
            .await
            .map_err(|_| Error::Timeout(idle.as_secs()))?
            .map_err(|source| Error::Transport {
                provider: provider.name.clone(),
                source,
            })?;
        let status = response.status().as_u16();
        let floor = retry_after(response.headers());
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = tokio::time::timeout(idle, stream.next())
            .await
            .map_err(|_| Error::Timeout(idle.as_secs()))?
        {
            let chunk = chunk.map_err(|source| Error::Transport {
                provider: provider.name.clone(),
                source,
            })?;
            if bytes.len() + chunk.len() > BUFFER_LIMIT {
                return Err(Error::BadUpstreamPayload(
                    "response_too_large: compact response exceeds 64 MiB".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if status >= 400 {
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
                retry_after: floor,
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::BadUpstreamPayload("invalid compact response JSON".into()))
    }
    /// Exactly one send, no hidden retry loop. The pipeline owns retry budgets,
    /// cancellation, accounting and the deadline across all these sends.
    pub async fn stage_codex(
        &self,
        provider: &ProviderConfig,
        key: Option<&str>,
        body: &Value,
        model: &str,
        identity: &ClientIdentity,
    ) -> Result<StagedResponse> {
        let idle = IDLE_TIMEOUT.min(Duration::from_secs(provider.timeout_secs));
        let request = self
            .client_for(provider)
            .post(provider.chat_url())
            .headers(build_headers_for_identity(provider, key, identity)?)
            .json(body);
        let response = tokio::time::timeout(idle, request.send())
            .await
            .map_err(|_| Error::Timeout(idle.as_secs()))?
            .map_err(|source| Error::Transport {
                provider: provider.name.clone(),
                source,
            })?;
        let status = response.status().as_u16();
        let floor = retry_after(response.headers());
        let sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"));
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        let mut decoder = SseDecoder::new();
        let mut frames = Vec::new();
        let mut parser = protocol::stream_parser(provider.dialect(), model);
        let mut events = Vec::new();
        let mut size = 0usize;
        loop {
            let chunk = tokio::time::timeout(idle, stream.next())
                .await
                .map_err(|_| Error::Timeout(idle.as_secs()))?;
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|source| Error::Transport {
                provider: provider.name.clone(),
                source,
            })?;
            size += chunk.len();
            if size > BUFFER_LIMIT {
                return Err(Error::BadUpstreamPayload(
                    "response_too_large: buffered response exceeds 64 MiB".into(),
                ));
            }
            if status != 200 || !sse {
                bytes.extend_from_slice(&chunk);
                continue;
            }
            for frame in decoder.push(&chunk) {
                if provider.dialect() == Dialect::OpenAIResponses {
                    let value = match frame.json() {
                        Ok(value) => value,
                        Err(_) => {
                            frames.push(frame);
                            continue;
                        }
                    };
                    let kind = frame
                        .event
                        .as_deref()
                        .or_else(|| value["type"].as_str())
                        .unwrap_or("")
                        .to_string();
                    if matches!(kind.as_str(), "error" | "response.failed") {
                        let mut value = value;
                        value["type"] = serde_json::json!(kind);
                        return Err(Error::Upstream {
                            provider: provider.name.clone(),
                            status: 200,
                            body: value.to_string(),
                            retry_after: floor,
                        });
                    }
                    let terminal =
                        matches!(kind.as_str(), "response.completed" | "response.incomplete");
                    frames.push(frame);
                    if terminal {
                        return Ok(StagedResponse::Responses(frames));
                    }
                } else {
                    events.extend(parser.push(&frame)?);
                    // Chat's finish_reason precedes its usage trailer. Do not
                    // stop here or the staged answer would lose its stop/usage.
                }
            }
        }
        if status >= 400 {
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
                retry_after: floor,
            });
        }
        if sse {
            if provider.dialect() != Dialect::OpenAIResponses && parser.saw_normal_terminal() {
                events.extend(parser.finish());
                return Ok(StagedResponse::Events(events));
            }
            return Err(Error::InterruptedStream {
                usage: if provider.dialect() == Dialect::OpenAIResponses {
                    Usage::default()
                } else {
                    parser.reported_usage()
                },
            });
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::BadUpstreamPayload("upstream returned invalid JSON".into()))?;
        if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status: 200,
                body: serde_json::json!({"error":error}).to_string(),
                retry_after: floor,
            });
        }
        if value["status"] == "failed" {
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status: 200,
                body: serde_json::json!({"type":"response.failed","response":value}).to_string(),
                retry_after: floor,
            });
        }
        Ok(StagedResponse::Json(value))
    }
}
