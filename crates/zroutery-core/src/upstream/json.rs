//! The non-streaming JSON calls: catalogue and balance.

use std::time::Duration;

use serde_json::Value;

use super::catalogue::extract_model_list;
use super::errors::truncate;
use super::{build_headers, DiscoveredModel, Upstream};
use crate::billing::{Balance, BalanceProbe};
use crate::config::ProviderConfig;
use crate::error::{Error, Result};
impl Upstream {
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
        let mut models = extract_model_list(&json, &provider.name);
        // A provider's `/models` route answers names and sometimes prices. The
        // public catalogue supplies what it usually omits — capabilities, the
        // context and output windows, and the reasoning-effort levels a client
        // picker may offer. Manual corrections happen after import and are not
        // touched here.
        if let Err(e) = crate::models_dev::enrich(&mut models, provider).await {
            tracing::warn!(provider = %provider.name, "models.dev enrichment failed: {e}");
        }
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
            self.client_for(provider)
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
                retry_after: None,
            });
        }
        serde_json::from_str(&text)
            .map_err(|e| Error::BadUpstreamPayload(format!("{e}: {}", truncate(&text, 500))))
    }
}
