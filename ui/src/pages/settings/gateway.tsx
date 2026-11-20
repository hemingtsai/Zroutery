/**
 * The Gateway pane of the settings page: the gateway transport, the local
 * token with its reveal/regenerate controls, the point-client snippets, and
 * the allowed origins.
 *
 * Cut out of `pages/Settings.tsx` by subject when the page grew too large to
 * read as one file; the shell in `pages/Settings.tsx` renders this next to
 * the general pane. The confirm dialog around the token regeneration stays
 * in the shell.
 */
import { api, errorText, type AppConfig, type Snapshot } from "../../api";
import {
  Button,
  CommitInput,
  CommitNumber,
  Section,
  useToast,
  type ConfirmRequest,
} from "../../components";
import { describeBackendError, useI18n } from "../../i18n";

export function GatewayPane({
  snapshot,
  save,
  run,
  busy,
  setConfirm,
  revealed,
  setRevealed,
  originError,
  setOriginError,
  originDraft,
  setOriginDraft,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
  setConfirm: (request: ConfirmRequest | null) => void;
  revealed: string | null;
  setRevealed: (value: string | null) => void;
  originError: string | null;
  setOriginError: (value: string | null) => void;
  originDraft: string;
  setOriginDraft: (value: string) => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const { config, server } = snapshot;

  const patchServer = (patch: Partial<AppConfig["server"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.server, patch);
      return next;
    });
  };

  const baseUrl = server.base_url ?? `http://${config.server.host}:${config.server.port}`;

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

  return gatewayPane;
}
