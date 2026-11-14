/** Compile-only design assertions; these do not prove runtime enforcement. */
import type {
  CallContext,
  Capability,
  CredentialSlotDeclaration,
  PermissionRequest,
  PluginHooksV1,
  PluginManifestV1,
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
