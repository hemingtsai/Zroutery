//! The one call the dashboard polls: everything on screen, in one document.

use zroutery_core::config::IssueSeverity;

use super::views::*;
use super::Desktop;
use crate::store;

impl Desktop {
    pub async fn snapshot(&self) -> Snapshot {
        let stored = self.core.config();
        let issues = stored.validate();
        let blocking = issues.iter().any(|i| i.severity == IssueSeverity::Error);
        let running_addr = self
            .server
            .lock()
            .await
            .as_ref()
            .map(|s| s.addr.to_string());

        // Provider keys under the provider id; an account's own reference is
        // its entry, so the drawer can show "has its own key" without the
        // secret itself ever crossing the bridge. A declaration that shares
        // the provider's reference adds no entry: the provider's row already
        // answers whether that secret exists. Inference keys are listed under
        // their own reference, because an account may hold several and each is
        // an independently replaceable secret.
        let keys = stored
            .providers
            .iter()
            .flat_map(|p| {
                let mut entries = vec![(p.id.clone(), self.secrets.has(&p.key_ref))];
                for a in &p.accounts {
                    if !a.key_ref.is_empty() && a.key_ref != p.key_ref {
                        entries.push((a.key_ref.clone(), self.secrets.has(&a.key_ref)));
                    }
                    for key in &a.inference_keys {
                        if !key.key_ref.is_empty() {
                            entries.push((key.key_ref.clone(), self.secrets.has(&key.key_ref)));
                        }
                    }
                }
                entries
            })
            .collect();

        // The webview gets everything except the token itself.
        let mut config = (*stored).clone();
        config.server.auth_token = String::new();

        Snapshot {
            server: ServerStatus {
                running: running_addr.is_some(),
                base_url: running_addr.as_ref().map(|a| format!("http://{a}")),
                address: running_addr,
                host: stored.server.host.clone(),
                port: stored.server.port,
                require_auth: stored.server.require_auth,
                token_hint: token_hint(&stored.server.auth_token),
                exposed: stored.server.is_exposed(),
            },
            keys,
            issues,
            blocking,
            health: self.core.router().health_snapshot(),
            summary: self.core.stats().summary(),
            recent: self.core.stats().recent(200),
            warning: self.warning(),
            config_path: self.config_dir.join(store::FILE_NAME).display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            exposed_ids: config.exposed_ids(),
            balances: self.balances(),
            election: self.core.router().election(),
            budgets: self.budget_status(),
            ml_available: cfg!(feature = "ml"),
            account_maintenance_available: cfg!(feature = "account-maint"),
            account_available: cfg!(feature = "account"),
            // The HTTP check-in needs both halves: the report views of
            // `account-maint` and the panel adapter of `newapi`. Declared for the
            // same reason the other two are — a command that was never registered
            // and a command that failed are the same event to the webview.
            api_checkin_available: cfg!(all(feature = "account-maint", feature = "newapi")),
            // The capture needs the browser registry *and* the account
            // credential path; `account-maint` does not imply the desktop
            // `account` feature.
            token_capture_available: cfg!(all(
                feature = "account-maint",
                feature = "account",
                feature = "newapi"
            )),
            // The OpenCode provider kinds and the `opencode_cli` profile are
            // configuration, not code, so they survive every build — but the
            // shaping that makes OpenCode's gateway accept a request does not.
            // A build without the feature can therefore be *told* to use them
            // and will have every request refused.
            opencode_available: cfg!(feature = "opencode"),
            #[cfg(feature = "account-maint")]
            checkin: self.checkin_views().await,
            #[cfg(feature = "account-maint")]
            account_events: self.account_event_views(),
            #[cfg(feature = "newapi")]
            account_balances: self.account_balance_views(),
            config,
        }
    }
}
