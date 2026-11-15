use serde_json::json;

use zroutery_core::ir::{Capability, ChatRequest, Dialect, StructuredOutput};
use zroutery_core::protocol::anthropic;
use zroutery_core::protocol::ProviderQuirks;
use zroutery_core::protocol::{decode_request, encode_request};

// ========================================================================
// 7. Structured output: IR modelling and explicit cross-dialect translation
// ========================================================================

fn structured_answer_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    })
}

fn openai_structured_request() -> serde_json::Value {
    json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "answer as json"}],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "strict": true,
                "schema": structured_answer_schema()
            }
        }
    })
}

fn responses_structured_request() -> serde_json::Value {
    json!({
        "model": "gpt-5",
        "input": "answer as json",
        "text": {
            "format": {
                "type": "json_schema",
                "name": "answer",
                "strict": true,
                "schema": structured_answer_schema()
            }
        }
    })
}

fn encode_for_target(dialect: Dialect, req: &ChatRequest) -> serde_json::Value {
    encode_request(
        dialect,
        req,
        "upstream-model",
        &ProviderQuirks::default(),
        anthropic::AnthropicApiVersion::Legacy,
    )
    .unwrap()
}

#[test]
fn openai_json_schema_decodes_into_ir_and_requires_capability() {
    let req = decode_request(Dialect::OpenAI, openai_structured_request()).unwrap();

    assert_eq!(
        req.structured_output,
        Some(StructuredOutput::JsonSchema {
            name: Some("answer".into()),
            schema: structured_answer_schema(),
            strict: true,
        })
    );
    assert_eq!(
        req.required_capabilities,
        vec![Capability::StructuredOutput]
    );
    assert_eq!(
        req.compute_required_capabilities(),
        req.required_capabilities
    );
    // The constraint is modelled, not left riding along as a vendor blob.
    assert!(req.passthrough.get("response_format").is_none());
}

#[test]
fn openai_json_schema_survives_same_dialect_re_encoding() {
    let req = decode_request(Dialect::OpenAI, openai_structured_request()).unwrap();
    let body = encode_for_target(Dialect::OpenAI, &req);

    assert_eq!(
        body["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "answer",
                "strict": true,
                "schema": structured_answer_schema()
            }
        })
    );
}

#[test]
fn openai_json_object_mode_round_trips() {
    let req = decode_request(
        Dialect::OpenAI,
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "json please"}],
            "response_format": {"type": "json_object"}
        }),
    )
    .unwrap();
    assert_eq!(req.structured_output, Some(StructuredOutput::JsonObject));
    assert_eq!(
        req.required_capabilities,
        vec![Capability::StructuredOutput]
    );

    let body = encode_for_target(Dialect::OpenAI, &req);
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
}

#[test]
fn openai_default_text_format_is_not_a_constraint() {
    let req = decode_request(
        Dialect::OpenAI,
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {"type": "text"}
        }),
    )
    .unwrap();
    assert_eq!(req.structured_output, None);
    assert!(req.required_capabilities.is_empty());
    assert!(encode_for_target(Dialect::OpenAI, &req)
        .get("response_format")
        .is_none());
}

#[test]
fn responses_text_format_decodes_into_ir_and_requires_capability() {
    let req = decode_request(Dialect::OpenAIResponses, responses_structured_request()).unwrap();

    assert_eq!(
        req.structured_output,
        Some(StructuredOutput::JsonSchema {
            name: Some("answer".into()),
            schema: structured_answer_schema(),
            strict: true,
        })
    );
    assert_eq!(
        req.required_capabilities,
        vec![Capability::StructuredOutput]
    );
}

#[test]
fn responses_json_schema_survives_same_dialect_re_encoding() {
    let req = decode_request(Dialect::OpenAIResponses, responses_structured_request()).unwrap();
    let body = encode_for_target(Dialect::OpenAIResponses, &req);

    // Responses puts the schema fields directly on the format object.
    assert_eq!(
        body["text"]["format"],
        json!({
            "type": "json_schema",
            "name": "answer",
            "strict": true,
            "schema": structured_answer_schema()
        })
    );
    assert!(body["text"]["format"].get("json_schema").is_none());
}

#[test]
fn responses_json_object_mode_round_trips() {
    let req = decode_request(
        Dialect::OpenAIResponses,
        json!({
            "model": "gpt-5",
            "input": "json please",
            "text": {"format": {"type": "json_object"}}
        }),
    )
    .unwrap();
    assert_eq!(req.structured_output, Some(StructuredOutput::JsonObject));

    let body = encode_for_target(Dialect::OpenAIResponses, &req);
    assert_eq!(body["text"]["format"], json!({"type": "json_object"}));
}

#[test]
fn openai_json_schema_translates_to_responses() {
    let req = decode_request(Dialect::OpenAI, openai_structured_request()).unwrap();
    let body = encode_for_target(Dialect::OpenAIResponses, &req);

    assert_eq!(
        body["text"]["format"],
        json!({
            "type": "json_schema",
            "name": "answer",
            "strict": true,
            "schema": structured_answer_schema()
        })
    );
}

#[test]
fn openai_json_schema_translates_to_gemini_with_gemini_schema_types() {
    let req = decode_request(Dialect::OpenAI, openai_structured_request()).unwrap();
    let body = encode_for_target(Dialect::Gemini, &req);

    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        body["generationConfig"]["responseSchema"],
        json!({
            "type": "OBJECT",
            "properties": {"answer": {"type": "STRING"}},
            "required": ["answer"],
            "additionalProperties": false
        })
    );
}

#[test]
fn openai_json_object_translates_to_gemini_and_responses() {
    let req = decode_request(
        Dialect::OpenAI,
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "json please"}],
            "response_format": {"type": "json_object"}
        }),
    )
    .unwrap();

    let gemini = encode_for_target(Dialect::Gemini, &req);
    assert_eq!(
        gemini["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert!(gemini["generationConfig"].get("responseSchema").is_none());

    let responses = encode_for_target(Dialect::OpenAIResponses, &req);
    assert_eq!(responses["text"]["format"], json!({"type": "json_object"}));
}

#[test]
fn gemini_json_schema_translates_back_to_openai_and_responses() {
    let req = decode_request(
        Dialect::Gemini,
        json!({
            "model": "gemini-2.5-pro",
            "contents": [{"role": "user", "parts": [{"text": "answer as json"}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": {"answer": {"type": "STRING"}},
                    "required": ["answer"],
                    "additionalProperties": false
                }
            }
        }),
    )
    .unwrap();

    // Gemini carries no schema name, so the target that requires one uses the
    // documented default instead of dropping the constraint.
    let openai = encode_for_target(Dialect::OpenAI, &req);
    assert_eq!(
        openai["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "response",
                "strict": false,
                "schema": structured_answer_schema()
            }
        })
    );

    let responses = encode_for_target(Dialect::OpenAIResponses, &req);
    assert_eq!(
        responses["text"]["format"],
        json!({
            "type": "json_schema",
            "name": "response",
            "strict": false,
            "schema": structured_answer_schema()
        })
    );
}

#[test]
fn anthropic_encoder_keeps_the_json_schema_constraint() {
    let req = decode_request(Dialect::OpenAI, openai_structured_request()).unwrap();
    let body = encode_request(
        Dialect::Anthropic,
        &req,
        "claude-sonnet",
        &ProviderQuirks::default(),
        anthropic::AnthropicApiVersion::Legacy,
    )
    .unwrap();
    assert_eq!(
        body["output_config"]["format"],
        json!({
            "type":"json_schema", "schema":structured_answer_schema()
        })
    );
    let decoded = decode_request(Dialect::Anthropic, body).unwrap();
    assert_eq!(
        decoded.structured_output,
        Some(StructuredOutput::JsonSchema {
            name: None,
            schema: structured_answer_schema(),
            strict: true
        })
    );
    assert_eq!(
        decoded.required_capabilities,
        vec![Capability::StructuredOutput]
    );
}

#[test]
fn anthropic_encoder_represents_json_object_mode_with_an_open_object_schema() {
    let req = decode_request(
        Dialect::OpenAI,
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "json please"}],
            "response_format": {"type": "json_object"}
        }),
    )
    .unwrap();
    let body = encode_request(
        Dialect::Anthropic,
        &req,
        "claude-sonnet",
        &ProviderQuirks::default(),
        anthropic::AnthropicApiVersion::Legacy,
    )
    .unwrap();
    assert_eq!(
        body["output_config"]["format"],
        json!({
            "type":"json_schema", "schema":{"type":"object","additionalProperties":true}
        })
    );
    let decoded = decode_request(Dialect::Anthropic, body).unwrap();
    assert_eq!(
        decoded.structured_output,
        Some(StructuredOutput::JsonSchema {
            name: None,
            schema: json!({"type":"object","additionalProperties":true}),
            strict: true
        })
    );
    assert_eq!(
        decoded.required_capabilities,
        vec![Capability::StructuredOutput]
    );
}
