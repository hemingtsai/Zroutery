use std::collections::HashMap;
use std::fmt;

use super::{
    LEGACY_UNVERSIONED_SCHEMA_VERSION, MODEL_CHECKPOINT_SCHEMA_VERSION, MODEL_COMMIT_SCHEMA_VERSION,
};
use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer, Serialize};

// ---------------------------------------------------------------------------
// Versioned identity and deterministic hashing
// ---------------------------------------------------------------------------

pub(super) fn legacy_envelope_version() -> u32 {
    LEGACY_UNVERSIONED_SCHEMA_VERSION
}

pub(super) fn deserialize_supported_envelope_version<'de, D>(
    deserializer: D,
) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == LEGACY_UNVERSIONED_SCHEMA_VERSION
        || version == MODEL_CHECKPOINT_SCHEMA_VERSION
        || version == MODEL_COMMIT_SCHEMA_VERSION
    {
        Ok(version)
    } else {
        Err(SerdeError::custom(format!(
            "unsupported model envelope schema version {version}"
        )))
    }
}

pub(super) const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
pub(super) const COMMIT_ID_DOMAIN: &[u8] = b"zroutery-model-commit-v1\0";

pub(super) fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

pub(super) fn hash_u64(hash: &mut u64, value: u64) {
    hash_bytes(hash, &value.to_le_bytes());
}

pub(super) fn hash_string(hash: &mut u64, value: &str) {
    hash_u64(hash, value.len() as u64);
    hash_bytes(hash, value.as_bytes());
}

pub(super) fn hash_optional_commit(hash: &mut u64, value: Option<&CommitId>) {
    match value {
        Some(commit) => {
            hash_bytes(hash, &[1]);
            hash_string(hash, commit.as_str());
        }
        None => hash_bytes(hash, &[0]),
    }
}

pub(super) fn hash_algorithm_versions(hash: &mut u64, versions: &[(String, String)]) {
    hash_u64(hash, versions.len() as u64);
    let mut ordered: Vec<&(String, String)> = versions.iter().collect();
    ordered.sort();
    for (model, version) in ordered {
        hash_string(hash, model);
        hash_string(hash, version);
    }
}

pub(super) fn hash_metadata(hash: &mut u64, metadata: &HashMap<String, String>) {
    hash_u64(hash, metadata.len() as u64);
    let mut ordered: Vec<(&String, &String)> = metadata.iter().collect();
    ordered.sort();
    for (key, value) in ordered {
        hash_string(hash, key);
        hash_string(hash, value);
    }
}
// ---------------------------------------------------------------------------
// ModelId — newtype String
// ---------------------------------------------------------------------------

/// Identifies a routing model by name (e.g. "success", "latency").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(String);

impl ModelId {
    pub fn success() -> Self {
        Self("success".into())
    }

    pub fn latency() -> Self {
        Self("latency".into())
    }

    pub fn ttft() -> Self {
        Self("ttft".into())
    }

    pub fn cost() -> Self {
        Self("cost".into())
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// CommitId — content-addressed hex string
// ---------------------------------------------------------------------------

/// Content-addressed commit identifier (16-char hex string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommitId(String);

impl CommitId {
    pub fn from_hash(hash: u64) -> Self {
        Self(format!("{:016x}", hash))
    }

    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
