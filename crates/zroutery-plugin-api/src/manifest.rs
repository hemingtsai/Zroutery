//! Manifest validation never infers a permission from the contribution kind.
use crate::capability::Scope;
use crate::provider::{ClientDescriptor, ProviderDescriptor};
use crate::ui::{HostActionBinding, UiContribution};
use crate::{strict, Capability, PermissionRequest, PluginError, Principal, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Localized {
    pub en: String,
    #[serde(rename = "zh-CN")]
    pub zh_cn: String,
    #[serde(rename = "zh-TW")]
    pub zh_tw: String,
}
impl Localized {
    pub fn valid(&self) -> bool {
        [&self.en, &self.zh_cn, &self.zh_tw]
            .iter()
            .all(|v| strict::text(v, 512))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiVersion {
    pub major: u16,
    pub min_minor: u16,
}
impl ApiVersion {
    pub const V1: Self = Self {
        major: 1,
        min_minor: 0,
    };
    pub fn supported(&self) -> bool {
        self.major == 1 && self.min_minor == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub heap_bytes: usize,
    pub stack_bytes: usize,
    pub active_js_ms: u64,
    pub wall_ms: u64,
    pub host_calls: usize,
    pub ipc_bytes: usize,
}
impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            heap_bytes: 16 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            active_js_ms: 50,
            wall_ms: 200,
            host_calls: 16,
            ipc_bytes: 1024 * 1024,
        }
    }
}
impl ResourceLimits {
    pub fn bounded(self) -> Result<Self> {
        let max = Self::default();
        if self.heap_bytes == 0
            || self.stack_bytes == 0
            || self.active_js_ms == 0
            || self.wall_ms == 0
            || self.host_calls == 0
            || self.ipc_bytes == 0
        {
            return Err(PluginError::InvalidManifest);
        }
        Ok(Self {
            heap_bytes: self.heap_bytes.min(max.heap_bytes),
            stack_bytes: self.stack_bytes.min(max.stack_bytes),
            active_js_ms: self.active_js_ms.min(max.active_js_ms),
            wall_ms: self.wall_ms.min(max.wall_ms),
            host_calls: self.host_calls.min(max.host_calls),
            ipc_bytes: self.ipc_bytes.min(max.ipc_bytes),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDeclaration {
    pub engine: Engine,
    pub entries: RuntimeEntries,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Quickjs,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEntries {
    #[serde(default)]
    pub control: Option<String>,
    #[serde(default)]
    pub ui: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub service: String,
    pub api: ApiVersion,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileContribution {
    pub id: String,
    pub file: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    #[serde(default)]
    pub providers: Vec<ProviderDescriptor>,
    #[serde(default)]
    pub clients: Vec<ClientDescriptor>,
    #[serde(default)]
    pub themes: Vec<FileContribution>,
    #[serde(default)]
    pub transport_profiles: Vec<FileContribution>,
    #[serde(default)]
    pub ui: Vec<UiContribution>,
    #[serde(default)]
    pub actions: Vec<HostActionBinding>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestV1 {
    pub manifest_version: u16,
    pub id: String,
    pub version: String,
    pub name: Localized,
    pub host_api: ApiVersion,
    pub runtime: Option<RuntimeDeclaration>,
    pub requires: Vec<Requirement>,
    pub permissions: Vec<PermissionRequest>,
    pub contributions: Contributions,
    #[serde(default)]
    pub options_schema: Option<String>,
    #[serde(default)]
    pub diagnostics: BTreeMap<String, Localized>,
    #[serde(default)]
    pub limits: Option<ResourceLimits>,
}

impl PluginManifestV1 {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = strict::decode(bytes, strict::MAX_FRAME_BYTES)
            .map_err(|_| PluginError::InvalidManifest)?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn declares(&self, capability: Capability, principal: Principal, scope: Scope<'_>) -> bool {
        self.permissions
            .iter()
            .any(|p| p.covers(capability, principal, scope))
    }
    fn require(
        &self,
        capability: Capability,
        principal: Principal,
        scope: Scope<'_>,
    ) -> Result<()> {
        if self.declares(capability, principal, scope) {
            Ok(())
        } else {
            Err(PluginError::InvalidManifest)
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.manifest_version != 1
            || !self.host_api.supported()
            || !strict::local_id(&self.id)
            || !self.id.contains('.')
            || !strict::text(&self.version, 32)
            || !self.name.valid()
            || self.permissions.len() > 128
            || self.requires.len() > 64
        {
            return Err(PluginError::InvalidManifest);
        }
        if let Some(limits) = self.limits {
            limits.bounded()?;
        }
        if let Some(runtime) = &self.runtime {
            if runtime.entries.control.is_none() && runtime.entries.ui.is_none() {
                return Err(PluginError::InvalidManifest);
            }
            for path in [&runtime.entries.control, &runtime.entries.ui]
                .into_iter()
                .flatten()
            {
                if !strict::package_path(path) || !path.ends_with(".mjs") {
                    return Err(PluginError::InvalidManifest);
                }
            }
        }
        if self
            .options_schema
            .as_deref()
            .is_some_and(|path| !strict::package_path(path) || !path.ends_with(".json"))
        {
            return Err(PluginError::InvalidManifest);
        }
        // Options validation and migration run in the control facet; a schema
        // without that entry could never be honoured.
        if self.options_schema.is_some()
            && self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.entries.control.as_ref())
                .is_none()
        {
            return Err(PluginError::InvalidManifest);
        }
        let c = &self.contributions;
        let mut ids = BTreeSet::new();
        for id in c
            .providers
            .iter()
            .map(|x| &x.id)
            .chain(c.clients.iter().map(|x| &x.id))
            .chain(c.themes.iter().map(|x| &x.id))
            .chain(c.transport_profiles.iter().map(|x| &x.id))
            .chain(c.ui.iter().map(|x| &x.id))
            .chain(c.actions.iter().map(HostActionBinding::id))
        {
            if !strict::local_id(id) || !ids.insert(id) || ids.len() > 128 {
                return Err(PluginError::InvalidManifest);
            }
        }
        if ids.is_empty() {
            return Err(PluginError::InvalidManifest);
        }
        for p in &self.permissions {
            p.validate()?;
            if p.contributions()
                .iter()
                .any(|id| id != "$plugin" && !ids.contains(id))
            {
                return Err(PluginError::InvalidManifest);
            }
            // Reject cross-contribution scopes rather than letting unrelated
            // slot/operation names imply a grant exists in another provider.
            let value = serde_json::to_value(p).map_err(|_| PluginError::InvalidManifest)?;
            for contribution in p.contributions() {
                let provider = c
                    .providers
                    .iter()
                    .find(|provider| &provider.id == contribution);
                if let Some(values) = value.get("credential_slots").and_then(|v| v.as_array()) {
                    let Some(provider) = provider else {
                        return Err(PluginError::InvalidManifest);
                    };
                    if values.iter().any(|slot| {
                        !provider
                            .credentials
                            .iter()
                            .any(|s| Some(s.id.as_str()) == slot.as_str())
                    }) {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                if let Some(values) = value.get("operations").and_then(|v| v.as_array()) {
                    let Some(provider) = provider else {
                        return Err(PluginError::InvalidManifest);
                    };
                    if values.iter().any(|operation| {
                        !provider
                            .operations
                            .iter()
                            .any(|o| Some(o.id.as_str()) == operation.as_str())
                    }) {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                if let Some(values) = value.get("actions").and_then(|v| v.as_array()) {
                    if !c.ui.iter().any(|ui| &ui.id == contribution)
                        || values.iter().any(|action| {
                            !c.actions
                                .iter()
                                .any(|a| Some(a.id().as_str()) == action.as_str())
                        })
                    {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                match p.capability() {
                    Capability::ProviderRegister if provider.is_none() => {
                        return Err(PluginError::InvalidManifest)
                    }
                    Capability::ClientRegister
                    | Capability::ClientSetupPropose
                    | Capability::ClientSetupRestore
                        if !c.clients.iter().any(|client| &client.id == contribution) =>
                    {
                        return Err(PluginError::InvalidManifest)
                    }
                    Capability::ThemeContribute
                        if !c.themes.iter().any(|theme| &theme.id == contribution) =>
                    {
                        return Err(PluginError::InvalidManifest)
                    }
                    Capability::UiContribute | Capability::UiProjectionRead
                        if !c.ui.iter().any(|ui| &ui.id == contribution) =>
                    {
                        return Err(PluginError::InvalidManifest)
                    }
                    _ => {}
                }
            }
        }
        for p in &c.providers {
            self.require(
                Capability::ProviderRegister,
                Principal::HostBinding,
                Scope::contribution(&p.id),
            )?;
            if !p.name.valid()
                || p.operations.is_empty()
                || p.operations.len() > 16
                || p.credentials.len() > 32
            {
                return Err(PluginError::InvalidManifest);
            }
            let slots: BTreeSet<_> = p.credentials.iter().map(|s| s.id.as_str()).collect();
            let ops: BTreeSet<_> = p.operations.iter().map(|o| o.id.as_str()).collect();
            if slots.len() != p.credentials.len() || ops.len() != p.operations.len() {
                return Err(PluginError::InvalidManifest);
            }
            for slot in &p.credentials {
                if !strict::local_id(&slot.id)
                    || !slot.name.valid()
                    || !slot.auth_service.api.supported()
                {
                    return Err(PluginError::InvalidManifest);
                }
                self.require(
                    Capability::CredentialStore,
                    Principal::HostBinding,
                    Scope {
                        slot: Some(&slot.id),
                        ..Scope::contribution(&p.id)
                    },
                )?;
            }
            for o in &p.operations {
                if !strict::local_id(&o.id)
                    || !o.service.api.supported()
                    || o.base_url.len() > 512
                    || o.credential_slots
                        .iter()
                        .any(|s| !slots.contains(s.as_str()))
                {
                    return Err(PluginError::InvalidManifest);
                }
                let origin = static_origin(&o.base_url).ok_or(PluginError::InvalidManifest)?;
                let scope = Scope {
                    operation: Some(&o.id),
                    origin: Some(origin),
                    ..Scope::contribution(&p.id)
                };
                self.require(Capability::HostOperationBind, Principal::HostBinding, scope)?;
                for slot in &o.credential_slots {
                    self.require(
                        Capability::CredentialUse,
                        Principal::HostBinding,
                        Scope {
                            slot: Some(slot),
                            ..scope
                        },
                    )?;
                }
            }
            if let Some(profile) = &p.transport_profile {
                self.require(
                    Capability::TransportProfileUse,
                    Principal::HostBinding,
                    Scope {
                        profile: Some(profile),
                        ..Scope::contribution(&p.id)
                    },
                )?;
            }
        }
        for client in &c.clients {
            if !client.name.valid() {
                return Err(PluginError::InvalidManifest);
            }
            self.require(
                Capability::ClientRegister,
                Principal::HostBinding,
                Scope::contribution(&client.id),
            )?;
        }
        for file in c.themes.iter().chain(&c.transport_profiles) {
            if !strict::package_path(&file.file) || !file.file.ends_with(".json") {
                return Err(PluginError::InvalidManifest);
            }
        }
        for theme in &c.themes {
            self.require(
                Capability::ThemeContribute,
                Principal::Theme,
                Scope::contribution(&theme.id),
            )?;
        }
        for profile in &c.transport_profiles {
            self.require(
                Capability::TransportProfileDeclare,
                Principal::HostBinding,
                Scope {
                    profile: Some(&profile.id),
                    ..Scope::contribution(&profile.id)
                },
            )?;
        }
        for ui in &c.ui {
            ui.validate()?;
            if self
                .runtime
                .as_ref()
                .and_then(|r| r.entries.ui.as_ref())
                .is_none()
            {
                return Err(PluginError::InvalidManifest);
            }
            self.require(
                Capability::UiContribute,
                Principal::Ui,
                Scope::contribution(&ui.id),
            )?;
            if let Some(projection) = &ui.projection {
                self.require(
                    Capability::UiProjectionRead,
                    Principal::Ui,
                    Scope {
                        projection: Some(projection),
                        ..Scope::contribution(&ui.id)
                    },
                )?;
            }
        }
        for action in &c.actions {
            action.validate(self)?;
        }
        if self.diagnostics.len() > 64
            || self
                .diagnostics
                .iter()
                .any(|(id, text)| !strict::local_id(id) || !text.valid())
        {
            return Err(PluginError::InvalidManifest);
        }
        Ok(())
    }
}

/// Only splits a static HTTPS origin. Host additionally uses its URL parser,
/// service path schema and pinned DNS checks before creating any transport.
pub fn static_origin(url: &str) -> Option<&str> {
    if !url.starts_with("https://")
        || url.contains(['@', '?', '#', '\\', '%'])
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let end = url[8..].find('/').map(|i| i + 8).unwrap_or(url.len());
    if end == 8 {
        None
    } else {
        Some(&url[..end])
    }
}
