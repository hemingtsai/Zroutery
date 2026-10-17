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

        let keys = stored
            .providers
            .iter()
            .map(|p| (p.id.clone(), self.secrets.has(&p.key_ref)))
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
