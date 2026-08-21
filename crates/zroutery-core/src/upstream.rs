//! HTTP transport to the upstream providers.
//!
//! Timeouts are enforced here rather than in `reqwest`: a whole-request
//! deadline would kill long streaming answers, so streams get an *idle*
//! timeout between chunks instead.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use crate::billing::{Balance, BalanceProbe, Pricing};
use crate::config::{ProviderConfig, ProviderKind};
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, ChatResponse, StreamEvent};
use crate::protocol::{self, SseDecoder, StreamParser};

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const USER_AGENT: &str = concat!("zroutery/", env!("CARGO_PKG_VERSION"));

/// Stream of canonical events coming from one upstream call.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

#[derive(Debug, Clone)]
pub struct Upstream {
    client: reqwest::Client,
}

impl Default for Upstream {
    fn default() -> Self {
        Self::new()
    }
}

impl Upstream {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent(USER_AGENT)
            .build()
            // The builder only fails when the TLS backend cannot initialise. A
            // default client is a better outcome than taking the process down.
            .unwrap_or_else(|e| {
                tracing::error!("falling back to a default HTTP client: {e}");
                reqwest::Client::new()
            });
        Upstream { client }
    }

    /// Non streaming call. `body` is already in the provider's dialect.
    pub async fn send(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
    ) -> Result<ChatResponse> {
        let request = self
            .client
            .post(provider.chat_url())
            .headers(build_headers(provider, api_key)?)
            .json(body);

        let response =
            tokio::time::timeout(Duration::from_secs(provider.timeout_secs), request.send())
                .await
                .map_err(|_| Error::Timeout(provider.timeout_secs))?
                .map_err(|source| Error::Transport {
                    provider: provider.name.clone(),
                    source,
                })?;

        let status = response.status();
        let text =
            tokio::time::timeout(Duration::from_secs(provider.timeout_secs), response.text())
                .await
                .map_err(|_| Error::Timeout(provider.timeout_secs))?
                .map_err(|source| Error::Transport {
                    provider: provider.name.clone(),
                    source,
                })?;

        if !status.is_success() {
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status: status.as_u16(),
                body: truncate(&text, 2000),
            });
        }

        let json: Value = serde_json::from_str(&text)
            .map_err(|e| Error::BadUpstreamPayload(format!("{e}: {}", truncate(&text, 500))))?;
        protocol::decode_response(provider.kind.dialect(), json)
    }

    /// Streaming call, yielding canonical events.
    pub async fn stream(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
        upstream_model: &str,
    ) -> Result<EventStream> {
        let request = self
            .client
            .post(provider.chat_url())
            .headers(build_headers(provider, api_key)?)
            .json(body);

        let response =
            tokio::time::timeout(Duration::from_secs(provider.timeout_secs), request.send())
                .await
                .map_err(|_| Error::Timeout(provider.timeout_secs))?
                .map_err(|source| Error::Transport {
                    provider: provider.name.clone(),
                    source,
                })?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status: status.as_u16(),
                body: truncate(&text, 2000),
            });
        }

        let state = StreamState {
            body: Box::pin(response.bytes_stream()),
            decoder: SseDecoder::new(),
            parser: protocol::stream_parser(provider.kind.dialect(), upstream_model),
            pending: VecDeque::new(),
            provider: provider.name.clone(),
            idle: Duration::from_secs(provider.timeout_secs),
            done: false,
        };

        Ok(Box::pin(futures_util::stream::unfold(
            state,
            |mut state| async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }
                    if state.done {
                        return None;
                    }
                    match tokio::time::timeout(state.idle, state.body.next()).await {
                        Err(_) => {
                            state.done = true;
                            return Some((Err(Error::Timeout(state.idle.as_secs())), state));
                        }
                        Ok(None) => {
                            state.done = true;
                            state.pending.extend(state.parser.finish());
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

    /// Ask the provider which models it offers, for the "fetch models" button.
    ///
    /// Some catalogues (OpenRouter style) publish per-token prices; those are
    /// carried along so the dashboard can prefill them.
    pub async fn list_models(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
    ) -> Result<Vec<DiscoveredModel>> {
        let json = self
            .get_json(provider, api_key, &provider.models_url(), 30)
            .await?;
        let mut models: Vec<DiscoveredModel> = json
            .get("data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|m| {
                        let id = m.get("id").and_then(Value::as_str)?;
                        Some(DiscoveredModel {
                            id: id.to_string(),
                            pricing: pricing_from_catalogue(m),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
        Ok(models)
    }

    /// Ask the provider how much credit is left.
    pub async fn fetch_balance(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        probe: &BalanceProbe,
    ) -> Result<Balance> {
        // Presets sometimes need an absolute URL: DeepSeek's balance hangs off the
        // API root while its chat endpoint lives under /v1.
        let url = if probe.path.starts_with("http://") || probe.path.starts_with("https://") {
            probe.path.clone()
        } else {
            format!(
                "{}/{}",
                provider.base_url.trim_end_matches('/'),
                probe.path.trim_start_matches('/')
            )
        };
        let json = self.get_json(provider, api_key, &url, 20).await?;
        Balance::from_payload(probe, &json).ok_or_else(|| {
            Error::BadUpstreamPayload(format!(
                "no balance found in the response from {url}: {}",
                truncate(&json.to_string(), 300)
            ))
        })
    }

    /// Shared GET plumbing for the two metadata endpoints.
    async fn get_json(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        url: &str,
        timeout_secs: u64,
    ) -> Result<Value> {
        let response = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.client
                .get(url)
                .headers(build_headers(provider, api_key)?)
                .send(),
        )
        .await
        .map_err(|_| Error::Timeout(timeout_secs))?
        .map_err(|source| Error::Transport {
            provider: provider.name.clone(),
            source,
        })?;

        let status = response.status();
        let text = response.text().await.map_err(|source| Error::Transport {
            provider: provider.name.clone(),
            source,
        })?;
        if !status.is_success() {
            return Err(Error::Upstream {
                provider: provider.name.clone(),
                status: status.as_u16(),
                body: truncate(&text, 2000),
            });
        }
        serde_json::from_str(&text)
            .map_err(|e| Error::BadUpstreamPayload(format!("{e}: {}", truncate(&text, 500))))
    }
}

/// One entry of a provider's model catalogue.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    /// Present only when the catalogue publishes prices.
    pub pricing: Option<Pricing>,
}

/// Read per-token prices out of a catalogue entry.
///
/// The shape is OpenRouter's: strings of dollars per single token, which become
/// dollars per million. Anything else is left for the user to type.
fn pricing_from_catalogue(entry: &Value) -> Option<Pricing> {
    let pricing = entry.get("pricing")?;
    let per_mtok = |key: &str| -> Option<f64> {
        let raw = pricing.get(key)?;
        let value = raw
            .as_f64()
            .or_else(|| raw.as_str()?.trim().parse::<f64>().ok())?;
        (value.is_finite() && value >= 0.0).then_some(value * 1_000_000.0)
    };

    let input = per_mtok("prompt").or_else(|| per_mtok("input"))?;
    let output = per_mtok("completion").or_else(|| per_mtok("output"))?;
    if input == 0.0 && output == 0.0 {
        // A free model, or a catalogue that fills the fields with zeros.
        return None;
    }
    Some(Pricing {
        currency: "USD".into(),
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: per_mtok("input_cache_read"),
        cache_write_per_mtok: per_mtok("input_cache_write"),
    })
}

struct StreamState {
    body: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    decoder: SseDecoder,
    parser: Box<dyn StreamParser>,
    pending: VecDeque<StreamEvent>,
    provider: String,
    idle: Duration,
    done: bool,
}

/// Build the auth and content headers for a provider.
pub fn build_headers(provider: &ProviderConfig, api_key: Option<&str>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );

    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        match provider.kind {
            ProviderKind::Anthropic => {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(key)
                        .map_err(|_| Error::internal("API key contains invalid characters"))?,
                );
            }
            ProviderKind::OpenAICompatible => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}"))
                        .map_err(|_| Error::internal("API key contains invalid characters"))?,
                );
            }
        }
    }

    if provider.kind == ProviderKind::Anthropic {
        let version = provider
            .anthropic_version
            .as_deref()
            .unwrap_or(DEFAULT_ANTHROPIC_VERSION);
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_str(version)
                .map_err(|_| Error::internal("invalid anthropic-version"))?,
        );
    }

    for (k, v) in &provider.extra_headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| Error::internal(format!("invalid header name `{k}`")))?;
        let value = HeaderValue::from_str(v)
            .map_err(|_| Error::internal(format!("invalid value for header `{k}`")))?;
        headers.insert(name, value);
    }

    Ok(headers)
}

/// Prepare the upstream body for a candidate.
pub fn encode_for(
    provider: &ProviderConfig,
    req: &ChatRequest,
    upstream_model: &str,
    max_output_tokens: Option<u32>,
) -> Result<Value> {
    let mut req = req.clone();
    if let Some(cap) = max_output_tokens {
        req.max_tokens = Some(req.max_tokens.map(|m| m.min(cap)).unwrap_or(cap));
    }
    protocol::encode_request(
        provider.kind.dialect(),
        &req,
        upstream_model,
        &provider.quirks,
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderQuirks;
    use crate::ir::{Dialect, Message};

    fn provider(kind: ProviderKind) -> ProviderConfig {
        let mut p = ProviderConfig::new("p", "Test Provider", kind);
        p.timeout_secs = 5;
        p
    }

    #[test]
    fn anthropic_headers_use_x_api_key() {
        let mut p = provider(ProviderKind::Anthropic);
        p.extra_headers
            .insert("anthropic-beta".into(), "output-128k".into());
        let h = build_headers(&p, Some("sk-ant-1")).unwrap();
        assert_eq!(h.get("x-api-key").unwrap(), "sk-ant-1");
        assert_eq!(
            h.get("anthropic-version").unwrap(),
            DEFAULT_ANTHROPIC_VERSION
        );
        assert_eq!(h.get("anthropic-beta").unwrap(), "output-128k");
        assert!(h.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn openai_headers_use_bearer_and_no_version() {
        let h = build_headers(&provider(ProviderKind::OpenAICompatible), Some("sk-1")).unwrap();
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer sk-1");
        assert!(h.get("anthropic-version").is_none());
    }

    #[test]
    fn missing_key_means_no_auth_header() {
        let h = build_headers(&provider(ProviderKind::OpenAICompatible), None).unwrap();
        assert!(h.get(AUTHORIZATION).is_none());
        let h = build_headers(&provider(ProviderKind::OpenAICompatible), Some("")).unwrap();
        assert!(h.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn custom_version_and_invalid_header_names() {
        let mut p = provider(ProviderKind::Anthropic);
        p.anthropic_version = Some("2024-10-22".into());
        assert_eq!(
            build_headers(&p, None)
                .unwrap()
                .get("anthropic-version")
                .unwrap(),
            "2024-10-22"
        );
        p.extra_headers.insert("bad header".into(), "v".into());
        assert!(build_headers(&p, None).is_err());
    }

    #[test]
    fn encode_for_applies_the_model_output_cap() {
        let mut req = ChatRequest::new("sonnet-class", Dialect::Anthropic);
        req.messages.push(Message::user_text("hi"));
        req.max_tokens = Some(64_000);

        let p = provider(ProviderKind::OpenAICompatible);
        let body = encode_for(&p, &req, "deepseek-v4-pro", Some(8192)).unwrap();
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["model"], "deepseek-v4-pro");

        // No cap configured: the client value survives.
        let body = encode_for(&p, &req, "deepseek-v4-pro", None).unwrap();
        assert_eq!(body["max_tokens"], 64_000);
    }

    #[test]
    fn encode_for_respects_provider_quirks() {
        let mut req = ChatRequest::new("m", Dialect::OpenAI);
        req.messages.push(Message::user_text("hi"));
        req.max_tokens = Some(100);
        let mut p = provider(ProviderKind::OpenAICompatible);
        p.quirks = ProviderQuirks {
            use_max_completion_tokens: true,
            ..ProviderQuirks::default()
        };
        let body = encode_for(&p, &req, "gpt-5.3-sol", None).unwrap();
        assert_eq!(body["max_completion_tokens"], 100);
    }

    #[test]
    fn truncate_keeps_utf8_intact() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("中文测试内容", 3), "中文测…");
    }

    #[test]
    fn catalogue_prices_become_per_million_tokens() {
        // OpenRouter publishes dollars per single token, as strings.
        let entry = serde_json::json!({
            "id": "deepseek/deepseek-chat",
            "pricing": {
                "prompt": "0.00000027",
                "completion": "0.0000011",
                "input_cache_read": "0.00000007"
            }
        });
        let pricing = pricing_from_catalogue(&entry).unwrap();
        assert_eq!(pricing.currency, "USD");
        assert!((pricing.input_per_mtok - 0.27).abs() < 1e-9);
        assert!((pricing.output_per_mtok - 1.10).abs() < 1e-9);
        assert!((pricing.cache_read_per_mtok.unwrap() - 0.07).abs() < 1e-9);
        assert!(pricing.cache_write_per_mtok.is_none());
    }

    #[test]
    fn catalogues_without_usable_prices_are_left_alone() {
        for entry in [
            serde_json::json!({"id": "m"}),
            serde_json::json!({"id": "m", "pricing": {}}),
            serde_json::json!({"id": "m", "pricing": {"prompt": "0", "completion": "0"}}),
            serde_json::json!({"id": "m", "pricing": {"prompt": "abc", "completion": "1"}}),
        ] {
            assert!(pricing_from_catalogue(&entry).is_none(), "{entry}");
        }
    }
}
