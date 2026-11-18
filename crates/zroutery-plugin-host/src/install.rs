//! In-memory immutable snapshots. No extraction onto user-controlled paths.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use zroutery_plugin_api::manifest::PluginManifestV1;
use zroutery_plugin_api::options::OptionsSchemaV1;
use zroutery_plugin_api::{strict, PluginError, Result};

pub const MAX_ARCHIVE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FILE_BYTES: usize = 256 * 1024;
pub const MAX_PACKAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FILES: usize = 128;

#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: PluginManifestV1,
    /// Decoded once at construction; schema validity is a package property,
    /// not a per-save surprise. `None` when the manifest declares no schema.
    pub schema: Option<OptionsSchemaV1>,
    pub digest: String,
    pub files: BTreeMap<String, Vec<u8>>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedFiles {
    pub files: BTreeMap<String, String>,
}
impl Package {
    pub fn archive_bytes(&self) -> Result<Vec<u8>> {
        // All v1 package resources are UTF-8 source/data. Persisting them as
        // strings avoids an unbounded JSON array per input byte on recovery.
        let mut files = BTreeMap::new();
        for (path, bytes) in &self.files {
            files.insert(
                path.clone(),
                std::str::from_utf8(bytes)
                    .map_err(|_| PluginError::InvalidManifest)?
                    .to_string(),
            );
        }
        serde_json::to_vec(&ArchivedFiles { files }).map_err(|_| PluginError::StorageUnavailable)
    }
    pub fn from_archived_bytes(bytes: &[u8], bundled: bool) -> Result<Self> {
        let archived: ArchivedFiles = strict::decode_package(bytes, MAX_PACKAGE_BYTES * 2)?;
        Self::from_files(
            archived
                .files
                .into_iter()
                .map(|(path, text)| (path, text.into_bytes()))
                .collect(),
            bundled,
        )
    }
}
impl Package {
    pub fn from_archive(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(PluginError::InvalidManifest);
        }
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| PluginError::InvalidManifest)?;
        if archive.len() > MAX_FILES {
            return Err(PluginError::InvalidManifest);
        }
        let mut files = BTreeMap::new();
        let mut folded = BTreeSet::new();
        let mut total = 0usize;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|_| PluginError::InvalidManifest)?;
            let name = entry
                .name()
                .map_err(|_| PluginError::InvalidManifest)?
                .to_string();
            // Only regular files: links, encrypted entries and directory aliases
            // cannot become a second loader path. ZIP attributes are untrusted.
            if !strict::package_path(&name)
                || entry.is_dir()
                || entry.encrypted()
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
                || !folded.insert(name.to_ascii_lowercase())
                || entry.size() > MAX_FILE_BYTES as u64
                || entry.size() > entry.compressed_size().max(1) * 100
            {
                return Err(PluginError::InvalidManifest);
            }
            let mut data = Vec::new();
            entry
                .by_ref()
                .take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut data)
                .map_err(|_| PluginError::InvalidManifest)?;
            total = total
                .checked_add(data.len())
                .ok_or(PluginError::InvalidManifest)?;
            if data.len() > MAX_FILE_BYTES
                || data.len() as u64 != entry.size()
                || total > MAX_PACKAGE_BYTES
            {
                return Err(PluginError::InvalidManifest);
            }
            files.insert(name, data);
        }
        Self::from_files(files, false)
    }

    /// Caller supplies bytes, never a mutable directory to execute later.
    /// Official reserved IDs are reachable only through compiled bundled bytes.
    pub fn from_files(files: BTreeMap<String, Vec<u8>>, bundled: bool) -> Result<Self> {
        let mut total = 0usize;
        let mut folded = BTreeSet::new();
        if files.len() > MAX_FILES {
            return Err(PluginError::InvalidManifest);
        }
        for (name, data) in &files {
            total = total
                .checked_add(data.len())
                .ok_or(PluginError::InvalidManifest)?;
            if !strict::package_path(name)
                || !folded.insert(name.to_ascii_lowercase())
                || data.len() > MAX_FILE_BYTES
                || total > MAX_PACKAGE_BYTES
                || !(name.ends_with(".mjs")
                    || name.ends_with(".json")
                    || matches!(name.as_str(), "LICENSE" | "NOTICE"))
            {
                return Err(PluginError::InvalidManifest);
            }
        }
        let manifest = PluginManifestV1::decode(
            files
                .get("zroutery.plugin.json")
                .ok_or(PluginError::InvalidManifest)?,
        )?;
        if manifest.id.starts_with("org.zroutery.") && !bundled {
            return Err(PluginError::PermissionDenied);
        }
        semver::Version::parse(&manifest.version).map_err(|_| PluginError::InvalidManifest)?;
        for contribution in manifest
            .contributions
            .themes
            .iter()
            .chain(&manifest.contributions.transport_profiles)
        {
            if !files.contains_key(&contribution.file) {
                return Err(PluginError::InvalidManifest);
            }
        }
        for theme in &manifest.contributions.themes {
            zroutery_plugin_api::theme::ThemeV1::decode(&files[&theme.file])?;
        }
        for profile in &manifest.contributions.transport_profiles {
            let profile: zroutery_plugin_api::transport::TransportProfileV1 =
                strict::decode(&files[&profile.file], MAX_FILE_BYTES)?;
            profile.validate()?;
        }
        let schema = match &manifest.options_schema {
            Some(path) => {
                let schema =
                    OptionsSchemaV1::decode(files.get(path).ok_or(PluginError::InvalidManifest)?)?;
                Some(schema)
            }
            None => None,
        };
        if let Some(runtime) = &manifest.runtime {
            for entry in [&runtime.entries.control, &runtime.entries.ui]
                .into_iter()
                .flatten()
            {
                if !files.contains_key(entry) {
                    return Err(PluginError::InvalidManifest);
                }
            }
        }
        // Length framing prevents concatenation collisions, BTreeMap supplies
        // canonical order, and every byte (including unused ESM) is covered.
        let mut digest = Sha256::new();
        for (name, data) in &files {
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            digest.update((data.len() as u64).to_le_bytes());
            digest.update(data);
        }
        Ok(Self {
            manifest,
            schema,
            digest: format!("{:x}", digest.finalize()),
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn rejects_zip_slip_case_aliases_links_and_bytecode_before_manifest() {
        for names in [
            vec!["../outside"],
            vec!["a.json", "A.json"],
            vec!["x.mjs:ads"],
            vec!["CON.json"],
            vec!["evil.qbc"],
        ] {
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for name in names {
                archive
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                archive.write_all(b"{}").unwrap();
            }
            let bytes = archive.finish().unwrap().into_inner();
            assert!(Package::from_archive(&bytes).is_err());
        }
    }
}
