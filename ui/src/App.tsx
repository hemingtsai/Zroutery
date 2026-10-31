import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, deepLinkCurrent, errorText, gatewayAction, onDeepLink, onGatewayStateChanged, type AppConfig, type Snapshot } from "./api";
import { Banner, MenuItem, Popover, Segment, StatusDot, Tabs, ToastProvider, useToast } from "./components";
import { describeBackendError, I18nProvider, useI18n } from "./i18n";
import { ModeContext, loadMode, storeMode, type UiMode } from "./mode";
import { createSaveQueue, SaveStatusContext, type SaveQueue, type SaveStatus } from "./saveQueue";
import { applySkin, loadSkin, SkinContext, storeSkin, type Skin } from "./skin";
import { routeForDeepLink, taskFor, type Page } from "./navigation";
const Overview = lazy(() => import("./pages/Overview"));
const Models = lazy(() => import("./pages/Models"));
const Providers = lazy(() => import("./pages/Providers"));
const Accounts = lazy(() => import("./pages/Accounts"));
const Takeover = lazy(() => import("./pages/Takeover"));
const Cutover = lazy(() => import("./pages/Cutover"));
const Routing = lazy(() => import("./pages/Routing"));
const Activity = lazy(() => import("./pages/Activity"));
const Settings = lazy(() => import("./pages/Settings"));

/**
 * The doors into Zroutery. No icons, no section headers — a desktop utility's
 * navigation is a plain list of words, and the active one is simply the one
 * that reads darkest. Everything else lives inside a page: Auto review inside
 * Routing, the gateway inside Settings, diagnostics inside Settings.
 */

/**
 * The page a `zroutery://` link names, or `null` when it names none.
 *
 * The link's host is the page name: `zroutery://providers` opens the
 * providers page. A link that names a page this build does not have opens
 * none, and says so — a link that opened the wrong page would be worse than
 * one that opened nothing, and the shell has already brought the window
 * forward by the time this runs.
 */
function pageForLink(url: string): Page | null {
  return routeForDeepLink(url);
}

const NAV: { id: Page; labelKey: Parameters<ReturnType<typeof useI18n>["t"]>[0] }[] = [
  { id: "overview", labelKey: "nav.overview" },
  { id: "providers", labelKey: "task.providers" },
  { id: "models", labelKey: "task.models" },
  { id: "takeover", labelKey: "task.clients" },
  { id: "activity", labelKey: "task.usage" },
  { id: "settings", labelKey: "nav.settings" },
];

/**
 * The nav entries simple mode keeps: the dashboard, the usage ledger, and the
 * settings. Everything else keeps working — a deep link or a bookmark still
 * opens its page — the short list just stops advertising doors a first user
 * has no reason to look for.
 */
const SIMPLE_NAV_IDS: readonly Page[] = ["overview", "activity", "settings"];

/**
 * The pages a `zroutery://` link may not open while simple mode is on.
 *
 * Models, client setup and their sub-areas are the geek-only surfaces. A link
 * naming one of them is answered with a notice rather than a navigation: the
 * window is already forward, so saying where the page lives is the honest
 * answer, and "stay" is the only response that does not throw away the page
 * the reader was on.
 */
const GEEK_ONLY_PAGES: readonly Page[] = ["models", "takeover", "routing", "cutover", "accounts"];

/**
 * The quirks the provider drawer offers as toggles.
 *
 * `stream_usage` ships enabled, is not a drawer control, and counting it would
 * report every provider as an advanced configuration; it is left out.
 */
const QUIRK_KEYS = [
  "use_max_completion_tokens",
  "drop_temperature",
  "drop_top_p",
  "drop_stop",
  "system_as_developer",
  "send_reasoning_effort",
  "portable_history",
] as const;

/** Session flag for the one-time "advanced configuration is running" notice. */
const ADVANCED_TOAST_FLAG = "zroutery-mode-advanced-toast";

/**
 * Count the advanced configuration items that are in effect right now.
 *
 * Switching to simple mode hides these rather than turning them off, and the
 * one-time notice says how many are running so hiding is never mistaken for
 * disabling: learned routing is one item, each budget is one, the Auto Mode
 * classifier (when it has candidates to route between) is one, and every
 * enabled provider quirk is one.
 */
function countAdvanced(config: AppConfig): number {
  let n = 0;
  if (config.ml_routing?.enabled) n += 1;
  n += config.budgets.length;
  if (config.classifier.enabled && config.classifier.candidates.length > 0) n += 1;
  for (const provider of config.providers) {
    for (const key of QUIRK_KEYS) {
      if (provider.quirks[key]) n += 1;
    }
  }
  return n;
}

type ThemePref = "system" | "light" | "dark";

/**
 * How many notification banners the shell shows before collapsing the rest.
 *
 * Two is the width of the window's usable height at 820x560: a third banner
 * pushes the first section below the fold, and a notification that costs the
 * page its content is a notification that gets dismissed without being read.
 */
const MAX_BANNERS = 2;

function loadThemePref(): ThemePref {
  const stored = localStorage.getItem("zroutery-theme");
  return stored === "light" || stored === "dark" ? stored : "system";
}

/** The concrete theme a preference resolves to right now. */
function resolveTheme(pref: ThemePref): "light" | "dark" {
  if (pref !== "system") return pref;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export default function App() {
  return (
    <I18nProvider>
      <ToastProvider>
        <Shell />
      </ToastProvider>
    </I18nProvider>
  );
}

function Shell() {
  const { t, plural } = useI18n();
  const notify = useToast();
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [page, setPage] = useState<Page>("overview");
  const taskPage = taskFor(page);
  const [themePref, setThemePref] = useState<ThemePref>(loadThemePref);
  // The mode is a presentation preference like the theme: read once from local
  // storage, written back on every change, never part of the configuration.
  const [mode, setMode] = useState<UiMode>(loadMode);
  // So is the skin, and by the same mechanism — one attribute on the document,
  // applied here so a stored md3/fluent choice lands before the first paint of
  // anything that reads the tokens.
  const [skin, setSkin] = useState<Skin>(loadSkin);
  useEffect(() => {
    applySkin(skin);
    storeSkin(skin);
  }, [skin]);
  // The deep-link listener registers once, so it reads the mode it was current
  // when it fired through a ref rather than re-registering on every switch.
  const modeRef = useRef(mode);
  const [error, setError] = useState<string | null>(null);
  const [pollError, setPollError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The save truth at drawer distance. begin/finish/fail below keep it in step
  // with the queue; Drawer reads it through context and speaks it at its foot.
  const [saveStatus, setSaveStatus] = useState<SaveStatus>({ phase: "idle" });
  const [showAllBanners, setShowAllBanners] = useState(false);
  const configEpoch = useRef(0);
  const saving = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus({ preventScroll: true }); }, [page]);

  useEffect(() => {
    const apply = () => {
      document.documentElement.dataset.theme = resolveTheme(themePref);
    };
    apply();
    localStorage.setItem("zroutery-theme", themePref);
    // "Follow system" has to keep following: listen while it is the preference.
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [themePref]);

  useEffect(() => {
    modeRef.current = mode;
  }, [mode]);

  /**
   * Switch the interface mode — the one handler behind both entrances.
   *
   * Persisting comes first so even an interrupted render has recorded the
   * choice. Narrowing to simple may hide configuration that is in effect, and
   * that is worth saying exactly once per session: the notice counts what is
   * running behind the curtain so a hidden setting is never read as an off
   * one. A session that has already seen the notice (or an installation with
   * nothing advanced in play) switches silently.
   */
  const applyMode = (next: UiMode) => {
    if (next === mode) return;
    storeMode(next);
    setMode(next);
    if (next !== "simple" || !snapshot) return;
    const advanced = countAdvanced(snapshot.config);
    if (advanced === 0 || sessionStorage.getItem(ADVANCED_TOAST_FLAG)) return;
    sessionStorage.setItem(ADVANCED_TOAST_FLAG, "1");
    notify("ok", plural(advanced, "mode.advanced_active_toast"));
  };

  // The newest config known to have been committed (or loaded). Saves are
  // updaters against this, so an edit queued behind an in-flight save is
  // applied on top of it instead of overwriting it with a stale snapshot.
  const configRef = useRef<AppConfig | null>(null);

  // The queue outlives renders, but the toast it fires on failure has to speak
  // the language the window is in now rather than the one it started in. `t`
  // changes identity with the language; the setState functions do not, so only
  // the translation goes through a ref.
  const translate = useRef(t);
  useEffect(() => {
    translate.current = t;
  }, [t]);

  const queueRef = useRef<SaveQueue | null>(null);
  if (!queueRef.current) {
    queueRef.current = createSaveQueue({
      begin: () => {
        saving.current = true;
        configEpoch.current += 1;
        setBusy(true);
        setError(null);
        setSaveStatus({ phase: "saving" });
      },
      apply: (next) => {
        configEpoch.current += 1;
        configRef.current = next.config;
        setSnapshot(next);
      },
      fail: (cause, dropped) => {
        setError(errorText(cause));
        setSaveStatus({ phase: "error", message: errorText(cause) });
        if (dropped > 0) {
          notify("error", translate.current("toast.queued_discarded"));
        }
      },
      finish: () => {
        saving.current = false;
        setBusy(false);
        // A failed save just wrote the error phase; a success (or the tail of a
        // failure chain) stamps the moment. Keeping the error wins here.
        setSaveStatus((s) =>
          s.phase === "error" ? s : { phase: "saved", at: Date.now() },
        );
      },
    });
  }
  const run = queueRef.current.run;

  useEffect(() => {
    void run(api.snapshot);
  }, [run]);

  /**
   * Re-read the whole snapshot without touching the save queue.
   *
   * The server state is not part of the activity counters the live pages poll,
   * so it only moves when something asks for a snapshot: the tray starting or
   * stopping the gateway, or the window being shown again. Refreshes are
   * coalesced, and a failure is reported like a failed save rather than
   * leaving the page quietly out of date.
   */
  const refreshRef = useRef<Promise<void> | null>(null);
  const refresh = useCallback((): Promise<void> => {
    if (refreshRef.current) return refreshRef.current;
    const pending = (async () => {
      try {
        const epoch = configEpoch.current;
        const next = await api.snapshot();
        // A refresh that began before a save/identity publication cannot undo it.
        if (epoch !== configEpoch.current || saving.current) return;
        configRef.current = next.config;
        setSnapshot(next);
        setPollError(null);
      } catch (e) {
        setError(errorText(e));
      } finally {
        refreshRef.current = null;
      }
    })();
    refreshRef.current = pending;
    return pending;
  }, []);

  // The tray can start or stop the gateway while this window sits on another
  // page, so it announces the change and the window re-reads the state instead
  // of trusting the snapshot it happened to load.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onGatewayStateChanged(() => {
      void refresh();
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch(() => {
        // No event bridge (a plain browser): visibility and focus below still
        // refresh the snapshot.
      });
    const onFocus = () => void refresh();
    const onVisibility = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      unlisten?.();
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh]);

  /*
   * A `zroutery://` link opens the page it names.
   *
   * The desktop shell brings the window forward and hands the URL over; this
   * is where the URL becomes a page. The shell cannot do it — it does not
   * know what the pages are called, and it has to keep working when the
   * window does not exist yet — and the window cannot be the one that waits
   * for a link, because on Windows and Linux a link *starts* the application.
   *
   * A link that names a page this build does not have opens none. The shell
   * has already brought the window forward by then, so the operator sees the
   * dashboard rather than a page that does not exist.
   */
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    const open = (url: string) => {
      const target = pageForLink(url);
      if (!target) return;
      // Simple mode filters visibility, not behaviour, and a link is outside
      // the window: refusing it would break a bookmark that works in geek
      // mode, and opening it would land a first user on a surface the short
      // nav never mentioned. So the page stays where it is and the notice says
      // where the surface lives — through the ref, because the listener was
      // registered once and the mode may have changed since.
      if (modeRef.current === "simple" && GEEK_ONLY_PAGES.includes(target)) {
        // info, not ok: the link worked and the app is describing where the
        // surface lives — a notice, not an accomplishment.
        notify("info", translate.current("mode.geek_only_page"));
        return;
      }
      setPage(target);
    };

    void onDeepLink(open)
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch(() => {
        // No event bridge (a plain browser): there are no links to open.
      });

    // And the link that *started* this application, which was announced before
    // there was a listener for it.
    void deepLinkCurrent()
      .then((url) => {
        if (!cancelled && url) open(url);
      })
      .catch(() => {
        // No such command in a build without the desktop shell.
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  /*
   * The nav the current mode presents. Geek mode lists everything; simple
   * mode keeps the three doors a first user needs. The page title is still
   * looked up in the full NAV, so a page a link opened stays named even when
   * the short list does not mention it.
   */
  const visibleNav = useMemo(
    () => (mode === "simple" ? NAV.filter((item) => SIMPLE_NAV_IDS.includes(item.id)) : NAV),
    [mode],
  );

  /*
   * Alt+1..9 jumps straight to the page at that position in the nav.
   *
   * A desktop utility is navigated from the keyboard as much as from the
   * pointer, and the nav is a fixed list, so the position *is* the address.
   * The position that counts is the one the sidebar is showing: the shortcut
   * names the visible entry at that index, the same promise the tooltips
   * make. Matched by `code` rather than `key`, because Alt is a dead key on
   * some layouts — Option+1 on a Mac keyboard types "¡", and a shortcut that
   * only worked on US layouts would be a shortcut that mostly did not work.
   * Plain digits are left alone — they are typing — and so are every other
   * modifier combination, which belongs to the OS and the webview.
   */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
      const digit = /^Digit([1-9])$/.exec(e.code);
      if (!digit) return;
      const target = visibleNav[Number(digit[1]) - 1];
      if (!target) return;
      e.preventDefault();
      setPage(target.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [visibleNav]);

  // Overview and Activity show live state; their pages poll the counters
  // only. A failure is reported rather than swallowed: silence would look
  // like an idle proxy.
  useEffect(() => {
    if (taskPage !== "activity" && taskPage !== "overview") return;
    let cancelled = false;
    let pending = false;
    const poll = async () => {
      if (pending || document.visibilityState === "hidden") return;
      pending = true;
      try {
        const activity = await api.activity();
        if (cancelled) return;
        setPollError(null);
        setSnapshot((current) => (current ? { ...current, ...activity } : current));
      } catch (e) {
        if (!cancelled) setPollError(errorText(e));
      } finally {
        pending = false;
      }
    };
    const timer = setInterval(poll, 2000);
    const onVisibility = () => { if (document.visibilityState === "visible") void poll(); };
    document.addEventListener("visibilitychange", onVisibility);
    void poll();
    return () => {
      cancelled = true;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [taskPage]);

  /**
   * Start or stop the gateway from the state Rust reports right now.
   *
   * Deciding from the rendered `server.running` would run the wrong command
   * whenever the tray changed the state first, so the current status is loaded
   * and the command chosen from that.
   */
  const toggleGateway = useCallback(async () => {
    if (busy) return;
    let fresh: Snapshot;
    try {
      fresh = await api.snapshot();
    } catch (e) {
      setError(errorText(e));
      return;
    }
    configRef.current = fresh.config;
    setSnapshot(fresh);
    void run(gatewayAction(fresh.server) === "start" ? api.start : api.stop);
  }, [busy, run]);

  /**
   * Apply a mutation to the freshest committed config and save it. Pages hand
   * in an updater so an edit that queues behind an in-flight save rebases onto
   * that save's result rather than clobbering it with a stale snapshot. An
   * updater may return null to mean "nothing to change".
   */
  const save = useCallback(
    (mutate: (config: AppConfig) => AppConfig | null) =>
      run(async () => {
        const base = configRef.current;
        if (!base) throw new Error("not ready");
        const next = mutate(structuredClone(base));
        return next ? api.saveConfig(next) : api.snapshot();
      }),
    [run],
  );

  // A decision model is deliberately outside every tier: it answers typed
  // questions and cannot serve a request that wants prose. Counting it would
  // nag about a classification that has no answer.
  const decisionProviders = useMemo(
    () =>
      new Set(
        (snapshot?.config.providers ?? [])
          .filter((p) => p.kind === "decision")
          .map((p) => p.id),
      ),
    [snapshot],
  );
  const unclassified = useMemo(
    () =>
      snapshot?.config.models.filter(
        (m) => m.tier === null && !decisionProviders.has(m.provider_id),
      ) ?? [],
    [snapshot, decisionProviders],
  );

  if (!snapshot) {
    return (
      <main className="loading">
        <p>{error ? describeBackendError(t, error) : t("common.loading")}</p>
      </main>
    );
  }

  const { server } = snapshot;

  /*
   * Every notification the shell can show, worst first.
   *
   * There used to be six independent slots and no ceiling: two poll failures,
   * a config warning, three configuration errors, an exposed listener and a
   * missing token could stack into six banners, which at 560px tall pushes the
   * page's own content off the screen. The ceiling is two visible, with the
   * rest behind one text action, so nothing is dropped and the page still
   * starts below the fold only once.
   *
   * `severity` orders them rather than JSX order: a missing auth token is a
   * warning and an exposed listener is a danger, and at a two-banner ceiling
   * the order decides which one an operator actually reads.
   */
  const banners: {
    key: string;
    tone: "danger" | "warn";
    node: ReactNode;
    severity: number;
    /** Present when the notice is local state and dismissing it means anything. */
    dismiss?: () => void;
  }[] = [];
  if (error) {
    banners.push({
      key: "error",
      tone: "danger",
      severity: 0,
      node: describeBackendError(t, error),
      dismiss: () => setError(null),
    });
  }
  for (const issue of snapshot.issues.filter((i) => i.severity === "error")) {
    banners.push({
      key: `issue:${issue.code}:${issue.subject ?? ""}`,
      tone: "danger",
      severity: 1,
      node: describeBackendError(t, issue.message),
    });
  }
  if (server.exposed) {
    banners.push({
      key: "exposed",
      tone: "danger",
      severity: 2,
      node: t("app.exposed", { host: server.host }),
    });
  }
  if (snapshot.accounting && (!snapshot.accounting.available || snapshot.accounting.error)) {
    banners.push({
      key: "accounting",
      tone: "danger",
      severity: 1,
      node: <>
        {t("accounting.blocked")}{" "}
        <button type="button" className="linky" onClick={() => setPage("activity")}>
          {t("accounting.inspect")}
        </button>
      </>,
    });
  }
  if (pollError) {
    banners.push({
      key: "poll",
      tone: "warn",
      severity: 3,
      node: t("app.live_stopped", { err: describeBackendError(t, pollError) }),
      // The next successful poll clears this server-side condition, but a
      // reader who has seen it can fold it away now instead of waiting.
      dismiss: () => setPollError(null),
    });
  }
  if (snapshot.warning) {
    banners.push({
      key: "warning",
      tone: "warn",
      severity: 4,
      node: describeBackendError(t, snapshot.warning),
    });
  }
  if (!server.require_auth) {
    banners.push({ key: "no-auth", tone: "warn", severity: 5, node: t("app.no_auth") });
  }
  banners.sort((a, b) => a.severity - b.severity);
  const visibleBanners = showAllBanners ? banners : banners.slice(0, MAX_BANNERS);
  const hiddenBanners = banners.length - MAX_BANNERS;

  return (
    <ModeContext.Provider value={mode}>
    <SkinContext.Provider value={skin}>
    <SaveStatusContext.Provider value={saveStatus}>
    <div className="shell">
      <nav className="sidebar" aria-label={t("nav.main_aria")}>
        <div className="sidebar-brand">
          Zroutery<span className="mono">v{snapshot.version}</span>
        </div>

        {visibleNav.map((item, index) => (
          <button type="button"
            key={item.id}
            className={`nav-item ${taskPage === item.id ? "active" : ""}`}
            // Named as well as classed. The two layout harnesses drive the
            // shell with real clicks, and an ordinal is a promise that breaks
            // the moment a page is inserted: adding one silently repoints every
            // later index at a different page, which measures a healthy surface
            // and reports it as a pass. `data-page` survives the insert.
            data-page={item.id}
            // Which page is open is carried by the `active` class alone, and a
            // class is invisible to a screen reader: the nav read as eight
            // identical buttons with no indication of where you were.
            aria-current={taskPage === item.id ? "page" : undefined}
            // The shortcut is the nav's position, and the tooltip is the only
            // place a keyboard user can discover it without reading source.
            aria-keyshortcuts={`Alt+${index + 1}`}
            title={`Alt+${index + 1}`}
            onClick={() => setPage(item.id)}
          >
            {t(item.labelKey)}
            {item.id === "models" && unclassified.length > 0 && (
              <span className="nav-count" title={t("nav.unclassified_title", { n: unclassified.length })}>
                {unclassified.length}
              </span>
            )}
          </button>
        ))}

        <div className="sidebar-spacer" />

        {/*
          * The mode switch lives at the foot of the nav because the mode is a
          * property of the nav itself — it decides how long the list above is.
          * One Segment, the same control the settings row renders: two doors
          * into one state, and a switch that is one click either way.
          */}
        <div className="sidebar-mode">
          <Segment<UiMode>
            ariaLabel={t("mode.toggle_aria")}
            value={mode}
            onChange={applyMode}
            options={[
              { value: "simple", label: t("mode.simple") },
              { value: "geek", label: t("mode.geek") },
            ]}
          />
        </div>
      </nav>

      <div className="main">
        <header className="mainbar">
          {busy && (
            /*
             * A progress bar that announces itself. `aria-hidden` was the old
             * answer: the line is visible, but a screen-reader user had no way
             * to know the application was waiting on a save. `role=progressbar`
             * with an indeterminate value says "working" without claiming a
             * percentage nobody can know.
             */
            <div
              className="busybar"
              role="progressbar"
              aria-busy="true"
              aria-label={t("common.working")}
            />
          )}
          {/*
            * The page's name is a heading, so a screen-reader user can navigate
            * by it. Everything else about it — the size, the weight, the
            * uppercase — is in the stylesheet, and a heading's only other
            * difference is a margin the user agent would add.
            */}
          <h1 ref={heading} tabIndex={-1} className="page-title">{t(NAV.find((n) => n.id === taskPage)!.labelKey)}</h1>
          <div className="mainbar-right">
            {/* The gateway lives here, not in a bottom strip: top of the
                window is where a desktop app keeps global state. */}
            <Popover
              title={t("settings.gateway")}
              trigger={
                <>
                  <StatusDot tone={server.running ? "ok" : "danger"} />
                  {/*
                    * The dot is the state, and the dot is a colour. Without this
                    * the button announced itself as "Gateway" and said nothing
                    * about whether the proxy was running, which is the one
                    * thing a person opening this menu needs to know.
                    */}
                  <span className="visually-hidden">
                    {server.running ? t("status.running") : t("status.stopped")}
                  </span>
                  {t("settings.gateway")}
                </>
              }
            >
              {(close) => (
                <>
                  {/*
                    * A menu may only contain menu items, groups and
                    * separators. The status rows and the rule are content, not
                    * items, and saying so is what keeps the menu's structure
                    * one a screen reader can walk.
                    */}
                  <div className="menu-row" role="none">
                    <StatusDot tone={server.running ? "ok" : "danger"} />
                    {t(server.running ? "status.running" : "status.stopped")}
                  </div>
                  {server.running && (
                    <div className="menu-row" role="none">
                      <span className="menu-key">{t("gw.address")}</span>
                      <span className="mono">
                        {server.host}:{server.port}
                      </span>
                    </div>
                  )}
                  <div className="menu-sep" role="separator" />
                  <MenuItem
                    onClick={() => {
                      void api.copy(`http://${server.host}:${server.port}`).then(() =>
                        notify("ok", t("toast.copied")),
                      );
                      close();
                    }}
                  >
                    {t("gw.copy_address")}
                  </MenuItem>
                  <MenuItem
                    onClick={() => {
                      void api.copyToken().then(() => notify("ok", t("toast.copied")));
                      close();
                    }}
                  >
                    {t("action.copy_token")}
                  </MenuItem>
                  <div className="menu-sep" />
                  <MenuItem
                    onClick={() => {
                      void toggleGateway();
                      close();
                    }}
                  >
                    {t(server.running ? "gw.stop" : "gw.start")}
                  </MenuItem>
                </>
              )}
            </Popover>

            {/* Appearance: a half-moon reads as "Appearance", not "dark
                mode", and the three-way choice includes following the OS. */}
            <Popover
              title={t("appearance.title")}
              ariaLabel={t("appearance.title")}
              trigger={
                <span aria-hidden className="glyph">
                  {"◐"}
                </span>
              }
            >
              {(close) => (
                <>
                  <MenuItem
                    active={themePref === "system"}
                    onClick={() => {
                      setThemePref("system");
                      close();
                    }}
                  >
                    {t("theme.system")}
                  </MenuItem>
                  <MenuItem
                    active={themePref === "light"}
                    onClick={() => {
                      setThemePref("light");
                      close();
                    }}
                  >
                    {t("theme.light")}
                  </MenuItem>
                  <MenuItem
                    active={themePref === "dark"}
                    onClick={() => {
                      setThemePref("dark");
                      close();
                    }}
                  >
                    {t("theme.dark")}
                  </MenuItem>
                </>
              )}
            </Popover>
          </div>
        </header>

        <main className="content">
          <div className="page">
            {visibleBanners.map((banner) => (
              <Banner
                key={banner.key}
                tone={banner.tone}
                actions={
                  banner.dismiss ? (
                    <button type="button" className="linky" onClick={banner.dismiss}>
                      {t("common.dismiss")}
                    </button>
                  ) : undefined
                }
              >
                {banner.node}
              </Banner>
            ))}
            {hiddenBanners > 0 && !showAllBanners && (
              <p className="banner-overflow">
                <button
                  type="button"
                  className="linky"
                  onClick={() => setShowAllBanners(true)}
                >
                  {t("app.more_notifications", { n: hiddenBanners })}
                </button>
              </p>
            )}
            {showAllBanners && banners.length > MAX_BANNERS && (
              <p className="banner-overflow">
                <button
                  type="button"
                  className="linky"
                  onClick={() => setShowAllBanners(false)}
                >
                  {t("app.fewer_notifications")}
                </button>
              </p>
            )}

            <Suspense fallback={<p className="empty" role="status">{t("common.loading")}</p>}>
            {page === "overview" && (
              <Overview
                snapshot={snapshot}
                onNavigate={setPage}
                // The pool drags on the simple home save through the same
                // queue as every page, and the add-models hand-off switches
                // the mode the same way the settings row does.
                save={save}
                run={run}
                busy={busy}
                onModeChange={applyMode}
              />
            )}
            {taskPage === "models" && <Tabs value={page} onChange={(next) => setPage(next as Page)} ariaLabel={t("task.models")}
              tabs={[
                { value: "models", label: t("task.catalogue"), content: <Models snapshot={snapshot} save={save} busy={busy} /> },
                { value: "routing", label: t("task.rules"), content: <Routing snapshot={snapshot} save={save} run={run} busy={busy} /> },
              ]} />}
            {taskPage === "providers" && <Tabs
              // Simple mode keeps only the connections tab. A reader standing
              // on the observations tab when the mode narrows is clamped to the
              // tab that remains rather than shown a tab the strip does not
              // list; switching back restores where they were.
              value={mode === "simple" && page === "accounts" ? "providers" : page}
              onChange={(next) => setPage(next as Page)}
              ariaLabel={t("task.providers")}
              tabs={[
                { value: "providers", label: t("task.connections"), content: <Providers snapshot={snapshot} save={save} run={run} busy={busy} /> },
                ...(mode === "geek"
                  ? [{ value: "accounts", label: t("task.accounts"), content: <Accounts snapshot={snapshot} run={run} busy={busy} /> }]
                  : []),
              ]} />}
            {taskPage === "takeover" && <Tabs value={page} onChange={(next) => setPage(next as Page)} ariaLabel={t("task.clients")}
              tabs={[
                { value: "takeover", label: t("task.connect"), content: <Takeover /> },
                { value: "cutover", label: t("task.migrate"), content: <Cutover /> },
              ]} />}
            {page === "activity" && <Activity snapshot={snapshot} run={run} save={save} busy={busy} />}
            {page === "settings" && (
              <Settings
                snapshot={snapshot}
                save={save}
                run={run}
                busy={busy}
                themePref={themePref}
                onThemePref={setThemePref}
                skin={skin}
                onSkinChange={setSkin}
                mode={mode}
                onModeChange={applyMode}
              />
            )}
            </Suspense>
          </div>
        </main>
      </div>
    </div>
    </SaveStatusContext.Provider>
    </SkinContext.Provider>
    </ModeContext.Provider>
  );
}
