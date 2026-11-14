//! Host components only. No markup, URL resources, style or invoke forwarding.
use crate::capability::Scope;
use crate::manifest::PluginManifestV1;
use crate::{strict, Capability, Localized, PluginError, Principal, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiSlot {
    #[serde(rename = "overview.extension")]
    Overview,
    #[serde(rename = "provider.detail.extension")]
    Provider,
    #[serde(rename = "model.detail.extension")]
    Model,
    #[serde(rename = "client.detail.extension")]
    Client,
    #[serde(rename = "usage.extension")]
    Usage,
    #[serde(rename = "settings.extension")]
    Settings,
    #[serde(rename = "settings.extensions.page")]
    Page,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiContribution {
    pub id: String,
    pub slot: UiSlot,
    pub title: Localized,
    #[serde(default)]
    pub projection: Option<String>,
    pub render_hook: String,
    #[serde(default)]
    pub action_hook: Option<String>,
}
impl UiContribution {
    pub fn validate(&self) -> Result<()> {
        // These projections are host-published schemas, never AppSnapshot/raw JSON.
        if !self.title.valid()
            || !strict::local_id(&self.render_hook)
            || self
                .action_hook
                .as_deref()
                .is_some_and(|v| !strict::local_id(v))
            || self.projection.as_deref().is_some_and(|v| {
                !matches!(
                    v,
                    "connection_status"
                        | "catalogue_summary"
                        | "account_summary"
                        | "usage_summary"
                        | "own_options"
                )
            })
        {
            return Err(PluginError::InvalidManifest);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostActionBinding {
    RefreshCatalog {
        id: String,
        provider: String,
        operation: String,
    },
    RefreshAccount {
        id: String,
        provider: String,
        operation: String,
    },
    StartSignIn {
        id: String,
        provider: String,
        credential_slot: String,
    },
    StoreCredential {
        id: String,
        provider: String,
        credential_slot: String,
    },
    ForgetCredential {
        id: String,
        provider: String,
        credential_slot: String,
    },
    PreviewClientSetup {
        id: String,
        client: String,
    },
    RestoreClientSetup {
        id: String,
        client: String,
    },
    SaveOptions {
        id: String,
    },
}
impl HostActionBinding {
    pub fn id(&self) -> &String {
        match self {
            Self::RefreshCatalog { id, .. }
            | Self::RefreshAccount { id, .. }
            | Self::StartSignIn { id, .. }
            | Self::StoreCredential { id, .. }
            | Self::ForgetCredential { id, .. }
            | Self::PreviewClientSetup { id, .. }
            | Self::RestoreClientSetup { id, .. }
            | Self::SaveOptions { id } => id,
        }
    }
    pub fn validate(&self, manifest: &PluginManifestV1) -> Result<()> {
        let declared = |cap, principal, scope| manifest.declares(cap, principal, scope);
        let valid = match self {
            Self::RefreshCatalog {
                provider,
                operation,
                ..
            }
            | Self::RefreshAccount {
                provider,
                operation,
                ..
            } => manifest
                .contributions
                .providers
                .iter()
                .find(|p| &p.id == provider)
                .is_some_and(|p| p.operations.iter().any(|o| &o.id == operation)),
            Self::StartSignIn {
                provider,
                credential_slot,
                ..
            }
            | Self::StoreCredential {
                provider,
                credential_slot,
                ..
            }
            | Self::ForgetCredential {
                provider,
                credential_slot,
                ..
            } => {
                let cap = if matches!(self, Self::ForgetCredential { .. }) {
                    Capability::CredentialClear
                } else {
                    Capability::CredentialStore
                };
                manifest
                    .contributions
                    .providers
                    .iter()
                    .find(|p| &p.id == provider)
                    .is_some_and(|p| p.credentials.iter().any(|s| &s.id == credential_slot))
                    && declared(
                        cap,
                        Principal::HostBinding,
                        Scope {
                            slot: Some(credential_slot),
                            ..Scope::contribution(provider)
                        },
                    )
            }
            Self::PreviewClientSetup { client, .. } | Self::RestoreClientSetup { client, .. } => {
                let cap = if matches!(self, Self::RestoreClientSetup { .. }) {
                    Capability::ClientSetupRestore
                } else {
                    Capability::ClientSetupPropose
                };
                manifest
                    .contributions
                    .clients
                    .iter()
                    .any(|c| &c.id == client && c.setup.is_some())
                    && declared(cap, Principal::HostBinding, Scope::contribution(client))
            }
            Self::SaveOptions { .. } => {
                manifest.options_schema.is_some()
                    && declared(
                        Capability::OptionsUpdate,
                        Principal::Ui,
                        Scope::contribution("$plugin"),
                    )
            }
        };
        if valid {
            Ok(())
        } else {
            Err(PluginError::InvalidManifest)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiNode {
    Stack {
        children: Vec<UiNode>,
    },
    Text {
        text: String,
    },
    Notice {
        tone: NoticeTone,
        text: String,
    },
    Value {
        label: String,
        value: String,
    },
    Button {
        label: String,
        action_id: String,
    },
    Input {
        field_id: String,
        label: String,
        value: String,
    },
    Select {
        field_id: String,
        label: String,
        value: String,
        options: Vec<SelectOption>,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeTone {
    Info,
    Warning,
    Error,
}
impl UiNode {
    pub fn validate(&self, actions: &[String], fields: &[String]) -> Result<()> {
        fn visit(
            node: &UiNode,
            depth: usize,
            count: &mut usize,
            actions: &[String],
            fields: &[String],
        ) -> bool {
            *count += 1;
            if depth > 16 || *count > 500 {
                return false;
            }
            let txt = |s: &str| s.len() <= 2048 && !s.chars().any(|c| c.is_control() && c != '\n');
            match node {
                UiNode::Stack { children } => children
                    .iter()
                    .all(|n| visit(n, depth + 1, count, actions, fields)),
                UiNode::Text { text } | UiNode::Notice { text, .. } => txt(text),
                UiNode::Value { label, value } => txt(label) && txt(value),
                UiNode::Button { label, action_id } => txt(label) && actions.contains(action_id),
                UiNode::Input {
                    field_id,
                    label,
                    value,
                } => fields.contains(field_id) && txt(label) && txt(value),
                UiNode::Select {
                    field_id,
                    label,
                    value,
                    options,
                } => {
                    fields.contains(field_id)
                        && txt(label)
                        && txt(value)
                        && options.len() <= 64
                        && options.iter().all(|o| txt(&o.label) && txt(&o.value))
                }
                UiNode::Table { columns, rows } => {
                    !columns.is_empty()
                        && columns.len() <= 16
                        && rows.len() <= 100
                        && columns.iter().all(|c| txt(c))
                        && rows
                            .iter()
                            .all(|r| r.len() == columns.len() && r.iter().all(|v| txt(v)))
                }
            }
        }
        if visit(self, 0, &mut 0, actions, fields) {
            Ok(())
        } else {
            Err(PluginError::InvalidResult)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiEffect {
    OptionsProposal {
        expected_revision: String,
        options: BTreeMap<String, serde_json::Value>,
    },
    HostAction {
        action_id: String,
    },
    None,
}

/// Frozen non-secret projection schema. No arbitrary host snapshot or upstream payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<String>,
}
