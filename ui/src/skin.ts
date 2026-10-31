import { createContext, useContext } from "react";

/**
 * The three skins of the same information structure.
 *
 * Industrial is the house design — neutral warm greys, a black primary, hairlines
 * — and it is the default, which is another way of saying the stylesheet's own
 * `:root` values are industrial and no rule anywhere carries its name. MD3 and
 * Fluent are the same UI re-inked through the layer-2 token blocks in
 * styles.css: colour and corner radius only. Everything a component consumes is
 * a layer-1 semantic token, so a skin never touches a rule outside `styles.css`'s
 * token blocks and its handful of layer-3 overrides.
 *
 * The skin is a reading preference like the theme and the mode: persisted in
 * local storage under this application's namespace, never part of the
 * configuration file.
 */
export type Skin = "industrial" | "md3" | "fluent";

/** The same storage mechanism, and the same namespace, as the theme and mode. */
const STORAGE_KEY = "zroutery-skin";

/**
 * The skin to start in.
 *
 * A first launch has no stored preference and is served industrial, exactly as
 * if the attribute were absent — the only stored strings that mean something
 * else are "md3" and "fluent", so an unreadable value reads as no change at all.
 */
export function loadSkin(): Skin {
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "md3" || stored === "fluent" ? stored : "industrial";
}

/** Remember the skin across sessions, the way the theme preference is. */
export function storeSkin(skin: Skin): void {
  localStorage.setItem(STORAGE_KEY, skin);
}

/**
 * Put the skin on the document.
 *
 * The whole mechanism is one attribute: layer-2 token blocks key off
 * `[data-skin]`, and industrial — being the `:root` values themselves — removes
 * the attribute rather than setting it, so the no-skin path changes not a pixel
 * of the existing presentation.
 */
export function applySkin(skin: Skin): void {
  if (skin === "industrial") {
    delete document.documentElement.dataset.skin;
  } else {
    document.documentElement.dataset.skin = skin;
  }
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
