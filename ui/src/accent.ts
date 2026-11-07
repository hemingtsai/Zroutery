import type { SystemAccent } from "./api";

export const SYSTEM_ACCENT_PROPERTIES = [
  "--system-accent",
  "--system-accent-soft",
  "--system-accent-fg",
] as const;

export function applySystemAccent(
  accent: SystemAccent | null,
  style: Pick<CSSStyleDeclaration, "setProperty" | "removeProperty">,
): void {
  if (!accent) {
    for (const property of SYSTEM_ACCENT_PROPERTIES) style.removeProperty(property);
    return;
  }
  style.setProperty("--system-accent", accent.accent);
  style.setProperty("--system-accent-soft", accent.accent_soft);
  style.setProperty("--system-accent-fg", accent.accent_fg);
}
