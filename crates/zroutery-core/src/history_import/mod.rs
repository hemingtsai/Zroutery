//! History imported from other tools, kept apart from this instance's own usage.
//!
//! Two rules shape every type here:
//!
//! * **A fact keeps its provenance.** The source, the source instance, the
//!   source's own entity id and its granularity travel with the fact, so a day
//!   aggregate can never be mistaken for a request and a re-import can recognise
//!   what it already holds.
//! * **Unknown is not zero.** A source that reports no price, no latency or no
//!   status leaves those fields absent. The importer never fills them with a
//!   plausible-looking default, because a fabricated HTTP 200 or a zero cost is
//!   a measurement the source never made.
//!
//! Imported history feeds history statistics only. It is not a budget, wallet or
//! health fact, and no path from here reaches the training data.

use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::billing::Cost;
use crate::ir::Usage;
use crate::runtime_store::RuntimeStore;

pub mod ccswitch;
pub mod claudecode;
pub mod opencode;

/// The stop signal an import honours between chunks.
///
/// Cancellation is cooperative and prompt at chunk boundaries, which is the
/// natural stopping point: the source read is read-only, and a cancelled run
/// discards everything it staged, so no partial import is ever published.
#[derive(Clone, Default)]
pub struct ImportCancel(Arc<AtomicU8>);

impl ImportCancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the running import to stop before its next chunk.
    pub fn cancel(&self) -> bool {
        match self
            .0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(1) => true,
            Err(_) => false,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == 1
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("import cancelled".into())
        } else {
            Ok(())
        }
    }

    /// The successful CAS is the publication boundary. Cancellation before it
    /// rolls back; after it the caller is told publication has already started.
    pub(crate) fn publish<T>(
        &self,
        commit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "import cancelled or already publishing".to_string())?;
        let result = commit();
        self.0
            .store(if result.is_ok() { 3 } else { 0 }, Ordering::Release);
        result
    }
}

impl std::fmt::Debug for ImportCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportCancel")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// A history source the importer can read.
///
/// The kind is decided by what the chosen path *is*, not by what the caller
/// claims: a folder holds client transcripts, and a file has to be a SQLite
/// database whose tables the cc-switch adapter recognises. A wrong guess by a
/// menu would import nothing or the wrong tool's history; a property of the
/// path cannot be guessed wrongly.
pub enum Source {
    CcSwitch(ccswitch::CcSwitchSource),
    ClaudeCode(claudecode::ClaudeCodeSource),
    OpenCode(opencode::OpenCodeSource),
}

impl Source {
    pub fn open(path: &Path, instance: impl Into<String>) -> Result<Self, String> {
        Self::open_with_cancel(path, instance, &ImportCancel::new())
    }

    pub fn open_with_cancel(
        path: &Path,
        instance: impl Into<String>,
        cancel: &ImportCancel,
    ) -> Result<Self, String> {
        cancel.check()?;
        let instance = instance.into();
        if path.is_dir() {
            return claudecode::ClaudeCodeSource::open_with_cancel(path, instance, cancel)
                .map(Self::ClaudeCode);
        }
        if opencode::OpenCodeSource::recognizes(path)? {
            return opencode::OpenCodeSource::open(path, instance).map(Self::OpenCode);
        }
        ccswitch::CcSwitchSource::open(path, instance).map(Self::CcSwitch)
    }

    /// The source family this path turned out to be, for the confirmation
    /// screen: the label the facts will carry.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::CcSwitch(_) => ccswitch::SOURCE,
            Self::ClaudeCode(_) => claudecode::SOURCE,
            Self::OpenCode(_) => opencode::SOURCE,
        }
    }

    pub fn preview(&self, range: &ImportRange, samples: usize) -> Result<ImportPreview, String> {
        match self {
            Self::CcSwitch(source) => source.preview(range, samples),
            Self::ClaudeCode(source) => source.preview(range, samples),
            Self::OpenCode(source) => source.preview(range, samples),
        }
    }

    pub fn import(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        chunk: usize,
        cancel: &ImportCancel,
    ) -> Result<ImportOutcome, String> {
        match self {
            Self::CcSwitch(source) => source.import(store, range, chunk, cancel),
            Self::ClaudeCode(source) => source.import(store, range, chunk, cancel),
            Self::OpenCode(source) => source.import(store, range, chunk, cancel),
        }
    }

    /// Prepare a durable preview. Only normalised accounting metadata is staged;
    /// confirming this preview never reopens a changing source.
    pub fn stage(
        &self,
        store: &RuntimeStore,
        range: &ImportRange,
        run_id: &str,
        cancel: &ImportCancel,
    ) -> Result<ImportPreview, String> {
        match self {
            Self::CcSwitch(source) => source.stage(store, range, run_id, cancel),
            Self::ClaudeCode(source) => source.stage(store, range, run_id, cancel),
            Self::OpenCode(source) => source.stage(store, range, run_id, cancel),
        }
    }
}

/// The source's own token accounting, as declared on the row.
///
/// cc-switch stores raw counts and records which question the count answers;
/// carrying that through keeps the conversion auditable instead of applying a
/// subtraction no one can re-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSemantics {
    /// Written before the semantics column existed. Cache-inclusive apps stored
    /// the total; every other app stored the fresh count.
    Legacy,
    /// `input_tokens` already contains the cache reads and writes.
    Total,
    /// `input_tokens` excludes the cache reads and writes.
    Fresh,
}

impl InputSemantics {
    fn from_ccswitch(raw: i64) -> Result<Self, String> {
        match raw {
            0 => Ok(Self::Legacy),
            1 => Ok(Self::Total),
            2 => Ok(Self::Fresh),
            _ => Err(format!("unknown cc-switch input token semantics {raw}")),
        }
    }
}

/// Whether a day carries request detail or a materialised day total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Request,
    Day,
}

/// What the importer decided about a row that may already be counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Duplicate {
    /// No evidence of another row saying the same thing.
    None,
    /// Another row in the same source describes the same call within the
    /// source's own matching window. Kept, shown, and excluded from totals
    /// until a person confirms it.
    Suspected,
}

impl Duplicate {
    /// Only an unopposed fact contributes to the canonical totals.
    pub fn is_counted(self) -> bool {
        matches!(self, Self::None)
    }
}

/// One request or one day of usage, as the source reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedFact {
    /// Source family, e.g. `ccswitch`.
    pub source: String,
    /// Which copy of the source database. Re-importing a different machine's
    /// history must not overwrite this one's facts.
    pub instance: String,
    /// The source's own stable id for the row.
    pub entity_id: String,
    /// Detects a changed row without trusting timestamps the source may not
    /// keep. The store computes it from the serialised fact, so an adapter
    /// leaves it empty on the way in.
    pub revision: String,
    pub granularity: Granularity,
    /// UTC instant of the request, or local midnight for a day total.
    pub at: DateTime<Utc>,
    /// Local calendar day, `YYYY-MM-DD`, as the source bucketed it.
    pub day: String,
    /// The client application the source attributed the call to.
    pub client: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub request_model: Option<String>,
    pub pricing_model: Option<String>,
    /// Normalised so that cache reads and writes are inside `input_tokens`,
    /// which is this repository's accounting invariant.
    pub tokens: Usage,
    pub input_semantics: Option<InputSemantics>,
    /// Known spend only.
    pub cost: Option<Cost>,
    /// What the source itself recorded, including a reported zero. Kept apart so
    /// a zero can be read as "the source said zero", never as "priced at zero".
    pub reported_cost: Option<Cost>,
    /// Whether a price was actually applied. A reported zero is not a price.
    pub priced: bool,
    pub latency_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub status: Option<u16>,
    pub ok: Option<bool>,
    pub streaming: bool,
    /// Requests this fact stands for: 1 for detail, the day's count for a total.
    pub requests: u64,
    pub failures: u64,
    /// The source's own provenance label (`proxy`, `session_log`, ...).
    pub data_source: String,
    pub duplicate: Duplicate,
    pub duplicate_of: Option<String>,
    pub multiplier: Option<f64>,
    pub error: Option<String>,
}

impl ImportedFact {
    /// The invariant check every adapter must pass before a fact is stored.
    pub fn validate(&self) -> Result<(), String> {
        if self.source.trim().is_empty() || self.instance.trim().is_empty() {
            return Err("an imported fact needs a source and an instance".into());
        }
        if self.entity_id.trim().is_empty() {
            return Err("an imported fact needs the source's entity id".into());
        }
        if self.requests == 0 {
            return Err("an imported fact stands for at least one request".into());
        }
        if self.failures > self.requests {
            return Err("more failures than requests in one imported fact".into());
        }
        if self.requests > i64::MAX as u64 || self.failures > i64::MAX as u64 {
            return Err("imported request count exceeds storage range".into());
        }
        NaiveDate::parse_from_str(&self.day, "%Y-%m-%d")
            .map_err(|_| "an imported fact needs a valid calendar day")?;
        if self
            .tokens
            .cache_read_tokens
            .checked_add(self.tokens.cache_write_tokens)
            .is_none_or(|cache| cache > self.tokens.input_tokens)
            || self.tokens.reasoning_tokens > self.tokens.output_tokens
        {
            return Err("imported token subsets exceed their total".into());
        }
        if self.priced != self.cost.is_some() {
            return Err("imported pricing completeness disagrees with known cost".into());
        }
        for cost in [self.cost.as_ref(), self.reported_cost.as_ref()]
            .into_iter()
            .flatten()
        {
            if !cost.amount.is_finite() || cost.amount < 0.0 {
                return Err("an imported cost must be finite and non-negative".into());
            }
            if cost.currency.trim().is_empty() {
                return Err("an imported cost needs a currency".into());
            }
        }
        if self
            .multiplier
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err("invalid imported cost multiplier".into());
        }
        if self.duplicate == Duplicate::Suspected && self.duplicate_of.is_none() {
            return Err("a suspected duplicate must name the row it duplicates".into());
        }
        Ok(())
    }
}

/// The time window an import reads. Bounds are half-open: `from` inclusive,
/// `until` exclusive, both UTC.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportRange {
    pub from: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl ImportRange {
    pub fn new(from: Option<DateTime<Utc>>, until: Option<DateTime<Utc>>) -> Result<Self, String> {
        if let (Some(from), Some(until)) = (&from, &until) {
            if from >= until {
                return Err("an import window must end after it starts".into());
            }
        }
        Ok(Self { from, until })
    }

    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.from.is_none_or(|from| at >= from) && self.until.is_none_or(|until| at < until)
    }

    pub fn validate(&self) -> Result<(), String> {
        Self::new(self.from, self.until).map(|_| ())
    }

    pub(crate) fn contains_fact(&self, fact: &ImportedFact) -> bool {
        if !self.contains(fact.at) {
            return false;
        }
        if fact.granularity == Granularity::Day {
            let end = NaiveDate::parse_from_str(&fact.day, "%Y-%m-%d")
                .ok()
                .and_then(|day| day.succ_opt())
                .and_then(|day| day_start_utc(&day.to_string()));
            return end.is_some_and(|end| self.until.is_none_or(|until| end <= until));
        }
        true
    }
}

/// What a preview found, without writing anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportPreview {
    pub source: String,
    pub instance: String,
    /// The source database's declared schema version.
    pub source_version: i64,
    pub requests: u64,
    pub aggregates: u64,
    pub suspected: u64,
    /// Day totals skipped because the same day still has request detail.
    pub covered_totals: u64,
    pub priced: u64,
    pub unpriced: u64,
    pub failures: u64,
    pub days: Vec<String>,
    pub warnings: Vec<String>,
    /// A few facts for the confirmation screen; never the whole import.
    pub samples: Vec<ImportedFact>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub range: ImportRange,
    #[serde(default)]
    pub cancelled: bool,
}

impl ImportPreview {
    /// Every fact the import would store (detail plus totals).
    pub fn facts(&self) -> u64 {
        self.requests + self.aggregates
    }

    /// Facts that would count towards the canonical totals.
    pub fn counted(&self) -> u64 {
        self.facts() - self.suspected
    }
}

/// What a commit did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportOutcome {
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
    /// True when the run was cancelled before anything was published.
    ///
    /// A cancelled import publishes nothing: the counters are zero, and this
    /// flag — not a failed count — is what the caller reports to the person
    /// who pressed stop.
    #[serde(default)]
    pub cancelled: bool,
    pub revision: i64,
}

impl ImportOutcome {
    pub fn facts(&self) -> u64 {
        self.inserted + self.updated + self.unchanged
    }
}

/// Totals over imported facts, canonical rows only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportTotals {
    pub requests: u64,
    pub failures: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Requests whose cost the source did not price.
    pub unpriced_requests: u64,
    /// Suspected duplicates, kept but counted separately.
    pub suspected_requests: u64,
    /// Requests that came from a day total rather than from detail.
    pub aggregate_requests: u64,
    pub cost: crate::billing::CostTotals,
}

/// A bounded, indexed read over imported facts, consistent with one revision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportedHistoryQuery {
    pub page: u32,
    pub page_size: u32,
    pub expected_revision: Option<i64>,
    pub source: Option<String>,
    pub instance: Option<String>,
    pub client: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub granularity: Option<Granularity>,
    pub duplicate: Option<Duplicate>,
    /// Completeness proxy: `Some(true)` for priced facts, `Some(false)` for unpriced.
    pub priced: Option<bool>,
    pub from: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl ImportedHistoryQuery {
    pub fn validate(&self) -> Result<(), String> {
        ImportRange::new(self.from, self.until)?;
        if self.expected_revision.is_some_and(|revision| revision < 0) {
            return Err("expected revision cannot be negative".into());
        }
        Ok(())
    }

    pub fn check_revision(&self, revision: i64) -> Result<(), String> {
        if self
            .expected_revision
            .is_some_and(|expected| expected != revision)
        {
            return Err("imported history changed; refresh from the first page".into());
        }
        Ok(())
    }

    pub fn pagination(&self, total: u64) -> (u32, u32, u64) {
        let size = self.page_size.clamp(1, 200);
        if total == 0 {
            return (1, size, 0);
        }
        let last = total.div_ceil(u64::from(size)).max(1);
        let page = u64::from(self.page.max(1)).min(last) as u32;
        (page, size, u64::from(page - 1) * u64::from(size))
    }
}

/// One page of imported facts, read at the reported revision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportedHistoryPage {
    pub records: Vec<ImportedFact>,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub revision: i64,
}

/// Scope for clearing imported history. Local usage/spend/wallet/health are never affected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportClearScope {
    pub source: Option<String>,
    pub instance: Option<String>,
    pub client: Option<String>,
}

/// Stage one chunk of an import run, with the cancel check and the cursor
/// bookkeeping both adapters share.
///
/// The cursor records the last entity the chunk staged. It is the run's
/// observable progress, and it only moves when the staged facts are durable;
/// re-staging an entity on a resumed run replaces the staged copy, so an
/// interrupted import is finished by re-reading rather than guessed at.
pub(crate) fn stage_chunk(
    store: &RuntimeStore,
    run: &crate::runtime_store::ImportRun,
    pending: &mut Vec<ImportedFact>,
    cancel: &ImportCancel,
) -> Result<(), String> {
    cancel.check()?;
    if pending.is_empty() {
        return Ok(());
    }
    let cursor = pending.last().map(|fact| fact.entity_id.clone());
    store.stage_import(&run.id, pending, cursor.as_deref())?;
    pending.clear();
    Ok(())
}

/// Finish a staged run the way its ending demands: cancelled runs discard
/// everything they staged, completed runs publish in one transaction.
pub(crate) fn finish_import(
    store: &RuntimeStore,
    preview: &ImportPreview,
    cancel: &ImportCancel,
) -> Result<ImportOutcome, String> {
    if preview.cancelled || cancel.is_cancelled() {
        if let Some(id) = &preview.run_id {
            store.discard_import_run(id)?;
        }
        return Ok(ImportOutcome {
            cancelled: true,
            revision: store.imported_revision()?,
            ..ImportOutcome::default()
        });
    }
    store.publish_import(
        preview
            .run_id
            .as_deref()
            .ok_or("preview has no staged run")?,
        preview
            .fingerprint
            .as_deref()
            .ok_or("preview has no fingerprint")?,
        cancel,
    )
}

/// Convert a source's raw counts into this repository's invariant: cache reads
/// and writes are inside `input_tokens`.
///
/// The legacy branch follows cc-switch's own read rule rather than guessing: a
/// cache-inclusive app's legacy row already contained its reads, so only writes
/// are added; every other legacy row stored a fresh count.
pub fn normalise_input(
    raw_input: u32,
    cache_read: u32,
    cache_write: u32,
    semantics: InputSemantics,
    cache_inclusive_app: bool,
) -> Result<u32, String> {
    let total = match semantics {
        InputSemantics::Total => Some(raw_input),
        InputSemantics::Fresh => raw_input
            .checked_add(cache_read)
            .and_then(|sum| sum.checked_add(cache_write)),
        InputSemantics::Legacy => {
            if cache_inclusive_app {
                raw_input.checked_add(cache_write)
            } else {
                raw_input
                    .checked_add(cache_read)
                    .and_then(|sum| sum.checked_add(cache_write))
            }
        }
    }
    .ok_or("imported token count overflow")?;
    if cache_read
        .checked_add(cache_write)
        .is_none_or(|cache| cache > total)
    {
        return Err("imported cache counts exceed input total".into());
    }
    Ok(total)
}

/// The local calendar day an instant falls in, in the operator's zone.
pub fn local_day(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

/// Local midnight of a `YYYY-MM-DD` day, as UTC. Ambiguous or non-existent
/// local midnights (DST) take the earlier instant; an unresolvable zone falls
/// back to treating the day as UTC rather than dropping the row.
pub fn day_start_utc(day: &str) -> Option<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let midnight = date.and_hms_opt(0, 0, 0)?;
    if let Some(local) = Local.from_local_datetime(&midnight).earliest() {
        return Some(local.with_timezone(&Utc));
    }
    Some(Utc.from_utc_datetime(&midnight))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_invariant_holds_for_every_declared_semantics() {
        assert_eq!(
            normalise_input(100, 30, 10, InputSemantics::Total, true).unwrap(),
            100
        );
        assert_eq!(
            normalise_input(100, 30, 10, InputSemantics::Fresh, true).unwrap(),
            140
        );
        assert_eq!(
            normalise_input(100, 30, 10, InputSemantics::Fresh, false).unwrap(),
            140
        );
        assert_eq!(
            normalise_input(100, 30, 0, InputSemantics::Legacy, true).unwrap(),
            100,
            "a cache-inclusive legacy row already held its reads"
        );
        assert_eq!(
            normalise_input(100, 30, 10, InputSemantics::Legacy, false).unwrap(),
            140
        );
        // A relay that reports a cache count larger than its own prompt total
        // must not wrap or go negative.
        assert_eq!(
            normalise_input(0, 5, 0, InputSemantics::Fresh, false).unwrap(),
            5
        );
        assert!(normalise_input(u32::MAX, 1, 0, InputSemantics::Fresh, false).is_err());
        assert!(normalise_input(1, 2, 0, InputSemantics::Total, false).is_err());
        assert!(InputSemantics::from_ccswitch(99).is_err());
    }

    #[test]
    fn a_day_total_without_a_price_is_not_a_zero_cost_request() {
        let fact = ImportedFact {
            source: "ccswitch".into(),
            instance: "laptop".into(),
            entity_id: "session:abc".into(),
            revision: String::new(),
            granularity: Granularity::Request,
            at: "2026-10-31T00:00:00Z".parse().unwrap(),
            day: "2026-10-31".into(),
            client: Some("claude".into()),
            provider_id: Some("_session".into()),
            model: Some("claude-sonnet-4-5".into()),
            request_model: None,
            pricing_model: None,
            tokens: Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Usage::default()
            },
            input_semantics: Some(InputSemantics::Fresh),
            cost: None,
            reported_cost: Some(Cost {
                currency: "USD".into(),
                amount: 0.0,
            }),
            priced: false,
            latency_ms: None,
            ttft_ms: None,
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
        };
        fact.validate().unwrap();
        assert!(fact.cost.is_none() && !fact.priced);

        let mut bad = fact.clone();
        bad.duplicate = Duplicate::Suspected;
        assert!(
            bad.validate().is_err(),
            "a suspected duplicate names its twin"
        );

        let mut empty = fact;
        empty.requests = 0;
        assert!(empty.validate().is_err());
    }

    #[test]
    fn windows_are_half_open_and_ordered() {
        let from: DateTime<Utc> = "2026-10-01T00:00:00Z".parse().unwrap();
        let until: DateTime<Utc> = "2026-10-02T00:00:00Z".parse().unwrap();
        assert!(ImportRange::new(Some(until), Some(from)).is_err());
        let range = ImportRange::new(Some(from), Some(until)).unwrap();
        assert!(range.contains(from));
        assert!(!range.contains(until));
    }

    #[test]
    fn a_local_day_round_trips_through_its_utc_start() {
        let at: DateTime<Utc> = "2026-10-31T00:00:00Z".parse().unwrap();
        let day = local_day(at);
        assert_eq!(day.len(), 10);
        let start = day_start_utc(&day).unwrap();
        assert_eq!(local_day(start), day);
        assert!(day_start_utc("not-a-day").is_none());
    }
}
