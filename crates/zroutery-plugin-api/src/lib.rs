//! Declarative plugin contracts. No network, GUI, credential backend or JS engine.
//!
//! Only the four control/UI calls in [`ipc`] can cross the worker boundary.
//! Protocols, request bodies, observations and settlement stay in Rust Core.

pub mod capability;
pub mod ipc;
pub mod manifest;
pub mod options;
pub mod provider;
pub mod strict;
pub mod theme;
pub mod transport;
pub mod ui;

pub use capability::{Capability, PermissionRequest, Principal};
pub use manifest::{ApiVersion, Localized, PluginManifestV1, ResourceLimits};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error,
)]
#[serde(rename_all = "snake_case")]
pub enum PluginError {
    #[error("permission_denied")]
    PermissionDenied,
    #[error("unsupported_host_service")]
    UnsupportedHostService,
    #[error("invalid_manifest")]
    InvalidManifest,
    #[error("invalid_result")]
    InvalidResult,
    #[error("stale_revision")]
    StaleRevision,
    #[error("plugin_busy")]
    PluginBusy,
    #[error("plugin_timeout")]
    PluginTimeout,
    #[error("plugin_memory_limit")]
    PluginMemoryLimit,
    #[error("plugin_crashed")]
    PluginCrashed,
    #[error("sandbox_unavailable")]
    SandboxUnavailable,
    #[error("credential_unavailable")]
    CredentialUnavailable,
    #[error("storage_unavailable")]
    StorageUnavailable,
}

pub type Result<T> = std::result::Result<T, PluginError>;
