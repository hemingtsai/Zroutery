#![cfg(feature = "ml")]

//! Node 7E-2F gate tests: immutable snapshots and atomic, journaled activation.
//!
//! Every accepted ML node in this repository is in-memory or a pure function.
//! This one writes, so each claim below is a property of bytes on disk rather
//! than of a field a handle happens to hold — and, because the mechanism must
//! be unreachable, the last gate is a structural one: a scan of the source tree
//! proving that nothing in the shipped product can name any of it.
//!
//! The gates, in the order this file asserts them:
//!
//! 1. **Verified immutable snapshot.** A snapshot is content-addressed, verifies
//!    on load through the accepted integrity and envelope checks, and one that
//!    fails any of them is refused. Writing never mutates an existing snapshot;
//!    loading never repairs one.
//! 2. **Atomic activation.** The pointer changes atomically, a reader never
//!    observes a partial or missing pointer, and a verification failure at
//!    activation time leaves the previous active snapshot byte-for-byte as it
//!    was. There is no state in which nothing is active and none in which a
//!    corrupt snapshot is.
//! 3. **Rollback.** Returning to the previous snapshot is atomic and journaled,
//!    and is refused when there is no previous snapshot.
//! 4. **Fail-closed.** Every refusal is typed and carries a reason: an
//!    unwritable location, a path that is not a snapshot, an unknown or legacy
//!    envelope, a failed integrity check, a failed lineage, a missing rollback
//!    target, and a journal that refuses. Never a panic, never a default, never
//!    a cold start.
//! 5. **Journaled.** Every activation and rollback is recorded as a *new*
//!    journal record carrying the snapshot identity and the commit, and the
//!    journal's own refusals propagate.
//! 6. **Reachability.** This module calls no live activation seam, and nothing in
//!    `server/`, `src-tauri/`, `ui/`, or `config.rs` names it — while the Tauri
//!    application still does not enable the `ml` feature.

mod atomic_activation;
mod crash_states;
mod journaling;
mod location_and_locking;
mod reachability;
mod rollback;
mod snapshot_gate;
mod support;
