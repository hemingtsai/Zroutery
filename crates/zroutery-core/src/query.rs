//! What kind of request this is, as opposed to which model it asks for.
//!
//! `Resolution` answers "which model" — a direct id or a tier. This module
//! answers "which *purpose*": a request from the client's main conversation, or
//! one of the side queries a client such as Claude Code issues alongside it.
//!
//! The two dimensions are deliberately kept apart. The same model string
//! (`claude-opus-4-8[1m]`) can arrive as a main request and as an Auto Mode
//! classifier request, so "is this the classifier" can never be derived from
//! the model name — only from the request's own shape.
//!
//! Routing intent lives here rather than on [`crate::ir::ChatRequest`] so the
//! protocol IR stays a pure representation of the wire payload and is never
//! polluted by one client's features.

use serde::{Deserialize, Serialize};

/// The kinds of side query Zroutery knows how to route separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideQueryKind {
    /// Claude Code Auto Mode's permission classifier: a small, deterministic
    /// request asking a model to judge a proposed action and answer with an
    /// XML `<block>` verdict.
    AutoMode,
    /// Codex's Guardian auto-review: a review request asking a model whether a
    /// planned action may proceed, answered with a JSON `outcome` (and, for a
    /// denial, the risk it saw). It is the same kind of question as Auto Mode
    /// — may this action run without asking the user? — asked in a different
    /// protocol, so it is routed to the same review pool.
    CodexGuardian,
}

impl SideQueryKind {
    /// Stable name used in stats and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            SideQueryKind::AutoMode => "auto_mode",
            SideQueryKind::CodexGuardian => "codex_guardian",
        }
    }

    /// Whether this side query is answered with a `<block>yes|no</block>` tag
    /// rather than Codex's JSON contract.
    pub fn uses_block_verdict(self) -> bool {
        matches!(self, SideQueryKind::AutoMode)
    }
}

/// Why the request exists, decided before any model resolution happens.
///
/// Serialises as its stable name (`"main"` / `"auto_mode"`) so request records
/// stay flat and readable in the activity log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// A normal request from the client's conversation.
    #[default]
    Main,
    /// A side query the client issued alongside the main conversation.
    Side(SideQueryKind),
}

impl RequestKind {
    /// Stable name used in stats and logs: `main` / `auto_mode`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RequestKind::Main => "main",
            RequestKind::Side(SideQueryKind::AutoMode) => "auto_mode",
            RequestKind::Side(SideQueryKind::CodexGuardian) => "codex_guardian",
        }
    }

    pub fn is_main(&self) -> bool {
        matches!(self, RequestKind::Main)
    }
}

impl Serialize for RequestKind {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RequestKind {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = RequestKind;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("`main`, `auto_mode` or `codex_guardian`")
            }
            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<RequestKind, E> {
                match value {
                    "main" => Ok(RequestKind::Main),
                    "auto_mode" => Ok(RequestKind::Side(SideQueryKind::AutoMode)),
                    "codex_guardian" => Ok(RequestKind::Side(SideQueryKind::CodexGuardian)),
                    other => Err(E::unknown_variant(
                        other,
                        &["main", "auto_mode", "codex_guardian"],
                    )),
                }
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// The client-visible model id with any client-side suffix modifier stripped.
///
/// Claude Code (and clients following it) appends modifiers such as `[1m]` to
/// select a context-window tier. The modifier is a client-side routing hint,
/// not part of the model's name: `claude-opus-4-8[1m]` is the same model as
/// `claude-opus-4-8`, seen through a one-million-token window. Zroutery's
/// registry keys on the plain name, so the modifier has to go before
/// resolution — but the original string is what gets logged and echoed, since
/// that is what the client asked for.
///
/// Only a short alphanumeric tag in trailing brackets is treated as a
/// modifier, so genuine ids containing brackets are left alone.
pub fn strip_client_model_modifier(model: &str) -> &str {
    let Some(open) = model.rfind('[') else {
        return model;
    };
    let suffix = &model[open + 1..];
    let Some(tag) = suffix.strip_suffix(']') else {
        return model;
    };
    // A real modifier is short and alphanumeric (e.g. `1m`). Anything longer,
    // or containing other characters, is more likely part of an id.
    if !tag.is_empty()
        && tag.len() <= 8
        && tag.chars().all(|c| c.is_ascii_alphanumeric())
        // `[1m]` on its own is not a model with a modifier; stripping it would
        // leave nothing to route.
        && open > 0
    {
        &model[..open]
    } else {
        model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_are_stable_for_stats() {
        assert_eq!(RequestKind::Main.as_str(), "main");
        assert_eq!(
            RequestKind::Side(SideQueryKind::AutoMode).as_str(),
            "auto_mode"
        );
    }

    #[test]
    fn strips_short_bracket_modifiers() {
        assert_eq!(
            strip_client_model_modifier("claude-opus-4-8[1m]"),
            "claude-opus-4-8"
        );
        assert_eq!(
            strip_client_model_modifier("claude-sonnet-4-5[16m]"),
            "claude-sonnet-4-5"
        );
        // A bare modifier with nothing before it stays whole rather than
        // collapsing to an empty string.
        assert_eq!(strip_client_model_modifier("[1m]"), "[1m]");
    }

    #[test]
    fn leaves_real_ids_alone() {
        assert_eq!(
            strip_client_model_modifier("claude-opus-4-8"),
            "claude-opus-4-8"
        );
        // Long or non-alphanumeric bracket content is treated as part of the id.
        assert_eq!(
            strip_client_model_modifier("model[not-a-modifier]"),
            "model[not-a-modifier]"
        );
        assert_eq!(strip_client_model_modifier("model[]"), "model[]");
        // An unmatched bracket is nothing special.
        assert_eq!(strip_client_model_modifier("model[1m"), "model[1m");
        // Only the *trailing* bracket group counts.
        assert_eq!(
            strip_client_model_modifier("model[1m]tail"),
            "model[1m]tail"
        );
    }

    #[test]
    fn serde_names_match_the_stats_strings() {
        assert_eq!(
            serde_json::to_string(&RequestKind::Side(SideQueryKind::AutoMode)).unwrap(),
            "\"auto_mode\""
        );
        assert_eq!(
            serde_json::to_string(&RequestKind::Main).unwrap(),
            "\"main\""
        );
        assert_eq!(
            serde_json::from_str::<RequestKind>("\"main\"").unwrap(),
            RequestKind::Main
        );
    }
}
