//! Stage 5 Account component tests.

#[cfg(feature = "account")]
mod account_tests {
    use zroutery_core::account::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::thread;

    // Account identity
    #[test]
    fn account_id_display() {
        let id = AccountId("acct-7".into());
        assert_eq!(id.to_string(), "acct-7");
        // Clone + equality
        let id2 = id.clone();
        assert_eq!(id, id2);
        // Different id
        assert_ne!(id, AccountId("acct-8".into()));
    }

    // Multiple accounts same provider
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
        store.upsert(AccountRuntime {
            account_id: AccountId("a3".into()),
            provider_id: "openai".into(),
            status: AccountStatus::Suspended,
            ..Default::default()
        });

        let list = store.list_by_provider("openai");
        assert_eq!(list.len(), 3);

        // Each account is independently retrievable
        assert!(store.get("openai", &AccountId("a1".into())).is_some());
        assert!(store.get("openai", &AccountId("a2".into())).is_some());
        assert!(store.get("openai", &AccountId("a3".into())).is_some());
    }

    // Provider isolation
    #[test]
    fn provider_isolation() {
        let store = AccountStore::new();

        // Same account id under different providers are independent
        store.upsert(AccountRuntime {
            account_id: AccountId("shared".into()),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            ..Default::default()
        });
        store.upsert(AccountRuntime {
            account_id: AccountId("shared".into()),
            provider_id: "anthropic".into(),
            status: AccountStatus::Suspended,
            ..Default::default()
        });

        let openai = store.get("openai", &AccountId("shared".into())).unwrap();
        let anthropic = store.get("anthropic", &AccountId("shared".into())).unwrap();
        assert_eq!(openai.status, AccountStatus::Active);
        assert_eq!(anthropic.status, AccountStatus::Suspended);

        // Removing from one provider does not affect the other
        assert!(store.remove("openai", &AccountId("shared".into())));
        assert!(store.get("openai", &AccountId("shared".into())).is_none());
        assert!(store.get("anthropic", &AccountId("shared".into())).is_some());
    }

    // Quota
    #[test]
    fn quota_utilization() {
        let q = AccountQuota {
            total: 200.0,
            used: 50.0,
            remaining: 150.0,
            unit: "credits".into(),
            resets_at: None,
        };
        assert!((q.utilization() - 0.25).abs() < f64::EPSILON);

        // Full utilization
        let full = AccountQuota {
            total: 100.0,
            used: 100.0,
            remaining: 0.0,
            unit: "usd".into(),
            resets_at: Some(1_700_000_000),
        };
        assert!((full.utilization() - 1.0).abs() < f64::EPSILON);

        // Zero total => 0.0
        let zero = AccountQuota {
            total: 0.0,
            used: 0.0,
            remaining: 0.0,
            unit: "tokens".into(),
            resets_at: None,
        };
        assert_eq!(zero.utilization(), 0.0);
    }

    // Rate limit
    #[test]
    fn rate_limit_pressure() {
        // RPM dominates
        let rpm_heavy = RateLimitState {
            rpm_limit: Some(100),
            rpm_used: Some(90),
            tpm_limit: Some(100_000),
            tpm_used: Some(10_000),
            ..Default::default()
        };
        assert!((rpm_heavy.pressure() - 0.9).abs() < f64::EPSILON);

        // TPM dominates
        let tpm_heavy = RateLimitState {
            rpm_limit: Some(1000),
            rpm_used: Some(100),
            tpm_limit: Some(10_000),
            tpm_used: Some(9_500),
            ..Default::default()
        };
        assert!((tpm_heavy.pressure() - 0.95).abs() < f64::EPSILON);

        // No limits set
        let empty = RateLimitState::default();
        assert_eq!(empty.pressure(), 0.0);
    }

    // Status default
    #[test]
    fn status_default_is_unknown() {
        assert_eq!(AccountStatus::default(), AccountStatus::Unknown);
        // Also verify a default AccountRuntime has Unknown status
        let rt = AccountRuntime::default();
        assert_eq!(rt.status, AccountStatus::Unknown);
    }

    // Store CRUD
    #[test]
    fn store_upsert_get_remove() {
        let store = AccountStore::new();
        let id = AccountId("crud-test".into());

        // Absent initially
        assert!(store.get("openai", &id).is_none());

        // Insert
        store.upsert(AccountRuntime {
            account_id: id.clone(),
            provider_id: "openai".into(),
            status: AccountStatus::Active,
            ..Default::default()
        });
        let got = store.get("openai", &id).unwrap();
        assert_eq!(got.account_id, id);
        assert_eq!(got.status, AccountStatus::Active);

        // Update via upsert
        store.upsert(AccountRuntime {
            account_id: id.clone(),
            provider_id: "openai".into(),
            status: AccountStatus::RateLimited,
            ..Default::default()
        });
        assert_eq!(store.get("openai", &id).unwrap().status, AccountStatus::RateLimited);

        // Remove
        assert!(store.remove("openai", &id));
        assert!(store.get("openai", &id).is_none());
        // Double-remove returns false
        assert!(!store.remove("openai", &id));
    }

    // Store list
    #[test]
    fn store_list_by_provider() {
        let store = AccountStore::new();
        for i in 0..5 {
            store.upsert(AccountRuntime {
                account_id: AccountId(format!("o{i}")),
                provider_id: "openai".into(),
                ..Default::default()
            });
        }
        for i in 0..3 {
            store.upsert(AccountRuntime {
                account_id: AccountId(format!("a{i}")),
                provider_id: "anthropic".into(),
                ..Default::default()
            });
        }

        assert_eq!(store.list_by_provider("openai").len(), 5);
        assert_eq!(store.list_by_provider("anthropic").len(), 3);
        assert_eq!(store.list_by_provider("nonexistent").len(), 0);
    }

    // Serde
    #[test]
    fn account_runtime_serde_round_trip() {
        let rt = AccountRuntime {
            account_id: AccountId("serde-42".into()),
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
                total: 500.0,
                used: 125.0,
                remaining: 375.0,
                unit: "credits".into(),
                resets_at: Some(1_700_000_000),
            }),
            usage: Some(AccountUsage {
                total_requests: 250,
                total_tokens: 500_000,
                total_cost: 21.25,
                currency: "usd".into(),
                period_start: Some(1_699_000_000),
                period_end: Some(1_700_000_000),
            }),
            rate_limit: Some(RateLimitState {
                rpm_limit: Some(60),
                rpm_used: Some(15),
                tpm_limit: Some(90_000),
                tpm_used: Some(45_000),
                concurrency_limit: Some(10),
                concurrency_used: Some(2),
                resets_at: Some(1_700_000_060),
            }),
            last_success: Some(1_699_999_900),
            last_failure: Some(1_699_999_800),
            last_sync: Some(1_699_999_950),
            metadata: {
                let mut m = HashMap::new();
                m.insert("team".into(), "ml-ops".into());
                m.insert("env".into(), "prod".into());
                m
            },
        };

        let json = serde_json::to_string(&rt).expect("serialize");
        let back: AccountRuntime = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.account_id, rt.account_id);
        assert_eq!(back.provider_id, rt.provider_id);
        assert_eq!(back.status, AccountStatus::Active);
        assert!(back.capabilities.supports_usage);
        assert!(back.capabilities.supports_quota);
        assert!(!back.capabilities.supports_refresh);
        assert!((back.quota.as_ref().unwrap().utilization() - 0.25).abs() < f64::EPSILON);
        assert_eq!(back.usage.as_ref().unwrap().total_requests, 250);
        assert_eq!(back.usage.as_ref().unwrap().total_cost, 21.25);
        // RPM pressure 15/60=0.25, TPM pressure 45000/90000=0.5 => max 0.5
        assert!((back.rate_limit.as_ref().unwrap().pressure() - 0.5).abs() < f64::EPSILON);
        assert_eq!(back.metadata.len(), 2);
        assert_eq!(back.metadata.get("team").unwrap(), "ml-ops");
        assert_eq!(back.last_failure, Some(1_699_999_800));
    }

    // Concurrent access
    #[test]
    fn concurrent_store_access() {
        let store = Arc::new(AccountStore::new());
        let num_threads = 8;
        let accounts_per_thread = 50;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    for i in 0..accounts_per_thread {
                        let id = AccountId(format!("t{t}-a{i}"));
                        store.upsert(AccountRuntime {
                            account_id: id.clone(),
                            provider_id: "concurrent".into(),
                            status: AccountStatus::Active,
                            ..Default::default()
                        });
                        // Immediately read back
                        let got = store.get("concurrent", &id);
                        assert!(got.is_some(), "account t{t}-a{i} should exist");
                        assert_eq!(got.unwrap().status, AccountStatus::Active);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        let all = store.list_by_provider("concurrent");
        assert_eq!(all.len(), num_threads * accounts_per_thread);
    }
}
