//! Read-only Codex history adapter skeleton (rollout JSONL / session summaries).
//!
//! Codex history can appear as rollout event streams (`*.jsonl`) and/or session
//! summary storage (e.g. `state_5.sqlite`). This adapter is intentionally
//! conservative: without a verified positive sample and confirmed token/summary
//! semantics, it **does not fabricate per-request facts** and does not guess how
//! to de-accumulate cumulative counters. Recognition and fixture-tested parsing
//! boundaries are implemented; full mapping remains gated on external evidence
//! ("Codex 正例").
//!
//! Safe behavior today:
//! - read-only detection of plausible Codex rollout directories / sqlite files;
//! - preview reports what was recognized and why facts were not imported;
//! - import publishes zero facts and records explicit warnings.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{ImportCancel, ImportOutcome, ImportPreview, ImportRange};
use crate::runtime_store::RuntimeStore;

/// Source family recorded for Codex history.
pub const SOURCE: &str = "codex";

const MAX_DEPTH: usize = 6;
const SAMPLE_LINES: usize = 40;
const SAMPLE_FILES: usize = 12;

/// A readable Codex history source (directory of rollout JSONL, or sqlite file).
pub struct CodexSource {
    root: PathBuf,
    instance: String,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    RolloutDir,
    SqliteSummary,
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    db.pragma_update(None, "query_only", "ON")
        .map_err(|error| error.to_string())?;
    Ok(db)
}

fn collect_jsonl(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    cancel: &ImportCancel,
) -> Result<(), String> {
    cancel.check()?;
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in entries {
        cancel.check()?;
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let path = entry.path();
        if kind.is_dir() {
            collect_jsonl(&path, depth + 1, out, cancel)?;
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn looks_like_claude_line(value: &Value) -> bool {
    let t = value.get("type").and_then(Value::as_str).unwrap_or("");
    matches!(t, "assistant" | "user") && value.get("message").and_then(Value::as_object).is_some()
}

fn looks_like_codex_event(value: &Value) -> bool {
    let t = value.get("type").and_then(Value::as_str).unwrap_or("");
    matches!(
        t,
        "session_meta"
            | "response_item"
            | "event_msg"
            | "turn_context"
            | "compacted"
            | "thread_rolled_back"
            | "message"
            | "function_call"
            | "function_call_output"
    ) && value.get("timestamp").is_some()
        || matches!(t, "session_meta" | "turn_context")
}

fn sample_rollout_signals(path: &Path) -> (bool, bool, bool) {
    let mut codex = false;
    let mut claude = false;
    let mut token_count = false;
    let Ok(file) = File::open(path) else {
        return (false, false, false);
    };
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i >= SAMPLE_LINES {
            break;
        }
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if looks_like_claude_line(&value) {
            claude = true;
        }
        if looks_like_codex_event(&value) {
            codex = true;
        }
        let t = value.get("type").and_then(Value::as_str).unwrap_or("");
        if t == "token_count" {
            token_count = true;
        }
        if t == "event_msg"
            && value
                .get("payload")
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str)
                == Some("token_count")
        {
            token_count = true;
        }
    }
    (codex, claude, token_count)
}

impl CodexSource {
    /// Recognize a directory as a plausible Codex rollout source without importing.
    pub fn recognizes_dir(path: &Path, cancel: &ImportCancel) -> Result<bool, String> {
        if !path.is_dir() {
            return Ok(false);
        }
        let mut files = Vec::new();
        collect_jsonl(path, 0, &mut files, cancel)?;
        if files.is_empty() {
            return Ok(false);
        }
        files.sort();
        let mut any_codex = false;
        let mut any_claude = false;
        let name_hint = files.iter().any(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
        });
        for file in files.iter().take(SAMPLE_FILES) {
            cancel.check()?;
            let (codex, claude, _token) = sample_rollout_signals(file);
            any_codex |= codex;
            any_claude |= claude;
        }
        Ok((any_codex || name_hint) && !any_claude)
    }

    /// Recognize a SQLite file as a plausible Codex session summary (`state_5.sqlite`).
    pub fn recognizes_sqlite(path: &Path) -> Result<bool, String> {
        if !path.is_file() {
            return Ok(false);
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "state_5.sqlite" || name.starts_with("state_") && name.ends_with(".sqlite") {
            // Presence/name match is enough for a conservative skeleton; schema mapping
            // is not assumed without a verified positive sample.
            return Ok(open_read_only(path).is_ok());
        }
        Ok(false)
    }

    pub fn open_dir(
        path: &Path,
        instance: impl Into<String>,
        cancel: &ImportCancel,
    ) -> Result<Self, String> {
        let instance = instance.into();
        if instance.trim().is_empty() {
            return Err("an import needs the source instance's name".into());
        }
        if !Self::recognizes_dir(path, cancel)? {
            return Err(format!(
                "no supported Codex rollout directory at {}",
                path.display()
            ));
        }
        Ok(Self {
            root: path.to_path_buf(),
            instance,
            kind: Kind::RolloutDir,
        })
    }

    pub fn open_sqlite(path: &Path, instance: impl Into<String>) -> Result<Self, String> {
        let instance = instance.into();
        if instance.trim().is_empty() {
            return Err("an import needs the source instance's name".into());
        }
        if !Self::recognizes_sqlite(path)? {
            return Err(format!(
                "no supported Codex session summary database at {}",
                path.display()
            ));
        }
        Ok(Self {
            root: path.to_path_buf(),
            instance,
            kind: Kind::SqliteSummary,
        })
    }

    pub fn preview(&self, range: &ImportRange, samples: usize) -> Result<ImportPreview, String> {
        self.preview_with_cancel(range, samples, &ImportCancel::new())
    }

    pub fn preview_with_cancel(
        &self,
        _range: &ImportRange,
        _samples: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        cancel.check()?;
        let mut warnings = vec![
            "Codex history mapping requires a verified positive sample (external evidence gate); no facts imported.".to_string(),
        ];
        match self.kind {
            Kind::RolloutDir => {
                let mut files = Vec::new();
                collect_jsonl(&self.root, 0, &mut files, cancel)?;
                let mut token_count_seen = false;
                for file in files.iter().take(SAMPLE_FILES) {
                    cancel.check()?;
                    let (_codex, _claude, token) = sample_rollout_signals(file);
                    token_count_seen |= token;
                }
                if token_count_seen {
                    warnings.push("token_count events were detected, but cumulative/turn semantics are not mapped without a verified positive sample.".to_string());
                } else {
                    warnings.push(
                        "No token_count usage events were found in sampled rollout files."
                            .to_string(),
                    );
                }
                warnings.push(format!(
                    "Recognized {} rollout JSONL file(s); facts were not imported.",
                    files.len()
                ));
            }
            Kind::SqliteSummary => {
                warnings.push("Recognized a Codex session summary SQLite file; aggregate mapping requires a verified schema/sample and was not imported.".to_string());
            }
        }
        Ok(ImportPreview {
            source: SOURCE.into(),
            instance: self.instance.clone(),
            source_version: 0,
            requests: 0,
            aggregates: 0,
            suspected: 0,
            covered_totals: 0,
            priced: 0,
            unpriced: 0,
            failures: 0,
            days: Vec::new(),
            warnings,
            ..Default::default()
        })
    }

    pub fn stage(
        &self,
        _store: &RuntimeStore,
        range: &ImportRange,
        _run_id: &str,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        range.validate()?;
        cancel.check()?;
        // Safety: no facts staged/published until mapping is verified against real
        // Codex usage samples (cumulative counters and summaries are easy to miscount).
        self.preview_with_cancel(range, 0, cancel)
    }

    pub fn import(
        &self,
        _store: &RuntimeStore,
        range: &ImportRange,
        _chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportOutcome, String> {
        let _preview = self.preview_with_cancel(range, 0, cancel)?;
        Ok(ImportOutcome::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp(name: &str) -> PathBuf {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("zroutery-codex-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_jsonl(path: &Path, lines: &[&str]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn recognizes_rollout_directory_but_imports_no_facts() {
        let dir = temp("rollout");
        write_jsonl(
            &dir.join("sessions/rollout-1.jsonl"),
            &[
                r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s1"}}"#,
                r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
            ],
        );
        let cancel = ImportCancel::new();
        assert!(CodexSource::recognizes_dir(&dir, &cancel).unwrap());
        let src = CodexSource::open_dir(&dir, "codex-fixture", &cancel).unwrap();
        let preview = src.preview(&ImportRange::default(), 0).unwrap();
        assert_eq!(preview.source, "codex");
        assert_eq!(preview.requests, 0);
        assert!(preview
            .warnings
            .iter()
            .any(|w| w.contains("verified positive sample")));
        assert!(preview.warnings.iter().any(|w| w.contains("token_count")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn does_not_mistake_claude_transcripts_for_codex() {
        let dir = temp("claude");
        write_jsonl(
            &dir.join("projects/alpha/session.jsonl"),
            &[
                r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","message":{"id":"m1","model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":2}}}"#,
            ],
        );
        let cancel = ImportCancel::new();
        assert!(!CodexSource::recognizes_dir(&dir, &cancel).unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn recognizes_state5_sqlite_name_conservatively() {
        let dir = temp("sqlite");
        let path = dir.join("state_5.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE sessions(id TEXT);").unwrap();
        drop(db);
        assert!(CodexSource::recognizes_sqlite(&path).unwrap());
        let src = CodexSource::open_sqlite(&path, "codex-state").unwrap();
        let preview = src.preview(&ImportRange::default(), 0).unwrap();
        assert_eq!(preview.requests, 0);
        assert!(preview
            .warnings
            .iter()
            .any(|w| w.contains("session summary SQLite")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rollout_preview_warns_when_no_usage_events_sampled() {
        let dir = temp("no-usage");
        write_jsonl(
            &dir.join("rollout-1.jsonl"),
            &[
                r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s1"}}"#,
                r#"{"timestamp":"2026-01-01T00:00:01Z","type":"response_item","payload":{"type":"message"}}"#,
            ],
        );
        let src = CodexSource::open_dir(&dir, "codex-fixture", &ImportCancel::new()).unwrap();
        let preview = src.preview(&ImportRange::default(), 0).unwrap();
        assert!(preview
            .warnings
            .iter()
            .any(|w| w.contains("No token_count usage events")));
        std::fs::remove_dir_all(dir).ok();
    }
}
