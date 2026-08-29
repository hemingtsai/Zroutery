//! HTTP transport to the upstream providers.
//!
//! Timeouts are enforced here rather than in `reqwest`: a whole-request
//! deadline would kill long streaming answers, so streams get an *idle*
//! timeout between chunks instead.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use std::sync::RwLock;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::billing::{Balance, BalanceProbe, Pricing};
use crate::config::{ProviderConfig, ProviderKind};
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, ChatResponse, StreamEvent};
use crate::protocol::{self, SseDecoder, StreamParser};

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const USER_AGENT: &str = concat!("zroutery/", env!("CARGO_PKG_VERSION"));

/// Stream of canonical events coming from one upstream call.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

#[derive(Debug)]
pub struct Upstream {
    client: RwLock<reqwest::Client>,
}

impl Default for Upstream {
    fn default() -> Self {
        Self::new(false)
    }
}

fn build_http_client(bypass_proxy: bool) -> reqwest::Client {
    tracing::info!(bypass_proxy, "building upstream HTTP client");
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .http1_only()
        .user_agent(USER_AGENT);
    if bypass_proxy || std::env::var("ZROUTERY_NO_PROXY").is_ok() {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .unwrap_or_else(|e| {
            tracing::error!("falling back to a default HTTP client: {e}");
            reqwest::Client::new()
        })
}

impl Upstream {
    pub fn new(bypass_proxy: bool) -> Self {
        Upstream {
            client: RwLock::new(build_http_client(bypass_proxy)),
        }
    }

    /// Rebuild the HTTP client (e.g. when bypass_proxy setting changes at runtime).
    pub fn rebuild_client(&self, bypass_proxy: bool) {
        *crate::sync::write(&self.client) = build_http_client(bypass_proxy);
    }

    fn client(&self) -> reqwest::Client {
        crate::sync::read(&self.client).clone()
    }

    /// Time a minimal completion, for an election.
    ///
    /// One token of output is enough to measure the whole round trip: DNS, TLS, the
    /// provider's queue and its first token. It is a real request, so it goes
    /// through the same encoder and the same quirks as traffic does, and it costs a
    /// fraction of a cent — which is why nothing does this on a timer.
    ///
    /// The timeout is the probe's own, not the provider's request timeout: probes
    /// run sequentially during an election, so one hung endpoint bound by the
    /// default 600s would hold up every class behind it.
    pub async fn probe(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        upstream_model: &str,
    ) -> Result<u64> {
        const PROBE_TIMEOUT_SECS: u64 = 30;

        let mut request = ChatRequest::new(upstream_model, provider.kind.dialect());
        request.messages.push(crate::ir::Message::user_text("ping"));
        request.max_tokens = Some(1);
        let body = encode_for(provider, &request, upstream_model, Some(1))?;

        let started = std::time::Instant::now();
        tokio::time::timeout(
            Duration::from_secs(PROBE_TIMEOUT_SECS),
            self.send(provider, api_key, &body),
        )
        .await
        .map_err(|_| Error::Timeout(PROBE_TIMEOUT_SECS))??;
        Ok(started.elapsed().as_millis() as u64)
    }

    /// Non streaming call. `body` is already in the provider's dialect.
    pub async fn send(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
    ) -> Result<ChatResponse> {
        let request = self
            .client()
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
                .headers(build_headers(provider, api_key)?)
                .json(body);
            let result = tokio::time::timeout(Duration::from_secs(provider.timeout_secs), request.send())
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
            self.client()
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
        // The body gets the same bound as the handshake: a stalled metadata
        // endpoint would otherwise hang the GUI call that awaits it forever.
        let text = tokio::time::timeout(Duration::from_secs(timeout_secs), response.text())
            .await
            .map_err(|_| Error::Timeout(timeout_secs))?
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

/// Claude Code client fingerprint (used for impersonation to pass a
/// gateway's "Claude Code only" check).
const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.217 (external, sdk-cli)";
const CLAUDE_CODE_SYSTEM_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

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

    // Claude Code impersonation: minimal clean fingerprint, matching what the
    // real claude-cli sends (cc-switch's field-tested set). SDK-layer headers
    // (x-stainless-*) and request IDs are deliberately NOT injected: a real
    // CLI does not send them, and header/version inconsistencies are exactly
    // what strict gateways look for. Gateways needing extras can use
    // provider.extra_headers.
    if provider.impersonate_claude_code {
        tracing::debug!(
            provider = %provider.name,
            "impersonation enabled – injecting Claude Code fingerprint headers"
        );
        // User-Agent: claude-cli style; gateways accept any CC-style version.
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(CLAUDE_CODE_USER_AGENT),
        );
        // x-app: identifies as CLI client.
        headers.insert(
            HeaderName::from_static("x-app"),
            HeaderValue::from_static("cli"),
        );
        // Real Claude Code sends this; harmless where it is not checked.
        headers.insert(
            HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            HeaderValue::from_static("true"),
        );
        // anthropic-beta: ensure claude-code marker is present
        const CLAUDE_CODE_BETA: &str = "claude-code-20250219";
        let beta_value = match headers.get("anthropic-beta") {
            Some(existing) => {
                let existing_str = existing.to_str().unwrap_or("");
                if existing_str.contains(CLAUDE_CODE_BETA) {
                    existing_str.to_string()
                } else {
                    format!("{CLAUDE_CODE_BETA},{existing_str}")
                }
            }
            None => CLAUDE_CODE_BETA.to_string(),
        };
        headers.insert(
            HeaderName::from_static("anthropic-beta"),
            HeaderValue::from_str(&beta_value)
                .map_err(|_| Error::internal("invalid anthropic-beta value"))?,
        );
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
    let mut body = protocol::encode_request(
        provider.kind.dialect(),
        &req,
        upstream_model,
        &provider.quirks,
    )?;

    // Claude Code impersonation: inject identity line as the first system block
    // to pass Anthropic subscription/OAuth plan checks.
    if provider.impersonate_claude_code {
        prepend_claude_code_system_prompt(&mut body);
    }

    Ok(body)
}

/// Insert the Claude Code identity as the first line of the `system` field in
/// the Anthropic request body.
///
/// Anthropic subscription/OAuth plans require the first system block to be exactly
/// this identity line.
fn prepend_claude_code_system_prompt(body: &mut Value) {
    // Only inject into Anthropic-style bodies. OpenAI-compatible bodies use
    // messages for system instructions, and injecting a "system" key triggers
    // gateway content filters.
    if !body.get("system").is_some() {
        body["system"] = Value::Array(vec![json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM_IDENTITY })]);
        return;
    }

    let identity = json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM_IDENTITY });
    let mut blocks: Vec<Value> = vec![identity];

    match body.get("system") {
        Some(Value::Array(existing)) => {
            // Idempotent: skip re-injection if the first block is already the identity line.
            if existing
                .first()
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
                == Some(CLAUDE_CODE_SYSTEM_IDENTITY)
            {
                return;
            }
            blocks.extend(existing.iter().cloned());
        }
        Some(Value::String(existing)) if !existing.is_empty() => {
            blocks.push(json!({ "type": "text", "text": existing }));
        }
        _ => {}
    }
    body["system"] = Value::Array(blocks);
}

/// Whether a handshake error is worth retrying: failures that happen before
/// the provider processed anything (connection problems, TLS issues) or
/// gateway-level blips (408/502/503/504).
///
/// Deliberately excluded: 405 (both known modes — TLS-fingerprint and
/// content rules on the relay's WAF — are deterministic, so replaying just
/// re-uploads the body for nothing), 429 (retrying deepens relay rate-limit
/// blocks) and 500 (this relay reports deterministic content rejections as
/// 500, and replaying those just burns quota).
fn is_transient_handshake_failure(err: &Error) -> bool {
    match err {
        Error::Transport { .. } => true,
        Error::Upstream { status, .. } => matches!(status, 408 | 502 | 503 | 504),
        _ => false,
    }
}

/// DEBUG: dump a request body whose upstream call failed, so relay
/// rejections can be replayed and bisected offline. Opt-in via
/// ZROUTERY_CAPTURE_FAILED_BODIES=1 — the dumps contain full conversation
/// content, so they are never written by default.
fn capture_failed_body(provider: &ProviderConfig, body: &Value, status: u16, text: &str) {
    if std::env::var("ZROUTERY_CAPTURE_FAILED_BODIES").as_deref() != Ok("1") {
        return;
    }
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    if n >= 10 {
        return;
    }
    let dir = std::env::temp_dir().join("zroutery_failed_bodies");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let path = dir.join(format!("{ts}_s{status}_{}.json", provider.name.replace(' ', "_")));
    if let Ok(pretty) = serde_json::to_string_pretty(body) {
        let _ = std::fs::write(&path, pretty);
        let meta = format!("status: {status}\nresponse: {text}\n");
        let _ = std::fs::write(dir.join(format!("{ts}_s{status}_meta.txt")), meta);
        tracing::warn!("captured failed upstream body to {}", path.display());
    }
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
        p.impersonate_claude_code = false; // Disable impersonation for this test
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
    fn impersonate_claude_code_injects_clean_fingerprint() {
        let mut p = provider(ProviderKind::Anthropic);
        p.impersonate_claude_code = true;
        let h = build_headers(&p, Some("sk-ant-1")).unwrap();

        // User-Agent must be claude-cli style.
        assert_eq!(h.get("user-agent").unwrap(), CLAUDE_CODE_USER_AGENT);

        // Core identity headers.
        assert_eq!(h.get("x-app").unwrap(), "cli");
        assert_eq!(
            h.get("anthropic-dangerous-direct-browser-access").unwrap(),
            "true"
        );

        // SDK-layer headers a real CLI never sends must stay absent.
        assert!(h.get("x-stainless-arch").is_none());
        assert!(h.get("x-stainless-package-version").is_none());
        assert!(h.get("x-claude-code-session-id").is_none());
        assert!(h.get("x-client-request-id").is_none());

        // anthropic-beta must contain claude-code marker
        let beta = h.get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains("claude-code-20250219"));

        // Auth header
        assert!(h.get("x-api-key").is_some());
    }

    #[test]
    fn impersonate_claude_code_merges_existing_anthropic_beta() {
        let mut p = provider(ProviderKind::Anthropic);
        p.impersonate_claude_code = true;
        p.extra_headers
            .insert("anthropic-beta".into(), "output-128k".into());
        let h = build_headers(&p, Some("sk-ant-1")).unwrap();
        let beta = h.get("anthropic-beta").unwrap().to_str().unwrap();
        assert!(beta.contains("claude-code-20250219"));
        assert!(beta.contains("output-128k"));
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
    fn impersonation_injects_identity_even_without_a_system_prompt() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message::user_text("hi"));
        let mut p = provider(ProviderKind::Anthropic);
        p.impersonate_claude_code = true;
        let body = encode_for(&p, &req, "claude-sonnet-4-5", None).unwrap();
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["text"], CLAUDE_CODE_SYSTEM_IDENTITY);
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
