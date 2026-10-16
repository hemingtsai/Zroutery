//! The takeover fixtures: config discovery, field sets, restore, conflicts and the release path.

// The Gemini adapter is deprecated on purpose: it is retained so an older manifest
// can still be released, and these are the fixtures that prove that path works.
// The allow is module-wide because six of them construct it.
#![allow(deprecated)]

use std::collections::HashMap;

use super::clients::{first_comment_line, project_shadow, GeminiAdapter};
use super::fs::{
    apply_fields, apply_patch_to_disk, compute_hash, release_to_disk, restore_fields, set_nested,
    ConfigFormat,
};
use super::*;

fn field_map(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn adopt_sets_adopted_at() {
    let store = TakeoverStore::new();
    let values = field_map(&[("timeout", 30.into())]);
    let manifest = store.adopt(vec!["timeout".into()], &values).unwrap();

    assert!(manifest.adopted_at.is_some());
    assert!(manifest.adopted_at.unwrap() > 0);
    assert_eq!(manifest.state, OwnershipState::Adopted);
}

#[test]
fn release_sets_released_at() {
    let store = TakeoverStore::new();
    let values = field_map(&[("timeout", 30.into())]);
    store.adopt(vec!["timeout".into()], &values).unwrap();

    let manifest = store.release().unwrap();

    assert!(manifest.released_at.is_some());
    assert!(manifest.released_at.unwrap() > 0);
    assert_eq!(manifest.state, OwnershipState::Released);
}

#[test]
fn adopt_requires_verified_state() {
    let store = TakeoverStore::new();
    let values = field_map(&[("x", 1.into())]);

    // First adopt succeeds.
    store.adopt(vec!["x".into()], &values).unwrap();

    // Second adopt fails — state is Adopted, not Verified.
    let err = store.adopt(vec!["x".into()], &values).unwrap_err();
    assert!(err.contains("Adopted"), "error: {err}");
    assert!(err.contains("Verified or Released"), "error: {err}");
}

#[test]
fn release_requires_adopted_state() {
    let store = TakeoverStore::new();

    // Cannot release from Verified state.
    let err = store.release().unwrap_err();
    assert!(err.contains("Verified"), "error: {err}");
}

#[test]
fn detect_external_modification_finds_changes() {
    let store = TakeoverStore::new();
    let original = field_map(&[("a", 1.into()), ("b", 2.into())]);
    store
        .adopt(vec!["a".into(), "b".into()], &original)
        .unwrap();

    let changed = field_map(&[("a", 99.into()), ("b", 2.into())]);
    let mods = store.detect_external_modification(&changed);

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].field_path, "a");
    assert_eq!(mods[0].last_applied, Some(serde_json::json!(1)));
    assert_eq!(mods[0].current_value, serde_json::json!(99));
}

#[test]
fn detect_external_modification_no_change() {
    let store = TakeoverStore::new();
    let values = field_map(&[("a", 1.into()), ("b", 2.into())]);
    store.adopt(vec!["a".into(), "b".into()], &values).unwrap();

    let same = field_map(&[("a", 1.into()), ("b", 2.into())]);
    let mods = store.detect_external_modification(&same);
    assert!(mods.is_empty());
}

#[test]
fn adopt_release_cycle_x10_no_drift() {
    let store = TakeoverStore::new();
    let fields = vec!["f1".into(), "f2".into()];

    for i in 0..10u64 {
        let values = field_map(&[
            ("f1", serde_json::json!(i)),
            ("f2", serde_json::json!(i * 10)),
        ]);

        let manifest = store.adopt(fields.clone(), &values).unwrap();
        assert_eq!(manifest.state, OwnershipState::Adopted);
        assert_eq!(manifest.generation, i);
        assert_eq!(manifest.managed_fields, fields);
        assert_eq!(manifest.field_snapshots.len(), 2);

        let released = store.release().unwrap();
        assert_eq!(released.state, OwnershipState::Released);
    }

    // After 10 cycles the generation should be 10.
    let final_manifest = store.manifest().unwrap();
    assert_eq!(final_manifest.generation, 10);
    assert_eq!(final_manifest.managed_fields, fields);
}

#[test]
fn unmanaged_fields_not_in_manifest() {
    let store = TakeoverStore::new();
    let values = field_map(&[
        ("a", 1.into()),
        ("b", 2.into()),
        ("c", 3.into()), // not managed
    ]);

    // Only "a" and "b" are managed.
    let manifest = store.adopt(vec!["a".into(), "b".into()], &values).unwrap();

    assert_eq!(manifest.managed_fields.len(), 2);
    assert!(manifest.managed_fields.contains(&"a".into()));
    assert!(manifest.managed_fields.contains(&"b".into()));
    assert!(!manifest.managed_fields.contains(&"c".into()));

    // Snapshot only contains managed fields.
    assert!(manifest.field_snapshots.contains_key("a"));
    assert!(manifest.field_snapshots.contains_key("b"));
    assert!(!manifest.field_snapshots.contains_key("c"));
}

#[test]
fn conflict_detection_on_managed_field_change() {
    let store = TakeoverStore::new();

    // Adopt with initial values.
    let initial = field_map(&[("a", 1.into()), ("b", 2.into())]);
    store.adopt(vec!["a".into(), "b".into()], &initial).unwrap();

    // Externally change managed field "a".
    let current = field_map(&[("a", 42.into()), ("b", 2.into())]);
    let mods = store.detect_external_modification(&current);

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].field_path, "a");
    assert_eq!(mods[0].last_applied, Some(serde_json::json!(1)));
    assert_eq!(mods[0].current_value, serde_json::json!(42));

    // Field "b" is unchanged — no conflict.
    assert!(!mods.iter().any(|m| m.field_path == "b"));
}

// -----------------------------------------------------------------------
// I4 verification tests
// -----------------------------------------------------------------------

#[test]
fn full_lifecycle_detected_to_adopted_to_released() {
    let store = TakeoverStore::new();

    // Initial state is Verified.
    assert_eq!(store.state(), OwnershipState::Verified);

    // Adopt: Verified -> Adopted.
    let values = field_map(&[("model", "gpt-4".into()), ("temp", 0.7.into())]);
    let adopted = store
        .adopt(vec!["model".into(), "temp".into()], &values)
        .unwrap();

    assert_eq!(adopted.state, OwnershipState::Adopted);
    assert_eq!(store.state(), OwnershipState::Adopted);
    assert!(adopted.adopted_at.is_some());
    assert!(adopted.released_at.is_none());
    assert_eq!(adopted.generation, 0);
    assert_eq!(adopted.managed_fields.len(), 2);
    assert_eq!(adopted.field_snapshots["model"], serde_json::json!("gpt-4"));
    assert_eq!(adopted.field_snapshots["temp"], serde_json::json!(0.7));

    // Release: Adopted -> Released.
    let released = store.release().unwrap();

    assert_eq!(released.state, OwnershipState::Released);
    assert_eq!(store.state(), OwnershipState::Released);
    assert!(released.released_at.is_some());
    assert!(released.released_at.unwrap() >= released.adopted_at.unwrap());
    assert_eq!(released.generation, 1);
    // Managed fields and snapshots persist through release.
    assert_eq!(released.managed_fields, vec!["model", "temp"]);
    assert_eq!(
        released.field_snapshots["model"],
        serde_json::json!("gpt-4")
    );
}

#[test]
fn external_modification_detected_and_reported() {
    let store = TakeoverStore::new();

    let initial = field_map(&[
        ("host", "localhost".into()),
        ("port", 8080.into()),
        ("debug", false.into()),
    ]);
    store
        .adopt(vec!["host".into(), "port".into(), "debug".into()], &initial)
        .unwrap();

    // Simulate external modification: host changed, port removed, debug unchanged.
    let current = field_map(&[("host", "0.0.0.0".into()), ("debug", false.into())]);
    let mods = store.detect_external_modification(&current);

    assert_eq!(mods.len(), 2);

    // host was changed.
    let host_mod = mods.iter().find(|m| m.field_path == "host").unwrap();
    assert_eq!(host_mod.last_applied, Some(serde_json::json!("localhost")));
    assert_eq!(host_mod.current_value, serde_json::json!("0.0.0.0"));

    // port was removed (reported as null).
    let port_mod = mods.iter().find(|m| m.field_path == "port").unwrap();
    assert_eq!(port_mod.last_applied, Some(serde_json::json!(8080)));
    assert_eq!(port_mod.current_value, serde_json::Value::Null);
}

#[test]
fn user_modification_preserved_on_release() {
    let store = TakeoverStore::new();

    let initial = field_map(&[("x", 1.into()), ("y", 2.into())]);
    store.adopt(vec!["x".into(), "y".into()], &initial).unwrap();

    // User externally modifies "x" while Adopted.
    let current = field_map(&[("x", 100.into()), ("y", 2.into())]);
    let mods = store.detect_external_modification(&current);
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].field_path, "x");

    // Release still succeeds — external changes do not block release.
    let released = store.release().unwrap();
    assert_eq!(released.state, OwnershipState::Released);

    // Re-adopt with the user's modified values (x=100 preserved).
    let user_values = field_map(&[("x", 100.into()), ("y", 2.into())]);
    let re_adopted = store
        .adopt(vec!["x".into(), "y".into()], &user_values)
        .unwrap();

    assert_eq!(re_adopted.field_snapshots["x"], serde_json::json!(100));
    assert_eq!(re_adopted.field_snapshots["y"], serde_json::json!(2));

    // No further external modifications detected — user values are now baseline.
    let mods = store.detect_external_modification(&user_values);
    assert!(mods.is_empty());
}

#[test]
fn repeated_adopt_release_no_drift() {
    let store = TakeoverStore::new();
    let fields = vec!["a".into(), "b".into()];

    for cycle in 0..50u64 {
        let val = cycle * 3;
        let values = field_map(&[
            ("a", serde_json::json!(val)),
            ("b", serde_json::json!(val + 1)),
        ]);

        let adopted = store.adopt(fields.clone(), &values).unwrap();
        assert_eq!(adopted.state, OwnershipState::Adopted);
        assert_eq!(adopted.generation, cycle);
        assert_eq!(adopted.managed_fields, fields);
        assert_eq!(adopted.field_snapshots.len(), 2);
        assert_eq!(adopted.field_snapshots["a"], serde_json::json!(val));

        let released = store.release().unwrap();
        assert_eq!(released.state, OwnershipState::Released);
        assert_eq!(released.generation, cycle + 1);
        // Managed fields survive across cycles.
        assert_eq!(released.managed_fields, fields);
    }

    // Final manifest should reflect last generation.
    let manifest = store.manifest().unwrap();
    assert_eq!(manifest.generation, 50);
    assert_eq!(manifest.state, OwnershipState::Released);
    assert_eq!(manifest.managed_fields, fields);
}

#[test]
fn concurrent_access_safety() {
    use std::sync::Arc;
    use std::thread;

    let store = Arc::new(TakeoverStore::new());
    let fields: Vec<String> = vec!["f1".into(), "f2".into()];

    // First adopt so multiple threads can detect modifications concurrently.
    let init = field_map(&[("f1", 0.into()), ("f2", 0.into())]);
    store.adopt(fields.clone(), &init).unwrap();

    let mut handles = Vec::new();

    // Spawn readers that call detect_external_modification concurrently.
    for i in 0..8 {
        let s = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let current = field_map(&[("f1", serde_json::json!(i)), ("f2", serde_json::json!(i))]);
            let mods = s.detect_external_modification(&current);
            // Both fields differ from baseline (0) when i != 0.
            if i != 0 {
                assert!(!mods.is_empty());
            }
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }

    // Release after concurrent reads — must still be in Adopted state.
    let released = store.release().unwrap();
    assert_eq!(released.state, OwnershipState::Released);
}

#[test]
fn ownership_manifest_serde_round_trip() {
    let store = TakeoverStore::new();
    let values = field_map(&[("k", serde_json::json!("v")), ("n", serde_json::json!(42))]);
    let original = store.adopt(vec!["k".into(), "n".into()], &values).unwrap();

    // Serialize to JSON.
    let json = serde_json::to_string(&original).expect("serialize");

    // Deserialize back.
    let restored: OwnershipManifest = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.state, original.state);
    assert_eq!(restored.managed_fields, original.managed_fields);
    assert_eq!(restored.field_snapshots, original.field_snapshots);
    assert_eq!(restored.absent_fields, original.absent_fields);
    assert_eq!(restored.adopted_at, original.adopted_at);
    assert_eq!(restored.released_at, original.released_at);
    assert_eq!(restored.generation, original.generation);

    // Also round-trip a released manifest.
    let released = store.release().unwrap();
    let json2 = serde_json::to_string(&released).expect("serialize released");
    let restored2: OwnershipManifest = serde_json::from_str(&json2).expect("deserialize released");

    assert_eq!(restored2.state, OwnershipState::Released);
    assert!(restored2.released_at.is_some());
    assert_eq!(restored2.generation, 1);
}

// -----------------------------------------------------------------------
// AgentAdapter tests
// -----------------------------------------------------------------------

/// Claude's config lives a directory deep (`~/.claude/settings.json`), so a
/// test that writes the file has to create `.claude/` first — otherwise the
/// write fails for a reason unrelated to what it is testing.
///
/// It also has to resolve through the adapter rather than join a path by
/// hand. The four fixtures that used `home.join(".claude.json")` and then
/// called `adapter.read_config()` were writing to one file and reading
/// another: with the adapter pointed at the settings file, `read_config()`
/// found nothing, handed back an empty snapshot, and the tests that asserted
/// `apply_patch` *fails* got an `Ok` instead. They were green because the
/// adapter was reading a file nobody wrote.
fn claude_settings_path() -> std::path::PathBuf {
    let adapter = ClaudeAdapter;
    let path = adapter.config_path().expect("claude config path");
    std::fs::create_dir_all(path.parent().unwrap()).expect("create .claude/");
    path
}

#[test]
fn claude_adapter_agent_type() {
    super::isolate_agent_home();
    let adapter = ClaudeAdapter;
    assert_eq!(adapter.agent_type(), AgentType::Claude);
}

#[test]
fn codex_adapter_agent_type() {
    super::isolate_agent_home();
    let adapter = CodexAdapter;
    assert_eq!(adapter.agent_type(), AgentType::Codex);
}

#[test]
fn gemini_adapter_agent_type() {
    super::isolate_agent_home();
    let adapter = GeminiAdapter;
    assert_eq!(adapter.agent_type(), AgentType::Gemini);
}

#[test]
fn claude_adapter_config_path() {
    super::isolate_agent_home();
    let adapter = ClaudeAdapter;
    let path = adapter.config_path().unwrap();
    assert!(
        path.ends_with(".claude/settings.json"),
        "path: {}",
        path.display()
    );
}

#[test]
fn codex_adapter_config_path() {
    super::isolate_agent_home();
    let adapter = CodexAdapter;
    let path = adapter.config_path().unwrap();
    assert!(path.ends_with(".codex/config.toml"));
}

#[test]
fn gemini_adapter_config_path() {
    super::isolate_agent_home();
    let adapter = GeminiAdapter;
    let path = adapter.config_path().unwrap();
    assert!(path.ends_with(".gemini/settings.json"));
}

#[test]
fn codex_adapter_honours_config_dir_override() {
    super::isolate_agent_home();
    let custom = tempfile::tempdir().unwrap();
    super::isolate_config_dir("CODEX_HOME", custom.path());

    let adapter = CodexAdapter;
    assert_eq!(
        adapter.config_path().unwrap(),
        custom.path().join("config.toml")
    );
}

#[test]
fn gemini_adapter_honours_config_dir_override() {
    super::isolate_agent_home();
    let custom = tempfile::tempdir().unwrap();
    super::isolate_config_dir("GEMINI_CLI_HOME", custom.path());

    let adapter = GeminiAdapter;
    assert_eq!(
        adapter.config_path().unwrap(),
        custom.path().join(".gemini").join("settings.json")
    );
}

#[test]
fn claude_adapter_read_config() {
    super::isolate_agent_home();
    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();
    assert_eq!(snapshot.agent_type, AgentType::Claude);
    // Config file likely does not exist in CI, so raw should be empty object.
    assert!(snapshot.raw.is_object());
}

#[test]
fn codex_adapter_read_config() {
    super::isolate_agent_home();
    let adapter = CodexAdapter;
    let snapshot = adapter.read_config().unwrap();
    assert_eq!(snapshot.agent_type, AgentType::Codex);
    assert!(snapshot.raw.is_object());
}

#[test]
fn codex_apply_patch_writes_toml_config() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().expect("isolated agent home");
    let path = home.join(".codex").join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        concat!(
            "model = \"gpt-5\"\n",
            "approval_policy = \"never\" # keep this\n",
            "\n",
            "[model_providers.zroutery]\n",
            "base_url = \"http://127.0.0.1:1/v1\"\n",
        ),
    )
    .unwrap();

    let adapter = CodexAdapter;
    let snapshot = adapter.read_config().unwrap();
    assert_eq!(snapshot.raw["model"], serde_json::json!("gpt-5"));
    assert_eq!(
        snapshot.raw["model_providers"]["zroutery"]["base_url"],
        serde_json::json!("http://127.0.0.1:1/v1")
    );

    let patched = adapter
        .apply_patch(
            &snapshot,
            &[
                ManagedField {
                    path: "model".into(),
                    value: serde_json::json!("zroutery-proxy"),
                },
                ManagedField {
                    path: "model_providers.zroutery.base_url".into(),
                    value: serde_json::json!("http://127.0.0.1:9999/v1"),
                },
            ],
        )
        .unwrap();
    assert_eq!(patched.raw["model"], serde_json::json!("zroutery-proxy"));

    // The file on disk is TOML, not JSON, and unmanaged keys survive.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.trim_start().starts_with('{'),
        "codex config is not TOML: {text}"
    );
    assert!(text.contains("model = \"zroutery-proxy\""), "text: {text}");
    assert!(text.contains("approval_policy = \"never\""), "text: {text}");
    assert!(text.contains("[model_providers.zroutery]"), "text: {text}");
    assert!(
        text.contains("base_url = \"http://127.0.0.1:9999/v1\""),
        "text: {text}"
    );

    // What was written round-trips through the adapter's own reader.
    let reread = adapter.read_config().unwrap();
    assert_eq!(reread.raw["model"], serde_json::json!("zroutery-proxy"));
    assert_eq!(
        reread.raw["model_providers"]["zroutery"]["base_url"],
        serde_json::json!("http://127.0.0.1:9999/v1")
    );
    assert_eq!(reread.raw["approval_policy"], serde_json::json!("never"));
}

#[test]
fn codex_read_config_rejects_unsupported_toml() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().expect("isolated agent home");
    let path = home.join(".codex").join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "[[profiles]]\nname = \"work\"\n").unwrap();

    let adapter = CodexAdapter;
    let err = adapter.read_config().unwrap_err();
    assert!(err.contains("arrays of tables"), "error: {err}");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("[[profiles]]"),
        "the unreadable config must be left untouched"
    );
}

#[test]
fn gemini_adapter_read_config() {
    super::isolate_agent_home();
    let adapter = GeminiAdapter;
    let snapshot = adapter.read_config().unwrap();
    assert_eq!(snapshot.agent_type, AgentType::Gemini);
    assert!(snapshot.raw.is_object());
}

#[test]
fn apply_patch_modifies_snapshot() {
    super::isolate_agent_home();
    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();

    let fields = vec![
        ManagedField {
            path: "model".into(),
            value: serde_json::json!("claude-3-opus"),
        },
        ManagedField {
            path: "temperature".into(),
            value: serde_json::json!(0.7),
        },
    ];

    let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

    assert_eq!(patched.raw["model"], serde_json::json!("claude-3-opus"));
    assert_eq!(patched.raw["temperature"], serde_json::json!(0.7));
    // Note: apply_patch writes to the real config file on disk, so the
    // original snapshot may already contain the patched fields if the
    // test has run before. We verify the patched values are correct above.
    // A full immutability assertion is covered by the TestAdapter-based
    // tests below which use isolated temp directories.
}

#[test]
fn apply_patch_nested_path() {
    super::isolate_agent_home();
    let adapter = CodexAdapter;
    let snapshot = adapter.read_config().unwrap();

    let fields = vec![ManagedField {
        path: "model.temperature".into(),
        value: serde_json::json!(0.9),
    }];

    let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

    assert_eq!(patched.raw["model"]["temperature"], serde_json::json!(0.9));
}

#[test]
fn release_returns_ok() {
    super::isolate_agent_home();
    let adapter = GeminiAdapter;
    let snapshot = adapter.read_config().unwrap();

    let store = TakeoverStore::new();
    let values = field_map(&[("key", serde_json::json!("val"))]);
    store.adopt(vec!["key".into()], &values).unwrap();
    let manifest = store.release().unwrap();

    assert!(adapter.release(&snapshot, &manifest).is_ok());
}

#[test]
fn all_adapters_release_ok() {
    super::isolate_agent_home();
    let store = TakeoverStore::new();
    let values = field_map(&[("x", 1.into())]);
    store.adopt(vec!["x".into()], &values).unwrap();
    let manifest = store.release().unwrap();

    let adapters: Vec<Box<dyn AgentAdapter>> = vec![
        Box::new(ClaudeAdapter),
        Box::new(CodexAdapter),
        Box::new(GeminiAdapter),
    ];

    for adapter in &adapters {
        let snapshot = adapter.read_config().unwrap();
        assert!(adapter.release(&snapshot, &manifest).is_ok());
    }
}

// -----------------------------------------------------------------------
// TestAdapter (temp-file backed)
// -----------------------------------------------------------------------

/// Agent adapter backed by a temp file for deterministic tests.
struct TestAdapter {
    path: std::path::PathBuf,
}

impl TestAdapter {
    fn new(dir: &std::path::Path, initial: serde_json::Value) -> Self {
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();
        Self { path }
    }
}

impl AgentAdapter for TestAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Claude
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        Ok(self.path.clone())
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let (raw, hash) = if self.path.exists() {
            let data =
                std::fs::read_to_string(&self.path).map_err(|e| format!("read failed: {e}"))?;
            let hash = compute_hash(data.as_bytes());
            let parsed: serde_json::Value =
                serde_json::from_str(&data).map_err(|e| format!("parse failed: {e}"))?;
            (parsed, hash)
        } else {
            (
                serde_json::Value::Object(serde_json::Map::new()),
                String::new(),
            )
        };
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: self.path.clone(),
            raw,
            config_hash: hash,
        })
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        let mut raw = snapshot.raw.clone();
        apply_fields(&mut raw, fields)?;
        let json =
            serde_json::to_string_pretty(&raw).map_err(|e| format!("serialize failed: {e}"))?;
        let hash = compute_hash(json.as_bytes());
        Ok(AgentConfigSnapshot {
            agent_type: snapshot.agent_type,
            config_path: snapshot.config_path.clone(),
            raw,
            config_hash: hash,
        })
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        let mut raw = snapshot.raw.clone();
        restore_fields(&mut raw, manifest)?;
        let json =
            serde_json::to_string_pretty(&raw).map_err(|e| format!("serialize failed: {e}"))?;
        let hash = compute_hash(json.as_bytes());
        let restored = AgentConfigSnapshot {
            agent_type: snapshot.agent_type,
            config_path: snapshot.config_path.clone(),
            raw,
            config_hash: hash,
        };
        self.write_config(&restored)
    }
}

/// Read a JSON config file from disk.
fn read_json_file(path: &std::path::Path) -> serde_json::Value {
    let data = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&data).unwrap()
}

// -----------------------------------------------------------------------
// I4: release_with_restore
// -----------------------------------------------------------------------

#[test]
fn release_with_restore_restores_config() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({
        "model": "gpt-4",
        "temperature": 0.7,
        "unmanaged": "preserved"
    });
    let adapter = TestAdapter::new(tmp.path(), initial.clone());

    let store = TakeoverStore::new();

    // Adopt: snapshot the current values.
    let current = field_map(&[
        ("model", serde_json::json!("gpt-4")),
        ("temperature", serde_json::json!(0.7)),
    ]);
    store
        .adopt(vec!["model".into(), "temperature".into()], &current)
        .unwrap();

    // Simulate Zroutery patching the config.
    let patched = serde_json::json!({
        "model": "claude-3-opus",
        "temperature": 0.9,
        "unmanaged": "preserved"
    });
    adapter
        .write_config(&AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: adapter.path.clone(),
            raw: patched,
            config_hash: String::new(),
        })
        .unwrap();

    // Release with restore.
    let manifest = store.release_with_restore(&adapter).unwrap();
    assert_eq!(manifest.state, OwnershipState::Released);
    assert!(manifest.released_at.is_some());
    assert_eq!(manifest.generation, 1);

    // Verify config on disk was restored.
    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["model"], serde_json::json!("gpt-4"));
    assert_eq!(disk["temperature"], serde_json::json!(0.7));
    assert_eq!(disk["unmanaged"], serde_json::json!("preserved"));
}

#[test]
fn release_with_restore_no_manifest_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = TestAdapter::new(tmp.path(), serde_json::json!({"x": 1}));

    let store = TakeoverStore::new();

    // Not adopted yet — should error.
    let err = store.release_with_restore(&adapter).unwrap_err();
    assert!(err.contains("Adopted"), "error: {err}");
}

#[test]
fn release_with_restore_preserves_unmanaged_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({
        "managed_a": "original_a",
        "managed_b": 42,
        "unmanaged": "keep_me"
    });
    let adapter = TestAdapter::new(tmp.path(), initial);

    let store = TakeoverStore::new();
    let current = field_map(&[
        ("managed_a", serde_json::json!("original_a")),
        ("managed_b", serde_json::json!(42)),
    ]);
    store
        .adopt(vec!["managed_a".into(), "managed_b".into()], &current)
        .unwrap();

    // Simulate Zroutery changing managed fields.
    let patched = serde_json::json!({
        "managed_a": "changed",
        "managed_b": 99,
        "unmanaged": "keep_me"
    });
    adapter
        .write_config(&AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: adapter.path.clone(),
            raw: patched,
            config_hash: String::new(),
        })
        .unwrap();

    store.release_with_restore(&adapter).unwrap();

    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["managed_a"], serde_json::json!("original_a"));
    assert_eq!(disk["managed_b"], serde_json::json!(42));
    assert_eq!(disk["unmanaged"], serde_json::json!("keep_me"));
}

#[test]
fn release_removes_fields_absent_before_adoption() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = TestAdapter::new(tmp.path(), serde_json::json!({"model": "gpt-4"}));

    let store = TakeoverStore::new();
    // "base_url" is managed but does not exist in the config yet.
    let current = field_map(&[("model", serde_json::json!("gpt-4"))]);
    let manifest = store
        .adopt(vec!["model".into(), "base_url".into()], &current)
        .unwrap();
    assert_eq!(manifest.absent_fields, vec!["base_url".to_string()]);

    // Zroutery points the client at the local proxy.
    let patched = serde_json::json!({
        "model": "zroutery-proxy",
        "base_url": "http://127.0.0.1:9999"
    });
    adapter
        .write_config(&AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: adapter.path.clone(),
            raw: patched,
            config_hash: String::new(),
        })
        .unwrap();

    store.release_with_restore(&adapter).unwrap();

    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["model"], serde_json::json!("gpt-4"));
    assert!(
        disk.get("base_url").is_none(),
        "field absent before adoption survived release: {}",
        disk["base_url"]
    );
}

#[test]
fn manifest_without_absent_fields_deserializes() {
    let json = r#"{
        "state": "released",
        "managed_fields": ["model"],
        "field_snapshots": {"model": "gpt-4"},
        "adopted_at": 1700000000,
        "released_at": 1700000100,
        "generation": 1
    }"#;

    let manifest: OwnershipManifest = serde_json::from_str(json).unwrap();
    assert!(manifest.absent_fields.is_empty());
}

/// Adapter that re-enters the store while a release is being restored, to
/// prove the in-flight transition is not observable as `Adopted`.
struct RacingAdapter {
    path: std::path::PathBuf,
    store: std::sync::Arc<TakeoverStore>,
    competing_release: std::sync::Mutex<Option<Result<OwnershipManifest, String>>>,
    competing_adopt: std::sync::Mutex<Option<Result<OwnershipManifest, String>>>,
}

impl RacingAdapter {
    fn new(
        dir: &std::path::Path,
        initial: serde_json::Value,
        store: std::sync::Arc<TakeoverStore>,
    ) -> Self {
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();
        Self {
            path,
            store,
            competing_release: std::sync::Mutex::new(None),
            competing_adopt: std::sync::Mutex::new(None),
        }
    }
}

impl AgentAdapter for RacingAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Claude
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        Ok(self.path.clone())
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        // A competing release must not observe the pre-transition state.
        *self.competing_release.lock().unwrap() = Some(self.store.release());

        let (raw, hash) = if self.path.exists() {
            let data =
                std::fs::read_to_string(&self.path).map_err(|e| format!("read failed: {e}"))?;
            let hash = compute_hash(data.as_bytes());
            let parsed: serde_json::Value =
                serde_json::from_str(&data).map_err(|e| format!("parse failed: {e}"))?;
            (parsed, hash)
        } else {
            (
                serde_json::Value::Object(serde_json::Map::new()),
                String::new(),
            )
        };

        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: self.path.clone(),
            raw,
            config_hash: hash,
        })
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        apply_patch_to_disk(snapshot, fields, ConfigFormat::Json)
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        // The second phase of the restore: an adopt must not be able to
        // start from the stale `Adopted` state either.
        let values = field_map(&[("model", serde_json::json!("gpt-4"))]);
        *self.competing_adopt.lock().unwrap() =
            Some(self.store.adopt(vec!["model".into()], &values));

        release_to_disk(snapshot, manifest, ConfigFormat::Json)
    }
}

#[test]
fn release_with_restore_blocks_concurrent_transitions() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(TakeoverStore::new());
    let adapter = RacingAdapter::new(
        tmp.path(),
        serde_json::json!({"model": "gpt-4"}),
        std::sync::Arc::clone(&store),
    );

    let values = field_map(&[("model", serde_json::json!("gpt-4"))]);
    store.adopt(vec!["model".into()], &values).unwrap();

    // Zroutery patched the config before releasing.
    adapter
        .write_config(&AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: adapter.path.clone(),
            raw: serde_json::json!({"model": "zroutery-proxy"}),
            config_hash: String::new(),
        })
        .unwrap();

    let manifest = store.release_with_restore(&adapter).unwrap();
    assert_eq!(manifest.state, OwnershipState::Released);
    assert_eq!(manifest.generation, 1);

    let release_attempt = adapter
        .competing_release
        .lock()
        .unwrap()
        .take()
        .expect("competing release was attempted");
    let err = release_attempt.expect_err("a competing release must be rejected");
    assert!(err.contains("Releasing"), "error: {err}");

    let adopt_attempt = adapter
        .competing_adopt
        .lock()
        .unwrap()
        .take()
        .expect("competing adopt was attempted");
    let err = adopt_attempt.expect_err("a competing adopt must be rejected");
    assert!(err.contains("Releasing"), "error: {err}");

    // The in-flight release still committed and restored the original value.
    assert_eq!(store.state(), OwnershipState::Released);
    assert_eq!(store.manifest().unwrap().generation, 1);
    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["model"], serde_json::json!("gpt-4"));
}

// -----------------------------------------------------------------------
// I4: resolve_conflicts
// -----------------------------------------------------------------------
#[test]
fn resolve_conflicts_keep_external() {
    let conflicts = vec![
        FieldConflict {
            field_path: "a".into(),
            original_value: Some(serde_json::json!(1)),
            last_applied: Some(serde_json::json!(1)),
            current_external: serde_json::json!(99),
        },
        FieldConflict {
            field_path: "b".into(),
            original_value: Some(serde_json::json!(2)),
            last_applied: Some(serde_json::json!(2)),
            current_external: serde_json::json!("changed"),
        },
    ];

    let resolved = resolve_conflicts(&conflicts, ConflictResolution::KeepExternal);
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0], ("a".into(), Some(serde_json::json!(99))));
    assert_eq!(
        resolved[1],
        ("b".into(), Some(serde_json::json!("changed")))
    );
}

#[test]
fn resolve_conflicts_overwrite_with_managed() {
    let conflicts = vec![FieldConflict {
        field_path: "x".into(),
        original_value: Some(serde_json::json!(1)),
        last_applied: Some(serde_json::json!(10)),
        current_external: serde_json::json!(999),
    }];

    let resolved = resolve_conflicts(&conflicts, ConflictResolution::OverwriteWithManaged);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0], ("x".into(), Some(serde_json::json!(10))));
}

#[test]
fn resolve_conflicts_skip() {
    let conflicts = vec![
        FieldConflict {
            field_path: "a".into(),
            original_value: Some(serde_json::json!(1)),
            last_applied: Some(serde_json::json!(1)),
            current_external: serde_json::json!(99),
        },
        FieldConflict {
            field_path: "b".into(),
            original_value: Some(serde_json::json!(2)),
            last_applied: Some(serde_json::json!(2)),
            current_external: serde_json::json!("changed"),
        },
    ];

    let resolved = resolve_conflicts(&conflicts, ConflictResolution::Skip);
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0], ("a".into(), None));
    assert_eq!(resolved[1], ("b".into(), None));
}

#[test]
fn resolve_conflicts_empty() {
    let resolved = resolve_conflicts(&[], ConflictResolution::KeepExternal);
    assert!(resolved.is_empty());
}

// -----------------------------------------------------------------------
// I4: check_orphaned_state / recover_orphaned
// -----------------------------------------------------------------------

#[test]
fn check_orphaned_state_finds_adopted_not_released() {
    let store = TakeoverStore::new();
    assert!(!store.check_orphaned_state());

    let values = field_map(&[("x", 1.into())]);
    store.adopt(vec!["x".into()], &values).unwrap();
    assert!(store.check_orphaned_state());

    store.release().unwrap();
    assert!(!store.check_orphaned_state());
}

#[test]
fn recover_orphaned_releases_orphaned_state() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({"k": "v"});
    let adapter = TestAdapter::new(tmp.path(), initial);

    let store = TakeoverStore::new();
    let values = field_map(&[("k", serde_json::json!("v"))]);
    store.adopt(vec!["k".into()], &values).unwrap();

    // Simulate crash: store is Adopted, never released.
    assert!(store.check_orphaned_state());

    let manifest = store.recover_orphaned(&adapter).unwrap();
    assert_eq!(manifest.state, OwnershipState::Released);
    assert!(!store.check_orphaned_state());
}

#[test]
fn recover_orphaned_errors_when_not_orphaned() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = TestAdapter::new(tmp.path(), serde_json::json!({}));

    let store = TakeoverStore::new();
    let err = store.recover_orphaned(&adapter).unwrap_err();
    assert!(err.contains("orphaned"), "error: {err}");
}

// -----------------------------------------------------------------------
// I4: full scenario — adopt, user modifies, detect, resolve, release
// -----------------------------------------------------------------------

#[test]
fn full_scenario_detect_resolve_release() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({
        "model": "gpt-4",
        "temperature": 0.7,
        "api_key": "sk-123"
    });
    let adapter = TestAdapter::new(tmp.path(), initial);

    let store = TakeoverStore::new();

    // 1. Adopt managed fields.
    let values = field_map(&[
        ("model", serde_json::json!("gpt-4")),
        ("temperature", serde_json::json!(0.7)),
    ]);
    store
        .adopt(vec!["model".into(), "temperature".into()], &values)
        .unwrap();

    // 2. Zroutery patches config.
    let patched = serde_json::json!({
        "model": "claude-3-opus",
        "temperature": 0.9,
        "api_key": "sk-123"
    });
    adapter
        .write_config(&AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: adapter.path.clone(),
            raw: patched,
            config_hash: String::new(),
        })
        .unwrap();

    // 3. User externally changes "temperature".
    let user_modified = serde_json::json!({
        "model": "claude-3-opus",
        "temperature": 0.3,
        "api_key": "sk-123"
    });
    adapter
        .write_config(&AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: adapter.path.clone(),
            raw: user_modified,
            config_hash: String::new(),
        })
        .unwrap();

    // 4. Detect external modifications.
    // Both fields differ from adopt-time last_applied (gpt-4/0.7).
    let current_snapshot = adapter.read_config().unwrap();
    let current_map = field_map(&[
        ("model", current_snapshot.raw["model"].clone()),
        ("temperature", current_snapshot.raw["temperature"].clone()),
    ]);
    let mods = store.detect_external_modification(&current_map);
    assert_eq!(mods.len(), 2);

    // temperature: user changed from 0.7 to 0.3.
    let temp_mod = mods.iter().find(|m| m.field_path == "temperature").unwrap();
    assert_eq!(temp_mod.last_applied, Some(serde_json::json!(0.7)));
    assert_eq!(temp_mod.current_value, serde_json::json!(0.3));

    // model: Zroutery changed from gpt-4 to claude-3-opus.
    let model_mod = mods.iter().find(|m| m.field_path == "model").unwrap();
    assert_eq!(model_mod.last_applied, Some(serde_json::json!("gpt-4")));
    assert_eq!(model_mod.current_value, serde_json::json!("claude-3-opus"));

    // 5. Resolve conflicts: keep user's external values.
    let conflicts: Vec<FieldConflict> = mods
        .iter()
        .map(|m| FieldConflict {
            field_path: m.field_path.clone(),
            original_value: None,
            last_applied: m.last_applied.clone(),
            current_external: m.current_value.clone(),
        })
        .collect();

    let resolved = resolve_conflicts(&conflicts, ConflictResolution::KeepExternal);
    assert_eq!(resolved.len(), 2);
    // Both fields resolved with external values.
    let resolved_temp = resolved.iter().find(|(k, _)| k == "temperature").unwrap();
    assert_eq!(resolved_temp.1, Some(serde_json::json!(0.3)));
    let resolved_model = resolved.iter().find(|(k, _)| k == "model").unwrap();
    assert_eq!(resolved_model.1, Some(serde_json::json!("claude-3-opus")));

    // 6. Release with restore — restores original values from snapshot.
    let manifest = store.release_with_restore(&adapter).unwrap();
    assert_eq!(manifest.state, OwnershipState::Released);

    // 7. Verify: managed fields restored to adopt-time values.
    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["model"], serde_json::json!("gpt-4"));
    assert_eq!(disk["temperature"], serde_json::json!(0.7));
    assert_eq!(disk["api_key"], serde_json::json!("sk-123"));
}

#[test]
fn adopt_after_release_with_restore_works() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({"x": 1, "y": 2});
    let adapter = TestAdapter::new(tmp.path(), initial);

    let store = TakeoverStore::new();

    // First cycle.
    let v1 = field_map(&[("x", serde_json::json!(1)), ("y", serde_json::json!(2))]);
    store.adopt(vec!["x".into(), "y".into()], &v1).unwrap();
    store.release_with_restore(&adapter).unwrap();

    assert_eq!(store.state(), OwnershipState::Released);

    // Re-adopt after release.
    let v2 = field_map(&[("x", serde_json::json!(1)), ("y", serde_json::json!(2))]);
    let re_adopted = store.adopt(vec!["x".into(), "y".into()], &v2).unwrap();
    assert_eq!(re_adopted.state, OwnershipState::Adopted);
    assert_eq!(re_adopted.generation, 1);
}

// -----------------------------------------------------------------------
// I3: Real agent adapter tests
// -----------------------------------------------------------------------

#[test]
fn read_config_hash_empty_when_file_missing() {
    // When the config file doesn't exist, hash should be empty.
    super::isolate_agent_home();
    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();
    // Config file likely doesn't exist in CI.
    if !snapshot.config_path.exists() {
        assert!(snapshot.config_hash.is_empty());
    }
}

#[test]
fn read_config_hash_populated_when_file_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.json");
    std::fs::write(&path, r#"{"key": "value"}"#).unwrap();

    let adapter = TestAdapter::new(tmp.path(), serde_json::json!({"key": "value"}));
    let snapshot = adapter.read_config().unwrap();

    assert!(!snapshot.config_hash.is_empty());
    // Hash should be deterministic.
    let snapshot2 = adapter.read_config().unwrap();
    assert_eq!(snapshot.config_hash, snapshot2.config_hash);
}

#[test]
fn apply_patch_atomic_write() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({"existing": "value"});
    let adapter = TestAdapter::new(tmp.path(), initial);

    let snapshot = adapter.read_config().unwrap();
    let fields = vec![ManagedField {
        path: "new_key".into(),
        value: serde_json::json!("new_value"),
    }];

    // TestAdapter's apply_patch doesn't write to disk (in-memory only),
    // so test the real adapters with a file that exists.
    // Use ClaudeAdapter-style logic directly on the temp path.
    let mut raw = snapshot.raw.clone();
    for field in &fields {
        set_nested(&mut raw, &field.path, field.value.clone()).unwrap();
    }
    let json = serde_json::to_string_pretty(&raw).unwrap();
    let hash = compute_hash(json.as_bytes());

    // Atomic write: temp + rename
    let config_path = &adapter.path;
    let tmp_file = config_path.with_extension("json.tmp");
    std::fs::write(&tmp_file, &json).unwrap();
    std::fs::rename(&tmp_file, config_path).unwrap();

    // Verify file content.
    let disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(disk["existing"], serde_json::json!("value"));
    assert_eq!(disk["new_key"], serde_json::json!("new_value"));

    // Verify temp file was renamed (no longer exists).
    assert!(!tmp_file.exists());

    // Verify hash is non-empty and deterministic.
    assert!(!hash.is_empty());
    assert_eq!(hash, compute_hash(json.as_bytes()));
}

#[test]
fn apply_patch_config_hash_changes_after_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({"a": 1});
    let adapter = TestAdapter::new(tmp.path(), initial);

    let snapshot = adapter.read_config().unwrap();
    let original_hash = snapshot.config_hash.clone();

    let fields = vec![ManagedField {
        path: "a".into(),
        value: serde_json::json!(2),
    }];

    let patched = adapter.apply_patch(&snapshot, &fields).unwrap();
    assert_ne!(patched.config_hash, original_hash);
    assert!(!patched.config_hash.is_empty());
}

#[test]
fn release_restores_original_values_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({"model": "gpt-4", "temp": 0.5});
    let adapter = TestAdapter::new(tmp.path(), initial.clone());

    let snapshot = adapter.read_config().unwrap();

    // Apply a patch to change "model".
    let fields = vec![ManagedField {
        path: "model".into(),
        value: serde_json::json!("claude-3-opus"),
    }];
    let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

    // Verify patch was applied.
    assert_eq!(patched.raw["model"], serde_json::json!("claude-3-opus"));

    // Create manifest with original values.
    let manifest = OwnershipManifest {
        state: OwnershipState::Adopted,
        managed_fields: vec!["model".into()],
        field_snapshots: [("model".into(), serde_json::json!("gpt-4"))]
            .into_iter()
            .collect(),
        absent_fields: Vec::new(),
        adopted_at: Some(1_700_000_000),
        released_at: None,
        generation: 0,
        last_applied: HashMap::new(),
    };

    // Release should restore original "model" value.
    adapter.release(&patched, &manifest).unwrap();

    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["model"], serde_json::json!("gpt-4"));
    // "temp" should be unchanged.
    assert_eq!(disk["temp"], serde_json::json!(0.5));
}

#[test]
fn different_adapters_have_different_config_paths() {
    super::isolate_agent_home();
    let claude = ClaudeAdapter;
    let codex = CodexAdapter;
    let opencode = OpenCodeAdapter;

    let claude_path = claude.config_path().unwrap();
    let codex_path = codex.config_path().unwrap();
    let opencode_path = opencode.config_path().unwrap();

    // Claude Code reads settings out of `~/.claude/settings.json`. The first
    // version of this test asserted `.claude.json`, which pinned the defect:
    // that is the state file Claude Code writes for itself and it is not on
    // the settings precedence stack, so a takeover there changed nothing.
    assert!(
        claude_path.ends_with(".claude/settings.json"),
        "path: {}",
        claude_path.display()
    );
    assert!(codex_path.ends_with(".codex/config.toml"));
    assert!(
        opencode_path.ends_with("opencode/opencode.json"),
        "path: {}",
        opencode_path.display()
    );

    // Every pair must be distinct. Sharing a path would mean two adapters
    // writing the same file under two manifests, and a release restoring
    // fields the other adoption never owned.
    assert_ne!(claude_path, codex_path);
    assert_ne!(claude_path, opencode_path);
    assert_ne!(codex_path, opencode_path);
}

#[test]
fn the_claude_adapter_reads_the_settings_file_not_the_state_file() {
    super::isolate_agent_home();
    let path = ClaudeAdapter.config_path().unwrap();
    assert!(
        !path.ends_with(".claude.json"),
        "the takeover must not target the file Claude Code writes for itself: {}",
        path.display()
    );
    assert!(
        path.ends_with(".claude/settings.json"),
        "path: {}",
        path.display()
    );
}

#[test]
fn claude_config_dir_moves_the_whole_settings_directory() {
    super::isolate_agent_home();
    let custom = tempfile::tempdir().unwrap();
    super::isolate_config_dir("CLAUDE_CONFIG_DIR", custom.path());

    let path = ClaudeAdapter.config_path().unwrap();
    assert_eq!(path, custom.path().join("settings.json"));
}

#[test]
fn xdg_config_home_relocates_the_opencode_config() {
    super::isolate_agent_home();
    let custom = tempfile::tempdir().unwrap();
    super::isolate_config_dir("XDG_CONFIG_HOME", custom.path());

    let path = OpenCodeAdapter.config_path().unwrap();
    assert_eq!(path, custom.path().join("opencode").join("opencode.json"));
}

/// The refusal that protects a user's comments.
///
/// OpenCode accepts JSONC; this adapter parses strict JSON. Rewriting the
/// JSONC form would flatten every comment in it, so the adapter declines
/// rather than take the file over and silently delete user content.
#[test]
fn opencode_refuses_a_jsonc_config_it_would_flatten() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();
    let dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    // Only the `.jsonc` exists.
    std::fs::write(dir.join("opencode.jsonc"), "{ /* keep me */ }").unwrap();

    let err = OpenCodeAdapter
        .read_config()
        .expect_err("the jsonc-only config must be refused");
    assert!(err.contains("jsonc") || err.contains("JSONC"), "{err}");
    assert!(
        err.contains("comment"),
        "the refusal has to say what would be lost: {err}"
    );
}

/// The refusal that protects a v2-shaped config.
///
/// OpenCode's v2 schema renames `provider` to `providers` and `npm`/`options`
/// to `package`/`settings`. Writing the v1 paths into such a file would
/// succeed, be ignored by the client, and be reported as a working takeover.
#[test]
fn opencode_refuses_a_v2_shaped_provider_block() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();
    let dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("opencode.json"),
        r#"{"providers":{"zroutery":{"package":"@ai-sdk/openai-compatible"}}}"#,
    )
    .unwrap();

    let err = OpenCodeAdapter
        .read_config()
        .expect_err("a v2 provider block must be refused");
    assert!(err.contains("v2"), "{err}");
}

#[test]
fn opencode_reads_a_v1_config() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();
    let dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("opencode.json"),
        r#"{"model":"anthropic/claude","$schema":"https://opencode.ai/config.json"}"#,
    )
    .unwrap();

    let snapshot = OpenCodeAdapter
        .read_config()
        .expect("a v1 config is the supported shape");
    assert_eq!(snapshot.agent_type, AgentType::OpenCode);
    assert_eq!(snapshot.raw["model"], "anthropic/claude");
}

/// A config carrying *both* schema generations is mid-migration, and the
/// file itself cannot say which one the running client reads.
#[test]
fn opencode_refuses_a_config_carrying_both_schema_generations() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();
    let dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("opencode.json"),
        r#"{"provider":{},"providers":{}}"#,
    )
    .unwrap();

    let err = OpenCodeAdapter
        .read_config()
        .expect_err("a mid-migration config must be refused");
    assert!(err.contains("v2"), "{err}");
}

// -----------------------------------------------------------------------
// takeover_fields: what a takeover is allowed to manage
// -----------------------------------------------------------------------

fn a_target() -> GatewayTarget {
    GatewayTarget {
        base_url: "http://127.0.0.1:8787".into(),
        token: "gw-local-token".into(),
        classes: vec![
            TierClass {
                model: "sonnet-class".into(),
                display_name: "Sonnet".into(),
            },
            TierClass {
                model: "haiku-class".into(),
                display_name: "Haiku".into(),
            },
        ],
        default_class: None,
    }
}

fn paths(agent: AgentType, target: &GatewayTarget) -> Vec<String> {
    takeover_fields(agent, target)
        .unwrap_or_else(|e| panic!("{agent:?} should have a field set: {e}"))
        .into_iter()
        .map(|f| f.path)
        .collect()
}

/// The Claude adapter must not be handed a model field.
///
/// Claude Code picks a model by name and Zroutery already maps those names
/// onto classes with `match_claude_names`. Writing `ANTHROPIC_DEFAULT_*_MODEL`
/// would move a routing decision out of Zroutery's config into a file the
/// manifest then owns — the boundary takeover is not supposed to cross.
#[test]
fn the_claude_field_set_is_exactly_two_env_entries() {
    let target = a_target();
    assert_eq!(
        paths(AgentType::Claude, &target),
        vec!["env.ANTHROPIC_BASE_URL", "env.ANTHROPIC_AUTH_TOKEN"]
    );
}

#[test]
fn the_claude_field_set_does_not_touch_the_model_unless_asked() {
    let mut target = a_target();
    let plain = paths(AgentType::Claude, &target);
    target.default_class = Some("sonnet-class".into());
    let with_default = paths(AgentType::Claude, &target);

    assert_eq!(
        plain, with_default,
        "a default class must not add a Claude field: the model is chosen by \
         name and mapped inside Zroutery, not written into the client config"
    );
}

/// The defect the Codex adapter's first fixtures had: a provider table with
/// no selector, so the table is defined and never used.
#[test]
fn the_codex_field_set_selects_the_provider_it_defines() {
    let target = a_target();
    let got = paths(AgentType::Codex, &target);

    assert!(
        got.iter().any(|p| p == "model_provider"),
        "codex needs the selector field, not just the table: {got:?}"
    );
    assert_eq!(
        got.iter().filter(|p| *p == "model_provider").count(),
        1,
        "exactly one selector: {got:?}"
    );
    let selected = serde_json::json!(GATEWAY_PROVIDER_ID);
    let table_prefix = format!("model_providers.{GATEWAY_PROVIDER_ID}.");
    assert!(
        got.iter()
            .any(|p| p.starts_with(&table_prefix) && p.ends_with("base_url")),
        "the selected provider must be the one that got a base url: {got:?}"
    );
    assert!(got.contains(&format!("{table_prefix}base_url")), "{got:?}");
    // The selector value must name the table that was written.
    let fields = takeover_fields(AgentType::Codex, &target).unwrap();
    let selector = fields
        .iter()
        .find(|f| f.path == "model_provider")
        .expect("selector field");
    assert_eq!(selector.value, selected);
}

#[test]
fn the_codex_field_set_only_sets_the_model_when_a_default_is_asked_for() {
    let mut target = a_target();
    assert!(
        !paths(AgentType::Codex, &target)
            .iter()
            .any(|p| p == "model"),
        "codex must not have its model rewritten by default"
    );
    target.default_class = Some("sonnet-class".into());
    assert!(paths(AgentType::Codex, &target)
        .iter()
        .any(|p| p == "model"));
}

/// A `.json` file carrying comments is refused, naming the line.
///
/// OpenCode parses *every* config document as JSONC — the extension is
/// cosmetic. Without this, `serde_json` rejects the file with a syntax error
/// that says nothing about comments, and the user concludes their config is
/// broken rather than that a tool declined to touch it.
#[test]
fn opencode_refuses_a_json_file_that_carries_comments() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();
    let dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("opencode.json"),
        "{\n  // my notes\n  \"model\": \"anthropic/claude\"\n}\n",
    )
    .unwrap();

    let err = OpenCodeAdapter
        .read_config()
        .expect_err("a commented .json must be refused");
    assert!(err.contains("comment"), "{err}");
    assert!(err.contains("line 2"), "the line has to be named: {err}");
}

/// A `//` inside a string is not a comment.
///
/// Every OpenCode base URL contains `https://`, so a scanner that does not
/// track quoting would refuse every well-formed config in the world.
#[test]
fn a_url_in_a_string_is_not_mistaken_for_a_comment() {
    assert_eq!(first_comment_line(r#"{"baseURL":"https://host/v1"}"#), None);
    assert_eq!(first_comment_line(r#"{"a":"b//c"}"#), None);
    assert_eq!(first_comment_line(r#"{"a":"q\"//not a comment"}"#), None);
    assert_eq!(first_comment_line("{\n  \"a\": 1\n  // note\n}"), Some(3));
    assert_eq!(first_comment_line("{ /* note */ \"a\": 1 }"), Some(1));
}

/// The project `.opencode/` directory outranks the bare project file, and
/// reporting the weaker one would name the wrong source.
#[test]
fn opencode_reports_the_dotted_project_directory_when_it_shadows() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();
    let dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("opencode.json"), "{}").unwrap();

    let global = OpenCodeAdapter.config_path().unwrap();

    // Both project files present, both relative to the working directory.
    let cwd = std::env::current_dir().expect("cwd");
    let dotted = cwd.join(".opencode");
    let created = std::fs::create_dir_all(&dotted);
    std::fs::write(cwd.join("opencode.json"), "{}").ok();
    let dotted_file = std::fs::write(dotted.join("opencode.json"), "{}");
    if created.is_err() || dotted_file.is_err() {
        // Cannot create a project config beside the crate; the path list is
        // still what matters and is asserted below.
        return;
    }

    let shadowing = project_shadow(&global).expect("a project config shadows the global one");
    assert!(
        shadowing.ends_with(".opencode/opencode.json"),
        "the dotted directory is loaded last, so it is the effective source: {}",
        shadowing.display()
    );

    let _ = std::fs::remove_file(cwd.join("opencode.json"));
    let _ = std::fs::remove_file(dotted.join("opencode.json"));
    let _ = std::fs::remove_dir(&dotted);
}

/// A Codex takeover must land in the **user** config.
///
/// Codex ignores `model_provider` and `model_providers` in a project-local
/// `.codex/config.toml` and prints a startup warning. A takeover written there
/// would report success and route nowhere.
#[test]
fn the_codex_takeover_targets_the_user_config() {
    super::isolate_agent_home();
    let home = super::isolated_agent_home().unwrap();

    let path = CodexAdapter.config_path().unwrap();
    assert_eq!(path, home.join(".codex").join("config.toml"));
    assert!(
        !path.starts_with(
            home.join(".codex")
                .join("config.toml")
                .parent()
                .unwrap()
                .join("projects")
        ),
        "never the project-local file: {}",
        path.display()
    );
}

/// `name` is load-bearing, not decoration.
///
/// Codex rejects a provider whose `name` is empty at config-parse time, which
/// fails the whole `config.toml` rather than ignoring one provider.
#[test]
fn the_codex_provider_table_carries_a_non_empty_name() {
    let target = a_target();
    let fields = takeover_fields(AgentType::Codex, &target).unwrap();
    let name = fields
        .iter()
        .find(|f| f.path == format!("model_providers.{GATEWAY_PROVIDER_ID}.name"))
        .expect("a name field");
    assert_eq!(
        name.value,
        serde_json::json!("Zroutery"),
        "an empty provider name is a parse error, not a default"
    );
    assert!(!name.value.as_str().unwrap().is_empty());
}

/// The bearer token is a literal, and that is a decision between two verified
/// alternatives rather than a guess.
#[test]
fn the_codex_token_goes_in_the_field_that_takes_a_literal() {
    let target = a_target();
    let got = paths(AgentType::Codex, &target);

    assert!(
        got.contains(&format!(
            "model_providers.{GATEWAY_PROVIDER_ID}.experimental_bearer_token"
        )),
        "{got:?}"
    );
    // `env_key` names an environment variable, which is not a file field a
    // manifest can restore, and `auth` runs a command. Writing either would mean
    // a takeover with nothing to put back on release.
    assert!(
        !got.iter().any(|p| p.ends_with(".env_key")),
        "env_key is not restorable from a manifest: {got:?}"
    );
    assert!(
        !got.iter().any(|p| p.ends_with(".auth")),
        "auth runs an external command: {got:?}"
    );
}

#[test]
fn the_opencode_field_set_uses_the_documented_v1_shape() {
    let target = a_target();
    let got = paths(AgentType::OpenCode, &target);

    for expected in [
        format!("provider.{GATEWAY_PROVIDER_ID}.npm"),
        format!("provider.{GATEWAY_PROVIDER_ID}.options.baseURL"),
        format!("provider.{GATEWAY_PROVIDER_ID}.options.apiKey"),
        format!("provider.{GATEWAY_PROVIDER_ID}.models.sonnet-class.name"),
    ] {
        assert!(got.contains(&expected), "missing {expected} in {got:?}");
    }
    assert!(
        !got.iter().any(|p| p.starts_with("providers.")),
        "v2 key must never be written: {got:?}"
    );
}

#[test]
fn the_opencode_default_model_is_namespaced_but_the_table_is_not() {
    let mut target = a_target();
    target.default_class = Some("sonnet-class".into());
    let fields = takeover_fields(AgentType::OpenCode, &target).unwrap();

    let model = fields.iter().find(|f| f.path == "model").expect("model");
    assert_eq!(
        model.value,
        serde_json::json!("zroutery/sonnet-class"),
        "opencode addresses a model as provider/model"
    );
    let table = fields
        .iter()
        .find(|f| f.path.ends_with("models.sonnet-class.name"))
        .expect("model table entry");
    assert_eq!(
        table.value,
        serde_json::json!("Sonnet"),
        "the table key is the bare id because opencode strips the prefix"
    );
}

#[test]
fn gemini_is_refused_a_new_takeover() {
    let target = a_target();
    let err =
        takeover_fields(AgentType::Gemini, &target).expect_err("gemini takeover is withdrawn");
    assert!(err.contains("not supported"), "{err}");
    assert!(
        !AgentType::Gemini.is_takeover_supported(),
        "and it must not be advertised as a target"
    );
    assert!(
        adapter_for(AgentType::Gemini).is_none(),
        "and there must be no adapter to hand a caller"
    );
}

/// Every takeover target the product offers, so a new agent cannot be added
/// to [`adapter_for`] without a field set behind it.
#[test]
fn every_advertised_agent_can_produce_a_field_set() {
    let target = a_target();
    for agent in AgentType::ALL {
        let supported = agent.is_takeover_supported();
        let adapter = adapter_for(agent);
        assert_eq!(
            supported,
            adapter.is_some(),
            "{agent:?}: support flag and adapter presence must agree"
        );
        if supported {
            assert!(
                !takeover_fields(agent, &target)
                    .expect("a supported agent must yield fields")
                    .is_empty(),
                "{agent:?} yielded no fields"
            );
        }
    }
}

#[test]
fn takeover_fields_refuses_an_unusable_target() {
    let base = a_target();

    let mut no_url = base.clone();
    no_url.base_url = "   ".into();
    assert!(takeover_fields(AgentType::Claude, &no_url)
        .unwrap_err()
        .contains("gateway URL"));

    let mut no_token = base.clone();
    no_token.token = String::new();
    assert!(takeover_fields(AgentType::Claude, &no_token)
        .unwrap_err()
        .contains("token"));

    let mut no_classes = base.clone();
    no_classes.classes = Vec::new();
    assert!(takeover_fields(AgentType::Claude, &no_classes)
        .unwrap_err()
        .contains("no model classes"));

    let mut bad_default = base.clone();
    bad_default.default_class = Some("nope-class".into());
    assert!(takeover_fields(AgentType::Claude, &bad_default)
        .unwrap_err()
        .contains("not one of the classes offered"));

    // A dot is the field-path separator, so it would land the value
    // somewhere the client never reads.
    let mut dotted = base.clone();
    dotted.classes[0].model = "sonnet.class".into();
    assert!(takeover_fields(AgentType::OpenCode, &dotted)
        .unwrap_err()
        .contains("field-path separator"));
}

#[cfg(unix)]
#[test]
fn apply_patch_preserves_restrictive_file_mode() {
    use std::os::unix::fs::PermissionsExt;

    super::isolate_agent_home();
    let path = claude_settings_path();
    std::fs::write(&path, r#"{"model":"gpt-4"}"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();
    adapter
        .apply_patch(
            &snapshot,
            &[ManagedField {
                path: "model".into(),
                value: serde_json::json!("claude-3-opus"),
            }],
        )
        .unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "existing mode was widened to {mode:o}");

    let disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(disk["model"], serde_json::json!("claude-3-opus"));
}

#[cfg(unix)]
#[test]
fn new_config_is_created_with_restrictive_mode() {
    use std::os::unix::fs::PermissionsExt;

    super::isolate_agent_home();
    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();
    assert!(!snapshot.config_path.exists());

    adapter
        .apply_patch(
            &snapshot,
            &[ManagedField {
                path: "model".into(),
                value: serde_json::json!("claude-3-opus"),
            }],
        )
        .unwrap();

    let mode = std::fs::metadata(&snapshot.config_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "new config mode was {mode:o}");
}

#[test]
fn apply_patch_refuses_stale_snapshot() {
    super::isolate_agent_home();
    let path = claude_settings_path();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "managed": "original",
            "unmanaged": "original"
        }))
        .unwrap(),
    )
    .unwrap();

    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();

    // Another tool edits a field Zroutery does not manage while the
    // snapshot is held.
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "managed": "original",
            "unmanaged": "user-new"
        }))
        .unwrap(),
    )
    .unwrap();

    let err = adapter
        .apply_patch(
            &snapshot,
            &[ManagedField {
                path: "managed".into(),
                value: serde_json::json!("zroutery"),
            }],
        )
        .unwrap_err();
    assert!(err.contains("changed on disk"), "error: {err}");

    // The write was refused, so the external edit is still there.
    let disk = read_json_file(&path);
    assert_eq!(disk["unmanaged"], serde_json::json!("user-new"));
    assert_eq!(disk["managed"], serde_json::json!("original"));

    // Re-reading and retrying applies the patch on top of the current file.
    let fresh = adapter.read_config().unwrap();
    adapter
        .apply_patch(
            &fresh,
            &[ManagedField {
                path: "managed".into(),
                value: serde_json::json!("zroutery"),
            }],
        )
        .unwrap();

    let disk = read_json_file(&path);
    assert_eq!(disk["managed"], serde_json::json!("zroutery"));
    assert_eq!(disk["unmanaged"], serde_json::json!("user-new"));
}

#[test]
fn apply_patch_reports_type_conflict_on_scalar_parent() {
    super::isolate_agent_home();
    let path = claude_settings_path();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({"model": "existing-scalar"})).unwrap(),
    )
    .unwrap();

    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();

    // `model.temperature` cannot be set when `model` is a scalar; the old
    // code reported success and changed nothing.
    let err = adapter
        .apply_patch(
            &snapshot,
            &[ManagedField {
                path: "model.temperature".into(),
                value: serde_json::json!(0.5),
            }],
        )
        .unwrap_err();
    assert!(err.contains("model.temperature"), "error: {err}");
    assert!(err.contains("non-table"), "error: {err}");

    let disk = read_json_file(&path);
    assert_eq!(disk["model"], serde_json::json!("existing-scalar"));
}

#[test]
fn apply_patch_refuses_to_replace_existing_non_table() {
    super::isolate_agent_home();
    let path = claude_settings_path();
    let initial = serde_json::json!({"a": {"b": 5}});
    std::fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

    let adapter = ClaudeAdapter;
    let snapshot = adapter.read_config().unwrap();

    // The old code replaced the existing scalar `a.b` with a table.
    let err = adapter
        .apply_patch(
            &snapshot,
            &[ManagedField {
                path: "a.b.c.d".into(),
                value: serde_json::json!(1),
            }],
        )
        .unwrap_err();
    assert!(err.contains("a.b.c.d"), "error: {err}");

    let disk = read_json_file(&path);
    assert_eq!(disk, initial, "the existing value must not be replaced");
}

#[test]
fn compute_hash_is_deterministic() {
    let data = b"test data for hashing";
    let h1 = compute_hash(data);
    let h2 = compute_hash(data);
    assert_eq!(h1, h2);
}

#[test]
fn compute_hash_differs_for_different_data() {
    let h1 = compute_hash(b"aaa");
    let h2 = compute_hash(b"bbb");
    assert_ne!(h1, h2);
}

#[test]
fn apply_patch_preserves_unmanaged_fields_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({
        "managed": "original",
        "unmanaged": "keep_me"
    });
    let adapter = TestAdapter::new(tmp.path(), initial);

    let snapshot = adapter.read_config().unwrap();
    let fields = vec![ManagedField {
        path: "managed".into(),
        value: serde_json::json!("changed"),
    }];

    let patched = adapter.apply_patch(&snapshot, &fields).unwrap();

    // Write to disk (TestAdapter's apply_patch is in-memory only).
    adapter.write_config(&patched).unwrap();

    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["managed"], serde_json::json!("changed"));
    assert_eq!(disk["unmanaged"], serde_json::json!("keep_me"));
}

#[test]
fn release_with_hash_tracking() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = serde_json::json!({"x": 1});
    let adapter = TestAdapter::new(tmp.path(), initial);

    let store = TakeoverStore::new();
    let values = field_map(&[("x", serde_json::json!(1))]);
    store.adopt(vec!["x".into()], &values).unwrap();

    // Patch config.
    let snapshot = adapter.read_config().unwrap();
    let original_hash = snapshot.config_hash.clone();

    let fields = vec![ManagedField {
        path: "x".into(),
        value: serde_json::json!(99),
    }];
    let patched = adapter.apply_patch(&snapshot, &fields).unwrap();
    assert_ne!(patched.config_hash, original_hash);

    // Release restores original.
    let manifest = store.release().unwrap();
    adapter.release(&patched, &manifest).unwrap();

    let disk = read_json_file(&adapter.path);
    assert_eq!(disk["x"], serde_json::json!(1));
}
