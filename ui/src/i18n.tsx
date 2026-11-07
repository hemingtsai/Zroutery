/**
 * Localisation: a typed dictionary, nothing more.
 *
 * Every user-visible string lives here under a structured key; the English
 * dictionary defines the key set and TypeScript checks the Chinese one
 * against it, so the two cannot drift apart silently. Plurals use the
 * "singular | plural" pipe form; interpolation uses {name} placeholders.
 *
 * The language follows the system by default and is remembered in local
 * storage. Model ids, provider names, URLs and numbers are data, never
 * translated.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";

export type Lang = "en" | "zh" | "zh-TW";

/** The two shipped dictionaries. English is also the TKey source and the
 * runtime fallback; both live in the entry chunk exactly as when they sat
 * inline, and the zh-TW dictionary stays a separate lazy chunk. */
import { en } from "./i18n/en";
import { zh } from "./i18n/zh";
import type { Dict, TKey } from "./i18n/en";

export type { TKey, Dict };

const english = en as Dict;

/**
 * The loaded dictionaries.
 *
 * English and Simplified Chinese ship in the entry chunk because every reader
 * has one of them as the fallback. Traditional Chinese is a second full copy
 * of the same 980 strings, so it loads on demand: a reader who never selects
 * it should not pay for it on every start.
 */
export const dicts: Partial<Record<Lang, Dict>> = { en: english, zh };

/** The dictionary for a language, falling back to English before a chunk lands. */
export function dictFor(lang: Lang): Dict {
  return dicts[lang] ?? english;
}

async function loadDict(lang: Lang): Promise<void> {
  if (dicts[lang]) return;
  if (lang === "zh-TW") {
    const { zhTW } = await import("./i18n.zhTW");
    dicts["zh-TW"] = zhTW;
  }
}

/**
 * Resolve a dictionary before the first paint.
 *
 * Without this a Traditional Chinese reader would see one frame of English
 * while the chunk arrives. The catch leaves the fallback in place: a language
 * that cannot load is a reason to keep working, not a reason to stay blank.
 */
export async function prepareLang(lang: Lang): Promise<void> {
  try {
    await loadDict(lang);
  } catch {
    // The entry dictionaries are the fallback.
  }
}

function interpolate(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name) =>
    params[name] !== undefined ? String(params[name]) : match,
  );
}

export function detectLang(): Lang {
  const stored = localStorage.getItem("zroutery-lang");
  if (stored === "en" || stored === "zh" || stored === "zh-TW") return stored;
  const system = navigator.language.toLowerCase();
  if (!system.startsWith("zh")) return "en";
  // Taiwan, Hong Kong and Macau write Traditional Chinese, and a `Hant` script
  // subtag says so outright whatever the region is.
  return /hant|-(tw|hk|mo)\b/.test(system) ? "zh-TW" : "zh";
}

/**
 * Re-state the backend errors an operator can act on, in their language.
 *
 * Backend messages are English technical strings and most of them are fine
 * that way — they name a setting or an action and paraphrasing would throw
 * that away. The handful that arrive as raw OS text (a taken port reads as a
 * localised socket paragraph) are matched here; everything else passes
 * through unchanged. Takes `t` rather than calling the hook so non-component
 * error paths can use it too.
 */
export function describeBackendError(
  t: (key: TKey, params?: Record<string, string | number>) => string,
  raw: string,
): string {
  const bind = raw.match(/cannot bind (\S+)/);
  if (bind) {
    if (/already in use|os error 10048/i.test(raw)) {
      return t("err.bind_in_use", { addr: bind[1] });
    }
    if (/os error 10013|access denied|permission denied/i.test(raw)) {
      return t("err.bind_denied", { addr: bind[1] });
    }
    // Any other reason the socket could not be taken is still the socket, and
    // still the one thing the operator can change.
    const detail = raw.match(/cannot bind \S+: (.+)$/);
    return t("err.bind_other", { addr: bind[1], err: detail ? detail[1] : "" });
  }
  // The panel refused the credential. The two shapes it takes are the same
  // advice with different causes: a stored relay key (refused at construction,
  // by name) and a rejected token (the 401 the probe surfaces). Both are fixed
  // by storing the panel's access token, so one wording serves both.
  if (/relay API key/i.test(raw)) {
    return t("err.relay_key");
  }
  if (/missing or invalid credentials/i.test(raw)) {
    return t("err.credentials_rejected");
  }
  if (/no panel access token is stored/i.test(raw)) {
    return t("err.no_panel_token");
  }
  // The browser token capture: four failures an operator can act on.
  if (/a token capture is already open/i.test(raw)) {
    return t("err.capture_open");
  }
  if (/no Chromium browser was found/i.test(raw)) {
    return t("err.no_browser");
  }
  if (/the panel rejected the captured token/i.test(raw)) {
    return t("err.capture_rejected");
  }
  if (/the browser was closed before a token was captured/i.test(raw)) {
    return t("err.capture_closed");
  }
  // The panel-backed balance path. Two conditions an operator can act on, and
  // both would otherwise arrive as raw English from the desktop layer.
  //
  // The first is the usual cause: the account has no panel token, so the
  // panel refused the account's own key — or there is no account at all. The
  // provider's name is carried because it is the thing to act on.
  const noBalance = raw.match(/no account of (.+?) reported a balance/);
  if (noBalance) {
    return t("err.no_account_balance", { provider: noBalance[1] });
  }
  // The second is a declaration that cannot be summed. The currencies are
  // named by the backend, and the message is the same either way: a figure
  // that cannot be spent is not a balance.
  if (/different currencies/i.test(raw)) {
    return t("err.mixed_currencies");
  }
  // A config issue that crosses the bridge in English; the model name, when the
  // message carries one, is the part worth keeping verbatim.
  const candidate = raw.match(/`([^`]+)` is not a configured model/);
  if (candidate) {
    return t("err.classifier_candidate", { model: candidate[1] });
  }

  // The reassurance class: a failure that says what is still in place and what
  // to do about it. These are the messages an operator meets when something
  // has gone wrong, and every one of them carries a clause that is advice
  // rather than a technical string — so every one of them is re-stated here.
  //
  // The technical half of each message (the path, the address, the operating
  // system's own words) is carried through verbatim, because it is the part
  // that identifies the thing to act on.
  const preserved = raw.match(/cannot preserve (.+?) as (.+?): ([^;]+); it was left as it is/);
  if (preserved) {
    return t("err.preserve_failed", {
      path: preserved[1],
      backup: preserved[2],
      err: preserved[3],
    });
  }
  const backupExists = raw.match(/cannot preserve (.+?): (.+?) already exists/);
  if (backupExists) {
    return t("err.backup_exists", { path: backupExists[1], backup: backupExists[2] });
  }
  const copyUnreadable = raw.match(/the copy of (.+?) in (.+?) cannot be read back \(([^)]+)\)/);
  if (copyUnreadable) {
    return t("err.copy_unreadable", {
      path: copyUnreadable[1],
      backup: copyUnreadable[2],
      err: copyUnreadable[3],
    });
  }
  const copyMismatch = raw.match(/the copy of (.+?) in (.+?) does not match it/);
  if (copyMismatch) {
    return t("err.copy_mismatch", { path: copyMismatch[1], backup: copyMismatch[2] });
  }
  const changeRefused = raw.match(
    /the change to (.+?) was refused and the previous configuration and gateway are still in place \(([^)]+)\)/,
  );
  if (changeRefused) {
    return t("err.change_refused", { addr: changeRefused[1], err: changeRefused[2] });
  }
  const stillListening = raw.match(
    /cannot use the new address: ([^;]+); the gateway is still listening on (.+?) and the configuration was not changed/,
  );
  if (stillListening) {
    return t("err.address_not_changed_running", {
      err: stillListening[1],
      addr: stillListening[2],
    });
  }
  const addressNotChanged = raw.match(
    /cannot use the new address: ([^;]+); the configuration was not changed/,
  );
  if (addressNotChanged) {
    return t("err.address_not_changed", { err: addressNotChanged[1] });
  }
  const listenFailed = raw.match(/cannot listen on (.+?): ([^;]+); the previous gateway was restored/);
  if (listenFailed) {
    return t("err.listen_failed_restored", { addr: listenFailed[1], err: listenFailed[2] });
  }
  const listenNotRestored = raw.match(
    /cannot listen on (.+?): ([^;]+); the previous gateway could not be restored either: (.+)$/,
  );
  if (listenNotRestored) {
    return t("err.listen_failed_not_restored", {
      addr: listenNotRestored[1],
      err: listenNotRestored[2],
      restore: listenNotRestored[3],
    });
  }
  const noSnapshot = raw.match(/no rollback snapshot could be taken of (.+?) —/);
  if (noSnapshot) {
    return t("err.no_rollback_snapshot", { path: noSnapshot[1] });
  }
  const noAccount = raw.match(/(?:no|unknown) account `?(.+?)`? on provider `?(.+?)`?$/);
  if (noAccount) {
    return t("err.no_such_account", { account: noAccount[1], provider: noAccount[2] });
  }
  return raw;
}

type I18n = {
  lang: Lang;
  setLang: (lang: Lang) => void;
  t: (key: TKey, params?: Record<string, string | number>) => string;
  /** For "singular | plural" entries: picks a side by the count. */
  plural: (n: number, key: TKey, params?: Record<string, string | number>) => string;
};

const I18nContext = createContext<I18n | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, selectLang] = useState<Lang>(detectLang);

  /*
   * A language whose dictionary lives in its own chunk is selected only after
   * that chunk has arrived, so the interface never renders a half-switched
   * mix of two languages. The await is invisible: `Segment` calls this on a
   * click, and the change lands a few milliseconds later.
   */
  const setLang = useCallback((next: Lang) => {
    void loadDict(next).then(
      () => selectLang(next),
      () => selectLang(next),
    );
  }, []);

  useEffect(() => {
    localStorage.setItem("zroutery-lang", lang);
    document.documentElement.lang = lang;
  }, [lang]);

  const t = useCallback(
    (key: TKey, params?: Record<string, string | number>) =>
      interpolate(dictFor(lang)[key] ?? english[key], params),
    [lang],
  );

  const plural = useCallback(
    (n: number, key: TKey, params?: Record<string, string | number>) => {
      const value = dictFor(lang)[key] ?? english[key];
      const [one, many] = value.split("|").map((side) => side.trim());
      return interpolate((n === 1 ? one : many ?? one) ?? value, { n, ...params });
    },
    [lang],
  );

  return (
    <I18nContext.Provider value={{ lang, setLang, t, plural }}>{children}</I18nContext.Provider>
  );
}

export function useI18n(): I18n {
  const context = useContext(I18nContext);
  if (!context) throw new Error("useI18n needs an I18nProvider");
  return context;
}
