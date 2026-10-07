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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
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

/// OpenCode's public capability catalog (models.dev shape).
const CATALOG_URL: &str = "https://models.opencode.ai/api.json";
/// How long a fetched catalog is reused before it is fetched again.
const CATALOG_TTL: Duration = Duration::from_secs(3600);

/// One model's capabilities, mapped from the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModel {
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    /// Served at zero cost on both input and output.
    pub free: bool,
    pub capabilities: crate::config::ModelCapabilities,
}

static CATALOG_CACHE: Mutex<Option<(Instant, HashMap<String, CatalogModel>)>> = Mutex::new(None);

/// The catalog, fetched once per [`CATALOG_TTL`] and cached in memory.
///
/// It is ~5 MB, so it is never fetched per request; the enrichment of a
/// discovered model list pays for it at most once an hour.
pub async fn catalog() -> Result<HashMap<String, CatalogModel>> {
    if let Some((at, cached)) = CATALOG_CACHE.lock().unwrap().as_ref() {
        if at.elapsed() < CATALOG_TTL {
            return Ok(cached.clone());
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::internal(format!("opencode catalog client: {e}")))?;
    let json: Value = client
        .get(CATALOG_URL)
        .send()
        .await
        .map_err(|source| Error::Transport {
            provider: PROVIDER_ID.into(),
            source,
        })?
        .json()
        .await
        .map_err(|e| Error::BadUpstreamPayload(format!("opencode catalog decode: {e}")))?;
    let parsed = parse_catalog(&json);
    *CATALOG_CACHE.lock().unwrap() = Some((Instant::now(), parsed.clone()));
    Ok(parsed)
}

/// Fill discovered models with the catalog's capabilities, windows and free
/// flag. Ids the catalog does not describe are left untouched.
pub async fn enrich(models: &mut [crate::upstream::DiscoveredModel]) -> Result<()> {
    let catalog = catalog().await?;
    for model in models {
        if let Some(entry) = catalog.get(&model.id) {
            model.context_window = entry.context_window;
            model.max_output = entry.max_output;
            model.free = entry.free;
            model.capabilities = Some(entry.capabilities.clone());
        }
    }
    Ok(())
}

/// Parse the `opencode.models` map out of the catalog document.
pub fn parse_catalog(json: &Value) -> HashMap<String, CatalogModel> {
    let Some(models) = json
        .get("opencode")
        .and_then(|provider| provider.get("models"))
        .and_then(Value::as_object)
    else {
        return HashMap::new();
    };
    models
        .iter()
        .map(|(id, entry)| (id.clone(), catalog_model(entry)))
        .collect()
}

pub(super) fn catalog_model(entry: &Value) -> CatalogModel {
    let limit = entry.get("limit");
    let limit_field = |name: &str| -> Option<u32> {
        limit
            .and_then(|limit| limit.get(name))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    };
    let input_modalities = entry
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array);
    let has_input = |name: &str| {
        input_modalities.is_some_and(|list| list.iter().any(|value| value.as_str() == Some(name)))
    };
    let flag = |name: &str| entry.get(name).and_then(Value::as_bool).unwrap_or(false);
    let cost = entry.get("cost");
    let zero_cost =
        |name: &str| cost.and_then(|cost| cost.get(name)).and_then(Value::as_f64) == Some(0.0);

    CatalogModel {
        context_window: limit_field("context"),
        max_output: limit_field("output"),
        free: zero_cost("input") && zero_cost("output"),
        capabilities: crate::config::ModelCapabilities {
            vision: has_input("image"),
            video: has_input("video"),
            audio: has_input("audio"),
            files: flag("attachment"),
            tools: flag("tool_call"),
            thinking: flag("reasoning"),
            structured_output: flag("structured_output"),
        },
    }
}

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
            org_id: None,
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
        assert_eq!(
            body["stream_options"]["include_usage"].as_bool(),
            Some(true)
        );
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

        let mut req = ChatRequest::new("space-bunny-free", provider.kind.dialect());
        req.messages.push(Message::user_text("hi"));

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
        assert!(names.contains(&"bash") && names.contains(&"read"));
        assert_eq!(body["tool_choice"].as_str(), Some("none"));

        let identity = ClientIdentity {
            profile: ProviderClientProfile::OpenCodeCli,
            passthrough: Vec::new(),
            session: Some("ses_0123456789abcdefghijklmnop".into()),
            org_id: Some("wrk_test".into()),
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
        assert_eq!(h("x-opencode-client"), "cli");
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
            "cli"
        );
    }

    /// Live smoke against the real gateway. Ignored: it needs the network and is
    /// subject to the free tier's per-IP rate limit.
    #[tokio::test]
    #[ignore = "live: hits https://opencode.ai/zen and needs network"]
    async fn live_anonymous_smoke() {
        let provider = ProviderConfig::new(
            "opencode",
            "OpenCode Zen Free",
            ProviderKind::OpenCodeZenFree,
        );
        let mut req = ChatRequest::new("space-bunny-free", provider.kind.dialect());
        req.messages
            .push(Message::user_text("Reply with the single word: pong"));
        let body = encode_for_mode(
            &provider,
            &req,
            "space-bunny-free",
            None,
            EncodeMode::Normal,
        )
        .expect("encodes");
        let identity = ClientIdentity {
            profile: ProviderClientProfile::OpenCodeCli,
            passthrough: Vec::new(),
            session: Some(session_id()),
            org_id: None,
        };
        let upstream = Upstream::new(false, 15);
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
        assert!(
            !collector.finish().content.is_empty(),
            "expected a non-empty turn"
        );
    }
}
