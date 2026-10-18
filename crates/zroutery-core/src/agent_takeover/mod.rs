//! Agent ownership / adopt lifecycle.
//!
//! Manages the lifecycle of ownership over configuration fields that Zroutery
//! can adopt from an external agent and release back. Tracks managed field
//! values, detects external modifications, and supports clean adopt/release
//! cycles without state drift.
//!
//! State machine:
//!
//! ```text
//! Verified ──adopt()──▶ Adopted ──release()──▶ Released
//!    ▲                     │                      │
//!    └─────────────────────┘◀─────────────────────┘
//!
//! release_with_restore(): Adopted ──▶ Releasing ──(config restored)──▶ Released
//! ```
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`ownership`] | the ownership model and the durable store — what is managed, what it replaced, how that is recorded, and how it is restored |
//! | [`status`] | [`TakeoverStatus`], the nine-state surface machine, and the report a caller must supply before `active` may be claimed |
//! | [`clients`] | the adapters: where each client keeps its config, and the shape the OpenCode adapter refuses to rewrite |
//! | [`gateway`] | [`takeover_fields`] — the field set each client needs, which is the only thing that may be written |
//! | [`fs`] | atomic writes, permission preservation, and the nested-path walk that makes a dotted manifest path real |
//! | [`toml`] | a small TOML reader and writer, scoped to what Codex's `config.toml` contains |
//!
//! `fs` and `toml` are two halves of one job. They are only separate because
//! the TOML subset is 500 lines that no other client needs and no reader of a
//! takeover is looking for.
//!
//! # What is public, and why
//!
//! Everything the command layer and the tests need is re-exported here, so
//! `agent_takeover::X` resolves exactly as it did when this was one file. The
//! only item reaching outside the module is [`write_config_atomic`], which
//! `migration.rs` shares; it stays `pub(crate)` at the same path rather than
//! being reimplemented, because two atomic writers would be two chances to get
//! the temp-sibling-then-rename sequence wrong and only one of them tested.

mod clients;
mod fs;
mod gateway;
mod ownership;
mod status;
mod toml;

#[cfg(test)]
mod tests;

pub use clients::{
    adapter_for, AgentAdapter, AgentConfigSnapshot, AgentType, ClaudeAdapter, CodexAdapter,
    ManagedField, OpenCodeAdapter,
};
pub use gateway::{
    takeover_fields, GatewayTarget, TierClass, GATEWAY_PROVIDER_ID, OPENCODE_CONFIG_ENV,
};
pub use ownership::{
    extract_field_values, resolve_conflicts, ConflictResolution, ExternalModification,
    FieldConflict, OwnershipManifest, OwnershipState, ProposedChange, ReleaseOutcome,
    ResolutionReport, TakeoverProposal, TakeoverStore,
};
pub use status::{TakeoverStatus, VerificationReport};

pub(crate) use fs::write_config_atomic;

// The per-test config roots live in `fs` next to the real `home_dir` they
// shadow, and the fixtures reach them through the barrel the way they reached
// them when everything was one file.
#[cfg(test)]
pub(crate) use fs::{isolate_agent_home, isolate_config_dir, isolated_agent_home};
