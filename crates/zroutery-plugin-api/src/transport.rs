//! Finite TLS/HTTP knobs. No verifier, socket, SNI, proxy or CA escape options.
use crate::{strict, PluginError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportProfileV1 {
    pub schema_version: u16,
    pub id: String,
    pub target: Target,
    pub tls: TlsProfile,
    pub http: HttpProfile,
    pub identity_headers: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub family: String,
    pub version: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TlsVersion {
    #[serde(rename = "1.2")]
    V12,
    #[serde(rename = "1.3")]
    V13,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsProfile {
    pub min_version: TlsVersion,
    pub max_version: TlsVersion,
    pub cipher_suites: Vec<u16>,
    pub groups: Vec<u16>,
    pub signature_algorithms: Vec<u16>,
    pub extension_order: Vec<String>,
    pub alpn: Vec<Alpn>,
    pub grease: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Alpn {
    #[serde(rename = "http/1.1")]
    H1,
    #[serde(rename = "h2")]
    H2,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpProfile {
    #[serde(default)]
    pub h1: Option<Http1>,
    #[serde(default)]
    pub h2: Option<Http2>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http1 {
    pub header_order: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http2 {
    pub settings: Vec<H2Setting>,
    pub pseudo_header_order: Vec<String>,
    pub connection_window: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct H2Setting {
    pub id: u16,
    pub value: u32,
}

pub fn allowed_header(name: &str) -> bool {
    matches!(
        name,
        "user-agent"
            | "accept"
            | "accept-language"
            | "accept-encoding"
            | "x-ide-version"
            | "x-client-version"
            | "x-client-platform"
            | "x-client-product"
            | "x-requested-with"
            | "x-no-user-id"
    )
}
impl TransportProfileV1 {
    pub fn validate(&self) -> Result<()> {
        let unique = |v: &[u16]| {
            !v.is_empty() && v.len() <= 64 && v.iter().collect::<BTreeSet<_>>().len() == v.len()
        };
        let tls = &self.tls;
        if self.schema_version != 1
            || !strict::local_id(&self.id)
            || !strict::text(&self.target.family, 64)
            || !strict::text(&self.target.version, 64)
            || tls.min_version > tls.max_version
            || !unique(&tls.cipher_suites)
            || !unique(&tls.groups)
            || !unique(&tls.signature_algorithms)
            || tls.cipher_suites.iter().any(|v| {
                !matches!(
                    v,
                    0x1301..=0x1303 | 0xc02b | 0xc02c | 0xc02f | 0xc030 | 0xcca8 | 0xcca9
                )
            })
            || tls
                .groups
                .iter()
                .any(|v| !matches!(v, 23..=25 | 29 | 30 | 4588))
            || tls
                .signature_algorithms
                .iter()
                .any(|v| !matches!(v, 0x0403 | 0x0503 | 0x0603 | 0x0804..=0x080b))
            || tls.alpn.is_empty()
            || tls.alpn.len() > 2
            || tls.extension_order.len() > 32
            || tls.extension_order.iter().any(|v| {
                !matches!(
                    v.as_str(),
                    "server_name"
                        | "supported_groups"
                        | "signature_algorithms"
                        | "alpn"
                        | "supported_versions"
                        | "key_share"
                        | "psk_modes"
                        | "session_ticket"
                        | "status_request"
                        | "compress_certificate"
                        | "grease"
                )
            })
            || self.identity_headers.len() > 4
            || self.identity_headers.iter().any(|(name, value)| {
                !allowed_header(name) || !strict::text(value, 256) || value.contains(['\r', '\n'])
            })
        {
            return Err(PluginError::InvalidManifest);
        }
        if let Some(h1) = &self.http.h1 {
            if h1.header_order.len() > 16 || h1.header_order.iter().any(|h| !allowed_header(h)) {
                return Err(PluginError::InvalidManifest);
            }
        }
        if let Some(h2) = &self.http.h2 {
            if h2.settings.len() > 6
                || h2
                    .settings
                    .iter()
                    .any(|s| !(1..=6).contains(&s.id) || s.value > 16 * 1024 * 1024)
                || h2
                    .settings
                    .iter()
                    .map(|s| s.id)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != h2.settings.len()
                || h2.connection_window > 16 * 1024 * 1024
                || h2.pseudo_header_order.len() != 4
                || h2.pseudo_header_order.iter().collect::<BTreeSet<_>>().len() != 4
                || h2
                    .pseudo_header_order
                    .iter()
                    .any(|s| !matches!(s.as_str(), ":method" | ":authority" | ":scheme" | ":path"))
            {
                return Err(PluginError::InvalidManifest);
            }
        }
        Ok(())
    }
}
