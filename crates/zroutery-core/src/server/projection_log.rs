//! Where the observability projection is invoked from.
//!
//! `observability::project_request` and `observability::project_batch` are pure
//! read-only functions over an `Outcome` and, optionally, the `RouteDecision`
//! that request was routed by. Until this module existed nothing in the shipped
//! product called them, so a projection capability that had been written,
//! reviewed and accepted was unreachable from any request.
//!
//! What lives here
//!
//! A bounded in-memory log of `(Outcome, Option<RouteDecision>)` pairs, written
//! exactly once per request at the same point in the terminal transition where
//! the outcome itself is written. It is a ring buffer for the same reason
//! `OutcomeLog` is: the request log is deliberately memory only, and a
//! diagnostic view that silently grows without bound is a leak wearing a
//! diagnostic's name.
//!
//! It is a *holding* structure, not a second accounting. Nothing routes on it,
//! no training consumes it, and its contents never reach a response. The
//! projections are computed on demand from the retained pairs, so the value of
//! what is stored is the same value the projection would consume — there is no
//! precomputed copy that could drift from it.
//!
//! Refusals are the point
//!
//! The projection refuses rather than guessing: a record with no decision id is
//! reported absent rather than filled in with the planned identity, and a
//! supplied decision whose id disagrees with the record refuses. Both cases
//! happen on the real serving path — a request that was not policy-routed has
//! no decision id at all — so `Withdrawn`/routed traffic produces a genuine
//! mixture of projections and refusals. This module reports both and never
//! routes around a refusal to make the run look clean. See [`ProjectionLog::batch`]
//! for the accounting invariant that makes a silent drop impossible.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::observability::{project_batch, project_request, Projection, ProjectionBatch};
use crate::outcome::Outcome;
use crate::policy::RouteDecision;

/// One retained request: the terminal outcome and the decision it was routed
/// by, if it was policy-routed at all.
///
/// The decision is optional rather than defaulted, and that is the whole point:
/// `None` here means "this request carried no routing decision", which is a fact
/// about the request. It is what lets the projection report an absent decision
/// id as absent instead of inventing one.
type Pair = (Outcome, Option<RouteDecision>);

/// A bounded log of terminal outcomes paired with their routing decisions.
#[derive(Debug)]
pub struct ProjectionLog {
    inner: Mutex<ProjectionLogInner>,
}

#[derive(Debug)]
struct ProjectionLogInner {
    limit: usize,
    pairs: VecDeque<Pair>,
}

impl ProjectionLog {
    /// A log holding at most `limit` requests.
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Mutex::new(ProjectionLogInner {
                limit: limit.max(1),
                pairs: VecDeque::new(),
            }),
        }
    }

    /// Retain one request's outcome and, when it had one, its decision.
    ///
    /// The lifecycle calls this exactly once per request, after the outcome has
    /// been validated, so what is retained is the same validated outcome the
    /// dataset boundary reads. Nothing is derived here: this stores, the
    /// projection reads.
    pub fn record(&self, outcome: Outcome, decision: Option<RouteDecision>) {
        let mut inner = crate::sync::lock(&self.inner);
        let limit = inner.limit;
        inner.pairs.push_back((outcome, decision));
        while inner.pairs.len() > limit {
            inner.pairs.pop_front();
        }
    }

    /// How many requests are retained.
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.inner).pairs.len()
    }

    /// Whether nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop everything retained.
    pub fn clear(&self) {
        crate::sync::lock(&self.inner).pairs.clear();
    }

    /// Project every retained request individually, in arrival order.
    ///
    /// This is the per-request form the serving path reasons about: one call,
    /// one `Projection`, and a refusal is a refusal rather than a gap in a
    /// list. The order is arrival order rather than content order, which is why
    /// determinism claims are made about the projection *content* and not about
    /// this list's order.
    pub fn projections(&self) -> Vec<Projection> {
        let inner = crate::sync::lock(&self.inner);
        inner
            .pairs
            .iter()
            .map(|(outcome, decision)| project_request(outcome, decision.as_ref()))
            .collect()
    }

    /// The correlated batch view: one record per request, content-ordered.
    ///
    /// Delegates to the accepted pure function, so this method adds storage and
    /// nothing else. The batch's accounting invariant — `records + refusals`
    /// equals the number of requests handed in — is what makes a silently
    /// dropped record impossible, and it is asserted by the node's tests rather
    /// than assumed here.
    pub fn batch(&self) -> ProjectionBatch {
        let inner = crate::sync::lock(&self.inner);
        let outcomes: Vec<Outcome> = inner
            .pairs
            .iter()
            .map(|(outcome, _)| outcome.clone())
            .collect();
        let decisions: Vec<RouteDecision> = inner
            .pairs
            .iter()
            .filter_map(|(_, decision)| decision.clone())
            .collect();
        project_batch(&outcomes, &decisions)
    }

    /// The retained requests, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Pair> {
        let inner = crate::sync::lock(&self.inner);
        inner.pairs.iter().rev().take(limit).cloned().collect()
    }
}

impl Default for ProjectionLog {
    fn default() -> Self {
        Self::new(500)
    }
}
