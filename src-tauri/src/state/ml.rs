//! The learned model: status, promotion, the durable trace log, and clearing it.

use super::views::*;
use super::Desktop;

impl Desktop {
    /// The learned model's status, read from the live serving components.
    ///
    /// Delegates to Core rather than re-deriving anything here, so the dashboard
    /// and the HTTP endpoint cannot disagree about what is serving.
    #[cfg(feature = "ml")]
    pub fn ml_status(&self) -> zroutery_core::MlStatus {
        self.core.ml_status()
    }

    /// Probe every declared account and return what the reconciler did.
    ///
    /// Delegates to Core rather than re-deriving anything here, for the same
    /// reason [`Self::ml_status`] does: the dashboard and the store must not be
    /// able to disagree about what an account is doing. The report is Core's own
    /// value, not a summary rebuilt in the desktop layer — a hand-built summary is
    /// how an absent quota starts rendering as a zero.
    ///
    /// Not called on a timer and not called at startup. Probing costs a network
    /// request per declared account, so the caller decides when; this is the
    /// operation to call, not a policy about when.
    #[cfg(feature = "account")]
    pub async fn reconcile_accounts(&self) -> zroutery_core::ReconcileReport {
        self.core.reconcile_accounts().await
    }

    /// Replay the serving model over recorded history.
    #[cfg(feature = "ml")]
    pub fn ml_shadow_analysis(&self, limit: usize) -> zroutery_core::ShadowAnalysisStatus {
        self.core.ml_shadow_analysis(limit)
    }

    /// Withdraw the current model and return to the previously promoted one.
    #[cfg(feature = "ml")]
    pub fn rollback_active_model(&self) -> zroutery_core::ReloadOutcome {
        self.core.rollback_active_model()
    }

    /// Run one promotion round over the durable history.
    ///
    /// The baseline is the caller's to choose; the evidence floors are not, and
    /// the shipped defaults are what this passes, so relaxing "what a model has
    /// to beat" can never become a way of stopping requiring evidence at all.
    #[cfg(feature = "ml")]
    pub fn ml_run_promotion_round(
        &self,
        install: bool,
        baseline: Option<String>,
    ) -> zroutery_core::ml::status::PromotionRoundStatus {
        let mut gate = zroutery_core::ml::PromotionConfig::default();
        if let Some(baseline) = baseline.as_deref() {
            gate.required_baseline = baseline.to_string();
        }
        let config = zroutery_core::ml::RoundConfig {
            gate,
            ..zroutery_core::ml::RoundConfig::default()
        };
        self.core.ml_run_promotion_round(config, install, None)
    }

    /// How much history is on disk, read through the log this process holds.
    ///
    /// The instance `AppState` owns rather than a second handle on the same path:
    /// two `TraceLog`s would be two independent append handles, and a count read
    /// from the wrong one describes a file nobody is writing to.
    #[cfg(feature = "ml")]
    pub fn ml_traces(&self) -> MlTraceInfo {
        match self.core.traces() {
            None => MlTraceInfo {
                records: None,
                bytes_on_disk: 0,
                path: None,
                counters: None,
                error: Some(
                    "no durable state directory is configured, so there is no trace log".into(),
                ),
            },
            Some(log) => match log.count() {
                // `count` streams the file, so this is a record count and not a
                // claim that the whole log was read into memory.
                Ok(records) => MlTraceInfo {
                    records: Some(records),
                    bytes_on_disk: std::fs::metadata(log.path()).map(|m| m.len()).unwrap_or(0),
                    path: Some(log.path().display().to_string()),
                    counters: Some(log.counters()),
                    error: None,
                },
                Err(error) => MlTraceInfo {
                    records: None,
                    bytes_on_disk: 0,
                    path: Some(log.path().display().to_string()),
                    counters: Some(log.counters()),
                    error: Some(error.to_string()),
                },
            },
        }
    }

    /// Discard the operator's own history, through the handle still appending to it.
    ///
    /// Goes through the live log on purpose: `TraceLog` holds its append handle
    /// for the lifetime of the process and Windows cannot delete an open file, so
    /// truncating through a second handle would leave the serving path holding a
    /// handle to a file it had already written.
    #[cfg(feature = "ml")]
    pub fn ml_clear_traces(&self) -> MlTraceClear {
        match self.core.traces() {
            None => MlTraceClear {
                cleared: false,
                removed_bytes: 0,
                error: Some(
                    "no durable state directory is configured, so there is no trace log".into(),
                ),
            },
            Some(log) => match log.clear() {
                Ok(outcome) => MlTraceClear {
                    cleared: outcome.cleared,
                    removed_bytes: outcome.removed_bytes,
                    error: None,
                },
                Err(error) => MlTraceClear {
                    cleared: false,
                    removed_bytes: 0,
                    error: Some(error.to_string()),
                },
            },
        }
    }
}
