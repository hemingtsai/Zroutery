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

pub mod console;
pub mod oauth;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::ir::{ChatResponse, ContentBlock};

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

/// Session ids this process has already registered with the OpenCode gateway
/// (`GET …/models` carrying the session header registers it). The gateway
/// rejects chat calls from a session it has never seen with a quota-flavored
/// 429 (field-verified 2026-11-21), so a fresh session registers before its
/// first chat call. Bounded: 256 sessions cover any realistic window, and a
/// clear forgets registrations whose re-registration costs one ungated GET.
static REGISTERED_SESSIONS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

pub fn is_session_registered(session: &str) -> bool {
    REGISTERED_SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(session)
}

pub fn mark_session_registered(session: &str) {
    let mut set = REGISTERED_SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if set.len() >= 256 {
        set.clear();
    }
    set.insert(session.to_string());
}

/// The environment block OpenCode's own system prompt carries (`<env>` with a
/// `Working directory:` line), said for `dir` — the home folder when empty.
/// The gateway's free tier serves a request as OpenCode's own only when a
/// system message carries one; without it a request gets a small allowance a
/// day, then a quota-flavored 429 (field-verified 2026-11-21; matches OpenCode
/// 2.2.5's `packages/core/src/instructions/builtins.ts` word for word).
pub fn environment_block() -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "unknown".into());
    let platform = std::env::consts::OS;
    let date = chrono::Utc::now().format("%a %b %d %Y");
    format!(
        "Here is some useful information about the environment you are running in:\n\
         <env>\n  \
         Working directory: {home}\n  \
         Workspace root folder: {home}\n  \
         Is directory a git repo: no\n  \
         Platform: {platform}\n  \
         Today's date: {date}\n\
         </env>"
    )
}

/// Whether the chat body already carries the environment block in a system or
/// developer message (OpenCode's own client does; other harnesses need it
/// injected). Text may be a plain string or an array of content parts.
pub fn has_environment_block(body: &Value) -> bool {
    fn carries_environment(content: &Value) -> bool {
        match content {
            Value::String(text) => text.contains("<env>") && text.contains("Working directory:"),
            Value::Array(parts) => parts.iter().any(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains("<env>"))
            }),
            _ => false,
        }
    }
    body.pointer("/messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                matches!(
                    message.get("role").and_then(Value::as_str),
                    Some("system") | Some("developer")
                ) && message.get("content").is_some_and(carries_environment)
            })
        })
        .unwrap_or(false)
}

/// Completes the system prompt with the environment block when it is missing:
/// a fresh leading system message is prepended, mirroring OpenCode's own
/// client — existing messages are never rewritten, so content-part arrays
/// survive untouched. The free tier answers a request without the block with
/// its small anonymous allowance and then a quota-flavored 429. Responses-
/// surface bodies carry it in `instructions` instead, appended when missing.
pub fn ensure_environment_block(body: &mut Value) {
    if has_environment_block(body) {
        return;
    }
    let Some(object) = body.as_object_mut() else {
        return;
    };
    let block = environment_block();
    match object.get_mut("messages") {
        Some(Value::Array(messages)) => {
            messages.insert(0, serde_json::json!({"role": "system", "content": block}));
        }
        _ => {
            let existing = object
                .get("instructions")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            object["instructions"] = Value::String(format!("{existing}\n\n{block}"));
        }
    }
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

/// Shape-canonicalise an arbitrary signal into a `ses_` id.
///
/// The 12-hex prefix here is a hash, **not** a timestamp. The gateway parses
/// that prefix as one and refuses a session whose embedded time is implausible
/// (field-verified 2026-11-27: a hashed prefix decoding to 1970 was answered
/// with the quota-flavored 429 while a real-timestamp id with the same request
/// was served). [`session_from_signal`] therefore never sends this form
/// upstream; it remains the pure shape helper for callers that need a
/// deterministic, shape-valid id.
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

/// Session ids already handed to one conversation, keyed by the conversation
/// seed. The gateway parses the id's 12-hex prefix as a timestamp and refuses
/// an implausible one (see [`canonical_session`]), so a seed cannot be hashed
/// into the id directly. The first request of a conversation instead gets a
/// fresh [`session_id`] — a real timestamp — and later requests reuse it for
/// upstream prompt-cache affinity. Bounded like [`REGISTERED_SESSIONS`]; a
/// clear costs one new session and one re-registration.
static SESSIONS_BY_SEED: std::sync::LazyLock<std::sync::Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// The session for one request, in priority order:
///
/// 1. an inbound `x-opencode-session` that is already canonical — its upstream
///    prompt-cache affinity belongs to the client;
/// 2. the session already created for `seed`, a conversation signal (a foreign
///    session, Zroutery's own conversation id, the first user message);
/// 3. a fresh [`session_id`] for an unseen seed, remembered for (2).
pub fn session_from_signal(inbound: Option<&str>, seed: Option<&str>) -> String {
    if let Some(value) = inbound.map(str::trim).filter(|s| !s.is_empty()) {
        if is_canonical_session(value) {
            return value.to_string();
        }
    }
    let Some(seed) = seed.map(str::trim).filter(|s| !s.is_empty()) else {
        return session_id();
    };
    let mut sessions = SESSIONS_BY_SEED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = sessions.get(seed) {
        return existing.clone();
    }
    let id = session_id();
    if sessions.len() >= 256 {
        sessions.clear();
    }
    sessions.insert(seed.to_string(), id.clone());
    id
}

/// Drop a tool call to a placeholder we injected only to satisfy the gate, when
/// the client never declared that tool. A call to a tool the client itself
/// declared is kept — only our fabricated `bash`/`read` are noise.
pub fn strip_injected_tool_calls(resp: &mut ChatResponse, declared: &[String]) {
    const INJECTED: [&str; 2] = ["bash", "read"];
    resp.content.retain(|block| match block {
        ContentBlock::ToolUse { name, .. } => {
            !(INJECTED.contains(&name.as_str()) && !declared.iter().any(|tool| tool == name))
        }
        _ => true,
    });
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

/// Shape an already-encoded OpenAI Chat Completions body for the OpenCode
/// gateway's free tier.
///
/// Forces `stream: true` with `stream_options.include_usage`, and guarantees
/// the `bash`/`read` tool definitions the gate requires
/// (`crates/zroutery-core/src/opencode/mod.rs::ensure_required_tools`). The
/// gateway checks those two names, not the rest of the catalogue: a client's
/// own tools pass through untouched, and a client that declared none gets the
/// minimal pair plus `tool_choice: "none"` so the placeholders are not called
/// in place of an answer.
///
/// Measured 2026-11-27: the same request, TLS handshake, identity headers and
/// body without the two tool entries is refused with the quota-flavored 429;
/// adding them is the difference between that refusal and a 200.
pub fn shape_v2_body(body: &mut Value, client_declared_tools: bool) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.insert("stream".into(), Value::Bool(true));
    let options = object.entry("stream_options").or_insert_with(|| json!({}));
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

/// `provider_id` reported in errors from this module.
const PROVIDER_ID: &str = "opencode";

/// One model's capabilities, mapped from the catalogue.
///
/// The OpenCode family and every other provider share one parser and one
/// cache: models.dev is the source of record and serves the same document.
pub use crate::models_dev::CatalogModel;

/// The `opencode` section of the public catalogue.
pub async fn catalog() -> Result<HashMap<String, CatalogModel>> {
    crate::models_dev::provider_catalog(PROVIDER_ID).await
}

/// Fill discovered models with the catalogue's capabilities, windows, effort
/// levels and free flag. Ids the catalogue does not describe are left
/// untouched.
pub async fn enrich(models: &mut [crate::upstream::DiscoveredModel]) -> Result<()> {
    let catalog = catalog().await?;
    for model in models {
        if let Some(entry) = catalog.get(&model.id) {
            model.context_window = entry.context_window;
            model.max_output = entry.max_output;
            model.free = entry.free;
            model.capabilities = Some(entry.capabilities.clone());
            model.effort_levels = entry.effort_levels.clone();
        }
    }
    Ok(())
}

/// Parse the `opencode.models` map out of a catalogue document.
pub fn parse_catalog(json: &Value) -> HashMap<String, CatalogModel> {
    crate::models_dev::parse_provider(json, PROVIDER_ID)
}

pub(super) use crate::models_dev::catalog_model;

/// Wrap a stream so a tool call to an injected placeholder the client never
/// declared is dropped — the gate's `bash`/`read` must not leak to a client that
/// did not ask for them. The `ToolUseStart` is dropped and its index muted, so
/// the deltas and the block-stop that follow are dropped too.
pub fn filter_injected_tool_calls(
    stream: crate::upstream::EventStream,
    declared: std::collections::HashSet<String>,
) -> crate::upstream::EventStream {
    use futures_util::StreamExt as _;
    const INJECTED: [&str; 2] = ["bash", "read"];
    let mut muted: std::collections::HashSet<u32> = std::collections::HashSet::new();
    Box::pin(stream.filter_map(move |item| {
        let event = match item {
            Ok(event) => event,
            Err(error) => return std::future::ready(Some(Err(error))),
        };
        let drop_event = match &event {
            crate::ir::StreamEvent::ToolUseStart { index, name, .. } => {
                if INJECTED.contains(&name.as_str()) && !declared.contains(name) {
                    muted.insert(*index);
                    true
                } else {
                    false
                }
            }
            crate::ir::StreamEvent::ToolUseDelta { index, .. }
            | crate::ir::StreamEvent::BlockStop { index } => muted.contains(index),
            _ => false,
        };
        std::future::ready(if drop_event { None } else { Some(Ok(event)) })
    }))
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
    fn session_from_signal_reuses_one_fresh_id_per_seed() {
        let from_seed = session_from_signal(None, Some("conv-1"));
        assert!(is_canonical_session(&from_seed));
        assert_eq!(session_from_signal(None, Some("conv-1")), from_seed);
        // A non-canonical inbound value is ignored in favour of the seed.
        assert_eq!(
            session_from_signal(Some("not-a-canonical-session"), Some("conv-1")),
            from_seed
        );
        // Different conversations get different sessions.
        assert_ne!(session_from_signal(None, Some("conv-2")), from_seed);
    }

    #[test]
    fn session_from_signal_embeds_a_real_timestamp() {
        // The gateway parses the 12-hex prefix as a timestamp and refuses an
        // implausible value with the quota-flavored 429 (2026-11-27): a
        // never-used session with random hex was refused, while the same
        // request with a generated id was served. The algorithm keeps the low
        // 36 bits of `ts * 0x1000 + counter`, so decoding `>> 12` must equal
        // the low 36 bits of the current clock.
        let id = session_from_signal(None, Some("conv-timestamp"));
        let stored = u64::from_str_radix(&id[4..16], 16).expect("hex prefix");
        let decoded = ((!stored) & ((1u64 << 48) - 1)) >> 12;
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let delta = decoded as i64 - (now & ((1u64 << 36) - 1)) as i64;
        assert!(
            delta.abs() <= 1_000,
            "session prefix does not encode the current time (delta {delta}ms)"
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
            native_anthropic: false,
            omit_server_review: false,
            profile: ProviderClientProfile::OpenCodeCli,
            passthrough: Vec::new(),
            session: Some("ses_0123456789abcdefghijklmnop".into()),
            org_id: None,
            reply_headers: Default::default(),
            observation: Default::default(),
        };
        let headers =
            crate::upstream::build_headers_for_identity(&provider, Some("public"), &identity)
                .expect("headers build");
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
        };
        assert!(
            get("user-agent").starts_with("opencode/"),
            "ua={}",
            get("user-agent")
        );
        assert_eq!(get("x-opencode-client"), "opencode");
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
        assert_eq!(
            body["stream_options"]["include_usage"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn v2_shaping_carries_the_required_tools_and_none_choice() {
        let mut body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        shape_v2_body(&mut body, false);
        let names: Vec<&str> = body["tools"]
            .as_array()
            .expect("tools are created")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert_eq!(names, ["bash", "read"]);
        assert_eq!(body["tool_choice"].as_str(), Some("none"));
        assert_eq!(body["stream"].as_bool(), Some(true));

        // A client that declared its own tools keeps them and gets no forced
        // choice; only the missing required names are appended.
        let mut declared = json!({"tools": [{"type": "function", "function": {"name": "grep"}}]});
        shape_v2_body(&mut declared, true);
        assert_eq!(declared["tools"].as_array().unwrap().len(), 3);
        assert!(declared.get("tool_choice").is_none());
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
        use crate::config::{ProviderClientProfile, ProviderConfig, ProviderKind};
        use crate::ir::{ChatRequest, Message};
        let mut provider = ProviderConfig::new("oc", "OpenCode", ProviderKind::OpenAICompatible);
        provider.client_profile = ProviderClientProfile::OpenCodeCli;
        let mut req = ChatRequest::new("space-bunny-free", provider.dialect());
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
            .expect("tools are created")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert_eq!(
            names,
            ["bash", "read"],
            "V2 adds the two required tool names when the client declared none"
        );
        assert_eq!(body["tool_choice"].as_str(), Some("none"));
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn injected_placeholder_calls_are_stripped_when_undeclared() {
        use crate::ir::{StopReason, Usage};
        let tool = |name: &str| ContentBlock::ToolUse {
            id: format!("call_{name}"),
            name: name.into(),
            input: json!({}),
        };
        let names = |resp: &ChatResponse| -> Vec<String> {
            resp.content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect()
        };
        let response = |content: Vec<ContentBlock>| ChatResponse {
            id: "x".into(),
            model: "m".into(),
            content,
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            usage: Usage::default(),
            passthrough: Default::default(),
        };

        // Client declared nothing: the injected bash/read are dropped; a name we
        // never inject stays.
        let mut resp = response(vec![tool("bash"), tool("read"), tool("my_tool")]);
        strip_injected_tool_calls(&mut resp, &[]);
        assert_eq!(names(&resp), vec!["my_tool".to_string()]);

        // Client declared `bash`: its own bash call is kept.
        let mut resp = response(vec![tool("bash")]);
        strip_injected_tool_calls(&mut resp, &["bash".to_string()]);
        assert_eq!(names(&resp), vec!["bash".to_string()]);
    }

    #[test]
    fn parses_a_catalog_entry_into_capabilities() {
        let json = json!({
            "opencode": {"models": {
                "space-bunny-free": {
                    "attachment": true,
                    "reasoning": true,
                    "tool_call": true,
                    "structured_output": true,
                    "modalities": {"input": ["text", "image", "video"], "output": ["text"]},
                    "limit": {"context": 1048576, "output": 524288},
                    "cost": {"input": 0, "output": 0}
                },
                "claude-opus-5": {
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "limit": {"context": 200000, "output": 64000},
                    "cost": {"input": 3, "output": 15}
                }
            }}
        });
        let catalog = parse_catalog(&json);
        let free = &catalog["space-bunny-free"];
        assert!(free.free);
        assert_eq!(free.context_window, Some(1048576));
        assert_eq!(free.max_output, Some(524288));
        assert!(free.capabilities.vision);
        assert!(free.capabilities.video);
        assert!(free.capabilities.tools);
        assert!(free.capabilities.thinking);
        assert!(free.capabilities.structured_output);

        let paid = &catalog["claude-opus-5"];
        assert!(!paid.free);
        assert!(!paid.capabilities.vision);
        assert_eq!(paid.context_window, Some(200000));
    }

    #[test]
    fn a_missing_catalog_section_is_empty() {
        assert!(parse_catalog(&json!({})).is_empty());
    }

    #[tokio::test]
    async fn streaming_filter_drops_undeclared_injected_calls() {
        use crate::ir::StreamEvent;
        use futures_util::StreamExt as _;
        let events = vec![
            StreamEvent::ToolUseStart {
                index: 0,
                id: "1".into(),
                name: "bash".into(),
            },
            StreamEvent::ToolUseDelta {
                index: 0,
                partial_json: "{}".into(),
            },
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::ToolUseStart {
                index: 1,
                id: "2".into(),
                name: "my_tool".into(),
            },
            StreamEvent::ToolUseDelta {
                index: 1,
                partial_json: "{}".into(),
            },
        ];
        let stream: crate::upstream::EventStream =
            Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)));
        let filtered = filter_injected_tool_calls(stream, std::collections::HashSet::new());
        let out: Vec<StreamEvent> = filtered.map(|item| item.unwrap()).collect().await;
        assert!(
            !out.iter()
                .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "bash")),
            "bash block must be dropped entirely"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "my_tool")),
            "a non-injected tool call survives"
        );
    }
}

/// Shaping + collapse against an in-process fake gateway that mirrors a captured
/// free-tier SSE stream, plus an opt-in live smoke. Lives in-crate so it can use
/// `axum`/`tokio` directly, the way the NewAPI adapter's fake panel does.
#[cfg(test)]
mod gateway_tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::Response;
    use axum::routing::post;
    use axum::Router;
    use futures_util::StreamExt as _;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    use crate::config::{ClientIdentity, ProviderClientProfile, ProviderConfig, ProviderKind};
    use crate::ir::{ChatRequest, Message};
    use crate::protocol::StreamCollector;
    use crate::upstream::{build_headers_for_identity, encode_for_mode, EncodeMode, Upstream};

    /// A captured stream: a text delta, a tool call, a usage-only chunk, `[DONE]`
    /// and the OpenCode `cost` trailer.
    const SSE: &str = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"space-bunny-free\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}],\"usage\":null}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"space-bunny-free\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}],\"usage\":null}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"space-bunny-free\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"my_tool\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":null}\n\n",
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"model\":\"space-bunny-free\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3,\"total_tokens\":13}}\n\n",
        "data: [DONE]\n\n",
        "data: {\"choices\":[],\"cost\":\"0\"}\n\n",
    );

    #[derive(Clone, Default)]
    struct Seen {
        headers: Arc<Mutex<Option<HeaderMap>>>,
    }

    async fn fake_gateway(State(seen): State<Seen>, headers: HeaderMap) -> Response {
        *seen.headers.lock().unwrap() = Some(headers);
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(SSE))
            .unwrap()
    }

    #[tokio::test]
    async fn shapes_the_request_and_collapses_the_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Seen::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(fake_gateway))
            .with_state(seen.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut provider = ProviderConfig::new(
            "opencode",
            "OpenCode Zen Free",
            ProviderKind::OpenCodeZenFree,
        );
        provider.base_url = format!("http://{addr}");
        assert_eq!(provider.client_profile, ProviderClientProfile::OpenCodeCli);

        let mut req = ChatRequest::new("space-bunny-free", provider.dialect());
        req.messages.push(Message::user_text("hi"));
        req.tools.push(crate::ir::ToolDef {
            name: "my_tool".into(),
            description: None,
            input_schema: json!({"type":"object"}),
            cache_control: None,
        });

        // The encoded, shaped body.
        let body = encode_for_mode(
            &provider,
            &req,
            "space-bunny-free",
            None,
            EncodeMode::Normal,
        )
        .expect("encodes");
        assert_eq!(body["stream"].as_bool(), Some(true));
        let names: Vec<&str> = body["tools"]
            .as_array()
            .expect("tools present")
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec!["my_tool", "bash", "read"],
            "V2 preserves the caller's tools and appends only the missing required names"
        );
        assert!(body.get("tool_choice").is_none());

        let identity = ClientIdentity {
            native_anthropic: false,
            omit_server_review: false,
            profile: ProviderClientProfile::OpenCodeCli,
            passthrough: Vec::new(),
            session: Some("ses_0123456789abcdefghijklmnop".into()),
            org_id: Some("wrk_test".into()),
            reply_headers: Default::default(),
            observation: Default::default(),
        };
        let headers = build_headers_for_identity(&provider, Some("public"), &identity)
            .expect("headers build");
        let h = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
        };
        assert!(
            h("user-agent").starts_with("opencode/"),
            "ua={}",
            h("user-agent")
        );
        assert_eq!(h("x-opencode-client"), "opencode");
        assert_eq!(h("x-opencode-project"), "global");
        assert_eq!(h("x-opencode-session"), "ses_0123456789abcdefghijklmnop");
        assert!(h("x-opencode-request").starts_with("msg_"));
        assert_eq!(h("x-opencode-org-id"), "wrk_test");

        // A forced-stream reply, folded back for a non-streaming client.
        let upstream = Upstream::new(true, 10);
        let mut stream = upstream
            .stream_with_identity(
                &provider,
                Some("public"),
                &body,
                "space-bunny-free",
                &identity,
            )
            .await
            .expect("handshake");
        let mut collector = StreamCollector::default();
        while let Some(item) = stream.next().await {
            collector.push(&item.expect("event"));
        }
        let resp = collector.finish();
        assert_eq!(resp.text(), "pong");
        assert_eq!(resp.usage.output_tokens, 3);
        assert!(
            resp.content.iter().any(
                |block| matches!(block, ContentBlock::ToolUse { name, .. } if name == "my_tool")
            ),
            "the model's own tool call survives the fold"
        );

        // The gateway saw the locked fingerprint and the session.
        let seen = seen
            .headers
            .lock()
            .unwrap()
            .clone()
            .expect("the gateway recorded a request");
        assert_eq!(
            seen.get("x-opencode-session").unwrap().to_str().unwrap(),
            "ses_0123456789abcdefghijklmnop"
        );
        assert_eq!(
            seen.get("x-opencode-client").unwrap().to_str().unwrap(),
            "opencode"
        );
    }

    /// Live smoke against the real gateway. Ignored: it needs the network and is
    /// subject to the free tier's per-IP rate limit.
    #[tokio::test]
    #[ignore = "live: hits https://opencode.ai/zen and needs network"]
    async fn live_anonymous_smoke() {
        let mut provider = ProviderConfig::new(
            "opencode",
            "OpenCode Zen Free",
            ProviderKind::OpenCodeZenFree,
        );
        // The anonymous lane. `ProviderKind::default_base_url` names the
        // token-authenticated lane instead, which `Bearer public` cannot use.
        provider.base_url = "https://opencode.ai/zen/v1".into();
        let mut req = ChatRequest::new("iceman-free", provider.dialect());
        req.messages
            .push(Message::user_text("Reply with the single word: pong"));
        let body = encode_for_mode(&provider, &req, "iceman-free", None, EncodeMode::Normal)
            .expect("encodes");
        let identity = ClientIdentity {
            native_anthropic: false,
            omit_server_review: false,
            profile: ProviderClientProfile::OpenCodeCli,
            passthrough: Vec::new(),
            session: Some(session_id()),
            org_id: None,
            reply_headers: Default::default(),
            observation: Default::default(),
        };
        let upstream = Upstream::new(false, 15);
        let mut stream = upstream
            .stream_with_identity(&provider, Some("public"), &body, "iceman-free", &identity)
            .await
            .expect("handshake");
        let mut collector = StreamCollector::default();
        while let Some(item) = stream.next().await {
            collector.push(&item.expect("event"));
        }
        assert!(
            !collector.finish().content.is_empty(),
            "expected a non-empty turn"
        );
    }
}
