//! Native transport is unchanged. Plugin operations get pinned direct DNS, an
//! isolated pool and a broker poll boundary; no request/response enters JS IPC.
use super::Upstream;
use crate::config::ProviderConfig;
use crate::error::{Error, Result};
#[cfg(feature = "plugin")]
use std::time::Duration;

impl Upstream {
    pub(super) async fn request_for(
        &self,
        provider: &ProviderConfig,
        url: &str,
        method: reqwest::Method,
        native_key: Option<&str>,
        identity: &crate::config::ClientIdentity,
    ) -> Result<reqwest::RequestBuilder> {
        #[cfg(feature = "plugin")]
        if let Some(binding) = &provider.plugin_binding {
            let operation = binding.operation_for_url(url)?;
            binding.authorize(operation)?;
            if binding.profile().is_some() {
                return Err(Error::NoCandidate(
                    "plugin TLS profile backend is unavailable".into(),
                ));
            }
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| Error::NoCandidate("plugin endpoint denied".into()))?;
            if parsed.scheme() != "https"
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(Error::NoCandidate("plugin endpoint denied".into()));
            }
            let host = parsed
                .host_str()
                .ok_or_else(|| Error::NoCandidate("plugin endpoint denied".into()))?;
            let port = parsed.port_or_known_default().unwrap_or(443);
            let addresses: Vec<_> = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| Error::NoCandidate("plugin DNS resolution failed".into()))?
                .collect();
            if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
                return Err(Error::NoCandidate(
                    "plugin destination address denied".into(),
                ));
            }
            // Re-resolution happens at each new operation/retry. Pin EVERY
            // candidate, preserve the URL host for TLS verification/SNI, and do
            // not hand the checked domain to a proxy for remote re-resolution.
            let client = reqwest::Client::builder()
                .no_proxy()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(provider.connect_timeout_secs))
                .pool_max_idle_per_host(0)
                .build()
                .map_err(|_| Error::NoCandidate("plugin transport unavailable".into()))?;
            let key = binding
                .selected_slot(&provider.key_ref, operation)?
                .map(|slot| binding.credential(&slot.key_ref, operation))
                .transpose()?
                .flatten();
            // Caller-supplied native keys/extra headers cannot change this
            // plugin operation's owner, slot or auth injection.
            let headers = super::build_headers_for_identity(provider, key.as_deref(), identity)?;
            return Ok(client.request(method, parsed).headers(headers));
        }
        Ok(self.client_for(provider).request(method, url).headers(
            super::build_headers_for_identity(provider, native_key, identity)?,
        ))
    }

    pub(super) async fn execute_request(
        &self,
        provider: &ProviderConfig,
        url: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let future = request.send();
        tokio::pin!(future);
        #[cfg(feature = "plugin")]
        if let Some(binding) = &provider.plugin_binding {
            let operation = binding.operation_for_url(url)?;
            let origin = reqwest::Url::parse(url)
                .map_err(|_| Error::NoCandidate("plugin endpoint denied".into()))?
                .origin()
                .ascii_serialization();
            return futures_util::future::poll_fn(|cx| {
                use std::future::Future;
                binding.poll_send(operation, &origin, &mut || {
                    future.as_mut().poll(cx).map(|r| {
                        r.map_err(|source| Error::Transport {
                            provider: provider.name.clone(),
                            source,
                        })
                    })
                })
            })
            .await;
        }
        #[cfg(not(feature = "plugin"))]
        let _ = url;
        future.await.map_err(|source| Error::Transport {
            provider: provider.name.clone(),
            source,
        })
    }
}

#[cfg(feature = "plugin")]
pub fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && matches!(b, 18 | 19))
                && !(a == 192 && b == 0)
                && !(a == 192 && b == 88 && c == 99)
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(v4.into());
            }
            let first = ip.segments()[0];
            !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast()
                && first & 0xfe00 != 0xfc00 && first & 0xffc0 != 0xfe80
                // Only ordinary global unicast; reject transition, documentation
                // and special routing prefixes rather than guessing an exception.
                && first & 0xe000 == 0x2000 && !(first == 0x2001 && ip.segments()[1] <= 0x1ff)
                && !(first == 0x2001 && ip.segments()[1] == 0xdb8)
                && first != 0x2002
        }
    }
}

#[cfg(all(test, feature = "plugin"))]
mod tests {
    use super::*;
    #[test]
    fn ssrf_special_and_mapped_addresses_fail_closed() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("1.1.1.1".parse().unwrap()));
    }
}
