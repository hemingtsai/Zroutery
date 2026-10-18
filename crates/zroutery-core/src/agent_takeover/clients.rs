//! The client adapters: where each one keeps its config, and the shape the OpenCode adapter refuses to rewrite.

use super::fs::{
    apply_patch_to_disk, config_dir_override, home_dir, read_snapshot, release_to_disk,
    serialize_config, write_config_atomic, ConfigFormat,
};
use super::gateway::OPENCODE_CONFIG_ENV;
use super::ownership::OwnershipManifest;

use serde::{Deserialize, Serialize};

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
#[allow(dead_code)] // constructed only by the release-path fixtures
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
pub(super) fn first_comment_line(text: &str) -> Option<usize> {
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
pub(super) fn project_shadow(global: &std::path::Path) -> Option<std::path::PathBuf> {
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
        read_snapshot(AgentType::OpenCode, path, ConfigFormat::Json)
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
