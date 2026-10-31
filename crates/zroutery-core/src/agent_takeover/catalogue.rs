//! Owned client catalogue generated from the same class contract as the gateway.
//! Codex format pinned to rust-v0.155.1 protocol/openai_models.rs; no token/body.

use serde_json::{json, Value};

use super::TierClass;

pub const CLIENT_CATALOGUE_VERSION: u32 = 1;

pub fn catalogue_fingerprint(classes: &[TierClass]) -> String {
    let bytes = serde_json::to_vec(classes).expect("class metadata is serializable");
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{hash:016x}")
}

pub fn codex_catalogue(classes: &[TierClass]) -> Value {
    json!({"models": classes.iter().enumerate().map(|(index, class)| {
        let effort = class.effort_levels.iter().find(|level| level.as_str() == "medium").or_else(|| class.effort_levels.first());
        json!({
            "slug": class.model, "display_name": class.display_name,
            "description": "Zroutery candidate pool. Usage tier and reasoning effort are independent; verify limitations in Zroutery.",
            "default_reasoning_level": effort,
            "supported_reasoning_levels": class.effort_levels.iter().map(|effort| json!({"effort":effort,"description":effort})).collect::<Vec<_>>(),
            "shell_type": if class.capabilities.tools { "unified_exec" } else { "disabled" },
            "visibility":"list", "supported_in_api":true, "priority":index,
            "availability_nux":null, "upgrade":null,
            "base_instructions":"You are a coding assistant. Follow the user's task and the client's tool and safety instructions.",
            "supports_reasoning_summary_parameter":false,
            "support_verbosity":false, "default_verbosity":null, "apply_patch_tool_type":null,
            "truncation_policy":{"mode":"bytes","limit":10000},
            "context_window":class.context_window, "max_context_window":class.context_window,
            "experimental_supported_tools":[],
            "input_modalities":if class.capabilities.vision { vec!["text","image"] } else { vec!["text"] },
            "supports_search_tool":false,
            "tool_mode":"direct"
        })
    }).collect::<Vec<_>>()})
}
