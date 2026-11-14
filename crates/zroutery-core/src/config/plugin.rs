//! Persistent plugin declarations remain passive in a build without `plugin`.
//! Never export them as native providers: old binaries must have nothing to send.
use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: u16 = 2;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInstanceConfig {
    pub id: String,
    pub name: String,
    pub installation_id: String,
    pub contribution_id: String,
    pub package_digest: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub credentials: Vec<PluginCredentialBinding>,
    #[serde(default)]
    pub accounts: Vec<super::AccountConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCredentialBinding {
    pub slot_id: String,
    pub slot_uuid: String,
    pub credential_revision: u64,
    pub account_generation: Option<String>,
    pub key_ref: String,
}
