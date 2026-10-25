import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, deepLinkCurrent, errorText, gatewayAction, onDeepLink, onGatewayStateChanged, pageForDeepLink, type AppConfig, type Snapshot } from "./api";
import { Banner, MenuItem, Popover, StatusDot, ToastProvider, useToast } from "./components";
import { describeBackendError, I18nProvider, useI18n } from "./i18n";
import { createSaveQueue, type SaveQueue } from "./saveQueue";
import Overview from "./pages/Overview";
import Models from "./pages/Models";
import Providers from "./pages/Providers";
import Accounts from "./pages/Accounts";
import Takeover from "./pages/Takeover";
import Cutover from "./pages/Cutover";
import Routing from "./pages/Routing";
import Activity from "./pages/Activity";
import Settings from "./pages/Settings";

/**
 * The doors into Zroutery. No icons, no section headers — a desktop utility's
 * navigation is a plain list of words, and the active one is simply the one
 * that reads darkest. Everything else lives inside a page: Auto review inside
 * Routing, the gateway inside Settings, diagnostics inside Settings.
 */
type Page =
  | "overview"
  | "models"
  | "providers"
  | "accounts"
  | "takeover"
  | "cutover"
  | "routing"
  | "activity"
  | "settings";

/** Every page, in the order the navigation lists them. */
const PAGES: Page[] = [
  "overview",
  "providers",
  "models",
  "routing",
  "accounts",
  "activity",
  "takeover",
  "cutover",
  "settings",
];

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
  const named = pageForDeepLink(url);
  if (!named) return null;
  return PAGES.includes(named as Page) ? (named as Page) : null;
}

const NAV: { id: Page; labelKey: Parameters<ReturnType<typeof useI18n>["t"]>[0] }[] = [
  { id: "overview", labelKey: "nav.overview" },
  { id: "providers", labelKey: "nav.providers" },
  { id: "models", labelKey: "nav.models" },
  { id: "routing", labelKey: "nav.routing" },
  { id: "accounts", labelKey: "nav.accounts" },
  { id: "activity", labelKey: "nav.activity" },
  { id: "takeover", labelKey: "nav.takeover" },
  { id: "cutover", labelKey: "nav.cutover" },
  { id: "settings", labelKey: "nav.settings" },
];

type ThemePref = "system" | "light" | "dark";

/**
 * How many notification banners the shell shows before collapsing the rest.
 *
 * Two is the width of the window's usable height at 820x560: a third banner
 * pushes the first section below the fold, and a notification that costs the
 * page its content is a notification that gets dismissed without being read.
 */
const MAX_BANNERS = 2;

/**
 * The label the busy line announces.
 *
 * Plain text, because the dictionary is owned by a change in flight and a key
 * added here would be lost in the merge. The key to add is
 * `common.working`; see the report.
 */
const WORKING_LABEL = "Working…";

/**
 * The text that reveals the banners the ceiling is hiding.
 *
 * Same reason as `WORKING_LABEL`: these need `app.more_notifications` and
 * `app.fewer_notifications` in the dictionary.
 */
function moreNotificationsLabel(count: number): string {
  return `Show ${count} more notification${count === 1 ? "" : "s"}`;
}

const FEWER_NOTIFICATIONS_LABEL = "Show fewer notifications";

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
  const { t } = useI18n();
  const notify = useToast();
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [page, setPage] = useState<Page>("overview");
  const [themePref, setThemePref] = useState<ThemePref>(loadThemePref);
  const [error, setError] = useState<string | null>(null);
  const [pollError, setPollError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showAllBanners, setShowAllBanners] = useState(false);

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
        setBusy(true);
        setError(null);
      },
      apply: (next) => {
        configRef.current = next.config;
        setSnapshot(next);
      },
      fail: (cause, dropped) => {
        setError(errorText(cause));
        if (dropped > 0) {
          notify("error", translate.current("toast.queued_discarded"));
        }
      },
      finish: () => setBusy(false),
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
        const next = await api.snapshot();
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
      if (target) setPage(target);
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
   * Alt+1..9 jumps straight to the page at that position in the nav.
   *
   * A desktop utility is navigated from the keyboard as much as from the
   * pointer, and the nav is a fixed list, so the position *is* the address.
   * Matched by `code` rather than `key`, because Alt is a dead key on some
   * layouts — Option+1 on a Mac keyboard types "¡", and a shortcut that only
   * worked on US layouts would be a shortcut that mostly did not work.
   * Plain digits are left alone — they are typing — and so are every other
   * modifier combination, which belongs to the OS and the webview.
   */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
      const digit = /^Digit([1-9])$/.exec(e.code);
      if (!digit) return;
      const target = PAGES[Number(digit[1]) - 1];
      if (!target) return;
      e.preventDefault();
      setPage(target);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Overview and Activity show live state; their pages poll the counters
  // only. A failure is reported rather than swallowed: silence would look
  // like an idle proxy.
  useEffect(() => {
    if (page !== "activity" && page !== "overview") return;
    let cancelled = false;
    const poll = async () => {
      try {
        const activity = await api.activity();
        if (cancelled) return;
        setPollError(null);
        setSnapshot((current) => (current ? { ...current, ...activity } : current));
      } catch (e) {
        if (!cancelled) setPollError(errorText(e));
      }
    };
    const timer = setInterval(poll, 2000);
    void poll();
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [page]);

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
    <div className="shell">
      <nav className="sidebar" aria-label={t("nav.main_aria")}>
        <div className="sidebar-brand">
          Zroutery<span className="mono">v{snapshot.version}</span>
        </div>

        {NAV.map((item, index) => (
          <button type="button"
            key={item.id}
            className={`nav-item ${page === item.id ? "active" : ""}`}
            // Named as well as classed. The two layout harnesses drive the
            // shell with real clicks, and an ordinal is a promise that breaks
            // the moment a page is inserted: adding one silently repoints every
            // later index at a different page, which measures a healthy surface
            // and reports it as a pass. `data-page` survives the insert.
            data-page={item.id}
            // Which page is open is carried by the `active` class alone, and a
            // class is invisible to a screen reader: the nav read as eight
            // identical buttons with no indication of where you were.
            aria-current={page === item.id ? "page" : undefined}
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
              aria-label={WORKING_LABEL}
            />
          )}
          {/*
            * The page's name is a heading, so a screen-reader user can navigate
            * by it. Everything else about it — the size, the weight, the
            * uppercase — is in the stylesheet, and a heading's only other
            * difference is a margin the user agent would add.
            */}
          <h1 className="page-title">{t(NAV.find((n) => n.id === page)!.labelKey)}</h1>
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
                {/*
                 * Plain text rather than `t()`: the dictionary is owned by
                 * another change in flight, and a key added here would disappear
                 * in the merge. See the report for the keys this needs.
                 */}
                <button
                  type="button"
                  className="linky"
                  onClick={() => setShowAllBanners(true)}
                >
                  {moreNotificationsLabel(hiddenBanners)}
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
                  {FEWER_NOTIFICATIONS_LABEL}
                </button>
              </p>
            )}

            {page === "overview" && <Overview snapshot={snapshot} onNavigate={setPage} />}
            {page === "models" && <Models snapshot={snapshot} save={save} busy={busy} />}
            {page === "providers" && (
              <Providers snapshot={snapshot} save={save} run={run} busy={busy} />
            )}
            {page === "accounts" && (
              <Accounts snapshot={snapshot} run={run} busy={busy} />
            )}
            {page === "takeover" && <Takeover />}
            {page === "cutover" && <Cutover />}
            {page === "routing" && <Routing snapshot={snapshot} save={save} run={run} busy={busy} />}
            {page === "activity" && <Activity snapshot={snapshot} run={run} />}
            {page === "settings" && (
              <Settings
                snapshot={snapshot}
                save={save}
                run={run}
                busy={busy}
                themePref={themePref}
                onThemePref={setThemePref}
              />
            )}
          </div>
        </main>
      </div>
    </div>
  );
}
