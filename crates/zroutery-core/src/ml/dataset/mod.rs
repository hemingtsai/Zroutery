//! Training dataset for ML routing models.
//!
//! Combines [`RoutingFeatures`](super::features::RoutingFeatures) snapshots,
//! [`Outcome`](crate::outcome::Outcome) results, and
//! [`FeedbackSignal`](crate::feedback::FeedbackSignal) signals into
//! [`TrainingSample`] units that ML models consume.
//!
//! [`DatasetStore`] holds the canonical [`OutcomeTrainingSample`] under both a
//! count and an age bound, and ingests one request at a time through
//! [`DatasetStore::ingest`].
//!
//! # Where a sample's feature vector comes from
//!
//! The [`Outcome`](crate::outcome::Outcome) deliberately holds identity,
//! terminal state, timing and usage only — it has no feature vector, and
//! nothing here derives one. The exact per-candidate vectors captured at
//! decision time live in the retained shadow record's
//! [`ShadowInput::candidates`](super::shadow::ShadowInput), so ingestion takes
//! the retained decision-time input as an input and correlates it with the
//! request's one Outcome by request id.
//!
//! A request with no retained record — the `ml` feature off,
//! `config.shadow.enabled` false, evaluation refused, or a shadow fault — has no
//! features and therefore no sample. That case is reported as
//! [`Ingestion::NoDecisionTimeInput`] and counted separately from a refusal, so
//! "nothing was collected" is never indistinguishable from "something was
//! collected and rejected". See [`canonical_samples_from_decision_time`].

mod conversion;
mod sample;
mod store;
mod validation;

pub use conversion::*;
pub use sample::*;
pub use store::*;
pub use validation::*;

#[cfg(test)]
mod tests;
