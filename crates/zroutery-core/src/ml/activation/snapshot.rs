use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ml::model::ModelState;
use crate::ml::model_identity::{
    CommitId, ModelCheckpoint, ModelCommit, ModelId, ModelStore, ReplayEngine,
    LEGACY_UNVERSIONED_SCHEMA_VERSION, MODEL_CHECKPOINT_SCHEMA_VERSION,
    MODEL_COMMIT_SCHEMA_VERSION,
};

use super::records::ActivationError;
use super::{
    ACTIVATION_POINTER_NAME, ACTIVATION_POINTER_SCHEMA_VERSION, ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
    FIRST_GENERATION, FNV_OFFSET_BASIS, FNV_PRIME, HEX_DIGITS, POINTER_DOMAIN, SNAPSHOT_DOMAIN,
    SNAPSHOT_ID_HEX_DIGITS, SNAPSHOT_ID_PREFIX,
};

// ---------------------------------------------------------------------------
// Checksum
// ---------------------------------------------------------------------------

pub(super) fn hash_extend(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

pub(super) fn hash_string(hash: &mut u64, value: &str) {
    hash_extend(hash, &(value.len() as u64).to_le_bytes());
    hash_extend(hash, value.as_bytes());
}

fn hash_optional(hash: &mut u64, value: Option<&str>) {
    match value {
        Some(text) => {
            hash_extend(hash, &[1]);
            hash_string(hash, text);
        }
        None => hash_extend(hash, &[0]),
    }
}

pub(super) fn format_checksum(checksum: u64) -> String {
    let mut text = String::with_capacity(SNAPSHOT_ID_HEX_DIGITS);
    for index in (0..SNAPSHOT_ID_HEX_DIGITS).rev() {
        let nibble = ((checksum >> (index * 4)) & 0xf) as usize;
        text.push(char::from(HEX_DIGITS[nibble]));
    }
    text
}

// ---------------------------------------------------------------------------
// SnapshotId
// ---------------------------------------------------------------------------

/// The content-addressed name of an immutable snapshot.
///
/// It is always `snap-` followed by [`SNAPSHOT_ID_HEX_DIGITS`] lowercase hex
/// digits, and it is checked at **every** entry point that turns one into a
/// path, because a name that reached [`Path::join`] unchecked would be a path
/// traversal. The constructor is infallible for source compatibility with the
/// accepted `CommitId::new`; the form is enforced by use, not by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Wrap a name. It is validated before it is ever used as a path.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this name is exactly the well-formed form.
    pub fn is_well_formed(&self) -> bool {
        let Some(suffix) = self.0.strip_prefix(SNAPSHOT_ID_PREFIX) else {
            return false;
        };
        suffix.len() == SNAPSHOT_ID_HEX_DIGITS
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A validated id, or a typed refusal naming what was wrong with it.
pub(super) fn checked_id(id: &SnapshotId) -> Result<&SnapshotId, ActivationError> {
    if id.is_well_formed() {
        Ok(id)
    } else {
        Err(ActivationError::MalformedSnapshotId { id: id.0.clone() })
    }
}

// ---------------------------------------------------------------------------
// Snapshot identity
// ---------------------------------------------------------------------------

/// The checksum a snapshot name is derived from.
///
/// It covers every model-bearing byte: the snapshot envelope version, the
/// commit's envelope version, its content-addressed id, the checkpoint's own
/// content hash, the feature schema, the cumulative learning-event count, the
/// parent reference, and the model lineage. `created_at` and commit metadata are
/// excluded for the reason the accepted identity excludes them — they are
/// persistence detail, not model content.
///
/// Public because a second implementation must be able to re-derive and check
/// the name. It is an accidental-corruption hash, not a MAC.
pub fn snapshot_checksum(commit: &ModelCommit) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, SNAPSHOT_DOMAIN);
    hash_extend(&mut hash, &ACTIVATION_SNAPSHOT_SCHEMA_VERSION.to_le_bytes());
    hash_extend(&mut hash, &commit.schema_version.to_le_bytes());
    hash_string(&mut hash, commit.commit_id.as_str());
    hash_extend(&mut hash, &commit.checkpoint.content_hash().to_le_bytes());
    hash_extend(&mut hash, &commit.feature_schema_version.to_le_bytes());
    hash_extend(&mut hash, &commit.learning_event_count.to_le_bytes());
    hash_optional(&mut hash, commit.parent.as_ref().map(CommitId::as_str));
    hash_string(&mut hash, commit.model_id.as_str());
    hash
}

/// The name a given commit's snapshot is written under.
///
/// Two different contents cannot reach the same name through the ordinary path,
/// and if they somehow did, the loader re-derives the name *and* the accepted
/// `ModelCommit::verify` requires `commit_id == canonical_identity()`, so at
/// most one content can verify under a name: a collision is
/// [`ActivationError::IdentityMismatch`], never a wrong artifact read.
pub fn snapshot_id_for(commit: &ModelCommit) -> SnapshotId {
    SnapshotId::new(format!(
        "{SNAPSHOT_ID_PREFIX}{}",
        format_checksum(snapshot_checksum(commit))
    ))
}

// ---------------------------------------------------------------------------
// The validating wire form
// ---------------------------------------------------------------------------

/// The stored form of one snapshot.
///
/// `Deserialize` is a derive because a stored artifact is untrusted input. It
/// is **not** a way in: [`SnapshotFile::try_into_snapshot`] re-runs every check
/// the write path runs, re-validating through the accepted `ModelCommit::verify`
/// and `ReplayEngine::verify_checkpoint_integrity`, refusing the accepted
/// envelope versions, and re-deriving the content address.
///
/// The commit is carried as [`CommitFile`], not as a `ModelCommit`, because of
/// the reason in that type's documentation: a plain JSON encoding of a
/// checkpoint did not survive a round trip when this module was written, and
/// this form is guaranteed to rather than incidentally so.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    /// The snapshot envelope version.
    pub schema_version: u32,
    /// The name the writer claims. Re-derived and compared on load.
    pub snapshot_id: String,
    /// The complete immutable commit, bit-exactly.
    pub commit: CommitFile,
    /// Wall-clock time the snapshot was written. Informational.
    pub created_at: i64,
}

impl SnapshotFile {
    /// Rebuild a snapshot, re-running every validation the write path runs.
    pub fn try_into_snapshot(&self, claimed: &SnapshotId) -> Result<Snapshot, ActivationError> {
        if self.schema_version != ACTIVATION_SNAPSHOT_SCHEMA_VERSION {
            return Err(ActivationError::UnsupportedSchema {
                component: "snapshot envelope".to_string(),
                version: self.schema_version,
                supported: ACTIVATION_SNAPSHOT_SCHEMA_VERSION,
            });
        }
        // The content is the authority. The declared name *and* the name the
        // caller asked for must both be the address the rebuilt content derives:
        // comparing only the header against the requested name — which a
        // rewritten header can be made to agree with — would let one legal
        // snapshot's content load under another snapshot's name. This is a
        // missing comparison, not a defence against a hash collision.
        let commit = self.rebuilt_commit()?;
        let derived = snapshot_id_for(&commit);
        if self.snapshot_id != claimed.as_str() || derived != *claimed {
            return Err(ActivationError::IdentityMismatch {
                snapshot: claimed.as_str().to_string(),
                stored: self.snapshot_id.clone(),
                derived: derived.as_str().to_string(),
            });
        }
        verify_commit(&commit)?;
        Ok(Snapshot {
            id: claimed.clone(),
            commit,
            created_at: self.created_at,
        })
    }

    /// Rebuild the accepted commit from the bit-exact wire form.
    ///
    /// This is the whole reason this module has a wire form: the accepted types
    /// stay the validators, and this is only the transport.
    fn rebuilt_commit(&self) -> Result<ModelCommit, ActivationError> {
        Ok(ModelCommit {
            schema_version: self.commit.schema_version,
            commit_id: CommitId::new(self.commit.commit_id.clone()),
            parent: self.commit.parent.clone().map(CommitId::new),
            model_id: ModelId::new(self.commit.model_id.clone()),
            checkpoint: self.commit.checkpoint.rebuilt()?,
            learning_event_count: self.commit.learning_event_count,
            algorithm_versions: self.commit.algorithm_versions.clone(),
            feature_schema_version: self.commit.feature_schema_version,
            created_at: self.commit.created_at,
            metadata: self
                .commit
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        })
    }
}

/// The stored form of a commit, in which the checkpoint is bit-exact.
///
/// # Why this type exists at all
///
/// It existed because a `ModelCheckpoint` **could not** be written as ordinary
/// JSON here and then verified again. `serde_json` was used without its
/// `float_roundtrip` feature, so its float parsing was a fast path that is not
/// correctly rounded: a trained ensemble's `f64` parameters came back **1–2 ULP
/// different** after a round trip. The accepted `ModelState::verify_checksum`
/// hashes `f64::to_bits`, and the accepted commit identity hashes the
/// checkpoint's content hash, so a round-tripped checkpoint failed both — a
/// snapshot stored as plain JSON refused to load itself.
///
/// **That is no longer true of this workspace.** `float_roundtrip` is enabled
/// workspace-wide, so ordinary JSON is lossless here and this type is
/// redundancy rather than necessity. It is kept because it is guaranteed
/// lossless by construction rather than by the current configuration of a
/// dependency, and because it carries the schema envelope the bare commit does
/// not.
///
/// Writing the parameters as 16 hex digits of their IEEE-754 bits removes the
/// decimal round trip entirely, and it is a transport change only: the values are
/// rebuilt into the accepted [`ModelCheckpoint`] and then verified by the
/// accepted checks, which remain the only validators in this path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFile {
    /// The accepted commit envelope version, refused unless it is current.
    pub schema_version: u32,
    /// The accepted content-addressed commit id.
    pub commit_id: String,
    /// The lineage parent, if any.
    pub parent: Option<String>,
    /// The model lineage.
    pub model_id: String,
    /// The four model states, bit-exactly.
    pub checkpoint: CheckpointFile,
    /// The cumulative learning-event count.
    pub learning_event_count: u64,
    /// The algorithm table.
    pub algorithm_versions: Vec<(String, String)>,
    /// The accepted feature schema version.
    pub feature_schema_version: u32,
    /// Wall-clock creation time. Not part of the identity.
    pub created_at: i64,
    /// Stable metadata, carried as an ordered map so the bytes are reproducible.
    pub metadata: BTreeMap<String, String>,
}

impl CommitFile {
    /// The wire form of a verified commit.
    pub fn from_commit(commit: &ModelCommit) -> Self {
        Self {
            schema_version: commit.schema_version,
            commit_id: commit.commit_id.as_str().to_string(),
            parent: commit
                .parent
                .as_ref()
                .map(|parent| parent.as_str().to_string()),
            model_id: commit.model_id.as_str().to_string(),
            checkpoint: CheckpointFile::from_checkpoint(&commit.checkpoint),
            learning_event_count: commit.learning_event_count,
            algorithm_versions: commit.algorithm_versions.clone(),
            feature_schema_version: commit.feature_schema_version,
            created_at: commit.created_at,
            metadata: commit
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

/// The stored form of a checkpoint whose four model states carry no lossy float.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    /// The accepted checkpoint envelope version, refused unless it is current.
    pub schema_version: u32,
    /// The success model.
    pub success: StateFile,
    /// The latency model.
    pub latency: StateFile,
    /// The time-to-first-token model.
    pub ttft: StateFile,
    /// The cost model.
    pub cost: StateFile,
    /// The accepted feature schema version.
    pub feature_schema_version: u32,
    /// Wall-clock creation time. Not part of the identity.
    pub created_at: i64,
}

impl CheckpointFile {
    fn from_checkpoint(checkpoint: &ModelCheckpoint) -> Self {
        Self {
            schema_version: checkpoint.schema_version,
            success: StateFile::from_state(&checkpoint.success),
            latency: StateFile::from_state(&checkpoint.latency),
            ttft: StateFile::from_state(&checkpoint.ttft),
            cost: StateFile::from_state(&checkpoint.cost),
            feature_schema_version: checkpoint.feature_schema_version,
            created_at: checkpoint.created_at,
        }
    }

    fn rebuilt(&self) -> Result<ModelCheckpoint, ActivationError> {
        Ok(ModelCheckpoint {
            schema_version: self.schema_version,
            success: self.success.rebuilt()?,
            latency: self.latency.rebuilt()?,
            ttft: self.ttft.rebuilt()?,
            cost: self.cost.rebuilt()?,
            feature_schema_version: self.feature_schema_version,
            created_at: self.created_at,
        })
    }
}

/// The stored form of one model state, with its parameters as raw bits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    /// The accepted model-state envelope version.
    pub schema_version: u32,
    /// The algorithm identifier.
    pub algorithm: String,
    /// How many updates produced it.
    pub update_count: u64,
    /// Each parameter as 16 lowercase hex digits of its IEEE-754 bits.
    pub parameters: Vec<String>,
    /// The accepted parameter checksum, over the same bits.
    pub checksum: u64,
}

impl StateFile {
    fn from_state(state: &ModelState) -> Self {
        Self {
            schema_version: state.schema_version,
            algorithm: state.algorithm.clone(),
            update_count: state.update_count,
            parameters: state
                .parameters
                .iter()
                .map(|parameter| format_checksum(parameter.to_bits()))
                .collect(),
            checksum: state.checksum,
        }
    }

    fn rebuilt(&self) -> Result<ModelState, ActivationError> {
        let mut parameters = Vec::with_capacity(self.parameters.len());
        for (index, text) in self.parameters.iter().enumerate() {
            parameters.push(f64::from_bits(parse_checksum(text).ok_or_else(|| {
                ActivationError::SnapshotUnreadable {
                    path: PathBuf::from("<model state>"),
                    reason: format!(
                        "parameter {index} is '{text}', which is not 16 lowercase hex digits of a \
                         64-bit value"
                    ),
                }
            })?));
        }
        Ok(ModelState {
            schema_version: self.schema_version,
            algorithm: self.algorithm.clone(),
            update_count: self.update_count,
            parameters,
            checksum: self.checksum,
        })
    }
}

/// Parse 16 lowercase hex digits into a 64-bit value.
fn parse_checksum(text: &str) -> Option<u64> {
    if text.len() != SNAPSHOT_ID_HEX_DIGITS {
        return None;
    }
    let mut value = 0u64;
    for byte in text.bytes() {
        let nibble = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            b'a'..=b'f' => u64::from(byte - b'a') + 10,
            _ => return None,
        };
        value = (value << 4) | nibble;
    }
    Some(value)
}

/// A verified, immutable snapshot in memory.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub(super) id: SnapshotId,
    pub(super) commit: ModelCommit,
    pub(super) created_at: i64,
}

impl Snapshot {
    /// The content-addressed name.
    pub fn id(&self) -> &SnapshotId {
        &self.id
    }

    /// The commit this snapshot holds. Inert: it is installed nowhere.
    pub fn commit(&self) -> &ModelCommit {
        &self.commit
    }

    /// When the snapshot was written.
    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    /// The commit id this snapshot activates.
    pub fn commit_id(&self) -> &CommitId {
        &self.commit.commit_id
    }
}

/// The stored form of one activation pointer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerFile {
    /// The pointer envelope version.
    pub schema_version: u32,
    /// Strictly increasing; the first activation is generation 1.
    pub generation: u64,
    /// The active snapshot.
    pub active: PointerEntryFile,
    /// The snapshot an operator can return to, if there is one.
    pub previous: Option<PointerEntryFile>,
    /// Checksum over every other field.
    pub pointer_checksum: u64,
}

/// The stored form of one pointer entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointerEntryFile {
    /// The snapshot name.
    pub snapshot: String,
    /// The commit that snapshot activates.
    pub commit: String,
    /// The model lineage.
    pub model_id: String,
    /// When this snapshot became active.
    pub activated_at: i64,
}

/// A verified activation pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPointer {
    /// Strictly increasing across activations, starting at 1.
    pub generation: u64,
    /// The active snapshot.
    pub active: ActivationEntry,
    /// The rollback target, if there is one.
    pub previous: Option<ActivationEntry>,
}

impl ActivationPointer {
    /// Rebuild the pointer, re-running every check the write path runs.
    pub(super) fn try_from_file(file: PointerFile) -> Result<Self, ActivationError> {
        if file.schema_version != ACTIVATION_POINTER_SCHEMA_VERSION {
            return Err(ActivationError::UnsupportedSchema {
                component: "activation pointer".to_string(),
                version: file.schema_version,
                supported: ACTIVATION_POINTER_SCHEMA_VERSION,
            });
        }
        if file.generation < FIRST_GENERATION {
            return Err(ActivationError::PointerCorrupt {
                path: PathBuf::from(ACTIVATION_POINTER_NAME),
                reason: format!(
                    "generation {} is below the first activation generation {FIRST_GENERATION}",
                    file.generation
                ),
            });
        }
        let active = ActivationEntry::try_from_file(file.active)?;
        let previous = file
            .previous
            .map(ActivationEntry::try_from_file)
            .transpose()?;
        let computed = pointer_checksum(file.generation, &active, previous.as_ref());
        if file.pointer_checksum != computed {
            return Err(ActivationError::PointerCorrupt {
                path: PathBuf::from(ACTIVATION_POINTER_NAME),
                reason: format!(
                    "it records checksum {:016x} but its fields hash to {computed:016x}",
                    file.pointer_checksum
                ),
            });
        }
        Ok(Self {
            generation: file.generation,
            active,
            previous,
        })
    }

    /// The stored form, for the write path.
    pub(super) fn to_file(&self) -> PointerFile {
        PointerFile {
            schema_version: ACTIVATION_POINTER_SCHEMA_VERSION,
            generation: self.generation,
            active: self.active.to_file(),
            previous: self.previous.as_ref().map(ActivationEntry::to_file),
            pointer_checksum: pointer_checksum(
                self.generation,
                &self.active,
                self.previous.as_ref(),
            ),
        }
    }
}

/// The checksum over a pointer's identity-bearing fields.
///
/// Public for the same reason [`snapshot_checksum`] is: a second implementation
/// must be able to check the format, and the checksum is what makes a partial
/// or hand-edited pointer loud instead of silently accepted.
pub fn pointer_checksum(
    generation: u64,
    active: &ActivationEntry,
    previous: Option<&ActivationEntry>,
) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    hash_extend(&mut hash, POINTER_DOMAIN);
    hash_extend(&mut hash, &ACTIVATION_POINTER_SCHEMA_VERSION.to_le_bytes());
    hash_extend(&mut hash, &generation.to_le_bytes());
    hash_entry(&mut hash, active);
    match previous {
        Some(entry) => {
            hash_extend(&mut hash, &[1]);
            hash_entry(&mut hash, entry);
        }
        None => hash_extend(&mut hash, &[0]),
    }
    hash
}

fn hash_entry(hash: &mut u64, entry: &ActivationEntry) {
    hash_string(hash, entry.snapshot.as_str());
    hash_string(hash, entry.commit.as_str());
    hash_string(hash, entry.model_id.as_str());
    hash_extend(hash, &entry.activated_at.to_le_bytes());
}

/// The identity part of a pointer entry, without the wall-clock time.
///
/// An activation *intent* names a transition — "from this snapshot, to that
/// one, at this generation" — and not the instant somebody typed it. Leaving
/// `activated_at` out of the intent id is what makes a retry after an
/// interrupted activation derive the same id and therefore report a no-op
/// instead of a second record or a conflict. The wall-clock time is still
/// covered, where it belongs, by the pointer checksum.
pub(super) fn hash_intent_entry(hash: &mut u64, entry: &ActivationEntry) {
    hash_string(hash, entry.snapshot.as_str());
    hash_string(hash, entry.commit.as_str());
    hash_string(hash, entry.model_id.as_str());
}

/// One snapshot's place in the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationEntry {
    /// The snapshot name.
    pub snapshot: SnapshotId,
    /// The commit it activates.
    pub commit: CommitId,
    /// The model lineage.
    pub model_id: ModelId,
    /// When it became active.
    pub activated_at: i64,
}

impl ActivationEntry {
    fn try_from_file(file: PointerEntryFile) -> Result<Self, ActivationError> {
        let snapshot = SnapshotId::new(file.snapshot);
        if !snapshot.is_well_formed() {
            return Err(ActivationError::MalformedSnapshotId { id: snapshot.0 });
        }
        if file.model_id.is_empty() {
            return Err(ActivationError::PointerCorrupt {
                path: PathBuf::from(ACTIVATION_POINTER_NAME),
                reason: "a pointer entry names an empty model lineage".to_string(),
            });
        }
        Ok(Self {
            snapshot,
            commit: CommitId::new(file.commit),
            model_id: ModelId::new(file.model_id),
            activated_at: file.activated_at,
        })
    }

    fn to_file(&self) -> PointerEntryFile {
        PointerEntryFile {
            snapshot: self.snapshot.as_str().to_string(),
            commit: self.commit.as_str().to_string(),
            model_id: self.model_id.as_str().to_string(),
            activated_at: self.activated_at,
        }
    }

    /// The entry naming this snapshot, for a new activation.
    pub(super) fn for_snapshot(snapshot: &Snapshot, activated_at: i64) -> Self {
        Self {
            snapshot: snapshot.id.clone(),
            commit: snapshot.commit.commit_id.clone(),
            model_id: snapshot.commit.model_id.clone(),
            activated_at,
        }
    }
}

/// The verified active snapshot and the pointer that named it.
#[derive(Debug, Clone)]
pub struct ActiveSnapshot {
    pub(super) pointer: ActivationPointer,
    pub(super) snapshot: Snapshot,
}

impl ActiveSnapshot {
    /// The pointer, including the rollback target.
    pub fn pointer(&self) -> &ActivationPointer {
        &self.pointer
    }

    /// The verified active snapshot.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The verified active commit.
    pub fn commit(&self) -> &ModelCommit {
        &self.snapshot.commit
    }

    /// The activation generation, which increases on every flip.
    pub fn generation(&self) -> u64 {
        self.pointer.generation
    }

    /// The snapshot a rollback would return to, if there is one.
    pub fn rollback_target(&self) -> Option<&ActivationEntry> {
        self.pointer.previous.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Verification through the accepted types
// ---------------------------------------------------------------------------

/// Re-validate a commit through the accepted checks, in the order that gives
/// the most specific refusal.
pub(super) fn verify_commit(commit: &ModelCommit) -> Result<(), ActivationError> {
    check_envelope_version(
        commit.schema_version,
        "commit envelope",
        MODEL_COMMIT_SCHEMA_VERSION,
    )?;
    check_envelope_version(
        commit.checkpoint.schema_version,
        "checkpoint envelope",
        MODEL_CHECKPOINT_SCHEMA_VERSION,
    )?;
    if !commit.verify() {
        return Err(ActivationError::CommitRejected {
            commit: commit.commit_id.clone(),
            reason: "the accepted ModelCommit::verify rejected its identity, schema, algorithm \
                     table, or checkpoint"
                .to_string(),
        });
    }
    if !ReplayEngine::verify_checkpoint_integrity(&commit.checkpoint) {
        return Err(ActivationError::CheckpointRejected {
            commit: commit.commit_id.clone(),
            reason: "the accepted ReplayEngine::verify_checkpoint_integrity rejected it"
                .to_string(),
        });
    }
    Ok(())
}

/// Re-verify a commit in the caller's store, so a snapshot can never name a
/// commit the store's lineage does not vouch for.
pub(super) fn verify_commit_in_store(
    commit: &ModelCommit,
    store: &ModelStore,
) -> Result<(), ActivationError> {
    verify_commit(commit)?;
    store
        .verify_lineage(&commit.commit_id)
        .map_err(|error| ActivationError::LineageRejected {
            commit: commit.commit_id.clone(),
            reason: error.to_string(),
        })
}

/// A legacy or unknown envelope is named as such, never interpreted.
pub(super) fn check_envelope_version(
    version: u32,
    component: &str,
    supported: u32,
) -> Result<(), ActivationError> {
    if version == LEGACY_UNVERSIONED_SCHEMA_VERSION {
        return Err(ActivationError::LegacyEnvelope {
            component: component.to_string(),
            version,
        });
    }
    if version != supported {
        return Err(ActivationError::UnsupportedSchema {
            component: component.to_string(),
            version,
            supported,
        });
    }
    Ok(())
}

/// Read the nested envelope versions out of unparsed bytes.
///
/// The accepted deserializers already refuse an unknown version, but they
/// refuse it as a *serde* error, which loses the difference between "this file
/// is not a snapshot at all" and "this snapshot is a legacy artifact". So the
/// versions are probed first and both refusals get their own name. A probe that
/// cannot answer falls through to the typed path, which refuses anyway.
pub(super) fn probe_envelope_versions(bytes: &[u8]) -> Option<(u32, u32)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let commit = value.get("commit")?;
    let commit_version = u32::try_from(commit.get("schema_version")?.as_u64()?).ok()?;
    let checkpoint_version =
        u32::try_from(commit.get("checkpoint")?.get("schema_version")?.as_u64()?).ok()?;
    Some((commit_version, checkpoint_version))
}
