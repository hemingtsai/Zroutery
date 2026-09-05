//! Canonical intermediate representation (IR) for chat requests, responses and
//! streaming events.
//!
//! Every inbound protocol (Anthropic Messages, OpenAI Chat Completions) is
//! decoded into this IR, and every upstream protocol is encoded from it. That
//! keeps the number of conversions at `2 decoders + 2 encoders` instead of a
//! full N x M matrix.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Which wire dialect a payload belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// Anthropic `/v1/messages`.
    Anthropic,
    /// OpenAI `/v1/chat/completions` (also DeepSeek and most "OpenAI compatible" APIs).
    OpenAI,
    /// OpenAI `/v1/responses`.
    OpenAIResponses,
    /// Google Gemini native API (`generateContent`).
    Gemini,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A single piece of message content.
///
/// Tool results live inside `Role::User` messages (Anthropic shape). The OpenAI
/// decoder converts `role: "tool"` messages into this form, and the OpenAI
/// encoder converts them back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text {
        text: String,
        /// Anthropic's `cache_control` marker, kept verbatim.
        ///
        /// Passed through rather than modelled: it is where a caller asks for a
        /// prompt cache breakpoint, and dropping it makes a caching client pay full
        /// price on every turn. Keeping the value as it arrived means a field the
        /// vendor adds later survives too, at the cost of not validating it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<Value>,
    },
    /// Extended thinking / reasoning text.
    Thinking {
        text: String,
        signature: Option<String>,
    },
    /// Opaque encrypted reasoning payload that must be echoed back verbatim.
    RedactedThinking {
        data: String,
    },
    Image {
        source: MediaSource,
    },
    Document {
        source: MediaSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        name: String,
        content: Vec<ToolResultPart>,
        is_error: bool,
    },
}

impl ContentBlock {
    pub fn text(t: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: t.into(),
            cache_control: None,
        }
    }

    /// A text block that ends a cacheable prefix.
    pub fn cached_text(t: impl Into<String>, cache_control: Value) -> Self {
        ContentBlock::Text {
            text: t.into(),
            cache_control: Some(cache_control),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolResultPart {
    Text { text: String },
    Image { source: MediaSource },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MediaSource {
    /// Inline data: `media_type` plus base64 payload.
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
}

impl MediaSource {
    /// Render as an OpenAI style `image_url.url` value.
    pub fn to_data_url(&self) -> String {
        match self {
            MediaSource::Base64 { media_type, data } => {
                format!("data:{media_type};base64,{data}")
            }
            MediaSource::Url { url } => url.clone(),
        }
    }

    /// Parse an OpenAI style url (either a plain URL or a `data:` URL).
    pub fn from_url(url: &str) -> Self {
        if let Some(rest) = url.strip_prefix("data:") {
            if let Some((meta, data)) = rest.split_once(",") {
                let media_type = meta.trim_end_matches(";base64").to_string();
                // Reject data: URLs with control characters in the media type:
                // newlines and null bytes can be used to smuggle content past
                // naive parsers and have no valid use in a MIME type.
                if media_type.contains('\n')
                    || media_type.contains('\r')
                    || media_type.contains('\0')
                {
                    return MediaSource::Url {
                        url: url.to_string(),
                    };
                }
                if meta.ends_with(";base64") {
                    return MediaSource::Base64 {
                        media_type: if media_type.is_empty() {
                            "application/octet-stream".into()
                        } else {
                            media_type
                        },
                        data: data.to_string(),
                    };
                }
            }
        }
        MediaSource::Url {
            url: url.to_string(),
        }
    }
}

/// One block of the system prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPart {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
}

impl SystemPart {
    pub fn new(text: impl Into<String>) -> Self {
        SystemPart {
            text: text.into(),
            cache_control: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(t: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::text(t)],
        }
    }
    pub fn assistant_text(t: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::text(t)],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    /// JSON schema for the tool input.
    pub input_schema: Value,
    /// `cache_control` on a tool definition, which is where agents most often put
    /// their breakpoint: the tool list is long and never changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolChoice {
    Auto,
    None,
    /// Model must call at least one tool.
    Any,
    Specific {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    pub enabled: bool,
    pub budget_tokens: Option<u32>,
}

/// A protocol independent chat request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Model id exactly as the client asked for it (may be a virtual id such as
    /// `sonnet-class`). Routing happens later.
    pub model: String,
    /// System prompt, as blocks so a cache marker on one of them survives.
    pub system: Vec<SystemPart>,
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub stop_sequences: Vec<String>,
    pub stream: bool,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    pub thinking: Option<ThinkingConfig>,
    pub metadata_user: Option<String>,
    /// Vendor specific fields we do not understand but pass through untouched.
    pub passthrough: Map<String, Value>,
    /// The dialect the request arrived in, so the response can be encoded back.
    pub source_dialect: Dialect,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, dialect: Dialect) -> Self {
        ChatRequest {
            model: model.into(),
            system: Vec::new(),
            messages: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            stream: false,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            metadata_user: None,
            passthrough: Map::new(),
            source_dialect: dialect,
        }
    }

    /// Very rough token estimate, used for `count_tokens` and for logging.
    pub fn estimate_tokens(&self) -> u32 {
        let mut chars = self
            .system
            .iter()
            .map(|s| s.text.chars().count())
            .sum::<usize>();
        for m in &self.messages {
            for b in &m.content {
                chars += match b {
                    ContentBlock::Text { text, .. } | ContentBlock::Thinking { text, .. } => {
                        text.chars().count()
                    }
                    ContentBlock::RedactedThinking { data } => data.chars().count() / 4,
                    ContentBlock::Image { .. } => 4000,
                    ContentBlock::Document { .. } => 4000,
                    ContentBlock::ToolUse { input, name, .. } => {
                        name.chars().count() + input.to_string().chars().count()
                    }
                    ContentBlock::ToolResult { content, .. } => content
                        .iter()
                        .map(|p| match p {
                            ToolResultPart::Text { text } => text.chars().count(),
                            ToolResultPart::Image { .. } => 4000,
                        })
                        .sum(),
                };
            }
        }
        for t in &self.tools {
            chars += t.name.chars().count()
                + t.description
                    .as_ref()
                    .map(|d| d.chars().count())
                    .unwrap_or(0)
                + t.input_schema.to_string().chars().count();
        }
        // ~3.4 chars per token averaged over mixed CJK/latin text, plus overhead.
        ((chars as f64 / 3.4).ceil() as u32) + 8 * self.messages.len() as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    Refusal,
    /// Upstream ended without telling us why.
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// A protocol independent, non streaming chat response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    /// Model id reported back to the client.
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

impl ChatResponse {
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Canonical streaming event.
///
/// Both upstream parsers emit these, and both egress encoders consume them.
/// `index` is the content block index, matching Anthropic semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamEvent {
    Start {
        id: String,
        model: String,
        usage: Usage,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ThinkingDelta {
        index: u32,
        text: String,
    },
    ThinkingSignature {
        index: u32,
        signature: String,
    },
    RedactedThinking {
        index: u32,
        data: String,
    },
    ToolUseStart {
        index: u32,
        id: String,
        name: String,
    },
    ToolUseDelta {
        index: u32,
        partial_json: String,
    },
    BlockStop {
        index: u32,
    },
    Stop {
        stop_reason: StopReason,
        stop_sequence: Option<String>,
        usage: Usage,
    },
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_source_round_trip() {
        let s = MediaSource::Base64 {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        };
        let url = s.to_data_url();
        assert_eq!(url, "data:image/png;base64,AAAA");
        assert_eq!(MediaSource::from_url(&url), s);

        let plain = MediaSource::from_url("https://example.com/a.png");
        assert_eq!(
            plain,
            MediaSource::Url {
                url: "https://example.com/a.png".into()
            }
        );
    }

    #[test]
    fn token_estimate_is_monotonic() {
        let mut a = ChatRequest::new("m", Dialect::Anthropic);
        a.messages.push(Message::user_text("hello"));
        let mut b = a.clone();
        b.messages
            .push(Message::assistant_text("a much longer reply than before"));
        assert!(b.estimate_tokens() > a.estimate_tokens());
    }
}
