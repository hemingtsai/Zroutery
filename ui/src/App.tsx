import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, type AppConfig, type Snapshot } from "./api";
import { Banner, MenuItem, Popover, StatusDot, ToastProvider, useToast } from "./components";
import { I18nProvider, useI18n } from "./i18n";
import Overview from "./pages/Overview";
import Models from "./pages/Models";
import Providers from "./pages/Providers";
import Routing from "./pages/Routing";
import Activity from "./pages/Activity";
import Settings from "./pages/Settings";

/**
 * The doors into Zroutery. No icons, no section headers — a desktop utility's
 * navigation is a plain list of words, and the active one is simply the one
 * that reads darkest. Everything else lives inside a page: Auto review inside
 * Routing, the gateway inside Settings, diagnostics inside Settings.
 */
type Page = "overview" | "models" | "providers" | "routing" | "activity" | "settings";

const NAV: { id: Page; labelKey: Parameters<ReturnType<typeof useI18n>["t"]>[0] }[] = [
  { id: "overview", labelKey: "nav.overview" },
  { id: "models", labelKey: "nav.models" },
  { id: "providers", labelKey: "nav.providers" },
  { id: "routing", labelKey: "nav.routing" },
  { id: "activity", labelKey: "nav.activity" },
  { id: "settings", labelKey: "nav.settings" },
];

type ThemePref = "system" | "light" | "dark";

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

  // Tasks run one at a time: every save round trips through Rust and rewrites
  // the config file, so two overlapping saves could interleave and land out of
  // order. Tasks arriving mid-flight are queued in order and re-based onto the
  // latest committed config when they run.
  const runningRef = useRef(false);
  const queuedRef = useRef<Array<() => Promise<Snapshot>>>([]);

  // The newest config known to have been committed (or loaded). Saves are
  // updaters against this, so an edit queued behind an in-flight save is
  // applied on top of it instead of overwriting it with a stale snapshot.
  const configRef = useRef<AppConfig | null>(null);

  const run = useCallback(async (task: () => Promise<Snapshot>): Promise<boolean> => {
    const apply = (s: Snapshot) => {
      configRef.current = s.config;
      setSnapshot(s);
      return s;
    };
    if (runningRef.current) {
      queuedRef.current.push(task);
      // The queued task's own outcome decides; this call did not fail.
      return true;
    }
    runningRef.current = true;
    setBusy(true);
    setError(null);
    try {
      apply(await task());
      // Drain every edit that arrived while this one was in flight. Each task
      // reads configRef when it runs, so later edits rebase on earlier ones.
      while (queuedRef.current.length > 0) {
        const queued = queuedRef.current.shift()!;
        apply(await queued());
      }
      return true;
    } catch (e) {
      // A failed save invalidates the edits that were waiting behind it: they
      // were built against a config that never became authoritative.
      const dropped = queuedRef.current.length;
      queuedRef.current = [];
      setError(errorText(e));
      if (dropped > 0) notify("error", t("toast.queued_discarded"));
      return false;
    } finally {
      runningRef.current = false;
      setBusy(false);
    }
  }, [notify, t]);

  useEffect(() => {
    void run(api.snapshot);
  }, [run]);

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

  const unclassified = useMemo(
    () => snapshot?.config.models.filter((m) => m.class === null) ?? [],
    [snapshot],
  );

  if (!snapshot) {
    return (
      <main className="loading">
        <p>{error ?? t("common.loading")}</p>
      </main>
    );
  }

  const { server } = snapshot;

  return (
    <div className="shell">
      <nav className="sidebar" aria-label="Main">
        <div className="sidebar-brand">
          Zroutery<span className="mono">v{snapshot.version}</span>
        </div>

        {NAV.map((item) => (
          <button
            key={item.id}
            className={`nav-item ${page === item.id ? "active" : ""}`}
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
          {busy && <div className="busybar" aria-hidden />}
          <span className="page-title">{t(NAV.find((n) => n.id === page)!.labelKey)}</span>
          <div className="mainbar-right">
            {/* The gateway lives here, not in a bottom strip: top of the
                window is where a desktop app keeps global state. */}
            <Popover
              title={t("settings.gateway")}
              trigger={
                <>
                  <StatusDot tone={server.running ? "ok" : "danger"} />
                  {t("settings.gateway")}
                </>
              }
            >
              {(close) => (
                <>
                  <div className="menu-row">
                    <StatusDot tone={server.running ? "ok" : "danger"} />
                    {t(server.running ? "status.running" : "status.stopped")}
                  </div>
                  {server.running && (
                    <div className="menu-row">
                      <span className="menu-key">{t("gw.address")}</span>
                      <span className="mono">
                        {server.host}:{server.port}
                      </span>
                    </div>
                  )}
                  <div className="menu-sep" />
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
                      void run(server.running ? api.stop : api.start);
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
              <MenuItem active={themePref === "system"} onClick={() => setThemePref("system")}>
                {t("theme.system")}
              </MenuItem>
              <MenuItem active={themePref === "light"} onClick={() => setThemePref("light")}>
                {t("theme.light")}
              </MenuItem>
              <MenuItem active={themePref === "dark"} onClick={() => setThemePref("dark")}>
                {t("theme.dark")}
              </MenuItem>
            </Popover>
          </div>
        </header>

        <main className="content">
          <div className="page">
            {error && (
              <Banner
                tone="danger"
                actions={
                  <button className="linky" onClick={() => setError(null)}>
                    {t("common.dismiss")}
                  </button>
                }
              >
                {error}
              </Banner>
            )}
            {pollError && <Banner tone="warn">{t("app.live_stopped", { err: pollError })}</Banner>}
            {snapshot.warning && <Banner tone="warn">{snapshot.warning}</Banner>}
            {snapshot.issues
              .filter((i) => i.severity === "error")
              .map((i) => (
                <Banner key={`${i.code}:${i.subject ?? ""}`} tone="danger">
                  {i.message}
                </Banner>
              ))}
            {server.exposed && (
              <Banner tone="danger">{t("app.exposed", { host: server.host })}</Banner>
            )}
            {!server.require_auth && <Banner tone="warn">{t("app.no_auth")}</Banner>}

            {page === "overview" && <Overview snapshot={snapshot} onNavigate={setPage} />}
            {page === "models" && <Models snapshot={snapshot} save={save} busy={busy} />}
            {page === "providers" && (
              <Providers snapshot={snapshot} save={save} run={run} busy={busy} />
            )}
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
