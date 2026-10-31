//! Versioned account binding. A local account ID is never a remote identity.

use serde::{Deserialize, Serialize};

pub const BINDING_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingVerification {
    Verified,
    #[default]
    Pending,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIdentity {
    pub panel_root: String,
    pub user_id: String,
    pub username: Option<String>,
    pub group: Option<String>,
    pub verified_at: i64,
    /// Exact verification endpoint/transport, not decoded token claims.
    pub source: String,
}

impl RemoteIdentity {
    pub fn same_user(&self, other: &Self) -> bool {
        self.panel_root.trim_end_matches('/') == other.panel_root.trim_end_matches('/')
            && self.user_id == other.user_id
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBinding {
    /// UUID per declaration incarnation. Recreated accounts get a fresh one.
    pub generation: String,
    pub credential_revision: u64,
    pub identity_revision: u64,
    pub verification: BindingVerification,
    pub identity: Option<RemoteIdentity>,
    pub reason: Option<String>,
}

impl AccountBinding {
    pub fn fresh() -> Self {
        Self {
            generation: uuid::Uuid::new_v4().simple().to_string(),
            ..Default::default()
        }
    }

    pub fn version(&self) -> String {
        format!(
            "{}:{}:{}",
            self.generation, self.credential_revision, self.identity_revision
        )
    }
}

/// Explicit probe kind, orthogonal to the inference protocol. Legacy account
/// declarations are migrated visibly; an OpenAI endpoint is not itself NewAPI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountProbeKind {
    #[default]
    None,
    NewApi,
}

/// Inference and panel roots are distinct. The exact AgentRouter redirect was
/// supplied/confirmed by the operator; it is not a generic compatible-endpoint
/// heuristic or permission to trust arbitrary redirects.
pub fn panel_root(base: &str, explicit: Option<&str>) -> String {
    if let Some(explicit) = explicit.map(str::trim).filter(|root| !root.is_empty()) {
        return strip_panel_suffixes(explicit);
    }
    let root = strip_panel_suffixes(base);
    if root.eq_ignore_ascii_case("https://ps.air-outer.com") {
        "https://agentrouter.org".into()
    } else {
        root
    }
}

fn strip_panel_suffixes(base: &str) -> String {
    let mut root = base.trim().trim_end_matches('/').to_string();
    loop {
        if let Some(next) = root
            .strip_suffix("/v1")
            .or_else(|| root.strip_suffix("/api"))
        {
            root = next.trim_end_matches('/').to_string();
        } else {
            return root;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_exact_confirmed_relay_maps_to_its_panel() {
        assert_eq!(
            panel_root("https://ps.air-outer.com/v1", None),
            "https://agentrouter.org"
        );
        assert_eq!(
            panel_root(
                "https://ps.air-outer.com/api/v1",
                Some("https://custom.example/api")
            ),
            "https://custom.example"
        );
        for base in [
            "https://ps.air-outer.com.evil.test/v1",
            "http://ps.air-outer.com/v1",
            "https://another.example/v1",
        ] {
            assert_ne!(panel_root(base, None), "https://agentrouter.org");
        }
    }
}
