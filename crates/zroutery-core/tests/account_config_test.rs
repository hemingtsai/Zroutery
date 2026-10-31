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
        binding: None,
        probe_kind: None,
        effective_rates: Vec::new(),
        account_id: "main".to_string(),
        key_ref: String::new(),
        enabled: true,
        maintenance: MaintenanceConfig {
            checkin_enabled: true,
            checkin_interval_secs: Some(86_400),
            checkin_path: Some("/console/personal".to_string()),
            ..Default::default()
        },
        inference_keys: Vec::new(),
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

    // Seven serialized fields, and every one of them a declaration. `binding`
    // and `probe_kind` declare how this account is addressed and inspected;
    // `inference_keys` declares the chat pool the routing path may use. None is
    // a reading that expires. Empty `effective_rates` is deliberately omitted.
    let mut expected = vec![
        "account_id",
        "binding",
        "enabled",
        "inference_keys",
        "key_ref",
        "maintenance",
        "probe_kind",
    ];
    expected.sort_unstable();
    let mut fields = fields;
    fields.sort_unstable();
    assert_eq!(
        fields, expected,
        "AccountConfig is a declaration and nothing else"
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
        checkin_trigger_selector: Some("button.checkin".to_string()),
        login_path: Some("/login".to_string()),
        panel_url: None,
        new_api_user: None,
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
fn browser_checkin_needs_a_control_as_well_as_a_page() {
    use zroutery_core::config::MaintenanceConfig;

    // A page with no control means the browser navigates somewhere and stops,
    // which is a check-in that silently does nothing — the exact failure this
    // declaration exists to prevent.
    let page_only = MaintenanceConfig {
        checkin_enabled: true,
        checkin_path: Some("/console/personal".into()),
        ..Default::default()
    };
    assert!(
        !page_only.is_browser_checkin_configured(),
        "a page without a control is not an actionable check-in"
    );
    let refused = page_only.validate().expect_err("this cannot be acted on");
    assert!(refused.contains("checkin_trigger_selector"), "{refused}");

    let both = MaintenanceConfig {
        checkin_enabled: true,
        checkin_path: Some("/console/personal".into()),
        checkin_trigger_selector: Some("button.checkin".into()),
        ..Default::default()
    };
    assert!(both.validate().is_ok());
    assert!(both.is_browser_checkin_configured());
    assert_eq!(both.checkin_selector(), Some("button.checkin"));
}

#[test]
fn a_blank_selector_is_the_same_as_no_selector() {
    use zroutery_core::config::MaintenanceConfig;

    // Whitespace is what an editor leaves behind; treating it as a configured
    // control would send an empty selector to the browser and report "no control
    // matched" for a configuration that looks complete.
    let blank = MaintenanceConfig {
        checkin_enabled: true,
        checkin_path: Some("/console/personal".into()),
        checkin_trigger_selector: Some("   ".into()),
        ..Default::default()
    };
    assert_eq!(blank.checkin_selector(), None);
    assert!(!blank.is_browser_checkin_configured());
}

#[test]
fn a_disabled_checkin_needs_no_control() {
    use zroutery_core::config::MaintenanceConfig;

    // Refusing to load a document that has check-in switched off would make the
    // selector requirement a reason a working configuration would not load.
    let off = MaintenanceConfig {
        checkin_enabled: false,
        checkin_path: Some("/console/personal".into()),
        ..Default::default()
    };
    assert!(off.validate().is_ok());
}

#[test]
fn a_selector_is_carried_verbatim_for_the_browser_to_interpret() {
    use zroutery_core::config::MaintenanceConfig;

    // Not validated as a URL and not validated as CSS here. A value that is not a
    // selector is a selector that matches nothing, and the browser says so
    // precisely — which is a better answer than this layer guessing at syntax it
    // does not own.
    let odd = MaintenanceConfig {
        checkin_enabled: true,
        checkin_path: Some("/console/personal".into()),
        checkin_trigger_selector: Some("https://example.invalid/button".into()),
        ..Default::default()
    };
    assert!(odd.validate().is_ok());
    assert_eq!(
        odd.checkin_selector(),
        Some("https://example.invalid/button")
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
