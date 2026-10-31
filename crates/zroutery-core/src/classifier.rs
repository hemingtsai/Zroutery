//! Detecting Auto Mode classifier requests on the wire.
//!
//! Claude Code issues its permission-classifier queries through the same
//! `/v1/messages` endpoint and often with the same model id as the main
//! conversation, so the classifier can only be told apart by its *shape*: a
//! deterministic temperature, a tiny `max_tokens`, a stop sequence, and a
//! distinctive system prompt. None of those is stable across Claude Code
//! versions — the prompt and parameters change with rollouts — which is why
//! detection is scored rather than hard-coded, and why the signatures can be
//! extended from the configuration file without a rebuild.
//!
//! Three layers, in order of trust:
//!
//! 1. An explicit `anthropic-beta` marker, should one ever be sent. Fast and
//!    certain, but it does not exist today, so it is only a fast path.
//! 2. Built-in structural signatures of the known classifier stages.
//! 3. User-configured fingerprints, which outlive any particular Claude Code
//!    version.
//!
//! A score has to reach the configured confidence before a request is treated
//! as the classifier, because the cost of a false positive is high: a normal
//! request would be routed away from the model the user asked for.

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::query::RequestKind;

/// What one detection pass concluded, with the evidence attached so logs and
/// tests can say *why* a request was classified.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub kind: RequestKind,
    /// 0.0 to 1.0. Only meaningful when `kind` is a side query; always 0.0 for
    /// a main request.
    pub confidence: f64,
    /// Which signature matched, when one did.
    pub matched: Option<String>,
    /// The human-readable reasons that contributed points, for DEBUG logging.
    pub reasons: Vec<&'static str>,
}

/// A structural fingerprint of one classifier stage.
///
/// Every field that is present must match; absent fields are not checked. The
/// prompt markers are substring tests over the concatenated system prompt,
/// which is how a prompt that grows a header or footer between versions still
/// matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierSignature {
    /// Name for logs and for saying which signature matched.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    /// Substrings that must all appear in the system prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_contains: Vec<String>,
}

impl ClassifierSignature {
    /// Points for how well `req` matches, in `[0, 1]`, plus which checks hit.
    ///
    /// A mismatch on any present field returns zero: the signature describes
    /// one stage exactly, and a request that disagrees on the parameters is a
    /// different kind of request, not a weaker match.
    fn score(&self, req: &Value, system_text: &str) -> (f64, Vec<&'static str>) {
        let mut reasons = Vec::new();
        if let Some(want) = self.max_tokens {
            let got = req
                .get("max_tokens")
                .or_else(|| req.get("max_output_tokens"))
                .and_then(Value::as_u64);
            if got != Some(u64::from(want)) {
                return (0.0, Vec::new());
            }
            reasons.push("max_tokens");
        }
        if let Some(want) = self.temperature {
            match req.get("temperature").and_then(Value::as_f64) {
                Some(got) if (got - want).abs() < 1e-9 => reasons.push("temperature"),
                _ => return (0.0, Vec::new()),
            }
        }
        if let Some(want) = &self.stop_sequence {
            let stops = req
                .get("stop_sequences")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            if !stops.contains(&want.as_str()) {
                return (0.0, Vec::new());
            }
            reasons.push("stop_sequence");
        }
        for marker in &self.system_contains {
            if !system_text.contains(marker.as_str()) {
                return (0.0, Vec::new());
            }
        }
        if !self.system_contains.is_empty() {
            reasons.push("system_prompt");
        }

        // A signature with more checks is stronger evidence: two parameter
        // matches alone are weaker than parameters plus the prompt marker.
        let weight = match (
            self.max_tokens.is_some(),
            self.temperature.is_some(),
            self.stop_sequence.is_some(),
            !self.system_contains.is_empty(),
        ) {
            (true, true, true, true) => 1.0,
            (true, true, false, true) => 0.95,
            (true, true, _, false) => 0.7,
            _ => 0.6,
        };
        (weight, reasons)
    }
}

/// Which built-in signatures to consult.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BuiltinDetectors {
    /// An explicit `anthropic-beta` classifier marker, when one exists.
    #[serde(default = "yes")]
    pub anthropic_beta: bool,
    /// The known Auto Mode stage signatures (parameters + prompt markers).
    #[serde(default = "yes")]
    pub xml_classifier_signature: bool,
    /// `[1m]`-style model modifiers, a weak positive signal on their own.
    #[serde(default = "yes")]
    pub model_1m_signature: bool,
    /// Codex's Guardian auto-review prompt.
    ///
    /// The Guardian request is a normal Responses request with a very
    /// distinctive developer message; there is no header or parameter that
    /// marks it. The prompt markers are long and specific enough to stand on
    /// their own, which is why this detector scores above the confidence
    /// threshold while a single parameter match cannot.
    #[serde(default = "yes")]
    pub codex_guardian_signature: bool,
}

impl Default for BuiltinDetectors {
    fn default() -> Self {
        BuiltinDetectors {
            anthropic_beta: true,
            xml_classifier_signature: true,
            model_1m_signature: true,
            codex_guardian_signature: true,
        }
    }
}

fn yes() -> bool {
    true
}

/// Detection settings, from the configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectionConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Score a request must reach to be classified. Defaults to 0.85, high
    /// enough that parameters alone never qualify: only a prompt marker or an
    /// explicit header can cross it.
    #[serde(default = "DetectionConfig::default_confidence")]
    pub minimum_confidence: f64,
    #[serde(default)]
    pub builtins: BuiltinDetectors,
    /// Fingerprints on top of the built-ins, so a new Claude Code version can
    /// be supported by editing the config instead of the code. Always
    /// serialized so the GUI round-trips the list.
    #[serde(default)]
    pub signatures: Vec<ClassifierSignature>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        DetectionConfig {
            enabled: true,
            minimum_confidence: Self::default_confidence(),
            builtins: BuiltinDetectors::default(),
            signatures: Vec::new(),
        }
    }
}

impl DetectionConfig {
    fn default_confidence() -> f64 {
        0.85
    }

    /// The built-in signatures of the known Auto Mode stages.
    ///
    /// Stage 1 is the cheap first pass: 64 tokens, frozen temperature, and a
    /// `</block>` stop sequence so the model never writes past the verdict.
    /// Stage 2 is the full analysis when stage 1 could not decide.
    fn builtin_signatures() -> Vec<ClassifierSignature> {
        vec![
            ClassifierSignature {
                name: "claude-auto-mode-stage-1".into(),
                max_tokens: Some(64),
                temperature: Some(0.0),
                stop_sequence: Some("</block>".into()),
                system_contains: vec!["security monitor for autonomous AI coding agents".into()],
            },
            ClassifierSignature {
                name: "claude-auto-mode-stage-2".into(),
                max_tokens: Some(4096),
                temperature: Some(0.0),
                stop_sequence: None,
                system_contains: vec!["security monitor for autonomous AI coding agents".into()],
            },
        ]
    }
}

/// The concatenated system/developer prompt of a request body.
///
/// Three shapes reach here, and the same words mean the same thing in each:
///
/// * Anthropic `system` — a string or a block array;
/// * Responses `instructions` — a string or a text-part array;
/// * Responses `input` items with role `system`/`developer` — the shape Codex's
///   Guardian review uses for its policy and output contract.
///
/// A detector that read only the first shape would see the Claude Code
/// classifier and miss every Codex review.
fn system_text(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |text: &str| {
        if !text.trim().is_empty() {
            parts.push(text.trim().to_string());
        }
    };
    match body.get("system") {
        Some(Value::String(s)) => push(s),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    push(text);
                }
            }
        }
        _ => {}
    }
    match body.get("instructions") {
        Some(Value::String(s)) => push(s),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    push(text);
                }
            }
        }
        _ => {}
    }
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            if !matches!(
                message.get("role").and_then(Value::as_str),
                Some("system") | Some("developer")
            ) {
                continue;
            }
            match message.get("content") {
                Some(Value::String(text)) => push(text),
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            push(text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(items) = body.get("input").and_then(Value::as_array) {
        for item in items {
            let role = item.get("role").and_then(Value::as_str);
            if !matches!(role, Some("system") | Some("developer")) {
                continue;
            }
            match item.get("content") {
                Some(Value::String(text)) => push(text),
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            push(text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    parts.join("\n")
}

/// Whether this request is one of Codex's Guardian review turns.
///
/// Guardian is a normal Responses request — same endpoint, same provider, no
/// marker header — whose developer message contains the review policy and the
/// JSON output contract. The phrases below are long and belong to that policy;
/// a client prompt that happened to contain one would be asking the same
/// question, which is the case this routing is for.
fn is_codex_guardian(body: &Value, system: &str) -> bool {
    let responses_shaped = body.get("input").is_some() || body.get("instructions").is_some();
    if !responses_shaped {
        return false;
    }
    let system = system.to_ascii_lowercase();
    [
        "first-step asynchronous security reviewer",
        "prior guardian decisions are context, not precedent",
        "you may use read-only tool checks to gather any additional context",
    ]
    .iter()
    .any(|marker| system.contains(marker))
}

/// Classify one inbound request.
///
/// `headers` and `body` are exactly what the client sent; nothing here mutates
/// either. A detection failure is never an error — the request is simply
/// treated as a main request, which is the routing it would have had anyway.
pub fn detect(headers: &HeaderMap, body: &Value, config: &DetectionConfig) -> Detection {
    if body.get("safeguards").is_some_and(|value| !value.is_null()) {
        // Embedded server review belongs to the main conversation. A beta
        // naming its classifier must not redirect this into the legacy paid
        // side-query pool and replace the model's answer with a verdict.
        return Detection {
            kind: RequestKind::Main,
            confidence: 0.0,
            matched: None,
            reasons: Vec::new(),
        };
    }
    if !config.enabled {
        return Detection {
            kind: RequestKind::Main,
            confidence: 0.0,
            matched: None,
            reasons: Vec::new(),
        };
    }

    let mut best: Option<(f64, String, Vec<&'static str>)> = None;
    let mut consider = |score: f64, name: &str, reasons: Vec<&'static str>| {
        if score > best.as_ref().map(|(s, _, _)| *s).unwrap_or(0.0) {
            best = Some((score, name.to_string(), reasons));
        }
    };

    // Level 1: an explicit beta header is a fast, certain path — when one
    // exists. Today none does, so this is future-proofing more than detection.
    if config.builtins.anthropic_beta {
        if let Some(beta) = headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase())
        {
            if beta.split(',').any(|flag| {
                flag.trim().contains("auto-mode-classifier")
                    || flag.trim().contains("auto_mode_classifier")
            }) {
                consider(1.0, "anthropic-beta", vec!["beta_header"]);
            }
        }
    }

    // Level 2a: Codex's Guardian review is a Responses request whose developer
    // message carries the review policy. It is short-circuited rather than
    // scored with the Anthropic signatures: it is a different protocol, and a
    // match that reached the shared scorer would be reported as Auto Mode and
    // answered with a `<block>` tag Codex cannot read.
    let system = system_text(body);
    if config.builtins.xml_classifier_signature {
        // Claude 2.1.281 defaults to temperature=1 and varies max_tokens with
        // the fast stage/thinking budget. Identify the specific monitor prompt
        // plus its verdict contract, not a model alias or a frozen parameter.
        let monitor = system.contains("security monitor for autonomous AI coding agents");
        let stop = body
            .get("stop_sequences")
            .and_then(Value::as_array)
            .is_some_and(|stops| {
                stops
                    .iter()
                    .any(|stop| matches!(stop.as_str(), Some("</block>" | "</severity>")))
            });
        let contract = system.contains("## Output Format")
            && (system.contains("<block>") || system.contains("<severity>"));
        if monitor && (stop || contract) {
            consider(
                0.98,
                "claude-auto-mode-current-contract",
                vec!["security_monitor_prompt", "verdict_contract"],
            );
        }
    }
    if config.builtins.codex_guardian_signature && is_codex_guardian(body, &system) {
        return Detection {
            kind: RequestKind::Side(crate::query::SideQueryKind::CodexGuardian),
            confidence: 0.97,
            matched: Some("codex-guardian".to_string()),
            reasons: vec!["guardian_prompt"],
        };
    }

    // Levels 2 and 3: structural signatures, built-in first, then the user's.
    if config.builtins.xml_classifier_signature {
        for signature in DetectionConfig::builtin_signatures() {
            let (score, reasons) = signature.score(body, &system);
            if score > 0.0 {
                consider(score, &signature.name, reasons);
            }
        }
    }
    for signature in &config.signatures {
        let (score, reasons) = signature.score(body, &system);
        if score > 0.0 {
            consider(score, &signature.name, reasons);
        }
    }

    // The model modifier is a weak positive: classifier requests do carry
    // `[1m]` in practice, but so can main requests, so it can only nudge an
    // existing match over the threshold, never carry one on its own.
    if config.builtins.model_1m_signature {
        if let Some(model) = body.get("model").and_then(Value::as_str) {
            let bare = crate::query::strip_client_model_modifier(model);
            if bare != model {
                if let Some((score, name, mut reasons)) = best {
                    reasons.push("model_modifier");
                    best = Some((score.min(0.99) + 0.01, name, reasons));
                }
            }
        }
    }

    match best {
        Some((score, name, reasons)) if score >= config.minimum_confidence => Detection {
            kind: RequestKind::Side(crate::query::SideQueryKind::AutoMode),
            confidence: score,
            matched: Some(name),
            reasons,
        },
        _ => {
            let (confidence, _, reasons) = best.unwrap_or_default();
            Detection {
                kind: RequestKind::Main,
                confidence,
                matched: None,
                reasons,
            }
        }
    }
}

// ---------------------------------------------------------------- validation

/// What the upstream classifier model answered, parsed without interpretation.
///
/// Zroutery validates the *shape* of the verdict only. Deciding what the
/// verdict means — the Auto Mode rules, the stages, what counts as dangerous —
/// is Claude Code's job, and rewriting or second-guessing it here would make
/// the proxy a party to the approval decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierVerdict {
    /// `<block>no</block>` — the action may proceed.
    Allow,
    /// `<block>yes</block>` — the action must be blocked.
    Block,
    /// Anything else, including nothing at all.
    ///
    /// An unparseable answer is *not* an Allow: Auto Mode is fail-closed, so a
    /// classifier that cannot produce a verdict has not approved anything.
    Unparseable,
}

/// Read the verdict out of a classifier response's text content.
///
/// Only exact `<block>…</block>` content counts. A natural-language
/// approximation ("looks safe", "somewhat risky") must never be mapped onto a
/// verdict — that would make the output parser a safety decision-maker.
pub fn parse_verdict(text: &str) -> ClassifierVerdict {
    // The verdict may be surrounded by whitespace or other prose; scan for the
    // tag pair and require an exact yes/no between them.
    let lower = text.to_ascii_lowercase();
    for (start_tag, end_tag) in [
        ("<block>", "</block>"),
        ("<block yes>", "</block>"),
        ("<block no>", "</block>"),
    ] {
        let Some(start) = lower.find(start_tag) else {
            continue;
        };
        let after = &lower[start + start_tag.len()..];
        let Some(end) = after.find(end_tag) else {
            continue;
        };
        let inner = after[..end].trim();
        if inner == "no" {
            return ClassifierVerdict::Allow;
        }
        if inner == "yes" {
            return ClassifierVerdict::Block;
        }
    }
    ClassifierVerdict::Unparseable
}

/// The client can request the newer declared 0..100 severity contract. A binary
/// decision is represented only at its two extremes; this is not a fabricated
/// model severity/calibration score. Unknown confidence still cannot approve.
pub fn decision_text_for_request(
    req: &crate::ir::ChatRequest,
    kind: RequestKind,
    verdict: ClassifierVerdict,
) -> String {
    if matches!(
        kind,
        RequestKind::Side(crate::query::SideQueryKind::AutoMode)
    ) && (req.stop_sequences.iter().any(|stop| stop == "</severity>")
        || req
            .system
            .iter()
            .any(|part| part.text.contains("<severity>")))
    {
        return if verdict == ClassifierVerdict::Allow {
            "<severity>0</severity>"
        } else {
            "<severity>100</severity>"
        }
        .into();
    }
    verdict_text_for(kind, verdict)
}

pub fn verdict_shape_valid(req: &crate::ir::ChatRequest, kind: RequestKind, text: &str) -> bool {
    verdict_shape_valid_for_format(kind, uses_severity(req), text)
}

pub fn uses_severity(req: &crate::ir::ChatRequest) -> bool {
    req.stop_sequences.iter().any(|stop| stop == "</severity>")
        || req
            .system
            .iter()
            .any(|part| part.text.contains("<severity>"))
}

pub fn verdict_shape_valid_for_format(kind: RequestKind, severity: bool, text: &str) -> bool {
    if matches!(
        kind,
        RequestKind::Side(crate::query::SideQueryKind::AutoMode)
    ) && severity
    {
        let Some((_, after)) = text.split_once("<severity>") else {
            return false;
        };
        let Some((value, _)) = after.split_once("</severity>") else {
            return false;
        };
        // Only validate the wire contract. The native client's own configured
        // severity thresholds decide a chat model's verdict, not this gateway.
        return value.trim().parse::<u8>().is_ok_and(|value| value <= 100);
    }
    parse_verdict_for(kind, text) != ClassifierVerdict::Unparseable
}

/// The verdict a decision model's answer carries.
///
/// A decision model answers with a probability, and a probability is not a
/// verdict. This is where the two meet.
///
/// The Auto Mode asks a binary question — the action proceeds or it does not —
/// and a decision model answers that question with the probability of "yes".
/// The model's confidence in either direction is therefore `noul` on one side
/// and `1 - noul` on the other, and the threshold is the confidence it must
/// have before the answer is acted on at all.
///
/// **The Auto Mode is fail-closed, so an answer that is not confident has not
/// cleared the action.** A model that is 60% sure is not a model that has
/// approved anything, and the answer in the middle band blocks rather than
/// allowing. That is the same rule the text verdict follows: an answer that
/// cannot be read has not approved anything.
///
/// `None` — no answer at all, or one that is not a probability — is the
/// unparseable case, and it blocks.
pub fn verdict_from_decision(
    decision: Option<crate::upstream::Decision>,
    threshold: f64,
) -> ClassifierVerdict {
    let Some(decision) = decision else {
        return ClassifierVerdict::Unparseable;
    };
    let threshold = threshold.clamp(0.5, 1.0);
    if decision.noul >= threshold {
        // Confident that the action must be blocked.
        return ClassifierVerdict::Block;
    }
    if decision.noul <= 1.0 - threshold {
        // Confident that the action may proceed.
        return ClassifierVerdict::Allow;
    }
    // Neither confident enough to act on, and the gate is closed.
    ClassifierVerdict::Unparseable
}

/// Read the verdict out of a classifier response's text content, in the
/// protocol the side query asked in.
///
/// Auto Mode clients read `<block>yes|no</block>`; Codex's Guardian reads a
/// JSON object whose only required field is `outcome`. The pool is shared, so
/// the parser has to be told which question was asked — a `<block>` tag in a
/// Guardian answer, or JSON in an Auto Mode answer, is not a verdict.
pub fn parse_verdict_for(kind: RequestKind, text: &str) -> ClassifierVerdict {
    match kind {
        RequestKind::Side(crate::query::SideQueryKind::CodexGuardian) => {
            parse_guardian_verdict(text)
        }
        _ => parse_verdict(text),
    }
}

/// Read Guardian's JSON contract.
///
/// Codex accepts a JSON object surrounded by prose ("strict JSON" is the
/// instruction, not a guarantee), so the outermost braces are what is parsed.
/// Only the two contract outcomes count; anything else is unparseable and the
/// gate stays closed.
fn parse_guardian_verdict(text: &str) -> ClassifierVerdict {
    let Some(start) = text.find('{') else {
        return ClassifierVerdict::Unparseable;
    };
    let Some(end) = text.rfind('}') else {
        return ClassifierVerdict::Unparseable;
    };
    if end <= start {
        return ClassifierVerdict::Unparseable;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) else {
        return ClassifierVerdict::Unparseable;
    };
    match value.get("outcome").and_then(serde_json::Value::as_str) {
        Some("allow") => ClassifierVerdict::Allow,
        Some("deny") => ClassifierVerdict::Block,
        _ => ClassifierVerdict::Unparseable,
    }
}

/// The text a verdict is spoken in, for the protocol the side query used.
pub fn verdict_text_for(kind: RequestKind, verdict: ClassifierVerdict) -> String {
    match kind {
        RequestKind::Side(crate::query::SideQueryKind::CodexGuardian) => {
            guardian_verdict_json(verdict)
        }
        _ => verdict_text(verdict),
    }
}

/// Speak a decision model's probability in Guardian's JSON contract.
///
/// Only a decision model reaches this: a chat model in the review pool is given
/// Guardian's own prompt and its answer is passed through unchanged. The
/// contract requires `outcome` alone; the rationale is for the person who sees
/// the denial and explains why the proxy, not a prose model, decided.
fn guardian_verdict_json(verdict: ClassifierVerdict) -> String {
    match verdict {
        ClassifierVerdict::Allow => r#"{"outcome":"allow"}"#.to_string(),
        ClassifierVerdict::Block => {
            r#"{"outcome":"deny","rationale":"Zroutery's Auto review decision model blocked this action."}"#
                .to_string()
        }
        ClassifierVerdict::Unparseable => {
            r#"{"outcome":"deny","rationale":"Zroutery's Auto review decision model was not confident that this action may proceed."}"#
                .to_string()
        }
    }
}

/// The text a verdict is spoken in.
///
/// The client that asked the question expects the answer in its own protocol,
/// and the verdict is spoken back in it — so a decision model's probability
/// reaches the client in exactly the shape a language model's answer would
/// have reached it, and nothing downstream has to know which kind of model
/// answered.
pub fn verdict_text(verdict: ClassifierVerdict) -> String {
    match verdict {
        ClassifierVerdict::Allow => "<block>no</block>".to_string(),
        ClassifierVerdict::Block => "<block>yes</block>".to_string(),
        // An unparseable answer is not an allow, so it is spoken as a block.
        ClassifierVerdict::Unparseable => "<block>yes</block>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    fn detect_with(body: Value) -> Detection {
        detect(&HeaderMap::new(), &body, &DetectionConfig::default())
    }

    fn stage_1_body() -> Value {
        json!({
            "model": "claude-opus-4-8",
            "max_tokens": 64,
            "temperature": 0,
            "stop_sequences": ["</block>"],
            "system": [{"type": "text", "text":
                "You are a security monitor for autonomous AI coding agents."}],
            "messages": [{"role": "user", "content": "transcript"}]
        })
    }

    fn stage_2_body() -> Value {
        json!({
            "model": "claude-opus-4-8",
            "max_tokens": 4096,
            "temperature": 0,
            "system": [{"type": "text", "text":
                "You are a security monitor for autonomous AI coding agents. Analyse in depth."}],
            "messages": [{"role": "user", "content": "transcript"}]
        })
    }

    /// A plausible main request: same model family, but a real prompt and
    /// normal parameters.
    fn main_body() -> Value {
        json!({
            "model": "claude-opus-4-8",
            "max_tokens": 8192,
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "refactor this module"}]
        })
    }

    #[test]
    fn stage_1_signature_is_detected() {
        let d = detect_with(stage_1_body());
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
        assert!(d.confidence >= 0.85);
        assert_eq!(d.matched.as_deref(), Some("claude-auto-mode-stage-1"));
    }

    #[test]
    fn stage_2_signature_is_detected() {
        let d = detect_with(stage_2_body());
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
        assert_eq!(d.matched.as_deref(), Some("claude-auto-mode-stage-2"));
    }

    #[test]
    fn a_main_request_is_not_detected() {
        let d = detect_with(main_body());
        assert_eq!(d.kind, RequestKind::Main);
        assert!(d.matched.is_none());
    }

    #[test]
    fn parameters_alone_are_not_enough() {
        // The exact classifier parameters, but a completely different prompt:
        // matching on temperature and max_tokens alone would misroute every
        // terse deterministic request a user sends.
        let mut body = stage_1_body();
        body["system"] = json!("You are a translation engine.");
        let d = detect_with(body);
        assert_eq!(d.kind, RequestKind::Main);
        assert!(d.confidence < 0.85, "score was {}", d.confidence);
    }

    #[test]
    fn the_wrong_prompt_marker_is_not_enough() {
        // The prompt marker with different parameters is not the classifier
        // either — a signature is all-or-nothing on the fields it declares.
        let mut body = stage_1_body();
        body["max_tokens"] = json!(1024);
        body["stop_sequences"] = json!(["END"]);
        let d = detect_with(body);
        assert_eq!(d.kind, RequestKind::Main);
    }

    #[test]
    fn current_classifier_temperature_caps_and_severity_do_not_route_to_main() {
        let body = json!({"model":"standard-class","max_tokens":256,"temperature":1,
            "stop_sequences":["</severity>"],
            "system":"You are a security monitor for autonomous AI coding agents.\n## Output Format\nOutput <severity>N</severity> where N is an integer 0-100.",
            "messages":[{"role":"user","content":"fixture transcript"}]});
        assert_eq!(
            detect_with(body.clone()).kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
        let mut server_review = body;
        server_review["safeguards"] = json!({"opaque_fixture":true});
        assert_eq!(
            detect_with(server_review).kind,
            RequestKind::Main,
            "embedded server review is not a legacy side query"
        );
    }

    #[test]
    fn a_lookalike_request_with_64_tokens_and_zero_temperature_stays_main() {
        // The regression case from the design brief: an ordinary request that
        // happens to be small and deterministic must not be hijacked.
        let body = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 64,
            "temperature": 0,
            "messages": [{"role": "user", "content": "classify this sentiment"}]
        });
        let d = detect_with(body);
        assert_eq!(d.kind, RequestKind::Main);
    }

    #[test]
    fn the_1m_modifier_is_only_a_nudge() {
        // On its own it detects nothing...
        let mut body = main_body();
        body["model"] = json!("claude-opus-4-8[1m]");
        let d = detect_with(body);
        assert_eq!(d.kind, RequestKind::Main);

        // ...and it strengthens an already-strong match.
        let mut body = stage_1_body();
        body["model"] = json!("claude-opus-4-8[1m]");
        let d = detect_with(body);
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
        assert!(d.reasons.contains(&"model_modifier"));
    }

    #[test]
    fn explicit_beta_header_wins_immediately() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("claude-code-20250219,auto-mode-classifier-2026"),
        );
        // Any body, even a main-shaped one.
        let d = detect(&headers, &main_body(), &DetectionConfig::default());
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
        assert_eq!(d.matched.as_deref(), Some("anthropic-beta"));
        assert_eq!(d.confidence, 1.0);
    }

    #[test]
    fn unrelated_beta_headers_detect_nothing() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("claude-code-20250219,output-128k"),
        );
        let d = detect(&headers, &main_body(), &DetectionConfig::default());
        assert_eq!(d.kind, RequestKind::Main);
    }

    #[test]
    fn user_configured_signatures_extend_detection() {
        // A future Claude Code whose prompt changed: the user adds a
        // fingerprint instead of waiting for a release.
        let config = DetectionConfig {
            signatures: vec![ClassifierSignature {
                name: "custom-v2".into(),
                max_tokens: Some(128),
                temperature: Some(0.0),
                stop_sequence: Some("</verdict>".into()),
                system_contains: vec!["new prompt header".into()],
            }],
            ..DetectionConfig::default()
        };
        let body = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 128,
            "temperature": 0,
            "stop_sequences": ["</verdict>"],
            "system": "new prompt header, stage 1",
            "messages": []
        });
        let d = detect(&HeaderMap::new(), &body, &config);
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
        assert_eq!(d.matched.as_deref(), Some("custom-v2"));
    }

    #[test]
    fn detection_can_be_disabled() {
        let config = DetectionConfig {
            enabled: false,
            ..DetectionConfig::default()
        };
        let d = detect(&HeaderMap::new(), &stage_1_body(), &config);
        assert_eq!(d.kind, RequestKind::Main);
    }

    #[test]
    fn a_string_system_prompt_is_matched_too() {
        let mut body = stage_1_body();
        body["system"] =
            json!("You are a security monitor for autonomous AI coding agents. Answer in XML.");
        let d = detect_with(body);
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::AutoMode)
        );
    }

    #[test]
    fn verdicts() {
        assert_eq!(parse_verdict("<block>no</block>"), ClassifierVerdict::Allow);
        assert_eq!(
            parse_verdict("<block>yes</block>"),
            ClassifierVerdict::Block
        );
        // Case and surrounding prose are tolerated...
        assert_eq!(
            parse_verdict("Sure! <block>NO</block> done"),
            ClassifierVerdict::Allow
        );
        // ...but paraphrases never become verdicts.
        assert_eq!(
            parse_verdict("this looks safe to me"),
            ClassifierVerdict::Unparseable
        );
        assert_eq!(
            parse_verdict("<block>maybe</block>"),
            ClassifierVerdict::Unparseable
        );
        assert_eq!(parse_verdict(""), ClassifierVerdict::Unparseable);
        assert_eq!(parse_verdict("no"), ClassifierVerdict::Unparseable);
    }

    #[test]
    fn signature_config_round_trips() {
        let config = DetectionConfig {
            signatures: vec![ClassifierSignature {
                name: "s".into(),
                max_tokens: Some(64),
                temperature: Some(0.0),
                stop_sequence: Some("</block>".into()),
                system_contains: vec!["marker".into()],
            }],
            ..DetectionConfig::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: DetectionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
    }

    /// A Codex Guardian review: a Responses request whose developer message is
    /// the review policy, not a header or a parameter.
    fn guardian_body() -> Value {
        json!({
            "model": "gpt-5.1-codex-mini",
            "stream": true,
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text":
                    "You are acting as a first-step asynchronous security reviewer. A high-risk \
                     decision triggers blocking Guardian review on future actions."}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text":
                    ">>> TRANSCRIPT START\nrun rm -rf build"}]}
            ]
        })
    }

    #[test]
    fn codex_guardian_review_is_detected_from_its_policy() {
        let d = detect_with(guardian_body());
        assert_eq!(
            d.kind,
            RequestKind::Side(crate::query::SideQueryKind::CodexGuardian),
            "{d:?}"
        );
        assert!(d.confidence >= 0.85, "{d:?}");
        assert_eq!(d.matched.as_deref(), Some("codex-guardian"));
    }

    #[test]
    fn the_guardian_detector_can_be_turned_off() {
        let mut config = DetectionConfig::default();
        config.builtins.codex_guardian_signature = false;
        let d = detect(&HeaderMap::new(), &guardian_body(), &config);
        assert_eq!(d.kind, RequestKind::Main, "{d:?}");
    }

    #[test]
    fn a_guardian_answer_is_json_and_only_the_contract_outcomes_count() {
        let kind = RequestKind::Side(crate::query::SideQueryKind::CodexGuardian);
        assert_eq!(
            parse_verdict_for(kind, r#"{"outcome":"allow"}"#),
            ClassifierVerdict::Allow
        );
        assert_eq!(
            parse_verdict_for(kind, r#"{"outcome":"deny","rationale":"rm -rf"}"#),
            ClassifierVerdict::Block
        );
        // Codex tolerates prose around the object, so the outermost braces are
        // parsed.
        assert_eq!(
            parse_verdict_for(kind, "The verdict is:\n{\"outcome\":\"allow\"}\nThanks."),
            ClassifierVerdict::Allow
        );
        for text in [
            "",
            "allow",
            r#"{"risk_level":"low"}"#,
            r#"{"outcome":"maybe"}"#,
        ] {
            assert_eq!(
                parse_verdict_for(kind, text),
                ClassifierVerdict::Unparseable
            );
        }
        // The same text in the other protocol is not a verdict.
        assert_eq!(
            parse_verdict_for(
                RequestKind::Side(crate::query::SideQueryKind::AutoMode),
                r#"{"outcome":"allow"}"#
            ),
            ClassifierVerdict::Unparseable
        );
    }

    #[test]
    fn a_decision_model_speaks_guardian_json() {
        let kind = RequestKind::Side(crate::query::SideQueryKind::CodexGuardian);
        assert_eq!(
            verdict_text_for(kind, ClassifierVerdict::Allow),
            r#"{"outcome":"allow"}"#
        );
        let denied: Value =
            serde_json::from_str(&verdict_text_for(kind, ClassifierVerdict::Block)).unwrap();
        assert_eq!(denied["outcome"], "deny");
        assert!(denied["rationale"].is_string());
        // And the Auto Mode protocol still gets its tag.
        assert_eq!(
            verdict_text_for(
                RequestKind::Side(crate::query::SideQueryKind::AutoMode),
                ClassifierVerdict::Block
            ),
            "<block>yes</block>"
        );
    }
}
