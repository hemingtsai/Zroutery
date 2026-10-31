import { useEffect, useState } from "react";
import { api, errorText } from "../api";
import {
  type AppConfig,
  type Snapshot,
} from "../api";
import {
  Button,
  CommitInput,
  CommitNumber,
  ConfirmDialog,
  PageHead,
  Section,
  Segment,
  Select,
  Tabs,
  useToast,
  type ConfirmRequest,
} from "../components";
import { useI18n, describeBackendError } from "../i18n";
import type { UiMode } from "../mode";
import type { Skin } from "../skin";
/**
 * Settings is deliberately the most conventional page: gateway transport,
 * spending limits, and the deep knobs. Nothing here is needed to understand
 * routing — it is all "how the box is wired".
 */
export default function Settings({
  snapshot,
  save,
  run,
  busy,
  themePref,
  onThemePref,
  skin,
  onSkinChange,
  mode,
  onModeChange,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
  themePref: "system" | "light" | "dark";
  onThemePref: (pref: "system" | "light" | "dark") => void;
  skin: Skin;
  onSkinChange: (skin: Skin) => void;
  mode: UiMode;
  onModeChange: (mode: UiMode) => void;
}) {
  const { config, server } = snapshot;
  const { t, lang, setLang } = useI18n();
  const notify = useToast();
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [originError, setOriginError] = useState<string | null>(null);
  const [originDraft, setOriginDraft] = useState("");
  const [revealed, setRevealed] = useState<string | null>(null);
  // Simple mode keeps the general tab alone — the gateway pane is an advanced
  // surface. A reader standing on it when the mode narrows falls back to the
  // tab that remains.
  const simpleOnly = mode === "simple";

  const patchServer = (patch: Partial<AppConfig["server"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.server, patch);
      return next;
    });
  };

  const patchWindow = (patch: Partial<AppConfig["window"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.window, patch);
      return next;
    });
  };

  const patchBalances = (patch: Partial<AppConfig["balances"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.balances, patch);
      return next;
    });
  };

  const addOrigin = () => {
    const origin = originDraft.trim().replace(/\/$/, "");
    if (!origin) return;
    if (!/^https?:\/\//.test(origin)) {
      setOriginError(t("settings.origin_notice"));
      return;
    }
    setOriginDraft("");
    setOriginError(null);
    void save((cfg) => {
      if (cfg.server.cors_origins.includes(origin)) return null;
      const next = structuredClone(cfg);
      next.server.cors_origins = [...cfg.server.cors_origins, origin];
      return next;
    });
  };

  const baseUrl = server.base_url ?? `http://${config.server.host}:${config.server.port}`;

  /**
   * The four panes, hoisted so the tab strip can place them without the panes
   * and the tab shell duplicating any markup.
   *
   * The cut is by concern rather than by size. Three sections belong to the HTTP
   * server other machines talk to — its address and token, the snippets that
   * point a client at it, and the browser origins allowed to reach it — and
   * reading them as three unrelated sections is what made the gateway the
   * longest thing on the page.
   */
  const generalPane = (
    <>
        <Section title={t("appearance.title")}>
          {simpleOnly && (
            /*
              * The mode row, in simple mode only: the reader it serves is the
              * one who wants out of simple mode or reassurance about what it
              * does. It is the same Segment the sidebar foot carries — two
              * entrances, one state.
              */
            <div className="setting-row">
              <div className="setting-left">
                <span className="setting-title">{t("mode.toggle_aria")}</span>
                <span className="setting-desc">{t("mode.settings_hint")}</span>
              </div>
              <div className="setting-right">
                <Segment<UiMode>
                  ariaLabel={t("mode.toggle_aria")}
                  value={mode}
                  onChange={onModeChange}
                  options={[
                    { value: "simple", label: t("mode.simple") },
                    { value: "geek", label: t("mode.geek") },
                  ]}
                />
              </div>
            </div>
          )}
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.language")}</span>
            </div>
            <div className="setting-right">
              <Segment
                ariaLabel={t("settings.language")}
                value={lang}
                onChange={setLang}
                options={[
                  { value: "zh", label: t("lang.zh") },
                  { value: "zh-TW", label: t("lang.zh_tw") },
                  { value: "en", label: t("lang.en") },
                ]}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.theme")}</span>
            </div>
            <div className="setting-right">
              <Select<"system" | "light" | "dark">
                ariaLabel={t("settings.theme")}
                value={themePref}
                onChange={onThemePref}
                options={[
                  { value: "system", label: t("theme.system") },
                  { value: "light", label: t("theme.light") },
                  { value: "dark", label: t("theme.dark") },
                ]}
              />
            </div>
          </div>
          {/*
            * The skin axis sits beside the theme axis: a skin re-inks the same
            * information structure, and a theme flips it light or dark, so the
            * two dropdowns read as one pair at the foot of the appearance
            * section.
            */}
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.skin")}</span>
            </div>
            <div className="setting-right">
              <Select<Skin>
                ariaLabel={t("settings.skin")}
                value={skin}
                onChange={onSkinChange}
                options={[
                  { value: "industrial", label: t("skin.industrial") },
                  { value: "md3", label: t("skin.md3") },
                  { value: "fluent", label: t("skin.fluent") },
                ]}
              />
            </div>
          </div>
        </Section>

        <Section title={t("settings.window")}>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("win.launch_on_login")}</span>
              <span className="setting-desc">{t("win.launch_on_login_hint")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("win.launch_on_login")}
                checked={config.window.launch_on_login}
                onChange={(e) => patchWindow({ launch_on_login: e.currentTarget.checked })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("win.silent_start")}</span>
              <span className="setting-desc">{t("win.silent_start_hint")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("win.silent_start")}
                checked={config.window.silent_start}
                onChange={(e) => patchWindow({ silent_start: e.currentTarget.checked })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("win.keep_in_tray")}</span>
              <span className="setting-desc">{t("win.keep_in_tray_hint")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("win.keep_in_tray")}
                checked={config.window.keep_in_tray}
                onChange={(e) => patchWindow({ keep_in_tray: e.currentTarget.checked })}
              />
            </div>
          </div>
          {/*
            The two ways to leave, next to the setting that decides what closing
            the window does.

            Neither is reachable from the window's own close button once
            keep-in-tray is on, and the tray is not a place a first-time user
            looks: the box says "closing keeps it running", so the way to actually
            stop it has to be here. `quit_app` is the same code path the tray's
            Quit item runs — the proxy is stopped before the process ends — which
            is why it is a button rather than a bare window close.
          */}
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("win.window_actions")}</span>
              <span className="setting-desc">{t("win.window_actions_hint")}</span>
            </div>
            <div className="setting-right">
              <Button kind="ghost" onClick={() => void api.hide()}>
                {t("win.hide_to_tray")}
              </Button>
              <Button kind="danger" onClick={() => void api.quit()}>
                {t("win.quit")}
              </Button>
            </div>
          </div>
        </Section>

        <Section title={t("settings.balances")} hint={t("settings.balances_hint")}>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.balance_auto")}</span>
              <span className="setting-desc">{t("settings.balance_auto_hint")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("settings.balance_auto")}
                checked={config.balances.enabled}
                onChange={(e) => patchBalances({ enabled: e.currentTarget.checked })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.balance_interval")}</span>
              <span className="setting-desc">{t("settings.balance_interval_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={60}
                max={86400}
                integer
                value={config.balances.interval_secs}
                ariaLabel={t("settings.balance_interval")}
                onCommit={(v) => patchBalances({ interval_secs: v ?? 900 })}
              />
            </div>
          </div>
        </Section>

        {simpleOnly && (
          /*
            * The about section, in simple mode only: the gateway pane it
            * complements is hidden there, and with it the one place that named
            * the configuration file. Version and path are facts, not knobs, so
            * the section reads and changes nothing.
            */
          <Section title={t("settings.about")} hint={t("settings.about_note")}>
            <div className="kv-row">
              <span className="kv-key">{t("settings.version")}</span>
              <span className="kv-val mono">v{snapshot.version}</span>
            </div>
            <div className="kv-row">
              <span className="kv-key">{t("settings.config_path")}</span>
              <span className="kv-val mono">{snapshot.config_path}</span>
            </div>
          </Section>
        )}
    </>
  );

  const gatewayPane = (
    <>
        <Section
          title={t("settings.gateway")}
          hint={t("settings.gateway_hint", { url: baseUrl, path: snapshot.config_path })}
        >
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("field.host")}</span>
              <span className="setting-desc">{t("field.host_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitInput
                value={config.server.host}
                ariaLabel={t("field.host")}
                onCommit={(host) => patchServer({ host })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("field.port")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={1}
                max={65535}
                integer
                value={config.server.port}
                ariaLabel={t("field.port")}
                onCommit={(port) => port && patchServer({ port })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("field.body_limit")}</span>
              <span className="setting-desc">{t("field.body_limit_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={1}
                max={512}
                integer
                value={config.server.max_body_mib}
                ariaLabel={t("field.body_limit")}
                onCommit={(v) => patchServer({ max_body_mib: v ?? 32 })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("field.log_limit")}</span>
              <span className="setting-desc">{t("field.log_limit_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={10}
                max={5000}
                integer
                value={config.server.log_limit}
                ariaLabel={t("field.log_limit")}
                onCommit={(v) => patchServer({ log_limit: v ?? 500 })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.require_auth")}</span>
              <span className="setting-desc">{t("settings.require_auth_hint")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("settings.require_auth")}
                checked={config.server.require_auth}
                onChange={(e) => patchServer({ require_auth: e.currentTarget.checked })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.autostart")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("settings.autostart")}
                checked={config.server.autostart}
                onChange={(e) => patchServer({ autostart: e.currentTarget.checked })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.bypass_proxy")}</span>
              <span className="setting-desc">{t("settings.bypass_proxy_hint")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("settings.bypass_proxy")}
                checked={config.server.bypass_proxy}
                onChange={(e) => patchServer({ bypass_proxy: e.currentTarget.checked })}
              />
            </div>
          </div>

          <div className="kv-row">
            <span className="kv-key">{t("settings.local_token")}</span>
            <span className="kv-val mono">{revealed ?? server.token_hint}</span>
          </div>
          <div className="controls">
            {revealed ? (
              <Button kind="ghost" onClick={() => setRevealed(null)}>
                {t("action.hide")}
              </Button>
            ) : (
              <Button
                kind="ghost"
                onClick={async () => {
                  try {
                    setRevealed(await api.revealToken());
                  } catch (e) {
                    notify("error", describeBackendError(t, errorText(e)));
                  }
                }}
                title={t("settings.reveal_title")}
              >
                {t("settings.reveal")}
              </Button>
            )}
            <Button kind="ghost" onClick={() => api.copyToken().then(() => notify("ok", t("toast.copied")))}>
              {t("settings.copy")}
            </Button>
            <Button
              kind="danger"
              onClick={() =>
                setConfirm({
                  title: t("confirm.regenerate_token"),
                  body: t("confirm.regenerate_token_body"),
                  confirmLabel: t("settings.regenerate"),
                  danger: true,
                  onConfirm: () => {
                    setRevealed(null);
                    void run(api.regenerateToken);
                  },
                })
              }
              title={t("settings.regenerate_title")}
            >
              {t("settings.regenerate")}
            </Button>
          </div>
          <p className="field-hint">{t("settings.token_hint")}</p>
        </Section>

        <Section title={t("settings.point_client")}>
          <p className="section-hint">{t("settings.anthropic_clients")}</p>
          <pre className="snippet">
            {`export ANTHROPIC_BASE_URL=${baseUrl}
  export ANTHROPIC_AUTH_TOKEN=<paste the token>
  export ANTHROPIC_MODEL=standard-class`}
          </pre>
          <p className="section-hint">{t("settings.openai_clients")}</p>
          <pre className="snippet">
            {`export OPENAI_BASE_URL=${baseUrl}/v1
  export OPENAI_API_KEY=<paste the token>
  ${t("settings.snippet_comment")}`}
          </pre>
        </Section>

        <Section title={t("settings.cors")} hint={t("settings.cors_hint")}>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.allow_origins")}</span>
            </div>
            <div className="setting-right">
              <input
                type="checkbox"
                aria-label={t("settings.allow_origins")}
                checked={config.server.allow_cors}
                onChange={(e) => patchServer({ allow_cors: e.currentTarget.checked })}
              />
            </div>
          </div>
          {config.server.allow_cors && (
            <>
              {config.server.cors_origins.length === 0 ? (
                <p className="field-hint">{t("settings.no_origins")}</p>
              ) : (
                <div className="chips">
                  {config.server.cors_origins.map((origin) => (
                    <span key={origin} className="chip chip-done">
                      {origin}
                      <button type="button"
                        className="chip-remove"
                        aria-label={t("settings.remove_origin", { origin })}
                        onClick={() =>
                          patchServer({
                            cors_origins: config.server.cors_origins.filter((o) => o !== origin),
                          })
                        }
                      >
                        ×
                      </button>
                    </span>
                  ))}
                </div>
              )}
              <div className="setting-row">
                <div className="setting-left">
                  <span className="setting-title">{t("field.allowed_origin")}</span>
                  <span className="setting-desc">{originError ?? t("field.allowed_origin_hint")}</span>
                </div>
                <div className="setting-right">
                  <input
                    aria-label={t("field.allowed_origin")}
                    value={originDraft}
                    placeholder="http://localhost:3000"
                    className={originError ? "input-error" : undefined}
                    onChange={(e) => {
                      setOriginDraft(e.currentTarget.value);
                      setOriginError(null);
                    }}
                    onKeyDown={(e) => e.key === "Enter" && addOrigin()}
                  />
                  <Button onClick={addOrigin} disabled={busy || !originDraft.trim()}>
                    {t("settings.add_origin")}
                  </Button>
                </div>
              </div>
            </>
          )}
        </Section>
    </>
  );

  const [tab, setTab] = useState<"general" | "gateway">("general");
  useEffect(() => {
    if (simpleOnly && tab !== "general") setTab("general");
  }, [simpleOnly, tab]);

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      <PageHead lede={t("settings.lede")} />

      <Tabs
        value={simpleOnly ? "general" : tab}
        onChange={(next) => setTab(next as typeof tab)}
        ariaLabel={t("settings.tab_group")}
        tabs={
          simpleOnly
            ? [{ value: "general", label: t("settings.tab_general"), content: generalPane }]
            : [
                { value: "general", label: t("settings.tab_general"), content: generalPane },
                { value: "gateway", label: t("settings.tab_gateway"), content: gatewayPane },
              ]
        }
      />
    </>
  );
}
