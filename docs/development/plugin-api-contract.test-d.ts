/** Compile-only design assertions; these do not prove runtime enforcement. */
import type {
  ArrayRuleV1,
  CallContext,
  Capability,
  CredentialSlotDeclaration,
  FieldRuleV1,
  IntegerRuleV1,
  ItemRuleV1,
  NumberRuleV1,
  ObjectRuleV1,
  OptionsSchemaV1,
  PermissionRequest,
  PluginHooksV1,
  PluginManifestV1,
  StringRuleV1,
  WorkerCall,
} from "./plugin-api-v1";

type Assert<T extends true> = T;
type Equal<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
type IsNever<T> = [T] extends [never] ? true : false;

export type NoRequestOrRawSecretAuthority = Assert<IsNever<Extract<
  Capability,
  | "provider.prepare"
  | "provider.payload.read"
  | "provider.payload.replace"
  | "provider.stream.decode"
  | "credential.read"
  | "credential.list"
  | "credential.export"
  | "network.fetch"
>>>;

export type OnlyFourControlHooks = Assert<Equal<
  keyof PluginHooksV1,
  "validate_options" | "migrate_options" | "ui_render" | "ui_action"
>>;

export type OnlyControlMessages = Assert<Equal<
  WorkerCall["value"]["kind"],
  "validate_options" | "migrate_options" | "ui_render" | "ui_action"
>>;

export type KeyringOnly = Assert<Equal<
  CredentialSlotDeclaration["storage"],
  "host_keyring"
>>;

export type NoAmbientHostHandles = Assert<IsNever<Extract<
  keyof CallContext,
  "body" | "request" | "response" | "fetch" | "keyring" | "secrets" | "fs" | "invoke"
>>>;

export type CredentialScopeCannotBeImplicit = Assert<Equal<
  {
    readonly capability: "credential.use";
    readonly principal: "host_binding";
    readonly contributions: readonly ["coding"];
    readonly reason: { readonly en: "Use token"; readonly "zh-CN": "使用令牌"; readonly "zh-TW": "使用權杖" };
  } extends PermissionRequest ? true : false,
  false
>>;

export type PermissionVocabularyAgrees = Assert<Equal<PermissionRequest["capability"], Capability>>;
export type ManifestRequiresPermissionList = Assert<Equal<
  Omit<PluginManifestV1, "permissions"> extends PluginManifestV1 ? true : false,
  false
>>;

/** The options schema is declarative data: closed vocabulary, no execution. */
export type OptionsSchemaTopLevelIsVersionAndFields = Assert<Equal<
  keyof OptionsSchemaV1,
  "schema_version" | "fields"
>>;

export type OptionsRuleVocabularyIsClosed = Assert<Equal<
  FieldRuleV1["type"],
  "string" | "integer" | "number" | "boolean" | "array" | "object"
>>;

/** Object properties are scalars: an object can never nest a container. */
export type ObjectPropertiesCannotNestContainers = Assert<Equal<
  ObjectRuleV1["properties"][string]["type"],
  "string" | "integer" | "number" | "boolean"
>>;

/** Depth is frozen at 2: array items are scalars or objects of scalars. */
export type ArrayItemsCannotNestArrays = Assert<Equal<
  Exclude<ItemRuleV1["type"], "object">,
  "string" | "integer" | "number" | "boolean"
>>;

/** No default injection, no pattern matching, no conditional logic. */
export type OptionsRulesCarryNoExecutionSurface = Assert<IsNever<
  Extract<
    keyof StringRuleV1 | keyof IntegerRuleV1 | keyof NumberRuleV1 | keyof ArrayRuleV1 | keyof ObjectRuleV1,
    "pattern" | "format" | "default" | "if" | "then" | "$ref" | "oneOf"
  >
>>;
