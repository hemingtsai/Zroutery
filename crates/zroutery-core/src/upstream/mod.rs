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
mod encode;
mod errors;
mod headers;
mod json;
mod stream;

#[cfg(test)]
#[allow(deprecated)]
mod tests;

pub use catalogue::DiscoveredModel;
pub use encode::{encode_for, encode_for_mode, EncodeMode};
pub use headers::{build_headers, build_headers_for_identity, build_headers_for_profile};

pub(super) const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const USER_AGENT: &str = concat!("zroutery/", env!("CARGO_PKG_VERSION"));
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
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        .http1_only()
        // Never follow a redirect. reqwest strips Authorization across hosts
        // but not custom headers, and the Anthropic key rides in x-api-key, so
        // a 3xx would hand the key to whatever host it names; a downgrade to
        // plain http would also put it on the wire in the clear.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT);
    if bypass_proxy || std::env::var("ZROUTERY_NO_PROXY").is_ok() {
        builder = builder.no_proxy();
    }
    builder.build().unwrap_or_else(|e| {
        tracing::error!("falling back to a default HTTP client: {e}");
        reqwest::Client::new()
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
}
