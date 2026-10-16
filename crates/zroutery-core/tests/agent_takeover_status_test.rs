//! Gate fixtures for the surface status machine: what a client surface is
//! allowed to claim, and what it must never claim.
//!
//! # What these exist to prevent
//!
//! `OwnershipState` answers "what may this store do next". It cannot answer the
//! question a user actually asks, which is *is this client really using
//! Zroutery?* — and getting that wrong is not hypothetical. The Claude adapter
//! shipped pointing at `~/.claude.json`, a file Claude Code writes for itself
//! and does not read settings from. The write succeeded. The store reported
//! `Adopted`. The client kept talking to `api.anthropic.com`.
//!
//! So every one of these fixtures asserts a **negative**: a state the surface
//! must refuse to report. `degraded` when nobody verified anything, `conflict`
//! when a managed field moved, `available` when Zroutery has no token to offer.
//! The `active` fixtures exist only to prove the door is reachable, because a
//! gate that can never pass is indistinguishable from a broken one.
//!
//! `verification::request_confirmed` is three-valued on purpose and has its own
//! fixture: `None` (nobody looked) must not read the same as `Some(false)`
//! (somebody looked and it failed).

use std::collections::HashMap;
use std::path::PathBuf;

use zroutery_core::agent_takeover::{
    adapter_for, takeover_fields, AgentAdapter, AgentConfigSnapshot, AgentType, ConflictResolution,
    GatewayTarget, ManagedField, OwnershipManifest, TakeoverStatus, TakeoverStore, TierClass,
    VerificationReport,
};

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

/// A per-test directory, removed on drop.
///
/// One directory per fixture rather than a shared `temp_dir()` name: these run
/// in parallel threads inside one binary.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new(label: &str) -> Self {
        let unique = format!(
            "zroutery_status_{}_{}_{:?}",
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

// ---------------------------------------------------------------------------
// A real adapter over a real file
// ---------------------------------------------------------------------------

/// Reads and writes a real file, so `inspect` is exercised through the same
/// read path production uses rather than through a struct field.
struct SandboxAdapter {
    path: PathBuf,
    agent: AgentType,
}

impl SandboxAdapter {
    fn new(sandbox: &Sandbox, agent: AgentType, initial: serde_json::Value) -> Self {
        let path = sandbox.join("agent-config.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&initial).expect("serialize"),
        )
        .expect("write config");
        Self { path, agent }
    }

    /// An adapter whose file was never created.
    fn absent(sandbox: &Sandbox, agent: AgentType) -> Self {
        Self {
            path: sandbox.join("never-written.json"),
            agent,
        }
    }

    fn write_raw(&self, value: &serde_json::Value) {
        std::fs::write(
            &self.path,
            serde_json::to_string_pretty(value).expect("serialize"),
        )
        .expect("write config");
    }

    fn read_raw(&self) -> serde_json::Value {
        let data = std::fs::read_to_string(&self.path).expect("config exists");
        serde_json::from_str(&data).expect("config parses")
    }

    /// Put one field somewhere else, as a user or another tool would.
    fn poke(&self, path: &str, value: serde_json::Value) {
        let mut raw = self.read_raw();
        set_at(&mut raw, path, value);
        self.write_raw(&raw);
    }
}

impl AgentAdapter for SandboxAdapter {
    fn agent_type(&self) -> AgentType {
        self.agent
    }

    fn config_path(&self) -> Result<PathBuf, String> {
        Ok(self.path.clone())
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let (raw, hash) = if self.path.exists() {
            let data = std::fs::read_to_string(&self.path).map_err(|e| format!("read: {e}"))?;
            let hash = format!("{:x}", data.len());
            let raw = serde_json::from_str(&data).map_err(|e| format!("parse: {e}"))?;
            (raw, hash)
        } else {
            (serde_json::json!({}), String::new())
        };
        Ok(AgentConfigSnapshot {
            agent_type: self.agent,
            config_path: self.path.clone(),
            config_hash: hash,
            raw,
        })
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        let mut raw = snapshot.raw.clone();
        for field in fields {
            set_at(&mut raw, &field.path, field.value.clone());
        }
        self.write_raw(&raw);
        Ok(AgentConfigSnapshot {
            raw,
            config_path: self.path.clone(),
            config_hash: String::new(),
            agent_type: self.agent,
        })
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        // Mirror the core's `restore_fields` exactly, including its ordering:
        // `absent_fields` is checked first, so a path in both lists is deleted.
        // A fixture that restored first and deleted second would pass cases the
        // production path fails.
        let mut raw = snapshot.raw.clone();
        for path in &manifest.managed_fields {
            if manifest.absent_fields.iter().any(|a| a == path) {
                remove_at(&mut raw, path);
            } else if let Some(original) = manifest.field_snapshots.get(path) {
                set_at(&mut raw, path, original.clone());
            }
        }
        self.write_raw(&raw);
        Ok(())
    }

    fn write_config(&self, snapshot: &AgentConfigSnapshot) -> Result<(), String> {
        self.write_raw(&snapshot.raw);
        Ok(())
    }
}

fn set_at(root: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let segments: Vec<&str> = path.split('.').collect();
    let mut cursor = root;
    for segment in &segments[..segments.len() - 1] {
        if !cursor.is_object() {
            *cursor = serde_json::json!({});
        }
        cursor = cursor
            .as_object_mut()
            .expect("object")
            .entry((*segment).to_string())
            .or_insert_with(|| serde_json::json!({}));
    }
    if !cursor.is_object() {
        *cursor = serde_json::json!({});
    }
    cursor
        .as_object_mut()
        .expect("object")
        .insert(segments[segments.len() - 1].to_string(), value);
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

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn a_target() -> GatewayTarget {
    GatewayTarget {
        base_url: "http://127.0.0.1:8787".into(),
        token: "gw-local-token".into(),
        classes: vec![TierClass {
            model: "sonnet-class".into(),
            display_name: "Sonnet".into(),
        }],
        default_class: None,
    }
}

/// A verification that actually proves the client is using Zroutery.
fn fully_verified() -> VerificationReport {
    VerificationReport {
        config_matches: true,
        credential_ok: true,
        request_confirmed: Some(true),
        notes: Vec::new(),
    }
}

fn store_for(sandbox: &Sandbox, label: &str) -> TakeoverStore {
    TakeoverStore::open(sandbox.join(&format!("{label}-manifest.json"))).expect("durable store")
}

/// Run a real takeover: plan, write, confirm, record.
fn take_over(store: &TakeoverStore, adapter: &SandboxAdapter, target: &GatewayTarget) {
    let fields = takeover_fields(AgentType::Claude, target).expect("field set");
    let paths: Vec<String> = fields.iter().map(|f| f.path.clone()).collect();
    let managed: HashMap<String, serde_json::Value> = fields
        .iter()
        .map(|f| (f.path.clone(), f.value.clone()))
        .collect();

    let before = adapter.read_config().expect("read");
    let current = extract(&before.raw, &paths);

    let proposal = store.plan_adopt(&paths, &current, &managed).expect("plan");
    let snapshot = adapter.apply_patch(&before, &fields).expect("apply");
    adapter.write_config(&snapshot).expect("write");
    store.confirm_adopt(&proposal, &current).expect("confirm");
    store.record_applied(&managed).expect("record applied");
}

/// The same dotted-path walk the core uses, so the fixture's `current` map
/// matches the one `confirm_adopt` fingerprints.
fn extract(root: &serde_json::Value, paths: &[String]) -> HashMap<String, serde_json::Value> {
    let mut found = HashMap::new();
    for path in paths {
        let mut cursor = root;
        let mut ok = true;
        for segment in path.split('.') {
            match cursor.as_object().and_then(|m| m.get(segment)) {
                Some(next) => cursor = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            found.insert(path.clone(), cursor.clone());
        }
    }
    found
}

// ---------------------------------------------------------------------------
// The states that must be reachable
// ---------------------------------------------------------------------------

#[test]
fn a_client_with_no_adapter_is_unsupported() {
    let sandbox = Sandbox::new("unsupported");
    let store = store_for(&sandbox, "unsupported");

    let status = store.inspect(None, Some(&a_target()), None);

    assert_eq!(status.name(), "unsupported", "{status:?}");
    match &status {
        TakeoverStatus::Unsupported { reason } => {
            assert!(
                reason.contains("gemini"),
                "the reason has to name the client: {reason}"
            );
        }
        other => panic!("expected unsupported, got {other:?}"),
    }
    assert!(
        adapter_for(AgentType::Gemini).is_none(),
        "and there must genuinely be no adapter behind that claim"
    );
}

#[test]
fn a_client_with_no_config_file_is_not_detected() {
    let sandbox = Sandbox::new("not_detected");
    let store = store_for(&sandbox, "not_detected");
    let adapter = SandboxAdapter::absent(&sandbox, AgentType::Claude);

    let status = store.inspect(Some(&adapter), Some(&a_target()), None);

    match &status {
        TakeoverStatus::NotDetected { searched, reason } => {
            assert_eq!(searched.len(), 1, "the paths tried must be reported");
            assert!(searched[0].ends_with("never-written.json"), "{searched:?}");
            assert!(!reason.is_empty(), "a dead end needs a reason");
        }
        other => panic!("expected not_detected, got {other:?}"),
    }
    assert!(
        !status.holds_ownership(),
        "nothing is held when nothing was found"
    );
}

#[test]
fn a_detected_client_with_no_gateway_token_is_available_not_ready() {
    let sandbox = Sandbox::new("available");
    let store = store_for(&sandbox, "available");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));

    // No target at all: the desktop shell has not minted a token yet.
    let status = store.inspect(Some(&adapter), None, None);
    match &status {
        TakeoverStatus::Available { blocker, .. } => {
            assert!(blocker.contains("token"), "{blocker}");
        }
        other => panic!("expected available, got {other:?}"),
    }

    // A target that exists but is unusable reports the same state with the
    // specific blocker, rather than a `ready` button that fails on click.
    let mut broken = a_target();
    broken.token = String::new();
    let status = store.inspect(Some(&adapter), Some(&broken), None);
    match &status {
        TakeoverStatus::Available { blocker, .. } => {
            assert!(blocker.contains("token"), "{blocker}");
        }
        other => panic!("expected available, got {other:?}"),
    }
}

#[test]
fn a_detected_client_with_a_usable_target_is_ready_with_a_preview() {
    let sandbox = Sandbox::new("ready");
    let store = store_for(&sandbox, "ready");
    let adapter = SandboxAdapter::new(
        &sandbox,
        AgentType::Claude,
        serde_json::json!({ "model": "opus", "permissions": { "allow": ["Bash"] } }),
    );
    let target = a_target();

    let status = store.inspect(Some(&adapter), Some(&target), None);

    match &status {
        TakeoverStatus::Ready {
            proposal,
            config_path,
        } => {
            assert!(config_path.ends_with("agent-config.json"), "{config_path}");
            assert_eq!(
                proposal.managed_fields,
                vec!["env.ANTHROPIC_BASE_URL", "env.ANTHROPIC_AUTH_TOKEN"],
                "the preview must name the exact fields that will change"
            );
            // Both are new, so both are reported as creations.
            assert!(
                proposal.changes.iter().all(|c| c.current.is_none()),
                "a preview that hides which fields are new is not a preview"
            );
        }
        other => panic!("expected ready, got {other:?}"),
    }
    assert!(!status.holds_ownership(), "ready holds nothing yet");
}

// ---------------------------------------------------------------------------
// The states that must be reachable only with proof
// ---------------------------------------------------------------------------

#[test]
fn a_written_but_unverified_takeover_is_degraded_not_active() {
    let sandbox = Sandbox::new("degraded");
    let store = store_for(&sandbox, "degraded");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();
    take_over(&store, &adapter, &target);

    // No verification supplied at all.
    let status = store.inspect(Some(&adapter), Some(&target), None);
    match &status {
        TakeoverStatus::Degraded { reasons, .. } => {
            assert!(
                reasons.iter().any(|r| r.contains("no request")),
                "the user has to be told what is missing: {reasons:?}"
            );
        }
        other => panic!("a written config must never read as active: {other:?}"),
    }
    assert!(
        status.holds_ownership(),
        "ownership is still held, so the release path must stay available"
    );
    assert!(
        !status.is_verified_effective(),
        "and it must not claim to be verified"
    );
}

#[test]
fn nobody_looking_is_not_the_same_as_looking_and_failing() {
    let sandbox = Sandbox::new("three_valued");
    let store = store_for(&sandbox, "three_valued");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();
    take_over(&store, &adapter, &target);

    // Not observable here: a caller that cannot see client traffic must not be
    // able to claim success.
    let unchecked = VerificationReport::unchecked();
    assert_eq!(unchecked.request_confirmed, None);
    let unobservable = store.inspect(Some(&adapter), Some(&target), Some(&unchecked));
    assert_eq!(unobservable.name(), "degraded", "{unobservable:?}");

    // Observable, and it failed.
    let failed = VerificationReport {
        config_matches: true,
        credential_ok: true,
        request_confirmed: Some(false),
        notes: vec!["the client sent a request that never arrived".into()],
    };
    let observed_failure = store.inspect(Some(&adapter), Some(&target), Some(&failed));
    match &observed_failure {
        TakeoverStatus::Degraded { reasons, .. } => {
            assert!(
                reasons.iter().any(|r| r.contains("never arrived")),
                "the caller's own note must survive: {reasons:?}"
            );
        }
        other => panic!("expected degraded, got {other:?}"),
    }
}

#[test]
fn a_config_that_still_matches_but_is_unproven_stays_degraded() {
    let sandbox = Sandbox::new("config_only");
    let store = store_for(&sandbox, "config_only");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();
    take_over(&store, &adapter, &target);

    // The file is untouched and the credential works, but no request was seen.
    let partial = VerificationReport {
        config_matches: true,
        credential_ok: true,
        request_confirmed: None,
        notes: Vec::new(),
    };
    let status = store.inspect(Some(&adapter), Some(&target), Some(&partial));
    assert_eq!(
        status.name(),
        "degraded",
        "a matching config is necessary but not sufficient: {status:?}"
    );
}

#[test]
fn a_fully_verified_takeover_is_active() {
    let sandbox = Sandbox::new("active");
    let store = store_for(&sandbox, "active");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();
    take_over(&store, &adapter, &target);

    let verified = fully_verified();
    let status = store.inspect(Some(&adapter), Some(&target), Some(&verified));

    match &status {
        TakeoverStatus::Active { generation, .. } => {
            assert_eq!(*generation, 0, "the first takeover is generation 0");
        }
        other => panic!("expected active, got {other:?}"),
    }
    assert!(status.is_verified_effective());
    assert!(status.holds_ownership());
}

#[test]
fn an_external_edit_to_a_managed_field_is_a_conflict() {
    let sandbox = Sandbox::new("conflict");
    let store = store_for(&sandbox, "conflict");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();
    take_over(&store, &adapter, &target);

    // Somebody else rewrites the field Zroutery owns.
    adapter.poke(
        "env.ANTHROPIC_BASE_URL",
        serde_json::json!("https://someone-elses-proxy.example"),
    );

    let verified = fully_verified();
    let status = store.inspect(Some(&adapter), Some(&target), Some(&verified));

    match &status {
        TakeoverStatus::Conflict { conflicts, .. } => {
            assert_eq!(conflicts.len(), 1, "{conflicts:?}");
            assert_eq!(conflicts[0].field_path, "env.ANTHROPIC_BASE_URL");
        }
        other => panic!(
            "a verified-looking client whose config moved must read as conflict, not active: \
             {other:?}"
        ),
    }
    assert!(
        status.holds_ownership(),
        "conflict still holds ownership: the release path must stay reachable"
    );
    assert!(
        !status.is_verified_effective(),
        "and it must never claim to be verified"
    );

    // The user's edit survived the inspection. Nothing was overwritten.
    let disk = adapter.read_raw();
    assert_eq!(
        disk["env"]["ANTHROPIC_BASE_URL"],
        serde_json::json!("https://someone-elses-proxy.example"),
        "inspect must never write"
    );
}

#[test]
fn an_edited_config_does_not_report_active_even_when_verified() {
    let sandbox = Sandbox::new("conflict_over_verified");
    let store = store_for(&sandbox, "conflict_over_verified");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();
    take_over(&store, &adapter, &target);
    adapter.poke(
        "env.ANTHROPIC_AUTH_TOKEN",
        serde_json::json!("rotated-by-someone"),
    );

    let verified = fully_verified();
    let status = store.inspect(Some(&adapter), Some(&target), Some(&verified));
    assert_eq!(
        status.name(),
        "conflict",
        "verification is checked against the live config, not trusted over it: {status:?}"
    );
}

#[test]
fn a_released_takeover_reports_released() {
    let sandbox = Sandbox::new("released");
    let store = store_for(&sandbox, "released");
    let adapter = SandboxAdapter::new(
        &sandbox,
        AgentType::Claude,
        serde_json::json!({ "env": { "ANTHROPIC_BASE_URL": "https://api.anthropic.com" } }),
    );
    let target = a_target();
    take_over(&store, &adapter, &target);

    // The field existed before, so release puts the original value back.
    store
        .release_resolved(&adapter, ConflictResolution::OverwriteWithManaged)
        .expect("release");

    let status = store.inspect(Some(&adapter), Some(&target), Some(&fully_verified()));
    match &status {
        TakeoverStatus::Released { generation, .. } => {
            assert_eq!(*generation, 1, "a release bumps the generation");
        }
        other => panic!("expected released, got {other:?}"),
    }
    assert!(!status.holds_ownership());
    assert_eq!(
        adapter.read_raw()["env"]["ANTHROPIC_BASE_URL"],
        serde_json::json!("https://api.anthropic.com"),
        "the original value must be back on disk"
    );
}

#[test]
fn a_released_takeover_can_be_planned_again() {
    let sandbox = Sandbox::new("replan");
    let store = store_for(&sandbox, "replan");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    let target = a_target();

    take_over(&store, &adapter, &target);
    store
        .release_resolved(&adapter, ConflictResolution::KeepExternal)
        .expect("release");

    let status = store.inspect(Some(&adapter), Some(&target), None);
    assert_eq!(
        status.name(),
        "released",
        "a re-adopt is permitted without an artificial reset, but the surface \
         reports what actually happened: {status:?}"
    );
}

#[test]
fn a_corrupt_config_is_an_error_not_a_silent_ready() {
    let sandbox = Sandbox::new("corrupt");
    let store = store_for(&sandbox, "corrupt");
    let adapter = SandboxAdapter::new(&sandbox, AgentType::Claude, serde_json::json!({}));
    std::fs::write(&adapter.path, b"{ this is not json").expect("write garbage");

    let status = store.inspect(Some(&adapter), Some(&a_target()), None);

    match &status {
        TakeoverStatus::Error { reason } => {
            assert!(reason.contains("cannot be used"), "{reason}");
        }
        other => panic!("a file that will not parse must be an error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The full journey, in one test
// ---------------------------------------------------------------------------

/// The whole loop, asserted at every state, so the machine cannot pass each
/// fixture individually and still not make sense end to end.
#[test]
fn a_takeover_runs_detect_to_ready_to_degraded_to_active_to_conflict_to_released() {
    let sandbox = Sandbox::new("journey");
    let store = store_for(&sandbox, "journey");
    let adapter = SandboxAdapter::absent(&sandbox, AgentType::Claude);
    let target = a_target();

    // 1. Nothing installed.
    assert_eq!(
        store.inspect(Some(&adapter), Some(&target), None).name(),
        "not_detected"
    );

    // 2. The client is installed and has run once.
    std::fs::write(&adapter.path, br#"{ "model": "opus" }"#).expect("create config");
    assert_eq!(
        store.inspect(Some(&adapter), Some(&target), None).name(),
        "ready"
    );

    // 3. The user takes it over.
    take_over(&store, &adapter, &target);
    assert_eq!(
        store
            .inspect(
                Some(&adapter),
                Some(&target),
                Some(&VerificationReport::unchecked())
            )
            .name(),
        "degraded"
    );

    // 4. A real request from the client is observed.
    assert_eq!(
        store
            .inspect(Some(&adapter), Some(&target), Some(&fully_verified()))
            .name(),
        "active"
    );

    // 5. The user edits a managed field by hand.
    adapter.poke(
        "env.ANTHROPIC_BASE_URL",
        serde_json::json!("https://mine.example"),
    );
    assert_eq!(
        store
            .inspect(Some(&adapter), Some(&target), Some(&fully_verified()))
            .name(),
        "conflict"
    );

    // 6. They keep their edit and release.
    store
        .release_resolved(&adapter, ConflictResolution::KeepExternal)
        .expect("release");
    assert_eq!(
        store.inspect(Some(&adapter), Some(&target), None).name(),
        "released"
    );
    assert_eq!(
        adapter.read_raw()["env"]["ANTHROPIC_BASE_URL"],
        serde_json::json!("https://mine.example"),
        "the user's edit is what survives"
    );

    // 7. Releasing again is refused rather than silently repeating.
    assert!(
        store
            .release_resolved(&adapter, ConflictResolution::Skip)
            .is_err(),
        "a second release must not quietly do it again"
    );
}
