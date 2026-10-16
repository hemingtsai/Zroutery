//! What Zroutery offers a taken-over client: the gateway address, a local credential, and the field set each client needs.

use super::clients::{AgentType, ManagedField};

// ---------------------------------------------------------------------------

// GatewayTarget / takeover_fields
// ---------------------------------------------------------------------------

/// One model class an agent is allowed to ask Zroutery for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierClass {
    /// The model id the agent puts in its request.
    ///
    /// OpenCode strips the provider prefix before sending, so this is the name
    /// Zroutery's own model matching sees.
    pub model: String,
    /// The name the client's model picker shows for it.
    pub display_name: String,
}

/// What Zroutery offers an agent when it takes it over.
///
/// This is a **local gateway credential**, not an upstream provider secret: the
/// agent authenticates to `127.0.0.1` and Zroutery substitutes the real provider
/// key from its own credential store. That distinction is what makes writing the
/// token into the agent's config defensible — the token's blast radius is this
/// machine — and it is why nothing here ever holds a provider key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayTarget {
    /// The local endpoint the agent should send to.
    pub base_url: String,
    /// Zroutery's local gateway token.
    pub token: String,
    /// The model classes this agent may ask for.
    pub classes: Vec<TierClass>,
    /// The class to make the agent's default, if any.
    ///
    /// `None` means the takeover does not touch the agent's model selection: the
    /// agent keeps whatever default it had and the user picks a class in the
    /// client or through Zroutery's per-client policy. Zroutery does not choose
    /// a tier on the user's behalf.
    pub default_class: Option<String>,
}

/// The provider table key every adapter registers Zroutery under.
///
/// Shared by Codex (`model_providers.<id>`) and OpenCode (`provider.<id>`) so
/// the two clients name the same gateway the same way, and so a UI can render
/// one identifier instead of guessing per client.
///
/// A free constant rather than one declared per adapter on purpose: two
/// adapters that each declared their own would be free to drift, and a client
/// whose provider id changed between releases would strand every manifest that
/// named the old one.
pub const GATEWAY_PROVIDER_ID: &str = "zroutery";

/// The environment variable OpenCode documents for relocating its config.
pub const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG";

/// The fields a takeover of `agent` manages, and the values Zroutery puts there.
///
/// # Why the set is derived here and not chosen by the user
///
/// The manifest's unit of ownership is a dotted path into one document, and a
/// takeover can only restore what it wrote. Those two facts bound the set from
/// both sides: it has to be small enough to be reviewable in a confirmation
/// dialog and stable enough to restore exactly, and only Zroutery knows which
/// fields those are. A UI that listed every dotted path in an agent's config
/// would be showing the user Zroutery's internals and asking them to pick the
/// subset that happens to work.
///
/// So each agent's set is a function of one [`GatewayTarget`], and
/// [`TakeoverStore::plan_adopt`] can still refuse if a caller asks for a field
/// this function does not produce.
///
/// # What it never manages
///
/// Zroutery's own tier → model mapping, the agent's model class unless the
/// caller set [`GatewayTarget::default_class`], approval or sandbox policy, the
/// working directory, and any provider entry the adapter did not write. Those
/// are the user's, and a manifest that recorded them would restore stale values
/// over newer ones.
///
/// # Errors
///
/// Returns an error rather than a partial set when the target is unusable or
/// the agent has no verified takeover. An empty or half-populated field set
/// would be adopted and recorded as complete.
pub fn takeover_fields(
    agent: AgentType,
    target: &GatewayTarget,
) -> Result<Vec<ManagedField>, String> {
    if target.base_url.trim().is_empty() {
        return Err("cannot take over: the local gateway URL is empty".into());
    }
    if target.token.trim().is_empty() {
        return Err(
            "cannot take over: the local gateway token is empty, so the agent would have no way \
             to authenticate to Zroutery"
                .into(),
        );
    }
    if target.classes.is_empty() {
        return Err(
            "cannot take over: Zroutery currently offers no model classes, so an agent pointed at \
             it would have nothing to ask for. Assign a tier to at least one enabled model first."
                .into(),
        );
    }
    if let Some(default) = &target.default_class {
        if !target.classes.iter().any(|c| &c.model == default) {
            return Err(format!(
                "cannot take over: default class `{default}` is not one of the classes offered \
                 ({})",
                target
                    .classes
                    .iter()
                    .map(|c| c.model.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for class in &target.classes {
        if class.model.trim().is_empty() {
            return Err("cannot take over: a class was offered with an empty model id".into());
        }
        // A dot in a model id would be read as a path separator by
        // `set_nested`/`extract_field_values` and land the value somewhere the
        // client never looks.
        if class.model.contains('.') {
            return Err(format!(
                "cannot take over: class model id `{}` contains a '.', which is the field-path \
                 separator",
                class.model
            ));
        }
    }

    match agent {
        AgentType::Claude => {
            // `env` in `~/.claude/settings.json`. Claude Code writes each `env`
            // entry into its own process environment, so these two fields are
            // what actually redirect the agent's own API traffic — not just
            // the tools it spawns — and a value here replaces the same variable
            // exported in the user's shell.
            //
            // No per-class field here on purpose. Claude Code picks a model by
            // name (`claude-sonnet-4-5`), and Zroutery already maps those names
            // onto classes with its own `match_claude_names` config. Writing
            // `ANTHROPIC_DEFAULT_*_MODEL` here would move a routing decision
            // out of Zroutery's configuration and into a file this manifest
            // then owns — the boundary takeover is not supposed to cross.
            Ok(vec![
                ManagedField {
                    path: "env.ANTHROPIC_BASE_URL".into(),
                    value: serde_json::json!(target.base_url),
                },
                ManagedField {
                    path: "env.ANTHROPIC_AUTH_TOKEN".into(),
                    value: serde_json::json!(target.token),
                },
            ])
        }

        AgentType::Codex => {
            // `model_provider` is the selector; without it the table below is
            // defined and never used.
            //
            // This table goes in the **user-level** `~/.codex/config.toml`.
            // Codex reads `model_provider` and `model_providers` out of a
            // project-local `.codex/config.toml` for nothing — it ignores those
            // keys there and prints a startup warning — so a takeover written to
            // the project file would report success and route nowhere.
            //
            // The token is written literally, and that is a deliberate choice between two
            // verified options rather than a guess:
            //
            // * `env_key` is what upstream recommends, but it names an
            //   environment variable — not a file field this manifest can
            //   restore. A takeover that pointed at an env var would have
            //   nothing to put back on release.
            // * `auth` runs a command, which would mean shipping a Zroutery CLI
            //   binary solely to print a token.
            // * `experimental_bearer_token` takes a literal token and is a real,
            //   documented, runtime-wired field (Codex emits
            //   `Authorization: Bearer <token>` from it). It is *discouraged*
            //   upstream for writing the secret in plaintext — which is exactly
            //   what it does here, into a file on the user's own machine, holding
            //   a credential whose blast radius is that machine.
            //
            // One caveat worth recording: `env_key` wins over
            // `experimental_bearer_token` when both are set, so a user who
            // already has an `env_key` in this provider table keeps using it and
            // this field is ignored. That shows up as a takeover that stays
            // `degraded` rather than as a wrong credential.
            //
            // `wire_api` is `responses` because that is the route Zroutery
            // serves for Codex's dialect. It is currently the only accepted
            // value and also the default; it is written explicitly so the
            // provider table does not depend on a default staying put.
            let id = GATEWAY_PROVIDER_ID;
            let mut out = vec![
                ManagedField {
                    path: "model_provider".into(),
                    value: serde_json::json!(id),
                },
                ManagedField {
                    // Required and non-empty: Codex rejects a provider whose
                    // `name` is blank at config-parse time, which would make the
                    // whole `config.toml` fail to load rather than just this
                    // provider being ignored.
                    path: format!("model_providers.{id}.name"),
                    value: serde_json::json!("Zroutery"),
                },
                ManagedField {
                    path: format!("model_providers.{id}.base_url"),
                    value: serde_json::json!(target.base_url),
                },
                ManagedField {
                    path: format!("model_providers.{id}.wire_api"),
                    value: serde_json::json!("responses"),
                },
                ManagedField {
                    path: format!("model_providers.{id}.experimental_bearer_token"),
                    value: serde_json::json!(target.token),
                },
            ];
            // Codex addresses a model by bare id in `model`; it has no
            // per-provider model table.
            if let Some(default) = &target.default_class {
                out.push(ManagedField {
                    path: "model".into(),
                    value: serde_json::json!(default),
                });
            }
            Ok(out)
        }

        AgentType::OpenCode => {
            let id = GATEWAY_PROVIDER_ID;
            let mut out = vec![
                ManagedField {
                    path: format!("provider.{id}.npm"),
                    value: serde_json::json!("@ai-sdk/openai-compatible"),
                },
                ManagedField {
                    path: format!("provider.{id}.name"),
                    value: serde_json::json!("Zroutery"),
                },
                ManagedField {
                    path: format!("provider.{id}.options.baseURL"),
                    value: serde_json::json!(target.base_url),
                },
                ManagedField {
                    path: format!("provider.{id}.options.apiKey"),
                    value: serde_json::json!(target.token),
                },
            ];
            for class in &target.classes {
                out.push(ManagedField {
                    path: format!("provider.{id}.models.{}.name", class.model),
                    value: serde_json::json!(class.display_name),
                });
            }
            if let Some(default) = &target.default_class {
                // OpenCode addresses a model as `<provider>/<model>`, but it
                // strips the prefix before sending, so Zroutery matches on the
                // bare class id.
                out.push(ManagedField {
                    path: "model".into(),
                    value: serde_json::json!(format!("{id}/{}", default)),
                });
            }
            Ok(out)
        }

        AgentType::Gemini => Err(
            "Gemini CLI is not supported for takeover. Its endpoint is only settable through the \
             GOOGLE_GEMINI_BASE_URL environment variable, which is not a file this manifest can \
             restore, so taking over the settings file would redirect the key but leave the client \
             talking to Google."
                .into(),
        ),
    }
}

// ---------------------------------------------------------------------------
