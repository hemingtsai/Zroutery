use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::conversion::canonical_samples_from_decision_time;
use super::sample::{OutcomeTrainingSample, TrainingSample};
use super::validation::validate_outcome_sample;

use crate::feedback::DataOrigin;
use crate::ml::shadow::ShadowInput;
use crate::outcome::Outcome;

// ---------------------------------------------------------------------------
// DatasetStore — bounded storage for canonical training samples
// ---------------------------------------------------------------------------

/// Production retention bounds.
///
/// A canonical sample carries its full attempt evidence, so the count bound is
/// deliberately far below the legacy 100k default: at roughly a kilobyte per
/// sample this caps the in-process dataset in the low tens of megabytes, which
/// is what a desktop proxy can hold without the dataset becoming the largest
/// thing in the process. Age is the second bound, because a sample's usefulness
/// is bounded by how stale the routing state it describes has become.
pub const PRODUCTION_MAX_SAMPLES: usize = 10_000;
/// Thirty days is the default age window; production uses a shorter one.
pub const PRODUCTION_MAX_AGE_SECS: i64 = 7 * 24 * 3600;

/// The outcome of one ingestion attempt, and the only way to tell the three
/// cases apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ingestion {
    /// The request produced these stored samples, in scope order.
    Ingested { sample_ids: Vec<String> },
    /// The request retained no decision-time input, so it has no features and
    /// no sample. Nothing was stored, and nothing was fabricated.
    NoDecisionTimeInput,
    /// The sample or its Outcome was refused. The reason is the boundary's own
    /// words; nothing is stored and nothing is repaired.
    Rejected { reason: String },
}

impl Ingestion {
    /// The ids stored, or an empty slice for the two non-storing cases.
    pub fn sample_ids(&self) -> &[String] {
        match self {
            Ingestion::Ingested { sample_ids } => sample_ids,
            _ => &[],
        }
    }

    /// Whether this ingestion stored at least one sample.
    pub fn is_ingested(&self) -> bool {
        matches!(self, Ingestion::Ingested { .. })
    }

    /// Whether nothing was stored because there were no decision-time features.
    pub fn is_without_decision_time_input(&self) -> bool {
        matches!(self, Ingestion::NoDecisionTimeInput)
    }
}

/// One snapshot of what the store collected, refused and evicted.
///
/// `no_decision_time_input` is the observable half of the dependency on
/// retained decision-time input: it counts requests that had no features to
/// collect from, which is a different fact from `rejected`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestionCounters {
    /// Requests that stored at least one sample.
    pub ingested: u64,
    /// Samples stored in total, across all requests.
    pub samples: u64,
    /// Requests with no retained decision-time input.
    pub no_decision_time_input: u64,
    /// Requests refused at the boundary, with a reason.
    pub rejected: u64,
    /// Samples dropped to stay inside the count bound.
    pub evicted_by_count: u64,
    /// Samples dropped for being older than the age bound.
    pub evicted_by_age: u64,
    /// Ingestion panics contained at the boundary.
    pub faults: u64,
}

/// Bounded store for canonical samples with a count and an age retention
/// policy.
///
/// The store holds [`OutcomeTrainingSample`]. The legacy [`TrainingSample`]
/// shape is reachable only through the explicit one-way
/// [`Self::legacy_training_slice`] adapter, never as the storage path.
pub struct DatasetStore {
    samples: Mutex<VecDeque<OutcomeTrainingSample>>,
    max_samples: usize,
    max_age_secs: i64,
    ingested: AtomicU64,
    samples_stored: AtomicU64,
    without_decision_time: AtomicU64,
    rejected: AtomicU64,
    evicted_by_count: AtomicU64,
    evicted_by_age: AtomicU64,
    faults: AtomicU64,
}

impl DatasetStore {
    pub fn new(max_samples: usize, max_age_secs: i64) -> Self {
        // A zero count bound can hold no sample and can evict none: the
        // retention loop below would spin on an empty queue forever while
        // holding the store lock. The bound is floored at one, so the store
        // always makes progress.
        let max_samples = max_samples.max(1);
        Self {
            samples: Mutex::new(VecDeque::with_capacity(max_samples.min(10_000))),
            max_samples,
            max_age_secs,
            ingested: AtomicU64::new(0),
            samples_stored: AtomicU64::new(0),
            without_decision_time: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            evicted_by_count: AtomicU64::new(0),
            evicted_by_age: AtomicU64::new(0),
            faults: AtomicU64::new(0),
        }
    }

    /// The production store: the bounds above, nothing configurable and no
    /// second feature source.
    pub fn production() -> Self {
        Self::new(PRODUCTION_MAX_SAMPLES, PRODUCTION_MAX_AGE_SECS)
    }

    /// The count bound.
    pub fn max_samples(&self) -> usize {
        self.max_samples
    }

    /// The age bound, in seconds.
    pub fn max_age_secs(&self) -> i64 {
        self.max_age_secs
    }

    /// Store one canonical sample, enforcing both retention bounds.
    ///
    /// The sample is validated first and refused with a reason if it is
    /// malformed or internally inconsistent; nothing is repaired and nothing
    /// invalid is stored. Expired samples are dropped before the insert, so a
    /// store whose traffic stops cannot keep expired samples resident.
    pub fn push(&self, sample: OutcomeTrainingSample) -> Result<(), String> {
        validate_outcome_sample(&sample)?;
        let mut samples = crate::sync::lock(&self.samples);
        self.evict_expired_locked(&mut samples, chrono::Utc::now().timestamp());
        while samples.len() >= self.max_samples {
            if samples.pop_front().is_none() {
                // The bound is never zero, but the loop still terminates on an
                // empty queue instead of spinning while holding the lock.
                break;
            }
            self.evicted_by_count.fetch_add(1, Ordering::Relaxed);
        }
        samples.push_back(sample);
        Ok(())
    }

    /// Ingest one request: the single validated Outcome plus the feature
    /// vectors retained at decision time.
    ///
    /// `request_id` is the correlation key and must be the request's own id: a
    /// caller that correlates the wrong record is refused rather than allowed
    /// to attribute one request's features to another request's outcome.
    ///
    /// `decision_time` is the request's retained shadow input, or `None` when it
    /// retained none. `None` yields [`Ingestion::NoDecisionTimeInput`] — never a
    /// sample built from re-derived or default features.
    ///
    /// All-or-nothing: the samples are built and validated together, and either
    /// every one of them is stored or none is. A request that is already
    /// represented is refused, so "one ingestion per request" is a property of
    /// the store rather than of the one caller that happens to exist today.
    pub fn ingest(
        &self,
        request_id: &str,
        outcome: &Outcome,
        decision_time: Option<&ShadowInput>,
    ) -> Ingestion {
        let Some(decision_time) = decision_time else {
            self.without_decision_time.fetch_add(1, Ordering::Relaxed);
            return Ingestion::NoDecisionTimeInput;
        };
        if outcome.request_id != request_id {
            return self.refuse(format!(
                "decision-time input correlated to request '{request_id}' does not match outcome request '{}'",
                outcome.request_id
            ));
        }
        let samples = match canonical_samples_from_decision_time(
            outcome,
            decision_time,
            DataOrigin::Native,
        ) {
            Ok(samples) => samples,
            Err(reason) => return self.refuse(reason),
        };
        let sample_ids = match self.store_all(request_id, &samples) {
            Ok(sample_ids) => sample_ids,
            Err(reason) => return self.refuse(reason),
        };
        self.ingested.fetch_add(1, Ordering::Relaxed);
        self.samples_stored
            .fetch_add(sample_ids.len() as u64, Ordering::Relaxed);
        Ingestion::Ingested { sample_ids }
    }

    /// Validate and store one whole ingestion, or none of it.
    fn store_all(
        &self,
        request_id: &str,
        samples: &[OutcomeTrainingSample],
    ) -> Result<Vec<String>, String> {
        for sample in samples {
            validate_outcome_sample(sample)?;
        }
        let mut stored = crate::sync::lock(&self.samples);
        if stored.iter().any(|sample| sample.request_id == request_id) {
            return Err(format!(
                "request '{request_id}' is already represented in the dataset"
            ));
        }
        self.evict_expired_locked(&mut stored, chrono::Utc::now().timestamp());
        // The count bound holds absolutely: a single request whose evidence
        // exceeds the whole store keeps its most recent samples and counts the
        // rest as evicted, rather than growing past the bound.
        let skipped = samples.len().saturating_sub(self.max_samples);
        let kept = &samples[skipped..];
        if skipped > 0 {
            self.evicted_by_count
                .fetch_add(skipped as u64, Ordering::Relaxed);
        }
        while stored.len() + kept.len() > self.max_samples {
            if stored.pop_front().is_some() {
                self.evicted_by_count.fetch_add(1, Ordering::Relaxed);
            } else {
                break;
            }
        }
        let sample_ids = kept.iter().map(|sample| sample.sample_id.clone()).collect();
        stored.extend(kept.iter().cloned());
        Ok(sample_ids)
    }

    /// Count one refusal and describe it.
    fn refuse(&self, reason: String) -> Ingestion {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        Ingestion::Rejected { reason }
    }

    /// Count one contained fault. Every panic this module can suffer is
    /// reported here rather than propagated.
    pub fn record_fault(&self) {
        self.faults.fetch_add(1, Ordering::Relaxed);
    }

    /// Everything the store collected, refused and evicted so far.
    pub fn counters(&self) -> IngestionCounters {
        IngestionCounters {
            ingested: self.ingested.load(Ordering::Relaxed),
            samples: self.samples_stored.load(Ordering::Relaxed),
            no_decision_time_input: self.without_decision_time.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            evicted_by_count: self.evicted_by_count.load(Ordering::Relaxed),
            evicted_by_age: self.evicted_by_age.load(Ordering::Relaxed),
            faults: self.faults.load(Ordering::Relaxed),
        }
    }

    /// Number of samples currently stored (regardless of age).
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.samples).len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        crate::sync::lock(&self.samples).is_empty()
    }

    /// Drop every sample older than the age bound and report how many left.
    pub fn evict_expired(&self) -> usize {
        let mut samples = crate::sync::lock(&self.samples);
        self.evict_expired_locked(&mut samples, chrono::Utc::now().timestamp())
    }

    fn evict_expired_locked(
        &self,
        samples: &mut VecDeque<OutcomeTrainingSample>,
        now: i64,
    ) -> usize {
        let before = samples.len();
        samples.retain(|sample| now - sample.timestamp < self.max_age_secs);
        let evicted = before - samples.len();
        if evicted > 0 {
            self.evicted_by_age
                .fetch_add(evicted as u64, Ordering::Relaxed);
        }
        evicted
    }

    /// The stored samples that are inside the retention window, oldest first.
    pub fn training_slice(&self) -> Vec<OutcomeTrainingSample> {
        let now = chrono::Utc::now().timestamp();
        crate::sync::lock(&self.samples)
            .iter()
            .filter(|sample| now - sample.timestamp < self.max_age_secs)
            .cloned()
            .collect()
    }

    /// The compatibility view of the retained samples.
    ///
    /// This is the only path from the store to the legacy [`TrainingSample`]
    /// shape, and it is one-way: nothing in this store is ever built from a
    /// legacy sample.
    pub fn legacy_training_slice(&self) -> Vec<TrainingSample> {
        self.training_slice()
            .into_iter()
            .map(OutcomeTrainingSample::into_legacy)
            .collect()
    }

    /// Clear all samples.
    pub fn clear(&self) {
        crate::sync::lock(&self.samples).clear();
    }
}

impl Default for DatasetStore {
    fn default() -> Self {
        Self::new(100_000, 30 * 24 * 3600) // 100k samples, 30 days
    }
}

/// Run one dataset step with any panic contained and counted.
///
/// A dataset problem is a dataset problem: the boundary reports it and the
/// request continues. The closure form is what makes that testable, because the
/// store's own failure modes are all ordinary refusals.
pub fn contain_dataset_fault<T>(
    store: &DatasetStore,
    step: impl FnOnce() -> T,
) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)) {
        Ok(value) => Ok(value),
        Err(panic) => {
            store.record_fault();
            let reason = panic
                .downcast_ref::<&str>()
                .map(|reason| (*reason).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown payload".to_string());
            Err(format!("dataset fault: {reason}"))
        }
    }
}

/// [`DatasetStore::ingest`] with any panic contained and turned into an
/// observable refusal, so ingestion can never fail a request.
pub fn contained_ingest(
    store: &DatasetStore,
    request_id: &str,
    outcome: &Outcome,
    decision_time: Option<&ShadowInput>,
) -> Ingestion {
    contain_dataset_fault(store, || store.ingest(request_id, outcome, decision_time))
        .unwrap_or_else(|reason| Ingestion::Rejected { reason })
}
