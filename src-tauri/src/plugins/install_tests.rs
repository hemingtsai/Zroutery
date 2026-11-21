//! The workbuddy relay plugin, exercised through the real composition:
//! install → activate → instantiate → credential → synthesize → conversation.
//!
//! Hermetic assertions run always. The live conversation test is `#[ignore]`:
//! it sends one request to the real `www.workbuddy.cn` upstream with a
//! placeholder credential, so it documents what the gateway does with an
//! unauthenticated relay request rather than asserting on network state.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::secrets::KeychainSecrets;
use crate::state::Desktop;
use zroutery_core::config::{AppConfig, ProviderKind};
use zroutery_core::upstream::Upstream;
use zroutery_plugin_api::manifest::PluginManifestV1;
use zroutery_plugin_host::install::Package;

const MANIFEST: &[u8] = include_bytes!("../../../plugins/workbuddy/zroutery.plugin.json");
const UI: &[u8] = include_bytes!("../../../plugins/workbuddy/ui.mjs");

fn workbuddy_package() -> Package {
    Package::from_files(
        BTreeMap::from([
            ("zroutery.plugin.json".to_string(), MANIFEST.to_vec()),
            ("ui.mjs".to_string(), UI.to_vec()),
        ]),
        false,
    )
    .expect("the workbuddy package must decode")
}

fn desktop_with() -> (Arc<Desktop>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("zroutery-wb-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = Arc::new(KeychainSecrets::new(format!(
        "app.zroutery.test.{}",
        uuid::Uuid::new_v4()
    )));
    (
        Arc::new(Desktop::new(dir.clone(), AppConfig::default(), secrets)),
        dir,
    )
}

async fn installed_and_wired(
    desktop: &Desktop,
) -> (String, String, zroutery_core::config::ProviderConfig) {
    let package = workbuddy_package();
    let manager = desktop
        .plugins
        .manager
        .as_ref()
        .expect("the plugin manager opens");
    let installation = manager.install(package.clone()).expect("install");
    let installation_id = installation.grant.identity.installation_id.clone();
    manager
        .activate(&installation_id, package.manifest.permissions.clone())
        .expect("activate with the declared permissions");

    desktop
        .instantiate_plugin_instance(&installation_id, "workbuddy", "WB Test")
        .await
        .expect("instantiate");

    let revision = crate::state::config_revision(&desktop.core.config());
    let instance = desktop
        .core
        .config()
        .plugin_instances
        .last()
        .expect("the instance exists")
        .clone();
    desktop
        .store_plugin_slot(
            &instance.id,
            "access_token",
            "wb-placeholder-token",
            &revision,
            None,
        )
        .await
        .expect("the access token reaches the scoped keyring");

    let provider = desktop
        .core
        .config()
        .providers
        .iter()
        .find(|p| p.plugin_binding.is_some())
        .expect("the plugin provider synthesizes into the core config")
        .clone();
    (installation_id, instance.id, provider)
}

#[tokio::test]
async fn the_workbuddy_plugin_installs_and_the_relay_provider_synthesizes() {
    let (desktop, dir) = desktop_with();
    let (_installation_id, instance_id, provider) = installed_and_wired(&desktop).await;

    assert_eq!(provider.base_url, "https://www.workbuddy.cn/v2");
    assert_eq!(provider.kind, ProviderKind::OpenAICompatible);
    assert_eq!(
        provider.chat_url(),
        "https://www.workbuddy.cn/v2/chat/completions"
    );
    assert_eq!(
        provider
            .extra_headers
            .get("x-requested-with")
            .map(String::as_str),
        Some("XMLHttpRequest")
    );
    assert_eq!(
        provider.extra_headers.get("user-agent").map(String::as_str),
        Some("WorkBuddy/5.11.3 WorkBuddy/5.11.3 CLI/2.176.0")
    );
    assert!(!provider.extra_headers.contains_key("authorization"));

    let binding = provider.plugin_binding.as_ref().expect("the binding");
    let operation = binding
        .operation_for_url(&provider.chat_url())
        .expect("the declared operation covers the relay chat path");
    binding
        .authorize(operation)
        .expect("the active grant authorizes the chat operation");
    let slot = binding
        .selected_slot(&provider.key_ref, operation)
        .expect("slot lookup")
        .expect("the default slot is bound after the credential is stored");
    assert_eq!(
        binding
            .credential(&slot.key_ref, operation)
            .expect("credential resolution")
            .as_deref(),
        Some("wb-placeholder-token"),
        "the stored access token must resolve through the binding"
    );

    // The package view the manager keeps matches the manifest contribution.
    let manifest: PluginManifestV1 = PluginManifestV1::decode(MANIFEST).expect("manifest decodes");
    assert_eq!(manifest.id, "dev.zroutery.workbuddy");
    let _ = instance_id;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "sends one request to the real www.workbuddy.cn upstream; needs network"]
async fn a_relay_conversation_reaches_the_real_upstream() {
    let (desktop, dir) = desktop_with();
    let (_installation_id, _instance_id, provider) = installed_and_wired(&desktop).await;
    let upstream = Upstream::new(true, 5);
    let body = serde_json::json!({
        "model": "claude-sonnet-4-5-20250929",
        "messages": [{ "role": "user", "content": "只回复两个字：成功" }],
    });
    let result = upstream.send(&provider, None, &body).await;
    match &result {
        Err(zroutery_core::error::Error::Upstream { status, body, .. }) => {
            println!("the relay conversation reached the upstream: http {status} body={body}")
        }
        Err(zroutery_core::error::Error::Transport { source, .. }) => println!(
            "the connection stage failed (TLS/WAF layer): {source} — see SPIKE-FINDINGS.md"
        ),
        Err(other) => println!("pipeline error: {other}"),
        Ok(answer) => println!("unexpected success: model={} ", answer.model),
    }
    // The only assertion: the request traveled the plugin pipeline to the
    // network stage — never a composition-side denial.
    assert!(
        matches!(
            &result,
            Err(zroutery_core::error::Error::Upstream { .. })
                | Err(zroutery_core::error::Error::Transport { .. })
        ),
        "the conversation must reach the network stage through the installed plugin: {result:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
