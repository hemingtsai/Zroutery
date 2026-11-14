import md3Manifest from "../../../plugins/theme-md3/zroutery.plugin.json";
import fluentManifest from "../../../plugins/theme-fluent/zroutery.plugin.json";
import { compileTheme } from "./compiler";
import { INDUSTRIAL, loadThemeRef, type ThemeRef } from "./preferences";
import type { Localized, PluginManifestV1, ThemeV1 } from "./types";

export interface ThemeEntry {
  id: ThemeRef; name: Localized; digest: string; css: string | null; font: ThemeV1["typography"]["family"];
  enabled: boolean;
}
const entries = new Map<ThemeRef, ThemeEntry>();
const listeners = new Set<() => void>();
export function registerTheme(manifest: PluginManifestV1, digest: string, value: unknown, enabled: boolean): void {
  const contribution = manifest.contributions.themes?.[0];
  if (manifest.runtime !== null || !contribution || manifest.permissions.length !== 1
      || !manifest.permissions.some((p) => p.capability === "theme.contribute" && p.principal === "theme" && p.contributions.includes(contribution.id))) throw new Error("invalid_theme_manifest");
  const css = compileTheme(value);
  entries.set(manifest.id, { id: manifest.id, name: manifest.name, digest, css, font: (value as ThemeV1).typography.family, enabled });
  for (const listener of listeners) listener();
}
// Bundled identity is compile-time provenance, not a bypass of validation.
for (const [manifest, font] of [[md3Manifest, "host:roboto"], [fluentManifest, "system:segoe_ui"]] as const) {
  // The Vite module compiles/validates their data with compileTheme and puts the
  // content-ID-scoped sheet in the initial CSS. These are not arbitrary CSS packs.
  entries.set(manifest.id, { id: manifest.id, name: manifest.name, digest: "bundled-v1", css: null, font, enabled: true });
}
export function themeEntries(): ThemeEntry[] { return [...entries.values()]; }
export function subscribeThemes(listener: () => void): () => void {
  listeners.add(listener); return () => listeners.delete(listener);
}
export function setThemeEnabled(id: ThemeRef, enabled: boolean): void {
  const entry = entries.get(id);
  if (entry) entry.enabled = enabled;
  applyThemeRef(loadThemeRef());
  for (const listener of listeners) listener();
}
export function applyThemeRef(preferred: ThemeRef): ThemeRef {
  const root = document.documentElement;
  const entry = entries.get(preferred);
  // Recover without changing the preference. No storage string becomes CSS.
  if (preferred === INDUSTRIAL || !entry?.enabled || root.dataset.safeAppearance === "true") {
    document.getElementById("zroutery-plugin-theme")?.remove();
    delete root.dataset.pluginTheme;
    delete root.dataset.skin;
    void import("@fontsource-variable/noto-serif-sc");
    return INDUSTRIAL;
  }
  if (entry.css !== null) {
    let style = document.getElementById("zroutery-plugin-theme") as HTMLStyleElement | null;
    if (!style) { style = document.createElement("style"); style.id = "zroutery-plugin-theme"; document.head.append(style); }
    style.textContent = entry.css;
  } else { document.getElementById("zroutery-plugin-theme")?.remove(); }
  root.dataset.pluginTheme = entry.id;
  delete root.dataset.skin;
  if (entry.font === "host:roboto") void import("@fontsource-variable/roboto");
  return entry.id;
}
export function safeAppearance(): void {
  document.documentElement.dataset.safeAppearance = "true";
  applyThemeRef(INDUSTRIAL);
}
