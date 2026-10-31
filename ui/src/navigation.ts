/** Task doors retain the old stable route names as addressable sub-areas. */
export const TASK_PAGES = ["overview", "providers", "models", "takeover", "activity", "settings"] as const;
export type TaskPage = typeof TASK_PAGES[number];
export type Page = TaskPage | "accounts" | "routing" | "cutover";

export function taskFor(page: Page): TaskPage {
  if (page === "accounts") return "providers";
  if (page === "routing") return "models";
  if (page === "cutover") return "takeover";
  return page;
}

export function routeForDeepLink(value: string): Page | null {
  try {
    const url = new URL(value);
    if (url.protocol !== "zroutery:") return null;
    const host = url.hostname.toLowerCase();
    const area = url.pathname.split("/").filter(Boolean)[0];
    if (host === "providers" && area === "accounts") return "accounts";
    if (host === "models" && area === "routing") return "routing";
    if (host === "takeover" && area === "cutover") return "cutover";
    if ([...TASK_PAGES, "accounts", "routing", "cutover"].includes(host)) return host as Page;
  } catch { /* Not this application's valid address. */ }
  return null;
}
