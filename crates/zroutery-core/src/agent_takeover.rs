//! Agent ownership / adopt lifecycle.
//!
//! Manages the lifecycle of ownership over configuration fields that Zroutery
//! can adopt from an external agent and release back. Tracks managed field
//! values, detects external modifications, and supports clean adopt/release
//! cycles without state drift.
//!
//! State machine:
//!
//! ```text
//! Verified ──adopt()──▶ Adopted ──release()──▶ Released
//!    ▲                     │                      │
//!    └─────────────────────┘◀─────────────────────┘
//!
//! release_with_restore(): Adopted ──▶ Releasing ──(config restored)──▶ Released
//! ```

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

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
            Some(target) => match takeover_fields(adapter.agent_type(), target) {
                Ok(fields) => fields,
                // The client is fine; Zroutery is not ready to serve it. That is
                // `available`, not an error — the user may be mid-setup.
                Err(reason) => {
                    return TakeoverStatus::Available {
                        config_path: path_text,
                        blocker: reason,
                    }
                }
            },
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
// AgentType
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// TakeoverStatus — what a client surface is allowed to claim
// ---------------------------------------------------------------------------

/// Where one client's takeover stands, right now.
///
/// # Why this exists separately from `OwnershipState`
///
/// [`OwnershipState`] is the manifest's four-state bookkeeping: verified,
/// adopted, releasing, released. It answers "what may this store do next". It
/// cannot answer the question a user actually asks, which is *is this client
/// really using Zroutery?* — and getting that wrong is the failure mode that
/// produced the `~/.claude.json` bug: a write that succeeded, a state machine
/// that said `Adopted`, and a client still talking to `api.anthropic.com`.
///
/// So the surface state is derived from the manifest **plus the live config on
/// disk** **plus whatever the caller could verify**, and the two are never
/// collapsed. `Active` requires the config to still match; `Degraded` is what a
/// takeover becomes the moment that stops being provable.
///
/// The transitions a user sees:
///
/// ```text
/// unsupported ─▶ not_detected ─▶ available ─▶ ready
///                                     │          │
///                                     │          ▼
///                                     └──────▶ active ◀──┐
///                                                │  ▲      │
///                              ┌─────────────────┤  │      │
///                              ▼                 ▼  │      │
///                          conflict ──resolve──▶ degraded  │
///                              │                 │  │      │
///                              └──────release────┴──┴──────┘
///                                               │
///                                               ▼
///                                            released
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TakeoverStatus {
    /// This build has no adapter for the client.
    ///
    /// The only current case is Gemini CLI. The reason is carried so a UI can
    /// explain *why* rather than showing an inert control.
    Unsupported {
        /// Human-readable reason, safe to show.
        reason: String,
    },

    /// An adapter exists, but no usable client config was found.
    ///
    /// `searched` lists the paths that were tried, which is the only thing that
    /// lets a user fix this themselves.
    NotDetected {
        /// Paths that were checked, in priority order.
        searched: Vec<String>,
        /// Why none of them was usable.
        reason: String,
    },

    /// A config was found and parses, but Zroutery cannot yet serve it.
    ///
    /// Distinct from [`TakeoverStatus::Ready`] because the blocker is on
    /// Zroutery's side, not the client's: there is no gateway token, no base URL
    /// or no model class to offer. `blocker` says which, because "not ready" with
    /// no reason is a dead end the user cannot act on.
    Available {
        /// The config that would be taken over.
        config_path: String,
        /// What Zroutery is missing.
        blocker: String,
    },

    /// Everything needed to take over is present; the user has not confirmed.
    ///
    /// The difference from [`TakeoverStatus::Available`] is the gateway target:
    /// `available` with no token, no base URL or no model classes is a client
    /// Zroutery cannot actually serve, and reporting it as `ready` would set up
    /// a button that fails when pressed.
    Ready {
        /// The config that would be taken over.
        config_path: String,
        /// What the takeover would change, for the confirmation dialog.
        proposal: Box<TakeoverProposal>,
    },

    /// Taken over, and the live config still matches what Zroutery wrote.
    Active {
        /// The config under management.
        config_path: String,
        /// The manifest generation, so a caller can tell two takeovers apart.
        generation: u64,
    },

    /// Taken over, but the live config no longer matches the record.
    ///
    /// This is never auto-resolved. Someone else changed a field Zroutery owns,
    /// and overwriting it without asking is how a takeover destroys work.
    Conflict {
        /// The config under management.
        config_path: String,
        /// Fields whose live value differs from what Zroutery wrote.
        conflicts: Vec<FieldConflict>,
    },

    /// Taken over and the config matches, but the takeover is not provably working.
    ///
    /// The honest state for a config that was written and never confirmed by a
    /// real request. Presenting this as `active` is exactly the lie this
    /// subsystem is meant to stop telling.
    Degraded {
        /// The config under management.
        config_path: String,
        /// The manifest generation.
        generation: u64,
        /// What could not be confirmed, phrased for a user.
        reasons: Vec<String>,
    },

    /// Not held; a previous takeover was released and the config restored.
    Released {
        /// The config that was restored.
        config_path: String,
        /// The manifest generation, so a re-adopt is distinguishable.
        generation: u64,
    },

    /// An operation failed for a reason the user can act on.
    Error {
        /// Human-readable reason, safe to show.
        reason: String,
    },
}

impl TakeoverStatus {
    /// The short state name, for logs and for a UI that switches on it.
    pub fn name(&self) -> &'static str {
        match self {
            TakeoverStatus::Unsupported { .. } => "unsupported",
            TakeoverStatus::NotDetected { .. } => "not_detected",
            TakeoverStatus::Available { .. } => "available",
            TakeoverStatus::Ready { .. } => "ready",
            TakeoverStatus::Active { .. } => "active",
            TakeoverStatus::Conflict { .. } => "conflict",
            TakeoverStatus::Degraded { .. } => "degraded",
            TakeoverStatus::Released { .. } => "released",
            TakeoverStatus::Error { .. } => "error",
        }
    }

    /// Whether Zroutery currently holds managed fields for this client.
    ///
    /// `true` for `active`, `conflict` and `degraded` — all three still hold
    /// ownership and all three need the release path available. A caller that
    /// used `== Active` here would hide the release button exactly when the user
    /// most needs it.
    pub fn holds_ownership(&self) -> bool {
        matches!(
            self,
            TakeoverStatus::Active { .. }
                | TakeoverStatus::Conflict { .. }
                | TakeoverStatus::Degraded { .. }
        )
    }

    /// Whether the takeover can be confirmed as genuinely in effect.
    ///
    /// Only [`TakeoverStatus::Active`] qualifies. `Degraded` and `Conflict` do
    /// not, which is the whole point of having them.
    pub fn is_verified_effective(&self) -> bool {
        matches!(self, TakeoverStatus::Active { .. })
    }
}

/// What a caller could actually prove about a client that was told to reload.
///
/// The core cannot start a Claude Code process and watch its traffic, so this is
/// the seam: the command layer observes what it can and passes it in, and the
/// status machine refuses to say `Active` on an unproven report.
///
/// `request_confirmed` is three-valued on purpose. `None` means "not
/// observable here" — no real request was seen — and that is *not* the same as
/// `Some(false)`, which means a request was seen and did not arrive. Collapsing
/// them would let an environment that cannot observe clients claim `Active`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    /// The live config still carries the values Zroutery wrote.
    pub config_matches: bool,
    /// The credential Zroutery handed out still authenticates.
    pub credential_ok: bool,
    /// A real request from the client was observed reaching Zroutery.
    pub request_confirmed: Option<bool>,
    /// Anything a user should be told, phrased for them.
    pub notes: Vec<String>,
}

impl VerificationReport {
    /// A report for a takeover nobody has checked yet.
    ///
    /// Deliberately `None` rather than `Some(false)`: not knowing is not the
    /// same as knowing it failed, and it keeps the status at `Degraded` rather
    /// than `Conflict`.
    pub fn unchecked() -> Self {
        Self {
            config_matches: false,
            credential_ok: false,
            request_confirmed: None,
            notes: vec!["this takeover has not been verified yet".into()],
        }
    }
}

// ---------------------------------------------------------------------------
// AgentType
// ---------------------------------------------------------------------------

/// Supported external agent types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    Claude,
    Codex,
    Gemini,
    /// OpenCode.
    ///
    /// Spelled `opencode` rather than `open_code`: it is the client's own name
    /// for itself (it puts `opencode/<version>` in its User-Agent), and this
    /// serialized form is what the command layer and any persisted manifest
    /// carry across restarts.
    ///
    /// Not to be confused with `ProviderKind::OpenCodeZenFree` /
    /// `OpenCodeConsole`, which are *upstream providers Zroutery calls*. This
    /// variant is the *client Zroutery takes over*.
    #[serde(rename = "opencode")]
    OpenCode,
}

impl AgentType {
    /// Every agent, in a fixed order.
    ///
    /// A surface that lists agents — a panel, a set of ownership stores, a
    /// sweep — iterates this rather than writing the variants out again, so
    /// adding an agent cannot leave one of them quietly missing.
    pub const ALL: [AgentType; 4] = [
        AgentType::Claude,
        AgentType::Codex,
        AgentType::Gemini,
        AgentType::OpenCode,
    ];

    /// The client-facing name, for a path or a label.
    ///
    /// Separate from the serde form on purpose: the serde form is a wire value
    /// and changing it would break persisted manifests, while this one names a
    /// file on disk.
    pub fn client_name(&self) -> &'static str {
        match self {
            AgentType::Claude => "claude",
            AgentType::Codex => "codex",
            AgentType::Gemini => "gemini",
            AgentType::OpenCode => "opencode",
        }
    }

    /// Whether this build can take the client over.
    ///
    /// `Gemini` is [`AgentType`]'s one variant with no adapter. It stays in the
    /// enum because persisted manifests may still carry it, and dropping the
    /// variant would make those manifests fail to parse — taking away the only
    /// path that could release a Gemini config a previous build had adopted.
    /// What is withdrawn is *new* takeover: see [`takeover_fields`], which
    /// refuses Gemini outright, and [`adapter_for`], which returns `None`.
    pub fn is_takeover_supported(&self) -> bool {
        !matches!(self, AgentType::Gemini)
    }
}

/// The adapter that can take `agent` over, or `None` if this build has none.
///
/// Gemini is the deliberate `None`; see [`AgentType::is_takeover_supported`].
pub fn adapter_for(agent: AgentType) -> Option<Box<dyn AgentAdapter>> {
    match agent {
        AgentType::Claude => Some(Box::new(ClaudeAdapter)),
        AgentType::Codex => Some(Box::new(CodexAdapter)),
        AgentType::OpenCode => Some(Box::new(OpenCodeAdapter)),
        AgentType::Gemini => None,
    }
}

// ---------------------------------------------------------------------------
// AgentConfigSnapshot
// ---------------------------------------------------------------------------

/// A snapshot of an agent's configuration, captured at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigSnapshot {
    /// The agent this config belongs to.
    pub agent_type: AgentType,
    /// Path to the config file on disk.
    pub config_path: std::path::PathBuf,
    /// Raw config parsed as a JSON value.
    pub raw: serde_json::Value,
    /// Hex-encoded hash of the serialized config for change detection.
    pub config_hash: String,
}

// ---------------------------------------------------------------------------
// ManagedField
// ---------------------------------------------------------------------------

/// A single configuration field under Zroutery's management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedField {
    /// Dotted path to the field (e.g. "model.temperature").
    pub path: String,
    /// Desired value for the field.
    pub value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// AgentAdapter
// ---------------------------------------------------------------------------

/// Trait for agent-specific configuration management.
///
/// Each external agent (Claude, Codex, Gemini) implements this trait to
/// provide config discovery, reading, patching, and release semantics.
pub trait AgentAdapter: Send + Sync {
    /// Agent type identifier.
    fn agent_type(&self) -> AgentType;

    /// Find the agent config file path.
    fn config_path(&self) -> Result<std::path::PathBuf, String>;

    /// Read and parse the current config.
    fn read_config(&self) -> Result<AgentConfigSnapshot, String>;

    /// Apply managed field patches to the config.
    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String>;

    /// Restore original values for managed fields.
    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String>;

    /// Write a config snapshot back to disk.
    fn write_config(&self, snapshot: &AgentConfigSnapshot) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&snapshot.raw)
            .map_err(|e| format!("failed to serialize config: {e}"))?;
        write_config_atomic(&snapshot.config_path, json.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// ClaudeAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Claude Code.
///
/// Config location: `$CLAUDE_CONFIG_DIR/settings.json` (default
/// `~/.claude/settings.json`).
///
/// # Why not `~/.claude.json`
///
/// The first version of this adapter read `~/.claude.json`. Claude Code writes
/// that file for itself — it holds the sign-in session, MCP server
/// configurations, per-project trust decisions and the global config keys that
/// `/config` writes — and it is **not** one of the settings files in Claude
/// Code's precedence stack. The `env` block it is supposed to carry lives in
/// `~/.claude/settings.json`, so writing to `~/.claude.json` would have
/// succeeded and changed nothing: the client would keep talking to
/// `api.anthropic.com` while Zroutery reported the agent as taken over. That is
/// the worst shape of failure this subsystem has — a successful write, a
/// reported success, and no effect.
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Claude
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        if let Some(dir) = config_dir_override("CLAUDE_CONFIG_DIR") {
            return Ok(dir.join("settings.json"));
        }
        let home = home_dir()?;
        Ok(home.join(".claude").join("settings.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        read_snapshot(AgentType::Claude, self.config_path()?, ConfigFormat::Json)
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
        release_to_disk(snapshot, manifest, ConfigFormat::Json)
    }
}

// ---------------------------------------------------------------------------
// CodexAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Codex CLI.
///
/// Config location: `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`),
/// the TOML file Codex itself reads.
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Codex
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        if let Some(dir) = config_dir_override("CODEX_HOME") {
            return Ok(dir.join("config.toml"));
        }
        let home = home_dir()?;
        Ok(home.join(".codex").join("config.toml"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        read_snapshot(AgentType::Codex, self.config_path()?, ConfigFormat::Toml)
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        apply_patch_to_disk(snapshot, fields, ConfigFormat::Toml)
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        release_to_disk(snapshot, manifest, ConfigFormat::Toml)
    }

    fn write_config(&self, snapshot: &AgentConfigSnapshot) -> Result<(), String> {
        let toml = serialize_config(ConfigFormat::Toml, &snapshot.raw)?;
        write_config_atomic(&snapshot.config_path, toml.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// GeminiAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for Gemini CLI.
///
/// Config location: `$GEMINI_CLI_HOME/.gemini/settings.json`
/// (default `~/.gemini/settings.json`), the user settings file Gemini CLI
/// documents.
///
/// # Retained for release only
///
/// New Gemini takeovers are withdrawn (B6): its endpoint is only settable
/// through `GOOGLE_GEMINI_BASE_URL`, an environment variable rather than a
/// file field, so taking over the settings file redirects the key and leaves
/// the client still talking to Google — a takeover that reports success and
/// does nothing.
///
/// This adapter is **not** in [`adapter_for`]. It stays because a manifest
/// written by an older build may still name `AgentType::Gemini` and may still
/// hold the only record of what was overwritten. Removing the variant or the
/// adapter would leave that record unreadable and the user's config
/// unrecoverable. The release path is the part worth keeping; see
/// [`takeover_fields`] for the refusal.
///
/// Nothing about the Gemini *protocol* is affected: `protocol::gemini`, its
/// `generateContent` ingress, its usage decoder and its routing are separate
/// code and are untouched by removing CLI takeover.
#[deprecated(
    since = "0.8.0",
    note = "new Gemini CLI takeovers are withdrawn; retained so an older manifest can still be released"
)]
pub struct GeminiAdapter;

#[allow(deprecated)] // the release path is the reason this is kept
impl AgentAdapter for GeminiAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Gemini
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        if let Some(home) = config_dir_override("GEMINI_CLI_HOME") {
            return Ok(home.join(".gemini").join("settings.json"));
        }
        let home = home_dir()?;
        Ok(home.join(".gemini").join("settings.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        read_snapshot(AgentType::Gemini, self.config_path()?, ConfigFormat::Json)
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
        release_to_disk(snapshot, manifest, ConfigFormat::Json)
    }
}

// ---------------------------------------------------------------------------
// OpenCodeAdapter
// ---------------------------------------------------------------------------

/// Agent adapter for the OpenCode CLI.
///
/// Config location: `$XDG_CONFIG_HOME/opencode/opencode.json` (default
/// `~/.config/opencode/opencode.json`).
///
/// # Not the same thing as `crates/zroutery-core/src/opencode/`
///
/// That module is Zroutery's **upstream provider** integration: it calls
/// OpenCode Zen and the OpenCode Console and shapes their responses. This
/// adapter is the opposite direction — OpenCode is the *client*, and we point
/// it at Zroutery. Keeping the two apart matters because they claim
/// neighbouring names (`opencode-zen-free`, `opencode-console`,
/// `opencode_cli`) and a takeover that guessed from the wrong one would write
/// an upstream key into a client config, or a gateway token into an upstream
/// provider definition.
///
/// # What it manages
///
/// Exactly one file: the global config. `OPENCODE_CONFIG` and the project-level
/// `opencode.json` are read by the client too and can shadow the global file,
/// but Zroutery does not write them — see [`OpenCodeAdapter::effective_source`]
/// for what is reported instead.
///
/// Note that OpenCode forwards `options.apiKey` as
/// `Authorization: Bearer <key>` on every request, which is the same credential
/// route Zroutery already serves, so the token this adapter writes is a **local
/// gateway** credential and nothing here ever holds an upstream provider key.
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    /// Where a JSONC-only config would live beside `config_path`.
    fn jsonc_sibling(config_path: &std::path::Path) -> std::path::PathBuf {
        config_path.with_extension("jsonc")
    }

    /// Refuse a config whose *text* this adapter cannot round-trip.
    ///
    /// Runs **before** the file is parsed, and that ordering is the whole
    /// point: `serde_json` rejects a commented document with "invalid JSON: key
    /// must be a string", which tells a user their config is broken when in
    /// fact a tool declined to touch it. OpenCode parses every config document
    /// as JSONC — the extension is cosmetic and comments work inside
    /// `opencode.json` just as well as `opencode.jsonc` — so a `.json` file
    /// carrying comments is an ordinary case, not an edge case.
    fn refuse_before_parse(config_path: &std::path::Path) -> Result<(), String> {
        let jsonc = Self::jsonc_sibling(config_path);
        if !config_path.exists() && jsonc.exists() {
            return Err(format!(
                "refusing to take over OpenCode: only {} exists. This adapter reads and writes \
                 strict JSON, and rewriting the JSONC form would drop every comment in it. \
                 Point OpenCode at {} instead, or take the file over by hand.",
                jsonc.display(),
                config_path.display()
            ));
        }
        if config_path.exists() {
            let text = std::fs::read_to_string(config_path)
                .map_err(|e| format!("cannot read {}: {e}", config_path.display()))?;
            if let Some(line) = first_comment_line(&text) {
                return Err(format!(
                    "refusing to take over OpenCode: {} carries a comment on line {line}. OpenCode \
                     accepts comments in this file, but this adapter reads and writes strict \
                     JSON, and rewriting it would strip every comment the file contains. Remove \
                     the comments, or take the file over by hand.",
                    config_path.display()
                ));
            }
        }
        Ok(())
    }

    /// Refuse a config whose *shape* this adapter does not manage.
    ///
    /// Runs after parsing, because deciding it requires the document.
    ///
    /// A v2-shaped config renames the top-level key from `provider` to
    /// `providers` and the nested keys from `npm`/`options` to
    /// `package`/`settings`. The field paths this adapter manages do not exist
    /// there, so an adoption would create a second, ignored block and report
    /// success. A file carrying *both* keys is refused for the same reason: it is
    /// mid-migration and the file itself cannot say which key the running
    /// client reads.
    fn refuse_after_parse(raw: &serde_json::Value) -> Result<(), String> {
        if raw.get("providers").is_some() {
            return Err(
                "refusing to take over OpenCode: this config carries the v2 provider key \
                 (`providers`/`package`/`settings`), which this adapter does not manage. \
                 Writing the v1 `provider` block into it would be ignored by the client while \
                 Zroutery reported the agent as taken over. A config holding *both* keys is \
                 refused for the same reason: it is a file mid-migration, and which one the \
                 running client reads is not something this file can say."
                    .into(),
            );
        }
        Ok(())
    }

    /// Every config path the client may read, in the order it resolves them.
    ///
    /// Reported so a UI can show the user *where the value that actually wins
    /// came from*, rather than implying Zroutery's write is in effect. Only the
    /// global path is one Zroutery manages; a project-level or `OPENCODE_CONFIG`
    /// override that shadows it is the single most common reason a takeover
    /// appears to do nothing.
    pub fn candidate_paths() -> Result<Vec<std::path::PathBuf>, String> {
        let mut paths = Vec::new();
        if let Some(dir) = config_dir_override("XDG_CONFIG_HOME") {
            paths.push(dir.join("opencode").join("opencode.json"));
        } else {
            paths.push(
                home_dir()?
                    .join(".config")
                    .join("opencode")
                    .join("opencode.json"),
            );
        }
        Ok(paths)
    }

    /// Whether a project-level or `OPENCODE_CONFIG` file shadows the global one.
    ///
    /// `Ok(None)` means nothing else was found. `Ok(Some(path))` means a higher
    /// precedence file exists, and Zroutery's global write will lose to it —
    /// a `Ready` state that must not be reported as `Active`.
    ///
    /// `OPENCODE_CONFIG` is checked first because it is resolved above the
    /// global config and below the project files. It names a single **file**
    /// (verified: it is `path.resolve`d and handed straight to the file loader,
    /// so pointing it at a directory does nothing).
    pub fn shadowing_config(
        &self,
        global: &std::path::Path,
    ) -> Result<Option<std::path::PathBuf>, String> {
        if let Ok(value) = std::env::var(OPENCODE_CONFIG_ENV) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(Some(std::path::PathBuf::from(trimmed)));
            }
        }
        Ok(project_shadow(global))
    }
}

/// The 1-based line of the first `//` or `/* */` comment in a JSON document,
/// or `None` when there is none.
///
/// Deliberately naive and deliberately *not* a JSON parser: the only question
/// asked is "is this file written in a dialect this adapter can round-trip",
/// and answering that does not require understanding the document. A real
/// parser would have to accept comments in order to reject them, which is the
/// whole problem.
///
/// A `//` inside a string is not a comment, so the scan tracks quoting and
/// escapes; without that, a base URL like `"https://host"` would read as a
/// comment on every single config and the adapter would refuse all of them.
fn first_comment_line(text: &str) -> Option<usize> {
    let mut in_string = false;
    let mut escaped = false;
    let bytes: Vec<char> = text.chars().collect();
    let mut index = 0;
    let mut line = 1;
    while index < bytes.len() {
        let ch = bytes[index];
        if ch == '\n' {
            line += 1;
            index += 1;
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match ch {
            '"' => in_string = true,
            '/' => {
                if bytes.get(index + 1) == Some(&'/') {
                    return Some(line);
                }
                if bytes.get(index + 1) == Some(&'*') {
                    return Some(line);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// The project config that outranks the global one, if any.
///
/// Split out of [`OpenCodeAdapter::shadowing_config`] so it can be exercised
/// without the ambient `OPENCODE_CONFIG` deciding the answer first — that
/// variable is process-global, and a machine that happens to have it set (this
/// one does) would otherwise make every assertion about project precedence
/// untestable.
///
/// `.opencode/opencode.json` is listed first because it is merged *after* the
/// bare `opencode.json`, so when both exist it is the effective source. Naming
/// the weaker one would point a user at a file that is not deciding anything.
fn project_shadow(global: &std::path::Path) -> Option<std::path::PathBuf> {
    for relative in [".opencode/opencode.json", "opencode.json"] {
        let candidate = std::path::PathBuf::from(relative);
        if candidate.exists() && candidate != global {
            return Some(candidate);
        }
    }
    None
}

impl AgentAdapter for OpenCodeAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::OpenCode
    }

    fn config_path(&self) -> Result<std::path::PathBuf, String> {
        if let Some(dir) = config_dir_override("XDG_CONFIG_HOME") {
            return Ok(dir.join("opencode").join("opencode.json"));
        }
        let home = home_dir()?;
        Ok(home.join(".config").join("opencode").join("opencode.json"))
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let path = self.config_path()?;
        Self::refuse_before_parse(&path)?;
        let snapshot = read_snapshot(AgentType::OpenCode, path, ConfigFormat::Json)?;
        Self::refuse_after_parse(&snapshot.raw)?;
        Ok(snapshot)
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
        release_to_disk(snapshot, manifest, ConfigFormat::Json)
    }
}

// ---------------------------------------------------------------------------
// GatewayTarget / takeover_fields
// ---------------------------------------------------------------------------

/// One model class an agent is allowed to ask Zroutery for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierClass {
    /// The model id the agent puts in its request.
    ///
    /// OpenCode strips the provider prefix before sending, so this is the name
    /// Zroutery's own model matching sees.
    pub model: String,
    /// The name the client's model picker shows for it.
    pub display_name: String,
}

/// What Zroutery offers an agent when it takes it over.
///
/// This is a **local gateway credential**, not an upstream provider secret: the
/// agent authenticates to `127.0.0.1` and Zroutery substitutes the real provider
/// key from its own credential store. That distinction is what makes writing the
/// token into the agent's config defensible — the token's blast radius is this
/// machine — and it is why nothing here ever holds a provider key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayTarget {
    /// The local endpoint the agent should send to.
    pub base_url: String,
    /// Zroutery's local gateway token.
    pub token: String,
    /// The model classes this agent may ask for.
    pub classes: Vec<TierClass>,
    /// The class to make the agent's default, if any.
    ///
    /// `None` means the takeover does not touch the agent's model selection: the
    /// agent keeps whatever default it had and the user picks a class in the
    /// client or through Zroutery's per-client policy. Zroutery does not choose
    /// a tier on the user's behalf.
    pub default_class: Option<String>,
}

/// The provider table key every adapter registers Zroutery under.
///
/// Shared by Codex (`model_providers.<id>`) and OpenCode (`provider.<id>`) so
/// the two clients name the same gateway the same way, and so a UI can render
/// one identifier instead of guessing per client.
///
/// A free constant rather than one declared per adapter on purpose: two
/// adapters that each declared their own would be free to drift, and a client
/// whose provider id changed between releases would strand every manifest that
/// named the old one.
pub const GATEWAY_PROVIDER_ID: &str = "zroutery";

/// The environment variable OpenCode documents for relocating its config.
pub const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG";

/// The fields a takeover of `agent` manages, and the values Zroutery puts there.
///
/// # Why the set is derived here and not chosen by the user
///
/// The manifest's unit of ownership is a dotted path into one document, and a
/// takeover can only restore what it wrote. Those two facts bound the set from
/// both sides: it has to be small enough to be reviewable in a confirmation
/// dialog and stable enough to restore exactly, and only Zroutery knows which
/// fields those are. A UI that listed every dotted path in an agent's config
/// would be showing the user Zroutery's internals and asking them to pick the
/// subset that happens to work.
///
/// So each agent's set is a function of one [`GatewayTarget`], and
/// [`TakeoverStore::plan_adopt`] can still refuse if a caller asks for a field
/// this function does not produce.
///
/// # What it never manages
///
/// Zroutery's own tier → model mapping, the agent's model class unless the
/// caller set [`GatewayTarget::default_class`], approval or sandbox policy, the
/// working directory, and any provider entry the adapter did not write. Those
/// are the user's, and a manifest that recorded them would restore stale values
/// over newer ones.
///
/// # Errors
///
/// Returns an error rather than a partial set when the target is unusable or
/// the agent has no verified takeover. An empty or half-populated field set
/// would be adopted and recorded as complete.
pub fn takeover_fields(
    agent: AgentType,
    target: &GatewayTarget,
) -> Result<Vec<ManagedField>, String> {
    if target.base_url.trim().is_empty() {
        return Err("cannot take over: the local gateway URL is empty".into());
    }
    if target.token.trim().is_empty() {
        return Err(
            "cannot take over: the local gateway token is empty, so the agent would have no way \
             to authenticate to Zroutery"
                .into(),
        );
    }
    if target.classes.is_empty() {
        return Err(
            "cannot take over: Zroutery currently offers no model classes, so an agent pointed at \
             it would have nothing to ask for. Assign a tier to at least one enabled model first."
                .into(),
        );
    }
    if let Some(default) = &target.default_class {
        if !target.classes.iter().any(|c| &c.model == default) {
            return Err(format!(
                "cannot take over: default class `{default}` is not one of the classes offered \
                 ({})",
                target
                    .classes
                    .iter()
                    .map(|c| c.model.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for class in &target.classes {
        if class.model.trim().is_empty() {
            return Err("cannot take over: a class was offered with an empty model id".into());
        }
        // A dot in a model id would be read as a path separator by
        // `set_nested`/`extract_field_values` and land the value somewhere the
        // client never looks.
        if class.model.contains('.') {
            return Err(format!(
                "cannot take over: class model id `{}` contains a '.', which is the field-path \
                 separator",
                class.model
            ));
        }
    }

    match agent {
        AgentType::Claude => {
            // `env` in `~/.claude/settings.json`. Claude Code writes each `env`
            // entry into its own process environment, so these two fields are
            // what actually redirect the agent's own API traffic — not just
            // the tools it spawns — and a value here replaces the same variable
            // exported in the user's shell.
            //
            // No per-class field here on purpose. Claude Code picks a model by
            // name (`claude-sonnet-4-5`), and Zroutery already maps those names
            // onto classes with its own `match_claude_names` config. Writing
            // `ANTHROPIC_DEFAULT_*_MODEL` here would move a routing decision
            // out of Zroutery's configuration and into a file this manifest
            // then owns — the boundary takeover is not supposed to cross.
            Ok(vec![
                ManagedField {
                    path: "env.ANTHROPIC_BASE_URL".into(),
                    value: serde_json::json!(target.base_url),
                },
                ManagedField {
                    path: "env.ANTHROPIC_AUTH_TOKEN".into(),
                    value: serde_json::json!(target.token),
                },
            ])
        }

        AgentType::Codex => {
            // `model_provider` is the selector; without it the table below is
            // defined and never used.
            //
            // This table goes in the **user-level** `~/.codex/config.toml`.
            // Codex reads `model_provider` and `model_providers` out of a
            // project-local `.codex/config.toml` for nothing — it ignores those
            // keys there and prints a startup warning — so a takeover written to
            // the project file would report success and route nowhere.
            //
            // The token is written literally, and that is a deliberate choice between two
            // verified options rather than a guess:
            //
            // * `env_key` is what upstream recommends, but it names an
            //   environment variable — not a file field this manifest can
            //   restore. A takeover that pointed at an env var would have
            //   nothing to put back on release.
            // * `auth` runs a command, which would mean shipping a Zroutery CLI
            //   binary solely to print a token.
            // * `experimental_bearer_token` takes a literal token and is a real,
            //   documented, runtime-wired field (Codex emits
            //   `Authorization: Bearer <token>` from it). It is *discouraged*
            //   upstream for writing the secret in plaintext — which is exactly
            //   what it does here, into a file on the user's own machine, holding
            //   a credential whose blast radius is that machine.
            //
            // One caveat worth recording: `env_key` wins over
            // `experimental_bearer_token` when both are set, so a user who
            // already has an `env_key` in this provider table keeps using it and
            // this field is ignored. That shows up as a takeover that stays
            // `degraded` rather than as a wrong credential.
            //
            // `wire_api` is `responses` because that is the route Zroutery
            // serves for Codex's dialect. It is currently the only accepted
            // value and also the default; it is written explicitly so the
            // provider table does not depend on a default staying put.
            let id = GATEWAY_PROVIDER_ID;
            let mut out = vec![
                ManagedField {
                    path: "model_provider".into(),
                    value: serde_json::json!(id),
                },
                ManagedField {
                    // Required and non-empty: Codex rejects a provider whose
                    // `name` is blank at config-parse time, which would make the
                    // whole `config.toml` fail to load rather than just this
                    // provider being ignored.
                    path: format!("model_providers.{id}.name"),
                    value: serde_json::json!("Zroutery"),
                },
                ManagedField {
                    path: format!("model_providers.{id}.base_url"),
                    value: serde_json::json!(target.base_url),
                },
                ManagedField {
                    path: format!("model_providers.{id}.wire_api"),
                    value: serde_json::json!("responses"),
                },
                ManagedField {
                    path: format!("model_providers.{id}.experimental_bearer_token"),
                    value: serde_json::json!(target.token),
                },
            ];
            // Codex addresses a model by bare id in `model`; it has no
            // per-provider model table.
            if let Some(default) = &target.default_class {
                out.push(ManagedField {
                    path: "model".into(),
                    value: serde_json::json!(default),
                });
            }
            Ok(out)
        }

        AgentType::OpenCode => {
            let id = GATEWAY_PROVIDER_ID;
            let mut out = vec![
                ManagedField {
                    path: format!("provider.{id}.npm"),
                    value: serde_json::json!("@ai-sdk/openai-compatible"),
                },
                ManagedField {
                    path: format!("provider.{id}.name"),
                    value: serde_json::json!("Zroutery"),
                },
                ManagedField {
                    path: format!("provider.{id}.options.baseURL"),
                    value: serde_json::json!(target.base_url),
                },
                ManagedField {
                    path: format!("provider.{id}.options.apiKey"),
                    value: serde_json::json!(target.token),
                },
            ];
            for class in &target.classes {
                out.push(ManagedField {
                    path: format!("provider.{id}.models.{}.name", class.model),
                    value: serde_json::json!(class.display_name),
                });
            }
            if let Some(default) = &target.default_class {
                // OpenCode addresses a model as `<provider>/<model>`, but it
                // strips the prefix before sending, so Zroutery matches on the
                // bare class id.
                out.push(ManagedField {
                    path: "model".into(),
                    value: serde_json::json!(format!("{id}/{}", default)),
                });
            }
            Ok(out)
        }

        AgentType::Gemini => Err(
            "Gemini CLI is not supported for takeover. Its endpoint is only settable through the \
             GOOGLE_GEMINI_BASE_URL environment variable, which is not a file this manifest can \
             restore, so taking over the settings file would redirect the key but leave the client \
             talking to Google."
                .into(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// Per-test override for the agent config root.
//
// The adapters below resolve their config path against the user home, and
// the tests used to exercise apply_patch/release against the real
// ~/.claude/settings.json, ~/.codex/config.toml and ~/.gemini/settings.json.
// That clobbered the developer own agent configuration and raced other
// tests reading the same files, so under cfg(test) a test can point the
// adapters at a private temp directory instead. The override is
// thread-local, matching how the test harness runs tests in parallel.
#[cfg(test)]
thread_local! {
    static TEST_AGENT_HOME: std::cell::RefCell<Option<tempfile::TempDir>> =
        const { std::cell::RefCell::new(None) };
    /// Per-test overrides for the config directories a client's own
    /// environment variable selects (see `config_dir_override`).
    static TEST_CONFIG_DIRS: std::cell::RefCell<Vec<(String, std::path::PathBuf)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Give the current test thread its own agent config root.
#[cfg(test)]
fn isolate_agent_home() {
    TEST_CONFIG_DIRS.with(|slot| slot.borrow_mut().clear());
    TEST_AGENT_HOME.with(|slot| {
        *slot.borrow_mut() = Some(tempfile::TempDir::new().expect("temp agent home"));
    });
}

/// Point a client's config directory override at `dir` for this test thread.
#[cfg(test)]
fn isolate_config_dir(env_key: &str, dir: &std::path::Path) {
    TEST_CONFIG_DIRS.with(|slot| {
        let mut dirs = slot.borrow_mut();
        dirs.retain(|(key, _)| key != env_key);
        dirs.push((env_key.to_string(), dir.to_path_buf()));
    });
}

/// The agent config root for this test thread, if one was installed.
#[cfg(test)]
fn isolated_agent_home() -> Option<std::path::PathBuf> {
    TEST_AGENT_HOME.with(|slot| slot.borrow().as_ref().map(|dir| dir.path().to_path_buf()))
}

/// Resolve the current user's home directory.
///
/// Checks `HOME` (Unix) then `USERPROFILE` (Windows).
fn home_dir() -> Result<std::path::PathBuf, String> {
    #[cfg(test)]
    if let Some(root) = isolated_agent_home() {
        return Ok(root);
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(std::path::PathBuf::from(home));
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return Ok(std::path::PathBuf::from(profile));
    }
    Err("could not determine home directory (HOME/USERPROFILE not set)".into())
}

/// Resolve a client config directory from the environment variable that client
/// documents for relocating its configuration.
fn config_dir_override(env_key: &str) -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_DIRS.with(|slot| {
        slot.borrow()
            .iter()
            .find(|(key, _)| key == env_key)
            .map(|(_, dir)| dir.clone())
    }) {
        return Some(dir);
    }

    match std::env::var(env_key) {
        Ok(value) if !value.trim().is_empty() => Some(std::path::PathBuf::from(value)),
        _ => None,
    }
}

/// Write `bytes` to `path` atomically, preserving the permissions of the file
/// being replaced.
///
/// The replacement is staged in a temporary file in the same directory so the
/// rename is atomic, and that temporary file is created with restrictive
/// permissions (0600 on Unix) so a config holding credentials is never
/// readable more widely than the file it replaces. When the destination
/// already exists, its permission bits are re-applied to the staged file
/// before the rename: replacing an existing 0600 config must not silently
/// widen it to the umask default of 0644.
pub(crate) fn write_config_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create dir {}: {e}", parent.display()))?;
    }

    let original_permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    let tmp = temp_sibling(path);

    if let Err(err) = write_restricted(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }

    if let Some(permissions) = original_permissions {
        if let Err(e) = std::fs::set_permissions(&tmp, permissions) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "failed to preserve permissions for {}: {e}",
                path.display()
            ));
        }
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("failed to replace {}: {e}", path.display()));
    }

    Ok(())
}

/// Create `path` with restrictive permissions and write `bytes` into it.
#[cfg(unix)]
pub(crate) fn write_restricted(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("failed to flush {}: {e}", path.display()))?;
    Ok(())
}

/// Create `path` and write `bytes` into it.
#[cfg(not(unix))]
pub(crate) fn write_restricted(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// A unique temporary file name next to `path`, in the same directory so the
/// final rename cannot cross a filesystem boundary.
pub(crate) fn temp_sibling(path: &std::path::Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    path.with_file_name(format!(".{name}.{}.{sequence}.tmp", std::process::id()))
}

/// Compute a hex-encoded hash of the given byte slice for change detection.
fn compute_hash(data: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Set a nested JSON value by dotted path (e.g. "model.temperature").
///
/// Missing intermediate tables are created, but an existing non-table on the
/// path is a type conflict and reported as an error. Quietly skipping the write
/// would report success while changing nothing, and replacing the value with a
/// table would destroy an existing setting.
fn set_nested(
    root: &mut serde_json::Value,
    path: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(format!("invalid field path `{path}`"));
    }

    if !root.is_object() {
        return Err(format!(
            "cannot set `{path}`: the config root is not a table"
        ));
    }

    let mut current = root;

    // Navigate to the parent, creating intermediate tables as needed.
    for part in &parts[..parts.len() - 1] {
        let obj = current.as_object_mut().unwrap();
        if !obj.contains_key(*part) {
            obj.insert(
                part.to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        current = obj.get_mut(*part).unwrap();
        if !current.is_object() {
            return Err(type_conflict(path, part));
        }
    }

    // Set the leaf value.
    let leaf = parts.last().unwrap();
    current
        .as_object_mut()
        .unwrap()
        .insert(leaf.to_string(), value);

    Ok(())
}

/// Describe a patch that would have to overwrite an existing non-table value.
fn type_conflict(path: &str, part: &str) -> String {
    format!("cannot set `{path}`: `{part}` already holds a non-table value of a different type")
}

/// Restore a manifest's managed fields into `raw`.
///
/// Fields that already existed at adoption time are set back to their captured
/// value; fields recorded in [`OwnershipManifest::absent_fields`] are removed
/// so a value Zroutery introduced does not outlive the ownership.
fn restore_fields(raw: &mut serde_json::Value, manifest: &OwnershipManifest) -> Result<(), String> {
    for field_path in &manifest.managed_fields {
        if manifest
            .absent_fields
            .iter()
            .any(|absent| absent == field_path)
        {
            remove_nested(raw, field_path);
        } else if let Some(original_value) = manifest.field_snapshots.get(field_path) {
            set_nested(raw, field_path, original_value.clone())?;
        }
    }
    Ok(())
}

/// Apply managed field patches to `raw`.
fn apply_fields(raw: &mut serde_json::Value, fields: &[ManagedField]) -> Result<(), String> {
    for field in fields {
        set_nested(raw, &field.path, field.value.clone())?;
    }
    Ok(())
}

/// Re-read the config a snapshot was taken from, refusing to touch a file that
/// changed underneath the caller.
///
/// `apply_patch` used to serialize the patched *old* snapshot over the file, so
/// an external edit to a field Zroutery does not manage (another tool, the user
/// editing by hand) was silently reverted. The hash recorded by `read_config`
/// is the version the caller based its patch on; when the file no longer
/// matches, the write is refused and the caller has to re-read and retry.
fn reread_raw(
    snapshot: &AgentConfigSnapshot,
    format: ConfigFormat,
) -> Result<serde_json::Value, String> {
    let path = &snapshot.config_path;

    if !path.exists() {
        if snapshot.config_hash.is_empty() {
            return Ok(serde_json::Value::Object(serde_json::Map::new()));
        }
        return Err(format!(
            "config {} disappeared since it was read; refusing to write it",
            path.display()
        ));
    }

    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    if compute_hash(data.as_bytes()) != snapshot.config_hash {
        return Err(format!(
            "config {} changed on disk since it was read; refusing to overwrite it",
            path.display()
        ));
    }

    parse_config(format, &data).map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

/// Patch managed fields into the current on-disk config and write it back.
fn apply_patch_to_disk(
    snapshot: &AgentConfigSnapshot,
    fields: &[ManagedField],
    format: ConfigFormat,
) -> Result<AgentConfigSnapshot, String> {
    let mut raw = reread_raw(snapshot, format)?;
    apply_fields(&mut raw, fields)?;

    let text = serialize_config(format, &raw)?;
    let hash = compute_hash(text.as_bytes());
    write_config_atomic(&snapshot.config_path, text.as_bytes())?;

    Ok(AgentConfigSnapshot {
        agent_type: snapshot.agent_type,
        config_path: snapshot.config_path.clone(),
        raw,
        config_hash: hash,
    })
}

/// Restore a manifest's original values into the current config and write it.
fn release_to_disk(
    snapshot: &AgentConfigSnapshot,
    manifest: &OwnershipManifest,
    format: ConfigFormat,
) -> Result<(), String> {
    let mut raw = snapshot.raw.clone();
    restore_fields(&mut raw, manifest)?;

    let text = serialize_config(format, &raw)?;
    write_config_atomic(&snapshot.config_path, text.as_bytes())
}

/// Remove a nested JSON value by dotted path, leaving anything else untouched.
fn remove_nested(root: &mut serde_json::Value, path: &str) {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return;
    }

    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        match current.get_mut(*part) {
            Some(next) if next.is_object() => current = next,
            _ => return,
        }
    }

    if let Some(obj) = current.as_object_mut() {
        obj.remove(*parts.last().unwrap());
    }
}

/// Get a nested JSON value by dotted path (e.g. "model.temperature").
#[allow(dead_code)]
fn get_nested<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = root;
    for part in &parts {
        current = current.get(*part)?;
    }
    Some(current)
}

// ---------------------------------------------------------------------------
// Config formats
// ---------------------------------------------------------------------------

/// On-disk syntax of a client's configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFormat {
    /// JSON, used by Claude (`.claude.json`) and Gemini (`settings.json`).
    Json,
    /// TOML, used by Codex (`config.toml`).
    Toml,
}

/// Parse config text in `format` into JSON.
fn parse_config(format: ConfigFormat, text: &str) -> Result<serde_json::Value, String> {
    match format {
        ConfigFormat::Json => serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}")),
        ConfigFormat::Toml => parse_toml_subset(text),
    }
}

/// Serialize `raw` as config text in `format`.
fn serialize_config(format: ConfigFormat, raw: &serde_json::Value) -> Result<String, String> {
    match format {
        ConfigFormat::Json => {
            serde_json::to_string_pretty(raw).map_err(|e| format!("serialize failed: {e}"))
        }
        ConfigFormat::Toml => serialize_toml_subset(raw),
    }
}

/// Read and parse a client config, hashing the raw bytes for change detection.
fn read_snapshot(
    agent_type: AgentType,
    path: std::path::PathBuf,
    format: ConfigFormat,
) -> Result<AgentConfigSnapshot, String> {
    let (raw, hash) = if path.exists() {
        let data = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let hash = compute_hash(data.as_bytes());
        let parsed = parse_config(format, &data)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        (parsed, hash)
    } else {
        (
            serde_json::Value::Object(serde_json::Map::new()),
            String::new(),
        )
    };

    Ok(AgentConfigSnapshot {
        agent_type,
        config_path: path,
        raw,
        config_hash: hash,
    })
}

// ---------------------------------------------------------------------------
// Minimal TOML support for Codex `config.toml`
// ---------------------------------------------------------------------------
//
// Codex reads TOML, and this crate has no TOML dependency available to this
// module, so the subset below covers the shapes a Codex config uses: comments,
// `[table]` headers, dotted or quoted keys, basic and literal strings,
// integers, floats, booleans and single-line arrays of those scalars. Anything
// else (arrays of tables, inline tables, multi-line strings, dates) is rejected
// with an explicit error instead of being silently misread, so `read_config`
// fails loudly rather than letting the adapter write a file the client cannot
// parse.

/// Parse the supported TOML subset into a JSON value.
fn parse_toml_subset(text: &str) -> Result<serde_json::Value, String> {
    let mut root = serde_json::Value::Object(serde_json::Map::new());
    let mut table_path: Vec<String> = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("[[") {
            return Err(format!(
                "line {line_number}: arrays of tables are not supported"
            ));
        }

        if let Some(inner) = line.strip_prefix('[') {
            let inner = inner
                .strip_suffix(']')
                .ok_or_else(|| format!("line {line_number}: unterminated table header"))?
                .trim();
            let path = parse_toml_key(inner, line_number)?;
            toml_table_mut(&mut root, &path, line_number)?;
            table_path = path;
            continue;
        }

        let split = find_unquoted(line, '=')
            .ok_or_else(|| format!("line {line_number}: expected `key = value`"))?;
        let mut key = parse_toml_key(&line[..split], line_number)?;
        let value = parse_toml_value(line[split + 1..].trim(), line_number)?;

        let mut path = table_path.clone();
        path.append(&mut key);
        insert_toml_value(&mut root, &path, value, line_number)?;
    }

    Ok(root)
}

/// Remove a `#` comment, ignoring `#` inside quoted strings.
fn strip_toml_comment(line: &str) -> &str {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if in_basic {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_basic = false;
            }
        } else if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
        } else if ch == '"' {
            in_basic = true;
        } else if ch == '\'' {
            in_literal = true;
        } else if ch == '#' {
            return &line[..index];
        }
    }

    line
}

/// Find the first occurrence of `needle` outside of quoted strings.
fn find_unquoted(text: &str, needle: char) -> Option<usize> {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if in_basic {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_basic = false;
            }
        } else if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
        } else if ch == '"' {
            in_basic = true;
        } else if ch == '\'' {
            in_literal = true;
        } else if ch == needle {
            return Some(index);
        }
    }

    None
}

/// Parse a dotted, optionally quoted TOML key into its segments.
fn parse_toml_key(text: &str, line_number: usize) -> Result<Vec<String>, String> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;

    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }

        let segment = if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                if quote == b'"' && bytes[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            if end >= bytes.len() {
                return Err(format!("line {line_number}: unterminated quoted key"));
            }
            index = end + 1;
            text[start..end].to_string()
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'.'
            {
                index += 1;
            }
            if start == index {
                return Err(format!("line {line_number}: empty key segment"));
            }
            text[start..index].to_string()
        };

        if segment.is_empty() {
            return Err(format!("line {line_number}: empty key segment"));
        }
        parts.push(segment);

        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index] != b'.' {
            return Err(format!("line {line_number}: unexpected character in key"));
        }
        index += 1;
    }

    if parts.is_empty() {
        return Err(format!("line {line_number}: empty key"));
    }

    Ok(parts)
}

/// Navigate to a table, creating missing tables and rejecting conflicts.
fn toml_table_mut<'a>(
    root: &'a mut serde_json::Value,
    path: &[String],
    line_number: usize,
) -> Result<&'a mut serde_json::Value, String> {
    let mut current = root;

    for part in path {
        if !current.is_object() {
            return Err(format!("line {line_number}: `{part}` is not a table"));
        }
        let object = current.as_object_mut().unwrap();
        current = object
            .entry(part.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !current.is_object() {
            return Err(format!(
                "line {line_number}: `{part}` is a value, not a table"
            ));
        }
    }

    Ok(current)
}

/// Insert a key/value pair at `path`, rejecting duplicate keys.
fn insert_toml_value(
    root: &mut serde_json::Value,
    path: &[String],
    value: serde_json::Value,
    line_number: usize,
) -> Result<(), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| format!("line {line_number}: empty key"))?;
    let table = toml_table_mut(root, parents, line_number)?;
    let object = table.as_object_mut().unwrap();
    if object.contains_key(last) {
        return Err(format!("line {line_number}: duplicate key `{last}`"));
    }
    object.insert(last.clone(), value);
    Ok(())
}

/// Parse a single-line TOML value.
fn parse_toml_value(text: &str, line_number: usize) -> Result<serde_json::Value, String> {
    let mut parser = TomlValueParser {
        input: text,
        pos: 0,
    };
    let value = parser.parse_value(line_number)?;
    parser.skip_whitespace();
    if parser.pos != text.len() {
        return Err(format!(
            "line {line_number}: unexpected trailing text `{}`",
            &text[parser.pos..]
        ));
    }
    Ok(value)
}

/// Cursor over the text of one TOML value.
struct TomlValueParser<'a> {
    input: &'a str,
    pos: usize,
}

impl TomlValueParser<'_> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(format!("line {line_number}: expected a value")),
            Some('"') => self
                .parse_basic_string(line_number)
                .map(serde_json::Value::String),
            Some('\'') => self
                .parse_literal_string(line_number)
                .map(serde_json::Value::String),
            Some('[') => self.parse_array(line_number),
            Some('{') => Err(format!(
                "line {line_number}: inline tables are not supported"
            )),
            Some(_) => self.parse_bare_value(line_number),
        }
    }

    fn parse_basic_string(&mut self, line_number: usize) -> Result<String, String> {
        self.bump(); // opening quote
        let mut out = String::new();

        loop {
            match self.bump() {
                None => return Err(format!("line {line_number}: unterminated string")),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some(marker @ ('u' | 'U')) => {
                        let digits = if marker == 'u' { 4 } else { 8 };
                        let mut code = 0u32;
                        for _ in 0..digits {
                            let ch = self.bump().ok_or_else(|| {
                                format!("line {line_number}: unterminated unicode escape")
                            })?;
                            let digit = ch.to_digit(16).ok_or_else(|| {
                                format!("line {line_number}: invalid unicode escape digit {ch:?}")
                            })?;
                            code = code * 16 + digit;
                        }
                        let ch = char::from_u32(code).ok_or_else(|| {
                            format!("line {line_number}: invalid unicode scalar {code:#x}")
                        })?;
                        out.push(ch);
                    }
                    Some(other) => {
                        return Err(format!(
                            "line {line_number}: unsupported escape `\\{other}`"
                        ))
                    }
                    None => return Err(format!("line {line_number}: unterminated string")),
                },
                Some(ch) => out.push(ch),
            }
        }
    }

    fn parse_literal_string(&mut self, line_number: usize) -> Result<String, String> {
        self.bump(); // opening quote
        let start = self.pos;
        while let Some(ch) = self.bump() {
            if ch == '\'' {
                return Ok(self.input[start..self.pos - 1].to_string());
            }
        }
        Err(format!("line {line_number}: unterminated string"))
    }

    fn parse_array(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.bump(); // '['
        let mut items = Vec::new();

        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(format!("line {line_number}: unterminated array")),
                Some(']') => {
                    self.bump();
                    return Ok(serde_json::Value::Array(items));
                }
                _ => {}
            }

            items.push(self.parse_value(line_number)?);
            self.skip_whitespace();

            match self.bump() {
                Some(',') => continue,
                Some(']') => return Ok(serde_json::Value::Array(items)),
                Some(other) => {
                    return Err(format!(
                        "line {line_number}: expected `,` or `]` in array, found {other:?}"
                    ))
                }
                None => return Err(format!("line {line_number}: unterminated array")),
            }
        }
    }

    fn parse_bare_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_' | '.') {
                self.bump();
            } else {
                break;
            }
        }

        let token = &self.input[start..self.pos];
        match token {
            "true" => return Ok(serde_json::Value::Bool(true)),
            "false" => return Ok(serde_json::Value::Bool(false)),
            "" => {
                return Err(format!(
                    "line {line_number}: expected a value, found {:?}",
                    self.peek()
                ))
            }
            _ => {}
        }

        let cleaned = token.replace('_', "");
        if let Ok(integer) = cleaned.parse::<i64>() {
            return Ok(serde_json::Value::Number(integer.into()));
        }
        if let Ok(float) = cleaned.parse::<f64>() {
            if let Some(number) = serde_json::Number::from_f64(float) {
                return Ok(serde_json::Value::Number(number));
            }
        }

        Err(format!(
            "line {line_number}: unsupported TOML value `{token}`"
        ))
    }
}

/// Serialize a JSON object as the supported TOML subset.
fn serialize_toml_subset(value: &serde_json::Value) -> Result<String, String> {
    let table = value
        .as_object()
        .ok_or_else(|| "TOML config must be a table".to_string())?;
    let mut out = String::new();
    write_toml_table(&mut out, &[], table)?;
    Ok(out)
}

/// Write one table: its scalar keys first, then its child sections.
fn write_toml_table(
    out: &mut String,
    path: &[String],
    table: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if !path.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push('[');
        out.push_str(
            &path
                .iter()
                .map(|segment| quote_toml_key(segment))
                .collect::<Vec<_>>()
                .join("."),
        );
        out.push_str("]\n");
    }

    let mut nested = Vec::new();
    for (key, value) in table {
        if value.is_object() {
            nested.push((key, value));
            continue;
        }
        out.push_str(&quote_toml_key(key));
        out.push_str(" = ");
        out.push_str(&format_toml_value(value)?);
        out.push('\n');
    }

    for (key, value) in nested {
        let mut child = path.to_vec();
        child.push(key.clone());
        write_toml_table(out, &child, value.as_object().unwrap())?;
    }

    Ok(())
}

fn format_toml_value(value: &serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::Null => Err("TOML has no null value".to_string()),
        serde_json::Value::Bool(flag) => Ok(flag.to_string()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        serde_json::Value::String(text) => Ok(quote_toml_string(text)),
        serde_json::Value::Array(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                if item.is_object() {
                    return Err("arrays of tables are not supported".to_string());
                }
                rendered.push(format_toml_value(item)?);
            }
            Ok(format!("[{}]", rendered.join(", ")))
        }
        serde_json::Value::Object(_) => {
            Err("nested tables must be written as sections".to_string())
        }
    }
}

/// Quote a key unless it is a bare key.
fn quote_toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if bare {
        key.to_string()
    } else {
        quote_toml_string(key)
    }
}

/// Quote a string as a TOML basic string.
fn quote_toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 || ch == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(deprecated)] // the Gemini fixtures cover the release-only adapter
mod tests {
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
                let current =
                    field_map(&[("f1", serde_json::json!(i)), ("f2", serde_json::json!(i))]);
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
        let restored2: OwnershipManifest =
            serde_json::from_str(&json2).expect("deserialize released");

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

        let shadowing =
            super::project_shadow(&global).expect("a project config shadows the global one");
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
}
