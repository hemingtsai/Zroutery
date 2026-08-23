import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, type AppConfig, type Snapshot } from "./api";
import { Badge, Banner, Button } from "./components";
import Providers from "./panels/Providers";
import Models from "./panels/Models";
import Routing from "./panels/Routing";
import Activity from "./panels/Activity";

type Tab = "providers" | "models" | "routing" | "activity";

const TABS: { id: Tab; label: string }[] = [
  { id: "providers", label: "Providers" },
  { id: "models", label: "Models" },
  { id: "routing", label: "Routing" },
  { id: "activity", label: "Activity" },
];

export default function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [tab, setTab] = useState<Tab>("providers");
  const [error, setError] = useState<string | null>(null);
  const [pollError, setPollError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Tasks run one at a time: every save round trips through Rust and rewrites
  // the config file, so two overlapping saves could interleave and land out of
  // order. A task arriving mid-flight is queued and runs when the current one
  // settles, which keeps the last edit the last write.
  const runningRef = useRef(false);
  const queuedRef = useRef<(() => Promise<Snapshot>) | null>(null);

  const run = useCallback(async (task: () => Promise<Snapshot>) => {
    if (runningRef.current) {
      queuedRef.current = task;
      return;
    }
    runningRef.current = true;
    setBusy(true);
    setError(null);
    try {
      setSnapshot(await task());
      const queued = queuedRef.current;
      queuedRef.current = null;
      if (queued) setSnapshot(await queued());
    } catch (e) {
      setError(errorText(e));
    } finally {
      runningRef.current = false;
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void run(api.snapshot);
  }, [run]);

  // The Activity tab needs fresh numbers. It polls the counters only, and a
  // failure is reported rather than swallowed: silence would look like an idle
  // proxy.
  useEffect(() => {
    if (tab !== "activity") return;
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
  }, [tab]);

  const save = useCallback(
    (config: AppConfig) => run(() => api.saveConfig(config)),
    [run],
  );

  const unclassified = useMemo(
    () => snapshot?.config.models.filter((m) => m.class === null) ?? [],
    [snapshot],
  );

  if (!snapshot) {
    return (
      <main className="loading">
        <p>{error ?? "Loading…"}</p>
      </main>
    );
  }

  const { server } = snapshot;
  const baseUrl = server.base_url ?? `http://${server.host}:${server.port}`;

  return (
    <div className="app">
      <header className="titlebar" data-tauri-drag-region>
        <div className="brand">
          <span className={`dot ${server.running ? "dot-on" : "dot-off"}`} aria-hidden />
          <strong>Zroutery</strong>
          <span className="muted">v{snapshot.version}</span>
        </div>

        <div className="row gap">
          <code className="url" title={baseUrl}>
            {baseUrl}
          </code>
          <Button kind="ghost" onClick={() => api.copy(baseUrl)} title="Copy the base URL">
            Copy URL
          </Button>
          <Button
            kind="ghost"
            onClick={() => api.copyToken()}
            title="Copy the local API token to the clipboard"
          >
            Copy token
          </Button>
          <Button
            kind={server.running ? "default" : "primary"}
            disabled={busy}
            onClick={() => run(server.running ? api.stop : api.start)}
          >
            {server.running ? "Stop proxy" : "Start proxy"}
          </Button>
        </div>
      </header>

      <nav className="tabs" role="tablist">
        {TABS.map((t) => (
          <button
            key={t.id}
            role="tab"
            aria-selected={tab === t.id}
            className={`tab ${tab === t.id ? "tab-active" : ""}`}
            onClick={() => setTab(t.id)}
          >
            {t.label}
            {t.id === "models" && unclassified.length > 0 && (
              <Badge tone="warn">{unclassified.length}</Badge>
            )}
          </button>
        ))}
      </nav>

      <main className="content">
        {error && (
          <Banner
            tone="danger"
            actions={
              <Button kind="ghost" onClick={() => setError(null)}>
                Dismiss
              </Button>
            }
          >
            {error}
          </Banner>
        )}
        {pollError && (
          <Banner tone="warn">Live updates stopped: {pollError}</Banner>
        )}
        {snapshot.warning && <Banner tone="warn">{snapshot.warning}</Banner>}
        {server.exposed && (
          <Banner tone="danger">
            The proxy is bound to <code>{server.host}</code>, so other machines on your network
            can use your API keys. Keep authentication enabled or switch back to{" "}
            <code>127.0.0.1</code>.
          </Banner>
        )}
        {!server.require_auth && (
          <Banner tone="warn">
            Authentication is off: any local process can spend your API credit.
          </Banner>
        )}
        {snapshot.issues
          .filter((i) => i.code === "server.cors_any_origin")
          .map((i) => (
            <Banner key={i.code} tone="danger">
              {i.message}
            </Banner>
          ))}

        {tab === "providers" && <Providers snapshot={snapshot} save={save} run={run} busy={busy} />}
        {tab === "models" && <Models snapshot={snapshot} save={save} busy={busy} />}
        {tab === "routing" && <Routing snapshot={snapshot} save={save} run={run} busy={busy} />}
        {tab === "activity" && <Activity snapshot={snapshot} run={run} />}
      </main>

      <footer className="statusbar">
        <span className="muted">{snapshot.config_path}</span>
        <span className="row gap">
          <Button kind="ghost" onClick={() => api.hide()}>
            Hide window
          </Button>
          <Button kind="ghost" onClick={() => api.quit()} title="Stop the proxy and quit">
            Quit
          </Button>
        </span>
      </footer>
    </div>
  );
}

