/**
 * The application shell: one screen, one configuration, one save queue.
 *
 * The wiring lives here; the pieces live beside it — `shell/` holds the nav,
 * the main-bar menus, the banner stack and the effect-shaped behaviours, and
 * `components/` holds the shared primitives. This file keeps only what is
 * genuinely entangled: the state, the save pipeline, and the page routing.
 */
import { Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { api, errorText, type AppConfig, type Snapshot } from "./api";
import { Tabs, ToastProvider, useToast } from "./components";
import { describeBackendError, I18nProvider, useI18n } from "./i18n";
import { ModeContext, loadMode, storeMode, type UiMode } from "./mode";
import {
  createSaveQueue,
  createSaveVerdict,
  SaveFeedbackResetContext,
  SaveStatusContext,
  type SaveQueue,
  type SaveStatus,
  type SaveVerdict,
} from "./saveQueue";
import { applySkin, loadSkin, SkinContext, storeSkin, type Skin } from "./skin";
import { applyTheme, loadThemePref, storeThemePref, type ThemePref } from "./theme";
import { taskFor, type Page } from "./navigation";
import { Accounts, Activity, Cutover, Models, Overview, Providers, Routing, Settings, Takeover } from "./shell/pages";
import { NAV } from "./shell/nav";
import { Sidebar } from "./shell/Sidebar";
import { AppearanceMenu, GatewayMenu, toggleGateway } from "./shell/menus";
import { BannerStack } from "./shell/BannerStack";
import {
  useActivityPolling,
  useAltDigitNav,
  useDeepLink,
  useSnapshotRefreshTriggers,
  useSystemAccent,
} from "./shell/hooks";
import { safeAppearance } from "./themes/registry";

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
  // Skins that render the nav indicator pill morph it across items with a
  // view transition (the shared-element snapshot slides — see industrial.css).
  // flushSync is required inside the transition callback or React defers the
  // DOM change past the frame the API captures. Reduced-motion users, and
  // browsers without the API, navigate instantly; industrial's iconless nav
  // produces no named snapshot either way, so its pages stay unanimated.
  const navigate = (next: Page) => {
    // The morph runs only for skins that declare `nav_indicator_morph` (the
    // compiler publishes the marker var in the theme root). The layout
    // harness injects its own global and scripts click sequences whose
    // timing assumes synchronous navigation.
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const harness = "__zrHarnessReady" in window;
    const morph = getComputedStyle(document.documentElement)
      .getPropertyValue("--nav-indicator-morph").trim() !== "";
    if (!document.startViewTransition || reduce || harness || !morph) {
      setPage(next);
      return;
    }
    document.startViewTransition(() => flushSync(() => setPage(next)));
  };
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
  }, [skin]);
  const changeSkin = (next: Skin) => {
    if (next === skin) return;
    storeSkin(next);
    applySkin(next);
    setSkin(next);
  };
  const changeThemePref = (next: ThemePref) => {
    storeThemePref(next);
    applyTheme(next);
    setThemePref(next);
  };
  const [error, setError] = useState<string | null>(null);
  const [pollError, setPollError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The save truth at drawer distance. begin/finish/fail below keep it in step
  // with the queue; Drawer reads it through context and speaks it at its foot.
  const [saveStatus, setSaveStatus] = useState<SaveStatus>({ phase: "idle" });
  const configEpoch = useRef(0);
  const saving = useRef(false);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus({ preventScroll: true }); }, [page]);

  useEffect(() => {
    const apply = () => {
      applyTheme(themePref);
    };
    apply();
    // "Follow system" has to keep following: listen while it is the preference.
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [themePref]);

  useSystemAccent(themePref);

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
  const verdictRef = useRef<SaveVerdict | null>(null);
  if (!queueRef.current) {
    // The verdict is the queue's truth phrased for a drawer's foot. Only a
    // configuration write may speak there: reads and observations — the
    // startup snapshot, a balance refresh, an account probe — apply their
    // snapshot and claim nothing, and a freshly mounted drawer inherits
    // nothing (see `SaveFeedbackReset` in the drawers themselves).
    const verdict = createSaveVerdict(setSaveStatus);
    verdictRef.current = verdict;
    queueRef.current = createSaveQueue({
      begin: (kind) => {
        saving.current = true;
        configEpoch.current += 1;
        setBusy(true);
        setError(null);
        verdict.begin(kind);
      },
      apply: (next) => {
        configEpoch.current += 1;
        configRef.current = next.config;
        setSnapshot(next);
      },
      fail: (cause, dropped, kind) => {
        setError(errorText(cause));
        verdict.fail(errorText(cause), kind);
        // Only a write holds edits behind it; its failure is theirs too. An
        // observation failed alone, and the surface that asked for it says so.
        if (kind === "write" && dropped > 0) {
          notify("error", translate.current("toast.queued_discarded"));
        }
      },
      finish: (kind) => {
        saving.current = false;
        setBusy(false);
        verdict.finish(kind);
      },
    });
  }
  const run = queueRef.current.run;

  // A drawer that mounts clears the verdict instead of inheriting it, so one
  // entity's "saved" never greets the next entity's drawer.
  const resetSaveFeedback = useCallback(() => verdictRef.current?.reset(), []);

  // The first read is an observation: it fills the pages and claims nothing.
  // A drawer opened afterwards must not meet a "saved" stamp it never earned.
  useEffect(() => {
    void run(api.snapshot, "observe");
  }, [run]);
  useEffect(() => { if (snapshot?.safe_appearance) safeAppearance(); }, [snapshot?.safe_appearance]);

  /**
   * Re-read the whole snapshot without touching the save queue, coalesced.
   *
   * A refresh that began before a save/identity publication cannot undo it:
   * the epoch guard drops anything that raced a save.
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

  useSnapshotRefreshTriggers(refresh);
  useDeepLink(setPage);
  useAltDigitNav(NAV, setPage);

  // Overview and Activity show live state; the sampling callbacks are stable
  // state setters, so the poll effect subscribes once per task page, exactly
  // as when this effect lived inline with `[taskPage]` deps.
  const onPollSample = useCallback((activity: Awaited<ReturnType<typeof api.activity>>) => {
    setPollError(null);
    setSnapshot((current) => (current ? { ...current, ...activity } : current));
  }, []);
  const onPollError = useCallback((message: string | null) => setPollError(message), []);
  useActivityPolling(taskPage, onPollSample, onPollError);

  const onGatewayToggle = useCallback(() => {
    void toggleGateway(busy, run, setError, setSnapshot);
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

  return (
    <ModeContext.Provider value={mode}>
    <SkinContext.Provider value={skin}>
    <SaveStatusContext.Provider value={saveStatus}>
    <SaveFeedbackResetContext.Provider value={resetSaveFeedback}>
    <div className="shell">
      <Sidebar snapshot={snapshot} taskPage={taskPage} unclassified={unclassified} onNavigate={navigate} />

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
            <GatewayMenu server={server} onToggle={onGatewayToggle} />

            {/* Appearance: a half-moon reads as "Appearance", not "dark
                mode", and the three-way choice includes following the OS. */}
            <AppearanceMenu themePref={themePref} onThemePref={changeThemePref} />
          </div>
        </header>

        <main className="content">
          <div className="page">
            <BannerStack
              error={error}
              onDismissError={() => setError(null)}
              pollError={pollError}
              onDismissPollError={() => setPollError(null)}
              snapshot={snapshot}
              onNavigate={setPage}
            />

            <Suspense fallback={<p className="empty" role="status">{t("common.loading")}</p>}>
            {page === "overview" && (
              <Overview
                snapshot={snapshot}
                onNavigate={setPage}
                // The pool drags on the simple home save through the same
                // queue as every page. The add-models hand-off navigates only;
                // the mode is a Settings preference and stays out of the way.
                save={save}
                run={run}
                busy={busy}
              />
            )}
            {taskPage === "models" && <Tabs value={page} onChange={(next) => setPage(next as Page)} ariaLabel={t("task.models")}
              tabs={[
                { value: "models", label: t("task.catalogue"), content: <Models snapshot={snapshot} save={save} busy={busy} /> },
                { value: "routing", label: t("task.rules"), content: <Routing snapshot={snapshot} save={save} run={run} busy={busy} /> },
              ]} />}
            {taskPage === "providers" && <Tabs
              value={page}
              onChange={(next) => setPage(next as Page)}
              ariaLabel={t("task.providers")}
              tabs={[
                { value: "providers", label: t("task.connections"), content: <Providers snapshot={snapshot} save={save} run={run} busy={busy} /> },
                { value: "accounts", label: t("task.accounts"), content: <Accounts snapshot={snapshot} run={run} busy={busy} /> },
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
                onThemePref={changeThemePref}
                skin={skin}
                onSkinChange={changeSkin}
                mode={mode}
                onModeChange={applyMode}
              />
            )}
            </Suspense>
          </div>
        </main>
      </div>
    </div>
    </SaveFeedbackResetContext.Provider>
    </SaveStatusContext.Provider>
    </SkinContext.Provider>
    </ModeContext.Provider>
  );
}
