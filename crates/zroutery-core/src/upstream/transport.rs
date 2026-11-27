//! Native transport is unchanged. Plugin operations get pinned direct DNS, an
//! isolated pool and a broker poll boundary; no request/response enters JS IPC.
//!
//! # The second client
//!
//! The OpenCode free tier admits one TLS ClientHello — the BoringSSL shape its
//! own runtime presents — and refuses everything else with a quota-flavored
//! 429, whatever the headers or body say (measured 2026-11-07, reproduced
//! 2026-11-27: Schannel, OpenSSL and a matched rustls configuration all fail).
//! `reqwest` cannot present that handshake, so providers whose resolved client
//! profile is [`ProviderClientProfile::OpenCodeCli`] are sent by `wreq`, which
//! is compiled only on Windows and macOS: those targets link no OpenSSL, so a
//! second TLS library cannot collide with the system one. On Linux the profile
//! answers with a clear unavailability error instead of a request the gateway is
//! known to refuse.
use super::response::UpstreamResponse;
use super::Upstream;
use crate::config::{ClientIdentity, ProviderClientProfile, ProviderConfig};
use crate::error::{Error, Result};
use std::time::Duration;

/// Whether this build carries the fingerprint transport at all.
const FINGERPRINT_TRANSPORT: bool = cfg!(all(
    feature = "opencode",
    any(target_os = "windows", target_os = "macos")
));

/// A request built for the client that must carry it.
pub(super) enum PreparedRequest {
    Reqwest(reqwest::RequestBuilder),
    #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
    Fingerprint(wreq::RequestBuilder),
}

impl PreparedRequest {
    pub(super) fn json(self, body: &serde_json::Value) -> Self {
        match self {
            Self::Reqwest(builder) => Self::Reqwest(builder.json(body)),
            #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
            Self::Fingerprint(builder) => Self::Fingerprint(builder.json(body)),
        }
    }
}

/// The ClientHello the gateway admits, copied from the verified probe.
///
/// Every name here was field-verified: a TLS 1.3 suite list that leads with the
/// AES suites, the post-quantum curve first, OCSP stapling and signed
/// certificate timestamps on, GREASE off, extension order fixed. Changing a
/// value changes the fingerprint and the gateway's verdict, so this is data
/// pinned to a measurement, not tuning knobs.
#[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
const FINGERPRINT_CIPHERS: &str = "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES128-SHA:ECDHE-ECDSA-AES256-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA:AES256-SHA";
#[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
const FINGERPRINT_SIGALGS: &str = "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:rsa_pss_rsae_sha512:rsa_pkcs1_sha512:ed25519:ecdsa_secp521r1_sha512:rsa_pkcs1_sha1";

/// Whether this request must go over the fingerprint transport.
fn needs_fingerprint(identity: &ClientIdentity, url: &str) -> bool {
    identity.profile == ProviderClientProfile::OpenCodeCli && url.starts_with("https://")
}

impl Upstream {
    pub(super) async fn request_for(
        &self,
        provider: &ProviderConfig,
        url: &str,
        method: reqwest::Method,
        native_key: Option<&str>,
        identity: &ClientIdentity,
    ) -> Result<PreparedRequest> {
        #[cfg(feature = "plugin")]
        let plugin = provider.plugin_binding.is_some();
        #[cfg(not(feature = "plugin"))]
        let plugin = false;
        tracing::debug!(
            provider = %provider.name,
            profile = identity.profile.as_str(),
            plugin,
            fingerprint = needs_fingerprint(identity, url),
            "prepared upstream request"
        );
        // A build without the second client fails loudly here instead of
        // sending a request the gateway refuses with a misleading quota error.
        if !FINGERPRINT_TRANSPORT && needs_fingerprint(identity, url) {
            return Err(Error::NoCandidate(
                "the OpenCode free tier needs the BoringSSL fingerprint transport, \
                 which this build does not carry"
                    .into(),
            ));
        }
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
            let key = binding
                .selected_slot(&provider.key_ref, operation)?
                .map(|slot| binding.credential(&slot.key_ref, operation))
                .transpose()?
                .flatten();
            // Caller-supplied native keys/extra headers cannot change this
            // plugin operation's owner, slot or auth injection.
            let headers = super::build_headers_for_identity(provider, key.as_deref(), identity)?;
            #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
            if needs_fingerprint(identity, url) {
                let client = fingerprint_client(provider, host, &addresses)?;
                return Ok(PreparedRequest::Fingerprint(
                    client.request(method, parsed).headers(headers),
                ));
            }
            let client = reqwest::Client::builder()
                .no_proxy()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(provider.connect_timeout_secs))
                .pool_max_idle_per_host(0)
                .build()
                .map_err(|_| Error::NoCandidate("plugin transport unavailable".into()))?;
            return Ok(PreparedRequest::Reqwest(
                client.request(method, parsed).headers(headers),
            ));
        }
        #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
        if needs_fingerprint(identity, url) {
            // Native shaped providers need the same SSRF boundary the plugin
            // path has: public addresses only, pinned for this request.
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| Error::NoCandidate("upstream endpoint denied".into()))?;
            let host = parsed
                .host_str()
                .ok_or_else(|| Error::NoCandidate("upstream endpoint denied".into()))?;
            let port = parsed.port_or_known_default().unwrap_or(443);
            let addresses: Vec<_> = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| Error::NoCandidate("upstream DNS resolution failed".into()))?
                .collect();
            if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
                return Err(Error::NoCandidate(
                    "upstream destination address denied".into(),
                ));
            }
            let headers = super::build_headers_for_identity(provider, native_key, identity)?;
            let client = fingerprint_client(provider, host, &addresses)?;
            return Ok(PreparedRequest::Fingerprint(
                client.request(method, parsed).headers(headers),
            ));
        }
        Ok(PreparedRequest::Reqwest(
            self.client_for(provider).request(method, url).headers(
                super::build_headers_for_identity(provider, native_key, identity)?,
            ),
        ))
    }

    pub(super) async fn execute_request(
        &self,
        provider: &ProviderConfig,
        url: &str,
        request: PreparedRequest,
    ) -> Result<UpstreamResponse> {
        let request = match request {
            PreparedRequest::Reqwest(request) => request,
            #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
            PreparedRequest::Fingerprint(request) => {
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
                        binding
                            .poll_send_fingerprint(operation, &origin, &mut || {
                                future.as_mut().poll(cx).map(|r| {
                                    r.map_err(|source| Error::FingerprintTransport {
                                        provider: provider.name.clone(),
                                        message: source.to_string(),
                                    })
                                })
                            })
                            .map(|r| r.map(UpstreamResponse::from))
                    })
                    .await;
                }
                return future.await.map(UpstreamResponse::from).map_err(|source| {
                    Error::FingerprintTransport {
                        provider: provider.name.clone(),
                        message: source.to_string(),
                    }
                });
            }
        };
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
            .await
            .map(UpstreamResponse::from);
        }
        #[cfg(not(feature = "plugin"))]
        let _ = url;
        future
            .await
            .map(UpstreamResponse::from)
            .map_err(|source| Error::Transport {
                provider: provider.name.clone(),
                source,
            })
    }
}

#[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
fn fingerprint_client(
    provider: &ProviderConfig,
    host: &str,
    addresses: &[std::net::SocketAddr],
) -> Result<wreq::Client> {
    use wreq::tls::AlpnProtos;
    use wreq::{EmulationProvider, SslCurve, TlsConfig};
    let tls = TlsConfig::builder()
        .alpn_protos(AlpnProtos::HTTP1)
        .grease_enabled(false)
        .permute_extensions(false)
        .curves(vec![
            SslCurve::X25519_MLKEM768,
            SslCurve::X25519,
            SslCurve::SECP256R1,
            SslCurve::SECP384R1,
        ])
        .enable_ocsp_stapling(true)
        .enable_signed_cert_timestamps(true)
        .cipher_list(FINGERPRINT_CIPHERS)
        .sigalgs_list(FINGERPRINT_SIGALGS)
        .build();
    let emulation = EmulationProvider::builder().tls_config(tls).build();
    wreq::Client::builder()
        .emulation(emulation)
        .no_proxy()
        .http1_only()
        .redirect(wreq::redirect::Policy::none())
        .resolve_to_addrs(host, addresses)
        .connect_timeout(Duration::from_secs(provider.connect_timeout_secs))
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|_| Error::NoCandidate("fingerprint transport unavailable".into()))
}

#[cfg(any(
    feature = "plugin",
    all(feature = "opencode", any(target_os = "windows", target_os = "macos"))
))]
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
