use zroutery_plugin_api::{
    capability::Scope, manifest::PluginManifestV1, Capability, PermissionRequest, Principal,
};
fn provider() -> serde_json::Value {
    serde_json::json!({
        "manifest_version":1, "id":"dev.test.provider", "version":"1.0.0",
        "name":{"en":"Test","zh-CN":"测试","zh-TW":"測試"}, "host_api":{"major":1,"min_minor":0},
        "runtime":null,"requires":[],
        "permissions":[
            {"capability":"provider.register","principal":"host_binding","contributions":["test"],"reason":{"en":"Register","zh-CN":"注册","zh-TW":"註冊"}},
            {"capability":"host.operation.bind","principal":"host_binding","contributions":["test"],"operations":["completion"],"origins":["https://api.example.test"],"reason":{"en":"Send","zh-CN":"发送","zh-TW":"傳送"}},
            {"capability":"credential.store","principal":"host_binding","contributions":["test"],"credential_slots":["app_token"],"reason":{"en":"Store","zh-CN":"保存","zh-TW":"儲存"}},
            {"capability":"credential.use","principal":"host_binding","contributions":["test"],"credential_slots":["app_token"],"operations":["completion"],"origins":["https://api.example.test"],"reason":{"en":"Use","zh-CN":"使用","zh-TW":"使用"}}
        ],
        "contributions":{"providers":[{
            "id":"test","name":{"en":"Test","zh-CN":"测试","zh-TW":"測試"},
            "operations":[{"id":"completion","service":{"id":"host.protocol.openai_chat","api":{"major":1,"min_minor":0}},"base_url":"https://api.example.test/v1","credential_slots":["app_token"]}],
            "credentials":[{"id":"app_token","name":{"en":"Token","zh-CN":"令牌","zh-TW":"權杖"},"kind":"app_token","scope":"provider_instance","storage":"host_keyring","acquisition":{"kind":"host_secret_form"},"auth_service":{"id":"host.auth.bearer","api":{"major":1,"min_minor":0}}}]
        }]}
    })
}
#[test]
fn every_required_permission_is_explicit_and_missing_one_denies_installation() {
    let valid = provider();
    PluginManifestV1::decode(&serde_json::to_vec(&valid).unwrap()).unwrap();
    for index in 0..4 {
        let mut invalid = valid.clone();
        invalid["permissions"].as_array_mut().unwrap().remove(index);
        assert!(PluginManifestV1::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}
#[test]
fn scope_fields_and_irrelevant_restrictions_are_closed() {
    for field in ["operations", "origins", "credential_slots"] {
        let mut invalid = provider();
        invalid["permissions"][3][field] = serde_json::json!([]);
        assert!(PluginManifestV1::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    let mut invalid = provider();
    invalid["permissions"][3]["profiles"] = serde_json::json!(["*"]);
    assert!(PluginManifestV1::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
    let mut invalid = provider();
    invalid["permissions"][3]["credential_slots"] = serde_json::json!(["other_plugin_token"]);
    assert!(PluginManifestV1::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
}
#[test]
fn two_local_grants_cannot_be_cross_stitched_into_a_new_operation_origin_slot() {
    let mut value = provider()["permissions"][3].clone();
    value["origins"] = serde_json::json!(["https://one.example"]);
    let one: PermissionRequest = serde_json::from_value(value.clone()).unwrap();
    value["credential_slots"] = serde_json::json!(["different_slot"]);
    value["origins"] = serde_json::json!(["https://two.example"]);
    let two: PermissionRequest = serde_json::from_value(value).unwrap();
    let scope = Scope {
        slot: Some("app_token"),
        operation: Some("completion"),
        origin: Some("https://two.example"),
        ..Scope::contribution("test")
    };
    assert!(![one, two].iter().any(|p| p.covers(
        Capability::CredentialUse,
        Principal::HostBinding,
        scope
    )));
}

#[test]
fn provider_extra_headers_carry_identity_and_never_auth() {
    let mut panel = provider();
    panel["contributions"]["providers"][0]["extra_headers"] = serde_json::json!({
        "user-agent": "WorkBuddy/5.11.3 WorkBuddy/5.11.3 CLI/2.176.0",
        "accept": "application/json",
        "x-ide-version": "5.11.3",
        "x-requested-with": "XMLHttpRequest"
    });
    PluginManifestV1::decode(&serde_json::to_vec(&panel).unwrap()).unwrap();

    // Auth and framing names can never travel through the identity header map.
    for name in [
        "authorization",
        "x-api-key",
        "cookie",
        "content-type",
        "x-unknown-custom",
    ] {
        let mut rejected = panel.clone();
        rejected["contributions"]["providers"][0]["extra_headers"] =
            serde_json::json!({ name: "value" });
        assert!(
            PluginManifestV1::decode(&serde_json::to_vec(&rejected).unwrap()).is_err(),
            "{name}"
        );
    }
    // The map is bounded (8 entries) and its values are bounded text.
    let mut overflow = panel.clone();
    overflow["contributions"]["providers"][0]["extra_headers"] = serde_json::json!({
        "user-agent": "a", "accept": "a", "accept-language": "a",
        "accept-encoding": "a", "x-ide-version": "a", "x-client-version": "a",
        "x-client-platform": "a", "x-client-product": "a", "x-requested-with": "a"
    });
    assert!(PluginManifestV1::decode(&serde_json::to_vec(&overflow).unwrap()).is_err());
}
