//! Canonical intermediate representation (IR) for chat requests, responses and
//! streaming events.
//!
//! Every inbound protocol (Anthropic Messages, OpenAI Chat Completions) is
//! decoded into this IR, and every upstream protocol is encoded from it. That
//! keeps the number of conversions at `2 decoders + 2 encoders` instead of a
//! full N x M matrix.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A capability a model may support and a request may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Vision,
    Audio,
    Video,
    Files,
    Tools,
    Thinking,
    StructuredOutput,
}

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
    /// Generic file attachment (PDF, CSV, etc).
    File {
        source: MediaSource,
        /// MIME type, e.g. "application/pdf".
        media_type: String,
        /// Optional filename.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Audio content.
    Audio {
        source: MediaSource,
        /// MIME type, e.g. "audio/mp3".
        media_type: String,
    },
    /// Video content.
    Video {
        source: MediaSource,
        /// MIME type, e.g. "video/mp4".
        media_type: String,
    },
    /// A citation from a source document.
    Citation {
        /// The cited text.
        text: String,
        /// Source reference (URL, document id, etc).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        /// Title of the cited source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// An annotation on content (e.g. footnotes, references).
    Annotation {
        /// Annotation type (e.g. "footnote", "reference", "highlight").
        annotation_type: String,
        /// The annotation text/value.
        text: String,
        /// Start offset in the parent content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<u32>,
        /// End offset in the parent content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<u32>,
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

/// What to do with content types the target provider cannot represent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedContentPolicy {
    /// Return an error to the client (safest default).
    Reject,
    /// Try to convert (e.g. URL image → base64 download).
    Transform,
    /// Replace with a placeholder text.
    Placeholder,
    /// Silently remove.
    #[default]
    Drop,
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
    /// Policy for content types the target provider cannot handle.
    #[serde(default)]
    pub unsupported_content_policy: UnsupportedContentPolicy,
    /// Capabilities the target model must have to handle this request.
    /// Populated by the protocol decoder based on the content types present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<Capability>,
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
            unsupported_content_policy: UnsupportedContentPolicy::Reject,
            required_capabilities: Vec::new(),
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
                    ContentBlock::File { name, .. } => {
                        name.as_ref().map(|n| n.chars().count()).unwrap_or(0) + 4000
                    }
                    ContentBlock::Audio { .. } => 4000,
                    ContentBlock::Video { .. } => 4000,
                    ContentBlock::Citation { text, .. } => text.chars().count(),
                    ContentBlock::Annotation { text, .. } => text.chars().count(),
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

    /// Compute which capabilities this request requires based on its content.
    pub fn compute_required_capabilities(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        for msg in &self.messages {
            for block in &msg.content {
                match block {
                    ContentBlock::Image { .. } => {
                        if !caps.contains(&Capability::Vision) {
                            caps.push(Capability::Vision);
                        }
                    }
                    ContentBlock::Audio { .. } => {
                        if !caps.contains(&Capability::Audio) {
                            caps.push(Capability::Audio);
                        }
                    }
                    ContentBlock::Video { .. } => {
                        if !caps.contains(&Capability::Video) {
                            caps.push(Capability::Video);
                        }
                    }
                    ContentBlock::File { .. } => {
                        if !caps.contains(&Capability::Files) {
                            caps.push(Capability::Files);
                        }
                    }
                    ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => {
                        if !caps.contains(&Capability::Tools) {
                            caps.push(Capability::Tools);
                        }
                    }
                    ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                        if !caps.contains(&Capability::Thinking) {
                            caps.push(Capability::Thinking);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !self.tools.is_empty() && !caps.contains(&Capability::Tools) {
            caps.push(Capability::Tools);
        }
        if self.thinking.as_ref().is_some_and(|t| t.enabled) {
            if !caps.contains(&Capability::Thinking) {
                caps.push(Capability::Thinking);
            }
        }
        caps
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

    #[test]
    fn content_block_file_round_trip() {
        let block = ContentBlock::File {
            source: MediaSource::Url {
                url: "https://example.com/doc.pdf".into(),
            },
            media_type: "application/pdf".into(),
            name: Some("report.pdf".into()),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn content_block_audio_round_trip() {
        let block = ContentBlock::Audio {
            source: MediaSource::Base64 {
                media_type: "audio/mp3".into(),
                data: "AAAA".into(),
            },
            media_type: "audio/mp3".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn content_block_video_round_trip() {
        let block = ContentBlock::Video {
            source: MediaSource::Url {
                url: "https://example.com/clip.mp4".into(),
            },
            media_type: "video/mp4".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn content_block_citation_round_trip() {
        let block = ContentBlock::Citation {
            text: "The quick brown fox".into(),
            source: Some("https://example.com".into()),
            title: Some("Example".into()),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn content_block_citation_optional_fields() {
        let json = r#"{"Citation":{"text":"minimal"}}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::Citation { text, source, title } => {
                assert_eq!(text, "minimal");
                assert!(source.is_none());
                assert!(title.is_none());
            }
            _ => panic!("expected Citation"),
        }
    }

    #[test]
    fn content_block_annotation_round_trip() {
        let block = ContentBlock::Annotation {
            annotation_type: "footnote".into(),
            text: "See reference [1]".into(),
            start: Some(10),
            end: Some(20),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn content_block_annotation_optional_offsets() {
        let json = r#"{"Annotation":{"annotation_type":"highlight","text":"important"}}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::Annotation {
                annotation_type,
                text,
                start,
                end,
            } => {
                assert_eq!(annotation_type, "highlight");
                assert_eq!(text, "important");
                assert!(start.is_none());
                assert!(end.is_none());
            }
            _ => panic!("expected Annotation"),
        }
    }

    #[test]
    fn compute_required_capabilities_vision() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                source: MediaSource::Url {
                    url: "https://example.com/img.png".into(),
                },
            }],
        });
        let caps = req.compute_required_capabilities();
        assert!(caps.contains(&Capability::Vision));
        assert!(!caps.contains(&Capability::Audio));
    }

    #[test]
    fn compute_required_capabilities_audio_video_files() {
        let mut req = ChatRequest::new("m", Dialect::OpenAI);
        req.messages.push(Message {
            role: Role::User,
            content: vec![
                ContentBlock::Audio {
                    source: MediaSource::Url {
                        url: "https://example.com/audio.mp3".into(),
                    },
                    media_type: "audio/mp3".into(),
                },
                ContentBlock::Video {
                    source: MediaSource::Url {
                        url: "https://example.com/video.mp4".into(),
                    },
                    media_type: "video/mp4".into(),
                },
                ContentBlock::File {
                    source: MediaSource::Url {
                        url: "https://example.com/doc.pdf".into(),
                    },
                    media_type: "application/pdf".into(),
                    name: None,
                },
            ],
        });
        let caps = req.compute_required_capabilities();
        assert!(caps.contains(&Capability::Audio));
        assert!(caps.contains(&Capability::Video));
        assert!(caps.contains(&Capability::Files));
        assert!(!caps.contains(&Capability::Vision));
    }

    #[test]
    fn compute_required_capabilities_tools() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message::user_text("hello"));
        req.tools.push(ToolDef {
            name: "search".into(),
            description: None,
            input_schema: serde_json::json!({}),
            cache_control: None,
        });
        let caps = req.compute_required_capabilities();
        assert!(caps.contains(&Capability::Tools));
    }

    #[test]
    fn compute_required_capabilities_thinking() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "reasoning...".into(),
                signature: None,
            }],
        });
        let caps = req.compute_required_capabilities();
        assert!(caps.contains(&Capability::Thinking));
    }

    #[test]
    fn compute_required_capabilities_thinking_config() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message::user_text("hello"));
        req.thinking = Some(ThinkingConfig {
            enabled: true,
            budget_tokens: Some(1024),
        });
        let caps = req.compute_required_capabilities();
        assert!(caps.contains(&Capability::Thinking));
    }

    #[test]
    fn unsupported_content_policy_defaults_to_reject() {
        let req = ChatRequest::new("m", Dialect::Anthropic);
        assert_eq!(
            req.unsupported_content_policy,
            UnsupportedContentPolicy::Reject
        );
    }

    #[test]
    fn unsupported_content_policy_serde_round_trip() {
        let policies = [
            UnsupportedContentPolicy::Reject,
            UnsupportedContentPolicy::Transform,
            UnsupportedContentPolicy::Placeholder,
            UnsupportedContentPolicy::Drop,
        ];
        for policy in &policies {
            let json = serde_json::to_string(policy).unwrap();
            let deserialized: UnsupportedContentPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(*policy, deserialized);
        }
    }

    #[test]
    fn estimate_tokens_handles_new_variants() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.messages.push(Message {
            role: Role::User,
            content: vec![
                ContentBlock::File {
                    source: MediaSource::Url {
                        url: "https://example.com/doc.pdf".into(),
                    },
                    media_type: "application/pdf".into(),
                    name: Some("report.pdf".into()),
                },
                ContentBlock::Audio {
                    source: MediaSource::Url {
                        url: "https://example.com/audio.mp3".into(),
                    },
                    media_type: "audio/mp3".into(),
                },
                ContentBlock::Video {
                    source: MediaSource::Url {
                        url: "https://example.com/video.mp4".into(),
                    },
                    media_type: "video/mp4".into(),
                },
                ContentBlock::Citation {
                    text: "A cited passage".into(),
                    source: None,
                    title: None,
                },
                ContentBlock::Annotation {
                    annotation_type: "footnote".into(),
                    text: "A footnote".into(),
                    start: None,
                    end: None,
                },
            ],
        });
        let tokens = req.estimate_tokens();
        // Just verify it doesn't panic and returns a reasonable value.
        assert!(tokens > 0);
    }

    #[test]
    fn required_capabilities_serde_default_empty() {
        let json = r#"{"model":"m","system":[],"messages":[],"stop_sequences":[],"stream":false,"tools":[],"passthrough":{},"source_dialect":"anthropic"}"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert!(req.required_capabilities.is_empty());
    }

    #[test]
    fn required_capabilities_serde_round_trip() {
        let mut req = ChatRequest::new("m", Dialect::Anthropic);
        req.required_capabilities = vec![Capability::Vision, Capability::Tools];
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.required_capabilities,
            vec![Capability::Vision, Capability::Tools]
        );
    }
}
