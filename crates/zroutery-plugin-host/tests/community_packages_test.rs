//! Community plugin packages must decode exactly as the host will read them.
//!
//! Every packaged community provider under `plugins/` is compiled through the
//! same `Package` path an install takes: manifest decode, permission closure,
//! resource presence, runtime entry check. A package that fails here fails in
//! the UI with the same error, so the test keeps the shipped sources honest.

use std::collections::BTreeMap;
use std::path::Path;

use zroutery_plugin_host::install::Package;

fn package_from(name: &str, files: &[&str]) -> Package {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(name);
    let map = files
        .iter()
        .map(|file| {
            (
                (*file).to_string(),
                std::fs::read(directory.join(file)).expect("package resource"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    Package::from_files(map, false).expect("the community package must decode")
}

#[test]
fn the_workbuddy_relay_package_decodes_with_provider_and_ui() {
    let package = package_from("workbuddy", &["zroutery.plugin.json", "ui.mjs"]);
    assert_eq!(package.manifest.id, "dev.zroutery.workbuddy");
    let providers = &package.manifest.contributions.providers;
    assert_eq!(providers.len(), 1);
    let workbuddy = &providers[0];
    assert_eq!(workbuddy.id, "workbuddy");
    assert_eq!(
        workbuddy
            .operations
            .iter()
            .map(|operation| operation.id.as_str())
            .collect::<Vec<_>>(),
        ["chat", "models"]
    );
    assert_eq!(
        workbuddy.operations[0].base_url,
        "https://www.workbuddy.cn/v2"
    );
    assert_eq!(workbuddy.credentials.len(), 1);
    assert_eq!(workbuddy.credentials[0].id, "access_token");
    // The official CLI identity travels through the generic extra_headers map.
    assert_eq!(
        workbuddy
            .extra_headers
            .get("x-requested-with")
            .map(String::as_str),
        Some("XMLHttpRequest")
    );
    assert!(workbuddy.extra_headers.contains_key("user-agent"));
    assert!(!workbuddy.extra_headers.contains_key("authorization"));
    let ui = &package.manifest.contributions.ui;
    assert_eq!(ui.len(), 1);
    assert_eq!(ui[0].slot, zroutery_plugin_api::ui::UiSlot::Settings);
    assert_eq!(
        package
            .manifest
            .runtime
            .as_ref()
            .and_then(|r| r.entries.ui.clone()),
        Some("ui.mjs".into())
    );
    assert!(package.schema.is_none());

    // The packed artifact takes the same path an install takes: a flat zip of
    // the package resources round-trips through `from_archive`.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/workbuddy");
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for name in ["zroutery.plugin.json", "ui.mjs"] {
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .expect("zip entry");
        let data = std::fs::read(source.join(name)).expect("package resource");
        std::io::Write::write_all(&mut archive, &data).expect("zip write");
    }
    let bytes = archive.finish().expect("zip finish").into_inner();
    let packed = Package::from_archive(&bytes).expect("the packed archive must decode");
    assert_eq!(packed.digest, package.digest);
}

#[test]
fn the_opencode_zen_package_declares_shaping_and_an_anonymous_provider() {
    let package = package_from("opencode-zen", &["zroutery.plugin.json", "ui.mjs"]);
    assert_eq!(package.manifest.id, "dev.zroutery.opencode-zen");
    let providers = &package.manifest.contributions.providers;
    assert_eq!(providers.len(), 1);
    let zen = &providers[0];
    assert_eq!(zen.id, "zen");
    // Chat + catalog: the catalog operation is what registers each session.
    assert_eq!(zen.operations.len(), 2);
    assert_eq!(zen.operations[0].base_url, "https://opencode.ai/zen/v1");
    assert_eq!(
        zen.operations[1].service.id, "host.catalog.openai",
        "the catalog operation is the session registration vehicle"
    );
    // Anonymous: no credential slots at all.
    assert!(zen.credentials.is_empty());
    assert!(zen.operations.iter().all(|o| o.credential_slots.is_empty()));
    // The opencode_cli shaping profile drives sessions, identity and the env block.
    assert_eq!(zen.shaping.as_deref(), Some("opencode_cli"));
    assert!(zen.extra_headers.contains_key("accept"));
    assert!(!zen.extra_headers.contains_key("authorization"));
}
