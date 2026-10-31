//! HTTP transport to the upstream providers.
//!
//! Timeouts are enforced here rather than in `reqwest`: a whole-request
//! deadline would kill long streaming answers, so streams get an *idle*
//! timeout between chunks instead.
//!
//! # Why this is seven files
//!
//! One `impl Upstream` ran to 380 lines and sat next to catalogue parsing,
//! header construction and request shaping, none of which touch the client.
//! The `opencode` gates are the other half of the reason: they were spread
//! across all four concerns, so a build that enabled the feature had to keep
//! four unrelated places honest.
//!
//! `Upstream` is a split file, so its inherent methods are too, and rustc
//! merges the blocks. This table is the index:
//!
//! | file | holds |
//! |---|---|
//! | `stream` | `stream`, `stream_with_identity`, and the per-chunk state |
//! | `json` | `list_models`, `fetch_balance`, `get_json` |
//! | `catalogue` | `DiscoveredModel` and the shapes catalogues arrive in |
//! | `headers` | the client fingerprints and every `build_headers*` |
//! | `encode` | `EncodeMode`, `encode_for`, `encode_for_mode` |
//! | `errors` | reading an upstream refusal, and the body truncation |
//!
//! `new`, `rebuild_client`, `probe` and the two unary sends stay here: they
//! are the entry points, and the rest are reached through them.

use std::pin::Pin;
use std::time::Duration;

use std::sync::RwLock;

use futures_util::Stream;
use serde_json::Value;

use crate::config::{ClientIdentity, ProviderConfig};
use crate::error::{Error, Result};
use crate::ir::{ChatRequest, ChatResponse, StreamEvent};
use crate::protocol::{self};

use errors::truncate;
use headers::anonymous_identity;

mod catalogue;
mod decision;
mod encode;
mod errors;

pub(crate) use errors::{retry_advice, MAX_TOTAL_RETRIES};
mod headers;
mod json;
mod stream;

#[cfg(test)]
#[allow(deprecated)]
mod tests;

#[cfg(test)]
mod client_tests;

pub use catalogue::DiscoveredModel;
pub use decision::{
    build_body as build_decision_body, parse_answer as parse_decision_answer,
    question_of as decision_question, Decision, BLOCK_POLARITY,
};
pub use encode::{encode_for, encode_for_mode, EncodeMode};
pub use headers::{build_headers, build_headers_for_identity, build_headers_for_profile};

pub(super) const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const USER_AGENT: &str = concat!("zroutery/", env!("CARGO_PKG_VERSION"));
/// The wait an upstream asked for before the next attempt.
///
/// `Retry-After` is either seconds or an HTTP date; `x-ms-retry-after-ms` is a
/// millisecond count some relays send instead. The retry policy treats the
/// answer as a floor, never as a reason to retry early.
pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let millis = ["retry-after-ms", "x-ms-retry-after-ms"]
        .iter()
        .filter_map(|name| {
            headers
                .get(*name)?
                .to_str()
                .ok()?
                .trim()
                .parse::<f64>()
                .ok()
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| Duration::from_secs_f64((value / 1000.0).min(1e12)))
        .max();
    let Some(value) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    else {
        return millis;
    };
    let seconds = value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| Duration::from_secs_f64(v.min(1e12)));
    let date = chrono::DateTime::parse_from_rfc2822(value).ok().map(|at| {
        (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .unwrap_or_default()
    });
    millis.into_iter().chain(seconds).chain(date).max()
}

/// Stream of canonical events coming from one upstream call.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

#[derive(Debug)]
pub struct Upstream {
    client: RwLock<reqwest::Client>,
    connect_timeout_secs: u64,
}

impl Default for Upstream {
    fn default() -> Self {
        Self::new(false, 15)
    }
}

fn build_http_client(bypass_proxy: bool, connect_timeout_secs: u64) -> reqwest::Client {
    tracing::info!(
        bypass_proxy,
        connect_timeout_secs,
        "building upstream HTTP client"
    );
    // Read once: the variable is allowed to change, but one client must not be
    // half-proxied. `rebuild_client` is how a runtime change reaches the pool.
    let direct = bypass_proxy || std::env::var("ZROUTERY_NO_PROXY").is_ok();
    // The properties below are the client's contract, not tuning: HTTP/1.1
    // only, no redirect following, and the proxy decision. They are applied to
    // the fallback too, so a client that failed to build its TLS pool is still
    // not a client that follows a 3xx to another host with `x-api-key` on it.
    let configure = |builder: reqwest::ClientBuilder, pooled: bool| {
        let mut builder = builder
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .http1_only()
            // Never follow a redirect. reqwest strips Authorization across
            // hosts but not custom headers, and the Anthropic key rides in
            // x-api-key, so a 3xx would hand the key to whatever host it names;
            // a downgrade to plain http would also put it on the wire in the
            // clear.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT);
        if pooled {
            builder = builder
                .pool_idle_timeout(Duration::from_secs(90))
                // A busy provider used to be served by an unbounded idle pool:
                // every finished connection sat there until the idle timeout,
                // and fifty concurrent streams to one host held fifty file
                // descriptors for that long. Sixteen is comfortably above the
                // concurrency a single desktop instance generates.
                .pool_max_idle_per_host(16)
                // Keepalive probes let a dead relay be noticed between
                // requests rather than by the next request timing out.
                .tcp_keepalive(Duration::from_secs(60));
        }
        if direct {
            builder = builder.no_proxy();
        }
        builder
    };

    configure(reqwest::Client::builder(), true)
        .build()
        .unwrap_or_else(|e| {
            // The A3 plan said `expect`. This keeps a usable process instead:
            // a panic in `AppState::new` would take the desktop shell down at
            // startup, while the degraded client still refuses redirects and
            // still speaks HTTP/1.1 — the two properties whose loss would be a
            // security incident rather than a performance regression. The log
            // is an error, so the fallback is never quiet.
            tracing::error!("falling back to an unpooled HTTP client: {e}");
            configure(reqwest::Client::builder(), false)
                .build()
                .expect("the fallback client must build when TLS does")
        })
}

impl Upstream {
    pub fn new(bypass_proxy: bool, connect_timeout_secs: u64) -> Self {
        Upstream {
            client: RwLock::new(build_http_client(bypass_proxy, connect_timeout_secs)),
            connect_timeout_secs,
        }
    }

    /// Rebuild the HTTP client (e.g. when bypass_proxy setting changes at runtime).
    pub fn rebuild_client(&self, bypass_proxy: bool) {
        *crate::sync::write(&self.client) =
            build_http_client(bypass_proxy, self.connect_timeout_secs);
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

        let mut request = ChatRequest::new(upstream_model, provider.dialect());
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
        let identity = anonymous_identity(provider);
        self.send_with_identity(provider, api_key, body, &identity)
            .await
    }

    /// Non-streaming call for one already resolved client identity.
    ///
    /// The identity only decides which fingerprint is presented and which of the
    /// client's own headers are forwarded; it never controls auth. Resolving it
    /// upstream of this call and passing it in means the handshake retries and
    /// every candidate attempt present the identical client.
    pub async fn send_with_identity(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
        client: &ClientIdentity,
    ) -> Result<ChatResponse> {
        let request = self
            .client()
            .post(provider.chat_url())
            .headers(build_headers_for_identity(provider, api_key, client)?)
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
        client.reply_headers.capture(response.headers());
        let retry_after = retry_after(response.headers());
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
                retry_after,
            });
        }

        let json: Value = serde_json::from_str(&text)
            .map_err(|e| Error::BadUpstreamPayload(format!("{e}: {}", truncate(&text, 500))))?;
        if let Some(error) = Error::session_binding_payload(&provider.name, &json) {
            return Err(error);
        }
        protocol::decode_response(provider.dialect(), json)
    }

    /// Ask a decision endpoint for an answer.
    ///
    /// A decision endpoint is not a chat endpoint: it takes a state and a set
    /// of typed questions, and answers with probabilities. So the body is the
    /// decision shape rather than the provider's dialect, the url is the
    /// decision path rather than the chat path, and what comes back is a
    /// probability rather than a message.
    ///
    /// The answer is returned as it arrived. Turning a probability into the
    /// verdict the Auto Mode expects is the Auto Mode's business, not the
    /// transport's.
    pub async fn send_decision(
        &self,
        provider: &ProviderConfig,
        api_key: Option<&str>,
        body: &Value,
    ) -> Result<Decision> {
        let identity = anonymous_identity(provider);
        let request = self
            .client()
            .post(provider.decision.url(&provider.base_url))
            .headers(build_headers_for_identity(provider, api_key, &identity)?)
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
        let retry_after = retry_after(response.headers());
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
                retry_after,
            });
        }

        let json: Value = serde_json::from_str(&text)
            .map_err(|e| Error::BadUpstreamPayload(format!("{e}: {}", truncate(&text, 500))))?;
        decision::parse_answer(&json).ok_or_else(|| {
            Error::BadUpstreamPayload(format!(
                "the decision endpoint answered with no probability: {}",
                truncate(&text, 500)
            ))
        })
    }
}
