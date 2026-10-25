/**
 * Localisation: a typed dictionary, nothing more.
 *
 * Every user-visible string lives here under a structured key; the English
 * dictionary defines the key set and TypeScript checks the Chinese one
 * against it, so the two cannot drift apart silently. Plurals use the
 * "singular | plural" pipe form; interpolation uses {name} placeholders.
 *
 * The language follows the system by default and is remembered in local
 * storage. Model ids, provider names, URLs and numbers are data, never
 * translated.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";

export type Lang = "en" | "zh";

export const en = {
  // ------------------------------------------------------------ shared
  "common.dismiss": "Dismiss",
  "common.cancel": "Cancel",
  "common.loading": "Loading…",
  "common.remove": "Remove",
  "common.delete": "Delete",
  "common.clear": "Clear",
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
  "nav.overview": "Overview",
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
  "appearance.title": "Appearance",
  "theme.system": "Follow system",
  "theme.light": "Light",
  "theme.dark": "Dark",
  "gw.address": "Address",
  "gw.copy_address": "Copy address",
  "gw.stop": "Stop gateway",
  "gw.start": "Start gateway",
  "settings.language": "Language",
  "settings.theme": "Theme",
  "lang.zh": "简体中文",
  "lang.en": "English",
  "action.hide": "Hide",
  "toast.copied": "Copied.",
  "toast.queued_discarded":
    "The save failed, so edits queued behind it were dropped.",

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
  "accounts.title_hint":
    "Accounts hosted by this provider. They feed probing, balances and check-in; requests still use the provider's key.",
  "accounts.empty": "No accounts declared. Every request uses the provider's own key.",
  "accounts.lede":
    "Balance, check-in and panel token for each declared account. Declarations are edited in the provider's account section.",
  "accounts.add": "Add account",
  "accounts.id_label": "Account ID",
  "accounts.id_hint":
    "Stable name within this provider. The (provider, account) pair identifies its runtime state.",
  "accounts.key_label_add": "Panel access token (optional)",
  "accounts.key_hint_add":
    "Optional. Empty uses the provider's key. For a NewAPI panel, paste the personal access token; an sk- relay key will not work.",
  "accounts.duplicate": "Account `{id}` already exists on this provider.",
  "accounts.key_store_failed": "The account was saved, but storing its key failed.",
  "accounts.set_key": "Set credential",
  "accounts.key_label": "Panel access token",
  "accounts.key_hint":
    "Personal access token from the panel (personal settings → access token). Stored in the system credential store and never shown again.",
  "accounts.clear_key": "Clear credential",
  "accounts.clear_key_title": "Clear account {id}'s credential",
  "accounts.clear_key_body":
    "The account uses the provider's key again. Its stored secret is deleted unless another account shares it.",
  "accounts.own_key": "Own credential",
  "accounts.no_key": "Credential not stored",
  "accounts.follows_provider": "Provider's credential",
  "accounts.remove_title": "Remove account {id}",
  "accounts.remove_body":
    "Removes the declaration and its stored key. Observed data remains until the id is reused.",
  "accounts.checkin": "Browser check-in",
  "accounts.login_path": "Panel sign-in path",
  "accounts.login_path_hint":
    "Used by the browser token capture. Current NewAPI signs in at /sign-in; older forks use /login.",
  "accounts.panel_url": "Panel address",
  "accounts.new_api_user": "Panel user ID",
  "accounts.new_api_user_hint":
    "The New-Api-User header some panels demand (AgentRouter does). Filled automatically by the browser token capture; empty sends none.",
  "accounts.panel_url_hint":
    "The dashboard's own root when it lives on a different host than the relay endpoint (e.g. relay on api.relay.com, panel on panel.relay.com). Empty means same host. Token capture and account probes are anchored here.",
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
  "accounts.checkin_path_hint": "Panel-relative path, e.g. /checkin. No default is set.",
  "accounts.checkin_selector": "Check-in button selector",
  "accounts.checkin_selector_hint": "CSS selector the browser clicks. No default on purpose.",
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

  // -------------------------------------------------------------- models
  "models.pick_provider_name": "Pick a provider and fill in the model name.",
  "models.duplicate": "That provider already offers “{model}”.",
  "models.unclassified":
    "{n} models have no tier: {ids}. They stay callable by exact id but do not participate in *-class routing.",
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
  "models.f_display_hint": "Shown in /v1/models",
  "models.f_priority": "Priority",
  "models.f_priority_hint": "Lower wins inside a tier",
  "models.f_weight": "Weight",
  "models.f_weight_hint": "Tie breaking among equal priorities",
  "models.f_max_tokens": "Max output tokens",
  "models.f_max_tokens_hint": "Caps what clients may ask for",
  "models.f_context_window": "Context window",
  "models.f_context_window_hint":
    "Context the model accepts, when known. Advertised in /v1/models; the upstream remains authoritative.",
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
  "models.effort_levels_hint": "Variant levels the takeover offers for this model's classes. Filled from models.dev on import; correct them here when a model differs.",
  "models.sync": "Sync metadata",
  "models.syncing": "Syncing…",
  "models.synced": "Filled missing metadata for {n} models",
  "models.sync_none": "Every configured model already has its metadata",
  "models.sync_failed": "Could not read the catalogue for {names}",
  "models.price": "Price per million tokens",
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
  "routing.priority_for": "Priority for {model}",
  "routing.move_up": "Move {model} up",
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
  "providers.duplicate": "A provider called “{id}” already exists.",
  "cc.source_hint":
    "Read from {path}. Keys are stored in the OS credential store, never the config file.",
  "cc.reload": "Reload",
  "cc.import": "Import",
  "cc.empty": "No Claude Code providers found in CC Switch.",
  "cc.active": "active",
  "cc.already": "already here",
  "cc.imported_notice":
    "Imported {n} providers. Keys went straight into the credential store.",
  "providers.endpoint": "Endpoint",
  "providers.dialect": "Dialect",
  "providers.auth": "Authentication",
  "providers.no_key": "no key stored",
  "providers.no_cred": "no credential needed",
  "providers.models_count": "{n} on this provider",
  "providers.models_none": "none yet",
  "providers.api_key": "API key",
  "providers.api_key_hint": "Stored in the OS credential store, never the config file.",
  "providers.save_key": "Save key",
  "providers.models_section": "Models",
  "providers.empty_catalogue": "{name} returned an empty model list.",
  "providers.configuration": "Configuration",
  "field.timeout": "Timeout (s)",
  "field.timeout_hint": "Whole request, including streaming idle gaps",
  "field.connect_timeout": "Connect timeout (s)",
  "providers.f_version": "anthropic-version",
  "providers.f_version_hint":
    "The dated API version, a mutually exclusive wire contract. 2023-06-01 is the three-year baseline. 2026-10-21 removes temperature/top_p/top_k and confines documents to user messages — requests are shaped accordingly.",
  "providers.f_beta": "anthropic-beta",
  "providers.f_beta_hint":
    "Beta capabilities to request, comma or semicolon separated (e.g. context-management-2025-06-27). Empty sends none. A new Anthropic capability ships as a beta header before it reaches a dated version.",
  "providers.api_surface": "API surface",
  "providers.api_surface_hint":
    "Which OpenAI protocol this endpoint answers. Responses is the newer surface some Codex-shaped gateways require; request, response and stream all differ.",
  "providers.surface_chat": "Chat Completions",
  "providers.surface_responses": "Responses",
  "providers.client_profile": "Client identity",
  "providers.client_profile_hint":
    "Who Zroutery presents as upstream. Claude Code and Codex also forward that client's own headers.",
  "providers.profile_auto": "Auto · detect from the client",
  "providers.profile_native": "Native · no client identity",
  "providers.profile_claude_code": "Claude Code · with its headers",
  "providers.profile_codex": "Codex · with its headers",
  "providers.profile_opencode_cli": "OpenCode CLI · locked for OpenCode providers",
  "providers.client_profile_locked_hint":
    "Locked. OpenCode's gateway checks this fingerprint and the config refuses any other profile.",
  "providers.opencode_zen_dialect": "OpenCode Zen (free)",
  "providers.opencode_console_dialect": "OpenCode Console",
  "providers.decision_dialect": "Decision model",
  "providers.decision": "Decisions",
  "providers.decision_hint":
    "Answers typed questions with a probability instead of text. Used only by Auto review; add its model to the pool by hand. Jev (including OpenCode Zen's free model) is the reference family.",
  "providers.decision_path": "Decision path",
  "providers.decision_path_hint":
    "Path joined to the base URL. The reference service answers at /v1/systemone.",
  "providers.decision_threshold": "Required confidence",
  "providers.decision_threshold_hint":
    "Confidence Auto review needs before acting. At or above it the action is blocked; at or below 1 minus it, allowed; in between, blocked. 0.5 lets the model's majority decide.",
  "providers.decision_no_catalogue":
    "A decision endpoint has no model catalogue. Add its model on the Models page.",
  "models.tier_decision": "Why no tier",
  "models.tier_decision_hint":
    "Decision models serve Auto review only and cannot answer chat, so they have no tier. Add it to the Auto review pool.",
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
    "For relays that read Authorization: Bearer instead of x-api-key; both headers are sent, so either check passes",
  "providers.balance": "Balance",
  "providers.balance_hint":
    "Providers that publish remaining credit at their own endpoint. Pick the endpoint yours uses; the provider key is sent.",
  "providers.balance_hint_panel":
    "A NewAPI relay keeps the wallet in its panel. The figure comes from the panel of this provider's accounts, read with an account's personal access token.",
  "providers.balance_hint_none":
    "This provider publishes no balance and declares no panel account. Pick a probe above, or declare an account with a personal access token.",
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
    "For providers that deviate from the dialect; leave everything off unless a request fails.",
  "quirk.max_completion_tokens": "Use max_completion_tokens",
  "quirk.drop_temperature": "Drop temperature",
  "quirk.drop_top_p": "Drop top_p",
  "quirk.drop_stop": "Drop stop sequences",
  "quirk.drop_stop_hint": "Never applied to Auto review queries",
  "quirk.system_as_developer": "System as developer role",
  "quirk.reasoning_effort": "Translate thinking to reasoning_effort",
  "quirk.portable_history": "Portable Responses history",
  "quirk.portable_history_hint":
    "For relays that may answer from another account or upstream (AgentRouter, CC Switch). Removes reasoning items, history item ids and previous_response_id before forwarding. Prior reasoning no longer reaches the model. Responses requests only.",
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
  /*
   * The Accounts page: the runtime face of every declared account. The split
   * from the provider drawer is declaration (there) versus observation (here),
   * and the lede says the one thing this page does not do — route by account —
   * because an operator who assumed it would otherwise learn it from a
   * misrouted request.
   */
  "nav.accounts": "Accounts",
  "nav.takeover": "Takeover",
  "accounts.empty_build":
    "This build does not contain the account subsystem, so declared accounts cannot be probed or checked in.",
  "accounts.none":
    "No accounts declared. Add one in a provider's Accounts section on the Providers page.",
  "accounts.probe_section": "Account probe",
  "accounts.probe_hint":
    "Ask each enabled account's panel for its current state. One request per account, only when you press the button.",
  "accounts.probe_now": "Probe every account",
  "accounts.probing": "Probing…",
  "accounts.probe_idle":
    "No probe has run yet.",
  "accounts.probe_none":
    "The probe found no enabled declared account.",
  "accounts.probe_outcome": "Outcome",
  "accounts.provider_disabled": "provider disabled",
  "accounts.own_key_stored": "its own access token is stored",
  "accounts.own_key_missing": "declared its own credential, but none is stored",
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
    "Checks in over the panel API with the stored token; no browser. Fails if the panel requires a challenge.",
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
  "account.sync.unsupported":
    "No backend in this build can ask this provider; not probed.",
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
    "Write the gateway address and token into a client's own config, and restore the original values on release. Requests still choose a tier by the model name the client sends.",
  "takeover.none": "No external agent is known to this build.",
  "takeover.load_failed":
    "The takeover record could not be read: {err}. Nothing on this page has changed a client's configuration.",
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
    "Adopting edits {agent}'s config file. Release restores exactly these fields; the rest of the file is untouched.",
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
    "The gateway token changed after this client was taken over. Release and take over again to hand it the new token.",
  "takeover.table.field": "Field",
  "takeover.table.current": "Current value",
  "takeover.table.next": "After adoption",
  "takeover.table.managed": "Zroutery wrote",
  "takeover.generation": "generation {n}",
  "takeover.adopt": "Take over",
  "takeover.adopt_again": "Take over again",
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
    "Move a client's configuration onto this gateway as a verified plan, with the original file captured first so a failed run can be undone. Use Takeover to switch a client on or off; this page is the undo.",
  "cutover.state.section": "Migration record",
  "cutover.state.detected": "nothing run yet",
  "cutover.state.prepared": "interrupted while preparing",
  "cutover.state.verified": "interrupted after verifying",
  "cutover.state.switched": "interrupted after switching",
  "cutover.state.completed": "completed",
  "cutover.state.rolled_back": "rolled back",
  "cutover.state.failed": "failed",
  "cutover.state.hint.detected":
    "No cutover has been run from this record. Nothing on this page has changed a client's configuration.",
  "cutover.state.hint.completed":
    "A cutover completed. There is no rollback from this state; the snapshot stays on disk.",
  "cutover.state.hint.rolled_back":
    "Rolled back; captured files were restored. Starting again uses the original file.",
  "cutover.state.hint.prepared":
    "The document was staged; the client file was not written. The snapshot is the only copy of the original.",
  "cutover.state.hint.verified":
    "Staged and verified; the client file was not changed. The snapshot is the only copy of the original.",
  "cutover.state.hint.switched":
    "Switched; the client file was replaced. Restoring the snapshot is the only way back.",
  "cutover.state.hint.failed":
    "Failed partway. Roll back from the snapshot below.",
  "cutover.snapshot.none": "No rollback snapshot has been captured.",
  "cutover.snapshot.files": "{n} file(s) captured, restorable byte for byte.",
  "cutover.not_durable":
    "This build's migration record is not durable; an interrupted cutover cannot be recovered after a restart.",
  "cutover.persist_error": "The record is not reaching disk: {err}",
  "cutover.plans.section": "Cutover plans",
  "cutover.plans.hint":
    "Each plan uses the client's real config path and this gateway's address. Nothing changes until you confirm.",
  "cutover.none":
    "This build knows no external client to cut over. Install and run one so its configuration file exists, then refresh this page.",
  "cutover.load_failed":
    "The migration record could not be read: {err}. Nothing on this page has changed a client's configuration.",
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
    "This record allows one cutover and has already run. Remove the record from the config directory to start over.",
  "cutover.rollback_unavailable":
    "Rollback is offered only after a failed run.",
  "cutover.confirm_run_title": "Replace {agent}'s configuration?",
  "cutover.confirm_run_body":
    "{n} step(s). The file is captured before writing, so a failed run can be undone. A running external process is reported, not stopped.",
  "cutover.confirm_rollback_title": "Put the configuration file back?",
  "cutover.confirm_rollback_body":
    "{n} file(s) will be overwritten with the bytes captured before the cutover. Later edits and the cutover's changes are lost.",
  "cutover.result.title": "Last run",
  "cutover.result.steps": "{done} of {total} step(s)",
  "cutover.result.duration": "{ms} ms",
  "cutover.result.errors": "Errors",
  "cutover.result.warnings": "Warnings",
  "cutover.result.restored": "Restored {n} file(s)",
  "cutover.result.restore_failed":
    "{n} file(s) could not be restored; the configuration on disk is not what it was. Fix them by hand.",
  "cutover.result.rolled_back_nothing":
    "Nothing was restored. The state advanced but the files on disk are unchanged.",
  "cutover.result.ok": "The cutover finished and the endpoint answered as Zroutery.",
  "cutover.toast_applied": "Cutover finished for {agent}.",
  "cutover.toast_failed":
    "The cutover failed with {n} error(s). The report below names the step that stopped it; roll back to restore.",
  "cutover.toast_rolled_back": "Restored {n} file(s).",
  "cutover.toast_nothing_restored": "Nothing was restored.",
  "cutover.agent.claude": "Claude Code",
  "cutover.agent.codex": "Codex CLI",
  "cutover.agent.opencode": "OpenCode",
  "cutover.agent.gemini": "Gemini CLI",

  // ------------------------------------------------------------- routing
  "routing.lede":
    "How requests are routed, by purpose. Works with any Anthropic or OpenAI client.",
  "routing.default_hint": "Every ordinary request, by the tier the client asks for.",
  "routing.edit": "Edit",
  "tier.hint.fast": "cheap and fast",
  "tier.hint.standard": "the everyday workhorse",
  "tier.hint.reasoning": "the strongest, most expensive tier",
  "tier.hint.frontier": "cutting-edge frontier models",
  "tier.name.fast": "Fast",
  "tier.name.standard": "Standard",
  "tier.name.reasoning": "Reasoning",
  "tier.name.frontier": "Frontier",
  "routing.tier_empty": "Empty. Requests for {id} fail until a model joins this tier.",
  "routing.fallback_n": "fallback {n}",
  "routing.more_fallbacks": "{n} more fallbacks",
  "routing.request": "request",
  "routing.side_query": "Side query",
  "routing.review_hint_on": "Permission checks go to their own pool of models.",
  "routing.review_hint_off": "Off. Permission checks follow Default routing.",
  "routing.verdict": "verdict",
  "routing.next": "next",
  "routing.review_no_candidate": "Enabled, but no candidate is live. Permission checks will fail.",
  "routing.review_off": "Off. Turn on to review permission checks with the pool below.",
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
  "routing.no_election_tiers": "No tier had an enabled model to measure.",
  "routing.primary": "primary",
  "routing.route_review": "Route permission checks",
  "routing.route_review_hint":
    "Permission checks go to the pool below instead of the conversation's model. Covers Claude Code Auto Mode and Codex Guardian.",
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
    "Adds the keyless OpenCode Zen Jev decision model at the top of the pool. Probability threshold 0.6. The free promotion can end or be region-blocked; check text is sent to Zen.",
  "routing.jev_added": "Jev was added first in the pool, and Auto review is on.",
  "routing.enable_for": "Enable {model}",
  "routing.add_model": "Add a model…",
  "routing.behaviour": "Behaviour",
  "routing.detection": "Detection",
  "routing.automatic": "automatic",
  "routing.verdict_parsing": "Verdict parsing",
  "routing.verdict_only": "<block>yes|no</block> only",
  "routing.verdict_only_hint":
    "Claude Code expects <block>yes|no</block>; Codex Guardian expects JSON `outcome`. A decision model answers with a probability, and its provider threshold decides the verdict.",
  "routing.no_verdict": "When no verdict",
  "routing.fails_closed": "fails closed; the client asks the user",
  "routing.fail_closed_note":
    "Errors, timeouts and wrong-protocol answers fail over. If no candidate answers, the query fails rather than approving. An unconfident decision blocks. Prose is never treated as permission.",

  // ------------------------------------------------------------ activity
  "activity.lede_none": "Nothing yet. Point a client at the gateway to see traffic here.",
  "activity.lede": "{requests} requests · {rate}% succeeded · {in} in / {out} out tokens",
  "activity.reset": "Reset counters",
  "activity.review_line":
    "Auto review: {n} checks · {failed} failed · avg {latency} (separate from main traffic).",
  "activity.nothing": "Nothing yet.",
  "activity.health": "Model health",
  "activity.tab_group": "Activity subjects",
  "activity.tab_usage": "Requests and usage",
  "activity.tab_log": "Internal log",
  "activity.log_section": "Internal log",
  "activity.log_hint": "The process's own output, followed while this pane is open",
  "activity.show_log": "Show the internal log",
  "activity.stop_follow": "Stop following",
  "activity.health_hint": "Circuit-breaker state per model, shared by every routing pool.",
  "activity.ok_failed": "{ok} ok / {failed} failed",
  "activity.cooling": "cooling {n}s",
  "activity.clear_health": "Clear the health of {id}",
  "activity.spend": "Spend",
  "activity.spend_hint":
    "Estimated from configured prices and reported usage; not a bill.",
  "activity.estimated": "estimated spend",
  "activity.n_failed": "{n} failed",
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
  "activity.cost": "Cost",
  "activity.unpriced": "unpriced",

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
  "settings.no_origins": "No origins listed, so any origin is accepted. Add the ones you actually use.",
  "field.allowed_origin": "Allowed origin",
  "field.allowed_origin_hint": "Scheme, host and port, no path",
  "settings.add_origin": "Add origin",
  "settings.origin_notice": "An origin looks like https://example.com, with no path.",
  "settings.budgets": "Budgets",
  "settings.budgets_hint":
    "A limit stops spending once reached. The request that crosses it completes; the next is rejected.",
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
  "settings.aliases": "Client model aliases",
  "settings.aliases_hint":
    "Map an exact model id a client sends onto a tier. Wins over the Claude name heuristic.",
  "settings.f_model_id": "Model id",
  "settings.f_class": "Tier",
  "settings.add_alias": "Add alias",
  "settings.price_weight": "Price weight",
  "settings.price_weight_hint": "Relative, so 3 and 1 means the same as 0.75 and 0.25",
  "settings.latency_weight": "Latency weight",
  "settings.ref_input": "Reference input tokens",
  "settings.ref_input_hint": "Prices are per Mtok, so they need a request to be compared on",
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
    "Models without vision receive a description of the image; if none can be produced, a placeholder is used. Images are never silently dropped.",
  "vision.enable": "Describe images for non-vision models",
  "vision.model": "Vision model",
  "vision.model_hint":
    "An existing model that can see, from the Models page; its provider and pricing are reused",
  "vision.model_required": "Pick a model that supports vision on the Models page first.",
  "vision.placeholder": "Placeholder",
  "vision.placeholder_hint": "What replaces an image when no description was possible",
  // ------------------------------------------------------------ learned routing
  "ml.title": "Learned routing",
  "ml.hint":
    "Whether a trained model may re-order the provider plan. Off means routing stays deterministic.",
  "ml.unavailable": "This build has no learning stack; routing is deterministic.",
  "ml.serving": "A promoted model is re-ordering the provider plan.",
  "ml.inert": "A model is promoted but learned routing is off, so it is not consulted.",
  "ml.off": "No model is installed. Routing is deterministic.",
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
    "Configured models receive no traffic while exploration is off. Give them top priority, or turn exploration on (which blocks learned-model promotion).",
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
    "Over the most recent {n} records. More history is available than this window covers.",
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
  "ml.config_hint": "Shadow writes the evidence; the trace log keeps it. No evidence, no learning.",
  "ml.config_unavailable": "This build has no learning stack, so there is nothing to configure.",
  "ml.shadow_enable": "Shadow evaluation",
  "ml.shadow_enable_hint": "When off, no request retains its decision-time input and every dataset ingestion is discarded as NoDecisionTimeInput — the 0-sample state.",
  "ml.shadow_max_decisions": "Max decisions (in memory)",
  "ml.shadow_max_decisions_hint": "Ring size before the oldest is evicted. 10k is the default.",
  "ml.shadow_max_age": "Max age (seconds)",
  "ml.shadow_max_age_hint": "Retention window. 604800 = 7 days.",
  "ml.routing_enable": "Learned routing",
  "ml.routing_enable_hint": "When on, the promoted model may re-order the plan. Needs a state directory to promote at all.",
  "ml.state_dir": "State directory",
  "ml.state_dir_hint": "Where traces and the active model pointer live. Empty = in-memory only, lost on restart. e.g. ml",
  "ml.exploration": "Exploration probability",
  "ml.exploration_hint": "0 = deterministic; >0 explores alternative candidates but blocks promotion.",
  "ml.config_shadow_off_hint": "Shadow is off — every request is discarded at the ingestion boundary. Turn it on to collect real samples.",
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
    "Learned routing is on but shadow evaluation is off, so no samples are collected ({n} discarded). Turn shadow evaluation on.",
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
    "Train a candidate from recorded history and ask the gate whether it beats the baseline. Nothing is installed or scheduled automatically.",
  "ml.round_judge": "Judge a candidate",
  "ml.round_judging": "Judging…",
  "ml.round_judged": "Candidate judged, not installed.",
  "ml.round_install": "Install the authorised candidate",
  "ml.round_installing": "Installing…",
  "ml.round_install_hint":
    "Available after the gate authorizes a candidate. Changes which model serves later requests.",
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
    "The evidence rounds train from. Retention is manual; nothing changes the next model on its own.",
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
type Dict = Record<TKey, string>;

export const zh: Dict = {
  // ------------------------------------------------------------ shared
  "common.dismiss": "知道了",
  "common.cancel": "取消",
  "common.loading": "加载中…",
  "common.remove": "移除",
  "common.delete": "删除",
  "common.clear": "清除",
  "common.enabled": "启用",
  "common.yes": "是",
  "common.no": "否",
  "common.dash": "—",
  "common.unknown": "未知",
  "count.models": "{n} 个模型",
  "count.providers": "{n} 个提供商",

  // ------------------------------------------------------------------ app
  "app.live_stopped": "实时更新已停止：{err}",
  "app.exposed":
    "监听 {host} 时，同一网络的其他设备可以使用网关和你的 API 密钥。请保持认证开启，或改回 127.0.0.1。",
  "app.no_auth": "认证已关闭：本机任何进程都能消耗你的 API 额度。",
  "nav.overview": "概览",
  "nav.models": "模型",
  "nav.providers": "提供商",
  "nav.routing": "路由",
  "nav.activity": "活动",
  "nav.settings": "设置",
  "nav.unclassified_title": "{n} 个模型未分层",
  "status.running": "运行中",
  "status.stopped": "已停止",
  "status.succeeded": "成功",
  "status.failed": "失败",
  "status.cooling": "正在冷却",
  "status.enabled": "已启用",
  "status.disabled": "已禁用",
  "status.no_key": "未存储凭据",
  "appearance.title": "外观",
  "theme.system": "跟随系统",
  "theme.light": "浅色",
  "theme.dark": "深色",
  "gw.address": "地址",
  "gw.copy_address": "复制地址",
  "gw.stop": "停止网关",
  "gw.start": "启动网关",
  "settings.language": "语言",
  "settings.theme": "主题",
  "lang.zh": "简体中文",
  "lang.en": "English",
  "action.hide": "隐藏",
  "toast.copied": "已复制。",
  "toast.queued_discarded": "保存失败，排队中的修改已丢弃。",

  // ---------------------------------------------------------------- confirm
  "confirm.remove_provider": "移除提供商",
  "confirm.remove_provider_body":
    "该提供商下的 {n} 个模型与已存密钥会一并删除。 | 该提供商下的 {n} 个模型与已存密钥会一并删除。",
  "confirm.remove_model": "移除模型",
  "confirm.remove_model_body": "客户端按 id 调用它时会报错。",
  "confirm.remove_budget": "删除预算",
  "confirm.reset_stats": "重置计数",
  "confirm.reset_stats_body": "请求历史、模型健康与花费估算会被清零。",
  "confirm.regenerate_token": "重新生成令牌",
  "confirm.regenerate_token_body": "现有客户端会全部失效，直到换用新令牌。",
  "err.bind_in_use":
    "{addr} 已被占用。请停止另一个 Zroutery 进程，或在设置中换一个端口。",
  "err.bind_denied":
    "代理无法启动：系统拒绝了 {addr}。Windows 有时会保留端口段，请在设置中换一个端口。",
  "err.credentials_rejected":
    "面板拒绝了此凭据。请使用面板 个人设置 → 访问令牌 中的个人访问令牌，而不是中转的 sk- 密钥。",
  "err.relay_key":
    "这是中转 API 密钥(sk-…)，面板不接受。请改用 个人设置 → 访问令牌 中的个人访问令牌。",
  "err.no_panel_token":
    "该账户未存储面板令牌。请把凭据设置为面板的个人访问令牌。",
  "err.capture_open":
    "该账户已有一个获取令牌的流程未结束。请先在那个窗口完成登录或取消它，再重新开始。",
  "err.no_browser":
    "未找到 Chromium 浏览器。请在该账户的维护设置中指定浏览器路径，或安装 Chrome / Edge。",
  "err.capture_rejected":
    "面板拒绝了它返回的令牌。请重新登录面板、完成所有验证后再获取。",
  "err.capture_closed":
    "浏览器在拿到令牌前被关闭。请重新开始获取，并保持窗口打开直到完成。",
  "err.classifier_candidate":
    "自动审查候选 {model} 不是已配置模型，已被跳过。请在 路由 → 自动审查 中移除，或重新添加该模型。",
  "err.no_account_balance":
    "{provider} 没有账户上报余额。请把面板的个人访问令牌存为账户凭据；中转密钥不能用于面板接口。",
  "err.mixed_currencies":
    "各账户余额币种不同，无法相加，因此不显示总额。",
  "err.backup_exists":
    "配置无法另存为 {backup}，因为该文件已经存在。请移动或删除它后重试。",
  "err.preserve_failed":
    "配置无法另存为 {backup}：{err}。它保持原样，请修复或移动后重新开始。",
  "err.copy_mismatch": "{backup} 中 {path} 的副本与它不一致，因此 {path} 保持原样。",
  "err.copy_unreadable":
    "{backup} 中 {path} 的副本无法回读({err})，因此 {path} 保持原样。",
  "err.change_refused": "对 {addr} 的更改被拒绝，之前的配置与网关仍在原位({err})。",
  "err.address_not_changed": "无法使用新的地址：{err}。配置未被改动。",
  "err.address_not_changed_running":
    "无法使用新的地址：{err}。网关仍在 {addr} 上监听，配置未被改动。",
  "err.listen_failed_restored": "网关无法在 {addr} 上监听：{err}。之前的网关已恢复。",
  "err.listen_failed_not_restored":
    "网关无法在 {addr} 上监听：{err}。之前的网关也未能恢复：{restore}",
  "err.no_rollback_snapshot":
    "无法为 {path} 创建快照：文件不可读，切换将无法回滚。请修复权限后重试。",
  "err.no_such_account": "提供商 {provider} 上不存在账户 {account}。",
  "err.bind_other": "代理无法启动：无法绑定 {addr}({err})。请在设置里换一个端口。",
  "accounts.title": "账户",
  "accounts.title_hint": "该提供商下的账户。用于探测、余额与签到；请求仍使用提供商的密钥。",
  "accounts.empty": "未声明账户。所有请求都使用提供商自身的密钥。",
  "accounts.lede": "每个已声明账户的余额、签到与面板令牌。声明在提供商的账户设置中编辑。",
  "accounts.add": "添加账户",
  "accounts.id_label": "账户 ID",
  "accounts.id_hint": "在该提供商内保持稳定。用 (提供商, 账户) 组合标识其运行状态。",
  "accounts.key_label_add": "面板访问令牌(可选)",
  "accounts.key_hint_add":
    "可选。留空则使用提供商的密钥。NewAPI 面板请粘贴个人访问令牌；sk- 中转密钥不可用。",
  "accounts.duplicate": "该提供商下已存在账户 `{id}`。",
  "accounts.key_store_failed": "账户已保存，但写入凭据失败。",
  "accounts.set_key": "设置凭据",
  "accounts.key_label": "面板访问令牌",
  "accounts.key_hint":
    "面板 个人设置 → 访问令牌 中的个人访问令牌。保存在系统凭据库中，之后不再显示。",
  "accounts.clear_key": "清除凭据",
  "accounts.clear_key_title": "清除账户 {id} 的凭据",
  "accounts.clear_key_body":
    "该账户将重新使用提供商的密钥。若没有其他账户共享，其已存密钥会被删除。",
  "accounts.own_key": "独立凭据",
  "accounts.no_key": "凭据未存",
  "accounts.follows_provider": "沿用提供商凭据",
  "accounts.remove_title": "删除账户 {id}",
  "accounts.remove_body":
    "删除该声明及其已存密钥。观测数据会保留到该 id 再次被使用。",
  "accounts.checkin": "浏览器签到",
  "accounts.login_path": "面板登录路径",
  "accounts.login_path_hint":
    "浏览器获取令牌时使用。当前 NewAPI 的登录页是 /sign-in;旧分支使用 /login。",
  "accounts.panel_url": "面板地址",
  "accounts.new_api_user": "面板用户 ID",
  "accounts.new_api_user_hint":
    "部分面板(AgentRouter)要求的 New-Api-User 头。浏览器获取令牌时自动填写;留空不发送。",
  "accounts.panel_url_hint":
    "面板与中转端点不在同一域名时填写(例如中转在 api.relay.com、面板在 panel.relay.com)。留空表示同域。浏览器获取令牌与账户探测都以此为准。",
  "token_capture.start": "用浏览器获取令牌",
  "token_capture.starting": "正在打开…",
  "token_capture.captured": "令牌已保存",
  "token_capture.failed": "获取令牌失败",
  "token_capture.cancelled": "已取消获取令牌。",
  "token_capture.done": "面板令牌已存为该账户的凭据。",
  "token_capture.phase.opening": "正在打开浏览器…",
  "token_capture.phase.waiting": "请在浏览器窗口中登录…",
  "token_capture.phase.needs_verification": "请在浏览器中完成验证…",
  "token_capture.phase.stored": "正在验证并保存…",
  "accounts.interval": "间隔(秒)",
  "accounts.interval_hint": "留空则不自动执行。",
  "accounts.checkin_path": "签到页路径",
  "accounts.checkin_path_hint": "面板相对路径，例如 /checkin。默认留空。",
  "accounts.checkin_selector": "签到按钮选择器",
  "accounts.checkin_selector_hint": "浏览器点击的 CSS 选择器。刻意不设默认值。",
  "error.title": "界面出现了问题",
  "error.unaffected": "网关不受影响，仍在菜单栏正常运行。",
  "common.try_again": "重试",
  "common.reload": "重新加载",
  "nav.main_aria": "主导航",
  "activity.tries": "尝试 {n} 次",
  "providers.open_row": "打开 {name}",
  "models.open_row": "打开 {id}",
  "cc.import_row": "导入 {name}",
  "settings.remove_origin": "移除 {origin}",

  // ------------------------------------------------------------ overview
  "overview.gateway": "网关",
  "overview.gateway_stopped": "网关已停止",
  "overview.counts": "{models} 个模型 · {providers} 个提供商",
  "action.copy_base_url": "复制 Base URL",
  "action.copy_token": "复制令牌",
  "overview.configure": "配置",
  "route.default": "默认",
  "route.default_hint": "普通请求",
  "route.auto_review": "自动审查",
  "route.review_hint_on": "权限检查",
  "routing.tab_group": "路由的三个主题",
  "routing.tab_ml": "学习型路由",
  "routing.tab_main": "模型路由",
  "routing.tab_auto": "自动审查",
  "route.review_hint_off": "跟随默认",
  "route.check": "检查",
  "route.no_tier": "尚无类别分配到模型。",
  "route.no_candidate": "已启用，但无候选模型。",
  "route.off": "关闭。",
  "overview.recent": "最近请求",
  "overview.all_activity": "全部活动",
  "overview.no_requests": "暂无请求。",
  "kind.main": "主请求",
  "kind.review": "审查",
  "overview.no_providers": "暂无提供商。",
  "overview.add_provider": "添加提供商",

  // -------------------------------------------------------------- models
  "models.pick_provider_name": "请选择提供商并填写模型名。",
  "models.duplicate": "该提供商已提供“{model}”。",
  "models.unclassified": "{n} 个模型尚未分层：{ids}。仍可按完整 id 调用，但不参与分层路由。",
  "models.lede_none": "添加到提供商的模型会显示在这里。",
  "models.exposed": "{n} 个模型 · 点击编辑，或从目录添加。",
  "models.empty": "暂无模型。",
  "models.empty_hint": "在下方添加，或在提供商页从 CC Switch 导入。",
  "models.no_tier": "未分层",
  "models.add_section": "添加模型",
  "models.add_hint": "添加后再指定层级；Zroutery 不会自动推断。",
  "field.provider": "提供商",
  "field.model_name": "模型名",
  "models.add": "添加模型",
  "models.add_provider_first": "请先添加提供商。",
  "models.missing_provider": "缺失提供商",
  "models.provider": "提供商",
  "models.upstream": "上游模型",
  "models.tier": "层级",
  "models.tier_for": "{model} 的层级",
  "models.aliases": "别名",
  "models.performance": "性能",
  "models.no_traffic": "暂无流量。",
  "models.avg_latency": "平均延迟",
  "models.requests": "请求数",
  "models.failures": "失败数",
  "models.last_error": "最近错误",
  "models.configuration": "配置",
  "models.f_model_name_hint": "发送到上游；重命名会改变暴露 id",
  "models.f_display": "显示名",
  "models.f_display_hint": "显示在 /v1/models",
  "models.f_priority": "优先级",
  "models.f_priority_hint": "层内数值小者优先",
  "models.f_weight": "权重",
  "models.f_weight_hint": "同优先级时的加权随机",
  "models.f_max_tokens": "最大输出 token",
  "models.f_max_tokens_hint": "限制客户端可请求的上限",
  "models.f_context_window": "上下文长度",
  "models.f_context_window_hint":
    "模型可接受的上下文长度(已知时)。会在 /v1/models 公布；上游仍是权威。",
  "models.context_set_1m": "设为 1M",
  "models.context_unknown": "未知",
  "models.f_aliases_hint": "逗号分隔的短名称",
  "models.search": "搜索模型",
  "models.showing": "显示 {shown}/{total}",
  "models.no_match": "没有匹配“{query}”的模型。",
  "models.name_taken": "`{id}` 已被另一个模型使用。",
  "models.exposed_as": "暴露为 {id}",
  "models.rename_required": "模型名称不能为空。",
  "models.rename_failed": "重命名未能保存；没有引用被改动。",
  "models.import_browse": "浏览提供商目录",
  "models.import_hide": "收起目录",
  "models.import_search": "筛选目录",
  "models.import_tier": "批量添加为",
  "models.import_add": "添加所选({n})",
  "models.import_empty": "没有匹配的模型。",
  "models.import_added": "已添加",
  "models.imported": "已添加 {n} 个模型。",
  "models.tool_use": "工具调用",
  "models.vision": "视觉",
  "models.thinking": "扩展思考",
  "models.effort_levels": "思考强度挡位",
  "models.effort_levels_hint": "接管时按挡位向客户端提供变体。导入时从 models.dev 自动填写，可按模型人工修正。",
  "models.sync": "同步模型信息",
  "models.syncing": "同步中…",
  "models.synced": "已为 {n} 个模型补齐信息",
  "models.sync_none": "已配置模型的信息都已齐全",
  "models.sync_failed": "无法读取以下提供商的目录：{names}",
  "models.price": "每百万 token 价格",
  "models.price_hint": "留空则请求不记录成本，而不是猜测一个。",
  "field.currency": "币种",
  "field.currency_hint": "USD、CNY 等",
  "field.input": "输入",
  "field.input_hint": "每 1M 输入 token",
  "models.input_price": "输入价格",
  "models.output_price": "输出价格",
  "models.price_unlimited": "不限",
  "models.price_same_as_input": "同输入",
  "models.price_not_billed": "不计费",
  "common.close": "关闭",
  "routing.priority_for": "{model} 的优先级",
  "routing.move_up": "上移 {model}",
  "routing.move_down": "下移 {model}",
  "field.output": "输出",
  "field.output_hint": "每 1M 输出 token",
  "models.cache_read": "缓存读",
  "models.cache_read_hint": "可选，默认同输入价",
  "models.cache_write": "缓存写",
  "models.cache_write_hint": "可选",
  "models.clear_price": "清除价格",
  "models.remove": "移除模型",

  // ----------------------------------------------------------- providers
  "providers.lede_none": "每个上游账号或端点一个提供商。",
  "providers.import_cc": "从 CC Switch 导入",
  "providers.reading_cc": "正在读取 CC Switch…",
  "providers.empty": "暂无提供商。",
  "providers.empty_hint": "在下方添加，或从 CC Switch 导入。",
  "providers.openai_dialect": "OpenAI 兼容",
  "providers.anthropic_dialect": "Anthropic",
  "providers.provider_off": "(已停用)",
  "providers.add_section": "添加提供商",
  "providers.add_hint": "端点、凭证与协议；模型在各自的页面配置。",
  "field.name": "名称",
  "field.api_dialect": "API 协议",
  "field.base_url": "Base URL",
  "field.base_url_hint": "不含端点路径",
  "providers.add": "添加提供商",
  "providers.duplicate": "名为“{id}”的提供商已存在。",
  "cc.source_hint": "读取自 {path}。密钥存入系统凭据管理器，绝不写入配置文件。",
  "cc.reload": "重新读取",
  "cc.import": "导入",
  "cc.empty": "CC Switch 中未找到 Claude Code 提供商。",
  "cc.active": "使用中",
  "cc.already": "已存在",
  "cc.imported_notice": "已导入 {n} 个提供商。密钥已直接存入凭据管理器。",
  "providers.endpoint": "端点",
  "providers.dialect": "协议",
  "providers.auth": "认证",
  "providers.no_key": "未存储密钥",
  "providers.no_cred": "无需凭证",
  "providers.models_count": "该提供商 {n} 个",
  "providers.models_none": "暂无",
  "providers.api_key": "API 密钥",
  "providers.api_key_hint": "存储在系统凭据管理器，绝不写入配置文件。",
  "providers.save_key": "保存密钥",
  "providers.models_section": "模型",
  "providers.empty_catalogue": "{name} 返回了空模型列表。",
  "providers.configuration": "配置",
  "field.timeout": "超时 (秒)",
  "field.timeout_hint": "整个请求，含流式空闲间隔",
  "field.connect_timeout": "连接超时 (秒)",
  "providers.f_version": "anthropic-version",
  "providers.f_version_hint":
    "带日期的 API 版本，互斥的线协议契约。2023-06-01 是沿用三年的基线。2026-10-21 移除了 temperature/top_p/top_k 并限制 document 只能出现在 user 消息——请求会按所选版本相应塑形。",
  "providers.f_beta": "anthropic-beta",
  "providers.f_beta_hint":
    "要请求的测试性能力，逗号或分号分隔(如 context-management-2025-06-27)。留空不发送。Anthropic 的新能力都先以 beta 头形式发布。",
  "providers.api_surface": "API 端面",
  "providers.api_surface_hint":
    "该端点说哪种 OpenAI 协议。Responses 是较新的端面，部分 Codex 形态的网关只认它。请求、响应与流式事件三者形状都不同。",
  "providers.surface_chat": "Chat Completions",
  "providers.surface_responses": "Responses",
  "providers.client_profile": "客户端身份",
  "providers.client_profile_hint":
    "以什么身份访问上游。选 Claude Code 或 Codex 时会转发该客户端的请求头。",
  "providers.profile_auto": "自动 · 识别调用方",
  "providers.profile_native": "原生 · 不伪装客户端",
  "providers.profile_claude_code": "Claude Code · 带其请求头",
  "providers.profile_codex": "Codex · 带其请求头",
  "providers.profile_opencode_cli": "OpenCode CLI · OpenCode 提供商固定使用",
  "providers.client_profile_locked_hint":
    "已锁定。OpenCode 网关会校验该指纹，配置校验也不接受其他取值。",
  "providers.opencode_zen_dialect": "OpenCode Zen(免费)",
  "providers.opencode_console_dialect": "OpenCode Console",
  "providers.decision_dialect": "决策模型",
  "providers.decision": "决策",
  "providers.decision_hint":
    "用概率回答有类型的问题，不生成文字。仅用于自动审查；模型需手动加入池中。Jev(包括 OpenCode Zen 免费模型)是参考家族。",
  "providers.decision_path": "决策接口路径",
  "providers.decision_path_hint":
    "与 Base URL 拼接的路径。参考服务使用 /v1/systemone。",
  "providers.decision_threshold": "所需置信度",
  "providers.decision_threshold_hint":
    "自动审查行动前所需的置信度。不低于该值则拦截；不高于 1 减该值则放行；中间按拦截。0.5 由模型多数票决定。",
  "providers.decision_no_catalogue":
    "决策接口没有模型目录。请在模型页手动添加模型。",
  "models.tier_decision": "为何没有层级",
  "models.tier_decision_hint":
    "决策模型只服务自动审查，不能应答聊天，因此不归入层级。请加入自动审查池。",
  "providers.free_badge": "免费",
  "providers.free_only": "仅显示免费模型",
  "providers.free_only_hint":
    "仅显示目录标记为免费的模型。免费模型仍可能限流、地区屏蔽或下线。",
  "providers.catalogue_shown": "显示 {total} 个中的 {shown} 个",
  "providers.catalogue_total": "共 {total} 个模型",
  "providers.catalogue_no_free":
    "目录共 {total} 个模型，没有标记为免费的。关闭筛选即可查看。",
  "providers.bearer_auth": "同时发送 Bearer 认证",
  "providers.bearer_auth_hint": "用于读 Authorization: Bearer 而非 x-api-key 的中转。",
  "providers.balance": "余额",
  "providers.balance_hint": "部分提供商在自己的接口公布剩余额度。选择对应接口，Zroutery 会用提供商密钥查询。",
  "providers.balance_hint_panel":
    "NewAPI 中转的钱包在面板里。余额来自该提供商账户所属的面板，使用账户的个人访问令牌读取。",
  "providers.balance_hint_none":
    "该提供商不公布余额，也没有可供查询的面板账户。请在上方选择探针，或声明账户并保存个人访问令牌。",
  "providers.balance_no_source": "该提供商没有可查询的余额来源：自身不公布，也未声明面板账户。",
  "balance_preset.none": "不支持",
  "balance_preset.deep_seek": "DeepSeek",
  "balance_preset.moonshot": "Moonshot",
  "balance_preset.silicon_flow": "SiliconFlow",
  "balance_preset.open_router": "OpenRouter",
  "balance_preset.sub2api": "Sub2API 中转",
  "balance_preset.custom": "自定义接口",
  "balance_hint.none": "OpenAI 与 Anthropic 自身都不公布余额。",
  "balance_hint.deep_seek": "在提供商自己的接口上查询 /user/balance。",
  "balance_hint.moonshot": "在提供商自己的接口上查询 /users/me/balance。",
  "balance_hint.silicon_flow": "在提供商自己的接口上查询 /user/info。",
  "balance_hint.open_router": "在提供商自己的接口上查询 /credits。",
  "balance_hint.sub2api":
    "查询 /v1/usage。答案可能是钱包余额、密钥额度或订阅配额，取决于该密钥的签发方式。",
  "balance_hint.custom": "自定义路径，并自行指定剩余、已用与总额对应的 JSON 指针。",
  "field.probe": "探针",
  "providers.check_now": "立即查询",
  "providers.checking": "查询中…",
  "providers.compatibility": "兼容性",
  "providers.compatibility_hint": "用于偏离标准协议的提供商；除非请求失败，否则保持全部关闭。",
  "quirk.max_completion_tokens": "使用 max_completion_tokens",
  "quirk.drop_temperature": "丢弃 temperature",
  "quirk.drop_top_p": "丢弃 top_p",
  "quirk.drop_stop": "丢弃 stop 序列",
  "quirk.drop_stop_hint": "从不作用于自动审查请求",
  "quirk.system_as_developer": "system 消息用 developer 角色",
  "quirk.reasoning_effort": "将 thinking 转为 reasoning_effort",
  "quirk.portable_history": "跨上游可移植的 Responses 历史",
  "quirk.portable_history_hint":
    "适用于可能由其他账户或上游应答的中转(AgentRouter、CC Switch)。转发前删除 reasoning 项、历史 item id 与 previous_response_id;此前的推理不再传给模型。仅影响 Responses 请求。",
  "providers.remove": "移除提供商",
  "providers.removed_models_notice": "已移除属于该提供商的 {n} 个模型。",
  "providers.balance_failed": "查询失败",

  // ------------------------------------------------------- account maintenance
  "account.phase.idle": "尚未签到",
  "account.phase.running": "正在签到…",
  "account.phase.waiting_for_user": "需要人工验证。请在已打开的窗口中完成。",
  "account.phase.succeeded": "已签到",
  "account.phase.already_completed": "今天已签到",
  "account.phase.not_supported": "该平台不支持签到",
  "account.phase.failed": "签到失败",
  "account.phase.cancelled": "已取消",
  "account.phase.cancelling": "正在取消…",
  /*
   * 六个决策，六句话。账户不自动运行的原因彼此差别足够大，共用一个「没有动作」会把
   * 人引去改一个本来没错的设置：其中一个是用户自己的选择，一个是声明缺了一块，只有
   * 两个才表示一个日程正在安静地倒数。
   */
    "account.balance": "余额",
"account.balance_amount": "剩 {remaining} {unit}，总额 {total}",
"account.schedule.disabled": "该账户未开启签到",
  "account.schedule.not_applicable":
    "签到已开启但无法执行。请设置签到页面和触发按钮选择器。",
  "account.schedule.needs_first_observation":
    "尚未签到，暂无到期时间。签到一次后开始计时。",
  "account.schedule.no_policy":
    "尚未设置签到周期，不会自动签到。设置签到周期后生效。",
  "account.schedule.not_due": "已排期",
  "account.schedule.due": "现在到期",
  "account.schedule.unknown": "日程未知",
  "account.next_run": "下次运行",
  "account.last_checkin": "上次签到",
  "account.reward": "奖励",
  "account.reward_weak_source": "取自平台日志文本",
  "account.reward_inferred": "由余额变化推断",
  "account.effect.credited": "已确认入账",
  "account.effect.debited": "已确认扣减",
  "account.effect.no_change": "已确认无变化",
  "account.effect.unobserved": "未观测到",
  "account.source.provider_response": "平台直接返回",
  "account.source.account_state": "读自账户状态",
  "account.source.account_log": "读自账户日志",
  "account.source.operation_response": "来自操作返回",
  "account.checkin": "签到",
  "account.resume": "在已打开的窗口中继续",
  "account.cancel": "取消",
  "account.close_browser": "关闭窗口",
  "account.not_enabled": "该账户未启用签到。请在提供商的账户设置中开启。",
  "account.need_path": "未设置签到页面。请在提供商的账户设置中填写。",
  "account.need_selector": "未设置签到按钮。请在提供商的账户设置中填写 CSS 选择器，例如 button.checkin。",
  "account.start_failed": "无法开始签到",
  "account.started": "签到已开始。若需要人工验证，浏览器会提示。",
  "account.resumed": "签到已继续。",
  "account.cancelled": "签到已取消。",
  "account.resume_failed": "无法继续签到",
  "account.cancel_failed": "无法取消签到",

  // ------------------------------------------------------- account probing
  //
  // 探测不是签到。它只询问平台账户处于什么状态并报告结果，不会打开浏览器，
  // 也不会领取任何东西。
  "providers.refresh_all": "刷新全部余额",
  "nav.accounts": "账户",
  "nav.takeover": "接管",
  "accounts.empty_build": "此构建未包含账户子系统，声明的账户无法探测或签到。",
  "accounts.none": "尚未声明账户。请在提供商页的账户设置中添加。",
  "accounts.probe_section": "账户探测",
  "accounts.probe_hint": "向每个已启用账户的面板查询当前状态。每个账户一次请求，仅在点击时执行。",
  "accounts.probe_now": "探测所有账户",
  "accounts.probing": "探测中…",
  "accounts.probe_idle": "尚未进行探测。",
  "accounts.probe_none": "探测未发现已启用且已声明的账户。",
  "accounts.probe_outcome": "结果",
  "accounts.provider_disabled": "提供商已停用",
  "accounts.own_key_stored": "已存面板访问令牌",
  "accounts.own_key_missing": "声明了独立凭据，但尚未存储",
  "accounts.shared_key": "共用提供商凭据",
  // 凭据类型提示。面板 API 认的是站点「访问令牌」(面板个人设置页里的那一串),
  // 中转的 sk- 密钥在那里只会被当成密码错误。说清楚两者的区别,声明才能被修好。
  "accounts.auth_hint": "面板拒绝了此凭据。账户需要面板的个人访问令牌，而不是中转 sk- 密钥。",
  "accounts.last_failure": "上次探测失败",
  "accounts.checkin_api": "直接签到",
  "accounts.checkin_api_hint": "使用已存令牌通过面板 API 签到，不打开浏览器。面板要求验证时会失败。",
  "accounts.checking": "签到中…",
  "accounts.checkin_api_failed": "直接签到失败",
  "accounts.events_section": "账户事件",
  "accounts.events_hint": "最近的账户事件，最新在前，并标注数字来源。",
  "accounts.events_none": "还没有观察到任何事件。",
  "accounts.events_when": "时间",
  "accounts.events_what": "事件",
  "accounts.status.active": "正常",
  "accounts.status.suspended": "已停用",
  "accounts.status.quota_exhausted": "额度用尽",
  "accounts.status.rate_limited": "被限速",
  "accounts.status.authentication_expired": "凭据被拒",
  "accounts.status.unknown": "未知",
  "accounts.status.unprobed": "未探测",
  "field.account": "账户",
  // 四种结果，四种说法。分开表述，是因为它们各自回答不同的问题；
  // 若归为「正常 / 异常」两类，无人能够询问的账户就会被显示成有问题的账户。
  "account.sync.refreshed": "已探测",
  "account.sync.partially_reported": "未上报",
  "account.sync.unsupported": "此构建没有能询问该平台的后端；未探测。",
  "account.sync.failed": "探测失败",

  // ------------------------------------------------------------- 接管
  // 接管页。接管会把本机网关地址与令牌写入外部客户端自己的配置文件,
  // 记录被替换的值,释放时恢复。九种状态逐个渲染,因为其中四种需要用户
  // 做决定;归并成「在管/不在管」两类,恰好会藏住这些时刻。
  "takeover.section": "Agent 接管",
  "takeover.section_hint":
    "把网关地址与令牌写入客户端自己的配置，释放时恢复原值。客户端仍按它发送的模型名选择层级。",
  "takeover.none": "此构建没有已知的外部 Agent。",
  "takeover.load_failed": "接管记录读取失败：{err}。本页没有改动过任何客户端的配置。",
  "takeover.agent.claude": "Claude Code",
  "takeover.agent.codex": "Codex CLI",
  "takeover.agent.opencode": "OpenCode",
  "takeover.agent.gemini": "Gemini CLI",
  "takeover.state.unsupported": "不支持",
  "takeover.state.not_detected": "未检测到",
  "takeover.state.available": "可接管",
  "takeover.state.ready": "就绪",
  "takeover.state.active": "接管中",
  "takeover.state.conflict": "存在冲突",
  "takeover.state.degraded": "未确认",
  "takeover.state.released": "已释放",
  "takeover.state.error": "出错",
  "takeover.hint.unsupported":
    "Gemini CLI 只能通过环境变量设置端点，配置文件无法记录，因此不提供接管。",
  "takeover.hint.not_detected":
    "未找到 {agent} 的配置。请安装并运行一次后刷新。",
  "takeover.hint.ready":
    "已就绪：接管会写入下方 {n} 个字段，并记录当前值。",
  "takeover.confirm_inline":
    "接管会修改 {agent} 的配置文件。释放时只恢复这些字段，其余内容不动。",
  "takeover.hint.active":
    "已收到该客户端的网关请求，托管字段与写入值一致。",
  "takeover.hint.degraded":
    "配置已托管，但验证尚未完成。具体原因见下方。",
  "takeover.hint.conflict":
    "{n} 个托管字段被外部修改。释放时请选择保留这些修改，还是恢复记录值。",
  "takeover.hint.released":
    "已释放并恢复原值。再次接管会记录当前值。",
  "takeover.blocker.no_token":
    "尚无网关令牌。请在 设置 → 网关 中设置或重新生成。",
  "takeover.blocker.no_classes":
    "没有已启用的分层模型，客户端无可请求的模型。请先分配层级。",
  "takeover.reason.unverified": "接管尚未被该客户端的真实请求确认过。",
  "takeover.reason.token_changed":
    "接管后网关令牌已变更。请释放并重新接管，把新令牌写入客户端。",
  "takeover.table.field": "字段",
  "takeover.table.current": "当前值",
  "takeover.table.next": "接管后",
  "takeover.table.managed": "Zroutery 写入",
  "takeover.generation": "第 {n} 代",
  "takeover.adopt": "接管",
  "takeover.adopt_again": "再次接管",
  "takeover.release": "释放",
  "takeover.working": "处理中…",
  "takeover.strategy.label": "冲突处理",
  "takeover.strategy.keep_external": "保留我的修改",
  "takeover.strategy.overwrite": "恢复 Zroutery 的值",
  "takeover.strategy.skip": "跳过已改动的字段",
  "takeover.confirm_adopt_title": "接管 {agent}?",
  "takeover.confirm_adopt_body":
    "{agent} 配置中的 {n} 个字段将被改写并托管。当前值已记录，供释放时恢复。",
  "takeover.confirm_adopt_again_body":
    "{agent} 的配置将被改写并进入托管，当前值会作为新的恢复点记录下来。",
  "takeover.confirm_release_title": "释放 {agent}?",
  "takeover.confirm_release_body":
    "{agent} 配置中的托管字段将恢复为记录值，Zroutery 不再管理它们。",
  "takeover.toast_adopted": "{agent} 已接管。",
  "takeover.toast_released": "已释放。",
  "takeover.report_kept": "保留 {n} 处修改",
  "takeover.report_overwritten": "恢复 {n} 个字段",
  "takeover.report_skipped": "跳过 {n} 个字段",

  // ------------------------------------------------------------- cutover
  /*
   * 切换页。切换会用一个暂存副本整体替换客户端的配置,并在动手之前逐字节
   * 快照原文件。它与接管页有意重叠,但两者不是同一件工具:接管写的是受管
   * 字段、并记录字段原值;这里写的是整个文件、并保留整个文件 —— 两者中只有
   * 这一种能在进程中途死掉后仍然幸存。这正是单独存在这一页、而不是在接管
   * 页再加一组按钮的原因:它回答的是一次失败或被中断的切换,以及用户的
   * 文件有没有回来。
   */
  "nav.cutover": "切换",
  "cutover.section": "配置切换",
  "cutover.section_hint":
    "把客户端配置切换到本网关：先完整备份原文件，失败时可精确还原。启停客户端请用接管页，此页负责回滚。",
  "cutover.state.section": "迁移记录",
  "cutover.state.detected": "尚未运行",
  "cutover.state.prepared": "在准备阶段中断",
  "cutover.state.verified": "在验证阶段中断",
  "cutover.state.switched": "在切换阶段中断",
  "cutover.state.completed": "已完成",
  "cutover.state.rolled_back": "已回滚",
  "cutover.state.failed": "已失败",
  "cutover.state.hint.detected": "该记录未运行过任何切换。本页没有改动过任何客户端的配置。",
  "cutover.state.hint.completed":
    "切换已完成，此状态不可回滚；快照仍保留在磁盘上。",
  "cutover.state.hint.rolled_back":
    "已回滚并恢复文件。再次运行会从原文件开始。",
  "cutover.state.hint.prepared":
    "文档已暂存，客户端文件尚未写入。快照是原文件的唯一副本。",
  "cutover.state.hint.verified":
    "已暂存并验证，客户端文件未改动。快照是原文件的唯一副本。",
  "cutover.state.hint.switched":
    "已切换，客户端文件已被替换。恢复快照是唯一的退路。",
  "cutover.state.hint.failed":
    "中途失败。请用下方快照回滚。",
  "cutover.snapshot.none": "尚未捕获任何回滚快照。",
  "cutover.snapshot.files": "已捕获 {n} 个文件，可逐字节还原。",
  "cutover.not_durable": "此构建的迁移记录不持久，重启后无法恢复中断的切换。",
  "cutover.persist_error": "记录未能写入磁盘：{err}",
  "cutover.plans.section": "切换计划",
  "cutover.plans.hint": "每个计划使用客户端的真实配置路径和本网关地址。确认前不会修改任何文件。",
  "cutover.none": "此构建没有已知的外部客户端可供切换。安装并运行一个，使其配置文件存在，然后刷新本页。",
  "cutover.load_failed": "迁移记录读取失败：{err}。本页没有改动过任何客户端的配置。",
  "cutover.step.not_reversible": "无法撤销",
  "cutover.step.reversible": "回滚时还原",
  "cutover.step.stop_external": "检查 {name} 是否仍在运行",
  "cutover.step.start_zroutery": "检查端口 {port} 是否空闲",
  "cutover.step.copy_config": "把暂存文档安装到 {path}",
  "cutover.step.verify_endpoint": "确认 {url} 以 Zroutery 的身份应答",
  "cutover.table.undo": "可撤销性",
  "cutover.table.step": "步骤",
  "cutover.run": "执行此次切换",
  "cutover.running": "执行中…",
  "cutover.rollback": "回滚",
  "cutover.rolling_back": "回滚中…",
  "cutover.already_run": "该记录只允许一次切换且已运行。如要重新开始，请从配置目录删除该记录。",
  "cutover.rollback_unavailable": "只有失败后才会提供回滚。",
  "cutover.confirm_run_title": "替换 {agent} 的配置？",
  "cutover.confirm_run_body":
    "共 {n} 步。写入前会完整备份，失败可回滚。检测到运行中的外部进程时只报告，不停止。",
  "cutover.confirm_rollback_title": "把配置文件还原回去？",
  "cutover.confirm_rollback_body":
    "将用切换前的备份覆盖 {n} 个文件。之后的修改和切换本身的改动都会丢失。",
  "cutover.result.title": "最近一次运行",
  "cutover.result.steps": "{total} 步中的 {done} 步",
  "cutover.result.duration": "{ms} 毫秒",
  "cutover.result.errors": "错误",
  "cutover.result.warnings": "警告",
  "cutover.result.restored": "已还原 {n} 个文件",
  "cutover.result.restore_failed":
    "{n} 个文件未能恢复，磁盘配置与原来不同。请手动修复。",
  "cutover.result.rolled_back_nothing":
    "没有文件被恢复。状态已推进，磁盘文件未变。",
  "cutover.result.ok": "切换已完成，端点以 Zroutery 的身份作答。",
  "cutover.toast_applied": "{agent} 的切换已完成。",
  "cutover.toast_failed": "切换失败，共 {n} 个错误。下方报告会指出停在哪一步；回滚即可恢复。",
  "cutover.toast_rolled_back": "已还原 {n} 个文件。",
  "cutover.toast_nothing_restored": "未还原任何内容。",
  "cutover.agent.claude": "Claude Code",
  "cutover.agent.codex": "Codex CLI",
  "cutover.agent.opencode": "OpenCode",
  "cutover.agent.gemini": "Gemini CLI",

  // ------------------------------------------------------------- routing
  "routing.lede": "按用途展示请求的路由方式。兼容任何 Anthropic 或 OpenAI 客户端。",
  "routing.default_hint": "所有普通请求，按客户端请求的层级。",
  "routing.edit": "编辑",
  "tier.hint.fast": "便宜且快",
  "tier.hint.standard": "日常主力",
  "tier.hint.reasoning": "最强、最贵的层级",
  "tier.hint.frontier": "最前沿的模型",
  "tier.name.fast": "快速",
  "tier.name.standard": "标准",
  "tier.name.reasoning": "推理",
  "tier.name.frontier": "前沿",
  "routing.tier_empty": "为空。在有模型加入该层级前，对 {id} 的请求会失败。",
  "routing.fallback_n": "备选 {n}",
  "routing.more_fallbacks": "还有 {n} 个备选",
  "routing.request": "请求",
  "routing.side_query": "侧查询",
  "routing.review_hint_on": "权限检查走独立的模型池。",
  "routing.review_hint_off": "已关闭。权限检查跟随默认路由。",
  "routing.verdict": "判定",
  "routing.next": "下一个",
  "routing.review_no_candidate": "已启用，但没有可用候选。权限检查将失败。",
  "routing.review_off": "已关闭。开启后权限检查走下方模型池。",
  "routing.default_drawer": "默认路由",
  "field.strategy": "策略",
  "strategy.priority": "优先级",
  "strategy.weighted_random": "加权随机",
  "strategy.round_robin": "轮询",
  "strategy.lowest_latency": "最低延迟",
  "strategy.balanced": "均衡(选举)",
  "field.max_attempts": "最大尝试次数",
  "field.max_attempts_hint": "每个请求的上游尝试次数",
  "field.unknown_ids": "未知模型 id",
  "routing.unknown_404": "返回 404",
  "routing.unknown_serve": "由 {id} 提供",
  "routing.failover": "层内失败转移",
  "routing.failover_hint": "出错时尝试下一个模型",
  "routing.claude_names": "理解 Claude 模型名",
  "routing.claude_names_hint": "claude-*-sonnet-* 映射到对应层级",
  "routing.aliases_note": "其余必须是完整 id，或下面配置的某个客户端别名。",
  "routing.election": "选举",
  "routing.election_hint": "每模型一次极小请求测量延迟与价格，然后固定顺序。",
  "routing.rerun": "立即重跑",
  "routing.elect_on_start": "启动时进行一次",
  "routing.no_election": "本次运行尚未进行选举。",
  "routing.no_election_tiers": "没有层级有可测量的启用模型。",
  "routing.primary": "主选",
  "routing.route_review": "路由权限检查",
  "routing.route_review_hint": "权限检查走下方模型池，而非会话模型。包括 Claude Code 自动审查与 Codex Guardian。",
  "routing.review_failover": "池内失败转移",
  "routing.review_failover_hint": "出错时尝试下一个候选",
  "routing.pool": "模型池",
  "routing.pool_hint": "模型来自模型页，提供商、密钥与价格设置直接复用。",
  "routing.no_candidates": "暂无候选。在下方选择模型。",
  "routing.not_configured": "不是已配置的模型",
  "routing.decision_candidate": "决策模型",
  "routing.add_jev": "添加 OpenCode Zen 免费 Jev",
  "routing.add_jev_hint":
    "把无需凭据的 OpenCode Zen Jev 决策模型加到池首。阈值 0.6。免费推广可能结束或按地区屏蔽；检查文本会发送给 Zen。",
  "routing.jev_added": "已把 Jev 放在池首，并开启自动审查。",
  "routing.enable_for": "启用 {model}",
  "routing.add_model": "添加模型…",
  "routing.behaviour": "行为",
  "routing.detection": "检测",
  "routing.automatic": "自动",
  "routing.verdict_parsing": "判定解析",
  "routing.verdict_only": "仅 <block>yes|no</block>",
  "routing.verdict_only_hint":
    "Claude Code 期望 <block>yes|no</block>;Codex Guardian 期望 JSON outcome。决策模型以概率作答，由提供商阈值决定判定。",
  "routing.no_verdict": "无判定时",
  "routing.fails_closed": "失败关闭；由客户端询问用户",
  "routing.fail_closed_note": "出错、超时或协议不符会转移到下一个；全部失败则查询失败，不会放行。决策模型无把握时拦截。散文不会被当作许可。",

  // ------------------------------------------------------------ activity
  "activity.lede_none": "暂无数据。将客户端指向网关即可在此看到流量。",
  "activity.lede": "{requests} 个请求 · {rate}% 成功 · 输入 {in} / 输出 {out} token",
  "activity.reset": "重置计数",
  "activity.review_line": "自动审查：{n} 次检查 · 失败 {failed} · 平均 {latency}(与主流量分开统计)。",
  "activity.nothing": "暂无数据。",
  "activity.health": "模型健康",
  "activity.tab_group": "活动的两个主题",
  "activity.tab_usage": "请求与用量",
  "activity.tab_log": "内部日志",
  "activity.log_section": "内部日志",
  "activity.log_hint": "进程自身的输出，在此页签打开时持续跟踪。",
  "activity.show_log": "显示内部日志",
  "activity.stop_follow": "停止跟踪",
  "activity.health_hint": "每模型的熔断状态，所有路由池共享。",
  "activity.ok_failed": "{ok} 成功 / {failed} 失败",
  "activity.cooling": "冷却 {n}s",
  "activity.clear_health": "清除 {id} 的健康状态",
  "activity.spend": "花费",
  "activity.spend_hint": "根据填写的价格与上报用量估算，不是账单。",
  "activity.estimated": "预估花费",
  "activity.n_failed": "{n} 失败",
  "activity.request": "请求",
  "activity.review": "审查",
  "activity.asked_for": "请求模型",
  "activity.answered_by": "实际响应",
  "activity.kind": "类型",
  "activity.result": "结果",
  "activity.attempts": "尝试",
  "kind.main_kv": "主请求",
  "kind.check_kv": "权限检查",
  "activity.timing": "用时",
  "activity.total": "总计",
  "activity.first_token": "首个 token",
  "activity.streamed": "流式",
  "activity.usage": "用量",
  "activity.input_tokens": "输入 token",
  "activity.output_tokens": "输出 token",
  "activity.cache_read": "缓存读",
  "activity.cache_write": "缓存写",
  "activity.cost": "成本",
  "activity.unpriced": "未定价",

  // ------------------------------------------------------------ settings
  "settings.lede": "应用行为、对外的网关，以及它被允许花多少。",
  "settings.gateway": "网关",
  "settings.gateway_hint": "服务地址 {url} · {path}",
  "field.host": "主机",
  "field.host_hint": "127.0.0.1 仅本机可访问",
  "field.port": "端口",
  "field.body_limit": "最大请求体 (MiB)",
  "field.body_limit_hint": "内嵌图片会撑大请求",
  "field.log_limit": "保留请求数",
  "field.log_limit_hint": "内存活动日志的容量",
  "settings.require_auth": "要求本地令牌",
  "settings.require_auth_hint": "强烈建议",
  "settings.autostart": "启动时开启网关",
  "settings.bypass_proxy": "绕过系统代理",
  "settings.bypass_proxy_hint": "代理剥掉客户端指纹头时需要",
  "settings.local_token": "本地令牌",
  "settings.reveal": "显示",
  "settings.reveal_title": "在窗口中显示令牌",
  "settings.copy": "复制",
  "settings.regenerate": "重新生成",
  "settings.regenerate_title": "现有客户端需要换用新令牌",
  "settings.token_hint": "以 x-api-key 或 Authorization: Bearer 发送。",
  "settings.point_client": "将客户端指向网关",
  "settings.anthropic_clients": "Anthropic 风格客户端：",
  "settings.openai_clients": "OpenAI 风格客户端：",
  "settings.snippet_comment": "# 或模型页中的任意完整 id",
  "settings.cors": "浏览器访问 (CORS)",
  "settings.cors_hint": "仅页面内运行的客户端需要。",
  "settings.allow_origins": "允许浏览器来源",
  "settings.no_origins": "未列出来源，即接受任意来源。请添加实际使用的。",
  "field.allowed_origin": "允许的来源",
  "field.allowed_origin_hint": "协议、主机与端口，不含路径",
  "settings.add_origin": "添加来源",
  "settings.origin_notice": "来源格式形如 https://example.com，不含路径。",
  "settings.budgets": "预算",
  "settings.budgets_hint": "限额达到后停止消费。越线的那次请求会完成，下一次被拒绝。",
  "budget.covers": "覆盖",
  "budget.window": "周期",
  "budget.limit": "限额",
  "budget.spent": "已花费",
  "budget.when": "到达时",
  "budget.on": "启用",
  "scope.global": "全部",
  "scope.provider": "提供商 {id}",
  "period.day": "按天",
  "period.month": "按月",
  "budget.reject": "拒绝请求",
  "budget.degrade": "由 {id} 提供",
  "budget.used_up": "已用尽",
  "budget.add": "添加预算",
  "budget.currency_hint": "以模型计费的币种",
  "settings.election_weights": "如何权衡",
  "settings.tier_naming_section": "客户端可以请求什么",
  "settings.naming_style_hint": "层级在整个应用里的显示方式，不只出现在路由页。",
  "settings.tab_group": "设置的四个主题",
  "settings.tab_general": "常规",
  "settings.tab_gateway": "网关",
  "settings.tab_budgets": "限额",
  "settings.aliases": "客户端模型别名",
  "settings.aliases_hint": "将客户端发送的完整模型 id 映射到层级。优先于 Claude 名称启发式。",
  "settings.f_model_id": "模型 id",
  "settings.f_class": "层级",
  "settings.add_alias": "添加别名",
  "settings.price_weight": "价格权重",
  "settings.price_weight_hint": "相对值，3:1 等价于 0.75:0.25",
  "settings.latency_weight": "延迟权重",
  "settings.ref_input": "参考输入 token",
  "settings.ref_input_hint": "价格按 Mtok 计，需要一个请求作比较基准",
  "settings.ref_output": "参考输出 token",
  "settings.naming_style": "层级命名",
  "settings.naming_internal": "内部",
  "settings.naming_anthropic": "Anthropic",
  "settings.naming_openai": "OpenAI",
  "settings.window": "窗口行为",
  "win.launch_on_login": "登录时启动",
  "win.launch_on_login_hint": "系统登录时自动运行 Zroutery",
  "win.silent_start": "静默启动",
  "win.silent_start_hint": "启动时不显示主窗口，仅在系统托盘运行",
  "win.keep_in_tray": "关闭窗口时保留在托盘",
  "win.keep_in_tray_hint":
    "开启：关闭窗口后 Zroutery 留在托盘。关闭：关闭窗口即退出并停止网关。",
  "win.window_actions": "隐藏或退出",
  "win.window_actions_hint": "隐藏则网关继续在托盘运行；退出会停止网关并结束进程。",
  "win.hide_to_tray": "隐藏到托盘",
  "win.quit": "退出",
  "vision.title": "视觉降级",
  "vision.hint": "不支持视觉的模型会收到图片描述；无法生成描述时使用占位符。图片不会被静默丢弃。",
  "vision.enable": "为不支持视觉的模型描述图片",
  "vision.model": "视觉模型",
  "vision.model_hint": "模型页中一个支持视觉的现有模型；复用其提供商与价格",
  "vision.model_required": "请先在模型页选择一个支持视觉的模型。",
  "vision.placeholder": "占位符",
  "vision.placeholder_hint": "无法生成描述时，替代图片的文本",
  "ml.config_title": "采集设置",
  "ml.config_hint": "Shadow 产出证据，轨迹日志保存证据。没有证据就没有学习。",
  "ml.config_unavailable": "此构建未包含学习能力，无需配置。",
  "ml.shadow_enable": "Shadow 评估",
  "ml.shadow_enable_hint": "关闭时，无请求保留决策时刻输入，全部在入库边界被丢弃为 NoDecisionTimeInput — 即 0 样本状态。",
  "ml.shadow_max_decisions": "最大决策数(内存)",
  "ml.shadow_max_decisions_hint": "环形缓冲区大小，超限逐出最旧。默认 10000。",
  "ml.shadow_max_age": "最长保留(秒)",
  "ml.shadow_max_age_hint": "保留窗口。604800 = 7 天。",
  "ml.routing_enable": "学习型路由",
  "ml.routing_enable_hint": "开启后，已晋升模型可重排候选顺序。晋升需要先配置持久化目录。",
  "ml.state_dir": "持久化目录",
  "ml.state_dir_hint": "轨迹与当前模型指针的落盘位置。留空=仅内存，重启丢失。例：ml",
  "ml.exploration": "探索概率",
  "ml.exploration_hint": "0 = 确定性；>0 会探索其他候选，但会阻止晋升。",
  "ml.config_shadow_off_hint": "Shadow 未开启 — 每个请求都在入库边界被丢弃。请打开以采集真实样本。",
  "ml.config_effect": "采集中",
  "ml.effect_collecting": "是 — 样本随流量增长",
  "ml.effect_idle": "否 — Shadow 未开启",
  "ml.config_serve": "生效方式",
  "ml.serve_active": "学习模型重排候选顺序",
  "ml.serve_deterministic": "确定性路由(不重排)",
  "ml.serve_blocked_hint": "学习型路由已开启，但未配置持久化目录，因此无法晋升也无法跨重启保留。",

  // ------------------------------------------------------------ learned routing
  "ml.title": "学习型路由",
  "ml.hint": "训练出的模型是否可以重排 provider 顺序。关闭时路由保持确定性。",
  "ml.unavailable": "此版本不含学习栈，路由保持确定性。",
  "ml.serving": "已提升的模型正在重排 provider 顺序。",
  "ml.inert": "模型已提升但学习型路由为关闭，不会被调用。",
  "ml.off": "未安装模型。路由为确定性路由。",
  "ml.exploring": "探索 {pct}%",
  "ml.no_state_dir": "未配置持久状态目录，重启后数据不会保留，也无法晋级模型。",
  "ml.installed": "已安装",
  "ml.none": "无",
  "ml.gate": "授权依据",
  "ml.baseline": "胜过 {name}",
  "ml.paired": "{n} 个配对请求",
  "ml.holdout_loss": "留出集损失",
  "ml.ranking": "已排序 / 已回退",
  "ml.explored": "已探索 / 已排序",
  "ml.blind_explored": "无模型时尝试",
  "ml.with_no_model": "无模型时为采集证据而尝试",
  "ml.never_tried": "从未尝试",
  "ml.blind_spot_warning":
    "已配置的模型在探索关闭时收不到流量。请把它们设为最高优先级，或开启探索(会阻止学习模型晋级)。",
  "ml.blind_spot_count": "{n} 个无法触达",
  "ml.collected": "已采集",
  "ml.samples": "样本",
  "ml.traces": "轨迹",
  "ml.criteria": "门禁条件",
  "ml.criterion.baseline_present": "基线已配对",
  "ml.criterion.paired_evidence": "配对证据量",
  "ml.criterion.holdout_quality": "留出集质量",
  "ml.criterion.routing_utility": "路由效用",
  "ml.criterion.selection_safety": "选择安全性",
  "ml.criterion.success_regression": "成功回退次数",
  "ml.criterion.cost_regression": "成本回退",
  "ml.criterion.latency_regression": "延迟回退",
  "ml.criterion.dataset_identity": "数据集一致",
  "ml.criterion_value": "实测 {measured} / 门槛 {threshold}",
  "ml.history": "提升历史",
  "ml.promoted": "已提升",
  "ml.rolled_back": "已回滚",
  "ml.replay": "用我的流量回放",
  "ml.replaying": "回放中…",
  "ml.replay_none": "回放没有产生任何测量结果。",
  "ml.replay_records": "已回放记录数",
  "ml.replay_bound": "基于最近 {n} 条记录。历史中的数据多于这个窗口所覆盖的。",
  "ml.agreement": "与生产一致率",
  "ml.disagreements": "不一致 / 已测量",
  "ml.measured": "已测量",
  "ml.regret": "平均遗憾",
  "ml.utility_delta": "平均实测效用差",
  "ml.alternatives": "备选",
  "ml.harmful": "有害",
  "ml.helpful": "有益",
  "ml.gaps": "不可用记录：",
  "ml.rollback_action": "回滚到上一个模型",
  "ml.rolling_back": "回滚中…",

  // ----------------------------------------------- learned routing: collection
  "ml.learning_blocked":
    "学习路由已开启，但影子评估关闭，未收集样本(已丢弃 {n} 次)。请开启影子评估。",
  "ml.learning_blocked_short": "学习已被阻断",
  "ml.ingested": "已入库请求",
  "ml.dropped_no_decision": "因缺少决策而丢弃",

  // -------------------------------------------- learned routing: promotion round
  "ml.round": "提升轮次",
  "ml.round_hint": "用已记录历史训练候选，并由门禁判断是否优于基线。不会自动安装或定时执行。",
  "ml.round_judge": "判断候选模型",
  "ml.round_judging": "判断中…",
  "ml.round_judged": "候选模型已评判，未安装。",
  "ml.round_install": "安装已获授权的候选模型",
  "ml.round_installing": "安装中…",
  "ml.round_install_hint":
    "门禁通过候选后才可用。会改变之后请求使用的模型。",
  "ml.round_none": "无法运行轮次。",
  "ml.round_records": "已读记录数",
  "ml.round_candidate": "候选模型",
  "ml.round_choices": "候选模型本会如何选择",
  "ml.round_single_choice":
    "候选在所有请求上都选择同一项，不构成学习到的排序。",
  "ml.round_installed": "已安装并正在服务",
  "ml.round_not_installed": "已评判，未安装",
  "ml.round_serving_now": "当前正在服务",
  "ml.round_deterministic": "路由为确定性路由",
  "ml.verdict": "结论",
  "ml.verdict_promoted": "已提升",
  "ml.verdict_rejected": "已拒绝",
  "ml.verdict_blocked": "已阻断",
  "ml.round_error": "轮次已评判，但无法安装",

  // ------------------------------------------------ learned routing: the log
  "ml.log": "已记录的历史",
  "ml.log_hint": "训练所用的记录。清理需手动执行，不会自动影响下一个模型。",
  "ml.log_records": "记录数",
  "ml.log_size": "占用空间",
  "ml.log_path": "文件",
  "ml.log_unreadable": "无法统计该日志，这不代表零条记录。",
  "ml.log_appended": "已追加",
  "ml.log_refused": "已拒绝",
  "ml.log_io_errors": "写入错误",
  "ml.log_empty": "尚未记录任何历史。",
  "ml.log_clear": "删除已记录的历史",
  "ml.log_clearing": "删除中…",
  "ml.log_cleared": "已删除 {bytes} 的已记录历史。",
  "ml.log_clear_nothing": "没有已记录的内容可删除。",
  "confirm.clear_traces": "删除已记录的历史",
  "confirm.clear_traces_body":
    "将永久删除 {n} 条请求记录。之后的训练只使用此后写入的记录。",
  "bytes.value": "{n} 字节",
};

export const dicts: Record<Lang, Dict> = { en: en as Dict, zh };

function interpolate(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name) =>
    params[name] !== undefined ? String(params[name]) : match,
  );
}

export function detectLang(): Lang {
  const stored = localStorage.getItem("zroutery-lang");
  if (stored === "en" || stored === "zh") return stored;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

/**
 * Re-state the backend errors an operator can act on, in their language.
 *
 * Backend messages are English technical strings and most of them are fine
 * that way — they name a setting or an action and paraphrasing would throw
 * that away. The handful that arrive as raw OS text (a taken port reads as a
 * localised socket paragraph) are matched here; everything else passes
 * through unchanged. Takes `t` rather than calling the hook so non-component
 * error paths can use it too.
 */
export function describeBackendError(
  t: (key: TKey, params?: Record<string, string | number>) => string,
  raw: string,
): string {
  const bind = raw.match(/cannot bind (\S+)/);
  if (bind) {
    if (/already in use|os error 10048/i.test(raw)) {
      return t("err.bind_in_use", { addr: bind[1] });
    }
    if (/os error 10013|access denied|permission denied/i.test(raw)) {
      return t("err.bind_denied", { addr: bind[1] });
    }
    // Any other reason the socket could not be taken is still the socket, and
    // still the one thing the operator can change.
    const detail = raw.match(/cannot bind \S+: (.+)$/);
    return t("err.bind_other", { addr: bind[1], err: detail ? detail[1] : "" });
  }
  // The panel refused the credential. The two shapes it takes are the same
  // advice with different causes: a stored relay key (refused at construction,
  // by name) and a rejected token (the 401 the probe surfaces). Both are fixed
  // by storing the panel's access token, so one wording serves both.
  if (/relay API key/i.test(raw)) {
    return t("err.relay_key");
  }
  if (/missing or invalid credentials/i.test(raw)) {
    return t("err.credentials_rejected");
  }
  if (/no panel access token is stored/i.test(raw)) {
    return t("err.no_panel_token");
  }
  // The browser token capture: four failures an operator can act on.
  if (/a token capture is already open/i.test(raw)) {
    return t("err.capture_open");
  }
  if (/no Chromium browser was found/i.test(raw)) {
    return t("err.no_browser");
  }
  if (/the panel rejected the captured token/i.test(raw)) {
    return t("err.capture_rejected");
  }
  if (/the browser was closed before a token was captured/i.test(raw)) {
    return t("err.capture_closed");
  }
  // The panel-backed balance path. Two conditions an operator can act on, and
  // both would otherwise arrive as raw English from the desktop layer.
  //
  // The first is the usual cause: the account has no panel token, so the
  // panel refused the account's own key — or there is no account at all. The
  // provider's name is carried because it is the thing to act on.
  const noBalance = raw.match(/no account of (.+?) reported a balance/);
  if (noBalance) {
    return t("err.no_account_balance", { provider: noBalance[1] });
  }
  // The second is a declaration that cannot be summed. The currencies are
  // named by the backend, and the message is the same either way: a figure
  // that cannot be spent is not a balance.
  if (/different currencies/i.test(raw)) {
    return t("err.mixed_currencies");
  }
  // A config issue that crosses the bridge in English; the model name, when the
  // message carries one, is the part worth keeping verbatim.
  const candidate = raw.match(/`([^`]+)` is not a configured model/);
  if (candidate) {
    return t("err.classifier_candidate", { model: candidate[1] });
  }

  // The reassurance class: a failure that says what is still in place and what
  // to do about it. These are the messages an operator meets when something
  // has gone wrong, and every one of them carries a clause that is advice
  // rather than a technical string — so every one of them is re-stated here.
  //
  // The technical half of each message (the path, the address, the operating
  // system's own words) is carried through verbatim, because it is the part
  // that identifies the thing to act on.
  const preserved = raw.match(/cannot preserve (.+?) as (.+?): ([^;]+); it was left as it is/);
  if (preserved) {
    return t("err.preserve_failed", {
      path: preserved[1],
      backup: preserved[2],
      err: preserved[3],
    });
  }
  const backupExists = raw.match(/cannot preserve (.+?): (.+?) already exists/);
  if (backupExists) {
    return t("err.backup_exists", { path: backupExists[1], backup: backupExists[2] });
  }
  const copyUnreadable = raw.match(/the copy of (.+?) in (.+?) cannot be read back \(([^)]+)\)/);
  if (copyUnreadable) {
    return t("err.copy_unreadable", {
      path: copyUnreadable[1],
      backup: copyUnreadable[2],
      err: copyUnreadable[3],
    });
  }
  const copyMismatch = raw.match(/the copy of (.+?) in (.+?) does not match it/);
  if (copyMismatch) {
    return t("err.copy_mismatch", { path: copyMismatch[1], backup: copyMismatch[2] });
  }
  const changeRefused = raw.match(
    /the change to (.+?) was refused and the previous configuration and gateway are still in place \(([^)]+)\)/,
  );
  if (changeRefused) {
    return t("err.change_refused", { addr: changeRefused[1], err: changeRefused[2] });
  }
  const stillListening = raw.match(
    /cannot use the new address: ([^;]+); the gateway is still listening on (.+?) and the configuration was not changed/,
  );
  if (stillListening) {
    return t("err.address_not_changed_running", {
      err: stillListening[1],
      addr: stillListening[2],
    });
  }
  const addressNotChanged = raw.match(
    /cannot use the new address: ([^;]+); the configuration was not changed/,
  );
  if (addressNotChanged) {
    return t("err.address_not_changed", { err: addressNotChanged[1] });
  }
  const listenFailed = raw.match(/cannot listen on (.+?): ([^;]+); the previous gateway was restored/);
  if (listenFailed) {
    return t("err.listen_failed_restored", { addr: listenFailed[1], err: listenFailed[2] });
  }
  const listenNotRestored = raw.match(
    /cannot listen on (.+?): ([^;]+); the previous gateway could not be restored either: (.+)$/,
  );
  if (listenNotRestored) {
    return t("err.listen_failed_not_restored", {
      addr: listenNotRestored[1],
      err: listenNotRestored[2],
      restore: listenNotRestored[3],
    });
  }
  const noSnapshot = raw.match(/no rollback snapshot could be taken of (.+?) —/);
  if (noSnapshot) {
    return t("err.no_rollback_snapshot", { path: noSnapshot[1] });
  }
  const noAccount = raw.match(/(?:no|unknown) account `?(.+?)`? on provider `?(.+?)`?$/);
  if (noAccount) {
    return t("err.no_such_account", { account: noAccount[1], provider: noAccount[2] });
  }
  return raw;
}

type I18n = {
  lang: Lang;
  setLang: (lang: Lang) => void;
  t: (key: TKey, params?: Record<string, string | number>) => string;
  /** For "singular | plural" entries: picks a side by the count. */
  plural: (n: number, key: TKey, params?: Record<string, string | number>) => string;
};

const I18nContext = createContext<I18n | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLang] = useState<Lang>(detectLang);

  useEffect(() => {
    localStorage.setItem("zroutery-lang", lang);
    document.documentElement.lang = lang;
  }, [lang]);

  const t = useCallback(
    (key: TKey, params?: Record<string, string | number>) =>
      interpolate(dicts[lang][key] ?? dicts.en[key], params),
    [lang],
  );

  const plural = useCallback(
    (n: number, key: TKey, params?: Record<string, string | number>) => {
      const value = dicts[lang][key] ?? dicts.en[key];
      const [one, many] = value.split("|").map((side) => side.trim());
      return interpolate((n === 1 ? one : many ?? one) ?? value, { n, ...params });
    },
    [lang],
  );

  return (
    <I18nContext.Provider value={{ lang, setLang, t, plural }}>{children}</I18nContext.Provider>
  );
}

export function useI18n(): I18n {
  const context = useContext(I18nContext);
  if (!context) throw new Error("useI18n needs an I18nProvider");
  return context;
}
