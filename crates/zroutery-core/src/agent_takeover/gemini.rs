//! Gemini CLI's composite takeover adapter.
//!
//! Gemini CLI v0.66.0 reads user settings from `settings.json`, but its API
//! endpoint and key from the sibling `.env`. A truthful takeover therefore
//! snapshots, compares and publishes both files as one unit.

use std::collections::{BTreeMap, HashMap};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::clients::{AgentAdapter, AgentConfigSnapshot, AgentType, ManagedField};
use super::fs::{compute_hash, remove_nested, set_nested};
use super::ownership::OwnershipManifest;
use crate::file_transaction::{self, FileChange};

const SETTINGS_PREFIX: &str = "settings.";
const ENV_PREFIX: &str = "env.";
const MODEL_ENV: &str = "GEMINI_MODEL";
const ORIGIN_VERSION: u32 = 1;

/// Adapter for Gemini CLI v0.66.0's user-level configuration.
pub struct GeminiAdapter;

impl GeminiAdapter {
    fn env_path(settings_path: &Path) -> Result<PathBuf, String> {
        settings_path
            .parent()
            .map(|parent| parent.join(".env"))
            .ok_or_else(|| {
                format!(
                    "Gemini settings path {} has no parent directory",
                    settings_path.display()
                )
            })
    }

    fn journal_path(settings_path: &Path) -> Result<PathBuf, String> {
        settings_path
            .parent()
            .map(|parent| parent.join(".zroutery").join("transactions"))
            .ok_or_else(|| {
                format!(
                    "Gemini settings path {} has no parent directory",
                    settings_path.display()
                )
            })
    }

    fn origin_path(settings_path: &Path) -> Result<PathBuf, String> {
        settings_path
            .parent()
            .map(|parent| parent.join(".zroutery").join("gemini-origin.json"))
            .ok_or_else(|| {
                format!(
                    "Gemini settings path {} has no parent directory",
                    settings_path.display()
                )
            })
    }

    fn read_pair(&self) -> Result<CompositeFiles, String> {
        let settings_path = self.config_path()?;
        let env_path = Self::env_path(&settings_path)?;
        let origin_path = Self::origin_path(&settings_path)?;
        let settings_bytes = read_optional(&settings_path, "Gemini settings")?;
        let env_bytes = read_optional(&env_path, "Gemini environment")?;
        let origin_bytes = read_optional(&origin_path, "Gemini takeover origin")?;
        let origin = origin_bytes
            .as_deref()
            .map(|bytes| {
                serde_json::from_slice::<OriginMetadata>(bytes).map_err(|error| {
                    format!(
                        "failed to parse Gemini takeover origin {}: {error}",
                        origin_path.display()
                    )
                })
            })
            .transpose()?;
        if origin
            .as_ref()
            .is_some_and(|metadata| metadata.version != ORIGIN_VERSION)
        {
            return Err(format!(
                "Gemini takeover origin {} has an unsupported version",
                origin_path.display()
            ));
        }

        let settings = match settings_bytes.as_deref() {
            Some(bytes) => parse_settings(&settings_path, bytes)?,
            None => Value::Object(Map::new()),
        };
        let env_file = DotenvFile::parse(&env_path, env_bytes.as_deref())?;
        let env = env_file.effective_values();

        Ok(CompositeFiles {
            settings_path,
            env_path,
            origin_path,
            settings_bytes,
            env_bytes,
            origin_bytes,
            origin,
            settings,
            env_file,
            env,
        })
    }

    fn prepare_patch_inner(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<(AgentConfigSnapshot, Vec<FileChange>), String> {
        validate_snapshot(snapshot)?;
        let mut files = self.read_pair()?;
        validate_snapshot_pair(snapshot, &files)?;
        validate_patch_fields(fields)?;

        for field in fields {
            if let Some(path) = field.path.strip_prefix(SETTINGS_PREFIX) {
                set_nested(&mut files.settings, path, field.value.clone())?;
                continue;
            }
            if let Some(key) = field.path.strip_prefix(ENV_PREFIX) {
                if field.value.is_null() {
                    files.env_file.remove_all(key)?;
                } else {
                    let value = field.value.as_str().ok_or_else(|| {
                        format!(
                            "Gemini environment field `{}` must be a string or null",
                            field.path
                        )
                    })?;
                    files.env_file.set_last(key, value)?;
                }
                continue;
            }
            return Err(format!(
                "unsupported Gemini managed field `{}`; expected `settings.` or `env.` prefix",
                field.path
            ));
        }

        let settings_after = serde_json::to_vec_pretty(&files.settings)
            .map_err(|error| format!("failed to serialize Gemini settings: {error}"))?;
        let env_after = files.env_file.render();
        let next_env = files.env_file.effective_values();
        let raw = composite_raw(files.settings.clone(), &next_env);
        let env_target = if env_after.is_empty() {
            None
        } else {
            Some(env_after.as_slice())
        };
        let origin_after = match &files.origin_bytes {
            Some(bytes) => bytes.clone(),
            None => serde_json::to_vec_pretty(&OriginMetadata {
                version: ORIGIN_VERSION,
                settings_present: files.settings_bytes.is_some(),
                env_present: files.env_bytes.is_some(),
            })
            .map_err(|error| format!("failed to serialize Gemini takeover origin: {error}"))?,
        };
        let config_hash = composite_hash(
            &files.settings_path,
            Some(&settings_after),
            &files.env_path,
            env_target,
            &files.origin_path,
            Some(&origin_after),
        );
        let next = AgentConfigSnapshot {
            agent_type: AgentType::Gemini,
            config_path: files.settings_path.clone(),
            raw,
            config_hash,
        };
        let changes = vec![
            FileChange {
                path: files.env_path,
                before: files.env_bytes,
                after: (!env_after.is_empty()).then_some(env_after),
            },
            FileChange {
                path: files.settings_path,
                before: files.settings_bytes,
                after: Some(settings_after),
            },
            FileChange {
                path: files.origin_path,
                before: files.origin_bytes,
                after: Some(origin_after),
            },
        ];
        Ok((next, changes))
    }

    fn prepare_release_inner(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<Vec<FileChange>, String> {
        validate_snapshot(snapshot)?;
        let mut files = self.read_pair()?;
        validate_snapshot_pair(snapshot, &files)?;

        let mut env_touched = false;
        for path in &manifest.managed_fields {
            let logical = release_path(path);
            if let Some(settings_path) = logical.strip_prefix(SETTINGS_PREFIX) {
                if manifest.absent_fields.iter().any(|absent| absent == path) {
                    remove_nested(&mut files.settings, settings_path);
                } else if let Some(original) = manifest.field_snapshots.get(path) {
                    set_nested(&mut files.settings, settings_path, original.clone())?;
                }
                continue;
            }
            if let Some(key) = logical.strip_prefix(ENV_PREFIX) {
                env_touched = true;
                if manifest.absent_fields.iter().any(|absent| absent == path) {
                    files.env_file.remove_all(key)?;
                } else if let Some(original) = manifest.field_snapshots.get(path) {
                    let value = original.as_str().ok_or_else(|| {
                        format!(
                            "cannot restore Gemini environment field `{path}`: the manifest value is not a string"
                        )
                    })?;
                    files.env_file.set_last(key, value)?;
                }
                // No snapshot and no absence marker means conflict resolution
                // chose an externally removed value. Leaving it untouched is
                // the requested KeepExternal result.
                continue;
            }
            return Err(format!("unsupported Gemini manifest field `{path}`"));
        }

        let settings_after = serde_json::to_vec_pretty(&files.settings)
            .map_err(|error| format!("failed to serialize Gemini settings: {error}"))?;
        let settings_after = if files
            .settings
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
            && (files.settings_bytes.is_none()
                || files
                    .origin
                    .as_ref()
                    .is_some_and(|origin| !origin.settings_present))
        {
            None
        } else {
            Some(settings_after)
        };
        let mut changes = vec![FileChange {
            path: files.settings_path,
            before: files.settings_bytes,
            after: settings_after,
        }];
        if env_touched {
            let env_after = files.env_file.render();
            let env_after = if env_after.is_empty()
                && files.env_bytes.is_some()
                && files
                    .origin
                    .as_ref()
                    .is_some_and(|origin| origin.env_present)
            {
                Some(Vec::new())
            } else {
                (!env_after.is_empty()).then_some(env_after)
            };
            changes.push(FileChange {
                path: files.env_path,
                before: files.env_bytes,
                after: env_after,
            });
        }
        if files.origin_bytes.is_some() {
            changes.push(FileChange {
                path: files.origin_path,
                before: files.origin_bytes,
                after: None,
            });
        }
        Ok(changes)
    }

    fn recover_direct(&self) -> Result<PathBuf, String> {
        let journal = Self::journal_path(&self.config_path()?)?;
        file_transaction::recover(&journal)?;
        Ok(journal)
    }
}

impl AgentAdapter for GeminiAdapter {
    fn agent_type(&self) -> AgentType {
        AgentType::Gemini
    }

    fn config_path(&self) -> Result<PathBuf, String> {
        AgentType::Gemini.declaration().config_path()
    }

    fn is_detected(&self) -> Result<bool, String> {
        AgentType::Gemini.declaration().binary_detected()
    }

    fn read_config(&self) -> Result<AgentConfigSnapshot, String> {
        let files = self.read_pair()?;
        Ok(AgentConfigSnapshot {
            agent_type: AgentType::Gemini,
            config_path: files.settings_path.clone(),
            raw: composite_raw(files.settings, &files.env),
            config_hash: composite_hash(
                &files.settings_path,
                files.settings_bytes.as_deref(),
                &files.env_path,
                files.env_bytes.as_deref(),
                &files.origin_path,
                files.origin_bytes.as_deref(),
            ),
        })
    }

    fn apply_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<AgentConfigSnapshot, String> {
        let journal = self.recover_direct()?;
        let live = self.read_config()?;
        validate_snapshot(snapshot)?;
        if snapshot.config_path != live.config_path {
            return Err(format!(
                "Gemini settings path changed from {} to {}; refresh the preview",
                snapshot.config_path.display(),
                live.config_path.display()
            ));
        }
        if snapshot.config_hash != live.config_hash {
            return Err(
                "Gemini configuration changed after transaction recovery; refresh the preview"
                    .into(),
            );
        }
        let (next, changes) = self.prepare_patch_inner(&live, fields)?;
        file_transaction::commit(&journal, &changes)?;
        Ok(next)
    }

    fn release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<(), String> {
        let journal = self.recover_direct()?;
        let live = self.read_config()?;
        validate_snapshot(snapshot)?;
        if snapshot.config_path != live.config_path {
            return Err(format!(
                "Gemini settings path changed from {} to {}; refresh the release",
                snapshot.config_path.display(),
                live.config_path.display()
            ));
        }
        if snapshot.config_hash != live.config_hash {
            return Err(
                "Gemini configuration changed after transaction recovery; refresh the release"
                    .into(),
            );
        }
        let changes = self.prepare_release_inner(&live, manifest)?;
        file_transaction::commit(&journal, &changes)
    }

    fn prepare_patch(
        &self,
        snapshot: &AgentConfigSnapshot,
        fields: &[ManagedField],
    ) -> Result<(AgentConfigSnapshot, Vec<FileChange>), String> {
        self.prepare_patch_inner(snapshot, fields)
    }

    fn prepare_release(
        &self,
        snapshot: &AgentConfigSnapshot,
        manifest: &OwnershipManifest,
    ) -> Result<Vec<FileChange>, String> {
        self.prepare_release_inner(snapshot, manifest)
    }

    fn write_config(&self, _snapshot: &AgentConfigSnapshot) -> Result<(), String> {
        Err(
            "Gemini CLI configuration spans settings.json and .env; use apply_patch or a prepared transaction"
                .into(),
        )
    }
}

struct CompositeFiles {
    settings_path: PathBuf,
    env_path: PathBuf,
    origin_path: PathBuf,
    settings_bytes: Option<Vec<u8>>,
    env_bytes: Option<Vec<u8>>,
    origin_bytes: Option<Vec<u8>>,
    origin: Option<OriginMetadata>,
    settings: Value,
    env_file: DotenvFile,
    env: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginMetadata {
    version: u32,
    settings_present: bool,
    env_present: bool,
}

fn validate_snapshot(snapshot: &AgentConfigSnapshot) -> Result<(), String> {
    if snapshot.agent_type != AgentType::Gemini {
        return Err("Gemini adapter received a snapshot for another client".into());
    }
    if !snapshot.raw.is_object()
        || !snapshot.raw.get("settings").is_some_and(Value::is_object)
        || !snapshot.raw.get("env").is_some_and(Value::is_object)
    {
        return Err(
            "Gemini snapshot is not composite; re-read settings.json and .env before applying"
                .into(),
        );
    }
    Ok(())
}

fn validate_snapshot_pair(
    snapshot: &AgentConfigSnapshot,
    files: &CompositeFiles,
) -> Result<(), String> {
    if snapshot.config_path != files.settings_path {
        return Err(format!(
            "Gemini settings path changed from {} to {}; refresh the preview",
            snapshot.config_path.display(),
            files.settings_path.display()
        ));
    }
    let live_hash = composite_hash(
        &files.settings_path,
        files.settings_bytes.as_deref(),
        &files.env_path,
        files.env_bytes.as_deref(),
        &files.origin_path,
        files.origin_bytes.as_deref(),
    );
    if snapshot.config_hash != live_hash {
        return Err(format!(
            "Gemini configuration changed on disk since it was read ({} or {}); refusing to overwrite it",
            files.settings_path.display(),
            files.env_path.display()
        ));
    }
    Ok(())
}

fn validate_patch_fields(fields: &[ManagedField]) -> Result<(), String> {
    const EXPECTED: [(&str, Option<&str>); 5] = [
        (
            "settings.security.auth.selectedType",
            Some("gemini-api-key"),
        ),
        ("settings.model.name", None),
        ("env.GOOGLE_GEMINI_BASE_URL", None),
        ("env.GEMINI_API_KEY", None),
        ("env.GEMINI_MODEL", None),
    ];
    if fields.len() != EXPECTED.len() {
        return Err(format!(
            "Gemini takeover requires exactly {} managed fields, got {}",
            EXPECTED.len(),
            fields.len()
        ));
    }
    let by_path: HashMap<&str, &Value> = fields
        .iter()
        .map(|field| (field.path.as_str(), &field.value))
        .collect();
    if by_path.len() != fields.len() {
        return Err("Gemini takeover contains a duplicate managed field".into());
    }
    for (path, fixed) in EXPECTED {
        let value = by_path
            .get(path)
            .ok_or_else(|| format!("Gemini takeover is missing required field `{path}`"))?;
        if let Some(expected) = fixed {
            if value.as_str() != Some(expected) {
                return Err(format!(
                    "Gemini `{path}` must be `{expected}` for CLI v0.66.0"
                ));
            }
        }
    }
    if !by_path
        .get("settings.model.name")
        .is_some_and(|value| value.as_str().is_some_and(|model| !model.trim().is_empty()))
    {
        return Err("Gemini takeover requires an explicit non-empty default model".into());
    }
    for path in ["env.GOOGLE_GEMINI_BASE_URL", "env.GEMINI_API_KEY"] {
        if !by_path
            .get(path)
            .is_some_and(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()))
        {
            return Err(format!("Gemini `{path}` must be a non-empty string"));
        }
    }
    if !by_path
        .get("env.GEMINI_MODEL")
        .is_some_and(|value| value.is_null())
    {
        return Err(
            "Gemini `env.GEMINI_MODEL` must be null so the settings model is not shadowed".into(),
        );
    }
    Ok(())
}

fn release_path(path: &str) -> String {
    if path.starts_with(SETTINGS_PREFIX) || path.starts_with(ENV_PREFIX) {
        path.to_string()
    } else {
        // Manifests from the old settings-only adapter used unprefixed paths.
        format!("{SETTINGS_PREFIX}{path}")
    }
}

fn read_optional(path: &Path, label: &str) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to read {label} {}: {error}",
            path.display()
        )),
    }
}

fn parse_settings(path: &Path, bytes: &[u8]) -> Result<Value, String> {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(format!(
            "failed to parse {}: JSON byte-order marks are not supported",
            path.display()
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("failed to read {} as UTF-8: {error}", path.display()))?;
    if let Some(line) = super::clients::first_comment_line(text) {
        return Err(format!(
            "refusing to rewrite Gemini settings {}: strict JSON cannot preserve the comment on line {line}",
            path.display()
        ));
    }
    serde_json::from_str(text).map_err(|error| {
        format!(
            "failed to parse {} as strict JSON at line {}, column {}: {}",
            path.display(),
            error.line(),
            error.column(),
            error
        )
    })
}

fn composite_raw(settings: Value, env: &Map<String, Value>) -> Value {
    let mut root = Map::new();
    root.insert("settings".into(), settings);
    root.insert("env".into(), Value::Object(env.clone()));
    Value::Object(root)
}

fn composite_hash(
    settings_path: &Path,
    settings: Option<&[u8]>,
    env_path: &Path,
    env: Option<&[u8]>,
    origin_path: &Path,
    origin: Option<&[u8]>,
) -> String {
    let mut binding = Vec::new();
    bind_file(&mut binding, settings_path, settings);
    bind_file(&mut binding, env_path, env);
    bind_file(&mut binding, origin_path, origin);
    compute_hash(&binding)
}

fn bind_file(binding: &mut Vec<u8>, path: &Path, bytes: Option<&[u8]>) {
    let path = path.to_string_lossy();
    binding.extend_from_slice(&(path.len() as u64).to_le_bytes());
    binding.extend_from_slice(path.as_bytes());
    match bytes {
        Some(bytes) => {
            binding.push(1);
            binding.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            binding.extend_from_slice(bytes);
        }
        None => binding.push(0),
    }
}

#[derive(Clone)]
struct DotenvFile {
    bom: bool,
    default_newline: String,
    missing: bool,
    lines: Vec<DotenvLine>,
}

#[derive(Clone)]
struct DotenvLine {
    text: String,
    ending: String,
}

impl DotenvFile {
    fn parse(path: &Path, bytes: Option<&[u8]>) -> Result<Self, String> {
        let missing = bytes.is_none();
        let bytes = bytes.unwrap_or_default();
        let (bom, bytes) = if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
            (true, rest)
        } else {
            (false, bytes)
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|error| format!("failed to read {} as UTF-8: {error}", path.display()))?;
        let mut lines = Vec::new();
        let mut start = 0;
        for (index, byte) in text.bytes().enumerate() {
            if byte != b'\n' {
                continue;
            }
            let (content_end, ending) = if index > start && text.as_bytes()[index - 1] == b'\r' {
                (index - 1, "\r\n")
            } else {
                (index, "\n")
            };
            lines.push(DotenvLine {
                text: text[start..content_end].to_string(),
                ending: ending.to_string(),
            });
            start = index + 1;
        }
        if start < text.len() {
            lines.push(DotenvLine {
                text: text[start..].to_string(),
                ending: String::new(),
            });
        }
        let default_newline = lines
            .iter()
            .find(|line| !line.ending.is_empty())
            .map(|line| line.ending.clone())
            .unwrap_or_else(|| "\n".to_string());
        let file = Self {
            bom,
            default_newline,
            missing,
            lines,
        };
        let mut multiline = None;
        for line in &file.lines {
            if let Some(quote) = multiline {
                if closes_quote(&line.text, quote) {
                    multiline = None;
                }
                continue;
            }
            if let Some(assignment) = parse_assignment(&line.text)? {
                if is_managed_env(assignment.key) {
                    let _ = parse_env_value(assignment.value, assignment.key)?;
                } else {
                    let quote = assignment
                        .value
                        .chars()
                        .next()
                        .filter(|quote| matches!(quote, '\'' | '"' | '`'));
                    if let Some(quote) = quote {
                        if !closes_quote(&assignment.value[quote.len_utf8()..], quote) {
                            multiline = Some(quote);
                        }
                    }
                }
            }
        }
        if multiline.is_some() {
            return Err(format!("refusing to append Gemini credentials inside an unterminated multiline value in {}", path.display()));
        }
        Ok(file)
    }

    fn effective_values(&self) -> Map<String, Value> {
        let mut values = BTreeMap::new();
        let mut multiline = None;
        for line in &self.lines {
            if let Some(quote) = multiline {
                if closes_quote(&line.text, quote) {
                    multiline = None;
                }
                continue;
            }
            if let Ok(Some(assignment)) = parse_assignment(&line.text) {
                if let Some(quote) = assignment
                    .value
                    .chars()
                    .next()
                    .filter(|quote| matches!(quote, '\'' | '"' | '`'))
                {
                    if !closes_quote(&assignment.value[quote.len_utf8()..], quote) {
                        multiline = Some(quote);
                    }
                }
                match parse_env_value(assignment.value, assignment.key) {
                    Ok(value) => {
                        values.insert(assignment.key.to_string(), Value::String(value));
                    }
                    // Dotenv supports multiline and expansion forms that this
                    // layout editor does not need to interpret. Unmanaged lines
                    // stay byte-for-byte and still appear in the logical view
                    // as their literal source rather than being fabricated by
                    // expansion. Managed fields are validated during parse.
                    Err(_) => {
                        values.insert(
                            assignment.key.to_string(),
                            Value::String(literal_env_source(assignment.value)),
                        );
                    }
                }
            }
        }
        values.into_iter().collect()
    }

    fn set_last(&mut self, key: &str, value: &str) -> Result<(), String> {
        validate_env_key(key)?;
        if value.contains(['\r', '\n', '\0']) {
            return Err(format!(
                "Gemini environment value for `{key}` contains a line break or NUL"
            ));
        }
        let mut last = None;
        let mut multiline = None;
        for (index, current) in self.lines.iter().enumerate() {
            if let Some(quote) = multiline {
                if closes_quote(&current.text, quote) {
                    multiline = None;
                }
                continue;
            }
            if let Some(assignment) = parse_assignment(&current.text)? {
                if let Some(quote) = assignment
                    .value
                    .chars()
                    .next()
                    .filter(|quote| matches!(quote, '\'' | '"' | '`'))
                {
                    if !closes_quote(&assignment.value[quote.len_utf8()..], quote) {
                        multiline = Some(quote);
                    }
                }
                if assignment.key == key {
                    last = Some((index, assignment.value_start, assignment.value_end));
                }
            }
        }
        match last {
            Some((index, value_start, value_end)) => {
                self.lines[index]
                    .text
                    .replace_range(value_start..value_end, &quote_env_value(value));
            }
            None => self.append(format!("{key}={}", quote_env_value(value))),
        }
        Ok(())
    }

    fn remove_all(&mut self, key: &str) -> Result<(), String> {
        validate_env_key(key)?;
        let had_trailing_newline = self
            .lines
            .last()
            .is_some_and(|line| !line.ending.is_empty());
        let mut keep = Vec::with_capacity(self.lines.len());
        let mut multiline = None;
        for line in self.lines.drain(..) {
            if let Some(quote) = multiline {
                if closes_quote(&line.text, quote) {
                    multiline = None;
                }
                keep.push(line);
                continue;
            }
            if let Some(assignment) = parse_assignment(&line.text)? {
                if let Some(quote) = assignment
                    .value
                    .chars()
                    .next()
                    .filter(|quote| matches!(quote, '\'' | '"' | '`'))
                {
                    if !closes_quote(&assignment.value[quote.len_utf8()..], quote) {
                        multiline = Some(quote);
                    }
                }
            }
            if !parse_assignment(&line.text)?.is_some_and(|assignment| assignment.key == key) {
                keep.push(line);
            }
        }
        self.lines = keep;
        if let Some(last) = self.lines.last_mut() {
            if had_trailing_newline {
                if last.ending.is_empty() {
                    last.ending = self.default_newline.clone();
                }
            } else {
                last.ending.clear();
            }
        }
        Ok(())
    }

    fn render(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.bom {
            out.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        for line in &self.lines {
            out.extend_from_slice(line.text.as_bytes());
            out.extend_from_slice(line.ending.as_bytes());
        }
        out
    }

    fn append(&mut self, text: String) {
        let trailing_newline = self
            .lines
            .last()
            .is_some_and(|line| !line.ending.is_empty());
        if let Some(last) = self.lines.last_mut() {
            if last.ending.is_empty() {
                last.ending = self.default_newline.clone();
            }
        }
        let ending = if trailing_newline || self.missing {
            self.default_newline.clone()
        } else {
            String::new()
        };
        self.lines.push(DotenvLine { text, ending });
        self.missing = false;
    }
}

fn literal_env_source(raw: &str) -> String {
    raw.split_once('#')
        .map_or(raw, |(head, _)| head)
        .trim_matches([' ', '\t'])
        .to_string()
}

fn closes_quote(text: &str, quote: char) -> bool {
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quote == '"' {
            escaped = true;
        } else if character == quote {
            return true;
        }
    }
    false
}

struct Assignment<'a> {
    key: &'a str,
    value: &'a str,
    value_start: usize,
    value_end: usize,
}

fn parse_assignment(line: &str) -> Result<Option<Assignment<'_>>, String> {
    let leading = line.len() - line.trim_start_matches([' ', '\t']).len();
    let trimmed = &line[leading..];
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }
    let (rest, rest_start) = if let Some(after) = trimmed.strip_prefix("export") {
        if !after.starts_with([' ', '\t']) {
            (trimmed, leading)
        } else {
            let without_space = after.trim_start_matches([' ', '\t']);
            (without_space, line.len() - without_space.len())
        }
    } else {
        (trimmed, leading)
    };
    let Some(equal) = rest.find('=') else {
        return Ok(None);
    };
    let key = rest[..equal].trim();
    if !valid_env_key(key) {
        return Ok(None);
    }
    let after_equal = &rest[equal + 1..];
    let value = after_equal.trim_start_matches([' ', '\t']);
    let value_start = rest_start + equal + 1 + (after_equal.len() - value.len());
    let value_end = value_start + env_value_end(value);
    Ok(Some(Assignment {
        key,
        value,
        value_start,
        value_end,
    }))
}

fn env_value_end(value: &str) -> usize {
    if let Some(rest) = value.strip_prefix('\'') {
        return rest.find('\'').map_or(value.len(), |end| end + 2);
    }
    if let Some(rest) = value.strip_prefix('"') {
        let mut escaped = false;
        for (index, ch) in rest.char_indices() {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return index + 2;
            }
        }
        return value.len();
    }
    let before_comment = value.split_once('#').map_or(value, |(head, _)| head);
    before_comment.trim_end_matches([' ', '\t']).len()
}

fn parse_env_value(raw: &str, key: &str) -> Result<String, String> {
    let value = raw.trim_end_matches([' ', '\t']);
    let parsed = if let Some(rest) = value.strip_prefix('\'') {
        match rest.find('\'') {
            Some(end) => {
                let trailing = rest[end + 1..].trim_start_matches([' ', '\t']);
                if !trailing.is_empty() && !trailing.starts_with('#') {
                    return Err(format!(
                        "unsupported .env value for `{key}`: content follows the closing quote"
                    ));
                }
                rest[..end].to_string()
            }
            None => {
                return Err(format!(
                    "unsupported .env value for `{key}`: unterminated single quote"
                ))
            }
        }
    } else if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.char_indices();
        let mut closed = false;
        let mut closing_end = 0;
        while let Some((index, ch)) = chars.next() {
            if ch == '"' {
                closed = true;
                closing_end = index + ch.len_utf8();
                break;
            }
            if ch == '\\' {
                let Some((_, escaped)) = chars.next() else {
                    return Err(format!(
                        "unsupported .env value for `{key}`: trailing escape in double quotes"
                    ));
                };
                match escaped {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '$' => out.push('$'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else {
                out.push(ch);
            }
        }
        if !closed {
            return Err(format!(
                "unsupported .env value for `{key}`: unterminated double quote"
            ));
        }
        let trailing = rest[closing_end..].trim_start_matches([' ', '\t']);
        if !trailing.is_empty() && !trailing.starts_with('#') {
            return Err(format!(
                "unsupported .env value for `{key}`: content follows the closing quote"
            ));
        }
        out
    } else {
        let head = value.split_once('#').map_or(value, |(head, _)| head);
        head.trim_end_matches([' ', '\t']).to_string()
    };
    if is_managed_env(key) && (parsed.contains("${") || parsed.contains("$(")) {
        return Err(format!(
            "unsupported expansion in managed Gemini environment variable `{key}`; preserve a literal value before takeover"
        ));
    }
    Ok(parsed)
}

fn quote_env_value(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '#' | '"' | '\'' | '$' | '\\'))
    {
        if !value.contains('\'') {
            format!("'{value}'")
        } else {
            let escaped = value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$");
            format!("\"{escaped}\"")
        }
    } else {
        value.to_string()
    }
}

fn is_managed_env(key: &str) -> bool {
    matches!(key, "GOOGLE_GEMINI_BASE_URL" | "GEMINI_API_KEY" | MODEL_ENV)
}

fn validate_env_key(key: &str) -> Result<(), String> {
    if valid_env_key(key) {
        Ok(())
    } else {
        Err(format!("invalid Gemini environment key `{key}`"))
    }
}

fn valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
