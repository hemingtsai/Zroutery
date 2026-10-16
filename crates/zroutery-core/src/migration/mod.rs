//! Migration support for moving from an external router to Zroutery.
//!
//! Provides state-machine driven migration with step-by-step plans,
//! rollback capability, and result history tracking.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`state`] | [`MigrationState`] and the plan it executes — what a migration is, what step it is on, and what it may do next |
//! | [`result`] | [`MigrationResult`] and the snapshots a rollback restores from |
//! | [`store`] | [`MigrationStore`], the durable record of how far a migration got |
//! | [`executor`] | the credential scanner and [`MigrationExecutor`], which steps a migration forward or rolls it back |
//!
//! The split follows the four questions a caller actually has: what is this, what
//! happened, where is it recorded, and who runs it. `state` defines the machine,
//! `executor` is the only thing that advances it, and `store` is the only thing
//! that remembers — so reading any one of them does not require reading the rest.
//!
//! # What is public, and why
//!
//! Everything is re-exported here, so `migration::X` resolves exactly as it did
//! when this was one file and no caller moved.

mod executor;
mod result;
mod state;
mod store;

#[cfg(test)]
mod tests;

pub use executor::MigrationExecutor;
pub use result::{MigrationResult, MigrationSnapshot, SnapshotFile};

pub use state::{
    EndpointExpect, MigrationAction, MigrationPlan, MigrationState, MigrationStep, SnapshotWriter,
    StdFsWriter,
};
pub use store::MigrationStore;
