//! Gate fixtures for agent takeover ownership: durable manifests, CAS restore,
//! conflict application and the confirmation flow.
//!
//! # What these exist to prevent
//!
//! Three of the four `I3` gates describe ways for ownership to be *claimed* and
//! then lost or misused, and none of them is visible from inside one process:
//!
//! * A manifest that lives only in memory is a manifest nobody can recover. If
//!   Zroutery crashes holding ownership, the next process does not know which
//!   fields it managed or what they used to hold — so the agent config keeps
//!   pointing at a proxy that is no longer there, and there is nothing to restore
//!   from. [`durable_manifest_survives_a_restart`] is the whole point of gate one.
//!
//! * The in-process generation check cannot see a *second process*. Two Zroutery
//!   instances over one manifest is the same race the single-process check exists
//!   for, one level up, and it needs the durable record to be observable at all.
//!   [`release_refuses_when_another_process_moved_the_manifest`] is gate four.
//!
//! * [`resolve_conflicts`] computed a resolution that no production path ever
//!   applied, so a release discarded whatever the user changed while Zroutery
//!   held the fields. The test named `user_modification_preserved_on_release`
//!   appeared to cover this and did not: it called the no-restore `release`,
//!   which writes nothing, so "preserved" was true because nothing happened.

use std::collections::HashMap;
use std::path::PathBuf;

use zroutery_core::agent_takeover::{
    AgentAdapter, AgentConfigSnapshot, AgentType, ConflictResolution, TakeoverStore,
};

fn values(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// A per-test directory, removed on drop.
///
/// Every fixture gets its own directory rather than sharing `temp_dir()` with a
/// fixed name: these tests run in parallel threads inside one binary, and the
/// two-process CAS fixture in particular depends on two stores not colliding on
/// one manifest path.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new(label: &str) -> Self {
        let unique = format!(
            "zroutery_takeover_{}_{}_{:?}",
            std::process::id(),
            label,
            std::thread::current().id()
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create sandbox");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Agent adapter over a real file, so restore and read genuinely round-trip
/// through the filesystem rather than through a struct field.
struct FileAdapter {
    path: PathBuf,
}

impl FileAdapter {
    fn new(sandbox: &Sandbox, name: &str, initial: serde_json::Value) -> Self {
        let path = sandbox.join(name);
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&initial).expect("serialize"),
        )
        .expect("write config");
        Self { path }
    }

    fn read_raw(&self) -> serde_json::Value {
        let data = std::fs::read_to_string(&self.path).expect("config exists");
        serde_json::from_str(&data).expect("config parses")
    }

    fn write_raw(&self, value: &serde_json::Value) {
        std::fs::write(
            &self.path,
            serde_json::to_string_pretty(value).expect("serialize"),
        )
        .expect("write config");
    }
}

impl AgentAdapter for FileAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Claude
    }

    fn config_path(&self) -> Result<PathBuf, String> {
        Ok(self.path.clone())
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let data = std::fs::read_to_string(&self.path).map_err(|e| format!("read failed: {e}"))?;
        let raw: serde_json::Value =
            serde_json::from_str(&data).map_err(|e| format!("parse failed: {e}"))?;
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Claude,
            config_path: self.path.clone(),
            config_hash: String::new(),
            raw,
        })
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[zroutery_core::agent_takeover::ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        let mut raw = snapshot.raw.clone();
        for field in fields {
            set_at(&mut raw, &field.path, field.value.clone());
        }
        Ok(AgentConfigSnapshot {
            raw,
            ..snapshot.clone()
        })
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &zroutery_core::agent_takeover::OwnershipManifest,
    ) -> Result<(), String> {
        let mut raw = snapshot.raw.clone();
        for (path, value) in &manifest.field_snapshots {
            set_at(&mut raw, path, value.clone());
        }
        for path in &manifest.absent_fields {
            remove_at(&mut raw, path);
        }
        self.write_raw(&raw);
        Ok(())
    }
}

fn set_at(root: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let segments: Vec<&str> = path.split('.').collect();
    let mut cursor = root;
    for segment in &segments[..segments.len() - 1] {
        if !cursor.is_object() {
            *cursor = serde_json::Value::Object(serde_json::Map::new());
        }
        cursor = cursor
            .as_object_mut()
            .expect("object")
            .entry((*segment).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    let last = segments[segments.len() - 1];
    if !cursor.is_object() {
        *cursor = serde_json::Value::Object(serde_json::Map::new());
    }
    cursor
        .as_object_mut()
        .expect("object")
        .insert(last.to_string(), value);
}

fn remove_at(root: &mut serde_json::Value, path: &str) {
    let segments: Vec<&str> = path.split('.').collect();
    let mut cursor = root;
    for segment in &segments[..segments.len() - 1] {
        match cursor.as_object_mut().and_then(|m| m.get_mut(*segment)) {
            Some(next) => cursor = next,
            None => return,
        }
    }
    if let Some(map) = cursor.as_object_mut() {
        map.remove(segments[segments.len() - 1]);
    }
}

fn nested(pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    serde_json::Value::Object(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Gate: durable ownership manifest
// ---------------------------------------------------------------------------

#[test]
fn durable_manifest_survives_a_restart() {
    let sandbox = Sandbox::new("restart");
    let manifest_path = sandbox.join("ownership.json");
    let original = values(&[
        ("model", serde_json::json!("opus")),
        ("base_url", serde_json::json!("https://api.example")),
    ]);

    {
        let store = TakeoverStore::open(&manifest_path).expect("open durable store");
        assert!(store.is_durable());
        store
            .adopt(vec!["model".into(), "base_url".into()], &original)
            .expect("adopt");
    } // the process-local store is gone here, as it would be on exit

    let reopened = TakeoverStore::open(&manifest_path).expect("reopen");
    assert_eq!(
        reopened.state(),
        zroutery_core::agent_takeover::OwnershipState::Adopted,
        "ownership held at crash time must still read as held, or the restore path is lost"
    );

    let manifest = reopened.manifest().expect("manifest recovered");
    assert_eq!(manifest.field_snapshots["model"], serde_json::json!("opus"));
    assert_eq!(
        manifest.field_snapshots["base_url"],
        serde_json::json!("https://api.example"),
        "the pre-takeover values are the only thing that makes release possible"
    );
    assert_eq!(manifest.managed_fields, vec!["model", "base_url"]);
    assert!(reopened.check_orphaned_state());
}

#[test]
fn an_in_memory_store_persists_nothing_and_says_so() {
    // The default constructor is not a lesser `open`: it is correct for a caller
    // that never adopts. This pins that difference so nobody "simplifies" a
    // durable call site into `new()`.
    let sandbox = Sandbox::new("in_memory");
    let store = TakeoverStore::new();
    assert!(!store.is_durable());
    store
        .adopt(
            vec!["model".into()],
            &values(&[("model", serde_json::json!("opus"))]),
        )
        .expect("adopt");

    let written = std::fs::read_dir(&sandbox.0)
        .expect("sandbox exists")
        .count();
    assert_eq!(written, 0, "an in-memory store must not create files");
}

#[test]
fn a_released_manifest_reopens_as_released_and_allows_re_adoption() {
    let sandbox = Sandbox::new("released");
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        &sandbox,
        "config.json",
        nested(&[("model", serde_json::json!("opus"))]),
    );

    let store = TakeoverStore::open(&manifest_path).expect("open");
    store
        .adopt(
            vec!["model".into()],
            &values(&[("model", serde_json::json!("opus"))]),
        )
        .expect("adopt");
    store.release_with_restore(&adapter).expect("release");

    let reopened = TakeoverStore::open(&manifest_path).expect("reopen");
    assert_eq!(
        reopened.state(),
        zroutery_core::agent_takeover::OwnershipState::Released
    );
    reopened
        .adopt(
            vec!["model".into()],
            &values(&[("model", serde_json::json!("opus"))]),
        )
        .expect("a released store must permit a fresh adoption without a reset");
}

#[test]
fn a_corrupt_manifest_is_refused_rather_than_read_as_empty() {
    // Starting from nothing when the record is unreadable would overwrite the
    // only evidence of what was overwritten.
    let sandbox = Sandbox::new("corrupt");
    let manifest_path = sandbox.join("ownership.json");
    std::fs::write(&manifest_path, b"{ this is not json").expect("write corrupt");

    // `TakeoverStore` is not `Debug`, so `expect_err` is unavailable.
    let err = match TakeoverStore::open(&manifest_path) {
        Ok(_) => panic!("a corrupt manifest must not open"),
        Err(err) => err,
    };
    assert!(
        err.contains("unreadable"),
        "the refusal must say why, got: {err}"
    );
}

#[test]
fn a_missing_manifest_opens_clean() {
    let sandbox = Sandbox::new("missing");
    let store = TakeoverStore::open(sandbox.join("ownership.json")).expect("open");
    assert_eq!(
        store.state(),
        zroutery_core::agent_takeover::OwnershipState::Verified
    );
    assert!(store.manifest().is_none());
}

// ---------------------------------------------------------------------------
// Gate: CAS restore fixtures
// ---------------------------------------------------------------------------

#[test]
fn release_refuses_when_another_process_moved_the_manifest() {
    // The single-process generation check cannot see this. Two stores over one
    // durable manifest is the same race one level up, and it is the case that
    // matters: the second process adopted, so the first restoring adoption-time
    // values would clobber whatever the second one is now doing.
    let sandbox = Sandbox::new("cas");
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        &sandbox,
        "config.json",
        nested(&[("model", serde_json::json!("opus"))]),
    );
    let fields = vec!["model".to_string()];
    let baseline = values(&[("model", serde_json::json!("opus"))]);

    let first = TakeoverStore::open(&manifest_path).expect("open first");
    first
        .adopt(fields.clone(), &baseline)
        .expect("first adopts");

    // A second process finds the same ownership, recovers it, releases it, and
    // adopts again — moving the generation on disk while `first` still believes
    // it holds generation 0. This is the realistic shape of the race: not a
    // second store appearing from nowhere, but an orphan being recovered by
    // another instance.
    let second = TakeoverStore::open(&manifest_path).expect("open second");
    assert_eq!(
        second.state(),
        zroutery_core::agent_takeover::OwnershipState::Adopted,
        "the second process must see ownership as held, or there is nothing to race"
    );
    second
        .release_with_restore(&adapter)
        .expect("second releases the orphan");
    second.adopt(fields, &baseline).expect("second re-adopts");

    let err = first
        .release_with_restore(&adapter)
        .expect_err("the stale holder must not commit");
    assert!(
        err.contains("moved while the config") || err.contains("changed while restoring"),
        "the refusal must name the cause, got: {err}"
    );

    assert_eq!(
        adapter.read_raw()["model"],
        serde_json::json!("opus"),
        "a refused release must not have written the config either"
    );
    assert_eq!(
        first.state(),
        zroutery_core::agent_takeover::OwnershipState::Adopted,
        "the refused claim must be rolled back, not left in flight"
    );
}

#[test]
fn release_refuses_when_the_manifest_disappeared() {
    let sandbox = Sandbox::new("cas_vanished");
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        &sandbox,
        "config.json",
        nested(&[("model", serde_json::json!("opus"))]),
    );

    let store = TakeoverStore::open(&manifest_path).expect("open");
    store
        .adopt(
            vec!["model".into()],
            &values(&[("model", serde_json::json!("opus"))]),
        )
        .expect("adopt");

    std::fs::remove_file(&manifest_path).expect("remove manifest");
    let err = store
        .release_with_restore(&adapter)
        .expect_err("a vanished record must stop the release");
    assert!(err.contains("disappeared"), "got: {err}");
}

#[test]
fn a_durable_release_commits_when_nothing_competes() {
    // The other direction, and the one that would fail if the CAS check were
    // simply always refusing.
    let sandbox = Sandbox::new("cas_clean");
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        &sandbox,
        "config.json",
        nested(&[
            ("model", serde_json::json!("opus")),
            ("base_url", serde_json::json!("http://127.0.0.1:8080")),
        ]),
    );

    let store = TakeoverStore::open(&manifest_path).expect("open");
    store
        .adopt(
            vec!["model".into(), "base_url".into()],
            &values(&[
                ("model", serde_json::json!("opus")),
                ("base_url", serde_json::json!("http://127.0.0.1:8080")),
            ]),
        )
        .expect("adopt");

    // Simulate the proxy having rewritten the field.
    adapter.write_raw(&nested(&[
        ("model", serde_json::json!("haiku")),
        ("base_url", serde_json::json!("http://127.0.0.1:8080")),
    ]));

    store.release_with_restore(&adapter).expect("clean release");
    assert_eq!(
        adapter.read_raw()["model"],
        serde_json::json!("opus"),
        "the adoption-time value must be back on disk"
    );

    let reopened = TakeoverStore::open(&manifest_path).expect("reopen");
    assert_eq!(
        reopened.state(),
        zroutery_core::agent_takeover::OwnershipState::Released
    );
}

// ---------------------------------------------------------------------------
// Gate: conflict detection and application
// ---------------------------------------------------------------------------

/// Adopt, let the "proxy" rewrite the managed fields *and record that it did*,
/// then have the user edit one of them — the situation `release_resolved` exists
/// for.
///
/// Recording the proxy's own write is not incidental. Without it the store
/// cannot distinguish a user edit from its own write, and reports every field the
/// proxy touched as a conflict, which is how this fixture found that
/// `last_applied` was never being updated.
fn adopted_then_edited(
    sandbox: &Sandbox,
    resolution: ConflictResolution,
) -> (FileAdapter, zroutery_core::agent_takeover::ReleaseOutcome) {
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        sandbox,
        "config.json",
        nested(&[
            ("model", serde_json::json!("opus")),
            ("temperature", serde_json::json!(1.0)),
        ]),
    );

    let store = TakeoverStore::open(&manifest_path).expect("open");
    store
        .adopt(
            vec!["model".into(), "temperature".into()],
            &values(&[
                ("model", serde_json::json!("opus")),
                ("temperature", serde_json::json!(1.0)),
            ]),
        )
        .expect("adopt");

    // Zroutery rewrites the managed fields while holding ownership, and records
    // the values it wrote.
    let managed = values(&[
        ("model", serde_json::json!("haiku")),
        ("temperature", serde_json::json!(0.2)),
    ]);
    adapter.write_raw(&nested(&[
        ("model", serde_json::json!("haiku")),
        ("temperature", serde_json::json!(0.2)),
    ]));
    store.record_applied(&managed).expect("record applied");

    // Then the user edits one of them behind Zroutery's back, and that edit is
    // deliberately *not* recorded.
    adapter.write_raw(&nested(&[
        ("model", serde_json::json!("sonnet")),
        ("temperature", serde_json::json!(0.2)),
    ]));

    let outcome = store
        .release_resolved(&adapter, resolution)
        .expect("resolved release");
    (adapter, outcome)
}

#[test]
fn keep_external_preserves_the_user_edit() {
    let sandbox = Sandbox::new("keep");
    let (adapter, outcome) = adopted_then_edited(&sandbox, ConflictResolution::KeepExternal);

    assert_eq!(
        adapter.read_raw()["model"],
        serde_json::json!("sonnet"),
        "the user's value must survive the release"
    );
    assert_eq!(outcome.report.kept_external, vec!["model".to_string()]);
    assert_eq!(outcome.report.total(), 1);
    assert!(
        !outcome.conflicts.is_empty(),
        "the conflict must be reported, not just resolved silently"
    );
}

#[test]
fn overwrite_with_managed_puts_zrouterys_value_back() {
    let sandbox = Sandbox::new("overwrite");
    let (adapter, outcome) =
        adopted_then_edited(&sandbox, ConflictResolution::OverwriteWithManaged);

    assert_eq!(
        adapter.read_raw()["model"],
        serde_json::json!("haiku"),
        "OverwriteWithManaged means the managed value lands, not the adoption-time one"
    );
    assert_eq!(outcome.report.overwritten, vec!["model".to_string()]);
}

#[test]
fn skip_leaves_the_field_untouched() {
    let sandbox = Sandbox::new("skip");
    let (adapter, outcome) = adopted_then_edited(&sandbox, ConflictResolution::Skip);

    assert_eq!(
        adapter.read_raw()["model"],
        serde_json::json!("sonnet"),
        "a skipped field must not be written at all"
    );
    assert_eq!(outcome.report.skipped, vec!["model".to_string()]);
}

#[test]
fn the_unconditional_restore_path_is_the_defect_this_replaces() {
    // `release_with_restore` is still the path that discards the user's edit.
    // That is a known, separate decision, and this test states it rather than
    // leaving it as folklore: if that path is ever changed to respect conflicts
    // without the resolution machinery, this fails and the decision gets made
    // deliberately.
    let sandbox = Sandbox::new("unconditional");
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        &sandbox,
        "config.json",
        nested(&[("model", serde_json::json!("opus"))]),
    );

    let store = TakeoverStore::open(&manifest_path).expect("open");
    store
        .adopt(
            vec!["model".into()],
            &values(&[("model", serde_json::json!("opus"))]),
        )
        .expect("adopt");
    adapter.write_raw(&nested(&[("model", serde_json::json!("sonnet"))]));

    store.release_with_restore(&adapter).expect("restore");
    assert_eq!(
        adapter.read_raw()["model"],
        serde_json::json!("opus"),
        "release_with_restore restores the snapshot unconditionally; use release_resolved \
         when the user's edits matter"
    );
}

#[test]
fn a_field_the_user_deleted_is_not_recreated_under_keep_external() {
    // Restoring a deleted field is the sharp edge of an unconditional snapshot
    // restore: it brings back something the user removed on purpose.
    let sandbox = Sandbox::new("deleted");
    let manifest_path = sandbox.join("ownership.json");
    let adapter = FileAdapter::new(
        &sandbox,
        "config.json",
        nested(&[
            ("model", serde_json::json!("opus")),
            ("base_url", serde_json::json!("https://x")),
        ]),
    );

    let store = TakeoverStore::open(&manifest_path).expect("open");
    store
        .adopt(
            vec!["model".into(), "base_url".into()],
            &values(&[
                ("model", serde_json::json!("opus")),
                ("base_url", serde_json::json!("https://x")),
            ]),
        )
        .expect("adopt");

    // The user deletes base_url entirely.
    adapter.write_raw(&nested(&[("model", serde_json::json!("opus"))]));

    let outcome = store
        .release_resolved(&adapter, ConflictResolution::KeepExternal)
        .expect("resolved release");

    let raw = adapter.read_raw();
    assert_eq!(raw["model"], serde_json::json!("opus"));
    assert!(
        raw.get("base_url").is_none(),
        "a field the user deleted must stay deleted, got: {raw}"
    );
    assert_eq!(outcome.report.kept_external.len(), 1);
}

// ---------------------------------------------------------------------------
// Gate: confirmation flow
// ---------------------------------------------------------------------------

#[test]
fn a_proposal_can_be_confirmed_against_unchanged_config() {
    let sandbox = Sandbox::new("confirm");
    let store = TakeoverStore::open(sandbox.join("ownership.json")).expect("open");
    let current = values(&[("model", serde_json::json!("opus"))]);
    let managed = values(&[("model", serde_json::json!("haiku"))]);

    let proposal = store
        .plan_adopt(&["model".to_string()], &current, &managed)
        .expect("plan");

    assert_eq!(proposal.changes.len(), 1);
    assert_eq!(proposal.changes[0].current, Some(serde_json::json!("opus")));
    assert_eq!(
        proposal.changes[0].managed,
        Some(serde_json::json!("haiku"))
    );
    assert!(
        proposal.summary().contains("take over 1 field"),
        "got: {}",
        proposal.summary()
    );

    let manifest = store.confirm_adopt(&proposal, &current).expect("confirm");
    assert_eq!(
        manifest.state,
        zroutery_core::agent_takeover::OwnershipState::Adopted
    );
    assert_eq!(manifest.field_snapshots["model"], serde_json::json!("opus"));
}

#[test]
fn confirmation_refuses_when_the_config_moved_underneath_the_plan() {
    // The property the flow exists for: what was confirmed must be what happens.
    let sandbox = Sandbox::new("confirm_stale");
    let store = TakeoverStore::open(sandbox.join("ownership.json")).expect("open");
    let current = values(&[("model", serde_json::json!("opus"))]);
    let managed = values(&[("model", serde_json::json!("haiku"))]);

    let proposal = store
        .plan_adopt(&["model".to_string()], &current, &managed)
        .expect("plan");

    // The user edits the agent config while the dialog is open.
    let moved = values(&[("model", serde_json::json!("sonnet"))]);

    let err = store
        .confirm_adopt(&proposal, &moved)
        .expect_err("a stale proposal must be refused");
    assert!(
        err.contains("changed since the takeover was planned"),
        "got: {err}"
    );
    assert_eq!(
        store.state(),
        zroutery_core::agent_takeover::OwnershipState::Verified,
        "a refused confirmation must not adopt"
    );
}

#[test]
fn the_fingerprint_is_stable_across_calls() {
    // A gate that is always closed is indistinguishable from a broken one, so
    // this pins that an unchanged config fingerprints identically -- which is
    // what stops the staleness check from refusing every confirmation.
    let sandbox = Sandbox::new("fingerprint");
    let store = TakeoverStore::open(sandbox.join("ownership.json")).expect("open");
    let mut a = values(&[
        ("z", serde_json::json!(1)),
        ("a", serde_json::json!(2)),
        ("m", serde_json::json!(3)),
    ]);
    let first = store
        .plan_adopt(
            &["a".to_string()],
            &a,
            &values(&[("a", serde_json::json!(9))]),
        )
        .expect("plan")
        .fingerprint;

    // Insertion order into a HashMap is not stable, so a second, differently
    // built map with identical contents must still hash the same.
    let mut b = HashMap::new();
    b.insert("m".to_string(), serde_json::json!(3));
    b.insert("a".to_string(), serde_json::json!(2));
    b.insert("z".to_string(), serde_json::json!(1));

    let second = store
        .plan_adopt(
            &["a".to_string()],
            &b,
            &values(&[("a", serde_json::json!(9))]),
        )
        .expect("plan")
        .fingerprint;
    assert_eq!(first, second, "fingerprint must not depend on map ordering");

    a.insert("a".to_string(), serde_json::json!(99));
    let third = store
        .plan_adopt(
            &["a".to_string()],
            &a,
            &values(&[("a", serde_json::json!(9))]),
        )
        .expect("plan")
        .fingerprint;
    assert_ne!(first, third, "a changed value must change the fingerprint");
}

#[test]
fn planning_refuses_an_incomplete_or_empty_takeover() {
    let sandbox = Sandbox::new("plan_refusals");
    let store = TakeoverStore::open(sandbox.join("ownership.json")).expect("open");
    let current = values(&[("model", serde_json::json!("opus"))]);

    let err = store
        .plan_adopt(&[], &current, &HashMap::new())
        .expect_err("nothing to confirm");
    assert!(err.contains("no fields"), "got: {err}");

    let err = store
        .plan_adopt(
            &["model".to_string(), "temperature".to_string()],
            &current,
            &values(&[("model", serde_json::json!("haiku"))]),
        )
        .expect_err("a managed field with no value");
    assert!(
        err.contains("no managed value supplied for temperature"),
        "got: {err}"
    );
}

#[test]
fn a_proposal_reports_fields_it_would_create() {
    let sandbox = Sandbox::new("plan_create");
    let store = TakeoverStore::open(sandbox.join("ownership.json")).expect("open");
    let current = values(&[("model", serde_json::json!("opus"))]);
    let managed = values(&[
        ("model", serde_json::json!("haiku")),
        ("base_url", serde_json::json!("http://127.0.0.1:8080")),
    ]);

    let proposal = store
        .plan_adopt(
            &["model".to_string(), "base_url".to_string()],
            &current,
            &managed,
        )
        .expect("plan");

    assert_eq!(
        proposal.changes[1].current, None,
        "base_url does not exist yet"
    );
    assert!(
        proposal.summary().contains("creating 1"),
        "creating a field is the part a user most needs told about, got: {}",
        proposal.summary()
    );
}
