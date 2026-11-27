//! Static service references; the host registry owns methods, codecs and effects.
use crate::{ApiVersion, Localized};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostServiceRef {
    pub id: String,
    pub api: ApiVersion,
}

/// P0 freezes v1 service options as a closed set, not an arbitrary JSON object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceOptions {
    #[serde(default)]
    pub bearer_auth: bool,
    #[serde(default)]
    pub stream_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostOperationBinding {
    pub id: String,
    pub service: HostServiceRef,
    pub base_url: String,
    pub credential_slots: Vec<String>,
    #[serde(default)]
    pub service_options: ServiceOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    AppToken,
    OauthBundle,
    SessionCookie,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialScope {
    ProviderInstance,
    Account,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStorage {
    HostKeyring,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Acquisition {
    HostSecretForm,
    HostAuth {
        service: HostServiceRef,
        operation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSlotDeclaration {
    pub id: String,
    pub name: Localized,
    pub kind: CredentialKind,
    pub scope: CredentialScope,
    pub storage: CredentialStorage,
    pub acquisition: Acquisition,
    pub auth_service: HostServiceRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: Localized,
    pub operations: Vec<HostOperationBinding>,
    pub credentials: Vec<CredentialSlotDeclaration>,
    #[serde(default)]
    pub transport_profile: Option<String>,
    /// Static, non-secret identity headers merged into every request this
    /// provider sends: client-look headers (`user-agent`, `accept`, the
    /// gateway's `x-client-*` family), never auth or framing. Values are
    /// bounded text; names come from the host's published allowlist
    /// (`transport::allowed_header`), the same closed set transport profiles
    /// use, so auth/framing names can never travel through it.
    #[serde(default)]
    pub extra_headers: std::collections::BTreeMap<String, String>,
    /// The host shaping profile applied to this provider's upstream requests:
    /// per-conversation session generation and injection, the profile's
    /// identity headers, environment-block completion, and pre-use catalog
    /// registration. Values come from the host's published set — v1 carries
    /// `"opencode_cli"` alone; anything else fails validation. A provider
    /// that selects a shaping keeps its `extra_headers`, which sit UNDER the
    /// profile's own fingerprint in the header precedence.
    #[serde(default)]
    pub shaping: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientDescriptor {
    pub id: String,
    pub name: Localized,
    #[serde(default)]
    pub ingress: Option<ClientIngress>,
    #[serde(default)]
    pub setup: Option<ClientSetup>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIngress {
    pub route_suffix: String,
    pub service: HostServiceRef,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSetup {
    pub target_schema: String,
    pub service: HostServiceRef,
    pub managed_slots: Vec<ManagedSlot>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSlot {
    GatewayUrl,
    GatewayKey,
    DefaultModel,
    ClassCatalog,
}
