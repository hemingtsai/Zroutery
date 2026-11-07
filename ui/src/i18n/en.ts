/**
 * The English dictionary - the source of truth for the key set.
 *
 * TKey = keyof typeof en is the contract every other dictionary is written
 * against and the type every t() call is checked with, so this file must
 * keep the object as a bare literal (as const): annotating it with a wide
 * Record type would collapse the keys to string and silently un-check every
 * translation. Moved verbatim from i18n.tsx when the dictionaries were
 * split out of the machinery.
 */
export const en = {
  // ------------------------------------------------------------ shared
  "common.dismiss": "Dismiss",
  "common.cancel": "Cancel",
  "common.loading": "Loading…",
  "common.working": "Working…",
  "common.collapse": "Collapse",
  "common.remove": "Remove",
  "common.delete": "Delete",
  "common.clear": "Clear",
  "common.actions": "Actions",
  "common.copy": "Copy",
  "common.enabled": "Enabled",
  "common.yes": "yes",
  "common.no": "no",
  "common.dash": "—",
  "common.unknown": "unknown",
  "count.models": "{n} models",
  "count.providers": "{n} providers",

  // ------------------------------------------------------------------ app
  "app.live_stopped": "Live updates stopped: {err}",
  "app.exposed":
    "Listening on {host} exposes the gateway and your API keys to your network. Keep authentication on, or bind to 127.0.0.1.",
  "app.no_auth": "Authentication is off: any local process can spend your API credit.",
  "app.more_notifications": "Show {n} more notification | Show {n} more notifications",
  "app.fewer_notifications": "Show fewer notifications",
  "nav.overview": "Overview",
  "task.providers": "Providers",
  "task.models": "Models and routing",
  "task.clients": "Client setup",
  "task.usage": "Usage and records",
  "task.catalogue": "Models by provider",
  "task.rules": "Routing rules",
  "task.connections": "Connections and credentials",
  "task.accounts": "Account observations",
  "task.connect": "Four-class setup",
  "client_setup.title": "Native four-class model menu",
  "client_setup.hint": "Preview the fields to write and the default tier, then verify the client's request.",
  "client_setup.client": "Client",
  "client_setup.default": "Default usage tier",
  "client_setup.keep": "Keep existing session/default",
  "client_setup.standby": "Development standby (8788)",
  "client_setup.preview": "Preview synchronization",
  "client_setup.pool": "Candidate pool",
  "client_setup.evidence": "Capability source",
  "client_setup.unavailable": "Not configured: {ids}",
  "client_setup.apply": "Synchronize native menu",
  "client_setup.previewing": "Preparing the preview…",
  "client_setup.toast_applied": "Wrote {agent}'s configuration.",
  "client_setup.confirm":
    "Applies the managed fields and the selected default; an existing session may keep its choice until Codex reopens.",
  "task.migrate": "Migrate and restore",
  "accounting.title": "Ledger",
  "binding.review": "Review account binding",
  "binding.verified": "Verified identity",
  "binding.pending": "Pending verification",
  "binding.remote_user": "Remote user",
  "binding.group": "User group",
  "binding.source": "Verification source",
  "binding.panel": "Panel",
  "binding.changed": "This token belongs to a different remote identity.",
  "binding.confirm_change": "Confirm replacing this account's remote identity",
  "binding.history_kept":
    "Earlier requests and prices keep their original identity; the account's other credentials are untouched.",
  "binding.publish": "Publish binding",
  "binding.user_hint": "New token's panel user ID (if required)",
  "binding.probe_kind": "Account observation",
  "binding.partial": "Partial data ({ok}/{n} accounts)",
  "binding.probe_none": "No panel observation",
  "binding.probe_newapi": "NewAPI panel",
  "accounting.blocked": "Accounting storage needs attention. No executed call will be replayed.",
  "accounting.inspect": "Inspect usage and records",
  "accounting.durable":
    "Every request is written to disk before its result arrives — a restart never loses the ledger or double-charges it.",
  "accounting.unavailable": "Durable accounting unavailable",
  "accounting.pending": "{n} entries await a local retry.",
  "accounting.unreconciled": "{n} requests were cut off by a restart; their final cost is unknown.",
  "accounting.state": "Persistence",
  "accounting.volatile": "Memory-only Core mode",
  "accounting.known_cost": "Known cost, by currency",
  "accounting.unknown_cost": "Attempts with unknown cost",
  "accounting.unknown_usage": "Attempts with unknown usage",
  "accounting.usage_evidence": "Usage evidence",
  "accounting.usage_absent": "Not reported",
  "accounting.usage_partial": "Known portion; final usage unavailable",
  "accounting.usage_measured": "Reported by the upstream",
  "accounting.usage_invalid": "Invalid upstream counters; token usage unknown",
  "accounting.usage_conflicting": "Conflicting counters; excluded from token totals",
  "accounting.identity": "Account / inference key",
  "accounting.no_replay": "Unknown cost is not zero. A local disk failure must not replay an upstream call or tool action.",
  "nav.models": "Models",
  "nav.providers": "Providers",
  "nav.routing": "Routing",
  "nav.activity": "Activity",
  "nav.settings": "Settings",
  "nav.unclassified_title": "{n} models without a tier",
  "status.running": "Running",
  "status.stopped": "Stopped",
  "status.succeeded": "Succeeded",
  "status.failed": "Failed",
  "status.cooling": "Cooling down",
  "status.enabled": "Enabled",
  "status.disabled": "Disabled",
  "status.no_key": "No credential stored",
  "status.wallet_held": "Balance empty — requests paused",
  "appearance.title": "Appearance",
  "theme.system": "Follow system",
  "theme.light": "Light",
  "theme.dark": "Dark",
  // ------------------------------------------------------------------ mode
  "mode.simple": "Simple",
  "mode.geek": "Advanced",
  "mode.toggle_aria": "Interface mode",
  "mode.geek_only_page": "That page is available in Advanced mode.",
  "mode.advanced_active_toast":
    "Advanced configuration is in effect: {n} item. Simple mode hides it, but it keeps running. | Advanced configuration is in effect: {n} items. Simple mode hides them, but they keep running.",
  "mode.settings_hint": "Advanced configuration keeps running; it is only hidden.",
  "gw.address": "Address",
  "gw.copy_address": "Copy address",
  "gw.stop": "Stop gateway",
  "gw.start": "Start gateway",
  "settings.language": "Language",
  "settings.theme": "Theme",
  "settings.skin": "Skin",
  "skin.industrial": "Default",
  "skin.md3": "Material 3",
  "skin.fluent": "Fluent",
  "lang.zh": "简体中文",
  "lang.zh_tw": "繁體中文",
  "lang.en": "English",
  "action.hide": "Hide",
  "toast.copied": "Copied.",
  "toast.queued_discarded":
    "The save failed, so edits queued behind it were dropped.",
  "toast.balances_refreshed": "Balances refreshed.",
  "toast.balances_refresh_failed":
    "The balance refresh failed; the last readings still stand.",

  // ---------------------------------------------------------------- confirm
  "confirm.remove_provider": "Remove provider",
  "confirm.remove_provider_body":
    "Its {n} model and stored key are removed with it. | Its {n} models and stored key are removed with it.",
  "confirm.remove_model": "Remove model",
  "confirm.remove_model_body": "Clients calling it by id will get errors.",
  "confirm.remove_budget": "Delete budget",
  "confirm.reset_stats": "Reset counters",
  "confirm.reset_stats_body":
    "Request history, model health and spend estimates are cleared.",
  "confirm.regenerate_token": "Regenerate token",
  "confirm.regenerate_token_body":
    "Existing clients stop working until you give them the new token.",
  // ------------------------------------------------------- backend errors
  // Raw backend text is English and technical; the few conditions an operator
  // can actually act on are re-stated here and matched in describeBackendError.
  "err.bind_in_use":
    "Port {addr} is already in use. Stop the other Zroutery process or choose a different port in Settings.",
  "err.bind_denied":
    "The proxy could not start: the system refused {addr}. Windows sometimes reserves port ranges; pick a different port in Settings.",
  "err.credentials_rejected":
    "The panel rejected this credential. Use its personal access token (personal settings → access token), not a relay sk- key.",
  "err.relay_key":
    "This is a relay API key (sk-…); the panel dashboard does not accept it. Use the personal access token from personal settings → access token.",
  "err.no_panel_token":
    "No panel token is stored for this account. Set its credential to the panel's personal access token.",
  "err.capture_open":
    "A token capture is already open for this account. Finish the sign-in in that window, or cancel it, before starting another.",
  "err.no_browser":
    "No Chromium browser was found. Set the browser executable in this account's maintenance settings, or install Chrome or Edge.",
  "err.capture_rejected":
    "The panel rejected the token it handed back. Sign in to the panel again and complete any verification step before capturing.",
  "err.capture_closed":
    "The browser was closed before a token was captured. Start the capture again and leave the window open until it finishes.",
  "err.classifier_candidate":
    "Auto review candidate {model} is not a configured model and is skipped. Remove it in Routing → Auto review or add the model.",
  "err.no_account_balance":
    "No account of {provider} reported a balance. Store the panel's personal access token as an account credential; relay keys do not work for the dashboard.",
  "err.mixed_currencies":
    "The accounts report different currencies, so their balances cannot be summed. No total is shown.",
  // The reassurance class: a failure that says what is still in place and what
  // to do about it. These are the messages an operator meets when something
  // goes wrong, and they are the ones whose English would otherwise be the
  // only English on a Chinese page.
  "err.backup_exists":
    "The configuration cannot be preserved as {backup}, because that file already exists. Move or remove it and try again.",
  "err.preserve_failed":
    "The configuration cannot be preserved as {backup}: {err}. It was left as it is, so fix or move it and start again.",
  "err.copy_mismatch":
    "The copy of {path} in {backup} does not match it, so {path} was left as it is.",
  "err.copy_unreadable":
    "The copy of {path} in {backup} cannot be read back ({err}), so {path} was left as it is.",
  "err.change_refused":
    "The change to {addr} was refused, and the previous configuration and gateway are still in place ({err}).",
  "err.address_not_changed":
    "The new address cannot be used: {err}. The configuration was not changed.",
  "err.address_not_changed_running":
    "The new address cannot be used: {err}. The gateway is still listening on {addr}, and the configuration was not changed.",
  "err.listen_failed_restored":
    "The gateway could not listen on {addr}: {err}. The previous gateway was restored.",
  "err.listen_failed_not_restored":
    "The gateway could not listen on {addr}: {err}. The previous gateway could not be restored either: {restore}",
  "err.no_rollback_snapshot":
    "Could not snapshot {path}: the file is unreadable, so the cutover cannot be undone. Fix its permissions and retry.",
  "err.no_such_account":
    "There is no account {account} on provider {provider}.",
  "err.bind_other":
    "The proxy could not start: {addr} could not be bound ({err}). Pick a different port in Settings.",
  // -------------------------------------------------------------- accounts
  "accounts.title": "Accounts",
  "accounts.title_hint": "Once an account has an inference key, chat requests go through it.",
  "accounts.empty": "No accounts yet; every request uses the provider's own key.",
  "accounts.lede":
    "Balance, check-in and panel token for each account. Add or edit accounts in the provider's account section.",
  "accounts.add": "Add account",
  "accounts.id_label": "Account ID",
  "accounts.id_hint": "A stable name within this provider.",
  "accounts.key_label_add": "Panel access token (optional)",
  "accounts.key_hint_add":
    "Empty uses the provider key. For a NewAPI panel paste the personal access token — sk- relay keys do not work.",
  "accounts.duplicate": "Account `{id}` already exists on this provider.",
  "accounts.key_store_failed": "The account was saved, but storing its key failed.",
  "accounts.set_key": "Set credential",
  "accounts.key_label": "Panel access token",
  "accounts.key_hint": "From the panel: personal settings → access tokens.",
  "accounts.clear_key": "Clear credential",
  "accounts.clear_key_title": "Clear account {id}'s credential",
  "accounts.clear_key_body": "This account goes back to the provider's key.",
  "accounts.own_key": "Own credential",
  "accounts.no_key": "Credential not stored",
  "accounts.follows_provider": "Provider's credential",
  "accounts.inference_title": "Inference keys",
  "accounts.inference_hint":
    "What chat requests are sent with; the panel token above is only for balances and check-in.",
  "accounts.inference_empty":
    "Once any account on this provider has an inference key, accounts without one are skipped.",
  "accounts.inference_stored": "secret stored",
  "accounts.inference_missing": "secret missing",
  "accounts.inference_priority": "priority {n}",
  "accounts.inference_priority_label": "Priority",
  "accounts.inference_priority_hint": "Lower is tried first.",
  "accounts.inference_note": "Note",
  "accounts.inference_note_placeholder": "e.g. main account",
  "accounts.inference_replace": "Replace secret",
  "accounts.inference_key_label": "New secret",
  "accounts.inference_key_hint": "",
  "accounts.inference_new_id": "Key ID",
  "accounts.inference_new_id_hint": "e.g. wallet-1. Letters, digits, `-` and `_`.",
  "accounts.inference_new_secret": "Inference secret",
  "accounts.inference_add": "Add inference key",
  "accounts.inference_duplicate": "Inference key `{id}` already exists on this account.",
  "accounts.inference_store_failed": "The inference key could not be saved.",
  "accounts.inference_remove_title": "Remove inference key {id}",
  "accounts.inference_remove_body": "Requests stop using it immediately.",
  "common.save": "Save",
  "accounts.remove_title": "Remove account {id}",
  "accounts.remove_body": "The account and its stored key are removed.",
  "accounts.checkin": "Browser check-in",
  "accounts.checkin_settings": "Check-in settings",
  "accounts.login_path": "Panel sign-in path",
  "accounts.login_path_hint": "NewAPI signs in at /sign-in; older forks use /login.",
  "accounts.panel_url": "Panel address",
  "accounts.new_api_user": "Panel user ID",
  "accounts.new_api_user_hint":
    "Required by some panels (AgentRouter). Filled in automatically by token capture.",
  "accounts.panel_url_hint": "Only needed when the panel lives on a different host than the relay.",
  "token_capture.start": "Get token in browser",
  "token_capture.starting": "Opening…",
  "token_capture.captured": "Token stored",
  "token_capture.failed": "Token capture failed",
  "token_capture.cancelled": "The token capture was cancelled.",
  "token_capture.done": "The panel token is stored as this account's credential.",
  "token_capture.phase.opening": "Opening the browser…",
  "token_capture.phase.waiting": "Sign in in the browser window…",
  "token_capture.phase.needs_verification": "Finish the verification in the browser…",
  "token_capture.phase.stored": "Verifying and storing…",
  "accounts.interval": "Interval (seconds)",
  "accounts.interval_hint": "Empty disables automatic runs.",
  "accounts.checkin_path": "Check-in page path",
  "accounts.checkin_path_hint": "Panel-relative path, e.g. /checkin.",
  "accounts.checkin_selector": "Check-in button selector",
  "accounts.checkin_selector_hint":
    "CSS selector of the button to click, e.g. button.checkin.",
  "error.title": "The dashboard hit a bug",
  "error.unaffected": "The proxy is unaffected and keeps running in the menu bar.",
  "common.try_again": "Try again",
  "common.reload": "Reload",
  "nav.main_aria": "Main",
  "activity.tries": "{n} tries",
  "providers.open_row": "Open {name}",
  "models.open_row": "Open {id}",
  "cc.import_row": "Import {name}",
  "settings.remove_origin": "Remove {origin}",

  // ------------------------------------------------------------ overview
  "overview.gateway": "gateway",
  "overview.gateway_stopped": "gateway stopped",
  "overview.counts": "{models} models · {providers} providers",
  "action.copy_base_url": "Copy base URL",
  "action.copy_token": "Copy token",
  "overview.configure": "Configure",
  "route.default": "Default",
  "route.default_hint": "ordinary requests",
  "route.auto_review": "Auto review",
  "route.review_hint_on": "permission checks",
  "routing.tab_group": "Routing subjects",
  "routing.tab_ml": "Learned routing",
  "routing.tab_main": "Model routes",
  "routing.tab_auto": "Auto review",
  "route.review_hint_off": "follows Default",
  "route.check": "check",
  "route.no_tier": "No tier has a model.",
  "route.no_candidate": "Enabled, no candidate.",
  "route.off": "Off.",
  "overview.recent": "Recent",
  "overview.all_activity": "All activity",
  "overview.no_requests": "No requests yet.",
  "kind.main": "Main",
  "kind.review": "Review",
  "overview.no_providers": "No providers.",
  "overview.add_provider": "Add a provider",
  "overview.add_models": "Add models →",

  // -------------------------------------------------------------- models
  "models.pick_provider_name": "Pick a provider and fill in the model name.",
  "models.duplicate": "That provider already offers “{model}”.",
  "models.unclassified":
    "{n} models have no tier: {ids}; they stay callable by exact id but do not join *-class routing.",
  "models.lede_none": "Models you add to a provider appear here.",
  "models.exposed": "{n} models · open one to edit, or add from a catalogue.",
  "models.empty": "No models.",
  "models.empty_hint": "Add one below, or import from CC Switch on the Providers page.",
  "models.no_tier": "no tier",
  "models.add_section": "Add a model",
  "models.add_hint": "Assign a tier after adding; Zroutery does not guess one.",
  "field.provider": "Provider",
  "field.model_name": "Model name",
  "models.add": "Add model",
  "models.add_provider_first": "Add a provider first.",
  "models.missing_provider": "missing provider",
  "models.provider": "Provider",
  "models.upstream": "Upstream model",
  "models.tier": "Tier",
  "models.tier_for": "Tier for {model}",
  "models.aliases": "Aliases",
  "models.performance": "Performance",
  "models.no_traffic": "No traffic yet.",
  "models.avg_latency": "Average latency",
  "models.requests": "Requests",
  "models.failures": "Failures",
  "models.last_error": "Last error",
  "models.configuration": "Configuration",
  "models.f_model_name_hint": "Sent upstream; renaming changes the exposed id",
  "models.f_display": "Display name",
  "models.f_display_hint": "",
  "models.f_priority": "Priority",
  "models.f_priority_hint": "Lower wins inside a tier",
  "models.f_weight": "Weight",
  "models.f_weight_hint": "Tie breaking among equal priorities",
  "models.f_max_tokens": "Max output tokens",
  "models.f_max_tokens_hint": "Caps what clients may ask for",
  "models.f_context_window": "Context window",
  "models.f_context_window_hint": "What the model accepts, when known.",
  "models.context_set_1m": "Set 1M",
  "models.context_unknown": "unknown",
  "models.f_aliases_hint": "Comma separated short names",
  "models.search": "Search models",
  "models.showing": "{shown} of {total}",
  "models.no_match": "No model matches “{query}”.",
  "models.name_taken": "`{id}` already names another model.",
  "models.exposed_as": "Exposed as {id}",
  "models.rename_required": "A model name is required.",
  "models.rename_failed": "The rename could not be saved; no reference was changed.",
  "models.import_browse": "Browse the provider's catalogue",
  "models.import_hide": "Hide the catalogue",
  "models.import_search": "Filter the catalogue",
  "models.import_tier": "Add selected as",
  "models.import_add": "Add selected ({n})",
  "models.import_empty": "No model matches.",
  "models.import_added": "Added",
  "models.imported": "{n} models added.",
  "models.tool_use": "Tool use",
  "models.vision": "Vision",
  "models.thinking": "Extended thinking",
  "models.effort_levels": "Reasoning effort levels",
  "models.effort_levels_hint":
    "Variants offered to the client during takeover, one per level; adjust any that look wrong.",
  "models.sync": "Sync metadata",
  "models.syncing": "Syncing…",
  "models.synced": "Filled missing metadata for {n} models",
  "models.sync_none": "Every configured model already has its metadata",
  "models.sync_failed": "Could not read the catalogue for {names}",
  "models.price": "Price per million tokens",
  "models.price_cell": "in {input} · out {output} · {currency}/1M",
  "models.price_missing": "No price configured",
  "models.price_side_unset": "not set",
  "models.price_hint":
    "Leave empty and requests are logged without a cost rather than a guessed one.",
  "field.currency": "Currency",
  "field.currency_hint": "USD, CNY, …",
  "field.input": "Input",
  "field.input_hint": "per 1M prompt tokens",
  "models.input_price": "Input price",
  "models.output_price": "Output price",
  "models.price_unlimited": "unlimited",
  "models.price_same_as_input": "same as input",
  "models.price_not_billed": "not billed",
  "common.close": "Close",
  "drawer.saving": "Saving…",
  "drawer.saved": "Saved · {time}",
  "drawer.save_failed": "Save failed: {err}",
  "routing.priority_for": "Priority for {model}",
  "routing.move_up": "Move {model} up",
  "routing.drag_hint": "Drag to reorder, or use the arrows. The order saves as you drop.",
  "routing.strategy_order_note":
    "The {strategy} strategy decides the live order; this list is only the starting priority.",
  "routing.move_down": "Move {model} down",
  "field.output": "Output",
  "field.output_hint": "per 1M completion tokens",
  "models.cache_read": "Cache read",
  "models.cache_read_hint": "optional, defaults to input",
  "models.cache_write": "Cache write",
  "models.cache_write_hint": "optional",
  "models.clear_price": "Clear price",
  "models.remove": "Remove model",

  // ----------------------------------------------------------- providers
  "providers.lede_none": "One provider per upstream account or endpoint.",
  "providers.import_cc": "Import from CC Switch",
  "providers.reading_cc": "Reading CC Switch…",
  "providers.empty": "No providers.",
  "providers.empty_hint": "Add one below, or import from CC Switch.",
  "providers.openai_dialect": "OpenAI compatible",
  "providers.anthropic_dialect": "Anthropic",
  "providers.provider_off": "(off)",
  "providers.add_section": "Add a provider",
  "providers.add_hint": "Endpoint, credential and dialect; models come next on their own page.",
  "field.name": "Name",
  "field.api_dialect": "API dialect",
  "field.base_url": "Base URL",
  "field.base_url_hint": "Without the endpoint path",
  "providers.add": "Add provider",
  "providers.add_failed": "The provider could not be saved; the draft is kept for retry.",
  "providers.duplicate": "A provider called “{id}” already exists.",
  "providers.name_optional": "Optional when a base URL is given; otherwise taken from its host.",
  "providers.name_placeholder": "Leave empty to use the host",
  "providers.name_required": "Enter a name or a base URL to derive one from.",
  "cc.source_hint":
    "Read from {path}; keys go into the OS credential store, never the config file.",
  "cc.reload": "Reload",
  "cc.import": "Import",
  "cc.empty": "No Claude Code providers found in CC Switch.",
  "cc.active": "active",
  "cc.already": "already here",
  "cc.imported_notice": "Imported {n} providers; keys went into the credential store.",
  "providers.endpoint": "Endpoint",
  "providers.dialect": "Dialect",
  "providers.auth": "Authentication",
  "providers.no_key": "no key stored",
  "providers.no_cred": "no credential needed",
  "providers.auth_account_key": "an account inference key",
  "providers.models_count": "{n} on this provider",
  "providers.models_none": "none yet",
  "providers.api_key": "API key",
  "providers.api_key_hint": "Kept in the system credential store, not the config file.",
  "providers.save_key": "Save key",
  "providers.models_section": "Models",
  "providers.empty_catalogue": "{name} returned an empty model list.",
  "providers.configuration": "Configuration",
  "field.timeout": "Timeout (s)",
  "field.timeout_hint": "Whole request, including streaming idle gaps",
  "field.connect_timeout": "Connect timeout (s)",
  "providers.proxy_policy": "Upstream proxy",
  "providers.proxy_policy_hint":
    "Applies to requests to this upstream only.",
  "providers.proxy_inherit": "Inherit gateway setting",
  "providers.proxy_direct": "Direct · bypass proxy",
  "providers.proxy_system": "Follow system proxy",
  "providers.f_version": "anthropic-version",
  "providers.f_version_hint":
    "Defaults to 2023-06-01; 2026-10-21 drops temperature, top_p and top_k.",
  "providers.f_beta": "anthropic-beta",
  "providers.f_beta_hint":
    "Comma separated beta capabilities; empty sends none.",
  "providers.api_surface": "API surface",
  "providers.api_surface_hint":
    "Which OpenAI API this endpoint speaks; some Codex gateways only accept Responses.",
  "providers.surface_chat": "Chat Completions",
  "providers.surface_responses": "Responses",
  "providers.advanced_settings": "Advanced",
  "providers.client_profile": "Client identity",
  "providers.client_profile_hint":
    "Who Zroutery presents as upstream. Claude Code and Codex also forward that client's own headers.",
  "providers.profile_auto": "Auto · detect from the client",
  "providers.profile_native": "Native · no client identity",
  "providers.profile_claude_code": "Claude Code · with its headers",
  "providers.profile_codex": "Codex · with its headers",
  "providers.profile_opencode_cli": "OpenCode CLI · locked for OpenCode providers",
  "providers.client_profile_locked_hint":
    "Locked: OpenCode's gateway verifies this identity and rejects any other.",
  "providers.opencode_zen_dialect": "OpenCode Zen (free)",
  "providers.opencode_console_dialect": "OpenCode Console",
  "providers.decision_dialect": "Decision model",
  "providers.decision": "Decisions",
  "providers.decision_hint":
    "Answers fixed questions with a probability instead of text; serves Auto review only.",
  "providers.decision_path": "Decision path",
  "providers.decision_path_hint": "Appended to the Base URL, e.g. /v1/systemone.",
  "providers.decision_threshold": "Required confidence",
  "providers.decision_threshold_hint":
    "How sure the model must be before Auto review blocks. Higher blocks more.",
  "providers.decision_no_catalogue":
    "A decision endpoint has no model catalogue. Add its model on the Models page.",
  "models.tier_decision": "Why no tier",
  "models.tier_decision_hint":
    "Decision models serve Auto review only and cannot answer chat; add it to the Auto review pool.",
  "providers.free_badge": "free",
  "providers.free_only": "Free models only",
  "providers.free_only_hint":
    "Shows models the catalogue marks as free. A free model can still be rate-limited, region-blocked or retired.",
  "providers.catalogue_shown": "Showing {shown} of {total} models",
  "providers.catalogue_total": "{total} models",
  "providers.catalogue_no_free":
    "The catalogue has {total} models and none is marked free. Turn the filter off to see them.",
  "providers.bearer_auth": "Also send Bearer auth",
  "providers.bearer_auth_hint":
    "For relays that read Authorization: Bearer instead of x-api-key",
  "providers.balance": "Balance",
  "providers.balance_hint":
    "Some providers publish remaining credit at their own endpoint. Pick the one this provider uses; Zroutery asks with the provider key.",
  "providers.balance_hint_panel":
    "Read from this provider's NewAPI panel with an account's personal access token.",
  "providers.balance_hint_none":
    "This provider publishes no balance and has no panel account to ask. Pick a probe above, or declare an account and store its personal access token.",
  "providers.balance_no_source":
    "This provider has no balance source: it publishes none and declares no panel account.",
  "balance_preset.none": "Not supported",
  "balance_preset.deep_seek": "DeepSeek",
  "balance_preset.moonshot": "Moonshot",
  "balance_preset.silicon_flow": "SiliconFlow",
  "balance_preset.open_router": "OpenRouter",
  "balance_preset.sub2api": "Sub2API relay",
  "balance_preset.custom": "Custom endpoint",
  "balance_hint.none": "OpenAI and Anthropic publish no balance of their own.",
  "balance_hint.deep_seek": "Asks /user/balance on the provider's own endpoint.",
  "balance_hint.moonshot": "Asks /users/me/balance on the provider's own endpoint.",
  "balance_hint.silicon_flow": "Asks /user/info on the provider's own endpoint.",
  "balance_hint.open_router": "Asks /credits on the provider's own endpoint.",
  "balance_hint.sub2api":
    "Asks /v1/usage. The answer is a wallet balance, a key quota or a subscription allowance, depending on how the key was issued.",
  "balance_hint.custom": "Your own path, and your own JSON pointers for the remaining, used and total.",
  "field.probe": "Probe",
  "providers.check_now": "Check now",
  "providers.checking": "Checking…",
  "providers.compatibility": "Compatibility",
  "providers.compatibility_hint":
    "For providers that do not follow the standard protocol exactly. Leave them off unless a request fails.",
  "providers.repair.title": "Request repair",
  "providers.repair.hint":
    "Overrides of the Routing page's global repair defaults for this provider. 'Follow the global' inherits the Routing page's switch.",
  "providers.repair.inherit": "Follow the global default",
  "providers.repair.on": "On for this provider",
  "providers.repair.off": "Off for this provider",
  "quirk.max_completion_tokens": "Use max_completion_tokens",
  "quirk.drop_temperature": "Drop temperature",
  "quirk.drop_top_p": "Drop top_p",
  "quirk.drop_stop": "Drop stop sequences",
  "quirk.drop_stop_hint": "Never applied to Auto review queries",
  "quirk.system_as_developer": "System as developer role",
  "quirk.reasoning_effort": "Translate thinking to reasoning_effort",
  "quirk.portable_history": "Reuse Responses history across upstreams",
  "quirk.portable_history_hint":
    "For relays where another upstream may answer next (AgentRouter, CC Switch). Responses requests only.",
  "providers.remove": "Remove provider",
  "providers.removed_models_notice": "Removed {n} models that belonged to that provider.",
  "providers.balance_failed": "check failed",

  // ------------------------------------------------------- account maintenance
  // Phase names are the vocabulary the backend publishes. Each one is a distinct
  // state rather than a colour, because they call for different things from the
  // person looking at them.
  "account.phase.idle": "not checked in yet",
  "account.phase.running": "checking in…",
  "account.phase.waiting_for_user":
    "Needs human verification. Finish it in the open window.",
  "account.phase.succeeded": "checked in",
  "account.phase.already_completed": "already checked in today",
  "account.phase.not_supported": "this provider does not offer check-in",
  "account.phase.failed": "check-in failed",
  "account.phase.cancelled": "cancelled",
  "account.phase.cancelling": "cancelling…",
  /*
   * Six scheduler decisions, six sentences. The reasons an account does not run
   * on a timer are different enough that a shared "nothing happened" would send
   * the reader to change a setting that is not the problem: one of these is the
   * user's own choice, one is a gap in their declaration, and only two mean a
   * schedule is quietly counting down.
   */
  "account.balance": "balance",
  "account.balance_amount": "{remaining} {unit} left of {total}",
  "account.schedule.disabled": "Check-in is switched off for this account",
  "account.schedule.not_applicable":
    "Check-in is on but cannot run. Set the check-in page and its trigger selector.",
  "account.schedule.needs_first_observation":
    "No check-in has run yet, so there is no due time. Run one to start the schedule.",
  "account.schedule.no_policy":
    "No check-in interval is set, so nothing will run automatically even though this account has been checked in before",
  "account.schedule.not_due": "Scheduled",
  "account.schedule.due": "Due now",
  "account.schedule.unknown": "Schedule not known",
  "account.next_run": "next run",
  "account.last_checkin": "last check-in",
  "account.reward": "reward",
  // A figure recovered from the provider's log text is weaker evidence than one
  // it reported in a structured field, and the two are labelled differently so a
  // panel never presents one as the other.
  "account.reward_weak_source": "read from provider log text",
  "account.reward_inferred": "inferred from the balance change",
  "account.effect.credited": "observed: added",
  "account.effect.debited": "observed: taken",
  "account.effect.no_change": "observed: nothing moved",
  "account.effect.unobserved": "not observed either way",
  "account.source.provider_response": "reported by the provider",
  "account.source.account_state": "read from the account state",
  "account.source.account_log": "read from the account log",
  "account.source.operation_response": "from the operation response",
  "account.checkin": "Check in",
  "account.resume": "Continue in the open window",
  "account.cancel": "Cancel",
  "account.close_browser": "Close window",
  "account.not_enabled":
    "Check-in is off for this account. Enable it in the provider's account settings.",
  "account.need_path":
    "No check-in page is set. Add it in the provider's account settings.",
  "account.need_selector":
    "No check-in control is set. Add its CSS selector in the provider's account settings, e.g. button.checkin.",
  "account.start_failed": "Could not start the check-in",
  "account.started": "The check-in has started. The browser will ask for a human if it needs one.",
  "account.resumed": "The check-in has resumed.",
  "account.cancelled": "The check-in was cancelled.",
  "account.resume_failed": "Could not continue the check-in",
  "account.cancel_failed": "Could not cancel the check-in",

  // ------------------------------------------------------- account probing
  //
  // A probe is not a check-in. It asks the provider what state an account is in
  // and reports what came back; it never opens a browser and never grants
  // anything. The wording keeps that apart, because "check in" next to a button
  // that only reads would send the reader looking for a reward that is not there.
  //
  // The probe runs on the button and never on a timer: it costs one request per
  // declared account, and a proxy that probes on its own turns a slow panel into
  // a slow start. Saying so is part of the hint, not a footnote.
  /*
   * One press over `refresh_balance` per provider, which is the only way to do
   * it today. It also probes every account, because `refresh_balances` does both
   * levels in one snapshot — so this is not merely "the same thing, all at once"
   * as the Accounts page's probe, which asks the same accounts but answers with
   * the per-account outcome.
   */
  "providers.refresh_all": "Refresh every balance",
  "providers.wallet_holds": "Empty wallets blocked",
  "providers.wallet_holds_hint":
    "A provider answered that its wallet is empty, so the router stopped sending to it. Refilling does not release the block; press Release after the account is usable again.",
  "providers.wallet_empty": "wallet empty",
  "providers.wallet_held_since": "held since {time}",
  "providers.wallet_release": "Release",
  "providers.panel_info": "Panel information",
  "providers.panel_info_hint":
    "Read announcements and USD prices from this provider's NewAPI panel. Uses the selected account's panel address when set.",
  "providers.panel_account": "Panel account",
  "providers.panel_account_hint": "Select which account's panel address to read. These public requests use no credentials.",
  "providers.panel_group": "Pricing group",
  "providers.panel_group_hint": "Select the public group quote to use for this account or key.",
  "providers.price_account_scope": "All keys on this account",
  "providers.price_fallback_hint": "Public quotes are estimates, not verified charges. The selected fallback expires after 24 hours; other accounts and manual overrides are preserved.",
  "providers.panel_no_announcements": "The panel has no active announcements.",
  "providers.panel_announcements_failed": "Announcements could not be read: {error}",
  "providers.panel_prices_failed": "Prices could not be read: {error}",
  "providers.panel_price_caption": "Published model prices in USD",
  "providers.panel_cache_read": "Cache read",
  "providers.panel_cache_write": "Cache write",
  "providers.panel_per_request": "Per request",
  "providers.panel_read": "Read panel",
  "providers.panel_read_hint":
    "Read the panel's announcements and published prices. Nothing is changed until you apply the prices.",
  "providers.panel_announcement": "Announcement",
  "providers.panel_prices": "{matched} of {total} published prices match this provider's models (group {group}).",
  "providers.panel_apply_prices": "Use as account/key fallback",
  "providers.panel_prices_imported": "Estimated account/key fallback prices saved.",
  /*
   * The Accounts page: the runtime face of every declared account. The split
   * from the provider drawer is declaration (there) versus observation (here),
   * and the lede says the one thing this page does not do — route by account —
   * because an operator who assumed it would otherwise learn it from a
   * misrouted request.
   */
  "nav.accounts": "Accounts",
  "nav.takeover": "Takeover",
  "accounts.empty_build": "This build cannot probe or check in accounts.",
  "accounts.none":
    "No accounts declared. Add one in a provider's Accounts section on the Providers page.",
  "accounts.probe_section": "Account probe",
  "accounts.probe_hint":
    "Ask each enabled account's panel for its current state. One request per account, only when you press the button.",
  "accounts.probe_now": "Probe every account",
  "accounts.probing": "Probing…",
  "accounts.probe_idle":
    "No probe has run yet.",
  "accounts.probe_none": "No enabled account to probe.",
  "accounts.probe_outcome": "Outcome",
  "accounts.provider_disabled": "provider disabled",
  "accounts.own_key_stored": "its own access token is stored",
  "accounts.own_key_missing": "configured its own credential, but none is stored",
  "accounts.shared_key": "uses the provider's credential",
  /*
   * The credential-type hint. The panel API authenticates with the site's
   * access token — the string on the panel's personal-settings page — and an
   * `sk-` relay key fails there with an answer that reads as "wrong password".
   * Saying which is which is the difference between a fixable declaration and
   * a stale one.
   */
  "accounts.auth_hint":
    "The panel rejected this credential. Accounts need the panel's personal access token, not a relay sk- key.",
  "accounts.last_failure": "last probe failed",
  "accounts.checkin_api": "Check in (direct)",
  "accounts.checkin_api_hint":
    "Signs in with the stored token, no browser. Fails if the panel asks for a challenge.",
  "accounts.checking": "Checking in…",
  "accounts.checkin_api_failed": "Direct check-in failed",
  "accounts.events_section": "Account events",
  "accounts.events_hint":
    "Recent account events, newest first, with the source of each figure.",
  "accounts.events_none": "Nothing observed yet.",
  "accounts.events_when": "When",
  "accounts.events_what": "What",
  "accounts.status.active": "active",
  "accounts.status.suspended": "suspended",
  "accounts.status.quota_exhausted": "quota exhausted",
  "accounts.status.rate_limited": "rate limited",
  "accounts.status.authentication_expired": "credential rejected",
  "accounts.status.unknown": "unknown",
  "accounts.status.unprobed": "not probed",
  "field.account": "Account",
  /*
   * Four outcomes, four sentences. They are kept apart because each answers a
   * different question, and a single healthy/unhealthy pair would say an account
   * nobody could ask about was an account in trouble.
   */
  "account.sync.refreshed": "Probed",
  "account.sync.partially_reported": "Reported without",
  "account.sync.unsupported": "This build cannot ask this provider; not probed.",
  "account.sync.failed": "Probe failed",

  // ------------------------------------------------------------ takeover
  /*
   * The takeover page. A takeover writes Zroutery's local gateway address and
   * token into a client's own config file, records what it replaced, and
   * restores it on release. The states are rendered per state because four of
   * them need a decision, and a collapsed held/not-held pair would hide the
   * exact moments a user must make one.
   */
  "takeover.section": "Agent takeover",
  "takeover.section_hint":
    "Writes the gateway address and token into a client's own config, and restores the original values on release.",
  "takeover.none": "No external agent is known to this build.",
  "takeover.load_failed":
    "The takeover record could not be read: {err}; nothing on this page has changed a client's configuration.",
  "takeover.agent.claude": "Claude Code",
  "takeover.agent.codex": "Codex CLI",
  "takeover.agent.opencode": "OpenCode",
  "takeover.agent.gemini": "Gemini CLI",
  "takeover.state.unsupported": "not supported",
  "takeover.state.not_detected": "not detected",
  "takeover.state.available": "available",
  "takeover.state.ready": "ready",
  "takeover.state.active": "active",
  "takeover.state.conflict": "conflict",
  "takeover.state.degraded": "degraded",
  "takeover.state.released": "released",
  "takeover.state.error": "error",
  "takeover.hint.unsupported":
    "Gemini CLI's endpoint is only settable through an environment variable, which no config file records. Not offered.",
  "takeover.hint.not_detected":
    "No {agent} config found. Install and run it once, then refresh.",
  "takeover.hint.ready":
    "Ready: adopting writes the {n} field(s) below and records their current values.",
  "takeover.confirm_inline":
    "Adopting edits {agent}'s config file; release restores exactly these fields and leaves the rest untouched.",
  "takeover.hint.active":
    "A request from this client reached the gateway, and its managed fields match what Zroutery wrote.",
  "takeover.hint.degraded":
    "The config is managed, but verification is incomplete. See the reason below.",
  "takeover.hint.conflict":
    "{n} managed field(s) changed outside Zroutery. Choose on release whether to keep those edits or restore the recorded values.",
  "takeover.hint.released":
    "Released; original values restored. Taking over again records the current values.",
  "takeover.blocker.no_token":
    "No gateway token yet. Set or regenerate it in Settings → Gateway.",
  "takeover.blocker.no_classes":
    "No enabled model has a tier, so an agent would have nothing to ask for. Assign a tier first.",
  "takeover.reason.unverified":
    "The takeover has not been confirmed by a real request from this client yet.",
  "takeover.reason.token_changed":
    "The gateway token changed after this client was taken over; release and take over again to hand it the new token.",
  "takeover.table.field": "Field",
  "takeover.table.current": "Current value",
  "takeover.table.next": "After adoption",
  "takeover.table.managed": "Zroutery wrote",
  "takeover.generation": "taken over {n} times",
  "takeover.adopt": "Take over",
  "takeover.adopt_again": "Take over again",
  "takeover.sync_menu": "Sync model menu",
  "takeover.setup_drawer": "Model menu & default tier",
  "takeover.refresh": "Refresh",
  "takeover.restart.claude": "Restart Claude Code to pick it up.",
  "takeover.restart.codex": "Restart Codex CLI to pick it up.",
  "takeover.restart.opencode": "Restart OpenCode to pick it up.",
  "takeover.restart.gemini": "Restart Gemini CLI to pick it up.",
  "takeover.release": "Release",
  "takeover.working": "Working…",
  "takeover.strategy.label": "Conflict resolution",
  "takeover.strategy.keep_external": "Keep my edits",
  "takeover.strategy.overwrite": "Restore Zroutery's values",
  "takeover.strategy.skip": "Skip changed fields",
  "takeover.confirm_adopt_title": "Take over {agent}?",
  "takeover.confirm_adopt_body":
    "{n} field(s) in {agent}'s config will be rewritten and managed. Their current values are recorded for release.",
  "takeover.confirm_adopt_again_body":
    "{agent}'s configuration will be rewritten and managed from then on, with the current values recorded for release.",
  "takeover.confirm_release_title": "Release {agent}?",
  "takeover.confirm_release_body":
    "The managed fields in {agent}'s config will be restored to the recorded values; Zroutery stops managing them.",
  "takeover.toast_adopted": "{agent} is now taken over.",
  "takeover.toast_released": "Released.",
  "takeover.report_kept": "kept {n} of your edit(s)",
  "takeover.report_overwritten": "restored {n} field(s)",
  "takeover.report_skipped": "skipped {n} field(s)",

  // ------------------------------------------------------------- cutover
  /*
   * The cutover page. A cutover replaces a client's whole configuration file
   * with a staged copy, having first snapshotted the original byte for byte.
   * It overlaps the takeover page on purpose and the two are not the same
   * tool: takeover writes managed fields and tracks their previous values,
   * this writes the file and keeps the file — which is the only one of the two
   * that survives the process dying mid-run. That is why this page exists
   * rather than a second set of buttons on the takeover page: the case it
   * answers is a cutover that failed or was interrupted, and whether the
   * user's file came back.
   */
  "nav.cutover": "Cutover",
  "cutover.section": "Configuration cutover",
  "cutover.section_hint":
    "Moves a client's configuration onto this gateway, capturing the original file first so a failed run can be undone; to switch a client on or off, use Takeover.",
  "cutover.state.section": "Migration record",
  "cutover.state.detected": "nothing run yet",
  "cutover.state.prepared": "interrupted while preparing",
  "cutover.state.verified": "interrupted after verifying",
  "cutover.state.switched": "interrupted after switching",
  "cutover.state.completed": "completed",
  "cutover.state.rolled_back": "rolled back",
  "cutover.state.failed": "failed",
  "cutover.state.hint.detected":
    "No cutover has been run from this record; nothing on this page has changed a client's configuration.",
  "cutover.state.hint.completed":
    "A cutover completed. There is no rollback from this state; the snapshot stays on disk.",
  "cutover.state.hint.rolled_back":
    "Rolled back and the captured files were restored; starting again uses the original file.",
  "cutover.state.hint.prepared":
    "The new configuration is staged and the client file not yet written; the snapshot is the only copy of the original.",
  "cutover.state.hint.verified":
    "Staged and verified, the client file not changed; the snapshot is the only copy of the original.",
  "cutover.state.hint.switched":
    "Switched and the client file was replaced; restoring the snapshot is the only way back.",
  "cutover.state.hint.failed": "Failed partway; roll back from the snapshot below.",
  "cutover.snapshot.none": "No rollback snapshot has been captured.",
  "cutover.snapshot.files": "{n} file(s) captured, restorable byte for byte.",
  "cutover.not_durable":
    "This build's migration record is not durable; an interrupted cutover cannot be recovered after a restart.",
  "cutover.persist_error": "The record is not reaching disk: {err}",
  "cutover.plans.section": "Cutover plans",
  "cutover.plans.hint":
    "Each plan is built from the client's real configuration; nothing changes until you confirm.",
  "cutover.none":
    "No external client is known to this build; install and run one, then refresh this page.",
  "cutover.load_failed":
    "The migration record could not be read: {err}; nothing on this page has changed a client's configuration.",
  "cutover.step.not_reversible": "cannot be undone",
  "cutover.step.reversible": "restored by a rollback",
  // The plan's own sentences, in the reader's language. The backend sends the
  // values each one is built from, because a sentence that names a client, a
  // port, a file or an endpoint cannot be translated without them — and a
  // Chinese table with English sentences down the middle of it is a page that
  // reads as half-made.
  "cutover.step.stop_external": "check whether {name} is still running",
  "cutover.step.start_zroutery": "check that port {port} is free",
  "cutover.step.copy_config": "install the staged document over {path}",
  "cutover.step.verify_endpoint": "confirm {url} answers as Zroutery",
  // The two column headers of the plan table. The first says what the column
  // holds — whether a rollback puts the step back — and the second names the
  // step. Both used to repeat the section's own title, which is how a table
  // ends up with two headers that say the same thing and neither of them
  // names its column.
  "cutover.table.undo": "Undo",
  "cutover.table.step": "Step",
  "cutover.run": "Run this cutover",
  "cutover.running": "Running…",
  "cutover.rollback": "Roll back",
  "cutover.rolling_back": "Rolling back…",
  "cutover.already_run":
    "This record allows one cutover and has already run; to start over, delete it from the config directory.",
  "cutover.rollback_unavailable":
    "Rollback is offered only after a failed run.",
  "cutover.confirm_run_title": "Replace {agent}'s configuration?",
  "cutover.confirm_run_body":
    "{n} step(s); the file is backed up before writing so a failed run can be undone, and a running external process is reported, not stopped.",
  "cutover.confirm_rollback_title": "Put the configuration file back?",
  "cutover.confirm_rollback_body":
    "{n} file(s) will be overwritten with the bytes captured before the cutover; later edits and the cutover's changes are lost.",
  "cutover.result.title": "Last run",
  "cutover.result.steps": "{done} of {total} step(s)",
  "cutover.result.duration": "{ms} ms",
  "cutover.result.errors": "Errors",
  "cutover.result.warnings": "Warnings",
  "cutover.result.restored": "Restored {n} file(s)",
  "cutover.result.restore_failed":
    "{n} file(s) could not be restored and the configuration on disk differs; fix them by hand.",
  "cutover.result.rolled_back_nothing":
    "Nothing was restored; the state advanced but the files on disk are unchanged.",
  "cutover.result.ok": "The cutover finished and the endpoint answered as Zroutery.",
  "cutover.toast_applied": "Cutover finished for {agent}.",
  "cutover.toast_failed":
    "The cutover failed with {n} error(s); the report names the step that stopped it, and rolling back restores the files.",
  "cutover.toast_rolled_back": "Restored {n} file(s).",
  "cutover.toast_nothing_restored": "Nothing was restored.",
  "cutover.agent.claude": "Claude Code",
  "cutover.agent.codex": "Codex CLI",
  "cutover.agent.opencode": "OpenCode",
  "cutover.agent.gemini": "Gemini CLI",

  // ------------------------------------------------------------- routing
  "routing.lede": "How requests are routed, by purpose; works with any Anthropic or OpenAI client.",
  "routing.default_hint": "Every ordinary request, by the tier the client asks for.",
  "routing.edit": "Edit",
  "tier.hint.fast": "short tasks, response first",
  "tier.hint.standard": "everyday questions and coding",
  "tier.hint.reasoning": "multi-step analysis and complex coding",
  "tier.hint.frontier": "your pool for demanding tasks",
  "overview.setup_path": "Connect your workflow",
  "overview.setup_hint": "Manage a provider, assign models, then connect your client.",
  "tier.name.fast": "Fast",
  "tier.name.standard": "Standard",
  "tier.name.reasoning": "Reasoning",
  "tier.name.frontier": "Frontier",
  "routing.tier_empty": "Empty; requests for {id} fail until a model joins this tier.",
  "routing.fallback_n": "fallback {n}",
  "routing.more_fallbacks": "{n} more fallbacks",
  "routing.request": "request",
  "routing.side_query": "Auto review",
  "routing.review_hint_on": "Permission checks go to their own pool of models.",
  "routing.review_hint_off": "Off; permission checks follow Default routing.",
  "routing.verdict": "verdict",
  "routing.next": "next",
  "routing.review_no_candidate": "Enabled, but no candidate is live; permission checks will fail.",
  "routing.review_off": "Off; turning it on sends permission checks to the pool below.",
  "routing.default_drawer": "Default routing",
  "field.strategy": "Strategy",
  "strategy.priority": "Priority",
  "strategy.weighted_random": "Weighted random",
  "strategy.round_robin": "Round robin",
  "strategy.lowest_latency": "Lowest latency",
  "strategy.balanced": "Balanced (elected)",
  "field.max_attempts": "Max attempts",
  "field.max_attempts_hint": "Upstream tries per request",
  "field.unknown_ids": "Unknown model ids",
  "routing.unknown_404": "Return 404",
  "routing.unknown_serve": "serve from {id}",
  "routing.failover": "Fail over inside a tier",
  "routing.failover_hint": "Try the next model when one errors",
  "routing.claude_names": "Understand Claude model names",
  "routing.claude_names_hint": "claude-*-sonnet-* maps onto the matching tier",
  "routing.aliases_note":
    "Everything else must be an exact id, or one of the client aliases below.",
  "routing.election": "Election",
  "routing.election_hint":
    "One tiny request per model measures latency and price, then pins the order.",
  "routing.rerun": "Re-run now",
  "routing.elect_on_start": "Hold one at startup",
  "routing.no_election": "No election has been held this run.",
  "routing.no_election_tiers": "No enabled model was available to measure.",
  "routing.primary": "primary",
  "routing.route_review": "Route permission checks",
  "routing.route_review_hint":
    "Permission checks go to the pool below instead of the conversation's model; covers Claude Code Auto Mode and Codex Guardian.",
  "routing.review_failover": "Fail over inside the pool",
  "routing.review_failover_hint": "Try the next candidate when one errors",
  "routing.pool": "Pool",
  "routing.pool_hint":
    "Models come from the Models page; provider, key and price settings are reused.",
  "routing.no_candidates": "No candidates yet. Pick a model below.",
  "routing.not_configured": "not a configured model",
  "routing.decision_candidate": "Decision model",
  "routing.add_jev": "Add OpenCode Zen's free Jev",
  "routing.add_jev_hint":
    "Adds the keyless Jev decision model to the top of the pool with a threshold of 0.6; the free offer can end, and the text being checked is sent to Zen.",
  "routing.jev_added": "Jev was added first in the pool, and Auto review is on.",
  "routing.enable_for": "Enable {model}",
  "routing.add_model": "Add a model…",
  "routing.behaviour": "Behaviour",
  "routing.detection": "Detection",
  "routing.automatic": "automatic",
  "routing.verdict_parsing": "Verdict parsing",
  "routing.verdict_only": "<block>yes|no</block> only",
  "routing.verdict_only_hint":
    "Claude Code wants <block>yes|no</block> and Codex Guardian wants an `outcome` field in JSON; a decision model only gives a probability, and the provider's threshold turns it into a verdict.",
  "routing.no_verdict": "When no verdict",
  "routing.fails_closed": "fails closed; the client asks the user",
  "routing.fail_closed_note":
    "Errors, timeouts and unsure verdicts all count as blocked, never allowed; prose in a reply is never read as permission.",

  // ------------------------------------------------------------ activity
  "activity.lede_none": "Nothing yet. Point a client at the gateway to see traffic here.",
  "activity.lede_since": "Totals since {since}.",
  "activity.lede_imported": "This device since {since}; imported history starts {imported}.",
  "activity.lede_imported_only": "No requests on this device yet; imported history starts {imported}.",
  // The headline figures. Two of the five drive the traffic chart's measure;
  // their labels are the chart's own metric words ("Requests", "Tokens",
  // "Cache" — the cache tokens are a series, while the hit rate itself is a
  // ratio and drives nothing).
  "activity.kpi_row": "Usage totals",
  "activity.kpi_success": "Success rate",
  "activity.kpi_cache": "Cache hit",
  "activity.kpi_cache_help":
    "cache-read tokens ÷ input tokens, as the backend aggregates them. Input is cache-inclusive, so the share never passes 100%.",
  // The spend cell's hover caveat: these requests' cost is wholly or partly
  // unknown, so the shown total is a floor, not the whole story.
  "activity.kpi_unpriced": "{n} unpriced requests",
  // ---------------------------------------------------------- activity charts
  "activity.traffic": "Traffic",
  "activity.traffic_hint": "Completed requests per local day; days without requests count as zero.",
  "activity.traffic_hint_imported": "Completed requests per local day, this device plus imported history; days without requests count as zero.",
  "activity.usage_source": "Usage source",
  "activity.source_local": "This device",
  "activity.source_standby": "Development standby",
  "activity.choose_standby_db": "Choose standby database",
  "activity.standby_not_found": "No standby usage database found. Choose its runtime.sqlite3 file.",
  "activity.standby_selected": "Standby database: {path}",
  "activity.source_tag": "{source}",
  "activity.external_read_only": "Standby history is read-only here. Clear, import and export are available only for this device.",
  "activity.standby_load_failed": "Standby usage could not be read: {error}",
  // ------------------------------------------------- imported history (other tools)
  "activity.import_source": "Import from another tool…",
  "activity.imported_history": "Imported history",
  "activity.import_source_title": "Import history from another tool",
  "activity.import_source_hint": "Choose a source, review its history, then add it to your usage. Importing does not change your current budget.",
  "activity.import_step_source": "Source",
  "activity.import_step_review": "Review",
  "activity.import_step_done": "Result",
  "activity.import_reading": "Reading history and preparing your preview…",
  "activity.import_publishing": "Saving the confirmed history…",
  "activity.import_snapshot_ready": "This preview is saved. Confirm now or continue here later; source changes will not change this batch.",
  "activity.import_cancelled": "Import cancelled. No history was added.",
  "activity.import_resume": "Continue a saved preview",
  "activity.import_instance": "Source copy name",
  "activity.import_instance_default": "Use the file or folder name",
  "activity.import_from": "From date (optional)",
  "activity.import_until": "Through date (optional)",
  "activity.import_bad_range": "The end date must not be before the start date.",
  "activity.inbound_api": "Client → Zroutery",
  "activity.outbound_api": "Zroutery → upstream",
  "activity.attempt_route": "Execution attempts",
  "activity.attempt_unavailable": "This record has no per-attempt protocol evidence.",
  "activity.planned_model": "Configured request model",
  "activity.declared_model": "Response model field",
  "activity.declared_version": "Response version field",
  "activity.model_missing": "Not reported / unknown",
  "activity.model_auto_only": "{value} · automatic selection, model unknown",
  "activity.model_conflict": "Conflicting declarations: {value}",
  "activity.declaration_hint": "Names come from structured upstream fields. They are provider declarations, not independent verification of the backend.",
  "activity.import_source_copy": "Source copy: {name} ({source}). Re-importing updates instead of counting twice.",
  "activity.import_found": "Found on this machine",
  "activity.import_recent": "used before",
  "activity.import_pick_file": "Choose database file…",
  "activity.import_pick_folder": "Choose transcripts folder…",
  "activity.import_preview_requests": "Request rows",
  "activity.import_preview_aggregates": "Day totals",
  "activity.import_preview_suspected": "Suspected duplicates",
  "activity.import_preview_unpriced": "Unpriced",
  "activity.import_preview_days": "Days covered",
  "activity.import_confirm": "Import {n} rows",
  "activity.import_report": "Stored {inserted} new rows, updated {updated}; {unchanged} were already stored.",
  "activity.import_error": "Import failed: {error}",
  "activity.imported_rows": "Imported {n} rows.",
  "activity.import_clear": "Clear imported history",
  "activity.import_clear_body": "Removes every imported fact. Your own usage, spend, wallet and health are unaffected.",
  "activity.import_cleared": "Removed {n} imported rows.",
  "activity.imported_totals": "{requests} of these imported · {unpriced} unpriced · {suspected} suspected",
  "activity.chart_style": "Chart",
  "activity.chart_heatmap": "Heatmap",
  "activity.chart_bars": "Bars",
  "activity.chart_line": "Line",
  "activity.chart_metric": "Measure",
  "activity.metric_requests": "Requests",
  "activity.metric_tokens": "Tokens",
  "activity.metric_cache": "Cache",
  "activity.chart_range": "Range",
  "activity.range_7": "7 days",
  "activity.range_30": "30 days",
  "activity.range_90": "90 days",
  "activity.range_365": "1 year",
  "activity.chart_reading": "{metric} · {span} · total {total}",
  "activity.chart_none": "No requests in this window yet.",
  "activity.chart_aria": "{metric} per day over the last {days} days, {total} in total.",
  "activity.heat_less": "Less",
  "activity.heat_more": "More",
  "activity.reset": "Reset counters",
  "activity.review_line":
    "Auto review: {n} checks · {failed} failed · avg {latency} (separate from main traffic).",
  "activity.nothing": "Nothing yet.",
  "activity.filter": "Filter requests",
  "activity.filter_all": "All",
  "activity.filter_failed": "Failed only",
  "activity.no_failures": "No failed requests in this window.",
  /*
   * The pager's status line is shared by both pagers — the Activity log's and
   * the Models list's — so the sentence stays generic. The zh dictionaries
   * never named the rows ("第 {from}–{to} 条 · 共 {total} 条"); the English
   * one drops "Requests" too, where it would have been false on the Models
   * page, whose rows are models.
   */
  "activity.page_status": "{from}–{to} of {total}",
  "activity.prev_page": "Previous",
  "activity.next_page": "Next",
  "activity.tab_group": "Activity subjects",
  "activity.tab_usage": "Requests and usage",
  "activity.tab_log": "Internal log",
  "activity.log_section": "Internal log",
  "activity.log_hint": "The process's own output, followed while this pane is open",
  "activity.show_log": "Show the internal log",
  "activity.stop_follow": "Stop following",
  "activity.ok_failed": "{ok} ok / {failed} failed",
  "activity.cooling": "cooling {n}s",
  "activity.rate_cooling": "rate-limit cooldown {n}s",
  "activity.spend": "Spend",
  "activity.estimated": "estimated spend",
  "activity.n_failed": "{n} failed",
  "activity.client_unknown": "Unknown client",
  "activity.minor_clients": "Other stray clients ({n})",
  "activity.request": "Request",
  "activity.review": "Review",
  "activity.asked_for": "Asked for",
  "activity.answered_by": "Answered by",
  "activity.kind": "Kind",
  "activity.result": "Result",
  "activity.attempts": "Attempts",
  "kind.main_kv": "main",
  "kind.check_kv": "permission check",
  "activity.timing": "Timing",
  "activity.total": "Total",
  "activity.first_token": "First token",
  "activity.streamed": "Streamed",
  "activity.usage": "Usage",
  "activity.input_tokens": "Input tokens",
  "activity.output_tokens": "Output tokens",
  "activity.cache_read": "Cache read",
  "activity.cache_write": "Cache write",
  /*
   * The stream row's micro labels: label and value are one string because the
   * row has no table header to carry the meaning — "ttft 412 ms" has to read
   * on its own. "tok/s" needs no key: a unit symbol, like `ms`, not a word.
   */
  "activity.ttft": "ttft {v}",
  "activity.cache_hit_rate": "Cache hit rate",
  "activity.cache_hit_hint":
    "Cache reads are a subset of the cache-inclusive input total; the rate is cache read over input, not over fresh tokens.",
  "activity.cost": "Cost",
  "activity.unpriced": "unpriced",
  /*
   * The ledger: one table, two cuts. "Cut" is the section's own word — the
   * same recorded requests summed by client or by model — and the hint
   * carries the three facts a reader needs before they trust the table:
   * clicking a row filters the stream, spend is an estimate, and nothing
   * said or answered is ever stored.
   */
  "activity.ledger": "Ledger",
  "activity.ledger_hint":
    "Every recorded request, cut by client and by model; click a row to show only its requests in the stream below. Spend is estimated from configured prices; prompts and completions are never stored.",
  "activity.ledger_cut": "Ledger dimension",
  "activity.ledger_local_only": "This split counts this device's live ledger. Imported history is in the totals and the curve above, and that store carries no client or kind evidence to divide it by.",
  "activity.by_client": "By client",
  "activity.by_model": "By model",
  "activity.health_col": "Health",
  /*
   * The stream's active ledger cut, and its way back. The strip names the
   * one subject the list is now filtered to; "the kept window" because the
   * ledger reaches further back than the request list does.
   */
  "activity.stream_only": "Showing only {subject}",
  "activity.stream_clear": "Show all requests",
  "activity.no_match_window": "No request in the kept window matches this filter.",
  "activity.export_history": "Export history",
  "activity.import_history": "Import history",
  /*
   * The native save path's feedback: the toast and the hint line share the
   * "Exported to {path}" sentence with the path's short form; the full path
   * rides on the title. The failure note covers both an older backend
   * without the save dialog and a dialog that failed to open — either way
   * the copyable archive is the fallback on screen.
   */
  "activity.exported_to": "Exported to {path}",
  "activity.export_file_failed":
    "Could not export to a file; opened the copyable archive instead.",
  "activity.export_hint":
    "A portable JSON archive of the recorded usage, ready to import elsewhere.",
  "activity.import_hint":
    "Paste the archive produced by Export history; one invalid row rejects the whole file.",
  "activity.import_placeholder": "Paste the JSON archive here",
  "activity.client": "Client",
  "activity.clear_client": "Clear client",
  "activity.clear_client_body":
    "Delete every stored usage record for {name}? Other clients and the live counters stay untouched.",
  "activity.clear_client_named": "Clear usage history for {name}",
  /*
   * The stream's facet selects: provider first, model cascaded inside it.
   * "Filter by …" names the control, not the choice — the selected option
   * itself is the trigger's description, so the labels below are only the
   * opt-out and never read as the current state.
   */
  "activity.facet_provider": "Filter by provider",
  "activity.facet_model": "Filter by model",
  "activity.facet_all_providers": "All providers",
  "activity.facet_all_models": "All models",

  // ------------------------------------------------------------ settings
  "settings.lede": "Application behaviour, the gateway it serves, and what it is allowed to spend.",
  "settings.gateway": "Gateway",
  "settings.gateway_hint": "Serving {url} · {path}",
  "field.host": "Host",
  "field.host_hint": "127.0.0.1 keeps it on this machine",
  "field.port": "Port",
  "field.body_limit": "Max request body (MiB)",
  "field.body_limit_hint": "Inline images make prompts big",
  "field.log_limit": "Kept requests",
  "field.log_limit_hint": "Size of the in-memory activity log",
  "settings.require_auth": "Require the local token",
  "settings.require_auth_hint": "Strongly recommended",
  "settings.autostart": "Start the gateway on launch",
  "settings.bypass_proxy": "Bypass the system proxy",
  "settings.bypass_proxy_hint": "Needed if a proxy strips the client fingerprint headers",
  "settings.local_token": "Local token",
  "settings.reveal": "Reveal",
  "settings.reveal_title": "Show the token in this window",
  "settings.copy": "Copy",
  "settings.regenerate": "Regenerate",
  "settings.regenerate_title": "Existing clients will need the new token",
  "settings.token_hint": "Send it as x-api-key or Authorization: Bearer.",
  "settings.point_client": "Point a client at the gateway",
  "settings.anthropic_clients": "Anthropic-style clients:",
  "settings.openai_clients": "OpenAI-style clients:",
  "settings.snippet_comment": "# or any exact model id from the Models page",
  "settings.cors": "Browser access (CORS)",
  "settings.cors_hint": "Only needed for clients that run inside a web page.",
  "settings.allow_origins": "Allow browser origins",
  "settings.no_origins":
    "No origins listed, so any origin is accepted; add the ones you actually use.",
  "field.allowed_origin": "Allowed origin",
  "field.allowed_origin_hint": "Scheme, host and port, no path",
  "settings.add_origin": "Add origin",
  "settings.origin_notice": "An origin looks like https://example.com, with no path.",
  "settings.budgets": "Budgets",
  "settings.budgets_hint":
    "A limit stops spending once reached; the request that crosses it completes and the next is rejected.",
  "settings.balances": "Balances",
  "settings.balances_hint":
    "Wallet checks cost one request per provider or account. The last successful figure is kept when a check fails.",
  "settings.balance_auto": "Check balances automatically",
  "settings.balance_auto_hint": "Runs in the background while the app is open.",
  "settings.balance_interval": "Interval (seconds)",
  "settings.balance_interval_hint": "Between 60 and 86400 seconds.",
  "budget.covers": "Covers",
  "budget.window": "Window",
  "budget.limit": "Limit",
  "budget.spent": "Spent",
  "budget.when": "When reached",
  "budget.on": "On",
  "scope.global": "everything",
  "scope.provider": "provider {id}",
  "period.day": "per day",
  "period.month": "per month",
  "budget.reject": "refuse the request",
  "budget.degrade": "serve from {id}",
  "budget.used_up": "used up",
  "budget.readonly": "Budget {scope}: {spent} / {limit} ({used}%)",
  "budget.add": "Add budget",
  "budget.currency_hint": "In the currency your models bill in",
  /*
   * The weights, under a heading of their own inside the balanced-election
   * section. They were a section on their own once and read as a peer of the
   * strategy that gives them meaning; here they are a part of it.
   */
  "settings.election_weights": "How it weighs them",
  /*
   * One section for tier naming and client aliases, because both decide what id
   * a client is allowed to ask for, and both are read by every page that
   * renders a virtual id.
   */
  "settings.tier_naming_section": "What a client may ask for",
  "settings.naming_style_hint":
    "How tiers are named everywhere in this app, not only in routing",
  "settings.tab_group": "Settings subjects",
  "settings.tab_general": "General",
  "settings.tab_gateway": "Gateway",
  "settings.tab_budgets": "Budgets",
  "settings.about": "About",
  "settings.about_note":
    "Zroutery stores everything it runs on in this file; changes made in the app are written back to it.",
  "settings.version": "Version",
  "settings.config_path": "Configuration file",
  "settings.aliases": "Client model aliases",
  "settings.aliases_hint":
    "Maps an exact model id a client sends onto a tier; wins over the Claude name heuristic.",
  "settings.f_model_id": "Model id",
  "settings.f_class": "Tier",
  "settings.add_alias": "Add alias",
  "settings.price_weight": "Price weight",
  "settings.price_weight_hint": "Relative, so 3 and 1 means the same as 0.75 and 0.25",
  "settings.latency_weight": "Latency weight",
  "settings.ref_input": "Reference input tokens",
  "settings.ref_input_hint":
    "Prices are per million tokens, so they need a request size to be compared on.",
  "settings.ref_output": "Reference output tokens",
  "settings.naming_style": "Tier naming",
  "settings.naming_internal": "Internal",
  "settings.naming_anthropic": "Anthropic",
  "settings.naming_openai": "OpenAI",
  "settings.window": "Window behaviour",
  "win.launch_on_login": "Launch at login",
  "win.launch_on_login_hint": "Start Zroutery automatically when you sign in",
  "win.silent_start": "Start silently",
  "win.silent_start_hint": "No main window on launch; the tray is the only presence",
  "win.keep_in_tray": "Keep in tray on close",
  "win.keep_in_tray_hint":
    "On: closing the window keeps Zroutery in the tray. Off: closing quits and stops the gateway.",
  "win.window_actions": "Hide or quit",
  "win.window_actions_hint":
    "Hide keeps the gateway running in the tray; quit stops the gateway and ends the process",
  "win.hide_to_tray": "Hide to tray",
  "win.quit": "Quit",
  "vision.title": "Vision fallback",
  "vision.hint":
    "A model without vision receives a description of the image, or a placeholder when none can be produced; images are never dropped silently.",
  "vision.enable": "Describe images for non-vision models",
  "vision.model": "Vision model",
  "vision.model_hint":
    "An existing model that can see, from the Models page; its provider and pricing are reused",
  "vision.model_required": "Pick a model that supports vision on the Models page first.",
  "vision.placeholder": "Placeholder",
  "vision.placeholder_hint": "What replaces an image when no description was possible",
  // --------------------------------------------------------- request repair
  "rectifier.title": "Request repair",
  "rectifier.hint":
    "When an upstream rejects a request, send it again to the same provider and model in a form it accepts; repairs do not count against the circuit breaker and never switch providers. These are the global defaults; each provider can override them in its own drawer.",
  "rectifier.enable": "Repair rejected requests",
  "rectifier.enable_hint": "Master switch: turning it off leaves every request to fail over at once.",
  "rectifier.thinking_signature": "Repair a stale thinking signature",
  "rectifier.thinking_signature_hint":
    "Anthropic rejects a request whose thinking block carries an old or misplaced signature; those blocks are dropped and the request is sent again.",
  "rectifier.media_fallback": "Replace an image with a description",
  "rectifier.media_fallback_hint":
    "Providers that refuse images receive a text description instead; the image itself is kept.",
  "rectifier.thinking_budget": "Halve an oversized thinking budget",
  "rectifier.thinking_budget_hint":
    "A provider that caps the reasoning budget gets the request with the budget halved (at least 1024 tokens) so it fits.",
  "rectifier.codex_auto": "Codex conditional history repair",
  "rectifier.codex_auto_hint":
    "History is kept and only errors the upstream names explicitly get cleaned; the reply is delivered complete, so visible output may be delayed.",
  "rectifier.codex_metadata": "Strip Codex's private metadata field",
  "rectifier.codex_metadata_hint":
    "Only removed and the request retried when an upstream rejects the field by name.",
  "rectifier.thinking_fallback": "Allow a model without thinking support",
  "rectifier.thinking_fallback_hint":
    "A reasoning request may go to a model that does not declare thinking support, with the reasoning controls removed. The attempt is marked degraded.",
  // ------------------------------------------------------------ learned routing
  "ml.title": "Learned routing",
  "ml.hint":
    "When on, the trained model may re-order the provider plan; off keeps routing deterministic.",
  "ml.unavailable": "This build cannot learn; routing stays deterministic.",
  "ml.serving": "A promoted model is re-ordering the provider plan.",
  "ml.inert": "A model is promoted but learned routing is off, so it is not consulted.",
  "ml.off": "No model is installed; routing stays deterministic.",
  "ml.exploring": "exploring {pct}%",
  "ml.no_state_dir":
    "No durable state directory is configured, so nothing survives a restart and no model can be promoted.",
  "ml.installed": "Installed",
  "ml.none": "none",
  "ml.gate": "Authorised by",
  "ml.baseline": "beating {name}",
  "ml.paired": "{n} paired requests",
  "ml.holdout_loss": "Holdout loss",
  "ml.ranking": "Ranked / fell back",
  "ml.explored": "Explored / ranked",
  "ml.blind_explored": "Tried with no model",
  "ml.with_no_model": "with no model, to gather evidence",
  "ml.never_tried": "Never tried",
  "ml.blind_spot_warning":
    "With exploration off, newly configured models get no traffic; give them top priority, or turn exploration on (which then blocks learned-model promotion).",
  "ml.blind_spot_count": "{n} unreachable",
  "ml.collected": "Collected",
  "ml.samples": "samples",
  "ml.traces": "traces",
  "ml.criteria": "Gate criteria",
  /*
   * The nine promotion criteria, by the stable identifier the gate reports.
   *
   * `PromotionCriterion::name` is documented as stable "so a decision can be
   * reasoned about by name", which makes it a translation key rather than a
   * display string — so these are looked up by id and an id this build does not
   * know falls back to itself, which is how a criterion added by a newer backend
   * stays visible instead of rendering as a blank row.
   */
  "ml.criterion.baseline_present": "Baseline paired",
  "ml.criterion.paired_evidence": "Paired evidence",
  "ml.criterion.holdout_quality": "Holdout quality",
  "ml.criterion.routing_utility": "Routing utility",
  "ml.criterion.selection_safety": "Selection safety",
  "ml.criterion.success_regression": "Success regressions",
  "ml.criterion.cost_regression": "Cost regression",
  "ml.criterion.latency_regression": "Latency regression",
  "ml.criterion.dataset_identity": "Dataset identity",
  /*
   * The figure a criterion was decided on. ADR-0007 asks for every criterion
   * "with the number it was decided on", so the two structured fields are
   * rendered rather than left inside the gate's English prose.
   *
   * Labelled rather than written as "measured >= threshold": the direction is a
   * property of each criterion (a cost budget is a ceiling, an evidence floor is
   * not) and asserting one direction for all nine would misstate six of them.
   */
  "ml.criterion_value": "{measured} measured / {threshold} required",
  "ml.history": "Promotion history",
  "ml.promoted": "promoted",
  "ml.rolled_back": "rolled back",
  "ml.replay": "Replay over my traffic",
  "ml.replaying": "Replaying…",
  "ml.replay_none": "The replay produced no measurement.",
  "ml.replay_records": "Records replayed",
  "ml.replay_bound":
    "Over the most recent {n} records; more history is available beyond this window.",
  "ml.agreement": "Agreed with production",
  "ml.disagreements": "Disagreements / measured",
  "ml.measured": "measured",
  "ml.regret": "Mean regret",
  "ml.utility_delta": "Mean observed utility delta",
  "ml.alternatives": "Alternatives",
  "ml.harmful": "harmful",
  "ml.helpful": "helpful",
  "ml.gaps": "Unusable records:",
  "ml.rollback_action": "Roll back to the previous model",
  "ml.rolling_back": "Rolling back…",

  "ml.config_title": "Collection settings",
  "ml.config_hint": "Shadow produces the samples; no samples, no learning.",
  "ml.config_unavailable": "This build cannot learn, so there is nothing to configure.",
  "ml.shadow_enable": "Shadow evaluation",
  "ml.shadow_enable_hint": "When off, no training samples are collected.",
  "ml.shadow_max_decisions": "Max decisions (in memory)",
  "ml.shadow_max_decisions_hint":
    "The oldest decisions are dropped beyond this; 10000 is the default.",
  "ml.shadow_max_age": "Max age (seconds)",
  "ml.shadow_max_age_hint": "Retention window; 604800 = 7 days.",
  "ml.routing_enable": "Learned routing",
  "ml.routing_enable_hint":
    "When on, the promoted model may re-order the plan; promotion needs a state directory first.",
  "ml.state_dir": "State directory",
  "ml.state_dir_hint":
    "Where traces and the active model are stored; empty keeps them in memory only, lost on restart, e.g. ml",
  "ml.exploration": "Exploration probability",
  "ml.exploration_hint": "0 = deterministic; >0 explores alternative candidates but blocks promotion.",
  "ml.config_shadow_off_hint": "Shadow is off; no training samples are collected.",
  "ml.config_effect": "Collecting",
  "ml.effect_collecting": "yes — samples grow with traffic",
  "ml.effect_idle": "no — shadow is off",
  "ml.config_serve": "Serving",
  "ml.serve_active": "learned model re-orders the plan",
  "ml.serve_deterministic": "deterministic (no re-ordering)",
  "ml.serve_blocked_hint": "Learned routing is on but no durable state directory is configured, so nothing can be promoted or survive a restart.",

  // ----------------------------------------------- learned routing: collection
  //
  // The state in which the stack is switched on and learning nothing is silent,
  // healthy everywhere, and only visible in these counters. Nothing in the
  // configuration, the status document or the promotion endpoint declares the
  // prerequisite, so the panel that shows the counters has to say it.
  "ml.learning_blocked":
    "Learned routing is on but Shadow evaluation is off, so no samples are collected ({n} requests dropped); turn Shadow evaluation on.",
  "ml.learning_blocked_short": "Learning is blocked",
  "ml.ingested": "Requests stored",
  "ml.dropped_no_decision": "Discarded for want of a decision",

  // -------------------------------------------- learned routing: promotion round
  //
  // Judging and installing are rendered as two buttons on purpose. A single
  // "promote" would collapse the distinction the backend makes load-bearing,
  // and the consequence of getting it wrong is a different model serving every
  // request that follows.
  "ml.round": "Promotion round",
  "ml.round_hint":
    "Train a candidate from the recorded history and let the gate judge it against the baseline; nothing is installed or scheduled automatically.",
  "ml.round_judge": "Judge a candidate",
  "ml.round_judging": "Judging…",
  "ml.round_judged": "Candidate judged, not installed.",
  "ml.round_install": "Install the authorised candidate",
  "ml.round_installing": "Installing…",
  "ml.round_install_hint":
    "Available once the gate authorizes a candidate; changes which model serves later requests.",
  "ml.round_none": "No round could be run.",
  "ml.round_records": "Records read",
  "ml.round_candidate": "Candidate",
  "ml.round_choices": "What the candidate would have chosen",
  "ml.round_single_choice":
    "The candidate chose the same option on every request, so it is not a learned ranking.",
  "ml.round_installed": "Installed and serving",
  "ml.round_not_installed": "Judged, not installed",
  "ml.round_serving_now": "Serving now",
  "ml.round_deterministic": "Routing is deterministic",
  "ml.verdict": "Verdict",
  "ml.verdict_promoted": "promoted",
  "ml.verdict_rejected": "rejected",
  "ml.verdict_blocked": "blocked",
  "ml.round_error": "The round was judged but could not be installed",

  // ------------------------------------------------ learned routing: the log
  //
  // Deletion is an operator's decision and never automatic: the round trains
  // from the whole log, so a retention policy firing on its own would silently
  // change what the next model learns. The dialog states that consequence.
  "ml.log": "Recorded history",
  "ml.log_hint":
    "The evidence rounds train from; retention is manual and nothing changes the next model on its own.",
  "ml.log_records": "Records",
  "ml.log_size": "On disk",
  "ml.log_path": "File",
  "ml.log_unreadable": "The log could not be counted, so this is not a count of zero.",
  "ml.log_appended": "Appended",
  "ml.log_refused": "Refused",
  "ml.log_io_errors": "Write errors",
  "ml.log_empty": "No history has been recorded yet.",
  "ml.log_clear": "Delete recorded history",
  "ml.log_clearing": "Deleting…",
  "ml.log_cleared": "Deleted {bytes} of recorded history.",
  "ml.log_clear_nothing": "There was nothing recorded to delete.",
  "confirm.clear_traces": "Delete recorded history",
  "confirm.clear_traces_body":
    "The {n} recorded requests are deleted permanently. Later rounds train only on records written after this.",
  "bytes.value": "{n} bytes",
} as const;

export type TKey = keyof typeof en;
export type Dict = Record<TKey, string>;

