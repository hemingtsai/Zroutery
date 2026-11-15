#![cfg(feature = "ml")]

//! Node 7E-2D gate tests: offline calibration and the K-way distribution.
//!
//! The claims under test are the ones this node owns —
//!
//! 1. purity: the module reaches no installation, scheduling, or routing
//!    surface;
//! 2. K-way normalization: the emitted vector sums to one within the 7E-2A
//!    tolerance, has arity equal to the axis, is accepted by the 7E-2A type,
//!    and an unranked candidate takes exactly `0.0` without moving the rest;
//! 3. calibration metrics: a binned reliability curve with per-bin counts and a
//!    summary error, measured **over the final emitted vector** through an API
//!    that cannot accept pre-normalization numbers, and sharp enough to catch a
//!    confidently-wrong model;
//! 4. determinism: the same snapshot and configuration give byte-identical
//!    calibrator and report, *and* two logically identical snapshots carrying
//!    deliberately different UUIDs give the same partition, calibrator,
//!    distributions, measurements and verdict;
//! 5. holdout and drift: the partitions are disjoint, a shifted holdout is
//!    refused, and a degenerate partition is refused rather than producing a
//!    calibrated-looking vector;
//! 6. fail-closed: every malformed input is a typed refusal with a reason, and
//!    the module holds no panic path.

mod calibration_metrics;
mod determinism;
mod fail_closed;
mod holdout_and_drift;
mod kway_normalization;
mod purity;
mod support;
