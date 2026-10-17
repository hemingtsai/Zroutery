//! Fanning an [`Outcome`] out to the feedback and observation stores.

use crate::failure::FailureClass;
use crate::observation::ObservationStore;
use crate::stats_ext::StatsStore;

use super::Outcome;
impl Outcome {
    /// Pure Outcome-to-Feedback bridge.  `None` means no signal was supplied;
    /// outcome success/failure never creates a rating.
    pub fn to_feedback(
        &self,
        signals: Option<Vec<crate::feedback::FeedbackSignal>>,
        timestamp: i64,
        source: crate::feedback::FeedbackSource,
        data_origin: crate::feedback::DataOrigin,
    ) -> Option<crate::feedback::Feedback> {
        crate::feedback::feedback_from_outcome(self, signals, timestamp, source, data_origin)
    }

    /// Checked variant of [`Outcome::to_feedback`].
    pub fn try_to_feedback(
        &self,
        signals: Option<Vec<crate::feedback::FeedbackSignal>>,
        timestamp: i64,
        source: crate::feedback::FeedbackSource,
        data_origin: crate::feedback::DataOrigin,
    ) -> Result<Option<crate::feedback::Feedback>, String> {
        crate::feedback::try_feedback_from_outcome(self, signals, timestamp, source, data_origin)
    }

    /// Convenience form for a concrete signal list.
    pub fn to_feedback_with_signals(
        &self,
        signals: Vec<crate::feedback::FeedbackSignal>,
        timestamp: i64,
        source: crate::feedback::FeedbackSource,
        data_origin: crate::feedback::DataOrigin,
    ) -> Option<crate::feedback::Feedback> {
        self.to_feedback(Some(signals), timestamp, source, data_origin)
    }

    /// Write outcome data to the observation store and stats store.
    ///
    /// For each attempt, records success or classified failure against the
    /// corresponding (model, provider) pair. This is the single fan-out point
    /// that keeps the two stores in sync.
    pub fn record_to_observation(&self, obs_store: &ObservationStore, stats_store: &StatsStore) {
        for attempt in &self.attempts {
            if attempt.is_terminal_success() {
                obs_store.record_success(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    attempt.latency_ms,
                    attempt.ttft_ms,
                );
                stats_store.record_success(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    attempt.latency_ms,
                    attempt.ttft_ms,
                );
            } else {
                let class = attempt.failure_class.unwrap_or(FailureClass::Unknown);
                obs_store.record_classified_failure(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    &crate::failure::ClassifiedFailure {
                        class,
                        status: attempt.http_status,
                        message: attempt.failure_message.clone().unwrap_or_default(),
                        impact: class.impact(),
                    },
                );
                stats_store.record_classified_failure(
                    &attempt.candidate_model,
                    &attempt.candidate_provider,
                    class,
                );
            }
        }
    }
}
