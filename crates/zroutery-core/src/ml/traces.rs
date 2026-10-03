//! Durable routing traces — the history a router can actually learn from.
//!
//! [`DatasetStore`](super::dataset::DatasetStore) is a bounded in-memory ring.
//! That is enough to *collect* samples inside one process, and not enough to
//! learn from history: when the process exits the ring is gone, so there is no
//! such thing as a "historical routing trace" to train on, evaluate against, or
//! replay. Every prior attempt to reason about learning from traffic ran into
//! this wall and concluded, correctly but unhelpfully, that the evidence volume
//! was too low. The volume was zero.
//!
//! [`TraceLog`] is the missing half: an append-only, line-delimited record of
//! what the router saw and what happened, written from the same terminal
//! transition that produces the sample.
//!
//! # What one record holds, and why both halves
//!
//! A [`RequestTrace`] pairs two things that are individually useless together:
//!
//! * [`ShadowInput`] — the decision-time candidate set, one feature vector per
//!   candidate, and each candidate's eligibility. This is the *counterfactual*
//!   surface: which providers were available, what they looked like, and which
//!   were permitted.
//! * [`Vec<OutcomeTrainingSample>`] — the canonical samples the request actually
//!   produced, one per attempt, carrying the observed targets.
//!
//! The samples alone can train a model but cannot say what else was available,
//! so no alternative strategy can be scored. The candidate set alone cannot say
//! what happened, so it cannot score anything either. Together they support the
//! only comparison that means anything: *over these requests, with these
//! candidates, what would each policy have chosen, and what is known about how
//! that choice turned out?*
//!
//! # Durability and safety
//!
//! Appending happens on the serving path, so [`TraceLog::append`] is called
//! through [`contained_append`], which catches panics and returns an
//! [`Ingestion`-shaped](super::dataset::Ingestion) verdict rather than an error.
//! A trace that cannot be written costs one request its record and nothing
//! else. The log is append-only: nothing rewrites a line, so a truncated write
//! at the tail costs the tail and nothing before it.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::feedback::DataOrigin;
use crate::ml::dataset::OutcomeTrainingSample;
use crate::ml::shadow::ShadowInput;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Schema version of a persisted [`RequestTrace`] line.
pub const TRACE_SCHEMA_VERSION: u32 = 1;

/// File name of the append-only trace log, inside the router's state directory.
pub const TRACES_FILE_NAME: &str = "traces.jsonl";

/// FNV-1a offset basis, the same constant every other checksum in this module
/// tree uses so a fingerprint is comparable across components.
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;

/// FNV-1a prime.
const FNV_PRIME: u64 = 0x100000001b3;

/// Hex digits retained from a dataset fingerprint. Sixteen is the width
/// [`super::model_identity::CommitId`] uses, so the two are visually
/// comparable; a fingerprint is a label, not a security primitive.
const FINGERPRINT_HEX_DIGITS: usize = 16;

// ---------------------------------------------------------------------------
// Hashing helpers
// ---------------------------------------------------------------------------

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn hash_u64(hash: &mut u64, value: u64) {
    hash_bytes(hash, &value.to_le_bytes());
}

fn hash_str(hash: &mut u64, value: &str) {
    hash_bytes(hash, value.as_bytes());
    hash_bytes(hash, &[0]);
}

// ---------------------------------------------------------------------------
// RequestTrace — one request's decision-time evidence and its outcome
// ---------------------------------------------------------------------------

/// One request's durable trace: what was on the table, and what happened.
///
/// Not `PartialEq`: [`ShadowInput`] deliberately does not implement it, and
/// inventing an equality over a decision-time snapshot would be a claim about
/// what makes two requests "the same" that nothing here needs to make.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTrace {
    pub schema_version: u32,
    /// Request id, the join key against the runtime's other logs.
    pub request_id: String,
    /// Routing decision id the candidate set belongs to.
    pub decision_id: String,
    /// Unix seconds when the trace was appended.
    pub recorded_at: i64,
    /// The decision-time candidate snapshot production routing planned against.
    pub input: ShadowInput,
    /// Canonical samples this request produced, one per attempt plus one for
    /// the request as a whole.
    pub samples: Vec<OutcomeTrainingSample>,
}

impl RequestTrace {
    /// Build a trace from the retained decision-time input and the samples the
    /// same request produced.
    pub fn new(
        request_id: impl Into<String>,
        recorded_at: i64,
        input: ShadowInput,
        samples: Vec<OutcomeTrainingSample>,
    ) -> Self {
        let request_id = request_id.into();
        Self {
            schema_version: TRACE_SCHEMA_VERSION,
            decision_id: input.decision_id.clone(),
            request_id,
            recorded_at,
            input,
            samples,
        }
    }

    /// The attempt-scoped samples, i.e. the ones that describe one candidate's
    /// observed result. The request-scoped sample describes the whole request
    /// and is the training target, not a per-candidate observation.
    pub fn attempt_samples(&self) -> impl Iterator<Item = &OutcomeTrainingSample> {
        self.samples.iter().filter(|sample| {
            matches!(
                sample.scope,
                crate::ml::dataset::SampleScope::Attempt { .. }
            )
        })
    }

    /// The request-scoped sample, when the request produced one.
    pub fn request_sample(&self) -> Option<&OutcomeTrainingSample> {
        self.samples
            .iter()
            .find(|sample| matches!(sample.scope, crate::ml::dataset::SampleScope::Request))
    }
}

// ---------------------------------------------------------------------------
// DatasetFingerprint — identity of an ordered body of samples
// ---------------------------------------------------------------------------

/// A content-addressed identity for an ordered body of training samples.
///
/// This is what makes "the same data" checkable. A training report that cannot
/// name the data it was fitted on is not reproducible, and a comparison whose
/// two arms were fitted on different data is not a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatasetFingerprint(String);

impl DatasetFingerprint {
    /// Fingerprint an ordered slice of samples.
    ///
    /// Order is part of the identity: online training consumes samples in order
    /// and the resulting parameters depend on that order, so two bodies with the
    /// same members in a different order are different datasets. Every field
    /// that can change a fitted parameter is mixed in, including raw feature
    /// bit patterns rather than only the feature checksum.
    pub fn of(samples: &[OutcomeTrainingSample]) -> Self {
        let mut hash = FNV_OFFSET_BASIS;
        hash_str(&mut hash, "zroutery-dataset-fingerprint-v1");
        hash_u64(&mut hash, samples.len() as u64);
        for sample in samples {
            hash_str(&mut hash, &sample.sample_id);
            hash_u64(&mut hash, sample.timestamp as u64);
            hash_u64(&mut hash, sample.schema_version as u64);
            hash_u64(&mut hash, sample.features.schema_version as u64);
            for value in &sample.features.values {
                hash_u64(&mut hash, u64::from(value.to_bits()));
            }
            hash_u64(&mut hash, u64::from(sample.success));
            match sample.targets.latency_ms {
                Some(value) => {
                    hash_u64(&mut hash, 1);
                    hash_u64(&mut hash, value.to_bits());
                }
                None => hash_u64(&mut hash, 0),
            }
            match sample.targets.ttft_ms {
                Some(value) => {
                    hash_u64(&mut hash, 1);
                    hash_u64(&mut hash, value.to_bits());
                }
                None => hash_u64(&mut hash, 0),
            }
            match sample.targets.cost {
                Some(value) => {
                    hash_u64(&mut hash, 1);
                    hash_u64(&mut hash, value.to_bits());
                }
                None => hash_u64(&mut hash, 0),
            }
            hash_str(&mut hash, &sample.provider_id);
            hash_str(&mut hash, &sample.model_id);
            hash_str(&mut hash, origin_label(sample.origin));
        }
        Self(format!("{:0width$x}", hash, width = FINGERPRINT_HEX_DIGITS))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl DatasetFingerprint {
    /// Restore a fingerprint from its recorded form.
    ///
    /// A promotion decision is serialised with the fingerprint that decided it,
    /// so reading that decision back has to reconstruct the identity rather
    /// than re-derive it from a body that may no longer exist. The format is
    /// validated, so a truncated or hand-edited field is refused instead of
    /// becoming an identity that matches nothing and nothing else.
    pub fn parse(text: &str) -> Result<Self, TraceError> {
        let valid = text.len() == FINGERPRINT_HEX_DIGITS
            && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !valid {
            return Err(TraceError::Corrupt {
                path: "<fingerprint>".to_string(),
                line: 0,
                reason: format!(
                    "expected {FINGERPRINT_HEX_DIGITS} hex digits, got {:?}",
                    text.chars().take(24).collect::<String>()
                ),
            });
        }
        Ok(Self(text.to_ascii_lowercase()))
    }
}

/// A stable label for a sample's provenance.
///
/// The provenance is part of the fitted identity: a body that includes synthetic
/// samples is not the same dataset as one that does not, and a comparison over
/// the two must not be able to claim they match.
fn origin_label(origin: DataOrigin) -> &'static str {
    match origin {
        DataOrigin::Native => "native",
        DataOrigin::Imported => "imported",
        DataOrigin::Synthetic => "synthetic",
    }
}

impl std::fmt::Display for DatasetFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// TraceError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("trace log io failed at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("trace line {line} in {path} is unreadable: {reason}")]
    Corrupt {
        path: String,
        line: usize,
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Ingestion verdict
// ---------------------------------------------------------------------------

/// What one terminal transition contributed to the durable log.
///
/// Shaped like [`super::dataset::Ingestion`] on purpose: a caller must be able
/// to say "nothing was collected" and "something was collected and refused"
/// without conflating them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceIngestion {
    /// Records were appended.
    Appended { records: u64 },
    /// There was nothing to record: the request produced no samples.
    Nothing,
    /// The write did not happen and was refused. Counted, never propagated.
    Refused { reason: String },
}

impl TraceIngestion {
    pub fn is_appended(&self) -> bool {
        matches!(self, TraceIngestion::Appended { .. })
    }
}

// ---------------------------------------------------------------------------
// TraceCounters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceCounters {
    /// Records successfully appended.
    pub appended: u64,
    /// Records dropped because the request had no samples to record.
    pub nothing: u64,
    /// Append attempts refused, including contained panics.
    pub refused: u64,
    /// Append attempts that failed with an I/O error.
    pub io_errors: u64,
}

/// The mutable counters behind [`TraceCounters`].
///
/// Atomics rather than a plain struct because the serving path appends from
/// every request thread and the counters are the only evidence a trace path is
/// quietly losing records.
#[derive(Debug, Default)]
struct AtomicTraceCounters {
    appended: AtomicU64,
    nothing: AtomicU64,
    refused: AtomicU64,
    io_errors: AtomicU64,
}

impl AtomicTraceCounters {
    fn snapshot(&self) -> TraceCounters {
        TraceCounters {
            appended: self.appended.load(Ordering::Relaxed),
            nothing: self.nothing.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            io_errors: self.io_errors.load(Ordering::Relaxed),
        }
    }
}

// ---------------------------------------------------------------------------
// TraceLog — the durable append-only log
// ---------------------------------------------------------------------------

/// Append-only durable record of routing decisions and their outcomes.
///
/// Constructed once per process against a directory. The file handle is held
/// open for append and re-opened lazily if it is ever lost, so a long-running
/// proxy does not pay an open per request.
pub struct TraceLog {
    path: PathBuf,
    append_lock: Mutex<Option<File>>,
    counters: AtomicTraceCounters,
    /// Total bytes appended, used to report log growth without a stat per write.
    bytes: AtomicU64,
}

impl std::fmt::Debug for TraceLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceLog")
            .field("path", &self.path)
            .field("counters", &self.counters.snapshot())
            .field("bytes", &self.bytes.load(Ordering::Relaxed))
            .finish()
    }
}

impl TraceLog {
    /// Open (creating if needed) a trace log in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, TraceError> {
        let dir = dir.as_ref();
        let path = dir.join(TRACES_FILE_NAME);
        let io = |source: std::io::Error| TraceError::Io {
            path: path.display().to_string(),
            source,
        };
        fs::create_dir_all(dir).map_err(io)?;
        // Touch the file so a later append never has to create it, and so an
        // unwritable directory fails here rather than on a serving request.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io)?;
        Ok(Self {
            path,
            append_lock: Mutex::new(None),
            counters: AtomicTraceCounters::default(),
            bytes: AtomicU64::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn counters(&self) -> TraceCounters {
        self.counters.snapshot()
    }

    /// Append one request trace. Returns the number of records written (0 or 1).
    pub fn append(&self, trace: &RequestTrace) -> Result<usize, TraceError> {
        let mut line = serde_json::to_string(trace).map_err(|error| TraceError::Corrupt {
            path: self.path.display().to_string(),
            line: 0,
            reason: format!("trace did not serialize: {error}"),
        })?;
        line.push('\n');
        let io = |source: std::io::Error| TraceError::Io {
            path: self.path.display().to_string(),
            source,
        };

        let mut guard = crate::sync::lock(&self.append_lock);
        if guard.is_none() {
            *guard = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .map_err(io)?,
            );
        }
        let file = guard
            .as_mut()
            .expect("the append handle was just installed");
        file.write_all(line.as_bytes()).map_err(io)?;
        // Flushed per record on purpose. A router that loses the tail of its own
        // history on an unclean exit has an evidence gap it cannot detect, and
        // an undetectable gap is worse than a slow append. Throughput on this
        // path is one small sequential write per request.
        file.flush().map_err(io)?;
        self.bytes.fetch_add(line.len() as u64, Ordering::Relaxed);
        self.counters.appended.fetch_add(1, Ordering::Relaxed);
        Ok(1)
    }

    /// Read every record back, in write order.
    ///
    /// A corrupt line is an error, not a silent skip: a truncated tail is
    /// expected and reported as [`TraceError::Corrupt`], and a caller can decide
    /// whether to salvage. Silently dropping records would make a fingerprint
    /// describe a body nobody can account for.
    pub fn load(&self) -> Result<Vec<RequestTrace>, TraceError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(TraceError::Io {
                    path: self.path.display().to_string(),
                    source,
                })
            }
        };
        let mut traces = Vec::new();
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|source| TraceError::Io {
                path: self.path.display().to_string(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let trace: RequestTrace =
                serde_json::from_str(&line).map_err(|error| TraceError::Corrupt {
                    path: self.path.display().to_string(),
                    line: index + 1,
                    reason: error.to_string(),
                })?;
            traces.push(trace);
        }
        Ok(traces)
    }

    /// Load only the traces, discarding their samples, for counting and
    /// fingerprinting a body without materialising every feature vector twice.
    pub fn count(&self) -> Result<usize, TraceError> {
        Ok(self.load()?.len())
    }

    /// Fingerprint the samples a load would produce.
    pub fn fingerprint(&self) -> Result<DatasetFingerprint, TraceError> {
        Ok(DatasetFingerprint::of(&samples_from(&self.load()?)))
    }

    fn note_nothing(&self) {
        self.counters.nothing.fetch_add(1, Ordering::Relaxed);
    }

    fn note_refused(&self) {
        self.counters.refused.fetch_add(1, Ordering::Relaxed);
    }

    fn note_io_error(&self) {
        self.counters.io_errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// The training samples a body of traces contains, in trace order.
pub fn samples_from(traces: &[RequestTrace]) -> Vec<OutcomeTrainingSample> {
    let mut samples = Vec::new();
    for trace in traces {
        for sample in &trace.samples {
            samples.push(sample.clone());
        }
    }
    samples
}

/// The training samples of one body of traces, deduplicated by sample id and in
/// first-seen order.
///
/// A request that is retried after a reconnect can produce the same sample id
/// twice; training on it twice would weight one observation twice for no reason.
pub fn deduped_samples_from(traces: &[RequestTrace]) -> Vec<OutcomeTrainingSample> {
    let mut seen = std::collections::HashSet::new();
    let mut samples = Vec::new();
    for trace in traces {
        for sample in &trace.samples {
            if seen.insert(sample.sample_id.clone()) {
                samples.push(sample.clone());
            }
        }
    }
    samples
}

// ---------------------------------------------------------------------------
// contained_append — the serving-path boundary
// ---------------------------------------------------------------------------

/// Append a trace with every failure contained.
///
/// The serving path calls this from the request's terminal transition. A trace
/// that cannot be written must cost the request nothing: no error is returned,
/// no panic escapes, and the reason is counted.
pub fn contained_append(
    log: &TraceLog,
    decision_id: &str,
    input: &ShadowInput,
    samples: Vec<OutcomeTrainingSample>,
    recorded_at: i64,
) -> TraceIngestion {
    if samples.is_empty() {
        log.note_nothing();
        return TraceIngestion::Nothing;
    }
    let trace = RequestTrace::new(
        input.decision_id.clone(),
        recorded_at,
        input.clone(),
        samples,
    );
    // `decision_id` is accepted so a caller can log the id it holds when the
    // trace itself fails to build; keeping it in the signature means the
    // counter is attributable from the call site without re-deriving it.
    let _ = decision_id;

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| log.append(&trace)));
    match outcome {
        Ok(Ok(_)) => TraceIngestion::Appended { records: 1 },
        Ok(Err(TraceError::Io { .. })) => {
            log.note_io_error();
            log.note_refused();
            tracing::warn!(
                decision_id = %trace.decision_id,
                "routing trace was not persisted"
            );
            TraceIngestion::Refused {
                reason: "trace log write failed".to_string(),
            }
        }
        Ok(Err(error)) => {
            log.note_refused();
            tracing::warn!(
                decision_id = %trace.decision_id,
                error = %error,
                "routing trace was refused"
            );
            TraceIngestion::Refused {
                reason: error.to_string(),
            }
        }
        Err(_) => {
            log.note_refused();
            tracing::warn!(
                decision_id = %trace.decision_id,
                "routing trace append panicked and was contained"
            );
            TraceIngestion::Refused {
                reason: "trace append panicked".to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::dataset::{SampleScope, Targets};
    use crate::ml::features::{
        RoutingFeatures, FEATURE_DIMENSION, FEATURE_SCHEMA_VERSION, UNKNOWN,
    };

    fn features(seed: u32) -> RoutingFeatures {
        let mut values = [UNKNOWN; FEATURE_DIMENSION];
        values[0] = seed as f32;
        RoutingFeatures {
            schema_version: FEATURE_SCHEMA_VERSION,
            values,
        }
    }

    fn sample(id: &str, seed: u32, success: bool) -> OutcomeTrainingSample {
        OutcomeTrainingSample {
            sample_id: id.to_string(),
            schema_version: 1,
            timestamp: 1_700_000_000 + i64::from(seed),
            streaming: false,
            dialect: "anthropic".to_string(),
            features: features(seed),
            targets: Targets {
                success,
                latency_ms: Some(120.0 + f64::from(seed)),
                ttft_ms: Some(40.0),
                cost: Some(0.01),
                failure_class: None,
                fallback_count: 0,
            },
            provider_id: format!("p{seed}"),
            model_id: format!("m{seed}"),
            origin: DataOrigin::Native,
            outcome_id: format!("out-{id}"),
            request_id: format!("req-{id}"),
            decision_id: Some("dec-1".to_string()),
            response_id: None,
            final_status: if success {
                crate::outcome::FinalStatus::Success
            } else {
                crate::outcome::FinalStatus::Failed
            },
            success,
            identity: crate::outcome::OutcomeIdentity::default(),
            scope: SampleScope::Attempt {
                index: 0,
                attempt_id: format!("att-{id}"),
            },
            attempt_id: Some(format!("att-{id}")),
            rectified: false,
            attempts: Vec::new(),
            usage: None,
            estimated_cost: None,
            actual_cost: None,
            terminal_error: None,
            feedback: None,
        }
    }

    fn trace(id: &str) -> RequestTrace {
        let input = ShadowInput {
            decision_id: "dec-1".to_string(),
            production_selected: "m0".to_string(),
            candidates: Vec::new(),
            ..ShadowInput::default()
        };
        RequestTrace::new(id, 1_700_000_000, input, vec![sample(id, 0, true)])
    }

    #[test]
    fn fingerprint_is_stable_for_the_same_body() {
        let samples = vec![sample("a", 0, true), sample("b", 1, false)];
        assert_eq!(
            DatasetFingerprint::of(&samples),
            DatasetFingerprint::of(&samples.clone())
        );
    }

    #[test]
    fn fingerprint_changes_with_order() {
        let a = sample("a", 0, true);
        let b = sample("b", 1, true);
        assert_ne!(
            DatasetFingerprint::of(&[a.clone(), b.clone()]),
            DatasetFingerprint::of(&[b, a])
        );
    }

    #[test]
    fn fingerprint_changes_with_targets() {
        let base = sample("a", 0, true);
        let mut flipped = base.clone();
        flipped.success = false;
        assert_ne!(
            DatasetFingerprint::of(&[base]),
            DatasetFingerprint::of(&[flipped])
        );
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TraceLog::open(dir.path()).expect("open");
        log.append(&trace("r1")).expect("append");
        log.append(&trace("r2")).expect("append");

        let loaded = log.load().expect("load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].request_id, "r1");
        assert_eq!(loaded[1].request_id, "r2");
        assert_eq!(loaded[0].samples.len(), 1);
        assert_eq!(log.counters().appended, 2);
    }

    #[test]
    fn load_of_a_missing_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TraceLog::open(dir.path()).expect("open");
        assert!(log.load().expect("load").is_empty());
    }

    #[test]
    fn fingerprint_over_disk_matches_the_in_memory_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TraceLog::open(dir.path()).expect("open");
        let traces = vec![trace("r1"), trace("r2")];
        for item in &traces {
            log.append(item).expect("append");
        }
        let on_disk = log.fingerprint().expect("fingerprint");
        let in_memory = DatasetFingerprint::of(&samples_from(&traces));
        assert_eq!(on_disk, in_memory);
    }

    #[test]
    fn contained_append_reports_a_refusal_without_propagating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TraceLog::open(dir.path()).expect("open");
        let input = ShadowInput {
            decision_id: "dec-x".to_string(),
            ..ShadowInput::default()
        };
        let verdict = contained_append(&log, "dec-x", &input, vec![sample("s", 0, true)], 1);
        assert_eq!(verdict, TraceIngestion::Appended { records: 1 });
        assert_eq!(log.counters().appended, 1);
    }

    #[test]
    fn contained_append_with_no_samples_reports_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TraceLog::open(dir.path()).expect("open");
        let input = ShadowInput::default();
        assert_eq!(
            contained_append(&log, "dec", &input, Vec::new(), 1),
            TraceIngestion::Nothing
        );
        assert_eq!(log.counters().nothing, 1);
    }

    #[test]
    fn dedupe_keeps_first_seen_order() {
        let a = trace("r1");
        let mut b = trace("r2");
        b.samples[0].sample_id = "same".to_string();
        let mut c = trace("r3");
        c.samples[0].sample_id = "same".to_string();
        let deduped = deduped_samples_from(&[a.clone(), b, c]);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].sample_id, a.samples[0].sample_id);
    }

    #[test]
    fn note_helpers_are_reachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = TraceLog::open(dir.path()).expect("open");
        log.note_nothing();
        log.note_refused();
        log.note_io_error();
        let counters = log.counters();
        assert_eq!(counters.nothing, 1);
        assert_eq!(counters.refused, 1);
        assert_eq!(counters.io_errors, 1);
    }
}
