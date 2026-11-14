use std::collections::BTreeMap;
use zroutery_plugin_api::PluginError;
use zroutery_plugin_host::{install::Package, lifecycle::Manager};

fn package(version: &str) -> Package {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "manifest_version":1,"id":"dev.test.lifecycle","version":version,
        "name":{"en":"Lifecycle","zh-CN":"生命周期","zh-TW":"生命週期"},"host_api":{"major":1,"min_minor":0},
        "runtime":null,"requires":[],"permissions":[{"capability":"provider.register","principal":"host_binding","contributions":["provider"],"reason":{"en":"Register","zh-CN":"注册","zh-TW":"註冊"}},
        {"capability":"host.operation.bind","principal":"host_binding","contributions":["provider"],"operations":["completion"],"origins":["https://example.test"],"reason":{"en":"Send","zh-CN":"发送","zh-TW":"傳送"}}],
        "contributions":{"providers":[{"id":"provider","name":{"en":"Provider","zh-CN":"服务商","zh-TW":"供應商"},"operations":[{"id":"completion","service":{"id":"host.protocol.openai_chat","api":{"major":1,"min_minor":0}},"base_url":"https://example.test/v1","credential_slots":[]}],"credentials":[]}]}
    })).unwrap();
    Package::from_files(
        BTreeMap::from([("zroutery.plugin.json".into(), bytes)]),
        false,
    )
    .unwrap()
}
#[test]
fn install_approve_update_rollback_disable_remove_and_reinstall_keep_authorities_distinct() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path().into()).unwrap();
    let one = package("1.0.0");
    let installed = manager.install(one.clone()).unwrap();
    let id = &installed.grant.identity.installation_id;
    assert!(!installed.grant.enabled);
    assert_eq!(
        manager.activate(id, vec![]),
        Err(PluginError::PermissionDenied)
    );
    manager
        .activate(id, one.manifest.permissions.clone())
        .unwrap();
    let updated = manager.update(id, package("2.0.0")).unwrap();
    assert!(!updated.grant.enabled);
    assert!(updated.grant.permissions.is_empty());
    assert_eq!(updated.grant.identity.installation_id, *id);
    let rolled = manager.rollback(id).unwrap();
    assert_eq!(rolled.version, "1.0.0");
    assert!(!rolled.grant.enabled);
    manager.remove(id).unwrap();
    let reinstalled = manager.install(one).unwrap();
    assert_ne!(reinstalled.grant.identity.installation_id, *id);
    drop(manager);
    let reopened = Manager::open(root.path().into()).unwrap();
    assert_eq!(reopened.list().unwrap().len(), 1);
    assert!(!reopened.list().unwrap()[0].grant.enabled);
}
#[test]
fn failed_persistent_disable_stays_denied_in_memory_and_reports_failure() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::open(root.path().into()).unwrap();
    let one = package("1.0.0");
    let installed = manager.install(one.clone()).unwrap();
    let id = installed.grant.identity.installation_id;
    manager.activate(&id, one.manifest.permissions).unwrap();
    std::fs::remove_file(root.path().join("grants.json")).unwrap();
    std::fs::create_dir(root.path().join("grants.json")).unwrap();
    assert_eq!(manager.disable(&id), Err(PluginError::StorageUnavailable));
    assert!(!manager.authority.record(&id).unwrap().enabled);
}
#[test]
fn newer_or_corrupt_grants_never_default_to_enabled() {
    for value in [r#"{"schema_version":99,"installations":{}}"#, "{not-json"] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("grants.json"), value).unwrap();
        assert!(Manager::open(root.path().into()).is_err());
    }
}
