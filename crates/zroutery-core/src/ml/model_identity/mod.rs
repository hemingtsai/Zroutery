//! Model identity, versioning, checkpoints, and replay for ML routing.
//!
//! Provides content-addressed model commits, checkpoint management,
//! learning event tracking, and deterministic replay of training history.

mod checkpoint;
mod event;
mod identity;
mod store;

pub use checkpoint::*;
pub use event::*;
pub use identity::*;
pub use store::*;

/// Schema version of the model-state serialization consumed by this module.
pub const MODEL_STATE_SCHEMA_VERSION: u32 = 1;
/// Schema version of the checkpoint envelope consumed by this module.
pub const MODEL_CHECKPOINT_SCHEMA_VERSION: u32 = 1;
/// Schema version of the commit envelope consumed by this module.
pub const MODEL_COMMIT_SCHEMA_VERSION: u32 = 1;
/// Schema version of the learning-event envelope consumed by this module.
pub const LEARNING_EVENT_SCHEMA_VERSION: u32 = 1;
/// Marker used only for artifacts serialized before envelope versioning.
pub const LEGACY_UNVERSIONED_SCHEMA_VERSION: u32 = 0;

#[cfg(test)]
mod tests;
