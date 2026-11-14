//! A closed capability union. A single request must cover every scope together.
use crate::{Localized, PluginError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Principal {
    HostBinding,
    Control,
    Ui,
    Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "provider.register")]
    ProviderRegister,
    #[serde(rename = "host.operation.bind")]
    HostOperationBind,
    #[serde(rename = "credential.store")]
    CredentialStore,
    #[serde(rename = "credential.use")]
    CredentialUse,
    #[serde(rename = "credential.status")]
    CredentialStatus,
    #[serde(rename = "credential.refresh")]
    CredentialRefresh,
    #[serde(rename = "credential.clear")]
    CredentialClear,
    #[serde(rename = "client.register")]
    ClientRegister,
    #[serde(rename = "client.setup.propose")]
    ClientSetupPropose,
    #[serde(rename = "client.setup.restore")]
    ClientSetupRestore,
    #[serde(rename = "transport.profile.use")]
    TransportProfileUse,
    #[serde(rename = "transport.profile.declare")]
    TransportProfileDeclare,
    #[serde(rename = "ui.contribute")]
    UiContribute,
    #[serde(rename = "ui.projection.read")]
    UiProjectionRead,
    #[serde(rename = "host.action.request")]
    HostActionRequest,
    #[serde(rename = "options.read")]
    OptionsRead,
    #[serde(rename = "options.update")]
    OptionsUpdate,
    #[serde(rename = "diagnostic.report")]
    DiagnosticReport,
    #[serde(rename = "theme.contribute")]
    ThemeContribute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionBase {
    pub principal: Principal,
    pub contributions: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    pub reason: Localized,
}

// Flatten + deny_unknown_fields is deliberately avoided. Each enum variant is
// decoded through a closed scope struct so an irrelevant restriction cannot be ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "capability", deny_unknown_fields)]
pub enum PermissionRequest {
    #[serde(rename = "provider.register")]
    ProviderRegister {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "client.register")]
    ClientRegister {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "client.setup.propose")]
    ClientSetupPropose {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "client.setup.restore")]
    ClientSetupRestore {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "host.operation.bind")]
    HostOperationBind {
        principal: Principal,
        contributions: Vec<String>,
        operations: Vec<String>,
        origins: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "credential.store")]
    CredentialStore {
        principal: Principal,
        contributions: Vec<String>,
        credential_slots: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "credential.clear")]
    CredentialClear {
        principal: Principal,
        contributions: Vec<String>,
        credential_slots: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "credential.use")]
    CredentialUse {
        principal: Principal,
        contributions: Vec<String>,
        credential_slots: Vec<String>,
        operations: Vec<String>,
        origins: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "credential.refresh")]
    CredentialRefresh {
        principal: Principal,
        contributions: Vec<String>,
        credential_slots: Vec<String>,
        operations: Vec<String>,
        origins: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "credential.status")]
    CredentialStatus {
        principal: Principal,
        contributions: Vec<String>,
        credential_slots: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "transport.profile.use")]
    TransportProfileUse {
        principal: Principal,
        contributions: Vec<String>,
        profiles: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "transport.profile.declare")]
    TransportProfileDeclare {
        principal: Principal,
        contributions: Vec<String>,
        profiles: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "ui.contribute")]
    UiContribute {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "ui.projection.read")]
    UiProjectionRead {
        principal: Principal,
        contributions: Vec<String>,
        projections: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "host.action.request")]
    HostActionRequest {
        principal: Principal,
        contributions: Vec<String>,
        actions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "options.read")]
    OptionsRead {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "options.update")]
    OptionsUpdate {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "diagnostic.report")]
    DiagnosticReport {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
    #[serde(rename = "theme.contribute")]
    ThemeContribute {
        principal: Principal,
        contributions: Vec<String>,
        reason: Localized,
        #[serde(default)]
        optional: bool,
    },
}

macro_rules! common {
    ($self:expr, $field:ident) => {
        match $self {
            PermissionRequest::ProviderRegister { $field, .. }
            | PermissionRequest::ClientRegister { $field, .. }
            | PermissionRequest::ClientSetupPropose { $field, .. }
            | PermissionRequest::ClientSetupRestore { $field, .. }
            | PermissionRequest::HostOperationBind { $field, .. }
            | PermissionRequest::CredentialStore { $field, .. }
            | PermissionRequest::CredentialClear { $field, .. }
            | PermissionRequest::CredentialUse { $field, .. }
            | PermissionRequest::CredentialRefresh { $field, .. }
            | PermissionRequest::CredentialStatus { $field, .. }
            | PermissionRequest::TransportProfileUse { $field, .. }
            | PermissionRequest::TransportProfileDeclare { $field, .. }
            | PermissionRequest::UiContribute { $field, .. }
            | PermissionRequest::UiProjectionRead { $field, .. }
            | PermissionRequest::HostActionRequest { $field, .. }
            | PermissionRequest::OptionsRead { $field, .. }
            | PermissionRequest::OptionsUpdate { $field, .. }
            | PermissionRequest::DiagnosticReport { $field, .. }
            | PermissionRequest::ThemeContribute { $field, .. } => $field,
        }
    };
}

/// Internal operation scope, never decoded from a worker-supplied owner.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    pub contribution: &'a str,
    pub operation: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub slot: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub projection: Option<&'a str>,
    pub action: Option<&'a str>,
}

impl<'a> Scope<'a> {
    pub fn contribution(id: &'a str) -> Self {
        Self {
            contribution: id,
            operation: None,
            origin: None,
            slot: None,
            profile: None,
            projection: None,
            action: None,
        }
    }
}

impl PermissionRequest {
    pub fn capability(&self) -> Capability {
        match self {
            Self::ProviderRegister { .. } => Capability::ProviderRegister,
            Self::ClientRegister { .. } => Capability::ClientRegister,
            Self::ClientSetupPropose { .. } => Capability::ClientSetupPropose,
            Self::ClientSetupRestore { .. } => Capability::ClientSetupRestore,
            Self::HostOperationBind { .. } => Capability::HostOperationBind,
            Self::CredentialStore { .. } => Capability::CredentialStore,
            Self::CredentialClear { .. } => Capability::CredentialClear,
            Self::CredentialUse { .. } => Capability::CredentialUse,
            Self::CredentialRefresh { .. } => Capability::CredentialRefresh,
            Self::CredentialStatus { .. } => Capability::CredentialStatus,
            Self::TransportProfileUse { .. } => Capability::TransportProfileUse,
            Self::TransportProfileDeclare { .. } => Capability::TransportProfileDeclare,
            Self::UiContribute { .. } => Capability::UiContribute,
            Self::UiProjectionRead { .. } => Capability::UiProjectionRead,
            Self::HostActionRequest { .. } => Capability::HostActionRequest,
            Self::OptionsRead { .. } => Capability::OptionsRead,
            Self::OptionsUpdate { .. } => Capability::OptionsUpdate,
            Self::DiagnosticReport { .. } => Capability::DiagnosticReport,
            Self::ThemeContribute { .. } => Capability::ThemeContribute,
        }
    }
    pub fn principal(&self) -> Principal {
        *common!(self, principal)
    }
    pub fn contributions(&self) -> &[String] {
        common!(self, contributions)
    }
    pub fn optional(&self) -> bool {
        *common!(self, optional)
    }
    pub fn validate(&self) -> Result<()> {
        use crate::strict::nonempty_ids;
        let own = matches!(
            self.capability(),
            Capability::OptionsRead | Capability::OptionsUpdate | Capability::DiagnosticReport
        );
        if !common!(self, reason).valid()
            || (own && self.contributions() != ["$plugin"])
            || (!own && !nonempty_ids(self.contributions()))
        {
            return Err(PluginError::InvalidManifest);
        }
        let expected = match self.capability() {
            Capability::ThemeContribute => Principal::Theme,
            Capability::UiContribute
            | Capability::UiProjectionRead
            | Capability::HostActionRequest
            | Capability::CredentialStatus => Principal::Ui,
            Capability::OptionsRead | Capability::OptionsUpdate | Capability::DiagnosticReport => {
                if !matches!(self.principal(), Principal::Control | Principal::Ui) {
                    return Err(PluginError::InvalidManifest);
                }
                self.principal()
            }
            _ => Principal::HostBinding,
        };
        if self.principal() != expected {
            return Err(PluginError::InvalidManifest);
        }
        let valid = match self {
            Self::HostOperationBind {
                operations,
                origins,
                ..
            } => nonempty_ids(operations) && valid_origins(origins),
            Self::CredentialUse {
                operations,
                origins,
                credential_slots,
                ..
            }
            | Self::CredentialRefresh {
                operations,
                origins,
                credential_slots,
                ..
            } => {
                nonempty_ids(operations) && valid_origins(origins) && nonempty_ids(credential_slots)
            }
            Self::CredentialStore {
                credential_slots, ..
            }
            | Self::CredentialClear {
                credential_slots, ..
            }
            | Self::CredentialStatus {
                credential_slots, ..
            } => nonempty_ids(credential_slots),
            Self::TransportProfileUse { profiles, .. }
            | Self::TransportProfileDeclare { profiles, .. } => nonempty_ids(profiles),
            Self::UiProjectionRead { projections, .. } => nonempty_ids(projections),
            Self::HostActionRequest { actions, .. } => nonempty_ids(actions),
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(PluginError::InvalidManifest)
        }
    }

    pub fn covers(&self, capability: Capability, principal: Principal, scope: Scope<'_>) -> bool {
        if self.capability() != capability
            || self.principal() != principal
            || !self
                .contributions()
                .iter()
                .any(|id| id == scope.contribution)
        {
            return false;
        }
        let has = |values: &[String], value: Option<&str>| {
            value.is_some_and(|v| values.iter().any(|x| x == v))
        };
        match self {
            Self::HostOperationBind {
                operations,
                origins,
                ..
            } => has(operations, scope.operation) && has(origins, scope.origin),
            Self::CredentialUse {
                operations,
                origins,
                credential_slots,
                ..
            }
            | Self::CredentialRefresh {
                operations,
                origins,
                credential_slots,
                ..
            } => {
                has(operations, scope.operation)
                    && has(origins, scope.origin)
                    && has(credential_slots, scope.slot)
            }
            Self::CredentialStore {
                credential_slots, ..
            }
            | Self::CredentialClear {
                credential_slots, ..
            }
            | Self::CredentialStatus {
                credential_slots, ..
            } => has(credential_slots, scope.slot),
            Self::TransportProfileUse { profiles, .. }
            | Self::TransportProfileDeclare { profiles, .. } => has(profiles, scope.profile),
            Self::UiProjectionRead { projections, .. } => has(projections, scope.projection),
            Self::HostActionRequest { actions, .. } => has(actions, scope.action),
            _ => true,
        }
    }
}

fn valid_origins(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 32
        && values.iter().all(|v| {
            v.starts_with("https://")
                && v.len() <= 256
                && !v[8..].is_empty()
                && !v[8..].contains(['/', '?', '#', '@', '*', '\\', ' ', '%'])
                && !v.chars().any(char::is_control)
        })
        && values
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == values.len()
}
