//! Native menu generation is configuration evidence, not a paid request test.
use serde_json::json;
use zroutery_core::agent_takeover::{
    codex_catalogue, takeover_fields, AgentType, GatewayTarget, TierClass,
};
use zroutery_core::config::ModelTier;

fn target() -> GatewayTarget {
    GatewayTarget {
        base_url: "http://127.0.0.1:8787".into(),
        token: "local-test-token".into(),
        classes: ModelTier::ALL
            .into_iter()
            .map(|tier| TierClass {
                model: tier.virtual_id().into(),
                display_name: format!("Zroutery · {}", tier.as_str()),
                effort_levels: vec!["low".into(), "high".into()],
                capabilities: zroutery_core::config::ModelCapabilities {
                    tools: true,
                    thinking: true,
                    ..Default::default()
                },
                context_window: Some(100_000),
                max_output_tokens: Some(8_000),
                capability_source: "fixture exact intersection".into(),
            })
            .collect(),
        default_class: Some("standard-class".into()),
        codex_catalogue_path: Some("C:/isolated/codex-models.json".into()),
    }
}

#[test]
fn claude_picker_and_family_defaults_use_real_stable_ids_and_not_native_capability_claims() {
    let target = target();
    let fields = takeover_fields(AgentType::Claude, &target, None).unwrap();
    let value = |path: &str| {
        fields
            .iter()
            .find(|field| field.path == path)
            .map(|field| &field.value)
            .unwrap()
    };
    assert_eq!(value("modelPicker")["options"].as_array().unwrap().len(), 4);
    assert_eq!(value("modelPicker")["replaceBuiltInOptions"], true);
    assert_eq!(value("env.ANTHROPIC_DEFAULT_HAIKU_MODEL"), "fast-class");
    assert_eq!(value("env.ANTHROPIC_DEFAULT_FABLE_MODEL"), "frontier-class");
    assert_eq!(value("model"), "standard-class");
    assert!(
        value("env.ANTHROPIC_MODEL").is_null(),
        "remove the override so subsequent /model selections can save defaults"
    );
    assert!(value("env.CLAUDE_CODE_SUBAGENT_MODEL").is_null());
    assert_eq!(
        value("env.ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"),
        "Zroutery · fast"
    );
    assert!(value("modelPicker")["options"]
        .as_array()
        .unwrap()
        .iter()
        .all(|option| option.get("behavesAs").is_none()));
}

#[test]
fn codex_catalogue_has_the_pinned_required_fields_without_invented_efforts() {
    let target = target();
    let catalogue = codex_catalogue(&target.classes);
    let models = catalogue["models"].as_array().unwrap();
    assert_eq!(models.len(), 4);
    for model in models {
        assert_eq!(
            model["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(model["input_modalities"], json!(["text"]));
        assert_eq!(model["context_window"], 100_000);
        for key in [
            "slug",
            "shell_type",
            "visibility",
            "supported_in_api",
            "support_verbosity",
            "truncation_policy",
            "base_instructions",
            "experimental_supported_tools",
        ] {
            assert!(model.get(key).is_some(), "Codex catalogue field {key}");
        }
    }
    let fields = takeover_fields(AgentType::Codex, &target, None).unwrap();
    assert_eq!(
        fields
            .iter()
            .find(|field| field.path == "model_catalog_json")
            .unwrap()
            .value,
        "C:/isolated/codex-models.json"
    );
}

#[test]
fn empty_effort_intersection_overwrites_stale_variants_with_an_empty_list() {
    let mut target = target();
    target.classes[0].effort_levels.clear();
    let fields = takeover_fields(
        AgentType::OpenCode,
        &target,
        Some(&json!({"provider":{"zroutery":{}}})),
    )
    .unwrap();
    let variants = fields
        .iter()
        .find(|field| field.path == "providers.zroutery.models.fast-class.variants")
        .unwrap();
    assert_eq!(
        variants.value,
        json!([]),
        "member capability shrink does not leave stale advertised max variants"
    );
    assert!(fields
        .iter()
        .any(|field| field.path == "providers.zroutery.models.fast-class.capabilities"));
}
