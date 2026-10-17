//! Declared accounts, their maintenance and check-in state, and the browsers this process holds open.

#[cfg(feature = "account-maint")]
use super::helpers::lock;
#[cfg(any(feature = "account-maint", feature = "newapi"))]
use super::views::*;

#[cfg(feature = "newapi")]
use super::views::panel_root;
use super::Desktop;

impl Desktop {
    /// Built from the *declared* accounts rather than from whatever sessions
    /// happen to exist, so an account nobody has tried yet still appears — with
    /// phase `idle`, which is a different fact from being absent. A panel that
    /// lists only accounts with observations is a panel that cannot show that
    /// something was never tried.
    #[cfg(feature = "account-maint")]
    pub async fn checkin_views(&self) -> Vec<CheckinView> {
        let config = self.core.config();
        let mut views = Vec::new();
        for provider in &config.providers {
            for account in &provider.accounts {
                let key = (provider.id.clone(), account.account_id.clone());
                let mut view = match lock(&self.checkin_reports).get(&key) {
                    Some(view) => view.clone(),
                    None => CheckinView::idle(&provider.id, &account.account_id),
                };
                // Derived at read time, never stored: see `CheckinScheduleView::unknown`.
                view.schedule = self.schedule_view(&provider.id, &account.account_id);
                views.push(view);
            }
        }
        views
    }

    /// The view for one account, if it is declared.
    #[cfg(feature = "account-maint")]
    pub async fn checkin_view(&self, provider_id: &str, account_id: &str) -> Option<CheckinView> {
        let key = (provider_id.to_string(), account_id.to_string());
        let stored = lock(&self.checkin_reports).get(&key).cloned();
        let held = self
            .checkin
            .is_browser_held(&crate::checkin::SessionKey::new(provider_id, account_id))
            .await;
        let awaiting = self
            .checkin
            .phase(&crate::checkin::SessionKey::new(provider_id, account_id))
            .await
            .map(|phase| phase.awaiting_user())
            .unwrap_or(false);

        let declared =
            self.core.config().providers.iter().any(|p| {
                p.id == provider_id && p.accounts.iter().any(|a| a.account_id == account_id)
            });
        if !declared {
            return None;
        }
        let mut view = stored.unwrap_or_else(|| CheckinView::idle(provider_id, account_id));
        // Liveness is asked of the runtime, never read from the stored report: a
        // report written before a pause must not claim no browser is held.
        view.browser_held = held;
        view.awaiting_user = awaiting;
        view.schedule = self.schedule_view(provider_id, account_id);
        Some(view)
    }

    /// Remember what a check-in did, so the next snapshot can render it.
    #[cfg(feature = "account-maint")]
    pub fn record_checkin_view(&self, view: CheckinView) {
        lock(&self.checkin_reports)
            .insert((view.provider_id.clone(), view.account_id.clone()), view);
    }

    /// The maintenance declaration for one account, if it is declared.
    ///
    /// Read from the live configuration rather than cached, so a check-in
    /// cannot be started against a declaration the user has since removed.
    #[cfg(feature = "account-maint")]
    pub fn maintenance_for(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> Option<zroutery_core::config::MaintenanceConfig> {
        self.core.config().providers.iter().find_map(|p| {
            (p.id == provider_id)
                .then(|| {
                    p.accounts
                        .iter()
                        .find(|a| a.account_id == account_id)
                        .map(|a| a.maintenance.clone())
                })
                .flatten()
        })
    }

    /// Observed resource events for the snapshot, newest first.
    ///
    /// Bounded by the log itself rather than here, so a long-running application
    /// cannot grow its snapshot without limit. Reversed so a panel can render the
    /// most recent without sorting, and the ordering is by what the provider said
    /// happened rather than by when Zroutery read it.
    #[cfg(feature = "account-maint")]
    pub fn account_event_views(&self) -> Vec<AccountEventView> {
        use zroutery_core::account::resource::{ResourceEffect, ResourceEventSource};
        let mut views: Vec<AccountEventView> = self
            .account_events
            .recent(ACCOUNT_EVENT_SNAPSHOT_LIMIT)
            .iter()
            .map(|event| AccountEventView {
                provider_id: event.provider_id.clone(),
                account_id: event.account_id.0.clone(),
                observed_at: event.observed_at,
                kind: match event.event_kind {
                    zroutery_core::account::resource::ResourceEventKind::Consumption => {
                        "consumption"
                    }
                    zroutery_core::account::resource::ResourceEventKind::Replenishment => {
                        "replenishment"
                    }
                    zroutery_core::account::resource::ResourceEventKind::Refund => "refund",
                    zroutery_core::account::resource::ResourceEventKind::SystemGrant => {
                        "system_grant"
                    }
                    zroutery_core::account::resource::ResourceEventKind::Management => "management",
                    zroutery_core::account::resource::ResourceEventKind::Error => "error",
                    zroutery_core::account::resource::ResourceEventKind::Login => "login",
                    zroutery_core::account::resource::ResourceEventKind::Unknown => "unknown",
                }
                .to_string(),
                effect: match &event.resource_effect {
                    ResourceEffect::Credited { .. } => "credited",
                    ResourceEffect::Debited { .. } => "debited",
                    ResourceEffect::NoChange => "no_change",
                    ResourceEffect::Unobserved => "unobserved",
                }
                .to_string(),
                amount: event.resource_effect.amount().map(|a| a.value),
                unit: event.resource_effect.amount().map(|a| a.unit.clone()),
                source: match event.source {
                    ResourceEventSource::AccountLog => "account_log",
                    ResourceEventSource::AccountState => "account_state",
                    ResourceEventSource::OperationResponse => "operation_response",
                }
                .to_string(),
                description: event.description.clone(),
            })
            .collect();
        views.sort_by_key(|v| std::cmp::Reverse(v.observed_at));
        views
    }

    /// The scheduler's answer for one account, computed now.
    ///
    /// Derived rather than stored, so it cannot disagree with the configuration the
    /// operator is looking at: an interval edited a second ago is reflected here
    /// immediately, and no cached copy can promise a time that is already wrong.
    ///
    /// An account that is not declared gets [`CheckinScheduleView::unknown`] rather
    /// than a fabricated decision. The alternative — deciding against whatever
    /// declaration happens to be at hand — would answer for an account that does not
    /// exist, and a panel showing that would be showing a panel failure as an
    /// account state.
    #[cfg(feature = "account-maint")]
    pub fn schedule_view(&self, provider_id: &str, account_id: &str) -> CheckinScheduleView {
        let Some(maintenance) = self.maintenance_for(provider_id, account_id) else {
            return CheckinScheduleView::unknown();
        };
        let last_settled_at = self
            .checkin_schedule
            .last_settled_at(provider_id, account_id);
        let now = chrono::Utc::now().timestamp();
        let decision = zroutery_core::account::checkin::decide(&maintenance, last_settled_at, now);
        CheckinScheduleView {
            decision: match decision {
                zroutery_core::account::checkin::MaintenanceDecision::NotApplicable => {
                    "not_applicable"
                }
                zroutery_core::account::checkin::MaintenanceDecision::Disabled => "disabled",
                zroutery_core::account::checkin::MaintenanceDecision::NeedsFirstObservation => {
                    "needs_first_observation"
                }
                zroutery_core::account::checkin::MaintenanceDecision::NoPolicy => "no_policy",
                zroutery_core::account::checkin::MaintenanceDecision::NotDue { next_due_at } => {
                    return CheckinScheduleView {
                        decision: "not_due".into(),
                        next_due_at: Some(next_due_at),
                        last_settled_at,
                    };
                }
                zroutery_core::account::checkin::MaintenanceDecision::Due { .. } => "due",
            }
            .into(),
            next_due_at: match decision {
                zroutery_core::account::checkin::MaintenanceDecision::NotDue { next_due_at } => {
                    Some(next_due_at)
                }
                _ => None,
            },
            last_settled_at,
        }
    }

    /// Close every browser this process is holding.
    ///
    /// Called on shutdown. A browser left running after the app exits would keep
    /// a profile locked and keep a window on screen that nothing owns.
    #[cfg(feature = "account-maint")]
    pub async fn shutdown_browsers(&self) {
        self.checkin.shutdown_all().await;
    }

    /// A NewAPI panel adapter for one account, built from its stored credential.
    ///
    /// Returns `None` — rather than an adapter with no credential — when no key is
    /// stored. That distinction matters: an adapter with an empty key would fail
    /// every call with an authentication error, which reads as "the credential is
    /// wrong" when the truth is that none was ever configured.
    ///
    /// The key is read here and goes straight into the adapter. It is never
    /// returned, stored on `Desktop`, or put in a snapshot: the credential lives
    /// in the keychain, and the only thing that outlives this call is an adapter
    /// holding it in memory.
    // Gated on `newapi` rather than on `account-maint`, because nothing in this
    // function touches the browser half of the maintenance surface: it builds an
    // adapter, reads one credential, and hands the adapter back. It was gated on
    // `account-maint` because that is where it was first needed, which left
    // `--features newapi` without `--features account-maint` uncompilable — and
    // every gate run for the change was feature-enabled, so nothing could see it.
    // That is the same shape as the defect E-115 records: a build the
    // verification never visits.
    #[cfg(feature = "newapi")]
    pub fn panel_adapter(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> Option<(zroutery_core::account::adapters::newapi::NewApiAdapter, f64)> {
        use zroutery_core::account::adapters::newapi::{NewApiAdapter, NewApiConfig};
        use zroutery_core::config::SecretStore;

        let config = self.core.config();
        let provider = config.providers.iter().find(|p| p.id == provider_id)?;
        let account = provider
            .accounts
            .iter()
            .find(|a| a.account_id == account_id)?;
        // An empty account key means "use the provider's own", which is the same
        // convention `AccountConfig::key_ref` documents.
        let key_ref = if account.key_ref.trim().is_empty() {
            provider.key_ref.as_str()
        } else {
            account.key_ref.as_str()
        };
        let api_key = self.secrets.get(key_ref)?;
        if api_key.trim().is_empty() {
            return None;
        }

        // The panel root, with any relay suffix stripped, exactly as the adapter's
        // own normalisation would do — it is repeated here only to pick the
        // default conversion rate, and a base the adapter later refuses is caught
        // by its own validation.
        let base = panel_root(&provider.base_url);
        let adapter = NewApiAdapter::new(NewApiConfig {
            base_url: base,
            api_key,
            ..Default::default()
        })
        .ok()?;
        Some((
            adapter,
            zroutery_core::account::adapters::newapi::DEFAULT_QUOTA_PER_UNIT,
        ))
    }
}
