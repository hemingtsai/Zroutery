/**
 * The doors into Zroutery. No icons, no section headers — a desktop utility's
 * navigation is a plain list of words, and the active one is simply the one
 * that reads darkest. Everything else lives inside a page: Auto review inside
 * Routing, the gateway inside Settings, diagnostics inside Settings.
 */
import { routeForDeepLink, type Page } from "../navigation";
import { useI18n } from "../i18n";

/**
 * The page a `zroutery://` link names, or `null` when it names none.
 *
 * The link's host is the page name: `zroutery://providers` opens the
 * providers page. A link that names a page this build does not have opens
 * none, and says so — a link that opened the wrong page would be worse than
 * one that opened nothing, and the shell has already brought the window
 * forward by the time this runs.
 */
export function pageForLink(url: string): Page | null {
  return routeForDeepLink(url);
}

export type NavItem = {
  id: Page;
  labelKey: Parameters<ReturnType<typeof useI18n>["t"]>[0];
};

export const NAV: NavItem[] = [
  { id: "overview", labelKey: "nav.overview" },
  { id: "providers", labelKey: "task.providers" },
  { id: "models", labelKey: "task.models" },
  { id: "takeover", labelKey: "task.clients" },
  { id: "activity", labelKey: "task.usage" },
  { id: "settings", labelKey: "nav.settings" },
];

// Both modes keep the six task doors. Basic setup cannot depend on discovering
// an interface-mode switch; advanced controls remain inside their task pages.
