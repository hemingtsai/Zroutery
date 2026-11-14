//! The only plugin/data-plane bridge. No QuickJS, worker or plugin IPC dependency.
//! Verified declarations call the existing codecs, Upstream and settlement.
mod bindings;
pub mod registry;
pub use bindings::{PluginBinding, PluginPort, PluginSendScope, SlotIdentity};
