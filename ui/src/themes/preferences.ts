export type ThemeRef = string;
export const THEME_REF_KEY = "zroutery-theme-ref";
export const INDUSTRIAL = "industrial";
const LEGACY: Record<string, ThemeRef> = {
  md3: "org.zroutery.theme.md3",
  fluent: "org.zroutery.theme.fluent",
};
export function loadThemeRef(storage: Pick<Storage, "getItem" | "setItem"> = localStorage): ThemeRef {
  try {
    const current = storage.getItem(THEME_REF_KEY);
    if (current) return /^[a-zA-Z0-9_.-]{1,128}$/.test(current) ? current : INDUSTRIAL;
    const legacy = storage.getItem("zroutery-skin");
    const ref = legacy ? LEGACY[legacy] ?? INDUSTRIAL : INDUSTRIAL;
    storage.setItem(THEME_REF_KEY, ref);
    return ref;
  } catch { return INDUSTRIAL; }
}
export function storeThemeRef(ref: ThemeRef): void {
  if (!/^[a-zA-Z0-9_.-]{1,128}$/.test(ref)) throw new Error("invalid_theme_reference");
  localStorage.setItem(THEME_REF_KEY, ref);
}
