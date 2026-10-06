//! Why the account registry is configuration, asserted rather than described.
//!
//! The choice in `docs/development/account-surface-plan.md` was between a
//! dedicated durable file and a field on `ProviderConfig`. The argument for the
//! field was that nothing has to migrate, and that argument is only worth anything
//! if a configuration written *before* the field existed still deserialises. That
//! is what the first test pins.
//!
//! JSON rather than the config file's own format, because no core test parses a
//! config file and adding a TOML dev-dependency to prove a `#[serde(default)]`
//! would be the larger change. The behaviour under test is serde's, and serde's
//! `default` attribute behaves the same in every format.
//!
//! The second half pins the part that is easy to get wrong. `AccountConfig` is a
//! declaration and has to stay minimal: `AccountRuntime` derives `Serialize`, so
//! adding an observation field would compile, deserialise, and then sit in
//! `AppConfig` claiming to be configuration while quietly expiring.

use zroutery_core::config::{
    AccountConfig, AppConfig, MaintenanceConfig, ProviderConfig, ProviderKind,
};

#[test]
fn a_configuration_written_before_accounts_existed_still_loads() {
    // Every optional field on `ProviderConfig` left at its default, and `accounts`
    // simply not mentioned -- which is what every config file written before this
    // field existed looks like.
    let config: AppConfig = serde_json::from_str(
        r#"{
            "providers": [
                { "id": "relay", "name": "Relay", "kind": "openai_compatible",
                  "base_url": "https://relay.example/v1" }
            ]
        }"#,
    )
    .expect("a pre-account config parses");

    assert_eq!(config.providers.len(), 1);
    assert!(
        config.providers[0].accounts.is_empty(),
        "a provider with no accounts key must read as having none, not fail to load"
    );
}

#[test]
fn declaring_accounts_on_one_provider_leaves_the_others_alone() {
    let declared: AppConfig = serde_json::from_str(
        r#"{
            "providers": [
                { "id": "relay", "name": "Relay", "kind": "openai_compatible",
                  "base_url": "https://relay.example/v1",
                  "accounts": [
                    { "account_id": "main" },
                    { "account_id": "spare",
                      "key_ref": "provider:relay:account:spare", "enabled": false }
                  ] },
                { "id": "solo", "name": "Solo", "kind": "openai_compatible",
                  "base_url": "https://solo.example/v1" }
            ]
        }"#,
    )
    .expect("config with accounts parses");

    let relay = &declared.providers[0];
    assert_eq!(relay.accounts.len(), 2, "two accounts were declared");

    // The documented defaults, not whatever serde infers from the type.
    assert_eq!(
        relay.accounts[0].key_ref, "",
        "an omitted key_ref means the provider's own key"
    );
    assert!(
        relay.accounts[0].enabled,
        "accounts are enabled unless the config says otherwise"
    );
    assert_eq!(relay.accounts[1].key_ref, "provider:relay:account:spare");
    assert!(
        !relay.accounts[1].enabled,
        "an explicit false is honoured rather than replaced by the default"
    );

    assert!(
        declared.providers[1].accounts.is_empty(),
        "adding accounts to one provider must not touch another"
    );
}

#[test]
fn a_provider_built_in_code_has_no_accounts() {
    // The other construction path. `ProviderConfig::new` is the only exhaustive
    // struct literal in the tree, so forgetting `accounts` there is a build failure
    // rather than a silent default -- which is the cheap half of this guarantee.
    let provider = ProviderConfig::new("relay", "Relay", ProviderKind::OpenAICompatible);

    assert_eq!(
        provider.accounts,
        Vec::<AccountConfig>::new(),
        "the default is no accounts, not a default account"
    );
}

#[test]
fn an_account_config_carries_no_observation_fields() {
    // The hazard, pinned. `AccountRuntime` derives Serialize, so an observation
    // could be added here and would compile, round-trip, and then sit in AppConfig
    // presenting as configuration while expiring.
    let declared = AccountConfig {
        account_id: "main".to_string(),
        key_ref: String::new(),
        enabled: true,
        maintenance: MaintenanceConfig {
            checkin_enabled: true,
            checkin_interval_secs: Some(86_400),
            checkin_path: Some("/console/personal".to_string()),
            ..Default::default()
        },
    };

    let json = serde_json::to_value(&declared).expect("an account serialises");
    let fields: Vec<String> = json
        .as_object()
        .expect("an account serialises to an object")
        .keys()
        .cloned()
        .collect();

    for forbidden in [
        "quota",
        "usage",
        "rate_limit",
        "last_success",
        "last_failure",
        "last_sync",
        "status",
        "capabilities",
        "metadata",
    ] {
        assert!(
            !fields.iter().any(|field| field == forbidden),
            "AccountConfig must not carry `{forbidden}`: it is derived by probing and \
             expires, so persisting it turns a number that was true once into \
             configuration that reads as authoritative afterwards. It belongs in \
             AccountStore. Present fields: {fields:?}"
        );
    }

    assert_eq!(
        fields.len(),
        4,
        "AccountConfig is a declaration and nothing else; got {fields:?}"
    );
}

#[test]
fn a_maintenance_declaration_carries_no_observation_or_credential_fields() {
    // The same hazard one level down. `MaintenanceConfig` says what the user
    // asked for; whether it worked, and whether the browser still holds a live
    // session, are runtime facts that expire. A credential field here would be a
    // credential in a file the app also writes back to.
    let declared = MaintenanceConfig {
        checkin_enabled: true,
        checkin_interval_secs: Some(86_400),
        checkin_path: Some("/console/personal".to_string()),
        login_path: Some("/login".to_string()),
        browser_executable: String::new(),
    };

    let json = serde_json::to_value(&declared).expect("maintenance serialises");
    let fields: Vec<String> = json
        .as_object()
        .expect("maintenance serialises to an object")
        .keys()
        .cloned()
        .collect();

    for forbidden in [
        "username",
        "password",
        "cookie",
        "cookies",
        "token",
        "access_token",
        "session",
        "headless",
        "last_checkin",
        "last_reward",
        "phase",
        "reward",
    ] {
        assert!(
            !fields.iter().any(|field| field == forbidden),
            "MaintenanceConfig must not carry `{forbidden}`. Observations expire and \
             credentials must live in the keyring or the browser profile, never in a \
             user-editable document. Present fields: {fields:?}"
        );
    }

    // `headless` is refused rather than merely absent: the operation needs a
    // browser a script cannot drive, so a headless switch would be a mode that
    // cannot work offered as though it could.
    assert!(
        !fields.iter().any(|f| f.contains("headless")),
        "a headless switch must not be introduced; got {fields:?}"
    );
}

#[test]
fn an_account_config_from_an_older_document_deserialises_with_maintenance_off() {
    // A configuration written before maintenance existed must load rather than
    // fail, and must not silently start checking accounts in.
    let parsed: AppConfig = serde_json::from_str(
        r#"{
            "providers": [
                { "id": "relay", "name": "Relay", "kind": "openai_compatible",
                  "base_url": "https://relay.example/v1",
                  "accounts": [ { "account_id": "main" } ] }
            ]
        }"#,
    )
    .expect("a document without maintenance still loads");

    let account = &parsed.providers[0].accounts[0];
    assert!(!account.maintenance.checkin_enabled);
    assert!(!account.maintenance.is_browser_checkin_configured());
}

#[test]
fn two_accounts_on_one_provider_need_distinct_ids() {
    // `account_id` is required rather than defaulted precisely so this cannot
    // happen silently. The store is keyed on `(provider_id, account_id)`, so two
    // accounts sharing an id would leave one unreachable while the configuration
    // still claimed both.
    let duplicated: AppConfig = serde_json::from_str(
        r#"{
            "providers": [
                { "id": "relay", "name": "Relay", "kind": "openai_compatible",
                  "base_url": "https://relay.example/v1",
                  "accounts": [ { "account_id": "main" }, { "account_id": "main" } ] }
            ]
        }"#,
    )
    .expect("a duplicated id parses -- serde cannot know it is a duplicate");

    let ids: Vec<&str> = duplicated.providers[0]
        .accounts
        .iter()
        .map(|account| account.account_id.as_str())
        .collect();

    assert_eq!(
        ids.len(),
        2,
        "serde accepts this, so the collision has to be visible to whoever adds \
         accounts rather than absorbed silently"
    );
    assert_eq!(
        ids[0], ids[1],
        "this is the collision the required field exists to make visible"
    );
}
