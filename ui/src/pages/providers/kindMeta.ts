/**
 * Provider-kind metadata and the pure defaults that mirror
 * `zroutery_core::config` — the label keys, the client profiles, and the
 * base-URL/profile/kind rules every provider surface shares.
 */
import type { ProviderClientProfile, ProviderKind } from "../../api";


export type KindLabelKey =
  | "providers.openai_dialect"
  | "providers.anthropic_dialect"
  | "providers.opencode_zen_dialect"
  | "providers.opencode_console_dialect"
  | "providers.decision_dialect";

export type ProfileLabelKey =
  | "providers.profile_auto"
  | "providers.profile_native"
  | "providers.profile_claude_code"
  | "providers.profile_codex"
  | "providers.profile_opencode_cli";

export const KINDS: { id: ProviderKind; labelKey: KindLabelKey }[] = [
  { id: "openai_compatible", labelKey: "providers.openai_dialect" },
  { id: "anthropic", labelKey: "providers.anthropic_dialect" },
  { id: "opencode_zen_free", labelKey: "providers.opencode_zen_dialect" },
  { id: "opencode_console", labelKey: "providers.opencode_console_dialect" },
  { id: "decision", labelKey: "providers.decision_dialect" },
];

/**
 * The provider kinds this build can actually serve.
 *
 * The two OpenCode kinds are configuration rather than code, so they survive
 * every build — but the request shaping that makes OpenCode's gateway accept a
 * request does not, and the gateway checks a fixed fingerprint and rejects
 * anything else. Offering them in a build that cannot shape the request is
 * offering a provider whose every request is refused, so they are offered only
 * where they work.
 */
export function kindsFor(opencodeAvailable: boolean): { id: ProviderKind; labelKey: KindLabelKey }[] {
  if (opencodeAvailable) return KINDS;
  return KINDS.filter((k) => k.id !== "opencode_zen_free" && k.id !== "opencode_console");
}

export const CLIENT_PROFILES: { id: ProviderClientProfile; labelKey: ProfileLabelKey }[] = [
  { id: "auto", labelKey: "providers.profile_auto" },
  { id: "native", labelKey: "providers.profile_native" },
  { id: "claude_code", labelKey: "providers.profile_claude_code" },
  { id: "codex", labelKey: "providers.profile_codex" },
  { id: "opencode_cli", labelKey: "providers.profile_opencode_cli" },
];

/**
 * The client profiles this build can actually shape a request with.
 *
 * The `opencode_cli` profile exists to make a request look like the OpenCode
 * CLI, which is what OpenCode's gateway accepts. Without the shaping behind
 * it, a request wearing that identity is refused — so the profile is offered
 * only where the shaping is.
 */
export function clientProfilesFor(opencodeAvailable: boolean): { id: ProviderClientProfile; labelKey: ProfileLabelKey }[] {
  if (opencodeAvailable) return CLIENT_PROFILES;
  return CLIENT_PROFILES.filter((c) => c.id !== "opencode_cli");
}

/**
 * Whether this kind targets OpenCode's gateway.
 *
 * Mirrors `ProviderKind::is_opencode`. Such a provider speaks the OpenAI
 * dialect but is not an OpenAI-compatible endpoint: the gateway checks a fixed
 * fingerprint and rejects anything else, which is why both the profile and the
 * discovery surface behave differently for these two kinds.
 */
export function isOpencodeKind(kind: ProviderKind): boolean {
  return kind === "opencode_zen_free" || kind === "opencode_console";
}

/**
 * Whether this kind answers only with a stored key.
 *
 * The Zen free gateway identifies its clients by a fixed fingerprint and sells
 * nothing, so it issues no keys: a free provider with the default
 * `provider:{id}` reference is *complete*, not missing a credential. The
 * reference-based check alone called every free provider "warn" — a yellow dot
 * with no action anywhere that could clear it, on the one kind of provider
 * that needs nothing.
 */
export function needsKey(kind: ProviderKind): boolean {
  return kind !== "opencode_zen_free";
}

/** Mirrors `ProviderKind::default_base_url` in `zroutery-core::config`. */
export function defaultBaseUrl(kind: ProviderKind): string {
  switch (kind) {
    case "anthropic":
      return "https://api.anthropic.com";
    case "opencode_zen_free":
      return "https://opencode.ai/zen";
    case "opencode_console":
      return "https://opencode.ai/inference/openai/v1";
    case "decision":
      return "https://api.typesafe.ai";
    default:
      return "https://api.openai.com/v1";
  }
}

/**
 * A readable provider name derived from a pasted endpoint.
 *
 * The add form accepts a URL alone because the endpoint is the thing an
 * operator has; making them invent a display name first is a step that adds
 * nothing. `https://ps.air-outer.com/v1` becomes `ps.air-outer`.
 */
export function nameFromBaseUrl(url: string): string {
  try {
    const host = new URL(url.trim()).hostname.toLowerCase();
    return host.replace(/^www\./, "") || "";
  } catch {
    return "";
  }
}

/**
 * The url a decision provider is asked at, mirroring `DecisionConfig::url` in
 * `zroutery-core::config`.
 *
 * The base is used as it stands and `/v1` is added to a path that does not
 * already begin with it, so `/v1/systemone` and `/systemone` name the same
 * endpoint — and a path that starts with neither still names one, just not the
 * one the operator may have meant. The drawer shows the result, because the
 * *Endpoint* row is where an operator checks the address and this is the
 * address the request goes to.
 */
export function decisionUrl(baseUrl: string, path: string): string {
  const base = baseUrl.replace(/\/+$/, "");
  const trimmed = path.trim();
  if (trimmed === "") return base;
  const segments = trimmed.split("/").filter(Boolean);
  const baseSegments = base.split("/").filter(Boolean);
  if (segments.length && segments.every((part, index) => part === baseSegments[baseSegments.length - segments.length + index])) return base;
  const suffix = `/${trimmed.replace(/^\/+/, "")}`;
  if (base.endsWith("/v1")) return `${base}${suffix.replace(/^\/v1\//, "/")}`;
  return suffix === "/v1" || suffix.startsWith("/v1/") ? `${base}${suffix}` : `${base}/v1${suffix}`;
}

/**
 * Mirrors the `client_profile` arm of `ProviderConfig::new`.
 *
 * The OpenCode arm is not a preference: `config.rs` rejects any OpenCode
 * provider whose configured profile is not `opencode_cli`, so creating or
 * re-kinding one without it produces a config that will not validate.
 *
 * A decision endpoint is `native` for the same kind of reason — not a
 * validation rule but a fact about the request. The decision body is not a
 * chat, so there is no client identity to impersonate, and `ProviderConfig::new`
 * files it as native. Offering `auto` here would let the dashboard create a
 * provider whose identity differs from the one the backend's own constructor
 * would have given it.
 */
export function defaultClientProfile(kind: ProviderKind): ProviderClientProfile {
  switch (kind) {
    case "anthropic":
      return "claude_code";
    case "opencode_zen_free":
    case "opencode_console":
      return "opencode_cli";
    case "decision":
      return "native";
    default:
      return "auto";
  }
}

/** The label `KINDS` gives a kind, for the places that show one directly. */
export function kindLabelKey(kind: ProviderKind): KindLabelKey {
  return KINDS.find((k) => k.id === kind)?.labelKey ?? "providers.openai_dialect";
}
