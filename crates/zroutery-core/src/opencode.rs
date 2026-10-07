//! OpenCode gateway shaping: request identifiers and the free-tier fingerprint.
//!
//! OpenCode's gateway only serves its free tier to requests shaped like its own
//! CLI: an `opencode/<ver>` User-Agent, a canonical `x-opencode-session`, and a
//! body that streams and declares `bash`/`read` tools. This module owns the
//! OpenCode-specific parts — identifier generation, session canonicalisation and
//! the body/tool shaping — while everything generic (the HTTP client, the OpenAI
//! wire format, SSE) stays in the modules that already do it.
//!
//! The identifier algorithm reproduces the OpenCode CLI
//! (`packages/schema/identifier.ts`): `hex48(ts_ms * 0x1000 + counter)` followed
//! by 14 random base62 characters. The 14 random characters never form a
//! sequential run, which is why this sidesteps a Console rule that rejects
//! hand-made sequential ids (`abcdefghijklmnop` is refused; random ids pass).
//!
//! See `docs/development/opencode-zen.md` for the field-verified gate.

use serde_json::{json, Map, Value};

use std::sync::atomic::{AtomicI64, Ordering};

/// The base62 alphabet OpenCode's identifiers use.
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Prefix of a session id (`x-opencode-session`).
pub const SESSION_PREFIX: &str = "ses_";
/// Prefix of a per-request id (`x-opencode-request`).
pub const REQUEST_PREFIX: &str = "msg_";

/// A full identifier is 12 lowercase hex characters plus 14 base62 characters.
const HEX_LEN: usize = 12;
const BODY_LEN: usize = HEX_LEN + 14;

static LAST_TS: AtomicI64 = AtomicI64::new(0);
static COUNTER: AtomicI64 = AtomicI64::new(0);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The 26-character identifier body: 12 lowercase hex derived from
/// `ts_ms * 0x1000 + counter` (bit-inverted when `descending`), then 14 random
/// base62 characters.
pub fn identifier(descending: bool) -> String {
    let ts = now_ms();
    let previous = LAST_TS.swap(ts, Ordering::Relaxed);
    let counter = if ts != previous {
        COUNTER.store(1, Ordering::Relaxed);
        1
    } else {
        COUNTER.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    };
    let mut value = (ts as u64)
        .wrapping_mul(0x1000)
        .wrapping_add(counter as u64);
    if descending {
        value = !value;
    }

    let mut id = String::with_capacity(BODY_LEN);
    for index in 0..6 {
        let byte = ((value >> (40 - 8 * index)) & 0xff) as u8;
        id.push_str(&format!("{byte:02x}"));
    }
    for byte in uuid::Uuid::new_v4().into_bytes().iter().take(14) {
        id.push(BASE62[usize::from(*byte) % BASE62.len()] as char);
    }
    id
}

/// A fresh, canonical, per-conversation session id (`ses_…`).
pub fn session_id() -> String {
    format!("{SESSION_PREFIX}{}", identifier(true))
}

/// A fresh per-request id (`msg_…`).
pub fn request_id() -> String {
    format!("{REQUEST_PREFIX}{}", identifier(false))
}

/// Whether `value` already carries the canonical session shape the gateway
/// demands: `ses_` + 12 lowercase hex + 14 base62 (30 characters total).
pub fn is_canonical_session(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(SESSION_PREFIX) else {
        return false;
    };
    if rest.len() != BODY_LEN {
        return false;
    }
    let (hex, tail) = rest.split_at(HEX_LEN);
    hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && tail.bytes().all(|b| BASE62.contains(&b))
}

/// Canonicalise an arbitrary conversation signal into a `ses_` id.
///
/// Idempotent: an already-canonical id is returned unchanged, so a real OpenCode
/// client's session — and the upstream prompt-cache affinity it carries — is
/// preserved. Anything else (a UUID, a foreign session, Zroutery's own
/// conversation id) is deterministically hashed into the canonical shape, so the
/// same conversation always maps to the same session.
pub fn canonical_session(signal: &str) -> String {
    if is_canonical_session(signal) {
        return signal.to_string();
    }
    let bytes = derive_bytes(signal);
    let mut out = String::with_capacity(SESSION_PREFIX.len() + BODY_LEN);
    out.push_str(SESSION_PREFIX);
    for byte in &bytes[..6] {
        out.push_str(&format!("{byte:02x}"));
    }
    for byte in &bytes[6..20] {
        out.push(BASE62[usize::from(*byte) % BASE62.len()] as char);
    }
    out
}

/// 24 deterministic bytes for a signal (three salted 64-bit hashes).
///
/// `DefaultHasher::new()` is deterministic within a build, which is all a
/// session id needs; this is uniqueness, not security.
fn derive_bytes(signal: &str) -> [u8; 24] {
    use std::hash::{Hash, Hasher};
    let mut out = [0u8; 24];
    for (index, chunk) in out.chunks_mut(8).enumerate() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (index as u64).hash(&mut hasher);
        signal.hash(&mut hasher);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    out
}

/// The session for one request, in priority order:
///
/// 1. an inbound `x-opencode-session` that is already canonical — its upstream
///    prompt-cache affinity belongs to the client;
/// 2. `seed`, a conversation signal (a foreign session, Zroutery's own
///    conversation id, the first user message), deterministically canonicalised;
/// 3. a fresh random session when neither is available.
pub fn session_from_signal(inbound: Option<&str>, seed: Option<&str>) -> String {
    if let Some(value) = inbound.map(str::trim).filter(|s| !s.is_empty()) {
        if is_canonical_session(value) {
            return value.to_string();
        }
    }
    match seed.map(str::trim).filter(|s| !s.is_empty()) {
        Some(seed) => canonical_session(seed),
        None => session_id(),
    }
}

/// Shape an already-encoded OpenAI Chat Completions body for the OpenCode
/// gateway's free tier.
///
/// Forces `stream: true` (the gate's first rule), keeps
/// `stream_options.include_usage` on (so a non-streaming client still gets a
/// usage count when the stream is collapsed), ensures the `bash` and `read`
/// tools the gate requires, and — when the client declared no tools of its own —
/// sets `tool_choice: "none"` so the injected placeholders are not called in
/// place of an answer.
///
/// `client_declared_tools` is the caller's view of the request's own tool list.
pub fn shape_body(body: &mut Value, client_declared_tools: bool) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.insert("stream".into(), Value::Bool(true));
    let options = object
        .entry("stream_options")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(options) = options.as_object_mut() {
        options.insert("include_usage".into(), Value::Bool(true));
    }
    ensure_required_tools(object);
    if !client_declared_tools {
        object.insert("tool_choice".into(), Value::String("none".into()));
    }
}

/// Append a minimal definition for `bash`/`read` when the client did not declare
/// one. A tool the client already named is left exactly as it arrived — the gate
/// checks only the name, so its description and parameters are never rewritten.
fn ensure_required_tools(object: &mut Map<String, Value>) {
    let tools = object
        .entry("tools")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(tools) = tools.as_array_mut() else {
        return;
    };
    for name in ["bash", "read"] {
        if !tools.iter().any(|tool| tool_name(tool) == Some(name)) {
            tools.push(minimal_tool(name));
        }
    }
}

/// The function name of a tool entry, tolerating the Chat (`function.name`) and
/// the flat (`name`) shapes.
fn tool_name(tool: &Value) -> Option<&str> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .or_else(|| tool.get("name").and_then(Value::as_str))
}

fn minimal_tool(name: &str) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": format!("Agent tool {name}"),
            "parameters": {"type": "object", "properties": {}}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_lower_hex(byte: u8) -> bool {
        matches!(byte, b'0'..=b'9' | b'a'..=b'f')
    }

    #[test]
    fn identifier_body_has_the_cli_shape() {
        let id = identifier(true);
        assert_eq!(id.len(), BODY_LEN, "identifier body length");
        let (hex, tail) = id.split_at(HEX_LEN);
        assert!(hex.bytes().all(is_lower_hex), "hex half: {hex}");
        assert!(
            tail.bytes().all(|b| BASE62.contains(&b)),
            "base62 half: {tail}"
        );
    }

    #[test]
    fn identifiers_are_unique_per_call() {
        let a = request_id();
        let b = request_id();
        assert_ne!(a, b);
        assert!(a.starts_with(REQUEST_PREFIX));
    }

    #[test]
    fn generated_session_is_canonical() {
        let s = session_id();
        assert_eq!(s.len(), 30);
        assert!(s.starts_with(SESSION_PREFIX));
        assert!(
            is_canonical_session(&s),
            "generated session must be canonical: {s}"
        );
    }

    #[test]
    fn canonical_session_is_idempotent() {
        let s = session_id();
        assert_eq!(canonical_session(&s), s);
    }

    #[test]
    fn canonical_session_from_signal_is_stable_and_shaped() {
        let a = canonical_session("conv-123");
        let b = canonical_session("conv-123");
        assert_eq!(a, b, "same signal -> same session");
        assert!(
            is_canonical_session(&a),
            "derived session must be canonical: {a}"
        );
        assert_ne!(
            canonical_session("conv-124"),
            a,
            "different signal -> different session"
        );
    }

    #[test]
    fn canonical_session_rejects_non_canonical_shapes() {
        for bad in [
            "",
            "ses_",
            "ses_short",
            // Wrong length (13 in the body).
            "ses_0123456789abc",
            // Wrong length (27 in the body).
            "ses_0123456789abcdefghijklmnopq",
            // Uppercase hex is not the canonical form.
            "ses_ABCDEF012345abcdefghijklmn",
            // A character outside base62 in the tail.
            "ses_0123456789abcdefghijklmn_",
        ] {
            assert!(!is_canonical_session(bad), "should reject {bad:?}");
            // A non-canonical value is repaired, not passed through.
            assert!(is_canonical_session(&canonical_session(bad)));
        }
    }

    #[test]
    fn canonical_session_preserves_a_shape_valid_input() {
        // A sequential tail is shape-valid; `is_canonical_session` is a shape
        // check only. We never generate such ids, but a client that sends one
        // keeps it (its affinity is the client's).
        let sequential = "ses_0123456789abcdefghijklmnop";
        assert!(is_canonical_session(sequential));
        assert_eq!(canonical_session(sequential), sequential);
    }

    #[test]
    fn session_from_signal_prefers_a_canonical_inbound_value() {
        let canonical = "ses_0123456789abcdefghijklmnop";
        assert_eq!(
            session_from_signal(Some(canonical), Some("a-seed")),
            canonical
        );
    }

    #[test]
    fn session_from_signal_canonicalises_a_seed_and_ignores_a_bad_inbound() {
        let from_seed = session_from_signal(None, Some("conv-1"));
        assert!(is_canonical_session(&from_seed));
        assert_eq!(session_from_signal(None, Some("conv-1")), from_seed);
        // A non-canonical inbound value is ignored in favour of the seed.
        assert_eq!(
            session_from_signal(Some("not-a-canonical-session"), Some("conv-1")),
            from_seed
        );
    }

    #[test]
    fn session_from_signal_falls_back_to_a_fresh_id() {
        let a = session_from_signal(None, None);
        assert!(is_canonical_session(&a));
        assert_ne!(session_from_signal(None, None), a);
    }

    #[test]
    fn the_locked_fingerprint_headers_are_emitted() {
        use crate::config::{ClientIdentity, ProviderClientProfile, ProviderConfig, ProviderKind};
        let provider = ProviderConfig::new("oc", "OpenCode", ProviderKind::OpenAICompatible);
        let identity = ClientIdentity {
            profile: ProviderClientProfile::OpenCodeCli,
            passthrough: Vec::new(),
            session: Some("ses_0123456789abcdefghijklmnop".into()),
        };
        let headers =
            crate::upstream::build_headers_for_identity(&provider, Some("public"), &identity)
                .expect("headers build");
        let get = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).unwrap_or("");
        assert!(
            get("user-agent").starts_with("opencode/"),
            "ua={}",
            get("user-agent")
        );
        assert_eq!(get("x-opencode-client"), "cli");
        assert_eq!(get("x-opencode-project"), "global");
        assert_eq!(get("x-opencode-session"), "ses_0123456789abcdefghijklmnop");
        assert!(
            get("x-opencode-request").starts_with("msg_"),
            "request={}",
            get("x-opencode-request")
        );
    }

    #[test]
    fn shaping_forces_stream_and_usage() {
        let mut body = json!({"model": "m", "messages": [], "stream": false, "tools": []});
        shape_body(&mut body, true);
        assert_eq!(body["stream"].as_bool(), Some(true));
        assert_eq!(body["stream_options"]["include_usage"].as_bool(), Some(true));
    }

    #[test]
    fn shaping_fills_missing_bash_and_read() {
        let mut body = json!({"tools": [{"type": "function", "function": {"name": "grep"}}]});
        shape_body(&mut body, true);
        let names: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read"));
        assert_eq!(names.len(), 3, "only the missing two are appended");
    }

    #[test]
    fn shaping_leaves_a_client_tool_verbatim() {
        let mut body = json!({"tools": [
            {"type": "function", "function": {"name": "bash", "description": "MINE"}},
            {"type": "function", "function": {"name": "read", "description": "MY READ"}}
        ]});
        shape_body(&mut body, true);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2, "nothing appended when both are present");
        let bash = tools
            .iter()
            .find(|tool| tool["function"]["name"] == "bash")
            .unwrap();
        assert_eq!(bash["function"]["description"].as_str(), Some("MINE"));
    }

    #[test]
    fn shaping_sets_tool_choice_none_only_without_client_tools() {
        let mut no_tools = json!({"tools": []});
        shape_body(&mut no_tools, false);
        assert_eq!(no_tools["tool_choice"].as_str(), Some("none"));

        let mut with_tools = json!({"tools": [{"type": "function", "function": {"name": "x"}}]});
        shape_body(&mut with_tools, true);
        assert!(
            with_tools.get("tool_choice").is_none(),
            "a client that declared tools keeps its own choice"
        );
    }

    #[test]
    fn encode_for_mode_shapes_an_opencode_provider() {
        use crate::config::{ProviderConfig, ProviderClientProfile, ProviderKind};
        use crate::ir::{ChatRequest, Message};
        let mut provider = ProviderConfig::new("oc", "OpenCode", ProviderKind::OpenAICompatible);
        provider.client_profile = ProviderClientProfile::OpenCodeCli;
        let mut req = ChatRequest::new("space-bunny-free", provider.kind.dialect());
        req.messages.push(Message::user_text("hi"));
        let body = crate::upstream::encode_for_mode(
            &provider,
            &req,
            "space-bunny-free",
            None,
            crate::upstream::EncodeMode::Normal,
        )
        .expect("encodes");
        assert_eq!(body["stream"].as_bool(), Some(true));
        let names: Vec<&str> = body["tools"]
            .as_array()
            .expect("tools present")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert!(
            names.contains(&"bash") && names.contains(&"read"),
            "tools={names:?}"
        );
        assert_eq!(body["tool_choice"].as_str(), Some("none"));
    }
}
