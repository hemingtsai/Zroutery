//! Client files and their ownership publish as one recoverable operation.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use serde_json::{json, Value};
use zroutery_core::agent_takeover::{
    extract_field_values, AgentAdapter, AgentConfigSnapshot, AgentType, ClaudeAdapter,
    ConflictResolution, ManagedField, OwnershipState, TakeoverStore,
};
use zroutery_core::file_transaction::{self, FileChange};

fn snapshot(path: &Path) -> AgentConfigSnapshot {
    let bytes = std::fs::read(path).unwrap();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    AgentConfigSnapshot {
        agent_type: AgentType::Claude,
        config_path: path.to_owned(),
        config_hash: format!("{:x}", hasher.finish()),
        raw: serde_json::from_slice(&bytes).unwrap(),
    }
}

fn fields() -> Vec<ManagedField> {
    vec![ManagedField {
        path: "env.ANTHROPIC_BASE_URL".into(),
        value: json!("http://127.0.0.1:8787"),
    }]
}

fn maps(raw: &Value) -> (Vec<String>, HashMap<String, Value>, HashMap<String, Value>) {
    let paths: Vec<_> = fields().iter().map(|field| field.path.clone()).collect();
    let current = extract_field_values(raw, &paths);
    let managed = fields()
        .into_iter()
        .map(|field| (field.path, field.value))
        .collect();
    (paths, current, managed)
}

#[test]
fn staging_never_changes_live_files_and_refresh_restores_the_users_latest_edits() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://original.invalid"},"theme":"old"}"#,
    )
    .unwrap();
    let store = TakeoverStore::open(root.path().join("owner.json")).unwrap();
    let before = snapshot(&path);
    let (paths, current, managed) = maps(&before.raw);
    let ownership = store.prepare_adoption(&paths, &current, &managed).unwrap();
    let (_, mut writes) = ClaudeAdapter.prepare_patch(&before, &fields()).unwrap();
    writes.push(ownership.file.unwrap());
    assert_eq!(snapshot(&path).raw, before.raw);
    assert_eq!(store.state(), OwnershipState::Verified);
    file_transaction::commit(&root.path().join("journal"), &writes).unwrap();
    store.reload().unwrap();
    assert_eq!(store.state(), OwnershipState::Adopted);

    let mut changed = snapshot(&path).raw;
    changed["env"]["ANTHROPIC_BASE_URL"] = json!("https://manually-changed.invalid");
    changed["theme"] = json!("new");
    std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    let live = snapshot(&path);
    let (paths, current, managed) = maps(&live.raw);
    let refresh = store.prepare_adoption(&paths, &current, &managed).unwrap();
    let (_, mut writes) = ClaudeAdapter.prepare_patch(&live, &fields()).unwrap();
    writes.push(refresh.file.unwrap());
    file_transaction::commit(&root.path().join("journal"), &writes).unwrap();
    store.reload().unwrap();
    let live = snapshot(&path);
    let (_, current, _) = maps(&live.raw);
    let release = store
        .prepare_release_transaction(&current, ConflictResolution::KeepExternal)
        .unwrap();
    let mut writes = ClaudeAdapter
        .prepare_release(&live, &release.effective)
        .unwrap();
    writes.push(release.ownership.file.unwrap());
    file_transaction::commit(&root.path().join("journal"), &writes).unwrap();
    store.reload().unwrap();
    assert_eq!(store.state(), OwnershipState::Released);
    assert_eq!(
        snapshot(&path).raw,
        changed,
        "managed and unrelated hand edits survive the switch"
    );
}

#[test]
fn stale_secondary_file_prevents_the_whole_adoption_and_preserves_ownership() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.json");
    std::fs::write(&path, b"{}").unwrap();
    let catalogue = root.path().join("models.json");
    std::fs::write(&catalogue, b"user edit").unwrap();
    let store = TakeoverStore::open(root.path().join("owner.json")).unwrap();
    let before = snapshot(&path);
    let (paths, current, managed) = maps(&before.raw);
    let ownership = store.prepare_adoption(&paths, &current, &managed).unwrap();
    let (_, mut writes) = ClaudeAdapter.prepare_patch(&before, &fields()).unwrap();
    writes.push(ownership.file.unwrap());
    writes.push(FileChange {
        path: catalogue.clone(),
        before: Some(b"old".to_vec()),
        after: Some(b"new".to_vec()),
    });
    assert!(file_transaction::commit(&root.path().join("journal"), &writes).is_err());
    assert_eq!(snapshot(&path).raw, json!({}));
    assert_eq!(std::fs::read(catalogue).unwrap(), b"user edit");
    assert!(!root.path().join("owner.json").exists());
    assert_eq!(store.state(), OwnershipState::Verified);
}
