//! Catalogue file ownership joins the same transaction as the client config.

use serde_json::json;
use zroutery_core::file_transaction;

use super::tests::desktop_with;

#[test]
fn codex_catalogue_restores_exact_original_bytes_and_refuses_manual_deletion() {
    let (desktop, dir) = desktop_with(Default::default());
    let folder = dir.join("takeover");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("codex-models.json");
    let original = b"{ \"models\": [], \"custom\": true }\n";
    std::fs::write(&path, original).unwrap();
    let changes = desktop
        .prepare_codex_catalogue(&json!({"models":[{"slug":"standard-class"}]}))
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    file_transaction::commit(&folder.join("test-journal"), &changes).unwrap();
    let written = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        desktop
            .prepare_codex_catalogue(&json!({"models":[]}))
            .is_err(),
        "a deleted owned file is an external edit"
    );
    std::fs::write(&path, written).unwrap();
    let restore = desktop.prepare_codex_restore(false).unwrap();
    file_transaction::commit(&folder.join("test-journal"), &restore).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!folder.join("codex-models-owner.json").exists());
    drop(desktop);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn keeping_external_catalogue_detaches_ownership_for_a_later_readopt() {
    let (desktop, dir) = desktop_with(Default::default());
    let folder = dir.join("takeover");
    let path = folder.join("codex-models.json");
    let changes = desktop
        .prepare_codex_catalogue(&json!({"models":[]}))
        .unwrap();
    file_transaction::commit(&folder.join("test-journal"), &changes).unwrap();
    let manual = b"{\"models\":[],\"external\":1}";
    std::fs::write(&path, manual).unwrap();
    assert!(desktop.prepare_codex_restore(false).is_err());
    let changes = desktop.prepare_codex_restore(true).unwrap();
    file_transaction::commit(&folder.join("test-journal"), &changes).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), manual);
    assert!(!folder.join("codex-models-owner.json").exists());
    assert!(desktop
        .prepare_codex_catalogue(&json!({"models":[]}))
        .is_ok());
    drop(desktop);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn no_op_catalogue_is_still_a_transaction_precondition() {
    let (desktop, dir) = desktop_with(Default::default());
    let folder = dir.join("takeover");
    let value = json!({"models":[]});
    let first = desktop.prepare_codex_catalogue(&value).unwrap();
    file_transaction::commit(&folder.join("test-journal"), &first).unwrap();
    let refresh = desktop.prepare_codex_catalogue(&value).unwrap();
    std::fs::write(folder.join("codex-models.json"), b"manual change").unwrap();
    assert!(file_transaction::commit(&folder.join("test-journal"), &refresh).is_err());
    assert_eq!(
        std::fs::read(folder.join("codex-models.json")).unwrap(),
        b"manual change"
    );
    drop(desktop);
    std::fs::remove_dir_all(dir).unwrap();
}
