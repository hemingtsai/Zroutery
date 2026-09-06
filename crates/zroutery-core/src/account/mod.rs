//! Optional account subsystem.
//!
//! Feature-gated behind `account`. Provides account identity, runtime state,
//! quota, usage, and rate-limit tracking. Account is always optional in
//! routing — Provider+Model works without it.

pub mod types;
pub mod store;
pub mod provider;
pub mod adapters;
pub use types::*;
pub use store::AccountStore;
pub use provider::{AccountOpResult, AccountProvider};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── Account identity ──

    #[test]
    fn account_creation_and_display() {
        let id = AccountId("acct-123".into());
        assert_eq!(id.to_string(), "acct-123");
        assert_eq!(id, AccountId("acct-123".into()));
        assert_ne!(id, AccountId("acct-456".into()));
    }

    #[test]
    fn multiple_accounts_same_provider() {
        let store = AccountStore::new();
        store.upsert(AccountRuntime {
            account_id: AccountId("a1".into()),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            ..Default::default()
        });
        store.upsert(AccountRuntime {
            account_id: AccountId("a2".into()),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            ..Default::default()
        });
        let list = store.list_by_provider("openai");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn multiple_accounts_different_providers() {
        let store = AccountStore::new();
        store.upsert(AccountRuntime {
            account_id: AccountId("a1".into()),
            provider_id: "openai".into(),
            ..Default::default()
        });
        store.upsert(AccountRuntime {
            account_id: AccountId("a1".into()),
            provider_id: "anthropic".into(),
            ..Default::default()
        });
        assert_eq!(store.list_by_provider("openai").len(), 1);
        assert_eq!(store.list_by_provider("anthropic").len(), 1);
    }

    #[test]
    fn account_isolation_by_provider() {
        let store = AccountStore::new();
        store.upsert(AccountRuntime {
            account_id: AccountId("x".into()),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            ..Default::default()
        });
        store.upsert(AccountRuntime {
            account_id: AccountId("x".into()),
            provider_id: "anthropic".into(),
            status: AccountStatus::Suspended,
            ..Default::default()
        });
        assert_eq!(
            store.get("openai", &AccountId("x".into())).unwrap().status,
            AccountStatus::Active,
        );
        assert_eq!(
            store.get("anthropic", &AccountId("x".into())).unwrap().status,
            AccountStatus::Suspended,
        );
    }

    // ── Quota ──

    #[test]
    fn quota_utilization_calculation() {
        let q = AccountQuota {
            total: 100.0,
            used: 75.0,
            remaining: 25.0,
            unit: "credits".into(),
            resets_at: None,
        };
        assert!((q.utilization() - 0.75).abs() < f64::EPSILON);

        let zero = AccountQuota {
            total: 0.0,
            used: 0.0,
            remaining: 0.0,
            unit: "credits".into(),
            resets_at: None,
        };
        assert_eq!(zero.utilization(), 0.0);
    }

    // ── Rate limit ──

    #[test]
    fn rate_limit_pressure_calculation() {
        let rl = RateLimitState {
            rpm_limit: Some(100),
            rpm_used: Some(80),
            tpm_limit: Some(10_000),
            tpm_used: Some(5_000),
            ..Default::default()
        };
        // RPM pressure = 80/100 = 0.8, TPM pressure = 5000/10000 = 0.5 => max is 0.8
        assert!((rl.pressure() - 0.8).abs() < f64::EPSILON);

        let empty = RateLimitState::default();
        assert_eq!(empty.pressure(), 0.0);

        let tpm_only = RateLimitState {
            rpm_limit: None,
            rpm_used: None,
            tpm_limit: Some(1000),
            tpm_used: Some(900),
            ..Default::default()
        };
        assert!((tpm_only.pressure() - 0.9).abs() < f64::EPSILON);
    }

    // ── AccountStatus default ──

    #[test]
    fn account_status_default_is_unknown() {
        assert_eq!(AccountStatus::default(), AccountStatus::Unknown);
    }

    // ── AccountStore CRUD ──

    #[test]
    fn store_upsert_get_remove() {
        let store = AccountStore::new();
        let id = AccountId("acct-1".into());
        let rt = AccountRuntime {
            account_id: id.clone(),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            ..Default::default()
        };

        // Not found initially
        assert!(store.get("openai", &id).is_none());

        // Insert
        store.upsert(rt.clone());
        let got = store.get("openai", &id).unwrap();
        assert_eq!(got.status, AccountStatus::Active);

        // Update
        store.upsert(AccountRuntime {
            status: AccountStatus::Suspended,
            ..rt.clone()
        });
        assert_eq!(store.get("openai", &id).unwrap().status, AccountStatus::Suspended);

        // Remove
        assert!(store.remove("openai", &id));
        assert!(store.get("openai", &id).is_none());
        assert!(!store.remove("openai", &id)); // already gone
    }

    // ── list_by_provider ──

    #[test]
    fn store_list_by_provider_filters_correctly() {
        let store = AccountStore::new();
        store.upsert(AccountRuntime {
            account_id: AccountId("a".into()),
            provider_id: "openai".into(),
            ..Default::default()
        });
        store.upsert(AccountRuntime {
            account_id: AccountId("b".into()),
            provider_id: "openai".into(),
            ..Default::default()
        });
        store.upsert(AccountRuntime {
            account_id: AccountId("c".into()),
            provider_id: "anthropic".into(),
            ..Default::default()
        });

        assert_eq!(store.list_by_provider("openai").len(), 2);
        assert_eq!(store.list_by_provider("anthropic").len(), 1);
        assert_eq!(store.list_by_provider("nonexistent").len(), 0);
    }

    // ── Serde round-trip ──

    #[test]
    fn account_runtime_serde_round_trip() {
        let rt = AccountRuntime {
            account_id: AccountId("acct-42".into()),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            capabilities: AccountCapabilities {
                supports_usage: true,
                supports_quota: true,
                supports_refresh: false,
                supports_checkin: false,
                supports_health_check: true,
            },
            quota: Some(AccountQuota {
                total: 1000.0,
                used: 250.0,
                remaining: 750.0,
                unit: "credits".into(),
                resets_at: Some(1_700_000_000),
            }),
            usage: Some(AccountUsage {
                total_requests: 500,
                total_tokens: 1_000_000,
                total_cost: 42.5,
                currency: "usd".into(),
                period_start: Some(1_699_000_000),
                period_end: Some(1_700_000_000),
            }),
            rate_limit: Some(RateLimitState {
                rpm_limit: Some(60),
                rpm_used: Some(30),
                tpm_limit: Some(90_000),
                tpm_used: Some(45_000),
                concurrency_limit: Some(10),
                concurrency_used: Some(3),
                resets_at: Some(1_700_000_060),
            }),
            last_success: Some(1_699_999_900),
            last_failure: None,
            last_sync: Some(1_699_999_950),
            metadata: {
                let mut m = HashMap::new();
                m.insert("team".into(), "research".into());
                m
            },
        };

        let json = serde_json::to_string(&rt).unwrap();
        let deserialized: AccountRuntime = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.account_id, rt.account_id);
        assert_eq!(deserialized.provider_id, rt.provider_id);
        assert_eq!(deserialized.status, rt.status);
        assert_eq!(deserialized.capabilities.supports_usage, true);
        assert!((deserialized.quota.as_ref().unwrap().utilization() - 0.25).abs() < f64::EPSILON);
        assert_eq!(deserialized.usage.as_ref().unwrap().total_requests, 500);
        assert!((deserialized.rate_limit.as_ref().unwrap().pressure() - 0.5).abs() < f64::EPSILON);
        assert_eq!(deserialized.metadata.get("team").unwrap(), "research");
    }
}
