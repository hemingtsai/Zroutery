//! Persistent configuration: providers, model registry entries, routing policy
//! and local server settings.
//!
//! Secrets are never stored here. A provider only holds a `key_ref` that is
//! resolved through a [`SecretStore`] (OS keychain in the desktop app).
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`model`] | [`ModelTier`], [`NamingStyle`] and the [`ModelCapabilities`] a model declares |
//! | [`provider`] | [`ProviderKind`], the client profiles an upstream needs, and how an inbound request is attributed to one |
//! | [`registry`] | the configured providers, accounts, maintenance windows and model entries |
//! | [`routing`] | how a tier picks among candidates, plus the rectifier, classifier and vision layers |
//! | [`server`] | the local server and shadow-routing knobs, as configured rather than as running |
//! | [`ml`] | whether a promoted model may re-order the plan, and where its durable state lives |
//! | [`app`] | [`AppConfig`] itself, with the window and notification behaviour it carries |
//! | [`validate`] | what [`AppConfig::validate`] checks, and the issues it reports |
//! | [`secrets`] | [`SecretStore`] — where credentials come from, never the configuration file |
//!
//! The split follows the document: each child is one section of what gets
//! written to disk, and `validate` is the only child that reads all of them.

pub use crate::protocol::ProviderQuirks;
mod app;
mod ml;
mod model;
mod provider;
mod registry;
mod routing;
mod secrets;
mod server;
mod validate;

#[cfg(test)]
mod tests;

pub use app::{AppConfig, NotificationConfig, WindowBehavior};
pub use ml::MlRoutingConfig;
pub use model::{ModelCapabilities, ModelTier, NamingStyle};
pub use provider::{
    passthrough_denied, resolve_client_identity, ClientIdentity, ProviderClientProfile,
    ProviderKind, PASSTHROUGH_DENIED,
};
pub use registry::{qualified_id, AccountConfig, MaintenanceConfig, ModelEntry, ProviderConfig};
pub use routing::{
    ClassifierCandidate, ClassifierConfig, RectifierConfig, RoutingConfig, RoutingStrategy,
    VisionConfig,
};
pub use secrets::{MemorySecretStore, SecretStore};
pub use server::{ServerConfig, ShadowConfig};
pub use validate::{ConfigIssue, IssueSeverity};
