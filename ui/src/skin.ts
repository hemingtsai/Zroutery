import { createContext, useContext } from "react";
import { applyThemeRef } from "./themes/registry";
import { loadThemeRef, storeThemeRef, type ThemeRef } from "./themes/preferences";

/**
 * Compatibility entry points for the shell's appearance axis. A skin now names
 * a validated data-plugin theme; the Industrial stylesheet remains the default
 * and recovery path. Light/dark/system stays independently owned by theme.ts.
 */
export type Skin = ThemeRef;

/**
 * The skin to start in.
 *
 * Legacy md3/fluent values migrate once; the preferred reference survives a
 * missing/disabled/corrupt pack while the active renderer falls back safely.
 */
export function loadSkin(): Skin {
  return loadThemeRef();
}

/** Remember the skin across sessions, the way the theme preference is. */
export function storeSkin(skin: Skin): void {
  storeThemeRef(skin);
}

/**
 * Put the skin on the document.
 *
 * A reference selects compiled host recipes. No stored string becomes CSS,
 * and there are no MD3/Fluent identity branches in the runtime renderer.
 */
export function applySkin(skin: Skin): void {
  applyThemeRef(skin);
}

/**
 * The skin the shell is presenting.
 *
 * The value lives with the shell (it applies the attribute and hands the choice
 * to Settings), and the same shape as ModeContext: a plain value in context,
 * and a hook that re-renders its readers when the value changes.
 */
export const SkinContext = createContext<Skin>("industrial");

/** Read the interface skin. */
export function useSkin(): Skin {
  return useContext(SkinContext);
}
