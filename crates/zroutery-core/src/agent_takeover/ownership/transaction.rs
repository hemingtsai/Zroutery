//! Non-publishing ownership plans for a multi-file client setup transaction.

use super::*;
use crate::file_transaction::FileChange;

pub struct PreparedOwnership {
    pub manifest: OwnershipManifest,
    pub file: Option<FileChange>,
}

pub struct PreparedRelease {
    pub ownership: PreparedOwnership,
    pub effective: OwnershipManifest,
    pub conflicts: Vec<FieldConflict>,
    pub report: ResolutionReport,
}

impl TakeoverStore {
    fn prepared_manifest(&self, manifest: OwnershipManifest) -> Result<PreparedOwnership, String> {
        let file = match &self.path {
            None => None,
            Some(path) => {
                let before = match std::fs::read(path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(format!("cannot stage client ownership: {error}")),
                };
                let persisted: Option<OwnershipManifest> = before
                    .as_deref()
                    .map(serde_json::from_slice)
                    .transpose()
                    .map_err(|error| format!("cannot read client ownership: {error}"))?;
                if persisted != self.manifest() {
                    return Err("client ownership changed on disk; refresh before applying".into());
                }
                Some(FileChange {
                    path: path.clone(),
                    before,
                    after: Some(
                        serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
                    ),
                })
            }
        };
        Ok(PreparedOwnership { manifest, file })
    }

    /// Capture a live backup before a switch. A user edit to a managed field
    /// becomes its restore value after explicit confirmation; unmanaged fields
    /// are carried by the adapter's fresh snapshot, never by an old whole file.
    pub fn prepare_adoption(
        &self,
        fields: &[String],
        current: &HashMap<String, serde_json::Value>,
        applied: &HashMap<String, serde_json::Value>,
    ) -> Result<PreparedOwnership, String> {
        self.plan_adopt(fields, current, applied)?;
        let inner = crate::sync::lock(&self.inner);
        if inner.state == OwnershipState::Releasing {
            return Err("client release is already in progress".into());
        }
        let held = inner.state == OwnershipState::Adopted;
        let mut manifest = if held {
            inner
                .manifest
                .clone()
                .ok_or("client ownership is missing")?
        } else {
            OwnershipManifest {
                state: OwnershipState::Adopted,
                managed_fields: Vec::new(),
                field_snapshots: HashMap::new(),
                absent_fields: Vec::new(),
                adopted_at: None,
                released_at: None,
                generation: inner.generation,
                last_applied: HashMap::new(),
            }
        };
        for field in fields {
            let previous = manifest.last_applied.get(field);
            let live = current.get(field);
            let was_removed = field.starts_with("env.")
                && previous.is_some_and(serde_json::Value::is_null)
                && live.is_none();
            if !manifest.managed_fields.contains(field)
                || (held && previous != live && !was_removed)
            {
                manifest.field_snapshots.remove(field);
                manifest.absent_fields.retain(|absent| absent != field);
                if let Some(value) = live {
                    manifest
                        .field_snapshots
                        .insert(field.clone(), value.clone());
                } else {
                    manifest.absent_fields.push(field.clone());
                }
            }
            if !manifest.managed_fields.contains(field) {
                manifest.managed_fields.push(field.clone());
            }
        }
        manifest.state = OwnershipState::Adopted;
        manifest.adopted_at = Some(chrono::Utc::now().timestamp());
        manifest.released_at = None;
        manifest.generation = manifest
            .generation
            .checked_add(1)
            .ok_or("client generation exhausted")?;
        manifest.last_applied.extend(applied.clone());
        drop(inner);
        self.prepared_manifest(manifest)
    }

    /// A release is not published until its file restores and this manifest are
    /// committed together. Failure while planning leaves ownership untouched.
    pub fn prepare_release_transaction(
        &self,
        current: &HashMap<String, serde_json::Value>,
        strategy: ConflictResolution,
    ) -> Result<PreparedRelease, String> {
        if self.state() != OwnershipState::Adopted {
            return Err("client is not currently adopted".into());
        }
        let conflicts = self.conflicts(current);
        let (effective, report) = self.apply_resolution(&conflicts, strategy, current);
        let mut manifest = self.manifest().ok_or("client ownership is missing")?;
        manifest.state = OwnershipState::Released;
        manifest.released_at = Some(chrono::Utc::now().timestamp());
        manifest.generation = manifest
            .generation
            .checked_add(1)
            .ok_or("client generation exhausted")?;
        Ok(PreparedRelease {
            ownership: self.prepared_manifest(manifest)?,
            effective,
            conflicts,
            report,
        })
    }

    /// Reload after recovery/commit. Never publish a not-yet-durable plan into
    /// the cached state that status reads.
    pub fn reload(&self) -> Result<(), String> {
        if self.path.is_none() {
            return Err("cannot reload memory-only client ownership".into());
        }
        let manifest = self.read_persisted()?;
        let mut inner = crate::sync::lock(&self.inner);
        inner.state = match &manifest {
            None => OwnershipState::Verified,
            Some(manifest) if manifest.state == OwnershipState::Released => {
                OwnershipState::Released
            }
            Some(_) => OwnershipState::Adopted,
        };
        inner.generation = manifest.as_ref().map_or(0, |manifest| manifest.generation);
        inner.last_applied = manifest
            .as_ref()
            .map(|manifest| manifest.last_applied.clone())
            .unwrap_or_default();
        inner.manifest = manifest;
        Ok(())
    }
}
