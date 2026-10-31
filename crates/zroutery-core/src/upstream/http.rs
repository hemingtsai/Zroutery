//! Separate pooled clients let one upstream follow the system proxy without
//! changing the network policy of its siblings or an in-flight request.

use std::time::Duration;

use crate::config::ProviderConfig;

#[derive(Debug)]
pub(super) struct HttpClients {
    inherited_bypass: bool,
    forced_direct: bool,
    direct: reqwest::Client,
    system: reqwest::Client,
}

impl HttpClients {
    pub(super) fn new(inherited_bypass: bool, connect_timeout_secs: u64) -> Self {
        // Read once for the whole pool pair. A retry keeps its client's policy;
        // rebuild_client is the explicit boundary for a changed environment.
        let forced_direct = std::env::var("ZROUTERY_NO_PROXY").is_ok();
        let direct = build_client(true, connect_timeout_secs);
        let system = if forced_direct {
            direct.clone()
        } else {
            build_client(false, connect_timeout_secs)
        };
        Self {
            inherited_bypass,
            forced_direct,
            direct,
            system,
        }
    }

    pub(super) fn for_provider(&self, provider: &ProviderConfig) -> reqwest::Client {
        if self.forced_direct || provider.bypass_proxy.unwrap_or(self.inherited_bypass) {
            self.direct.clone()
        } else {
            self.system.clone()
        }
    }

    /// An isolated socket fixture, never an environment/OS proxy mutation.
    #[cfg(test)]
    pub(super) fn with_test_proxy(inherited_bypass: bool, proxy: &str) -> Self {
        Self {
            inherited_bypass,
            forced_direct: false,
            direct: build_client(true, 5),
            system: configure(false, 5, true)
                .no_proxy()
                .proxy(reqwest::Proxy::all(proxy).unwrap())
                .build()
                .unwrap(),
        }
    }
}

fn configure(direct: bool, connect_timeout_secs: u64, pooled: bool) -> reqwest::ClientBuilder {
    // These are the transport's contract, including on the unpooled fallback:
    // no redirect can leak x-api-key, and both policies stay HTTP/1.1 only.
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(super::USER_AGENT);
    if pooled {
        builder = builder
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(16)
            .tcp_keepalive(Duration::from_secs(60));
    }
    if direct {
        builder = builder.no_proxy();
    }
    builder
}

fn build_client(direct: bool, connect_timeout_secs: u64) -> reqwest::Client {
    tracing::info!(
        bypass_proxy = direct,
        connect_timeout_secs,
        "building upstream HTTP client"
    );
    configure(direct, connect_timeout_secs, true)
        .build()
        .unwrap_or_else(|error| {
            tracing::error!("falling back to an unpooled HTTP client: {error}");
            configure(direct, connect_timeout_secs, false)
                .build()
                .expect("the fallback client must build when TLS does")
        })
}
