//! Claude Code's own transcripts, read from a copy of its `projects` folder.
//!
//! Claude Code writes one JSONL file per conversation under
//! `~/.claude/projects/<project>/`, plus a `subagents/` folder whose files are
//! separate conversations. Only assistant lines carry usage, and one assistant
//! message can appear more than once while it streams, so the file is deduped
//! by `message.id` before anything is counted.
//!
//! Two facts about this source shape what is imported:
//!
//! * Its token counts follow the Anthropic convention: `input_tokens` is the
//!   fresh prompt, with cache reads and cache writes in their own fields. They
//!   are added together here so the stored count matches this repository's
//!   cache-inside-input invariant, and the conversion is recorded on the fact.
//! * A transcript records completed assistant messages. A call that failed, or
//!   was retried before answering, leaves no line at all — so the imported
//!   history has no failures to count and no price to read. A missing price is
//!   stored as unpriced, never as free.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::ir::Usage;
use crate::runtime_store::RuntimeStore;

use super::{
    finish_import, local_day, stage_chunk, Duplicate, Granularity, ImportCancel, ImportOutcome,
    ImportPreview, ImportRange, ImportedFact, InputSemantics,
};

/// The source family recorded on every fact from this adapter.
pub const SOURCE: &str = "claudecode";

/// How deep the folder walk goes. The real layout is
/// `projects/<project>/<session>/subagents/workflows/<run>/*.jsonl`, which is
/// five levels below the chosen root; the extra level is headroom.
const MAX_DEPTH: usize = 6;

/// One readable copy of a Claude Code projects folder.
pub struct ClaudeCodeSource {
    root: PathBuf,
    instance: String,
    files: Vec<PathBuf>,
}

/// One assistant message, as a transcript line states it.
#[derive(Debug, Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    stop_reason: Option<String>,
    usage: Option<MessageUsage>,
}

#[derive(Debug, Deserialize)]
struct MessageUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

/// The winning line for one message id, and how to prefer it over a rival.
struct Candidate {
    fact: ImportedFact,
    completed: bool,
}

#[derive(Default)]
struct ScanReport {
    requests: u64,
    days: std::collections::BTreeSet<String>,
    /// Lines that looked like usage but could not be placed: a bad timestamp,
    /// a missing id, or an unreadable line.
    skipped: u64,
}

impl ClaudeCodeSource {
    /// Point the adapter at a transcripts folder. Nothing is written anywhere.
    pub fn open(root: &Path, instance: impl Into<String>) -> Result<Self, String> {
        Self::open_with_cancel(root, instance, &ImportCancel::new())
    }

    pub fn open_with_cancel(
        root: &Path,
        instance: impl Into<String>,
        cancel: &ImportCancel,
    ) -> Result<Self, String> {
        let instance = instance.into();
        if instance.trim().is_empty() {
            return Err("an import needs the source instance's name".into());
        }
        if !root.is_dir() {
            return Err(format!(
                "no Claude Code transcripts folder at {}",
                root.display()
            ));
        }
        let mut files = Vec::new();
        collect(root, 0, &mut files, cancel)?;
        if files.is_empty() {
            return Err(format!(
                "no .jsonl transcript files under {}",
                root.display()
            ));
        }
        files.sort();
        Ok(Self {
            root: root.to_path_buf(),
            instance,
            files,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// What an import would store, without writing anything.
    pub fn preview(&self, range: &ImportRange, samples: usize) -> Result<ImportPreview, String> {
        range.validate()?;
        self.preview_scan(range, samples.min(20), &ImportCancel::new(), |_| Ok(()))
    }

    fn preview_scan(
        &self,
        range: &ImportRange,
        samples: usize,
        cancel: &ImportCancel,
        mut visit: impl FnMut(ImportedFact) -> Result<(), String>,
    ) -> Result<ImportPreview, String> {
        let mut preview = ImportPreview {
            source: SOURCE.into(),
            instance: self.instance.clone(),
            ..ImportPreview::default()
        };
        preview
            .warnings
            .push("transcripts hold completed assistant messages only: failed calls are absent and no price is recorded".into());
        let report = self.scan(range, cancel, |fact| {
            if preview.samples.len() < samples {
                preview.samples.push(fact.clone());
            }
            visit(fact)
        })?;
        preview.requests = report.requests;
        preview.unpriced = report.requests;
        preview.days = report.days.into_iter().collect();
        if report.skipped > 0 {
            preview.warnings.push(format!(
                "{} line(s) were left out because they carry no id or no usable timestamp",
                report.skipped
            ));
        }
        Ok(preview)
    }

    /// Store every fact in the window.
    ///
    /// The read is chunked and staged; the publish is one transaction at the
    /// end. An incomplete scan is restarted from a fresh snapshot; only a
    /// completed preview can be resumed without reopening the source.
    pub fn import(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportOutcome, String> {
        let preview = self.stage_chunks(
            store,
            range,
            &uuid::Uuid::new_v4().to_string(),
            chunk,
            cancel,
        )?;
        finish_import(store, &preview, cancel)
    }

    pub fn stage(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        run_id: &str,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        self.stage_chunks(store, range, run_id, 500, cancel)
    }

    fn stage_chunks(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        run_id: &str,
        chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        range.validate()?;
        if cancel.is_cancelled() {
            return Ok(ImportPreview {
                cancelled: true,
                ..ImportPreview::default()
            });
        }
        let run = store.begin_import_run(run_id, SOURCE, &self.instance, 0, range)?;
        let result = (|| {
            let chunk = chunk.clamp(1, 500);
            let mut pending = Vec::with_capacity(chunk);
            let preview = self.preview_scan(range, 0, cancel, |fact| {
                pending.push(fact);
                if pending.len() >= chunk {
                    stage_chunk(store, &run, &mut pending, cancel)?;
                }
                Ok(())
            })?;
            stage_chunk(store, &run, &mut pending, cancel)?;
            store.ready_import(run_id, preview)
        })();
        if result.is_err() {
            store.discard_import_run(run_id)?;
            if cancel.is_cancelled() {
                return Ok(ImportPreview {
                    cancelled: true,
                    ..ImportPreview::default()
                });
            }
        }
        result
    }

    fn scan(
        &self,
        range: &ImportRange,
        cancel: &ImportCancel,
        mut visit: impl FnMut(ImportedFact) -> Result<(), String>,
    ) -> Result<ScanReport, String> {
        let mut report = ScanReport::default();
        let before = file_versions(&self.root, &self.files, cancel)?;
        for path in &self.files {
            cancel.check()?;
            for fact in self.facts_in(path, range, &mut report, cancel)? {
                report.requests += 1;
                report.days.insert(fact.day.clone());
                visit(fact)?;
            }
        }
        if before != file_versions(&self.root, &self.files, cancel)? {
            return Err(
                "transcripts changed while preparing the preview; retry for a fresh snapshot"
                    .into(),
            );
        }
        Ok(report)
    }

    /// Every fact in one transcript file, deduped by message id.
    fn facts_in(
        &self,
        path: &Path,
        range: &ImportRange,
        report: &mut ScanReport,
        cancel: &ImportCancel,
    ) -> Result<Vec<ImportedFact>, String> {
        let file =
            File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut winners: BTreeMap<String, Candidate> = BTreeMap::new();
        let mut session_fallback: Option<String> = None;
        loop {
            cancel.check()?;
            line.clear();
            let read = reader
                .read_line(&mut line)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            // A line without its newline is a write in progress. It is not a
            // fact yet, and a re-import picks it up once it is complete.
            if !line.ends_with('\n') {
                break;
            }
            if !line.contains("\"assistant\"") {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Line>(line.trim_end()) else {
                report.skipped += 1;
                continue;
            };
            if parsed.kind.as_deref() != Some("assistant") {
                continue;
            }
            if session_fallback.is_none() {
                session_fallback = parsed.session_id.clone().filter(|id| !id.trim().is_empty());
            }
            let session = parsed
                .session_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .or_else(|| session_fallback.clone());
            let Some(fact) = self.line_fact(&parsed, session.as_deref(), report) else {
                continue;
            };
            let id = fact.entity_id.clone();
            let completed = parsed
                .message
                .as_ref()
                .and_then(|message| message.stop_reason.as_deref())
                .is_some_and(|reason| !reason.is_empty());
            // The same message streams in more than once. The completed
            // rendering wins; between two completed ones the longer output
            // does, because a truncated line understates the call.
            let replace = match winners.get(&id) {
                None => true,
                Some(current) => {
                    (completed && !current.completed)
                        || (completed == current.completed
                            && fact.tokens.output_tokens > current.fact.tokens.output_tokens)
                }
            };
            if replace {
                winners.insert(id, Candidate { fact, completed });
            }
        }
        Ok(winners
            .into_values()
            .filter(|candidate| range.contains(candidate.fact.at))
            .map(|candidate| candidate.fact)
            .collect())
    }

    fn line_fact(
        &self,
        line: &Line,
        session: Option<&str>,
        report: &mut ScanReport,
    ) -> Option<ImportedFact> {
        let message = line.message.as_ref()?;
        let usage = message.usage.as_ref()?;
        let Some(id) = message.id.as_deref().filter(|id| !id.trim().is_empty()) else {
            report.skipped += 1;
            return None;
        };
        let Some(at) = line
            .timestamp
            .as_deref()
            .and_then(|text| text.parse::<DateTime<Utc>>().ok())
        else {
            report.skipped += 1;
            return None;
        };
        let fresh = usage.input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        // An assistant message with no tokens at all is not a call: it is a
        // bookkeeping line. Writing it as a zero-token request would invent a
        // request the source never charged for.
        if fresh == 0 && cache_read == 0 && cache_write == 0 && output == 0 {
            return None;
        }
        let tokens = usage_u32(fresh, output, cache_read, cache_write)?;
        let model = message
            .model
            .clone()
            .filter(|model| !model.trim().is_empty());
        Some(ImportedFact {
            source: SOURCE.into(),
            instance: self.instance.clone(),
            entity_id: match session {
                Some(session) => format!("claude:{session}:{id}"),
                None => format!("claude:{id}"),
            },
            revision: String::new(),
            granularity: Granularity::Request,
            at,
            day: local_day(at),
            client: Some("claude".into()),
            provider_id: None,
            model: model.clone(),
            request_model: model,
            pricing_model: None,
            tokens,
            input_semantics: Some(InputSemantics::Fresh),
            // A transcript carries no price. The call is unpriced, not free.
            cost: None,
            reported_cost: None,
            priced: false,
            latency_ms: None,
            ttft_ms: None,
            // Nor a status: the line exists because the call succeeded.
            status: None,
            ok: None,
            streaming: false,
            requests: 1,
            failures: 0,
            data_source: "session_log".into(),
            duplicate: Duplicate::None,
            duplicate_of: None,
            multiplier: None,
            error: None,
        })
    }
}

fn usage_u32(fresh: u64, output: u64, read: u64, write: u64) -> Option<Usage> {
    Some(Usage {
        input_tokens: u32::try_from(fresh.saturating_add(read).saturating_add(write)).ok()?,
        output_tokens: u32::try_from(output).ok()?,
        cache_read_tokens: u32::try_from(read).ok()?,
        cache_write_tokens: u32::try_from(write).ok()?,
        reasoning_tokens: 0,
    })
}

type FileVersion = (PathBuf, u64, std::time::SystemTime);

/// Files are not a transactional database. Refuse an unstable scan instead of
/// calling a mixture of file generations one source snapshot. Only metadata is
/// retained, and confirmation subsequently reads the staged accounting facts.
fn file_versions(
    root: &Path,
    expected: &[PathBuf],
    cancel: &ImportCancel,
) -> Result<Vec<FileVersion>, String> {
    cancel.check()?;
    let mut files = Vec::new();
    collect(root, 0, &mut files, cancel)?;
    files.sort();
    if files != expected {
        return Err("transcript files changed; prepare a new preview".into());
    }
    files
        .into_iter()
        .map(|path| {
            cancel.check()?;
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("transcript file was replaced; prepare a new preview".into());
            }
            Ok((
                path,
                metadata.len(),
                metadata.modified().map_err(|error| error.to_string())?,
            ))
        })
        .collect()
}

/// Collect transcript files below `dir`, without following symlinks.
fn collect(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    cancel: &ImportCancel,
) -> Result<(), String> {
    cancel.check()?;
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot list {}: {error}", dir.display()))?;
    for entry in entries {
        cancel.check()?;
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        let path = entry.path();
        if kind.is_dir() {
            collect(&path, depth + 1, out, cancel)?;
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::CostTotals;
    use std::io::Write;

    fn write(path: &Path, lines: &[&str]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn assistant(id: &str, at: &str, stop: Option<&str>, input: u64, output: u64) -> String {
        let stop = match stop {
            Some(reason) => format!("\"stop_reason\":\"{reason}\","),
            None => String::new(),
        };
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"sessionId\":\"s1\",\
             \"message\":{{\"id\":\"{id}\",\"model\":\"claude-sonnet-4-5\",{stop}\
             \"usage\":{{\"input_tokens\":{input},\"output_tokens\":{output},\
             \"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":10}}}}}}"
        )
    }

    /// A transcript tree with the shapes that matter: a streamed duplicate, a
    /// subagent conversation, a turn with no usage, and noise.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("projects/alpha/session.jsonl"),
            &[
                // Not an assistant line: a user turn and a summary.
                "{\"type\":\"user\",\"timestamp\":\"2026-01-02T10:00:00Z\"}",
                "{\"type\":\"summary\",\"summary\":\"hi\"}",
                // The same assistant message twice, once truncated.
                &assistant("msg_1", "2026-01-02T10:00:01Z", None, 100, 5),
                &assistant("msg_1", "2026-01-02T10:00:02Z", Some("end_turn"), 100, 40),
                // A second call in the same session.
                &assistant("msg_2", "2026-01-02T10:05:00Z", Some("end_turn"), 7, 3),
                // An assistant line with no usage at all.
                "{\"type\":\"assistant\",\"timestamp\":\"2026-01-02T10:06:00Z\",\"message\":{\"id\":\"msg_3\"}}",
                // A line with a bad timestamp.
                &assistant("msg_4", "not-a-time", Some("end_turn"), 5, 5),
            ],
        );
        write(
            &dir.path().join("projects/alpha/s1/subagents/agent.jsonl"),
            &[&assistant(
                "msg_5",
                "2026-01-02T11:00:00Z",
                Some("end_turn"),
                20,
                2,
            )],
        );
        // A file that is not a transcript.
        write(&dir.path().join("projects/alpha/notes.txt"), &["hello"]);
        dir
    }

    fn range() -> ImportRange {
        ImportRange::default()
    }

    #[test]
    fn a_streamed_message_counts_once_and_its_cache_is_inside_input() {
        let fixture = fixture();
        let source = ClaudeCodeSource::open(&fixture.path().join("projects"), "laptop").unwrap();
        assert_eq!(source.file_count(), 2);
        let preview = source.preview(&range(), 10).unwrap();
        assert_eq!(preview.source, "claudecode");
        assert_eq!(
            preview.requests, 3,
            "two conversations, the duplicate wins once"
        );
        assert_eq!(preview.unpriced, 3, "a transcript carries no price");
        assert_eq!(preview.failures, 0);
        assert_eq!(preview.suspected, 0);
        assert!(preview.warnings[0].contains("failed calls are absent"));
        assert!(preview
            .warnings
            .iter()
            .any(|warning| warning.contains("1 line(s)")));

        let facts = source
            .scan(&range(), &ImportCancel::new(), |_| Ok(()))
            .map(|report| report.requests)
            .unwrap();
        assert_eq!(facts, 3);
        let wins = source
            .facts_in(
                source
                    .files
                    .iter()
                    .find(|path| path.ends_with("session.jsonl"))
                    .expect("the main transcript"),
                &range(),
                &mut ScanReport::default(),
                &ImportCancel::new(),
            )
            .unwrap();
        let first = wins
            .iter()
            .find(|fact| fact.entity_id.contains("msg_1"))
            .unwrap();
        assert_eq!(
            first.tokens.output_tokens, 40,
            "the completed rendering won"
        );
        assert_eq!(
            first.tokens.input_tokens, 140,
            "fresh prompt plus cache reads and writes"
        );
        assert_eq!(first.tokens.cache_read_tokens, 30);
        assert_eq!(first.tokens.cache_write_tokens, 10);
        assert_eq!(first.input_semantics, Some(InputSemantics::Fresh));
        assert!(!first.priced && first.cost.is_none());
        assert_eq!(first.status, None, "a transcript records no status");
        assert_eq!(first.ok, None);
        assert_eq!(first.data_source, "session_log");
    }

    #[test]
    fn importing_twice_stores_each_message_once() {
        let fixture = fixture();
        let store = RuntimeStore::open(&fixture.path().join("runtime.db")).unwrap();
        let source = ClaudeCodeSource::open(&fixture.path().join("projects"), "laptop").unwrap();

        let first = source
            .import(&store, &range(), 2, &ImportCancel::new())
            .unwrap();
        assert_eq!(first.inserted, 3);
        assert!(first.revision > 0);
        let second = source
            .import(&store, &range(), 2, &ImportCancel::new())
            .unwrap();
        assert_eq!(second.unchanged, 3);
        assert_eq!(second.inserted, 0);
        assert_eq!(second.revision, first.revision);

        let totals = store.imported_totals(None, None).unwrap();
        assert_eq!(totals.requests, 3);
        assert_eq!(totals.unpriced_requests, 3);
        assert_eq!(totals.cost, CostTotals::default(), "no price was invented");
        assert_eq!(totals.input_tokens, 140 + 47 + 60);
        assert_eq!(totals.output_tokens, 40 + 3 + 2);
        assert_eq!(totals.failures, 0);
    }

    #[test]
    fn a_window_keeps_only_the_calls_inside_it() {
        let fixture = fixture();
        let source = ClaudeCodeSource::open(&fixture.path().join("projects"), "laptop").unwrap();
        let from = "2026-01-02T10:04:00Z".parse().unwrap();
        let preview = source
            .preview(&ImportRange::new(Some(from), None).unwrap(), 0)
            .unwrap();
        assert_eq!(
            preview.requests, 2,
            "the 10:05 call and the subagent at 11:00"
        );
    }

    #[test]
    fn a_folder_without_transcripts_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/empty")).unwrap();
        let error = ClaudeCodeSource::open(&dir.path().join("projects"), "laptop")
            .err()
            .expect("a folder without transcripts is refused");
        assert!(error.contains("no .jsonl"), "{error}");
        let missing = tempfile::tempdir().unwrap();
        assert!(ClaudeCodeSource::open(&missing.path().join("nope"), "laptop").is_err());
        assert!(ClaudeCodeSource::open(&missing.path().join("nope"), "  ").is_err());
    }

    #[test]
    fn reading_transcripts_never_changes_them() {
        let fixture = fixture();
        let path = fixture.path().join("projects/alpha/session.jsonl");
        let before = std::fs::read(&path).unwrap();
        let store = RuntimeStore::open(&fixture.path().join("runtime.db")).unwrap();
        let source = ClaudeCodeSource::open(&fixture.path().join("projects"), "laptop").unwrap();
        source
            .import(&store, &range(), 8, &ImportCancel::new())
            .unwrap();
        assert_eq!(before, std::fs::read(&path).unwrap());
    }
}
