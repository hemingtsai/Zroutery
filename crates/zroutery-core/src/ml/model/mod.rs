//! Core model abstraction and concrete models for ML routing.
//!
//! Provides the [`RoutingModel`] trait that all routing models implement,
//! along with concrete models:
//!
//! - [`SuccessModel`]: binary classifier for request success prediction (AdaGrad).
//! - [`LatencyModel`]: online linear regression for total latency (SGD + EWMA).
//! - [`TtftModel`]: online linear regression for time-to-first-token (SGD + EWMA).
//! - [`CostModel`]: online linear regression for cost prediction (SGD + EWMA).

mod base;
mod regression;
mod success;

pub use base::*;
pub use regression::*;
pub use success::*;

#[cfg(test)]
mod tests;
