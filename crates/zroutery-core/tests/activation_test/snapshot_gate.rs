use std::fs;

use zroutery_core::ml::activation::{
    snapshot_id_for, ActivationAudit, ActivationError, ActivationStore, Snapshot, SnapshotId,
    ACTIVATION_SNAPSHOT_SCHEMA_VERSION, JOURNAL_DIR_NAME, SNAPSHOT_ID_PREFIX,
};
use zroutery_core::ml::features::FEATURE_SCHEMA_VERSION;
use zroutery_core::ml::model_identity::{ModelId, ModelStore};

use super::support::*;

// ---------------------------------------------------------------------------
// Gate 1: a verified, immutable, content-addressed snapshot
// ---------------------------------------------------------------------------

#[test]
fn a_snapshot_is_content_addressed_and_verifies_on_load() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, commit_id) =
        activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("a verified commit is writable as a snapshot");

    // The name is derived from the content, not chosen by the caller.
    assert_eq!(snapshot.id(), &snapshot_id_for(&commit));
    assert!(snapshot.id().is_well_formed());
    assert!(snapshot.id().as_str().starts_with(SNAPSHOT_ID_PREFIX));
    assert_eq!(snapshot.commit_id(), &commit_id);

    // The file name is the identity.
    let path = snapshot_file(&root, snapshot.id());
    assert!(path.is_file(), "{} is on disk", path.display());

    // Loading re-verifies through the accepted checks and re-derives the name.
    let loaded = activation
        .read_snapshot(snapshot.id())
        .expect("a stored snapshot verifies");
    assert_eq!(loaded.id(), snapshot.id());
    assert_eq!(&loaded.commit().commit_id, loaded.commit_id());
    assert!(loaded.commit().verify(), "the accepted verify still passes");
    assert_eq!(
        snapshot_id_for(loaded.commit()),
        *snapshot.id(),
        "the name is a pure function of the content"
    );

    // A different commit gets a different name, so one name cannot hold two
    // models through the ordinary path.
    let (other, _) = trained_commit(&mut store, Some(commit_id), "beta", 2);
    assert_ne!(snapshot_id_for(&other), *snapshot.id());
    assert_clean_no_tmp(&root);
}

#[test]
fn writing_a_snapshot_never_mutates_an_existing_one() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the first write succeeds");
    let path = snapshot_file(&root, snapshot.id());
    let before = fs::read(&path).expect("the snapshot is readable");

    // Writing the same commit again is refused, not absorbed.
    match activation.write_snapshot(&commit) {
        Err(ActivationError::SnapshotExists { snapshot: name, .. }) => {
            assert_eq!(name, snapshot.id().as_str());
        }
        other => panic!("expected a SnapshotExists refusal, got {other:?}"),
    }

    // The immutability guarantee holds even against a *corrupt* existing
    // snapshot: the refusal is about the name being taken, not about validity.
    fs::write(&path, b"{ not a snapshot").expect("the snapshot is writable");
    let corrupt = fs::read(&path).expect("the snapshot is readable");
    assert!(matches!(
        activation.write_snapshot(&commit),
        Err(ActivationError::SnapshotExists { .. })
    ));
    assert_eq!(
        fs::read(&path).expect("readable"),
        corrupt,
        "a refused write must not touch the bytes that are there"
    );
    assert_ne!(
        corrupt, before,
        "the corruption is in place for the next assertion"
    );

    // And loading refuses it rather than repairing it.
    assert!(matches!(
        activation.read_snapshot(snapshot.id()),
        Err(ActivationError::SnapshotUnreadable { .. })
    ));
    assert_eq!(
        fs::read(&path).expect("readable"),
        corrupt,
        "a refused load must not repair anything"
    );
    assert_clean_no_tmp(&root);
}

#[test]
fn a_snapshot_that_fails_the_accepted_integrity_checks_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the write succeeds");
    let path = snapshot_file(&root, snapshot.id());

    // The algorithm table is inside the commit's canonical identity but outside
    // the snapshot's name derivation, so this is an integrity failure rather
    // than a name mismatch — and it is caught by the accepted `verify`.
    edit_snapshot_file(&path, |value| {
        value["commit"]["algorithm_versions"][0][1] = serde_json::json!("tampered");
    });
    let tampered = fs::read(&path).expect("readable");
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::CommitRejected { commit: id, reason }) => {
            assert_eq!(&id, snapshot.commit_id());
            assert!(
                reason.contains("verify"),
                "the reason names the check: {reason}"
            );
        }
        other => panic!("expected a CommitRejected refusal, got {other:?}"),
    }
    assert_eq!(
        fs::read(&path).expect("readable"),
        tampered,
        "loading never repairs"
    );
}

#[test]
fn a_legacy_or_unknown_envelope_is_refused_by_name() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the write succeeds");
    let path = snapshot_file(&root, snapshot.id());

    // A legacy commit envelope: the unversioned form that needs an explicit
    // migration, which this module does not perform.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(0);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::LegacyEnvelope { component, version }) => {
            assert_eq!(component, "commit envelope");
            assert_eq!(version, 0);
        }
        other => panic!("expected a LegacyEnvelope refusal, got {other:?}"),
    }

    // A legacy checkpoint envelope, inside an otherwise current commit.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(1);
        value["commit"]["checkpoint"]["schema_version"] = serde_json::json!(0);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::LegacyEnvelope { component, version }) => {
            assert_eq!(component, "checkpoint envelope");
            assert_eq!(version, 0);
        }
        other => panic!("expected a LegacyEnvelope refusal, got {other:?}"),
    }

    // An unknown commit envelope version.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(1);
        value["commit"]["checkpoint"]["schema_version"] = serde_json::json!(1);
        value["commit"]["schema_version"] = serde_json::json!(7);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::UnsupportedSchema {
            component,
            version,
            supported,
        }) => {
            assert_eq!(component, "commit envelope");
            assert_eq!(version, 7);
            assert_eq!(supported, 1);
        }
        other => panic!("expected an UnsupportedSchema refusal, got {other:?}"),
    }

    // An unknown snapshot envelope version of its own.
    edit_snapshot_file(&path, |value| {
        value["commit"]["schema_version"] = serde_json::json!(1);
        value["commit"]["checkpoint"]["schema_version"] = serde_json::json!(1);
        value["schema_version"] = serde_json::json!(9);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::UnsupportedSchema {
            component,
            version,
            supported,
        }) => {
            assert_eq!(component, "snapshot envelope");
            assert_eq!(version, 9);
            assert_eq!(supported, ACTIVATION_SNAPSHOT_SCHEMA_VERSION);
        }
        other => panic!("expected an UnsupportedSchema refusal, got {other:?}"),
    }
}

#[test]
fn a_snapshot_whose_stored_name_disagrees_with_its_content_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot = activation
        .write_snapshot(&commit)
        .expect("the write succeeds");
    let path = snapshot_file(&root, snapshot.id());

    // A colliding name is not a silent overwrite and not a wrong read: the
    // loader re-derives the address and refuses.
    let other_name = format!("{SNAPSHOT_ID_PREFIX}0000000000000000");
    edit_snapshot_file(&path, |value| {
        value["snapshot_id"] = serde_json::json!(other_name);
    });
    match activation.read_snapshot(snapshot.id()) {
        Err(ActivationError::IdentityMismatch {
            snapshot: found,
            stored,
            derived,
        }) => {
            assert_eq!(found, snapshot.id().as_str());
            assert_eq!(stored, other_name);
            assert_eq!(derived, snapshot_id_for(&commit).as_str());
        }
        other => panic!("expected an IdentityMismatch refusal, got {other:?}"),
    }
}

/// ML-16: the loader must compare the *content-derived* name, not only the
/// header the file declares. A file holding snapshot B's content with the header
/// rewritten to A used to load as A, which makes a name no longer bind content.
#[test]
fn a_snapshot_whose_content_derives_a_different_name_is_refused() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, _) = trained_commit(&mut store, None, "alpha", 1);
    let (commit_b, commit_b_id) = trained_commit(&mut store, None, "beta", 2);
    let derived_a = snapshot_id_for(&commit_a);
    let derived_b = snapshot_id_for(&commit_b);
    assert_ne!(
        derived_a, derived_b,
        "the two fixtures must have different content addresses"
    );

    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    assert_eq!(snapshot_a.id(), &derived_a);
    assert_eq!(snapshot_b.id(), &derived_b);

    // B's whole legal payload, with only its header rewritten to A: the header
    // agrees with the requested (and file) name, so only the re-derived content
    // address can catch it. This is a missing comparison, not a hash collision.
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(snapshot_file(&root, snapshot_b.id())).expect("readable"))
            .expect("the snapshot is valid json");
    assert_eq!(value["snapshot_id"], serde_json::json!(derived_b.as_str()));
    value["snapshot_id"] = serde_json::json!(derived_a.as_str());
    let rewritten = serde_json::to_vec_pretty(&value).expect("serializable");
    let path_a = snapshot_file(&root, snapshot_a.id());
    fs::write(&path_a, &rewritten).expect("the snapshot is writable");

    match activation.read_snapshot(snapshot_a.id()) {
        Err(ActivationError::IdentityMismatch {
            snapshot,
            stored,
            derived,
        }) => {
            assert_eq!(snapshot, derived_a.as_str());
            assert_eq!(
                stored,
                derived_a.as_str(),
                "the header was rewritten to agree with the path"
            );
            assert_eq!(
                derived,
                derived_b.as_str(),
                "the content is what derives the address"
            );
        }
        other => panic!("expected an IdentityMismatch refusal, got {other:?}"),
    }
    assert_eq!(
        fs::read(&path_a).expect("readable"),
        rewritten,
        "loading never repairs the bytes it refused"
    );

    // The untouched B snapshot still loads under its own, correct name.
    assert_eq!(
        activation
            .read_snapshot(snapshot_b.id())
            .expect("B still verifies")
            .commit_id(),
        &commit_b_id
    );
}

#[test]
fn a_malformed_snapshot_name_is_refused_before_it_becomes_a_path() {
    let temp = scratch();
    let root = store_root(temp.path());
    let activation = ActivationStore::open(&root).expect("the store opens");
    let store = ModelStore::with_model_id(ModelId::new(MODEL));

    // A name that reached `Path::join` unchecked would be a path traversal, so
    // every entry point that turns one into a path checks it first.
    for hostile in [
        "../../evil",
        "snap-",
        "snap-0123456789ABCDEF",
        "snap-0123456789abcde",
        "snap-0123456789abcdefff",
        "snap-zzzzzzzzzzzzzzzz",
        "",
        "snap-0123456789abcdef/../..",
        "snap-0123456789abcdef.json",
    ] {
        let id = SnapshotId::new(hostile);
        assert!(!id.is_well_formed(), "{hostile} must not be well formed");
        assert!(matches!(
            activation.read_snapshot(&id),
            Err(ActivationError::MalformedSnapshotId { id: refused }) if refused == hostile
        ));
        assert!(matches!(
            activation.snapshot_path(&id),
            Err(ActivationError::MalformedSnapshotId { .. })
        ));
        assert!(matches!(
            activation.activate(
                &zroutery_core::ml::activation::ActivationRequest::new(id.clone()),
                &store
            ),
            Err(ActivationError::MalformedSnapshotId { .. })
        ));
    }

    // A well-formed name that simply is not there is a different refusal.
    let absent = SnapshotId::new(format!("{SNAPSHOT_ID_PREFIX}0123456789abcdef"));
    assert!(matches!(
        activation.read_snapshot(&absent),
        Err(ActivationError::SnapshotNotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// A small compile-time reminder of what the mechanism is, and is not
// ---------------------------------------------------------------------------

/// A snapshot verifies and carries a commit. That is the whole of it: there is
/// no predicate, no engine, and no serving handle anywhere in this type's
/// surface, which is why an activation here cannot install a model even if a
/// caller asked it to.
#[test]
fn a_snapshot_carries_a_verified_commit_and_nothing_else() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit, _) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot: Snapshot = activation.write_snapshot(&commit).expect("written");
    assert!(snapshot.commit().verify());
    assert_eq!(
        snapshot.commit().checkpoint.feature_schema_version,
        FEATURE_SCHEMA_VERSION
    );
    // The audit is the reporting surface, and it reports records, not models.
    // It must not mutate anything either.
    let log_before = log_bytes(&root);
    let audit: ActivationAudit = activation.audit(&store).expect("readable");
    assert!(
        audit.traces.is_empty(),
        "no activation has happened, so there is nothing to report"
    );
    assert!(audit.pending.is_none());
    assert!(audit.disagreement.is_none());
    assert_eq!(log_bytes(&root), log_before, "an audit writes nothing");
    assert!(
        pointer_bytes(&root).is_none(),
        "an audit never brings a snapshot into being"
    );
}
