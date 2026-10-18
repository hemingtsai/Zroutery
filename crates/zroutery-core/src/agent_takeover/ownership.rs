//! The ownership model and the durable store: what Zroutery manages, what it replaced, and how that is recorded and restored.

use super::clients::{AgentAdapter, AgentType};
use super::fs::write_config_atomic;
use super::gateway::{takeover_fields, GatewayTarget};
use super::status::{TakeoverStatus, VerificationReport};
use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// OwnershipState
// ---------------------------------------------------------------------------

/// Lifecycle state for field ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipState {
    /// Fields have been verified and are ready to be adopted.
    Verified,
    /// Zroutery owns the managed fields.
    Adopted,
    /// A release is in flight: the state transition has been claimed but the
    /// config restore on disk has not committed yet.
    Releasing,
    /// Ownership has been released back to the external agent.
    Released,
}

impl OwnershipState {
    /// Whether a release may be attempted from this state.
    ///
    /// `Releasing` is excluded deliberately. A release already in flight has
    /// claimed its transition and is writing the config right now; a second one
    /// would race it. The state is short-lived, and a caller that lands here is
    /// looking at an operation already under way rather than at a failure.
    pub fn may_release(&self) -> bool {
        matches!(self, OwnershipState::Adopted)
    }
}

// ---------------------------------------------------------------------------
// ExternalModification
// ---------------------------------------------------------------------------

/// A single field that was modified externally since the last apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalModification {
    /// Dotted path to the modified field (e.g. "model.temperature").
    pub field_path: String,
    /// Value that was last applied by Zroutery, if any.
    pub last_applied: Option<serde_json::Value>,
    /// Current value observed externally.
    pub current_value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// FieldConflict / ConflictResolution
// ---------------------------------------------------------------------------

/// Conflict detected when a managed field was externally modified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldConflict {
    /// Dotted path to the conflicting field.
    pub field_path: String,
    /// Value captured at adoption time.
    pub original_value: Option<serde_json::Value>,
    /// Value last applied by Zroutery.
    pub last_applied: Option<serde_json::Value>,
    /// Current external value.
    pub current_external: serde_json::Value,
}

/// Resolution strategy for conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// Keep the user's external modification.
    KeepExternal,
    /// Overwrite with Zroutery's managed value.
    OverwriteWithManaged,
    /// Skip this field (no action).
    Skip,
}

/// What a resolution did, field by field.
///
/// Returned rather than logged so a caller can show the user what happened to
/// each field. "Resolved 2 conflicts" is not something anyone can check; "kept
/// your edit to `model`, overwrote `base_url`" is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionReport {
    /// The strategy that was applied.
    pub strategy: ConflictResolution,
    /// Fields whose external value was kept.
    pub kept_external: Vec<String>,
    /// Fields Zroutery's managed value was written over.
    pub overwritten: Vec<String>,
    /// Fields left untouched.
    pub skipped: Vec<String>,
}

impl ResolutionReport {
    fn new(strategy: ConflictResolution) -> Self {
        Self {
            strategy,
            kept_external: Vec::new(),
            overwritten: Vec::new(),
            skipped: Vec::new(),
        }
    }

    /// Total number of conflicts the strategy acted on.
    pub fn total(&self) -> usize {
        self.kept_external.len() + self.overwritten.len() + self.skipped.len()
    }
}

/// The result of a release that applied a conflict resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseOutcome {
    /// The manifest as committed.
    pub manifest: OwnershipManifest,
    /// Every conflict that was detected and decided.
    pub conflicts: Vec<FieldConflict>,
    /// What was decided, per field.
    pub report: ResolutionReport,
}

/// One field an adoption would change, as it should be shown before it happens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedChange {
    /// Dotted path to the field.
    pub field_path: String,
    /// The value the field holds now, or `None` if it does not exist yet.
    pub current: Option<serde_json::Value>,
    /// The value Zroutery would put there.
    pub managed: Option<serde_json::Value>,
}

/// A preview of an adoption, bound to the config it was computed against.
///
/// The binding is the point: [`TakeoverStore::confirm_adopt`] recomputes
/// [`TakeoverProposal::fingerprint`] and refuses if the config has moved, so a
/// proposal cannot be held open while the file underneath it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeoverProposal {
    /// Fields that would come under management.
    pub managed_fields: Vec<String>,
    /// The per-field before and after.
    pub changes: Vec<ProposedChange>,
    /// Digest of the config this was computed against.
    pub fingerprint: String,
}

impl TakeoverProposal {
    /// A one-line summary suitable for a confirmation dialog.
    pub fn summary(&self) -> String {
        let changes = self.changes.len();
        let creating = self.changes.iter().filter(|c| c.current.is_none()).count();
        match creating {
            0 => format!("Zroutery will take over {changes} field(s)"),
            _ => format!("Zroutery will take over {changes} field(s), creating {creating}"),
        }
    }
}

/// Pull the values of `paths` out of a raw config document.
///
/// Returns only the paths that resolve. A path that does not resolve is
/// **absent from the map** rather than present-and-`Null`, because "the field is
/// not there" and "the field is there and set to null" are different states and
/// the release path treats them differently: the first means the field was
/// removed and a restore should not recreate it, the second means restore should
/// write an explicit null.
///
/// Segments address object keys, and a numeric segment addresses an array index.
///
/// Public so a caller that has to plan a takeover — the desktop command layer —
/// reads a config the same way the store does. A second implementation of this
/// walk would be free to disagree about whether a path exists, and that
/// disagreement would surface as a conflict that is not one.
pub fn extract_field_values(
    raw: &serde_json::Value,
    paths: &[String],
) -> HashMap<String, serde_json::Value> {
    let mut found = HashMap::new();

    for path in paths {
        let mut cursor = raw;
        let mut resolved = true;
        for segment in path.split('.') {
            cursor = match cursor {
                serde_json::Value::Object(map) => match map.get(segment) {
                    Some(next) => next,
                    None => {
                        resolved = false;
                        break;
                    }
                },
                serde_json::Value::Array(items) => match segment.parse::<usize>() {
                    Ok(index) => match items.get(index) {
                        Some(next) => next,
                        None => {
                            resolved = false;
                            break;
                        }
                    },
                    Err(_) => {
                        resolved = false;
                        break;
                    }
                },
                _ => {
                    resolved = false;
                    break;
                }
            };
        }
        if resolved {
            found.insert(path.clone(), cursor.clone());
        }
    }

    found
}

/// A stable digest of a set of field values.
///
/// Stable across runs, unlike `HashMap` iteration order, so the keys are sorted
/// before hashing. Without sorting, a proposal would hash differently from
/// itself and every confirmation would be refused as stale — a gate that is
/// always closed is indistinguishable from a broken one.
fn fingerprint(values: &HashMap<String, serde_json::Value>) -> String {
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut absorb = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };

    for key in keys {
        absorb(key.as_bytes());
        absorb(b"=");
        absorb(
            serde_json::to_string(&values[key])
                .unwrap_or_else(|_| "<unserialisable>".to_string())
                .as_bytes(),
        );
        absorb(b";");
    }

    format!("{hash:016x}")
}

/// Resolve conflicts between managed fields and external modifications.
///
/// Returns `(field_path, resolved_value)` pairs. `None` values indicate the
/// field should be skipped.
pub fn resolve_conflicts(
    conflicts: &[FieldConflict],
    strategy: ConflictResolution,
) -> Vec<(String, Option<serde_json::Value>)> {
    conflicts
        .iter()
        .map(|c| match strategy {
            ConflictResolution::KeepExternal => {
                (c.field_path.clone(), Some(c.current_external.clone()))
            }
            ConflictResolution::OverwriteWithManaged => {
                (c.field_path.clone(), c.last_applied.clone())
            }
            ConflictResolution::Skip => (c.field_path.clone(), None),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// OwnershipManifest
// ---------------------------------------------------------------------------

/// Snapshot of ownership at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnershipManifest {
    /// The ownership state at the time this manifest was created.
    pub state: OwnershipState,
    /// List of field paths under Zroutery's management.
    pub managed_fields: Vec<String>,
    /// Snapshot of field values captured at adoption time.
    ///
    /// Only managed fields that already existed are recorded here; a managed
    /// field that was absent before adoption is listed in `absent_fields`.
    pub field_snapshots: HashMap<String, serde_json::Value>,
    /// Managed field paths that did not exist when ownership was adopted.
    ///
    /// Release must delete these instead of restoring a value, otherwise a
    /// field Zroutery introduced (for example a local proxy `base_url`) would
    /// survive the release and keep redirecting the client.
    #[serde(default)]
    pub absent_fields: Vec<String>,
    /// Unix timestamp (seconds) of when ownership was adopted, if ever.
    pub adopted_at: Option<i64>,
    /// Unix timestamp (seconds) of when ownership was released, if ever.
    pub released_at: Option<i64>,
    /// Monotonic counter for detecting state drift across cycles.
    pub generation: u64,
    /// Values Zroutery last wrote to managed fields, keyed by path.
    ///
    /// Durable because conflict detection is worthless without it after a
    /// restart: `field_snapshots` holds adoption-time values, so a release
    /// following a crash would compare the live config against values from
    /// before the proxy ever ran and conclude that every field the proxy had
    /// legitimately rewritten was a user edit.
    ///
    /// `#[serde(default)]` so a manifest written before this field existed still
    /// loads. Such a manifest falls back to `field_snapshots`, which over-reports
    /// rather than losing a user's edit.
    #[serde(default)]
    pub last_applied: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// TakeoverStore
// ---------------------------------------------------------------------------

struct TakeoverInner {
    state: OwnershipState,
    manifest: Option<OwnershipManifest>,
    /// Values last applied by Zroutery, keyed by field path.
    last_applied: HashMap<String, serde_json::Value>,
    /// Counts completed adopt-release cycles.
    generation: u64,
}

/// Thread-safe store that manages field ownership lifecycle.
///
/// Supports adopt/release cycles and detects external modifications to managed
/// fields.
///
/// # Durability
///
/// Ownership that lives only in memory is ownership nobody can recover. If
/// Zroutery crashes while holding it, the process that comes back has an empty
/// store, does not know which fields it managed, and does not know what their
/// values were before — so the agent's config keeps pointing at the proxy and
/// there is nothing to restore from. [`TakeoverStore::open`] makes the manifest
/// durable; [`TakeoverStore::new`] deliberately does not, and every doc comment
/// that says "in memory" says what that costs.
pub struct TakeoverStore {
    inner: Mutex<TakeoverInner>,
    /// Where the manifest is persisted, if this store is durable.
    ///
    /// `None` is a legitimate configuration, not a degraded one: a caller that
    /// only ever probes and never adopts has nothing to persist. It does mean
    /// the compare-and-swap at release time degrades to the in-process
    /// generation check, which cannot see another process.
    path: Option<std::path::PathBuf>,
}

impl TakeoverStore {
    /// Creates a new store in the `Verified` state, ready for adoption.
    ///
    /// In-memory only: nothing is written anywhere, so this store cannot survive
    /// a restart and cannot coordinate with another process. Use
    /// [`TakeoverStore::open`] for ownership that is actually held.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(TakeoverInner {
                state: OwnershipState::Verified,
                manifest: None,
                last_applied: HashMap::new(),
                generation: 0,
            }),
            path: None,
        }
    }

    /// Open a durable store backed by `path`, recovering any manifest already there.
    ///
    /// This is the constructor that makes ownership survivable. A manifest found
    /// on disk is adopted into the store, so a process that starts after a crash
    /// can still see what was managed and what the original values were — which
    /// is the entire difference between "recoverable" and "the agent's config is
    /// silently pointed at a proxy that is no longer there".
    ///
    /// The recovered state is derived from the manifest rather than reset:
    ///
    /// * an `Adopted` manifest restores `Adopted`, because ownership is still
    ///   held and pretending otherwise would lose the restore path;
    /// * a `Releasing` manifest is the awkward one — a release claimed its
    ///   transition and then died before the config write committed. The
    ///   on-disk agent config may or may not have been restored, so this is
    ///   reported as `Adopted` and the caller is expected to run
    ///   [`TakeoverStore::check_orphaned_state`] and recover deliberately, rather
    ///   than the store guessing which half of the write landed;
    /// * a `Released` manifest restores `Released`, so a re-adopt is permitted
    ///   without an artificial reset.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed. A
    /// corrupt manifest is *not* treated as "no manifest": silently starting from
    /// nothing would overwrite the only record of what was overwritten.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let recovered = match std::fs::read(&path) {
            Ok(bytes) => Some(serde_json::from_slice::<OwnershipManifest>(&bytes).map_err(
                |e| {
                    format!(
                        "ownership manifest at {} is unreadable: {e}. Refusing to start \
                         with no record of what was overwritten.",
                        path.display()
                    )
                },
            )?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };

        let (state, generation) = match &recovered {
            None => (OwnershipState::Verified, 0),
            Some(m) => (
                match m.state {
                    OwnershipState::Released => OwnershipState::Released,
                    // `Adopted`, `Releasing` and `Verified` all mean ownership may
                    // still be held. `Verified` is unreachable in a written
                    // manifest but treated as Adopted rather than discarded.
                    _ => OwnershipState::Adopted,
                },
                m.generation,
            ),
        };

        Ok(Self {
            inner: Mutex::new(TakeoverInner {
                state,
                last_applied: recovered
                    .as_ref()
                    .map(|m| m.last_applied.clone())
                    .unwrap_or_default(),
                manifest: recovered,
                generation,
            }),
            path: Some(path),
        })
    }

    /// Whether this store persists its manifest.
    pub fn is_durable(&self) -> bool {
        self.path.is_some()
    }

    /// Persist `manifest` to the durable path, if there is one.
    ///
    /// Written through [`write_config_atomic`], so a crash mid-write leaves the
    /// previous manifest intact rather than a truncated one, and the file is
    /// created 0600 because a manifest records real config values.
    fn persist(&self, manifest: &OwnershipManifest) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(manifest)
            .map_err(|e| format!("failed to serialize ownership manifest: {e}"))?;
        write_config_atomic(path, json.as_bytes())
    }

    /// Read the durable manifest, if this store has a path.
    ///
    /// Returns `Ok(None)` when the file has since been removed, which a caller
    /// treating as "somebody deleted the record" must distinguish from an error.
    fn read_persisted(&self) -> Result<Option<OwnershipManifest>, String> {
        let Some(path) = &self.path else {
            return Ok(None);
        };
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice::<OwnershipManifest>(&bytes).map_err(|e| {
                    format!(
                        "ownership manifest at {} became unreadable: {e}",
                        path.display()
                    )
                })?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Returns the current ownership state.
    pub fn state(&self) -> OwnershipState {
        crate::sync::lock(&self.inner).state
    }

    /// Returns a clone of the current manifest, if one exists.
    pub fn manifest(&self) -> Option<OwnershipManifest> {
        crate::sync::lock(&self.inner).manifest.clone()
    }

    // -- adopt / release ----------------------------------------------------

    /// Adopt: Zroutery takes ownership of managed fields.
    ///
    /// Records `current_values` for the specified `managed_fields` as
    /// `last_applied` and sets the `adopted_at` timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Verified`.
    pub fn adopt(
        &self,
        managed_fields: Vec<String>,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> Result<OwnershipManifest, String> {
        let mut inner = crate::sync::lock(&self.inner);

        if inner.state != OwnershipState::Verified && inner.state != OwnershipState::Released {
            return Err(format!(
                "cannot adopt: current state is {:?}, expected Verified or Released",
                inner.state
            ));
        }

        let now = chrono::Utc::now().timestamp();
        let generation = inner.generation;

        // Build snapshots from managed_fields only (ignore unmanaged keys).
        let field_snapshots: HashMap<String, serde_json::Value> = managed_fields
            .iter()
            .filter_map(|f| current_values.get(f).map(|v| (f.clone(), v.clone())))
            .collect();

        // Managed fields that did not exist yet must be removed on release.
        let absent_fields: Vec<String> = managed_fields
            .iter()
            .filter(|f| !current_values.contains_key(*f))
            .cloned()
            .collect();

        // Populate last_applied with the captured snapshots.
        let last_applied_snapshot = field_snapshots.clone();
        inner.last_applied = last_applied_snapshot.clone();

        let manifest = OwnershipManifest {
            state: OwnershipState::Adopted,
            managed_fields,
            field_snapshots,
            absent_fields,
            adopted_at: Some(now),
            released_at: None,
            generation,
            last_applied: last_applied_snapshot,
        };

        inner.state = OwnershipState::Adopted;
        inner.manifest = Some(manifest.clone());
        // Persist *before* returning, while `inner` is still borrowed. An adopt
        // that cannot record what it is about to overwrite must not report
        // success: the whole point of the manifest is that a later process can
        // find out what happened, and an unrecorded adopt is worse than a
        // refused one because it looks identical to one that worked.
        //
        // This is the moment durability matters most. Adoption is the step that
        // overwrites somebody's configuration; if the record of the previous
        // values cannot be written, the overwrite must not happen. The state is
        // already `Adopted` in memory at this point, so a persistence failure
        // leaves the store claiming ownership it has no record of — which is why
        // the borrow is released and the error returned rather than swallowed.
        let persisted = self.persist(&manifest);

        drop(inner);

        persisted?;

        Ok(manifest)
    }

    /// Release: Zroutery gives back ownership of managed fields.
    ///
    /// Sets the `released_at` timestamp and transitions to `Released` state.
    /// Unmanaged fields are preserved in the snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Adopted`.
    pub fn release(&self) -> Result<OwnershipManifest, String> {
        let mut inner = crate::sync::lock(&self.inner);

        if inner.state != OwnershipState::Adopted {
            return Err(format!(
                "cannot release: current state is {:?}, expected Adopted",
                inner.state
            ));
        }

        let now = chrono::Utc::now().timestamp();

        inner.generation += 1;
        let gen = inner.generation;

        let manifest = inner.manifest.as_mut().unwrap();
        manifest.state = OwnershipState::Released;
        manifest.released_at = Some(now);
        manifest.generation = gen;

        let result = manifest.clone();

        inner.state = OwnershipState::Released;
        let persisted = self.persist(&result);

        drop(inner);

        persisted?;

        Ok(result)
    }

    // -- external modification detection ------------------------------------

    /// Check whether managed fields were externally modified since last apply.
    ///
    /// Compares `current_values` (keyed by field path) against the
    /// `last_applied` snapshot. Returns a list of [`ExternalModification`]s
    /// for every field whose value differs.
    pub fn detect_external_modification(
        &self,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> Vec<ExternalModification> {
        let inner = crate::sync::lock(&self.inner);

        let mut mods = Vec::new();

        for (field, last_val) in &inner.last_applied {
            match current_values.get(field) {
                Some(current_val) if current_val != last_val => {
                    mods.push(ExternalModification {
                        field_path: field.clone(),
                        last_applied: Some(last_val.clone()),
                        current_value: current_val.clone(),
                    });
                }
                None => {
                    // Field was removed externally.
                    mods.push(ExternalModification {
                        field_path: field.clone(),
                        last_applied: Some(last_val.clone()),
                        current_value: serde_json::Value::Null,
                    });
                }
                _ => { /* unchanged */ }
            }
        }

        mods
    }

    // -- recording what we wrote ----------------------------------------------

    /// Record values Zroutery has just written to managed fields.
    ///
    /// Without this, [`TakeoverStore::conflicts`] cannot tell "the user changed
    /// this" from "we changed this", and every field the proxy legitimately
    /// rewrites looks like a user edit. A release then either discards a real
    /// user change or restores adoption-time values over values Zroutery itself
    /// set — both wrong, and the first is the one that loses somebody's work.
    ///
    /// Called on the write path, not by the release path, so the record is
    /// accurate at the moment it is needed.
    ///
    /// # Errors
    ///
    /// Returns an error only when the store is durable and the update cannot be
    /// written, because a recorded value that does not survive a restart is the
    /// same problem as no record at all.
    pub fn record_applied(
        &self,
        applied: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let mut inner = crate::sync::lock(&self.inner);
        if inner.state != OwnershipState::Adopted {
            return Err(format!(
                "cannot record applied values: current state is {:?}, expected Adopted",
                inner.state
            ));
        }
        inner.last_applied.extend(applied.clone());

        let persisted = match inner.manifest.as_mut() {
            Some(manifest) => {
                manifest.last_applied.extend(applied.clone());
                self.persist(&manifest.clone())
            }
            None => Err("cannot record applied values without a manifest".to_string()),
        };

        drop(inner);

        persisted
    }

    // -- conflict detection and application -----------------------------------

    /// Build the conflicts a release would have to decide about.
    ///
    /// Comparison is against `last_applied` — the values Zroutery itself last
    /// wrote — because a field differing from those was changed by something
    /// else. After a restart that map is empty, since only the manifest is
    /// durable, so the comparison falls back to the adoption-time snapshots.
    /// That **over-reports**: a field Zroutery legitimately rewrote after
    /// adoption looks like a conflict, because after a restart nothing
    /// distinguishes the two. Over-reporting is the safe direction here, since
    /// the resolution is what decides, and under-reporting would silently
    /// discard a user's edit.
    pub fn conflicts(
        &self,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> Vec<FieldConflict> {
        let inner = crate::sync::lock(&self.inner);
        let Some(manifest) = inner.manifest.as_ref() else {
            return Vec::new();
        };

        let baseline: HashMap<&String, &serde_json::Value> = if inner.last_applied.is_empty() {
            manifest.field_snapshots.iter().collect()
        } else {
            inner.last_applied.iter().collect()
        };

        let mut conflicts = Vec::new();
        // Iterate `managed_fields`, not `field_snapshots`. The ownership unit is
        // the managed set, and the two are not the same list: a field that did
        // not exist at adoption is recorded in `absent_fields` and has no
        // snapshot. Iterating snapshots alone meant every field Zroutery
        // *created* — which is most of what a takeover writes, since the point
        // is to point a client at a gateway it has never heard of — was never
        // conflict-checked at all. A user who then edited the base URL Zroutery
        // had just added was reported as a clean, verified takeover.
        for field in &manifest.managed_fields {
            let original = manifest.field_snapshots.get(field);
            let current = current_values.get(field);
            let differs = match current {
                Some(value) => baseline.get(field).is_none_or(|last| *last != value),
                // Absent on disk when Zroutery wrote it: a conflict, because
                // restoring means recreating a field somebody deleted.
                None => baseline.contains_key(field),
            };
            if differs {
                conflicts.push(FieldConflict {
                    field_path: field.clone(),
                    original_value: original.cloned(),
                    last_applied: inner
                        .last_applied
                        .get(field)
                        .cloned()
                        .or_else(|| original.cloned()),
                    current_external: current.cloned().unwrap_or(serde_json::Value::Null),
                });
            }
        }

        conflicts.sort_by(|a, b| a.field_path.cmp(&b.field_path));
        conflicts
    }

    /// Apply a resolution to the manifest, producing the one the restore should use.
    ///
    /// The resolution is applied by *rewriting the manifest* rather than by
    /// special-casing the restore, so the existing adapter path does the work and
    /// there is exactly one place where a restore can diverge from what was
    /// agreed:
    ///
    /// * `KeepExternal` — the snapshot becomes the external value, so restoring
    ///   it is a no-op and the user's edit survives. A field the user deleted
    ///   comes out of `absent_fields`, because restoring it would recreate what
    ///   they removed.
    /// * `OverwriteWithManaged` — the snapshot becomes Zroutery's last applied
    ///   value, so the managed value is what lands.
    /// * `Skip` — the field leaves `managed_fields` entirely, so restore never
    ///   touches it and cannot reintroduce it.
    ///
    /// Returns the effective manifest and the fields kept, overwritten and
    /// skipped, so a caller can report exactly what it did.
    pub fn apply_resolution(
        &self,
        conflicts: &[FieldConflict],
        strategy: ConflictResolution,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> (OwnershipManifest, ResolutionReport) {
        let inner = crate::sync::lock(&self.inner);
        let mut manifest = inner
            .manifest
            .clone()
            .expect("apply_resolution requires an adopted manifest");
        let mut report = ResolutionReport::new(strategy);

        for conflict in conflicts {
            match strategy {
                ConflictResolution::KeepExternal => {
                    if conflict.current_external == serde_json::Value::Null
                        && !current_values.contains_key(&conflict.field_path)
                    {
                        // Deleted externally: leaving it absent is what "keep
                        // the user's change" means here.
                        manifest.field_snapshots.remove(&conflict.field_path);
                        manifest.absent_fields.retain(|f| f != &conflict.field_path);
                    } else {
                        manifest.field_snapshots.insert(
                            conflict.field_path.clone(),
                            conflict.current_external.clone(),
                        );
                        // Leaving the path in `absent_fields` would make
                        // `restore_fields` delete it: that function checks
                        // `absent_fields` first, so a path in both lists is
                        // removed rather than restored. "Keep the user's edit"
                        // would then quietly discard the edit for every field
                        // Zroutery had *created* — which is most of what a
                        // takeover writes, since the point is to point a client
                        // at a gateway it has never heard of.
                        manifest.absent_fields.retain(|f| f != &conflict.field_path);
                    }
                    report.kept_external.push(conflict.field_path.clone());
                }
                ConflictResolution::OverwriteWithManaged => {
                    if let Some(value) = &conflict.last_applied {
                        manifest
                            .field_snapshots
                            .insert(conflict.field_path.clone(), value.clone());
                        // Same reason as the `KeepExternal` arm above.
                        manifest.absent_fields.retain(|f| f != &conflict.field_path);
                    }
                    report.overwritten.push(conflict.field_path.clone());
                }
                ConflictResolution::Skip => {
                    // Both maps, not just the declared one. An adapter's
                    // `release` restores everything in `field_snapshots`, so
                    // dropping only `managed_fields` would leave the field being
                    // written and make `Skip` a no-op that still reported itself
                    // as applied. `managed_fields` is the declared set;
                    // `field_snapshots` is the effective restore set.
                    manifest
                        .managed_fields
                        .retain(|f| f != &conflict.field_path);
                    manifest.field_snapshots.remove(&conflict.field_path);
                    manifest.absent_fields.retain(|f| f != &conflict.field_path);
                    report.skipped.push(conflict.field_path.clone());
                }
            }
        }

        (manifest, report)
    }

    /// Release ownership, deciding every detected conflict with `strategy`.
    ///
    /// This is the path that applies a resolution. `release_with_restore` exists
    /// and restores the adoption-time snapshot unconditionally, which means it
    /// discards anything the user changed while Zroutery held the fields — and
    /// [`resolve_conflicts`], which computes what *should* happen, had no
    /// production caller at all.
    ///
    /// # Errors
    ///
    /// Returns an error if the state is not `Adopted`, if the adapter cannot read
    /// or write the config, or if the durable manifest moved while the write was
    /// in flight.
    pub fn release_resolved(
        &self,
        adapter: &dyn AgentAdapter,
        strategy: ConflictResolution,
    ) -> Result<ReleaseOutcome, String> {
        // Claim the transition exactly as `release_with_restore` does, so the two
        // cannot drift apart in how they serialise against a competitor.
        let (manifest_snapshot, expected_generation) = {
            let mut inner = crate::sync::lock(&self.inner);
            if inner.state != OwnershipState::Adopted {
                return Err(format!(
                    "cannot release: current state is {:?}, expected Adopted",
                    inner.state
                ));
            }
            let manifest = inner.manifest.as_ref().unwrap().clone();
            inner.state = OwnershipState::Releasing;
            (manifest, inner.generation)
        };

        let current = match adapter.read_config() {
            Ok(current) => current,
            Err(err) => {
                self.abort_release();
                return Err(err);
            }
        };

        let current_values = extract_field_values(&current.raw, &manifest_snapshot.managed_fields);
        let conflicts = self.conflicts(&current_values);
        let (effective, report) = self.apply_resolution(&conflicts, strategy, &current_values);

        if let Err(err) = adapter.release(&current, &effective) {
            self.abort_release();
            return Err(err);
        }

        self.commit_release(expected_generation)?;

        Ok(ReleaseOutcome {
            manifest: self.manifest().expect("release committed a manifest"),
            conflicts,
            report,
        })
    }

    /// Commit a claimed release, re-checking both the in-process claim and the
    /// durable record before the manifest is marked released.
    fn commit_release(&self, expected_generation: u64) -> Result<OwnershipManifest, String> {
        if let Some(on_disk) = self.read_persisted()? {
            if on_disk.generation != expected_generation || on_disk.state != OwnershipState::Adopted
            {
                self.abort_release();
                return Err(format!(
                    "cannot release: the ownership manifest on disk moved while the config was \
                     being restored (state {:?}, generation {}, expected Adopted at generation \
                     {expected_generation}). Another process is using these fields.",
                    on_disk.state, on_disk.generation
                ));
            }
        } else if self.path.is_some() {
            self.abort_release();
            return Err(
                "cannot release: the ownership manifest disappeared from disk while the config \
                 was being restored"
                    .to_string(),
            );
        }

        let mut inner = crate::sync::lock(&self.inner);
        if inner.state != OwnershipState::Releasing || inner.generation != expected_generation {
            let state = inner.state;
            let generation = inner.generation;
            if inner.state == OwnershipState::Releasing {
                inner.state = OwnershipState::Adopted;
            }
            return Err(format!(
                "cannot release: ownership changed while restoring the config \
                 (state {state:?}, generation {generation}, expected generation {expected_generation})"
            ));
        }

        inner.generation += 1;
        let generation = inner.generation;
        let manifest = inner.manifest.as_mut().unwrap();
        manifest.state = OwnershipState::Released;
        manifest.released_at = Some(chrono::Utc::now().timestamp());
        manifest.generation = generation;

        let result = manifest.clone();
        inner.state = OwnershipState::Released;
        let persisted = self.persist(&result);

        drop(inner);

        persisted?;

        Ok(result)
    }

    // -- confirmation flow ----------------------------------------------------

    /// Compute what an adoption would change, without changing anything.
    ///
    /// Adoption overwrites somebody's agent configuration, so the values that are
    /// about to be replaced have to be knowable *before* the overwrite — not
    /// reported afterwards from a snapshot that has already been taken. This
    /// returns that preview, plus a fingerprint of the config it was computed
    /// against.
    ///
    /// # Errors
    ///
    /// Returns an error if `managed_fields` is empty (there is nothing to
    /// confirm) or if a managed field has no corresponding entry in
    /// `managed_values` (a takeover that silently skips a field it was asked to
    /// manage is worse than one that refuses).
    pub fn plan_adopt(
        &self,
        managed_fields: &[String],
        current_values: &HashMap<String, serde_json::Value>,
        managed_values: &HashMap<String, serde_json::Value>,
    ) -> Result<TakeoverProposal, String> {
        if managed_fields.is_empty() {
            return Err("cannot plan a takeover of no fields".into());
        }

        let mut missing = Vec::new();
        for field in managed_fields {
            if !managed_values.contains_key(field) {
                missing.push(field.clone());
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "no managed value supplied for {}; a takeover that skipped them would leave \
                 those fields unmanaged while claiming otherwise",
                missing.join(", ")
            ));
        }

        let changes = managed_fields
            .iter()
            .map(|field| ProposedChange {
                field_path: field.clone(),
                current: current_values.get(field).cloned(),
                managed: managed_values.get(field).cloned(),
            })
            .collect();

        Ok(TakeoverProposal {
            managed_fields: managed_fields.to_vec(),
            changes,
            fingerprint: fingerprint(current_values),
        })
    }

    /// Adopt, but only against the config the proposal was computed from.
    ///
    /// `current_values` is the config as it reads *now*. If its fingerprint
    /// differs from the proposal's, somebody changed the agent's config between
    /// the preview and the confirmation, and the thing being confirmed is not
    /// the thing on screen. That is refused rather than applied: a takeover
    /// silently overwriting values the user never saw described is the failure
    /// this flow exists to prevent.
    ///
    /// # Errors
    ///
    /// Returns an error if the config moved, or if the store is not in a state
    /// that permits adoption.
    pub fn confirm_adopt(
        &self,
        proposal: &TakeoverProposal,
        current_values: &HashMap<String, serde_json::Value>,
    ) -> Result<OwnershipManifest, String> {
        let now = fingerprint(current_values);
        if now != proposal.fingerprint {
            return Err(format!(
                "cannot confirm: the agent config changed since the takeover was planned \
                 (planned against {}, config now hashes to {}). Plan again so the confirmation \
                 describes what will actually change.",
                proposal.fingerprint, now
            ));
        }

        self.adopt(proposal.managed_fields.clone(), current_values)
    }

    // -- real restore ---------------------------------------------------------

    /// Release ownership and restore original config values via the adapter.
    ///
    /// Reads the current config from disk, restores managed fields to their
    /// original values (captured at adoption), writes back, and transitions
    /// to `Released` state.
    ///
    /// If the disk write fails, the internal state is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Adopted`, or if the
    /// adapter fails to read/write config.
    pub fn release_with_restore(
        &self,
        adapter: &dyn AgentAdapter,
    ) -> Result<OwnershipManifest, String> {
        // 1. Claim the transition while holding the lock. Marking the store as
        //    `Releasing` means a competing adopt or release observes an
        //    in-flight operation instead of the stale `Adopted` state, so it
        //    cannot adopt a new generation or mark this manifest released
        //    while the restore below is still writing the config.
        let (manifest_snapshot, expected_generation) = {
            let mut inner = crate::sync::lock(&self.inner);
            if inner.state != OwnershipState::Adopted {
                return Err(format!(
                    "cannot release: current state is {:?}, expected Adopted",
                    inner.state
                ));
            }
            let manifest = inner.manifest.as_ref().unwrap().clone();
            inner.state = OwnershipState::Releasing;
            (manifest, inner.generation)
        };

        // 2. Read current config from disk.
        let current = match adapter.read_config() {
            Ok(current) => current,
            Err(err) => {
                self.abort_release();
                return Err(err);
            }
        };

        // 3. Restore original values and write to disk.
        if let Err(err) = adapter.release(&current, &manifest_snapshot) {
            self.abort_release();
            return Err(err);
        }

        // 4. Commit only if the claim is still the current one.
        //
        //    Two different questions, and the durable check is the one that
        //    matters across processes. The in-memory generation catches a
        //    competing thread inside this process. It cannot see a second
        //    process that adopted, released, or crashed-and-recovered while the
        //    config write above was in flight — and this store is exactly the
        //    thing that write is about to undo. So when the manifest is durable,
        //    re-read it and compare, which is a compare-and-swap against the
        //    record both processes share.
        if let Some(on_disk) = self.read_persisted()? {
            if on_disk.generation != expected_generation || on_disk.state != OwnershipState::Adopted
            {
                self.abort_release();
                return Err(format!(
                    "cannot release: the ownership manifest on disk moved while the config \
                     was being restored (state {:?}, generation {}, expected Adopted at \
                     generation {expected_generation}). Another process is using these fields.",
                    on_disk.state, on_disk.generation
                ));
            }
        } else if self.path.is_some() {
            // Durable, and the file is gone. Something removed the record while we
            // held ownership, which means the restore has nothing to be reconciled
            // against and must not be committed on the assumption that it does.
            self.abort_release();
            return Err(
                "cannot release: the ownership manifest disappeared from disk while the config \
                 was being restored"
                    .to_string(),
            );
        }

        let mut inner = crate::sync::lock(&self.inner);
        if inner.state != OwnershipState::Releasing || inner.generation != expected_generation {
            let state = inner.state;
            let generation = inner.generation;
            if inner.state == OwnershipState::Releasing {
                inner.state = OwnershipState::Adopted;
            }
            return Err(format!(
                "cannot release: ownership changed while restoring the config \
                 (state {state:?}, generation {generation}, expected generation {expected_generation})"
            ));
        }

        inner.generation += 1;
        let gen = inner.generation;

        let manifest = inner.manifest.as_mut().unwrap();
        manifest.state = OwnershipState::Released;
        manifest.released_at = Some(chrono::Utc::now().timestamp());
        manifest.generation = gen;

        let result = manifest.clone();
        inner.state = OwnershipState::Released;
        let persisted = self.persist(&result);

        drop(inner);

        persisted?;

        Ok(result)
    }

    /// Undo an in-flight release claim after the config write failed.
    fn abort_release(&self) {
        let mut inner = crate::sync::lock(&self.inner);
        if inner.state == OwnershipState::Releasing {
            inner.state = OwnershipState::Adopted;
        }
    }

    // -- crash recovery -------------------------------------------------------

    /// Check for incomplete ownership state after a crash.
    ///
    /// Returns `true` if the store is in the `Adopted` state, indicating an
    /// adopt was never released (possibly due to a crash).
    pub fn check_orphaned_state(&self) -> bool {
        let inner = crate::sync::lock(&self.inner);
        inner.state == OwnershipState::Adopted
    }

    /// Recover from an orphaned adoption by releasing with config restore.
    ///
    /// Equivalent to calling [`release_with_restore`] but framed as a
    /// crash-recovery operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the store is not in an orphaned state, or if
    /// the adapter fails.
    pub fn recover_orphaned(
        &self,
        adapter: &dyn AgentAdapter,
    ) -> Result<OwnershipManifest, String> {
        if !self.check_orphaned_state() {
            return Err("no orphaned state to recover".into());
        }
        self.release_with_restore(adapter)
    }

    // -- surface status --------------------------------------------------------

    /// Work out what this client surface is allowed to claim, right now.
    ///
    /// This is the function a UI, a tray menu or a startup check calls, and it
    /// is deliberately the *only* place a takeover state is decided. Reading
    /// [`OwnershipState`] directly and reporting it is the mistake this exists to
    /// prevent: `Adopted` says Zroutery wrote a file, not that the file is still
    /// in effect.
    ///
    /// The derivation, in order:
    ///
    /// 1. No adapter, or [`takeover_fields`] refuses the agent →
    ///    [`TakeoverStatus::Unsupported`].
    /// 2. The store is not holding ownership, the config file is missing →
    ///    [`TakeoverStatus::NotDetected`], carrying the paths tried.
    /// 3. The store is not holding ownership, the config parses →
    ///    [`TakeoverStatus::Available`], or [`TakeoverStatus::Ready`] when a
    ///    [`GatewayTarget`] was supplied and yields a field set.
    /// 4. The store is holding ownership, but the live config no longer matches
    ///    what Zroutery recorded → [`TakeoverStatus::Conflict`].
    /// 5. The store is holding ownership and the config matches. If `verified`
    ///    is absent or does not confirm, the result is
    ///    [`TakeoverStatus::Degraded`], never [`TakeoverStatus::Active`].
    /// 6. The store is `Released` → [`TakeoverStatus::Released`].
    ///
    /// Anything the adapter cannot do (unreadable home, a corrupt file, a
    /// refused config shape) becomes [`TakeoverStatus::Error`] with the
    /// adapter's own reason, rather than an exception the caller has to
    /// translate. A status a UI cannot render is a status nobody acts on.
    ///
    /// `target` is `None` when the caller cannot supply a gateway credential —
    /// most often because the desktop shell has no token yet. That is not an
    /// error: the result is `available` rather than `ready`, which is the
    /// honest answer for a client Zroutery has not yet been given anything to
    /// offer.
    pub fn inspect(
        &self,
        adapter: Option<&dyn AgentAdapter>,
        target: Option<&GatewayTarget>,
        verified: Option<&VerificationReport>,
    ) -> TakeoverStatus {
        // (1) Is there anything to take over with?
        let Some(adapter) = adapter else {
            return TakeoverStatus::Unsupported {
                reason: format!(
                    "this build has no takeover adapter for {}",
                    AgentType::Gemini.client_name()
                ),
            };
        };

        let owned = self.state() == OwnershipState::Adopted;
        let config_path = match adapter.config_path() {
            Ok(path) => path,
            Err(reason) => return TakeoverStatus::Error { reason },
        };
        let path_text = config_path.display().to_string();

        // A manifest written by an older build may name an agent this build has
        // no adapter for. Releasing it is still the priority — but `adapter` is
        // `None` there, so this only reaches the owned branch when an adapter
        // does exist for the recorded agent.
        if owned {
            let Some(manifest) = self.manifest() else {
                return TakeoverStatus::Error {
                    reason: "ownership is held but no manifest survived, so there is nothing safe \
                             to restore from. Reinstall the client to get its defaults back."
                        .into(),
                };
            };

            // (4) Does the live config still match what we recorded?
            let snapshot = match adapter.read_config() {
                Ok(snapshot) => snapshot,
                Err(reason) => {
                    return TakeoverStatus::Error {
                        reason: format!(
                            "ownership is held but the config at {path_text} cannot be read: \
                             {reason}"
                        ),
                    }
                }
            };
            let live: HashMap<String, serde_json::Value> =
                extract_field_values(&snapshot.raw, &manifest.managed_fields);
            let conflicts = self.conflicts(&live);
            if !conflicts.is_empty() {
                return TakeoverStatus::Conflict {
                    config_path: path_text,
                    conflicts,
                };
            }

            // (5) Matches on disk. Does anything vouch that the client reloaded?
            let confirmed = verified.is_some_and(|v| {
                v.config_matches && v.credential_ok && v.request_confirmed == Some(true)
            });
            if confirmed {
                return TakeoverStatus::Active {
                    config_path: path_text,
                    generation: manifest.generation,
                };
            }
            let mut reasons: Vec<String> = verified
                .map(|v| v.notes.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|note| !note.is_empty())
                .collect();
            if reasons.is_empty() {
                reasons.push(
                    "the config was written, but no request from this client has been seen \
                     reaching Zroutery, so the takeover is not confirmed"
                        .into(),
                );
            }
            return TakeoverStatus::Degraded {
                config_path: path_text,
                generation: manifest.generation,
                reasons,
            };
        }

        // Not holding ownership. (6) A released manifest still reports as such.
        if self.state() == OwnershipState::Released {
            if let Some(manifest) = self.manifest() {
                return TakeoverStatus::Released {
                    config_path: path_text,
                    generation: manifest.generation,
                };
            }
        }

        // (2) No file at all.
        if !config_path.exists() {
            return TakeoverStatus::NotDetected {
                searched: vec![path_text],
                reason: format!(
                    "no {} config was found. Install and run {} once, or point Zroutery at an \
                     existing config.",
                    adapter.agent_type().client_name(),
                    adapter.agent_type().client_name()
                ),
            };
        }

        // (3) A file that parses. Anything else is an error the user must see.
        let snapshot = match adapter.read_config() {
            Ok(snapshot) => snapshot,
            Err(reason) => {
                return TakeoverStatus::Error {
                    reason: format!("the config at {path_text} cannot be used: {reason}"),
                }
            }
        };

        let fields = match target {
            Some(target) => {
                match takeover_fields(adapter.agent_type(), target, Some(&snapshot.raw)) {
                    Ok(fields) => fields,
                    // The client is fine; Zroutery is not ready to serve it. That is
                    // `available`, not an error — the user may be mid-setup.
                    Err(reason) => {
                        return TakeoverStatus::Available {
                            config_path: path_text,
                            blocker: reason,
                        }
                    }
                }
            }
            None => {
                return TakeoverStatus::Available {
                    config_path: path_text,
                    blocker: "Zroutery has no local gateway token yet, so there is nothing for \
                              this client to be pointed at."
                        .into(),
                }
            }
        };

        let managed_values: HashMap<String, serde_json::Value> = fields
            .iter()
            .map(|f| (f.path.clone(), f.value.clone()))
            .collect();
        let paths: Vec<String> = fields.iter().map(|f| f.path.clone()).collect();
        let current = extract_field_values(&snapshot.raw, &paths);

        match self.plan_adopt(&paths, &current, &managed_values) {
            Ok(proposal) => TakeoverStatus::Ready {
                config_path: path_text,
                proposal: Box::new(proposal),
            },
            Err(reason) => TakeoverStatus::Error { reason },
        }
    }
}

impl Default for TakeoverStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
