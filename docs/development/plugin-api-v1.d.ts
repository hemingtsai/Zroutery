/**
 * Proposed Zroutery plugin API v1; design contract, not a shipped SDK.
 * Authority, validation and lifecycle rules: plugin-api-design.md.
 * User constraints: all request/response/protocol work stays in existing Rust
 * foundations; app tokens use the existing keyring; every authority is declared.
 * Only control/UI DTOs cross the JS boundary. TypeScript brands are not auth.
 */

export type Json = null | boolean | number | string | readonly Json[] | JsonObject;
export interface JsonObject { readonly [key: string]: Json }
export type Facet = "control" | "ui";
export type Locale = "en" | "zh-CN" | "zh-TW";
export type Localized = Readonly<Record<Locale, string>>;

declare const host_handle: unique symbol;
export type Handle<K extends string> = { readonly [host_handle]: K };
/** Opaque wire token. Its authority comes from the host single-use table. */
export type UiActionRef = string & { readonly [host_handle]: "ui_action" };
export type Revision = string;
export type NonEmpty<T> = readonly [T, ...T[]];

export interface ApiVersion {
  readonly major: 1;
  readonly min_minor: number;
}

export interface ResourceLimits {
  readonly heap_bytes: number;
  readonly stack_bytes: number;
  readonly active_js_ms: number;
  readonly wall_ms: number;
  readonly host_calls: number;
  readonly ipc_bytes: number;
}

export type Capability =
  | "provider.register"
  | "host.operation.bind"
  | "credential.store"
  | "credential.use"
  | "credential.status"
  | "credential.refresh"
  | "credential.clear"
  | "client.register"
  | "client.setup.propose"
  | "client.setup.restore"
  | "transport.profile.use"
  | "transport.profile.declare"
  | "ui.contribute"
  | "ui.projection.read"
  | "host.action.request"
  | "options.read"
  | "options.update"
  | "diagnostic.report"
  | "theme.contribute";

export interface PermissionBase {
  /**
   * Manifest-local contribution ids. $plugin is reserved for own options/logging.
   * Wildcards, foreign identities and inferred scopes are not accepted.
   */
  readonly contributions: NonEmpty<string>;
  readonly optional?: boolean;
  readonly reason: Localized;
}

/** Required scope fields are explicit, including for data-only contributions. */
export type PermissionRequest = PermissionBase & (
  | {
      readonly capability: "provider.register" | "client.register" | "client.setup.propose" | "client.setup.restore";
      readonly principal: "host_binding";
    }
  | {
      readonly capability: "host.operation.bind";
      readonly principal: "host_binding";
      readonly operations: NonEmpty<string>;
      readonly origins: NonEmpty<string>;
    }
  | {
      readonly capability: "credential.store" | "credential.clear";
      readonly principal: "host_binding";
      readonly credential_slots: NonEmpty<string>;
    }
  | {
      readonly capability: "credential.use" | "credential.refresh";
      readonly principal: "host_binding";
      readonly credential_slots: NonEmpty<string>;
      readonly operations: NonEmpty<string>;
      readonly origins: NonEmpty<string>;
    }
  | {
      readonly capability: "credential.status";
      readonly principal: "ui";
      readonly credential_slots: NonEmpty<string>;
    }
  | {
      readonly capability: "transport.profile.use" | "transport.profile.declare";
      readonly principal: "host_binding";
      readonly profiles: NonEmpty<string>;
    }
  | { readonly capability: "ui.contribute"; readonly principal: "ui" }
  | {
      readonly capability: "ui.projection.read";
      readonly principal: "ui";
      readonly projections: NonEmpty<string>;
    }
  | {
      readonly capability: "host.action.request";
      readonly principal: "ui";
      readonly actions: NonEmpty<string>;
    }
  | { readonly capability: "options.read" | "options.update" | "diagnostic.report"; readonly principal: Facet }
  | { readonly capability: "theme.contribute"; readonly principal: "theme" }
);

export interface PluginManifestV1 {
  readonly manifest_version: 1;
  readonly id: string;
  readonly version: string;
  readonly name: Localized;
  readonly host_api: ApiVersion;
  /** null for any data-only package, including providers and themes. */
  readonly runtime: null | {
    readonly engine: "quickjs";
    readonly entries: Partial<Readonly<Record<Facet, string>>>;
  };
  readonly requires: readonly { readonly service: string; readonly api: ApiVersion }[];
  readonly permissions: readonly PermissionRequest[];
  readonly contributions: {
    readonly providers?: readonly ProviderDescriptor[];
    readonly clients?: readonly ClientDescriptor[];
    readonly themes?: readonly { readonly id: string; readonly file: string }[];
    readonly transport_profiles?: readonly { readonly id: string; readonly file: string }[];
    readonly ui?: readonly UiContribution[];
    readonly actions?: readonly HostActionBinding[];
  };
  readonly options_schema?: string;
  readonly diagnostics?: Readonly<Record<string, Localized>>;
  /** Requested ceilings; the host's lower effective ceilings always win. */
  readonly limits?: Partial<ResourceLimits>;
}

/**
 * The host-authoritative options contract: declarative, bounded, no execution.
 * The plugin's validate_options hook only adds errors to what this schema
 * accepts; it can never relax it. No regex, `$ref`, conditionals or defaults.
 */
export interface OptionsSchemaV1 {
  readonly schema_version: number;
  readonly fields: Readonly<Record<string, FieldRuleV1>>;
}

export type FieldRuleV1 =
  | StringRuleV1
  | IntegerRuleV1
  | NumberRuleV1
  | BooleanRuleV1
  | ArrayRuleV1
  | ObjectRuleV1;

export type ScalarRuleV1 = StringRuleV1 | IntegerRuleV1 | NumberRuleV1 | BooleanRuleV1;

export interface StringRuleV1 {
  readonly type: "string";
  readonly required?: boolean;
  /** Bytes, matching the host's strict text quotas. */
  readonly min_length?: number;
  /** Capped at 16384; the host default is 4096. */
  readonly max_length?: number;
  readonly enum?: readonly string[];
}
export interface IntegerRuleV1 {
  readonly type: "integer";
  readonly required?: boolean;
  readonly min?: number;
  readonly max?: number;
}
export interface NumberRuleV1 {
  readonly type: "number";
  readonly required?: boolean;
  readonly min?: number;
  readonly max?: number;
}
export interface BooleanRuleV1 {
  readonly type: "boolean";
  readonly required?: boolean;
}
/** Depth is frozen at 2: items may be scalars or objects of scalars. */
export interface ArrayRuleV1 {
  readonly type: "array";
  readonly required?: boolean;
  readonly min_items?: number;
  readonly max_items?: number;
  readonly items: ItemRuleV1;
}
export interface ObjectRuleV1 {
  readonly type: "object";
  readonly required?: boolean;
  readonly properties: Readonly<Record<string, ScalarRuleV1>>;
}
/** An array item may be an object of scalars; containers never nest deeper. */
export type ItemRuleV1 = ScalarRuleV1 | ObjectRuleV1;

/** A registered Rust host service, not a plugin module or a dynamic library. */
export interface HostServiceRef {
  readonly id: string;
  readonly api: ApiVersion;
}

export interface HostOperationBinding {
  readonly id: string;
  /** Service registry determines effects, HTTP method, codecs and accounting. */
  readonly service: HostServiceRef;
  readonly base_url: string;
  readonly credential_slots: readonly string[];
  /** Host service schema validates all fields; no JS expressions or templates. */
  readonly service_options?: { readonly bearer_auth?: boolean; readonly stream_usage?: boolean };
}

export interface ProviderDescriptor {
  readonly id: string;
  readonly name: Localized;
  readonly operations: readonly HostOperationBinding[];
  readonly credentials: readonly CredentialSlotDeclaration[];
  /** Host executes fixed profile rules; this is never chosen by a request hook. */
  readonly transport_profile?: string;
  /**
   * Static, non-secret identity headers merged into every request this
   * provider sends: client-look headers only (`user-agent`, `accept`, the
   * gateway's `x-client-*` family). Names come from the host's published
   * allowlist — the same closed set transport profiles use — so auth and
   * framing names (`authorization`, `x-api-key`, `content-type`, …) can
   * never travel through it. Values are bounded text.
   */
  readonly extra_headers?: Readonly<Record<string, string>>;
}

export interface CredentialSlotDeclaration {
  /** Local slot name; not a key_ref, OS service name or credential lookup path. */
  readonly id: string;
  readonly name: Localized;
  readonly kind: "api_key" | "app_token" | "oauth_bundle" | "session_cookie";
  readonly scope: "provider_instance" | "account";
  readonly storage: "host_keyring";
  readonly acquisition:
    | { readonly kind: "host_secret_form" }
    | { readonly kind: "host_auth"; readonly service: HostServiceRef; readonly operation: string };
  /** Injection/exchange/signing occurs within this versioned Rust service. */
  readonly auth_service: HostServiceRef;
}

/** Data only. Rust accepts only its published TLS/HTTP knob allowlists. */
export interface TransportProfileV1 {
  readonly schema_version: 1;
  readonly id: string;
  /** A publisher claim, not host-verified fingerprint evidence. */
  readonly target: { readonly family: string; readonly version: string };
  readonly tls: {
    readonly min_version: "1.2" | "1.3";
    readonly max_version: "1.2" | "1.3";
    readonly cipher_suites: NonEmpty<number>;
    readonly groups: NonEmpty<number>;
    readonly signature_algorithms: NonEmpty<number>;
    readonly extension_order: readonly string[];
    readonly alpn: NonEmpty<"http/1.1" | "h2">;
    readonly grease: boolean;
  };
  readonly http: {
    readonly h1?: { readonly header_order: readonly string[] };
    readonly h2?: {
      readonly settings: readonly { readonly id: number; readonly value: number }[];
      readonly pseudo_header_order: readonly (":method" | ":authority" | ":scheme" | ":path")[];
      readonly connection_window: number;
    };
  };
  /** Static, non-secret identity values only; auth/framing names are denied. */
  readonly identity_headers: Readonly<Record<string, string>>;
  /** No peer-verification, CA, SNI-override, proxy or socket escape options. */
}

export interface CallContext {
  readonly call_id: string;
  readonly plugin_generation: Revision;
  readonly grant_epoch: Revision;
  /** Present only with this facet's explicit options.read grant. */
  readonly options?: JsonObject;
  readonly limits: ResourceLimits;
  readonly cancelled: () => boolean;
  /** Codes are declared in the package; production never persists raw JS text. */
  readonly report?: (code: string) => void;
}

export interface OptionsInput {
  readonly schema_version: number;
  readonly revision: Revision;
  readonly options: JsonObject;
}

export interface OptionsValidation {
  /** Adds field errors; it can never relax the host schema's validation. */
  readonly errors: readonly { readonly field: string; readonly message_code: string }[];
}

export interface OptionsMigrationInput extends OptionsInput {
  readonly target_schema_version: number;
}

export interface ClientDescriptor {
  readonly id: string;
  readonly name: Localized;
  readonly ingress?: {
    readonly route_suffix: string;
    /** Rust host registry owns authentication, codecs and request handling. */
    readonly service: HostServiceRef;
  };
  readonly setup?: {
    /** Host-reviewed config schema, never a free-form filesystem path. */
    readonly target_schema: string;
    readonly service: HostServiceRef;
    readonly managed_slots: readonly ("gateway_url" | "gateway_key" | "default_model" | "class_catalog")[];
  };
}

export type HostActionBinding = { readonly id: string } & (
  | { readonly kind: "refresh_catalog" | "refresh_account"; readonly provider: string; readonly operation: string }
  | { readonly kind: "start_sign_in" | "store_credential" | "forget_credential"; readonly provider: string; readonly credential_slot: string }
  | { readonly kind: "preview_client_setup" | "restore_client_setup"; readonly client: string }
  | { readonly kind: "save_options" }
);

export interface UiContribution {
  readonly id: string;
  readonly slot: "overview.extension" | "provider.detail.extension" | "model.detail.extension" | "client.detail.extension" | "settings.extension" | "settings.extensions.page" | "usage.extension";
  readonly title: Localized;
  readonly projection?: string;
  readonly render_hook: string;
  readonly action_hook?: string;
}

export type UiNode =
  | { readonly kind: "stack"; readonly children: readonly UiNode[] }
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "notice"; readonly tone: "info" | "warning" | "error"; readonly text: string }
  | { readonly kind: "value"; readonly label: string; readonly value: string }
  | { readonly kind: "button"; readonly label: string; readonly action_id: string }
  | { readonly kind: "input"; readonly field_id: string; readonly label: string; readonly value: string }
  | { readonly kind: "select"; readonly field_id: string; readonly label: string; readonly value: string; readonly options: readonly { readonly label: string; readonly value: string }[] }
  | { readonly kind: "table"; readonly columns: readonly string[]; readonly rows: readonly (readonly string[])[] };

export interface UiInput {
  readonly contribution_id: string;
  readonly locale: Locale;
  readonly view_revision: Revision;
  /** Empty without a projection grant; never an HTTP response or Core state. */
  readonly projection: {
    readonly status?: string;
    readonly model_count?: number;
    readonly account_count?: number;
    readonly coverage?: string;
  };
}

export interface UiActionInput extends UiInput {
  readonly action: UiActionRef;
  readonly action_id: string;
  readonly fields: Readonly<Record<string, string>>;
}

export type UiEffect =
  /** Only for a clicked save_options action with options.update permission. */
  | { readonly kind: "options_proposal"; readonly expected_revision: Revision; readonly options: JsonObject }
  /** Must match the clicked action_id; target/slot/operation are host-bound. */
  | { readonly kind: "host_action"; readonly action_id: string }
  | { readonly kind: "none" };

export type ColorExpr =
  | { readonly hex: string }
  | { readonly role: string }
  | { readonly system: "accent" | "accent_foreground"; readonly fallback: ColorExpr }
  | {
      /**
       * MD3 dynamic color: keeps the OS accent's hue and takes the given
       * OKLCH lightness and chroma ceiling, so a pack declares the baseline
       * role's tone while the hue follows the platform accent. `fallback`
       * doubles as the no-accent seed and the literal color an engine
       * without relative-color syntax falls back to.
       */
      readonly tonal: {
        readonly of: "accent" | "accent_foreground";
        readonly lightness: number;
        readonly chroma: number;
        readonly fallback: ColorExpr;
      };
    }
  | { readonly mix: readonly [ColorExpr, ColorExpr]; readonly first_fraction: number };

/**
 * Host motion tokens a skin may retune: the three duration slots every host
 * transition reads (`--t-fast/--t-page/--t-drawer`) and the shared easing
 * curve (`--ease`), chosen by name from the host's closed curve set. Absent
 * keeps the industrial defaults.
 */
export interface ThemeMotion {
  readonly fast_ms: number;
  readonly page_ms: number;
  readonly drawer_ms: number;
  readonly ease:
    | "standard"
    | "emphasized_accelerate"
    | "emphasized_decelerate"
    | "easy_ease"
    | "decelerate_max"
    | "accelerate_max"
    | "expo_out";
}

export interface ShadowValue {
  readonly x: number;
  readonly y: number;
  readonly blur: number;
  readonly spread: number;
  readonly color: ColorExpr;
  readonly inset?: boolean;
}

export interface ComponentState {
  readonly interaction: "rest" | "hover" | "pressed";
  readonly focus_visible?: boolean;
  readonly disabled?: boolean;
  readonly selected?: boolean;
  readonly checked?: boolean;
  readonly readonly?: boolean;
  readonly context?: "page" | "section" | "nested_section" | "overlay";
}

export interface ComponentRecipe {
  /** Names map to the host's published parts/variants; no selectors. */
  readonly part: string;
  readonly variant?: string;
  /** Supports checked+hover, selected+focus and other real component states. */
  readonly when: ComponentState;
  readonly fill?: ColorExpr;
  readonly foreground?: ColorExpr;
  readonly stroke?: { readonly width: number; readonly color: ColorExpr };
  /**
   * Additive in v1 (2026-11-14): a two-stop gradient stroke, the WinUI
   * ControlElevationBorderBrush signature. Mutually exclusive with `stroke`
   * and requires `fill` — the fill is re-emitted as the padding-box layer of
   * the border-box gradient composite, which is what keeps the gradient in
   * the ring. `angle` is a css linear-gradient angle (default 180 = to top
   * edge first, matching the brush's StartPoint→EndPoint).
   */
  readonly stroke_gradient?: {
    readonly width: number;
    readonly from: ColorExpr;
    readonly to: ColorExpr;
    readonly angle?: number;
  };
  readonly radius?: number;
  readonly shadow?: readonly ShadowValue[];
  readonly focus_rings?: readonly { readonly width: number; readonly offset: number; readonly color: ColorExpr }[];
  readonly decoration?: "none" | "underline" | "leading_indicator" | "state_layer" | "inherit_ink" | "plain_title" | "circle" | "indicator_anchor" | "nav_indicator_morph";
  readonly layout?: "flat" | "filled_section" | "flatten_nested" | "card_stack" | "nav_rail";
  readonly material?: { readonly kind: "solid" | "acrylic"; readonly opacity: number; readonly blur: number; readonly saturation?: number };
  readonly stroke_edge?: "all" | "start" | "end" | "bottom";
  /** Each part has its own published bounds; these cannot position an overlay. */
  readonly metrics?: {
    readonly width?: number;
    readonly height?: number;
    readonly padding_block?: number;
    readonly padding_inline?: number;
    readonly font_size?: number;
    readonly font_weight?: number;
    readonly padding_top?: number;
    readonly padding_bottom?: number;
    readonly padding_start?: number;
    readonly gap?: number;
    readonly column_gap?: number;
    /** Host-published switch.thumb geometry only; never arbitrary positioning. */
    readonly thumb_x?: number;
    readonly thumb_y?: number;
  };
}

export interface ThemeV1 {
  readonly schema_version: 1;
  readonly name: Localized;
  readonly modes: Readonly<Record<"light" | "dark", {
    readonly colors: Readonly<Record<string, ColorExpr>>;
    readonly recipes: readonly ComponentRecipe[];
  }>>;
  readonly typography: {
    readonly family: "host:industrial" | "host:roboto" | "system:segoe_ui";
    readonly scale: "compact" | "material_desktop" | "fluent_desktop";
  };
  readonly metrics: {
    readonly control_height: number;
    readonly segment_height: number;
    readonly switch_width: number;
    readonly switch_height: number;
  };
  readonly motion?: ThemeMotion;
}

export interface PluginHooksV1 {
  /** Requires control options.read; only adds errors to the host validator. */
  readonly validate_options?: (input: OptionsInput, ctx: CallContext) => OptionsValidation;
  /** Requires control options.read + options.update and a host migration intent. */
  readonly migrate_options?: (input: OptionsMigrationInput, ctx: CallContext) => JsonObject;
  readonly ui_render?: Readonly<Record<string, (input: UiInput, ctx: CallContext) => UiNode>>;
  readonly ui_action?: Readonly<Record<string, (input: UiActionInput, ctx: CallContext) => UiEffect>>;
}

/** Pipe identity supplies plugin/principal/scope; no caller-provided owner id. */
export interface WorkerEnvelope<T> {
  readonly ipc_version: 1;
  readonly call_id: string;
  readonly worker_generation: Revision;
  readonly grant_epoch: Revision;
  readonly value: T;
}

export type WorkerCall = WorkerEnvelope<
  | { readonly kind: "validate_options"; readonly input: OptionsInput }
  | { readonly kind: "migrate_options"; readonly input: OptionsMigrationInput }
  | { readonly kind: "ui_render"; readonly hook: string; readonly input: UiInput }
  | { readonly kind: "ui_action"; readonly hook: string; readonly input: UiActionInput }
>;

export type PluginErrorCode =
  | "permission_denied"
  | "unsupported_host_service"
  | "invalid_manifest"
  | "invalid_result"
  | "stale_revision"
  | "plugin_busy"
  | "plugin_timeout"
  | "plugin_memory_limit"
  | "plugin_crashed"
  | "sandbox_unavailable"
  | "credential_unavailable"
  | "storage_unavailable";

export type WorkerReply = WorkerEnvelope<
  | { readonly kind: "options_validation"; readonly result: OptionsValidation }
  | { readonly kind: "options_migration"; readonly result: JsonObject }
  | { readonly kind: "ui_tree"; readonly result: UiNode }
  | { readonly kind: "ui_effect"; readonly result: UiEffect }
  | { readonly kind: "error"; readonly code: PluginErrorCode }
>;

/** Pure control/UI hooks only; no request, response, network or secret API. */
export declare function define_plugin(hooks: PluginHooksV1): PluginHooksV1;
