//! Importing providers from CC Switch.
//!
//! CC Switch (https://github.com/farion1231/cc-switch) keeps its provider list
//! in `~/.cc-switch/cc-switch.db` (SQLite; newer versions) or
//! `~/.cc-switch/config.json` (older ones). Each Claude Code provider is a
//! name plus the exact environment block Claude Code runs with — most
//! importantly `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN` and the
//! `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL` tier defaults.
//!
//! Those map onto Zroutery almost one to one: a provider becomes a
//! `ProviderConfig` (Anthropic dialect — every Claude Code relay speaks it),
//! and each tier default becomes a `ModelEntry` with that class. A relay that
//! serves the same model for every tier collapses to one entry, classified as
//! sonnet: that is the tier a conversation actually asks for, and an import
//! that leaves `sonnet-class` empty would be a poor surprise.
//!
//! Nothing is imported in place: reading produces drafts, the dashboard shows
//! them, and only the selected ones are written — providers into the config,
//! API keys straight into the OS credential store, never the config file.
//!
//! # Why this is five files
//!
//! Four jobs shared one file and no boundary: finding CC Switch's data,
//! describing what it contains, deciding what of it should be imported, and
//! turning the accepted drafts into Zroutery config. Half the file was
//! fixtures, and the other half was four answers to four different questions
//! with nothing to say which was which.
//!
//! | file | holds |
//! |---|---|
//! | `read` | locating CC Switch's data and parsing it |
//! | `model` | the shapes on both sides of the import |
//! | `plan` | which entries become new providers, and under what id |
//! | `convert` | validating drafts and writing them into the config |

mod convert;
mod model;
mod plan;
mod read;

#[cfg(test)]
mod tests;

pub use convert::{
    build_report, to_zroutery, unique_provider_id, validate_draft, validate_import_batch,
    ImportReport,
};
pub use model::{CcModel, CcProvider, CcProviderDraft, SourceAccount};
pub use plan::{
    account_of, credential_fingerprint, plan_import, source_target_id, AlreadyImported,
    ImportDecision, PlannedImport,
};
pub use read::{cc_switch_dir, read_providers};
