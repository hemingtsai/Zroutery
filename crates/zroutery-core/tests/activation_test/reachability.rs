use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use zroutery_core::ml::activation::{
    ActivationStore, ACTIVATION_LOCK_NAME, ACTIVATION_POINTER_NAME, ACTIVATION_ROLE,
    JOURNAL_DIR_NAME,
};
use zroutery_core::ml::model_identity::{ModelId, ModelStore};

use super::support::*;

// ---------------------------------------------------------------------------
// Containment, and the reachability gate
// ---------------------------------------------------------------------------

#[test]
fn nothing_is_created_outside_the_caller_directory() {
    let temp = scratch();
    let root = store_root(temp.path());
    let mut store = ModelStore::with_model_id(ModelId::new(MODEL));
    let (commit_a, id_a) = activatable(&mut store, &root.join(JOURNAL_DIR_NAME), None, "alpha", 1);
    let activation = ActivationStore::open(&root).expect("the store opens");
    let snapshot_a = activation.write_snapshot(&commit_a).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_a.id().clone()),
            &store,
        )
        .expect("activated");
    let (commit_b, _) = activatable(
        &mut store,
        &root.join(JOURNAL_DIR_NAME),
        Some(id_a.clone()),
        "beta",
        2,
    );
    let snapshot_b = activation.write_snapshot(&commit_b).expect("written");
    activation
        .activate(
            &zroutery_core::ml::activation::ActivationRequest::new(snapshot_b.id().clone()),
            &store,
        )
        .expect("activated");
    activation.rollback(&store).expect("rolled back");

    // Exactly the documented layout, and the locks while the handles are live.
    let expected: BTreeSet<String> = [
        ACTIVATION_POINTER_NAME.to_string(),
        "snapshots".to_string(),
        JOURNAL_DIR_NAME.to_string(),
        ACTIVATION_LOCK_NAME.to_string(),
    ]
    .into_iter()
    .collect();
    let present: BTreeSet<String> = fs::read_dir(&root)
        .expect("the root is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        present, expected,
        "the layout is exactly what is documented"
    );
    // The store lock is held for the handle's lifetime; the journal's own lock
    // exists only inside one operation, so nothing is left holding it between
    // calls.
    assert!(root.join(ACTIVATION_LOCK_NAME).is_file());
    assert!(!root.join(JOURNAL_DIR_NAME).join("journal.lock").exists());

    // Nothing was created beside the root, and no default location was used.
    let siblings: BTreeSet<String> = fs::read_dir(temp.path())
        .expect("the parent is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(siblings, BTreeSet::from(["activation-store".to_string()]));

    // With the handles dropped, only the durable artifacts remain.
    drop(activation);
    let present: BTreeSet<String> = fs::read_dir(&root)
        .expect("the root is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        present,
        BTreeSet::from([
            ACTIVATION_POINTER_NAME.to_string(),
            "snapshots".to_string(),
            JOURNAL_DIR_NAME.to_string(),
        ])
    );
    assert_eq!(
        fs::read_dir(root.join("snapshots"))
            .expect("readable")
            .filter_map(Result::ok)
            .count(),
        2,
        "both snapshots are still there: a rollback deletes nothing"
    );
    assert!(ACTIVATION_ROLE.contains("does not enable the ml feature"));
    assert!(ACTIVATION_ROLE.contains("no shipped caller"));
}

#[test]
fn this_module_reaches_no_live_activation_seam() {
    // `activation.rs` became `activation/`, so the fence reads every file of
    // the module, the way the calibration fences do.
    let module_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ml/activation");
    let mut source = String::new();
    for path in rust_and_manifest_files(&module_dir) {
        source.push_str(&fs::read_to_string(&path).expect("this module's own source is readable"));
        source.push('\n');
    }

    // Call-shaped patterns, so prose that *describes* the boundary does not
    // count as violating it: what must not exist is code that can reach the
    // accepted swap, its training sibling, the cold-start ensemble, a thread, a
    // timer, or the network. `ModelState` is the persisted weight vector and is
    // the one model-layer type this module needs; the four live model types are
    // named individually so the allowance cannot quietly widen.
    for forbidden in [
        "super::shadow",
        "crate::ml::shadow",
        "zroutery_core::ml::shadow",
        "ShadowEngine::",
        ".swap(",
        "try_train(",
        ".train(",
        "ModelEnsemble",
        "ModelEnsemblePredictor",
        "EnsemblePredictor",
        "DecisionEngine",
        "SuccessModel",
        "LatencyModel",
        "TtftModel",
        "CostModel",
        "RoutingModel",
        "thread::",
        "tokio::",
        "reqwest::",
        "set_head(",
        "migrate_legacy",
        "update_all(",
    ] {
        assert!(
            !source.contains(forbidden),
            "activation.rs must not contain {forbidden:?}"
        );
    }
    // The import spells `crate::ml::model` now that the module is a directory
    // child; the claim is the same one `use super::model::ModelState;` pinned
    // when it was a single file: the only model-layer type this module may name
    // is the persisted state.
    assert!(
        source.contains("use crate::ml::model::ModelState;"),
        "the only model-layer type this module may name is the persisted state"
    );

    // No configuration switch, and no location this module would choose itself.
    for forbidden in [
        "std::env",
        "var(\"",
        "ZROUTERY_",
        "ShadowConfig",
        "::default()",
    ] {
        assert!(
            !source.contains(forbidden),
            "activation.rs must not contain {forbidden:?}"
        );
    }
}

#[test]
fn nothing_in_the_shipped_product_can_name_this_module() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = core
        .parent()
        .and_then(Path::parent)
        .expect("the crate sits two levels below the workspace root");

    // The names this node introduces. A bare "activation" is not one of them: the
    // accepted bandit and decision-contract modules use the word in prose about
    // a mathematical activation function, and a scan that flagged those would be
    // measuring the wrong thing.
    let symbols = [
        "ml::activation",
        "activation::",
        "ActivationStore",
        "ActivationRequest",
        "ActivationOutcome",
        "ActivationError",
        "ActivationAudit",
        "ActivationPointer",
        "ActivationEntry",
        "ActivationTrace",
        "ActivationKind",
        "ActivationStage",
        "ActiveSnapshot",
        "SnapshotId",
        "SnapshotFile",
        "PointerFile",
        "PointerEntryFile",
        "PendingActivation",
        "PointerDisagreement",
        "ACTIVATION_POINTER_NAME",
        "ACTIVATION_ROLE",
        "ACTIVATION_SNAPSHOT_SCHEMA_VERSION",
        "POINTER_TMP_NAME",
        "SNAPSHOTS_DIR_NAME",
        "SNAPSHOT_FILE_SUFFIX",
        "ACTIVATION_LOCK_NAME",
        "JOURNAL_DIR_NAME",
        "activation_plan_event_id",
        "activation_applied_event_id",
        "snapshot_id_for",
        "snapshot_checksum",
        "pointer_checksum",
    ];

    let mut scanned = 0usize;
    for (label, dir) in [
        ("server", core.join("src/server")),
        ("src-tauri", repo.join("src-tauri/src")),
        ("ui", repo.join("ui")),
    ] {
        assert!(
            dir.is_dir(),
            "{} is expected to exist for this gate: {}",
            label,
            dir.display()
        );
        for path in rust_and_manifest_files(&dir) {
            let text = fs::read_to_string(&path).expect("readable");
            scanned += 1;
            for symbol in symbols {
                assert!(
                    !text.contains(symbol),
                    "{} must not name {symbol:?}",
                    path.display()
                );
            }
        }
    }

    // The desktop application ships the learning stack, since ADR-0006 replaced
    // the prohibition on a model serving with a gate, a deterministic fallback
    // and a rollback. Two things are still worth asserting about that, and they
    // are the opposite of the assertion that used to be here.
    //
    // First, the escape hatch has to remain: `--no-default-features` must still
    // produce a desktop binary with no ML stack, because "ship without
    // learning" is a legitimate choice for someone who does not want their
    // routing history on disk.
    let manifest = fs::read_to_string(repo.join("src-tauri/Cargo.toml")).expect("readable");
    assert!(
        manifest.contains("ml = [\"zroutery-core/ml\"]"),
        "the desktop app must expose the ml feature as a named package feature, \
         so it can be turned off; the manifest is:\n{manifest}"
    );
    // And it ships by default.
    //
    // This is a membership check and not an equality check, and the difference
    // is the property itself: what is asserted here is that the desktop app
    // ships *with* the learning stack, not that the learning stack is the only
    // thing it ships. The default list carries the account subsystem, the
    // NewAPI panel adapter, the browser check-in and the OpenCode stack as
    // well, because the dashboard offers all of them and an interface that
    // offers a capability the build does not contain is an interface that
    // tells the operator the thing works and then does not do it.
    //
    // An equality check would have failed on that change while the property it
    // stands for still held, which is a gate measuring the wrong thing — and
    // the escape hatch above still holds, so the learning stack can still be
    // turned off by whoever wants it off.
    let default_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("default = ["))
        .expect("the manifest declares a default feature list");
    let defaults: Vec<&str> = default_line
        .trim()
        .trim_start_matches("default = [")
        .trim_end_matches(']')
        .split(',')
        .map(|feature| feature.trim().trim_matches('"'))
        .filter(|feature| !feature.is_empty())
        .collect();
    assert!(
        defaults.contains(&"ml"),
        "the desktop app ships with the learning stack by default; the default \
         feature list is {defaults:?}"
    );
    // And it is not enabled by a bare feature reference on the dependency, which
    // would make it impossible to turn off without editing the manifest.
    for line in manifest.lines() {
        if line.contains("zroutery-core") && line.contains("features") {
            panic!(
                "the core dependency must not name features directly; the package \
                 feature is the switch, so it can be turned off. Offending line: {line}"
            );
        }
    }

    // Second, and the property that actually matters now: enabling the stack is
    // not the same as changing behaviour. A desktop install that has never
    // promoted a model must route exactly as it did before. That is asserted
    // behaviourally in `config.rs`, against a default-constructed and a
    // document-deserialised configuration, because the property is about what
    // those configurations *do* rather than about how the default is written.

    // The crate still gates the whole ml module, and its own re-exports do not
    // widen the surface. Line endings are normalized so the check is about the
    // text rather than about how a checkout happens to be stored.
    let lib = fs::read_to_string(core.join("src/lib.rs"))
        .expect("readable")
        .replace("\r\n", "\n");
    let module_line = lib
        .lines()
        .position(|line| line.trim() == "pub mod ml;")
        .expect("lib.rs declares the ml module");
    let gate = lib.lines().nth(module_line - 1).unwrap_or_default();
    assert_eq!(
        gate.trim(),
        "#[cfg(feature = \"ml\")]",
        "the whole ml module stays behind the feature flag"
    );
    let re_export = lib
        .find("pub use ml::{")
        .expect("lib.rs re-exports a curated set of ml items");
    let ml_block = lib[re_export..]
        .split("};")
        .next()
        .expect("the block is terminated");
    for symbol in symbols {
        assert!(
            !ml_block.contains(symbol),
            "lib.rs must not re-export {symbol:?}"
        );
    }

    // Nothing else inside the ml module names this one either, so there is no
    // sibling caller waiting for a feature flag. `mod.rs` is the registration
    // point and is expected to mention it exactly twice: the declaration and the
    // re-export list.
    for entry in fs::read_dir(core.join("src/ml")).expect("readable") {
        let path = entry.expect("readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name == "mod.rs" {
            let text = fs::read_to_string(&path).expect("readable");
            assert_eq!(
                text.matches("pub mod activation;").count(),
                1,
                "the module is declared once"
            );
            assert_eq!(
                text.matches("pub use activation::{").count(),
                1,
                "and re-exported once"
            );
            continue;
        }
        if name == "activation" {
            // The module under test. It was `activation.rs` when the scan read
            // single files; now it is a directory and its own fence above scans
            // its files.
            continue;
        }
        // The siblings became directories too, so the scan walks each one's
        // files instead of assuming a flat file per module.
        let files: Vec<PathBuf> = if path.is_dir() {
            rust_and_manifest_files(&path)
                .into_iter()
                .filter(|file| file.extension().and_then(|ext| ext.to_str()) == Some("rs"))
                .collect()
        } else {
            vec![path.clone()]
        };
        for path in files {
            let text = fs::read_to_string(&path).expect("readable");
            for symbol in [
                "activation::",
                "ActivationStore",
                "SnapshotId",
                "ACTIVATION_ROLE",
            ] {
                assert!(
                    !text.contains(symbol),
                    "{} must not name {symbol:?}",
                    path.display()
                );
            }
        }
    }
    assert!(scanned >= 3, "the scan covered the shipped trees");
}

/// Every `.rs` file under a directory, plus the manifests, in a stable order.
fn rust_and_manifest_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("readable") {
            let path = entry.expect("readable").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.ends_with(".rs") || name == "Cargo.toml" {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
