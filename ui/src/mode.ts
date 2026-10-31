import { createContext, useContext } from "react";

/**
 * The two presentations of the same application.
 *
 * Simple mode filters what the interface *shows*; geek mode shows everything.
 * The split is a reading preference, not a configuration: nothing behind the
 * simple mode's curtain stops working, and nothing about the mode is written
 * to the config file. Switching is one click, and it is remembered the way the
 * theme is remembered — in local storage, under this application's namespace.
 */
export type UiMode = "simple" | "geek";

/** The same storage mechanism, and the same namespace, as the theme. */
const STORAGE_KEY = "zroutery-mode";

/**
 * The mode to start in.
 *
 * A first launch has no stored preference, and a first user is served by the
 * short list: simple is the default. An unreadable or absent value reads as
 * simple for the same reason — the only stored string that means geek is
 * "geek".
 */
export function loadMode(): UiMode {
  return localStorage.getItem(STORAGE_KEY) === "geek" ? "geek" : "simple";
}

/** Remember the mode across sessions, the way the theme preference is. */
export function storeMode(mode: UiMode): void {
  localStorage.setItem(STORAGE_KEY, mode);
}

/**
 * The mode the shell is presenting.
 *
 * The value lives with the shell (it needs the snapshot to warn about advanced
 * configuration when the mode narrows), and everything that only filters its
 * rendering reads it here — the same shape as SaveStatusContext: a plain value
 * in context, and a hook that re-renders its readers when the value changes,
 * which is the point.
 */
export const ModeContext = createContext<UiMode>("simple");

/** Read the interface mode. */
export function useMode(): UiMode {
  return useContext(ModeContext);
}
