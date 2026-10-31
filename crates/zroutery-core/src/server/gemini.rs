//! Gemini SDK native resource routes. The URL, not a body extension, owns the
//! model and streaming mode; all generation still enters the shared lifecycle.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::error::Error;
use crate::ir::Dialect;

use super::handlers::{error_response, unwrap_body, JsonBody};
use super::state::AppState;

fn native_body(
    state: &AppState,
    model: &str,
    stream: bool,
    body: JsonBody,
) -> Result<Value, Error> {
    let mut body = unwrap_body(state, body)?;
    if model.trim().is_empty() || model.len() > 512 {
        return Err(Error::invalid("Gemini URL model is empty or too long"));
    }
    let object = body
        .as_object_mut()
        .ok_or_else(|| Error::invalid("Gemini request body must be a JSON object"))?;
    object.insert("model".into(), json!(model));
    object.insert("stream".into(), json!(stream));
    Ok(body)
}

pub(super) async fn resource(
    State(state): State<Arc<AppState>>,
    Path(resource): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> Response {
    let Some((model, operation)) = resource.rsplit_once(':') else {
        return error_response(
            Dialect::Gemini,
            &Error::invalid("Gemini resource requires an operation"),
        );
    };
    if operation == "countTokens" {
        return count_tokens(state, model, body).await;
    }
    let stream = match operation {
        "generateContent" => false,
        "streamGenerateContent" => true,
        _ => {
            return error_response(
                Dialect::Gemini,
                &Error::UnknownRoute(format!("Gemini operation {operation}")),
            )
        }
    };
    match native_body(&state, model, stream, body) {
        Ok(body) => super::pipeline::handle_chat(state, Dialect::Gemini, headers, body).await,
        Err(error) => error_response(Dialect::Gemini, &error),
    }
}

async fn count_tokens(state: Arc<AppState>, model: &str, body: JsonBody) -> Response {
    let body = match unwrap_body(&state, body) {
        Ok(mut body) => {
            if body.get("generateContentRequest").is_some() && body.get("contents").is_some() {
                return error_response(
                    Dialect::Gemini,
                    &Error::invalid(
                        "send contents or generateContentRequest to countTokens, not both",
                    ),
                );
            }
            body.as_object_mut()
                .and_then(|object| object.remove("generateContentRequest"))
                .unwrap_or(body)
        }
        Err(error) => return error_response(Dialect::Gemini, &error),
    };
    let request = native_body(&state, model, false, Ok(Json(body)))
        .and_then(crate::protocol::gemini::decode_request);
    match request {
        Ok(request) => Json(json!({
            "totalTokens": request.estimate_tokens(),
            "zroutery": {"estimated": true},
        }))
        .into_response(),
        Err(error) => error_response(Dialect::Gemini, &error),
    }
}

pub(super) async fn list_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    let registry = state.registry();
    let models: Vec<_> = registry
        .list()
        .into_iter()
        .map(|model| {
            let id = if model.virtual_model {
                model
                    .tier
                    .map(|tier| tier.virtual_id().to_owned())
                    .unwrap_or(model.id)
            } else {
                model.id
            };
            let mut item = json!({
                "name": format!("models/{id}"), "displayName": model.display_name,
                "supportedGenerationMethods": ["generateContent", "countTokens"],
            });
            if let Some(limit) = model.context_window {
                item["inputTokenLimit"] = json!(limit);
            }
            item
        })
        .collect();
    Json(json!({"models": models}))
}
