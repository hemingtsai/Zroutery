//! Published service names map to existing Rust codecs, not plugin-authored effects.
use crate::config::{ProviderConfig, ProviderKind};
use crate::error::{Error, Result};
use zroutery_plugin_api::provider::{HostOperationBinding, ProviderDescriptor};

pub fn services() -> Vec<&'static str> {
    // No Gemini upstream, generic rotation or private protocol claims merely
    // because an inbound codec or memory-only refresh implementation exists.
    vec![
        "host.protocol.openai_chat",
        "host.protocol.openai_responses",
        "host.protocol.anthropic",
        "host.auth.bearer",
        "host.auth.anthropic",
        "host.catalog.openai",
        "host.catalog.anthropic",
        "host.client.claude_code",
        "host.client.codex",
        "host.client.opencode",
    ]
}
pub fn inference(descriptor: &ProviderDescriptor) -> Result<&HostOperationBinding> {
    descriptor
        .operations
        .iter()
        .find(|op| {
            matches!(
                op.service.id.as_str(),
                "host.protocol.openai_chat"
                    | "host.protocol.openai_responses"
                    | "host.protocol.anthropic"
            )
        })
        .ok_or_else(|| Error::NoCandidate("unsupported plugin inference service".into()))
}
pub fn provider(
    id: &str,
    name: &str,
    descriptor: &ProviderDescriptor,
    binding: super::PluginBinding,
) -> Result<ProviderConfig> {
    let operation = inference(descriptor)?;
    let kind = match operation.service.id.as_str() {
        "host.protocol.anthropic" => ProviderKind::Anthropic,
        _ => ProviderKind::OpenAICompatible,
    };
    let mut provider = ProviderConfig::new(id, name, kind);
    provider.base_url = operation.base_url.clone();
    provider.bypass_proxy = Some(true);
    // A manifest-declared shaping profile replaces the default Native pin:
    // the core's own shaping layer (session generation, identity headers,
    // environment-block completion, catalog registration) then applies to
    // this provider's upstream requests exactly as it does for the native
    // kinds that select the same profile.
    #[cfg(feature = "opencode")]
    if descriptor.shaping.as_deref() == Some("opencode_cli") {
        provider.client_profile = crate::config::ProviderClientProfile::OpenCodeCli;
    }
    #[cfg(not(feature = "opencode"))]
    let _ = &descriptor.shaping;
    provider.bearer_auth = operation.service_options.bearer_auth;
    provider.quirks.stream_usage = operation.service_options.stream_usage;
    if operation.service.id == "host.protocol.openai_responses" {
        provider.api_surface = crate::config::ApiSurface::Responses;
    }
    provider.extra_headers = descriptor.extra_headers.clone();
    provider.key_ref.clear();
    provider.plugin_binding = Some(binding);
    Ok(provider)
}
