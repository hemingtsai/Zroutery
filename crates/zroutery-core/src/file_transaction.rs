//! Crash-recoverable transactions over a small set of configuration files.
//!
//! A transaction is staged in an application-owned journal directory. Every
//! snapshot and the manifest is flushed before a durable commit marker is
//! published. Once that marker exists, recovery only rolls forward: a live
//! destination must contain either its exact original bytes or its exact target
//! bytes. Anything else is treated as an external edit and is never overwritten.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const JOURNAL_MAGIC: &str = "zroutery-file-transaction";
const JOURNAL_VERSION: u32 = 1;
const LOCK_FILE: &str = "transaction.lock";
const PENDING_DIR: &str = "pending";
const COMPLETED_DIR: &str = "completed";
const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_TEMP: &str = "manifest.tmp";
const COMMIT_FILE: &str = "commit.json";
const COMMIT_TEMP: &str = "commit.tmp";

// Configuration files should be many orders of magnitude smaller than these
// limits. The bounds keep a damaged journal from requesting absurd allocations
// before its hashes can be checked.
const MAX_CHANGES: usize = 4_096;
const MAX_BLOB_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BLOB_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_COMMIT_BYTES: u64 = 4 * 1024;
const MAX_PATH_BYTES: usize = 32 * 1024;
const MAX_PENDING_FILES: usize = MAX_CHANGES * 2 + 4;

/// One file's expected and desired byte state in a transaction.
///
/// `None` means that the file is missing. Thus `Some(Vec::new())` is an
/// existing empty file and is distinct from `None`.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
}

/// Commit all `changes` as a recoverable operation, or leave a durable journal that
/// [`recover`] can roll forward.
/// Each file replacement is atomic; external readers can see an intermediate
/// multi-file state until publication or recovery finishes.
///
/// Relative destinations are resolved against the current directory before
/// they enter the journal. Exact duplicate destinations are rejected.
pub fn commit(journal_dir: &Path, changes: &[FileChange]) -> Result<(), String> {
    let lock = JournalLock::acquire(journal_dir)?;
    commit_locked(&lock.root, changes, StageFault::default())
}

/// Recover the journal left by an interrupted [`commit`].
///
/// An uncommitted stage is discarded without touching live files. A committed
/// stage is rolled forward only while every destination is still at its exact
/// original or target state.
pub fn recover(journal_dir: &Path) -> Result<(), String> {
    let lock = JournalLock::acquire(journal_dir)?;
    recover_locked(&lock.root)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    magic: String,
    version: u32,
    transaction_id: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    index: u32,
    path: String,
    before: Option<BlobRecord>,
    after: Option<BlobRecord>,
    permissions: Option<PermissionRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlobRecord {
    file: String,
    len: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionRecord {
    readonly: bool,
    #[cfg(unix)]
    unix_mode: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitMarker {
    magic: String,
    version: u32,
    transaction_id: String,
    manifest_sha256: String,
}

struct NormalizedChange {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

struct PreparedTransaction {
    transaction_id: String,
    manifest_sha256: String,
}

struct LoadedTransaction {
    transaction_id: String,
    entries: Vec<LoadedEntry>,
}

struct LoadedEntry {
    index: usize,
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    permissions: Option<PermissionRecord>,
}

#[derive(Default)]
struct StageFault {
    // Private deterministic fault injection for module tests. Production always
    // passes None and has no environment-variable or global-state backdoor.
    fail_after_blobs: Option<usize>,
}

struct JournalLock {
    root: PathBuf,
    _file: File,
}

impl JournalLock {
    fn acquire(journal_dir: &Path) -> Result<Self, String> {
        let root = absolute_clean(journal_dir, "journal directory")?;
        let existed = path_exists_no_follow(&root)?;
        if !existed {
            create_private_dir_all(&root).map_err(|error| {
                format!(
                    "failed to create transaction journal {}: {error}",
                    root.display()
                )
            })?;
            sync_created_directory_chain(&root)?;
        }

        let metadata = fs::symlink_metadata(&root).map_err(|error| {
            format!(
                "failed to inspect transaction journal {}: {error}",
                root.display()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "transaction journal {} is not a real directory",
                root.display()
            ));
        }

        let lock_path = root.join(LOCK_FILE);
        if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "transaction lock {} is not a regular file",
                    lock_path.display()
                ));
            }
        }

        let lock_existed = path_exists_no_follow(&lock_path)?;
        let file = open_lock_file(&lock_path).map_err(|error| {
            format!(
                "failed to open transaction lock {}: {error}",
                lock_path.display()
            )
        })?;
        if !lock_existed {
            file.sync_all().map_err(|error| {
                format!(
                    "failed to flush transaction lock {}: {error}",
                    lock_path.display()
                )
            })?;
            sync_dir(&root)?;
        }
        file.try_lock().map_err(|error| {
            format!(
                "transaction journal {} is already locked: {error}",
                root.display()
            )
        })?;

        Ok(Self { root, _file: file })
    }
}

fn commit_locked(root: &Path, changes: &[FileChange], fault: StageFault) -> Result<(), String> {
    recover_locked(root)?;
    let changes = normalize_changes(root, changes)?;
    if changes.is_empty() {
        return Ok(());
    }

    let prepared = match stage_transaction(root, &changes, &fault) {
        Ok(prepared) => prepared,
        Err(stage_error) => {
            return match discard_uncommitted(root) {
                Ok(()) => Err(stage_error),
                Err(cleanup_error) => Err(format!(
                    "{stage_error}; additionally failed to discard uncommitted stage: {cleanup_error}"
                )),
            };
        }
    };

    if let Err(validation_error) = validate_original_live(&changes) {
        return match discard_uncommitted(root) {
            Ok(()) => Err(validation_error),
            Err(cleanup_error) => Err(format!(
                "{validation_error}; additionally failed to discard uncommitted stage: {cleanup_error}"
            )),
        };
    }

    if let Err(marker_error) = write_commit_marker(root, &prepared) {
        let marker_path = root.join(PENDING_DIR).join(COMMIT_FILE);
        if !path_exists_no_follow(&marker_path)? {
            return match discard_uncommitted(root) {
                Ok(()) => Err(marker_error),
                Err(cleanup_error) => Err(format!(
                    "{marker_error}; additionally failed to discard uncommitted stage: {cleanup_error}"
                )),
            };
        }
        // A visible marker is a commit decision even when its directory flush
        // reported an error. Retain it so this process or a later recovery can
        // safely roll forward; never reinterpret it as an uncommitted stage.
        return Err(marker_error);
    }

    drop(changes);
    let transaction = load_committed(root)?;
    roll_forward(&transaction)?;
    cleanup_committed(root)
}

fn validate_original_live(changes: &[NormalizedChange]) -> Result<(), String> {
    for change in changes {
        let live = read_live_state(&change.path)?;
        if live != change.before {
            return Err(format!(
                "transaction destination {} does not match its expected before bytes",
                change.path.display()
            ));
        }
    }
    Ok(())
}

fn recover_locked(root: &Path) -> Result<(), String> {
    cleanup_completed(root)?;
    let pending = root.join(PENDING_DIR);
    if !path_exists_no_follow(&pending)? {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&pending).map_err(|error| {
        format!(
            "failed to inspect pending journal {}: {error}",
            pending.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "pending journal {} is not a real directory",
            pending.display()
        ));
    }

    if !path_exists_no_follow(&pending.join(COMMIT_FILE))? {
        return discard_uncommitted(root);
    }

    let transaction = load_committed(root)?;
    roll_forward(&transaction)?;
    cleanup_committed(root)
}

fn normalize_changes(root: &Path, changes: &[FileChange]) -> Result<Vec<NormalizedChange>, String> {
    if changes.len() > MAX_CHANGES {
        return Err(format!(
            "transaction contains {} changes; maximum is {MAX_CHANGES}",
            changes.len()
        ));
    }

    let current_dir = std::env::current_dir()
        .map_err(|error| format!("failed to resolve current directory: {error}"))?;
    let root_key = path_key(root);
    let mut seen = HashSet::with_capacity(changes.len());
    let mut normalized = Vec::with_capacity(changes.len());
    let mut total_bytes = 0_u64;

    for change in changes {
        if change.path.as_os_str().is_empty() {
            return Err("transaction destination path is empty".to_string());
        }
        let path = if change.path.is_absolute() {
            clean_absolute(&change.path)?
        } else {
            clean_absolute(&current_dir.join(&change.path))?
        };
        let encoded = encode_path(&path)?;
        if encoded.len() > MAX_PATH_BYTES {
            return Err(format!(
                "transaction destination path is too long: {}",
                path.display()
            ));
        }

        let key = path_key(&path);
        if path_key_is_within(&key, &root_key) {
            return Err(format!(
                "transaction destination {} is inside its journal directory",
                path.display()
            ));
        }
        if !seen.insert(key) {
            return Err(format!(
                "transaction contains duplicate destination {}",
                path.display()
            ));
        }

        // Even a no-op is a caller's compare-and-swap precondition. Dropping it
        // before validating would let a changed auxiliary file authorize an
        // otherwise unrelated publication from a stale preview.
        if read_live_state(&path)? != change.before {
            return Err(format!(
                "transaction destination {} does not match its expected before bytes",
                path.display()
            ));
        }

        for bytes in [&change.before, &change.after].into_iter().flatten() {
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if len > MAX_BLOB_BYTES {
                return Err(format!(
                    "snapshot for {} is {len} bytes; maximum is {MAX_BLOB_BYTES}",
                    path.display()
                ));
            }
            total_bytes = total_bytes
                .checked_add(len)
                .ok_or_else(|| "transaction snapshot size overflow".to_string())?;
            if total_bytes > MAX_TOTAL_BLOB_BYTES {
                return Err(format!(
                    "transaction snapshots total {total_bytes} bytes; maximum is {MAX_TOTAL_BLOB_BYTES}"
                ));
            }
        }

        normalized.push(NormalizedChange {
            path,
            before: change.before.clone(),
            after: change.after.clone(),
        });
    }
    Ok(normalized)
}

fn stage_transaction(
    root: &Path,
    changes: &[NormalizedChange],
    fault: &StageFault,
) -> Result<PreparedTransaction, String> {
    let pending = root.join(PENDING_DIR);
    if path_exists_no_follow(&pending)? {
        return Err(format!(
            "pending journal {} already exists after recovery",
            pending.display()
        ));
    }
    create_private_dir(&pending).map_err(|error| {
        format!(
            "failed to create pending journal {}: {error}",
            pending.display()
        )
    })?;
    sync_dir(root)?;

    let transaction_id = Uuid::new_v4().to_string();
    let mut entries = Vec::with_capacity(changes.len());
    let mut blob_count = 0_usize;

    for (index, change) in changes.iter().enumerate() {
        let before = stage_optional_blob(
            &pending,
            index,
            "before",
            change.before.as_deref(),
            fault,
            &mut blob_count,
        )?;
        let after = stage_optional_blob(
            &pending,
            index,
            "after",
            change.after.as_deref(),
            fault,
            &mut blob_count,
        )?;

        entries.push(ManifestEntry {
            index: index as u32,
            path: encode_path(&change.path)?,
            before,
            after,
            permissions: permissions_for_expected(&change.path, change.before.is_some())?,
        });
    }

    let manifest = Manifest {
        magic: JOURNAL_MAGIC.to_string(),
        version: JOURNAL_VERSION,
        transaction_id: transaction_id.clone(),
        entries,
    };
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|error| format!("failed to serialize transaction manifest: {error}"))?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(format!(
            "transaction manifest is {} bytes; maximum is {MAX_MANIFEST_BYTES}",
            manifest_bytes.len()
        ));
    }
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    write_restricted_file(&pending.join(MANIFEST_TEMP), &manifest_bytes)?;
    durable_rename(&pending.join(MANIFEST_TEMP), &pending.join(MANIFEST_FILE)).map_err(
        |error| {
            format!(
                "failed to publish transaction manifest {}: {error}",
                pending.join(MANIFEST_FILE).display()
            )
        },
    )?;
    sync_dir(&pending)?;

    Ok(PreparedTransaction {
        transaction_id,
        manifest_sha256,
    })
}

fn stage_optional_blob(
    pending: &Path,
    index: usize,
    kind: &str,
    bytes: Option<&[u8]>,
    fault: &StageFault,
    blob_count: &mut usize,
) -> Result<Option<BlobRecord>, String> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    if fault.fail_after_blobs == Some(*blob_count) {
        return Err("injected staging failure".to_string());
    }
    let file = format!("{index:08}.{kind}.bin");
    write_restricted_file(&pending.join(&file), bytes)?;
    *blob_count += 1;
    Ok(Some(BlobRecord {
        file,
        len: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    }))
}

fn write_commit_marker(root: &Path, prepared: &PreparedTransaction) -> Result<(), String> {
    let pending = root.join(PENDING_DIR);
    let marker = CommitMarker {
        magic: JOURNAL_MAGIC.to_string(),
        version: JOURNAL_VERSION,
        transaction_id: prepared.transaction_id.clone(),
        manifest_sha256: prepared.manifest_sha256.clone(),
    };
    let bytes = serde_json::to_vec(&marker)
        .map_err(|error| format!("failed to serialize transaction commit marker: {error}"))?;
    write_restricted_file(&pending.join(COMMIT_TEMP), &bytes)?;
    durable_rename(&pending.join(COMMIT_TEMP), &pending.join(COMMIT_FILE)).map_err(|error| {
        format!(
            "failed to publish transaction commit marker {}: {error}",
            pending.join(COMMIT_FILE).display()
        )
    })?;
    sync_dir(&pending)
}

fn permissions_for_expected(
    path: &Path,
    expected_exists: bool,
) -> Result<Option<PermissionRecord>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "transaction destination {} is not a regular file",
                    path.display()
                ));
            }
            if !expected_exists {
                return Err(format!(
                    "transaction expected {} to be missing, but it exists",
                    path.display()
                ));
            }
            Ok(Some(permission_record(&metadata.permissions())))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if expected_exists {
                Err(format!(
                    "transaction expected {} to exist, but it is missing",
                    path.display()
                ))
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(format!(
            "failed to inspect transaction destination {}: {error}",
            path.display()
        )),
    }
}

fn load_committed(root: &Path) -> Result<LoadedTransaction, String> {
    let pending = root.join(PENDING_DIR);
    let names = pending_file_names(&pending)?;
    let manifest_bytes = read_bounded_regular_file(
        &pending.join(MANIFEST_FILE),
        MAX_MANIFEST_BYTES,
        "transaction manifest",
    )?;
    let marker_bytes = read_bounded_regular_file(
        &pending.join(COMMIT_FILE),
        MAX_COMMIT_BYTES,
        "transaction commit marker",
    )?;

    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("transaction manifest is invalid: {error}"))?;
    let marker: CommitMarker = serde_json::from_slice(&marker_bytes)
        .map_err(|error| format!("transaction commit marker is invalid: {error}"))?;
    validate_header(&manifest.magic, manifest.version, "manifest")?;
    validate_header(&marker.magic, marker.version, "commit marker")?;
    validate_uuid(&manifest.transaction_id, "manifest transaction id")?;
    validate_uuid(&marker.transaction_id, "commit marker transaction id")?;
    if manifest.transaction_id != marker.transaction_id {
        return Err("transaction manifest and commit marker IDs differ".to_string());
    }
    validate_hash_text(&marker.manifest_sha256, "manifest hash")?;
    if !constant_time_eq(
        marker.manifest_sha256.as_bytes(),
        sha256_hex(&manifest_bytes).as_bytes(),
    ) {
        return Err("transaction manifest hash does not match commit marker".to_string());
    }
    if manifest.entries.is_empty() || manifest.entries.len() > MAX_CHANGES {
        return Err(format!(
            "transaction manifest has invalid entry count {}",
            manifest.entries.len()
        ));
    }

    let root_key = path_key(root);
    let mut paths = HashSet::with_capacity(manifest.entries.len());
    let mut blob_files = HashSet::with_capacity(manifest.entries.len() * 2);
    let mut total_blob_bytes = 0_u64;
    let mut entries = Vec::with_capacity(manifest.entries.len());

    for (position, entry) in manifest.entries.into_iter().enumerate() {
        if entry.index as usize != position {
            return Err(format!(
                "transaction manifest entry {position} has invalid index {}",
                entry.index
            ));
        }
        if entry.path.len() > MAX_PATH_BYTES {
            return Err(format!(
                "transaction manifest entry {position} path is too long"
            ));
        }
        let path = decode_path(&entry.path)?;
        if !path.is_absolute() {
            return Err(format!(
                "transaction manifest entry {position} path is not absolute"
            ));
        }
        let path = clean_absolute(&path)?;
        let key = path_key(&path);
        if path_key_is_within(&key, &root_key) {
            return Err(format!(
                "transaction manifest destination {} is inside its journal",
                path.display()
            ));
        }
        if !paths.insert(key) {
            return Err(format!(
                "transaction manifest contains duplicate destination {}",
                path.display()
            ));
        }

        let before = load_blob(
            &pending,
            position,
            "before",
            entry.before,
            &mut blob_files,
            &mut total_blob_bytes,
        )?;
        let after = load_blob(
            &pending,
            position,
            "after",
            entry.after,
            &mut blob_files,
            &mut total_blob_bytes,
        )?;
        if before.is_none() && entry.permissions.is_some() {
            return Err(format!(
                "transaction manifest entry {position} records permissions for a missing original"
            ));
        }
        if let Some(permissions) = &entry.permissions {
            validate_permission_record(permissions, position)?;
        }

        entries.push(LoadedEntry {
            index: position,
            path,
            before,
            after,
            permissions: entry.permissions,
        });
    }

    let mut expected_names = blob_files;
    expected_names.insert(OsString::from(MANIFEST_FILE));
    expected_names.insert(OsString::from(COMMIT_FILE));
    if names != expected_names {
        let unexpected = names
            .difference(&expected_names)
            .next()
            .or_else(|| expected_names.difference(&names).next())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(format!(
            "transaction journal contains an unexpected or missing file: {unexpected}"
        ));
    }

    Ok(LoadedTransaction {
        transaction_id: marker.transaction_id,
        entries,
    })
}

fn load_blob(
    pending: &Path,
    position: usize,
    kind: &str,
    record: Option<BlobRecord>,
    names: &mut HashSet<OsString>,
    total: &mut u64,
) -> Result<Option<Vec<u8>>, String> {
    let Some(record) = record else {
        return Ok(None);
    };
    let expected_name = format!("{position:08}.{kind}.bin");
    if record.file != expected_name || !valid_leaf_name(&record.file) {
        return Err(format!(
            "transaction manifest entry {position} has invalid {kind} blob name"
        ));
    }
    if record.len > MAX_BLOB_BYTES {
        return Err(format!(
            "transaction blob {} claims {} bytes; maximum is {MAX_BLOB_BYTES}",
            record.file, record.len
        ));
    }
    *total = total
        .checked_add(record.len)
        .ok_or_else(|| "transaction blob size overflow".to_string())?;
    if *total > MAX_TOTAL_BLOB_BYTES {
        return Err(format!(
            "transaction blobs claim {total} bytes; maximum is {MAX_TOTAL_BLOB_BYTES}"
        ));
    }
    validate_hash_text(&record.sha256, "blob hash")?;
    if !names.insert(OsString::from(&record.file)) {
        return Err(format!(
            "transaction journal reuses blob name {}",
            record.file
        ));
    }
    let bytes = read_exact_sized_regular_file(&pending.join(&record.file), record.len)?;
    if !constant_time_eq(record.sha256.as_bytes(), sha256_hex(&bytes).as_bytes()) {
        return Err(format!(
            "transaction blob {} failed its hash check",
            record.file
        ));
    }
    Ok(Some(bytes))
}

fn roll_forward(transaction: &LoadedTransaction) -> Result<(), String> {
    // First validate the entire live set. This avoids making progress when an
    // external edit is already visible on a later destination.
    let mut states = Vec::with_capacity(transaction.entries.len());
    for entry in &transaction.entries {
        let state = read_live_state(&entry.path)?;
        if state == entry.after {
            states.push(LiveState::Target);
        } else if state == entry.before {
            states.push(LiveState::Original);
        } else {
            return Err(format!(
                "transaction destination {} has unexpected external changes; journal retained",
                entry.path.display()
            ));
        }
    }

    for (entry, state) in transaction.entries.iter().zip(states) {
        if state == LiveState::Original {
            publish_entry(transaction, entry)?;
        }
    }
    // Recheck already-published entries too. This catches an external writer
    // changing an earlier destination while a later one was being published;
    // in that case the committed journal stays available rather than being
    // cleaned as if every target were still present.
    for entry in &transaction.entries {
        if read_live_state(&entry.path)? != entry.after {
            return Err(format!(
                "transaction destination {} changed before publication completed; journal retained",
                entry.path.display()
            ));
        }
        ensure_target_permissions(entry)?;
    }
    Ok(())
}

fn ensure_target_permissions(entry: &LoadedEntry) -> Result<(), String> {
    let (Some(_), Some(expected)) = (&entry.after, &entry.permissions) else {
        return Ok(());
    };
    let metadata = fs::symlink_metadata(&entry.path).map_err(|error| {
        format!(
            "failed to inspect transaction destination permissions {}: {error}",
            entry.path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "transaction destination {} is not a regular file",
            entry.path.display()
        ));
    }
    if permissions_match(&metadata.permissions(), expected) {
        return Ok(());
    }
    restore_permissions(&entry.path, &metadata.permissions(), expected)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiveState {
    Original,
    Target,
}

fn publish_entry(transaction: &LoadedTransaction, entry: &LoadedEntry) -> Result<(), String> {
    // Revalidate immediately before publication to narrow the compare/write
    // race. Callers also serialize config mutations, as required by this API.
    let live = read_live_state(&entry.path)?;
    if live == entry.after {
        return Ok(());
    }
    if live != entry.before {
        return Err(format!(
            "transaction destination {} changed immediately before publication; journal retained",
            entry.path.display()
        ));
    }

    match &entry.after {
        Some(bytes) => publish_bytes(transaction, entry, bytes),
        None => publish_delete(entry),
    }
}

fn publish_bytes(
    transaction: &LoadedTransaction,
    entry: &LoadedEntry,
    bytes: &[u8],
) -> Result<(), String> {
    let parent = entry.path.parent().ok_or_else(|| {
        format!(
            "transaction destination {} has no parent directory",
            entry.path.display()
        )
    })?;
    let parent_existed = path_exists_no_follow(parent)?;
    if !parent_existed {
        create_private_dir_all(parent).map_err(|error| {
            format!(
                "failed to create destination directory {}: {error}",
                parent.display()
            )
        })?;
        sync_created_directory_chain(parent)?;
    }
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "failed to inspect destination directory {}: {error}",
            parent.display()
        )
    })?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(format!(
            "destination parent {} is not a real directory",
            parent.display()
        ));
    }

    let temp = publication_temp(&entry.path, &transaction.transaction_id, entry.index)?;
    if path_exists_no_follow(&temp)? {
        let temp_metadata = fs::symlink_metadata(&temp).map_err(|error| {
            format!(
                "failed to inspect publication temp {}: {error}",
                temp.display()
            )
        })?;
        if !temp_metadata.is_file() || temp_metadata.file_type().is_symlink() {
            return Err(format!(
                "publication temp {} is not a regular file",
                temp.display()
            ));
        }
        remove_known_file(&temp)?;
    }

    let write_result = (|| {
        let mut file = open_restricted_new(&temp).map_err(|error| {
            format!(
                "failed to create publication temp {}: {error}",
                temp.display()
            )
        })?;
        file.write_all(bytes).map_err(|error| {
            format!(
                "failed to write publication temp {}: {error}",
                temp.display()
            )
        })?;
        if let Some(permissions) = &entry.permissions {
            apply_permissions(&file, permissions).map_err(|error| {
                format!(
                    "failed to preserve permissions for {}: {error}",
                    entry.path.display()
                )
            })?;
        }
        file.sync_all().map_err(|error| {
            format!(
                "failed to flush publication temp {}: {error}",
                temp.display()
            )
        })?;
        drop(file);

        // The target temp is now ready, so make the last practical compare as
        // close as possible to the atomic replacement. A caller must still
        // serialize its own writers; no portable filesystem API offers an
        // atomic compare-by-content-and-rename operation against other tools.
        let live = read_live_state(&entry.path)?;
        if live == entry.after {
            remove_known_file(&temp)?;
            sync_dir(parent)?;
            return Ok(());
        }
        if live != entry.before {
            return Err(format!(
                "transaction destination {} changed while its replacement was prepared; journal retained",
                entry.path.display()
            ));
        }

        atomic_replace(&temp, &entry.path).map_err(|error| {
            format!(
                "failed to publish transaction destination {}: {error}",
                entry.path.display()
            )
        })?;
        sync_dir(parent)
    })();

    if write_result.is_err() && path_exists_no_follow(&temp).unwrap_or(false) {
        let _ = remove_known_file(&temp);
    }
    write_result
}

fn publish_delete(entry: &LoadedEntry) -> Result<(), String> {
    let live = read_live_state(&entry.path)?;
    if live == entry.after {
        return Ok(());
    }
    if live != entry.before {
        return Err(format!(
            "transaction destination {} changed immediately before deletion; journal retained",
            entry.path.display()
        ));
    }
    remove_regular_file(&entry.path).map_err(|error| {
        format!(
            "failed to delete transaction destination {}: {error}",
            entry.path.display()
        )
    })?;
    let parent = entry.path.parent().ok_or_else(|| {
        format!(
            "transaction destination {} has no parent directory",
            entry.path.display()
        )
    })?;
    sync_dir(parent)
}

fn cleanup_committed(root: &Path) -> Result<(), String> {
    let pending = root.join(PENDING_DIR);
    let completed = root.join(COMPLETED_DIR);
    if path_exists_no_follow(&completed)? {
        return Err(format!(
            "completed transaction cleanup directory {} already exists",
            completed.display()
        ));
    }
    // The rename is the cleanup commit point. Before it, recovery sees the
    // complete committed journal. After it, no pending work remains and the
    // fixed completed directory is safe to clean file-by-file after a crash.
    durable_rename(&pending, &completed).map_err(|error| {
        format!(
            "failed to retire completed transaction journal {}: {error}",
            pending.display()
        )
    })?;
    sync_dir(root)?;
    cleanup_completed(root)
}

fn cleanup_completed(root: &Path) -> Result<(), String> {
    let completed = root.join(COMPLETED_DIR);
    if !path_exists_no_follow(&completed)? {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&completed).map_err(|error| {
        format!(
            "failed to inspect completed journal {}: {error}",
            completed.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "completed transaction journal {} is not a real directory",
            completed.display()
        ));
    }
    let names = journal_file_names(&completed, valid_completed_name)?;
    for name in names {
        remove_known_file(&completed.join(name))?;
    }
    sync_dir(&completed)?;
    remove_known_dir(&completed)?;
    sync_dir(root)
}

fn discard_uncommitted(root: &Path) -> Result<(), String> {
    let pending = root.join(PENDING_DIR);
    if !path_exists_no_follow(&pending)? {
        return Ok(());
    }
    if path_exists_no_follow(&pending.join(COMMIT_FILE))? {
        return Err(format!(
            "refusing to discard committed transaction journal {}",
            pending.display()
        ));
    }
    let names = journal_file_names(&pending, valid_uncommitted_name)?;
    for name in names {
        remove_known_file(&pending.join(&name))?;
    }
    sync_dir(&pending)?;
    remove_known_dir(&pending)?;
    sync_dir(root)
}

fn pending_file_names(pending: &Path) -> Result<HashSet<OsString>, String> {
    journal_file_names(pending, |_| true)
}

fn journal_file_names(
    directory: &Path,
    allowed_name: impl Fn(&OsString) -> bool,
) -> Result<HashSet<OsString>, String> {
    let mut names = HashSet::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to read journal {}: {error}", directory.display()))?;
    for item in entries {
        let item = item
            .map_err(|error| format!("failed to read journal {}: {error}", directory.display()))?;
        if names.len() >= MAX_PENDING_FILES {
            return Err(format!(
                "transaction journal {} contains too many entries",
                directory.display()
            ));
        }
        let metadata = item.file_type().map_err(|error| {
            format!(
                "failed to inspect journal entry {}: {error}",
                item.path().display()
            )
        })?;
        if !metadata.is_file() || metadata.is_symlink() {
            return Err(format!(
                "transaction journal entry {} is not a regular file",
                item.path().display()
            ));
        }
        let name = item.file_name();
        if !allowed_name(&name) {
            return Err(format!(
                "transaction journal contains unexpected entry {}; refusing cleanup",
                name.to_string_lossy()
            ));
        }
        let opened = open_read_no_follow(&item.path()).map_err(|error| {
            format!(
                "failed to safely open journal entry {}: {error}",
                item.path().display()
            )
        })?;
        if !opened
            .metadata()
            .map_err(|error| {
                format!(
                    "failed to inspect open journal entry {}: {error}",
                    item.path().display()
                )
            })?
            .is_file()
        {
            return Err(format!(
                "transaction journal entry {} changed while it was inspected",
                item.path().display()
            ));
        }
        names.insert(name);
    }
    Ok(names)
}

fn valid_uncommitted_name(name: &OsString) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if matches!(name, MANIFEST_FILE | MANIFEST_TEMP | COMMIT_TEMP) {
        return true;
    }
    let Some((index, suffix)) = name.split_once('.') else {
        return false;
    };
    index.len() == 8
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(suffix, "before.bin" | "after.bin")
        && index
            .parse::<usize>()
            .is_ok_and(|index| index < MAX_CHANGES)
}

fn valid_completed_name(name: &OsString) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if matches!(name, MANIFEST_FILE | COMMIT_FILE) {
        return true;
    }
    let Some((index, suffix)) = name.split_once('.') else {
        return false;
    };
    index.len() == 8
        && index.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(suffix, "before.bin" | "after.bin")
        && index
            .parse::<usize>()
            .is_ok_and(|index| index < MAX_CHANGES)
}

fn read_live_state(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to inspect transaction destination {}: {error}",
                path.display()
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "transaction destination {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > MAX_BLOB_BYTES {
        return Err(format!(
            "transaction destination {} is {} bytes; maximum supported size is {MAX_BLOB_BYTES}",
            path.display(),
            metadata.len()
        ));
    }
    read_exact_sized_regular_file(path, metadata.len()).map(Some)
}

fn read_exact_sized_regular_file(path: &Path, expected_len: u64) -> Result<Vec<u8>, String> {
    if expected_len > MAX_BLOB_BYTES {
        return Err(format!(
            "transaction file {} claims too many bytes ({expected_len})",
            path.display()
        ));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "failed to inspect transaction file {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "transaction file {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() != expected_len {
        return Err(format!(
            "transaction file {} has length {}, expected {expected_len}",
            path.display(),
            metadata.len()
        ));
    }
    let capacity = usize::try_from(expected_len)
        .map_err(|_| format!("transaction file {} is too large", path.display()))?;
    let file = open_read_no_follow(path).map_err(|error| {
        format!(
            "failed to open transaction file {}: {error}",
            path.display()
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(expected_len.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "failed to read transaction file {}: {error}",
                path.display()
            )
        })?;
    if bytes.len() as u64 != expected_len {
        return Err(format!(
            "transaction file {} changed while it was read",
            path.display()
        ));
    }
    Ok(bytes)
}

fn read_bounded_regular_file(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {label} {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} {} is not a regular file", path.display()));
    }
    if metadata.len() > maximum {
        return Err(format!(
            "{label} {} is {} bytes; maximum is {maximum}",
            path.display(),
            metadata.len()
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| format!("{label} {} is too large", path.display()))?;
    let file = open_read_no_follow(path)
        .map_err(|error| format!("failed to open {label} {}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read {label} {}: {error}", path.display()))?;
    if bytes.len() as u64 > maximum || bytes.len() as u64 != metadata.len() {
        return Err(format!(
            "{label} {} changed while it was read",
            path.display()
        ));
    }
    Ok(bytes)
}

fn write_restricted_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = open_restricted_new(path).map_err(|error| {
        format!(
            "failed to create transaction file {}: {error}",
            path.display()
        )
    })?;
    file.write_all(bytes).map_err(|error| {
        format!(
            "failed to write transaction file {}: {error}",
            path.display()
        )
    })?;
    file.sync_all().map_err(|error| {
        format!(
            "failed to flush transaction file {}: {error}",
            path.display()
        )
    })
}

#[cfg(unix)]
fn open_read_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    // O_NOFOLLOW refuses a final-component symlink swapped in after the
    // metadata check. These values are stable on the Unix families supported
    // by Rust; unknown Unix targets retain the metadata validation fallback.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const O_NOFOLLOW: i32 = 0o400000;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    const O_NOFOLLOW: i32 = 0x0000_0100;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    const O_NOFOLLOW: i32 = 0;

    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_no_follow(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn open_restricted_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_restricted_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

#[cfg(unix)]
fn create_private_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

fn remove_known_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect journal file {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "refusing to remove non-file journal entry {}",
            path.display()
        ));
    }
    remove_regular_file(path)
        .map_err(|error| format!("failed to remove journal file {}: {error}", path.display()))
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn remove_regular_file(path: &Path) -> io::Result<()> {
    let original_permissions = fs::metadata(path)?.permissions();
    let was_readonly = original_permissions.readonly();
    if was_readonly {
        let mut writable = original_permissions.clone();
        writable.set_readonly(false);
        fs::set_permissions(path, writable)?;
    }
    if let Err(remove_error) = fs::remove_file(path) {
        if was_readonly {
            if let Err(restore_error) = fs::set_permissions(path, original_permissions) {
                return Err(io::Error::other(format!(
                    "removal failed ({remove_error}) and restoring read-only permissions failed ({restore_error})"
                )));
            }
        }
        return Err(remove_error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn remove_regular_file(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

fn remove_known_dir(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "failed to inspect journal directory {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "refusing to remove non-directory journal entry {}",
            path.display()
        ));
    }
    fs::remove_dir(path).map_err(|error| {
        format!(
            "failed to remove journal directory {}: {error}",
            path.display()
        )
    })
}

fn validate_header(magic: &str, version: u32, label: &str) -> Result<(), String> {
    if magic != JOURNAL_MAGIC {
        return Err(format!("transaction {label} has invalid magic"));
    }
    if version != JOURNAL_VERSION {
        return Err(format!(
            "transaction {label} version {version} is unsupported; expected {JOURNAL_VERSION}"
        ));
    }
    Ok(())
}

fn validate_uuid(value: &str, label: &str) -> Result<(), String> {
    let parsed = Uuid::parse_str(value).map_err(|_| format!("{label} is invalid"))?;
    if parsed.to_string() != value {
        return Err(format!("{label} is not canonical"));
    }
    Ok(())
}

fn validate_hash_text(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("transaction {label} is invalid"));
    }
    Ok(())
}

fn valid_leaf_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

fn path_exists_no_follow(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect {}: {error}", path.display())),
    }
}

fn absolute_clean(path: &Path, label: &str) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{label} path is empty"));
    }
    if path.is_absolute() {
        clean_absolute(path)
    } else {
        let current = std::env::current_dir()
            .map_err(|error| format!("failed to resolve current directory: {error}"))?;
        clean_absolute(&current.join(path))
    }
}

fn clean_absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("path {} is not absolute", path.display()));
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                clean.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    return Err(format!(
                        "path {} escapes its filesystem root",
                        path.display()
                    ));
                }
            }
        }
    }
    if !clean.is_absolute() {
        return Err(format!(
            "path {} did not resolve to an absolute path",
            path.display()
        ));
    }
    Ok(clean)
}

#[cfg(windows)]
type PathKey = String;

#[cfg(not(windows))]
type PathKey = PathBuf;

#[cfg(windows)]
fn path_key(path: &Path) -> PathKey {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[cfg(not(windows))]
fn path_key(path: &Path) -> PathKey {
    path.to_path_buf()
}

#[cfg(windows)]
fn path_key_is_within(path: &PathKey, root: &PathKey) -> bool {
    if path == root {
        return true;
    }
    if root.ends_with('/') {
        path.starts_with(root)
    } else {
        path.strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

#[cfg(not(windows))]
fn path_key_is_within(path: &PathKey, root: &PathKey) -> bool {
    path.starts_with(root)
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;
    Ok(format!("unix:{}", hex_encode(path.as_os_str().as_bytes())))
}

#[cfg(unix)]
fn decode_path(value: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    let encoded = value
        .strip_prefix("unix:")
        .ok_or_else(|| "transaction manifest path has invalid encoding".to_string())?;
    let bytes = hex_decode(encoded)?;
    if bytes.contains(&0) {
        return Err("transaction manifest path contains a NUL byte".to_string());
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(windows)]
fn encode_path(path: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(format!("windows-utf16le:{}", hex_encode(&bytes)))
}

#[cfg(windows)]
fn decode_path(value: &str) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    let encoded = value
        .strip_prefix("windows-utf16le:")
        .ok_or_else(|| "transaction manifest path has invalid encoding".to_string())?;
    let bytes = hex_decode(encoded)?;
    if !bytes.len().is_multiple_of(2) {
        return Err("transaction manifest path has truncated UTF-16".to_string());
    }
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let (pairs, remainder) = bytes.as_slice().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for pair in pairs {
        units.push(u16::from_le_bytes([pair[0], pair[1]]));
    }
    if units.contains(&0) {
        return Err("transaction manifest path contains a NUL unit".to_string());
    }
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) || value.len() > MAX_PATH_BYTES * 2 {
        return Err("transaction manifest path has invalid hex length".to_string());
    }
    let mut output = Vec::with_capacity(value.len() / 2);
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for pair in pairs {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("transaction manifest path contains invalid hex".to_string()),
    }
}

fn publication_temp(path: &Path, transaction_id: &str, index: usize) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or_else(|| {
        format!(
            "transaction destination {} has no file name",
            path.display()
        )
    })?;
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".zroutery-{transaction_id}-{index}.tmp"));
    Ok(path.with_file_name(temp_name))
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    if path_exists_io(destination)? {
        // Windows refuses to replace a read-only destination. The replacement
        // temp already carries the captured read-only attribute, so clear only
        // the old file's attribute for the atomic call. If the call fails,
        // restore it; if the process crashes in this tiny window, the committed
        // journal still sees the original bytes and recovery reapplies the
        // snapshot.
        let original_permissions = fs::metadata(destination)?.permissions();
        let was_readonly = original_permissions.readonly();
        if was_readonly {
            let mut writable = original_permissions.clone();
            writable.set_readonly(false);
            fs::set_permissions(destination, writable)?;
        }
        let result = move_file_replace_windows(source, destination);
        if let Err(replace_error) = result {
            if was_readonly {
                if let Err(restore_error) = fs::set_permissions(destination, original_permissions) {
                    return Err(io::Error::other(format!(
                        "replacement failed ({replace_error}) and restoring read-only permissions failed ({restore_error})"
                    )));
                }
            }
            return Err(replace_error);
        }
        fs::set_permissions(destination, original_permissions)?;
        Ok(())
    } else {
        move_file_replace_windows(source, destination)
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn path_exists_io(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn move_file_replace_windows(source: &Path, destination: &Path) -> io::Result<()> {
    // Keep the native move independent of the read-only attribute on either
    // side. The caller reapplies the captured destination attribute after the
    // atomic replacement, and this function restores the temp on failure.
    let replacement_permissions = fs::metadata(source)?.permissions();
    let replacement_was_readonly = replacement_permissions.readonly();
    if replacement_was_readonly {
        let mut writable = replacement_permissions.clone();
        writable.set_readonly(false);
        fs::set_permissions(source, writable)?;
    }

    if let Err(replace_error) = durable_rename(source, destination) {
        if replacement_was_readonly {
            let _ = fs::set_permissions(source, replacement_permissions);
        }
        return Err(replace_error);
    }
    Ok(())
}

#[cfg(windows)]
fn durable_rename(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "Kernel32")]
    extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // REPLACE_EXISTING | WRITE_THROUGH. The buffers are NUL-terminated and
    // owned through the call; all journals and destinations are local paths.
    let result = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0x1 | 0x8) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn durable_rename(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn sync_dir(path: &Path) -> Result<(), String> {
    match File::open(path) {
        Ok(directory) => match directory.sync_all() {
            Ok(()) => Ok(()),
            Err(error) if directory_sync_unsupported(&error) => Ok(()),
            Err(error) => Err(format!(
                "failed to flush directory {}: {error}",
                path.display()
            )),
        },
        Err(error) if directory_sync_unsupported(&error) => Ok(()),
        Err(error) => Err(format!(
            "failed to open directory {} for flush: {error}",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn directory_sync_unsupported(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    )
}

#[cfg(not(unix))]
fn directory_sync_unsupported(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    )
}

fn sync_created_directory_chain(path: &Path) -> Result<(), String> {
    // Flush every directory whose child entry may have been created, stopping
    // before the filesystem root (Windows cannot open a drive root this way).
    let mut directory = path;
    while let Some(parent) = directory.parent() {
        sync_dir(directory)?;
        directory = parent;
    }
    Ok(())
}

fn permission_record(permissions: &Permissions) -> PermissionRecord {
    PermissionRecord {
        readonly: permissions.readonly(),
        #[cfg(unix)]
        unix_mode: {
            use std::os::unix::fs::PermissionsExt;
            permissions.mode() & 0o7777
        },
    }
}

#[cfg(unix)]
fn validate_permission_record(record: &PermissionRecord, position: usize) -> Result<(), String> {
    if record.unix_mode & !0o7777 != 0 || record.readonly != (record.unix_mode & 0o222 == 0) {
        return Err(format!(
            "transaction manifest entry {position} has invalid Unix permissions"
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_permission_record(_record: &PermissionRecord, _position: usize) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn permissions_match(actual: &Permissions, expected: &PermissionRecord) -> bool {
    use std::os::unix::fs::PermissionsExt;
    actual.mode() & 0o7777 == expected.unix_mode
}

#[cfg(not(unix))]
fn permissions_match(actual: &Permissions, expected: &PermissionRecord) -> bool {
    actual.readonly() == expected.readonly
}

#[cfg(unix)]
fn apply_permissions(file: &File, record: &PermissionRecord) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(Permissions::from_mode(record.unix_mode))
}

#[cfg(not(unix))]
fn apply_permissions(file: &File, record: &PermissionRecord) -> io::Result<()> {
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(record.readonly);
    file.set_permissions(permissions)
}

#[cfg(unix)]
fn restore_permissions(
    path: &Path,
    _actual: &Permissions,
    expected: &PermissionRecord,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, Permissions::from_mode(expected.unix_mode)).map_err(|error| {
        format!(
            "failed to restore permissions for transaction destination {}: {error}",
            path.display()
        )
    })?;
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            format!(
                "failed to flush restored permissions for transaction destination {}: {error}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn restore_permissions(
    path: &Path,
    actual: &Permissions,
    expected: &PermissionRecord,
) -> Result<(), String> {
    let mut permissions = actual.clone();
    permissions.set_readonly(expected.readonly);
    fs::set_permissions(path, permissions).map_err(|error| {
        format!(
            "failed to restore permissions for transaction destination {}: {error}",
            path.display()
        )
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

// Minimal SHA-256 implementation kept local so the transaction format has a
// stable cryptographic checksum without expanding the crate's dependency API.
fn sha256_hex(bytes: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut hash = INITIAL;
    let (chunks, remainder) = padded.as_slice().as_chunks::<64>();
    debug_assert!(remainder.is_empty());
    for chunk in chunks {
        let mut words = [0_u32; 64];
        let (word_bytes, remainder) = chunk.as_slice().as_chunks::<4>();
        debug_assert!(remainder.is_empty());
        for (index, bytes) in word_bytes.iter().enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for index in 0..64 {
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sigma1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sigma0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in hash.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut output = String::with_capacity(64);
    for word in hash {
        output.push_str(&format!("{word:08x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(value: &str) -> Option<Vec<u8>> {
        Some(value.as_bytes().to_vec())
    }

    fn change(path: PathBuf, before: Option<&str>, after: Option<&str>) -> FileChange {
        FileChange {
            path,
            before: before.map(|value| value.as_bytes().to_vec()),
            after: after.map(|value| value.as_bytes().to_vec()),
        }
    }

    fn stage_and_mark(root: &Path, changes: &[FileChange]) -> LoadedTransaction {
        let lock = JournalLock::acquire(root).unwrap();
        let changes = normalize_changes(&lock.root, changes).unwrap();
        let prepared = stage_transaction(&lock.root, &changes, &StageFault::default()).unwrap();
        write_commit_marker(&lock.root, &prepared).unwrap();
        load_committed(&lock.root).unwrap()
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn commit_updates_creates_deletes_and_distinguishes_empty() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let update = temp.path().join("update.json");
        let delete = temp.path().join("delete.json");
        let create = temp.path().join("nested/create.json");
        let empty = temp.path().join("empty.json");
        fs::write(&update, b"old").unwrap();
        fs::write(&delete, b"gone").unwrap();

        commit(
            &journal,
            &[
                change(update.clone(), Some("old"), Some("new")),
                change(delete.clone(), Some("gone"), None),
                change(create.clone(), None, Some("made")),
                FileChange {
                    path: empty.clone(),
                    before: None,
                    after: Some(Vec::new()),
                },
            ],
        )
        .unwrap();

        assert_eq!(fs::read(update).unwrap(), b"new");
        assert!(!delete.exists());
        assert_eq!(fs::read(create).unwrap(), b"made");
        assert_eq!(fs::read(empty).unwrap(), b"");
        assert!(!journal.join(PENDING_DIR).exists());
    }

    #[test]
    fn sequential_commits_create_then_replace_the_same_destination() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");

        commit(&journal, &[change(file.clone(), None, Some("first"))]).unwrap();
        commit(
            &journal,
            &[change(file.clone(), Some("first"), Some("second"))],
        )
        .unwrap();

        assert_eq!(fs::read(file).unwrap(), b"second");
        assert!(!journal.join(PENDING_DIR).exists());
        assert!(!journal.join(COMPLETED_DIR).exists());
    }

    #[test]
    fn no_op_files_still_guard_the_transaction_against_external_edits() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let config = temp.path().join("config.json");
        let unchanged = temp.path().join("catalogue.json");
        fs::write(&config, b"old").unwrap();
        fs::write(&unchanged, b"external").unwrap();
        assert!(commit(
            &journal,
            &[
                change(config.clone(), Some("old"), Some("new")),
                change(unchanged.clone(), Some("expected"), Some("expected")),
            ]
        )
        .is_err());
        assert_eq!(fs::read(&config).unwrap(), b"old");
        fs::write(&unchanged, b"expected").unwrap();
        let transaction = stage_and_mark(
            &journal,
            &[
                change(config.clone(), Some("old"), Some("new")),
                change(unchanged.clone(), Some("expected"), Some("expected")),
            ],
        );
        assert_eq!(transaction.entries.len(), 2);
        fs::write(&unchanged, b"changed after marker").unwrap();
        assert!(recover(&journal).is_err());
        assert_eq!(fs::read(&config).unwrap(), b"old");
    }

    #[test]
    fn recovery_discards_crash_before_commit_without_live_changes() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"before").unwrap();
        {
            let lock = JournalLock::acquire(&journal).unwrap();
            let changes = normalize_changes(
                &lock.root,
                &[change(file.clone(), Some("before"), Some("after"))],
            )
            .unwrap();
            stage_transaction(&lock.root, &changes, &StageFault::default()).unwrap();
        }

        recover(&journal).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"before");
        assert!(!journal.join(PENDING_DIR).exists());
    }

    #[test]
    fn recovery_rolls_forward_after_crash_between_publishes_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let first = temp.path().join("first.json");
        let second = temp.path().join("second.json");
        fs::write(&first, b"old-1").unwrap();
        fs::write(&second, b"old-2").unwrap();
        {
            let transaction = stage_and_mark(
                &journal,
                &[
                    change(first.clone(), Some("old-1"), Some("new-1")),
                    change(second.clone(), Some("old-2"), Some("new-2")),
                ],
            );
            publish_entry(&transaction, &transaction.entries[0]).unwrap();
        }

        recover(&journal).unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"new-1");
        assert_eq!(fs::read(&second).unwrap(), b"new-2");
        recover(&journal).unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"new-1");
        assert_eq!(fs::read(&second).unwrap(), b"new-2");
    }

    #[test]
    fn recovery_cleans_all_published_journal_leftover() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        {
            let transaction =
                stage_and_mark(&journal, &[change(file.clone(), Some("old"), Some("new"))]);
            roll_forward(&transaction).unwrap();
        }
        assert!(journal.join(PENDING_DIR).exists());

        recover(&journal).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"new");
        assert!(!journal.join(PENDING_DIR).exists());
    }

    #[test]
    fn recovery_discards_retired_cleanup_journal_without_touching_live_file() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        {
            let transaction =
                stage_and_mark(&journal, &[change(file.clone(), Some("old"), Some("new"))]);
            roll_forward(&transaction).unwrap();
            fs::rename(journal.join(PENDING_DIR), journal.join(COMPLETED_DIR)).unwrap();
        }

        recover(&journal).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"new");
        assert!(!journal.join(PENDING_DIR).exists());
        assert!(!journal.join(COMPLETED_DIR).exists());
    }

    #[test]
    fn external_edit_retains_recoverable_journal_and_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        let _transaction =
            stage_and_mark(&journal, &[change(file.clone(), Some("old"), Some("new"))]);
        fs::write(&file, b"user edit").unwrap();

        let error = recover(&journal).unwrap_err();
        assert!(error.contains("external changes"), "{error}");
        assert_eq!(fs::read(&file).unwrap(), b"user edit");
        assert!(journal.join(PENDING_DIR).join(COMMIT_FILE).exists());

        fs::write(&file, b"old").unwrap();
        recover(&journal).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"new");
    }

    #[test]
    fn staging_failure_is_cleaned_and_live_file_is_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        let lock = JournalLock::acquire(&journal).unwrap();
        let changes = normalize_changes(
            &lock.root,
            &[change(file.clone(), Some("old"), Some("new"))],
        )
        .unwrap();
        let error = commit_locked(
            &lock.root,
            &changes
                .iter()
                .map(|change| FileChange {
                    path: change.path.clone(),
                    before: change.before.clone(),
                    after: change.after.clone(),
                })
                .collect::<Vec<_>>(),
            StageFault {
                fail_after_blobs: Some(1),
            },
        )
        .unwrap_err();
        assert!(error.contains("injected staging failure"), "{error}");
        assert_eq!(fs::read(file).unwrap(), b"old");
        assert!(!journal.join(PENDING_DIR).exists());
    }

    #[test]
    fn rejects_duplicate_destinations_before_staging() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        let error = commit(
            &journal,
            &[
                change(file.clone(), None, Some("one")),
                change(file, None, Some("two")),
            ],
        )
        .unwrap_err();
        assert!(error.contains("duplicate destination"), "{error}");
    }

    #[test]
    fn corrupt_committed_blob_is_rejected_before_live_writes() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let first = temp.path().join("first.json");
        let second = temp.path().join("second.json");
        fs::write(&first, b"old-1").unwrap();
        fs::write(&second, b"old-2").unwrap();
        let _transaction = stage_and_mark(
            &journal,
            &[
                change(first.clone(), Some("old-1"), Some("new-1")),
                change(second.clone(), Some("old-2"), Some("new-2")),
            ],
        );
        fs::write(
            journal.join(PENDING_DIR).join("00000001.after.bin"),
            b"broken",
        )
        .unwrap();

        let error = recover(&journal).unwrap_err();
        assert!(
            error.contains("length") || error.contains("hash check"),
            "{error}"
        );
        assert_eq!(fs::read(first).unwrap(), b"old-1");
        assert_eq!(fs::read(second).unwrap(), b"old-2");
        assert!(journal.join(PENDING_DIR).exists());
    }

    #[test]
    fn uncommitted_cleanup_refuses_unknown_entries_without_removing_known_stage() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        {
            let lock = JournalLock::acquire(&journal).unwrap();
            let changes = normalize_changes(
                &lock.root,
                &[change(file.clone(), Some("old"), Some("new"))],
            )
            .unwrap();
            stage_transaction(&lock.root, &changes, &StageFault::default()).unwrap();
            fs::write(lock.root.join(PENDING_DIR).join("unrelated.txt"), b"keep").unwrap();
        }

        let error = recover(&journal).unwrap_err();
        assert!(error.contains("unexpected entry"), "{error}");
        assert_eq!(fs::read(file).unwrap(), b"old");
        assert!(journal.join(PENDING_DIR).join(MANIFEST_FILE).exists());
        assert!(journal.join(PENDING_DIR).join("unrelated.txt").exists());
    }

    #[test]
    fn lock_is_exclusive_but_not_stale() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let first = JournalLock::acquire(&journal).unwrap();
        let error = JournalLock::acquire(&journal).err().unwrap();
        assert!(error.contains("already locked"), "{error}");
        drop(first);
        JournalLock::acquire(&journal).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_original_unix_permissions_and_stage_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        fs::set_permissions(&file, Permissions::from_mode(0o640)).unwrap();
        {
            let _transaction =
                stage_and_mark(&journal, &[change(file.clone(), Some("old"), Some("new"))]);
            let blob_mode = fs::metadata(journal.join(PENDING_DIR).join("00000000.after.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(blob_mode, 0o600);
        }
        recover(&journal).unwrap();
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(windows)]
    #[test]
    fn replacement_preserves_original_windows_readonly_attribute() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();

        commit(&journal, &[change(file.clone(), Some("old"), Some("new"))]).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new");
        assert!(fs::metadata(&file).unwrap().permissions().readonly());
    }

    #[cfg(windows)]
    #[test]
    fn deletion_handles_an_original_windows_readonly_file() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"old").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();

        commit(&journal, &[change(file.clone(), Some("old"), None)]).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn expected_live_bytes_are_validated_before_commit_marker() {
        let temp = tempfile::tempdir().unwrap();
        let journal = temp.path().join("journal");
        let file = temp.path().join("config.json");
        fs::write(&file, b"actual").unwrap();
        let error = commit(
            &journal,
            &[FileChange {
                path: file.clone(),
                before: bytes("expected"),
                after: bytes("target"),
            }],
        )
        .unwrap_err();
        assert!(error.contains("expected before bytes"), "{error}");
        assert_eq!(fs::read(file).unwrap(), b"actual");
        assert!(!journal.join(PENDING_DIR).exists());
    }
}
