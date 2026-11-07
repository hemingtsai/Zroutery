export type ThemePref = "system" | "light" | "dark";

const STORAGE_KEY = "zroutery-theme";

export function loadThemePref(): ThemePref {
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" ? stored : "system";
}

export function storeThemePref(pref: ThemePref): void {
  localStorage.setItem(STORAGE_KEY, pref);
}

export function resolveTheme(pref: ThemePref): "light" | "dark" {
  if (pref !== "system") return pref;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function applyTheme(pref: ThemePref): void {
  document.documentElement.dataset.theme = resolveTheme(pref);
}
