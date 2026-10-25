//! Choosing how faithfully a request is shaped for the provider.

use serde_json::{json, Value};

use super::headers::CLAUDE_CODE_SYSTEM_IDENTITY;
use crate::config::{ProviderClientProfile, ProviderConfig};
use crate::error::Result;
use crate::ir::ChatRequest;
use crate::protocol::{self, ProviderQuirks};
/// How faithfully a request must be encoded upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeMode {
    /// Normal chat traffic: provider quirks may drop fields for compatibility.
    Normal,
    /// Classifier side queries. The stop sequences, temperature and top_p are
    /// part of the judging protocol, not preferences: `drop_*` quirks that
    /// would silently strip them are overridden so the verdict contract
    /// (`<block>…</block>` terminated by the stop sequence) reaches the model
    /// intact.
    Classifier,
}

impl EncodeMode {
    /// The quirks that may be dropped in this mode.
    fn apply(self, quirks: &ProviderQuirks) -> ProviderQuirks {
        match self {
            EncodeMode::Normal => quirks.clone(),
            EncodeMode::Classifier => ProviderQuirks {
                drop_temperature: false,
                drop_top_p: false,
                drop_stop: false,
                ..quirks.clone()
            },
        }
    }
}

/// Prepare the upstream body for a candidate.
pub fn encode_for(
    provider: &ProviderConfig,
    req: &ChatRequest,
    upstream_model: &str,
    max_output_tokens: Option<u32>,
) -> Result<Value> {
    encode_for_mode(
        provider,
        req,
        upstream_model,
        max_output_tokens,
        EncodeMode::Normal,
    )
}

/// Prepare the upstream body for a candidate, with an explicit fidelity mode.
pub fn encode_for_mode(
    provider: &ProviderConfig,
    req: &ChatRequest,
    upstream_model: &str,
    max_output_tokens: Option<u32>,
    mode: EncodeMode,
) -> Result<Value> {
    let mut req = req.clone();
    if let Some(cap) = max_output_tokens {
        req.max_tokens = Some(req.max_tokens.map(|m| m.min(cap)).unwrap_or(cap));
    }
    // The provider's configured version header decides the Anthropic wire
    // shape: 2026-10-21 strips the sampling trio and confines documents, and
    // the same value rides the anthropic-version header so the upstream knows
    // which contract this request claims to honour.
    let anthropic_version =
        crate::protocol::anthropic::AnthropicApiVersion::of(provider.anthropic_version.as_deref());
    let mut body = protocol::encode_request(
        provider.dialect(),
        &req,
        upstream_model,
        &mode.apply(&provider.quirks),
        anthropic_version,
    )?;

    // Claude Code profile: inject identity line as the first system block
    // to pass Anthropic subscription/OAuth plan checks.
    if provider.client_profile_for(req.client_identity.as_deref())
        == ProviderClientProfile::ClaudeCode
    {
        prepend_claude_code_system_prompt(&mut body);
    }

    // OpenCode gateway: the free tier only serves a shaped request (streaming,
    // with `bash`/`read` tools). The client profile is the shaping carrier, so a
    // provider that selects it gets the shaping regardless of its `kind`.
    #[cfg(feature = "opencode")]
    if provider.client_profile_for(req.client_identity.as_deref())
        == ProviderClientProfile::OpenCodeCli
    {
        crate::opencode::shape_v2_body(&mut body);
    }

    Ok(body)
}

/// Insert the Claude Code identity as the first line of the `system` field in
/// the Anthropic request body.
///
/// Anthropic subscription/OAuth plans require the first system block to be exactly
/// this identity line.
fn prepend_claude_code_system_prompt(body: &mut Value) {
    // Only inject into Anthropic-style bodies. OpenAI-compatible bodies use
    // messages for system instructions, and injecting a "system" key triggers
    // gateway content filters.
    if !body.get("system").is_some() {
        body["system"] = Value::Array(vec![
            json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM_IDENTITY }),
        ]);
        return;
    }

    let identity = json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM_IDENTITY });
    let mut blocks: Vec<Value> = vec![identity];

    match body.get("system") {
        Some(Value::Array(existing)) => {
            // Idempotent: skip re-injection if the first block is already the identity line.
            if existing
                .first()
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
                == Some(CLAUDE_CODE_SYSTEM_IDENTITY)
            {
                return;
            }
            blocks.extend(existing.iter().cloned());
        }
        Some(Value::String(existing)) if !existing.is_empty() => {
            blocks.push(json!({ "type": "text", "text": existing }));
        }
        _ => {}
    }
    body["system"] = Value::Array(blocks);
}
