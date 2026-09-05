import { useCallback, useEffect, useState } from "react";
import { api, costText, errorText, modelRows, money, virtualId } from "../api";
import {
  CLASSES,
  type AppConfig,
  type Budget,
  type BudgetPeriod,
  type BudgetScope,
  type ModelClass,
  type Snapshot,
} from "../api";
import {
  Badge,
  Banner,
  Button,
  Field,
  NumberField,
  PageHead,
  Section,
  Segment,
  Select,
  TextField,
  Toggle,
} from "../components";
import { useI18n } from "../i18n";

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
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
  themePref: "system" | "light" | "dark";
  onThemePref: (pref: "system" | "light" | "dark") => void;
}) {
  const { config, server } = snapshot;
  const { t, lang, setLang } = useI18n();
  // Candidates for the vision fallback: enabled models that can see, on
  // enabled providers. Mirrors what the backend resolver accepts.
  const visionCapable = modelRows(snapshot).filter(
    (r) =>
      r.model.enabled &&
      r.model.supports_vision &&
      config.providers.find((p) => p.id === r.model.provider_id)?.enabled,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const [aliasDraft, setAliasDraft] = useState({ from: "", to: "sonnet" as ModelClass });
  const [originDraft, setOriginDraft] = useState("");
  const [budgetDraft, setBudgetDraft] = useState({
    scope: "global",
    period: "day" as BudgetPeriod,
    amount: 0,
    currency: "USD",
  });
  const [revealed, setRevealed] = useState<string | null>(null);
  const [logs, setLogs] = useState<string[] | null>(null);

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

  const patchVision = (patch: Partial<AppConfig["vision"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.vision, patch);
      return next;
    });
  };

  const patchScoring = (patch: Partial<AppConfig["routing"]["scoring"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.routing.scoring, patch);
      return next;
    });
  };

  const parseScope = (value: string): BudgetScope => {
    const [kind, rest] = value.split(":");
    if (kind === "class" && CLASSES.includes(rest as ModelClass)) return { kind: "class", class: rest as ModelClass };
    if (kind === "provider") return { kind: "provider", id: rest };
    return { kind: "global" };
  };

  const addBudget = () => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      next.budgets.push({
        id: "",
        scope: parseScope(budgetDraft.scope),
        period: budgetDraft.period,
        limit: {
          currency: budgetDraft.currency.trim().toUpperCase() || "USD",
          amount: budgetDraft.amount,
        },
        on_exceeded: { action: "reject" },
        enabled: true,
      });
      return next;
    });
    setBudgetDraft({ ...budgetDraft, amount: 0 });
  };

  const patchBudget = (id: string, patch: Partial<Budget>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const budget = next.budgets.find((b) => b.id === id);
      if (!budget) return null;
      Object.assign(budget, patch);
      return next;
    });
  };

  const removeBudget = (id: string) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const index = next.budgets.findIndex((b) => b.id === id);
      if (index < 0) return null;
      next.budgets.splice(index, 1);
      return next;
    });
  };

  const addAlias = () => {
    const from = aliasDraft.from.trim();
    if (!from) return;
    void save((cfg) => {
      const next = structuredClone(cfg);
      next.routing.client_aliases[from] = aliasDraft.to;
      return next;
    });
    setAliasDraft({ from: "", to: aliasDraft.to });
  };

  const removeAlias = (from: string) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      delete next.routing.client_aliases[from];
      return next;
    });
  };

  const addOrigin = () => {
    const origin = originDraft.trim().replace(/\/$/, "");
    if (!origin) return;
    if (!/^https?:\/\//.test(origin)) {
      setNotice(t("settings.origin_notice"));
      return;
    }
    setOriginDraft("");
    setNotice(null);
    void save((cfg) => {
      if (cfg.server.cors_origins.includes(origin)) return null;
      const next = structuredClone(cfg);
      next.server.cors_origins = [...cfg.server.cors_origins, origin];
      return next;
    });
  };

  const loadLogs = useCallback(async () => {
    try {
      setLogs(await api.logs());
    } catch {
      setLogs([]);
    }
  }, []);

  useEffect(() => {
    if (logs === null) return;
    const timer = setInterval(loadLogs, 1500);
    return () => clearInterval(timer);
  }, [logs !== null, loadLogs]);

  const baseUrl = server.base_url ?? `http://${config.server.host}:${config.server.port}`;

  return (
    <>
      {notice && (
        <Banner
          tone="warn"
          actions={
            <Button kind="ghost" onClick={() => setNotice(null)}>
              {t("common.ok")}
            </Button>
          }
        >
          {notice}
        </Banner>
      )}

      <PageHead lede={t("settings.lede")} />

      <Section title={t("appearance.title")}>
        <div className="controls">
          <Field label={t("settings.language")}>
            <Segment
              ariaLabel={t("settings.language")}
              value={lang}
              onChange={setLang}
              options={[
                { value: "zh", label: t("lang.zh") },
                { value: "en", label: t("lang.en") },
              ]}
            />
          </Field>
          <Field label={t("settings.theme")}>
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
          </Field>
        </div>
      </Section>

      <Section title={t("settings.window")}>
        <div className="grid-two">
          <Toggle
            label={t("win.launch_on_login")}
            hint={t("win.launch_on_login_hint")}
            checked={config.window.launch_on_login}
            onChange={(launch_on_login) => patchWindow({ launch_on_login })}
          />
          <Toggle
            label={t("win.silent_start")}
            hint={t("win.silent_start_hint")}
            checked={config.window.silent_start}
            onChange={(silent_start) => patchWindow({ silent_start })}
          />
          <Toggle
            label={t("win.keep_in_tray")}
            hint={t("win.keep_in_tray_hint")}
            checked={config.window.keep_in_tray}
            onChange={(keep_in_tray) => patchWindow({ keep_in_tray })}
          />
        </div>
      </Section>

      <Section title={t("vision.title")} hint={t("vision.hint")}>
        <Toggle
          label={t("vision.enable")}
          checked={config.vision.enabled}
          onChange={(enabled) => patchVision({ enabled })}
        />
        {config.vision.enabled && (
          <>
            <div className="controls">
              <Field label={t("vision.model")} hint={t("vision.model_hint")}>
                <Select
                  ariaLabel={t("vision.model")}
                  value={config.vision.model}
                  onChange={(model) => patchVision({ model: model || null })}
                  placeholder="—"
                  options={visionCapable.map((r) => ({
                    value: r.id,
                    label: r.id,
                  }))}
                />
              </Field>
              <TextField
                label={t("vision.placeholder")}
                hint={t("vision.placeholder_hint")}
                value={config.vision.placeholder}
                onCommit={(placeholder) =>
                  placeholder.trim() && patchVision({ placeholder })
                }
              />
            </div>
            {visionCapable.length === 0 && (
              <p className="field-hint">{t("vision.model_required")}</p>
            )}
          </>
        )}
      </Section>

      <Section
        title={t("settings.gateway")}
        hint={t("settings.gateway_hint", { url: baseUrl, path: snapshot.config_path })}
      >
        <div className="controls">
          <TextField
            label={t("field.host")}
            hint={t("field.host_hint")}
            value={config.server.host}
            onCommit={(host) => patchServer({ host })}
          />
          <NumberField
            label={t("field.port")}
            min={1}
            max={65535}
            integer
            value={config.server.port}
            onCommit={(port) => port && patchServer({ port })}
          />
          <NumberField
            label={t("field.body_limit")}
            hint={t("field.body_limit_hint")}
            min={1}
            max={512}
            integer
            value={config.server.max_body_mib}
            onCommit={(v) => patchServer({ max_body_mib: v ?? 32 })}
          />
          <NumberField
            label={t("field.log_limit")}
            hint={t("field.log_limit_hint")}
            min={10}
            max={5000}
            integer
            value={config.server.log_limit}
            onCommit={(v) => patchServer({ log_limit: v ?? 500 })}
          />
        </div>
        <div className="grid-two">
          <Toggle
            label={t("settings.require_auth")}
            hint={t("settings.require_auth_hint")}
            checked={config.server.require_auth}
            onChange={(require_auth) => patchServer({ require_auth })}
          />
          <Toggle
            label={t("settings.autostart")}
            checked={config.server.autostart}
            onChange={(autostart) => patchServer({ autostart })}
          />
          <Toggle
            label={t("settings.bypass_proxy")}
            hint={t("settings.bypass_proxy_hint")}
            checked={config.server.bypass_proxy}
            onChange={(bypass_proxy) => patchServer({ bypass_proxy })}
          />
        </div>

        <div className="row gap wrap">
          <span className="field-label">{t("settings.local_token")}</span>
          <code className="mono">{revealed ?? server.token_hint}</code>
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
                  setNotice(errorText(e));
                }
              }}
              title={t("settings.reveal_title")}
            >
              {t("settings.reveal")}
            </Button>
          )}
          <Button kind="ghost" onClick={() => api.copyToken()}>
            {t("settings.copy")}
          </Button>
          <Button
            kind="danger"
            onClick={() => {
              setRevealed(null);
              void run(api.regenerateToken);
            }}
            title={t("settings.regenerate_title")}
          >
            {t("settings.regenerate")}
          </Button>
        </div>
        <p className="field-hint">{t("settings.token_hint")}</p>
      </Section>

      <Section title={t("settings.point_client")}>
        <p className="field-hint">{t("settings.anthropic_clients")}</p>
        <pre className="snippet">
          {`export ANTHROPIC_BASE_URL=${baseUrl}
export ANTHROPIC_AUTH_TOKEN=<paste the token>
export ANTHROPIC_MODEL=sonnet-class`}
        </pre>
        <p className="field-hint">{t("settings.openai_clients")}</p>
        <pre className="snippet">
          {`export OPENAI_BASE_URL=${baseUrl}/v1
export OPENAI_API_KEY=<paste the token>
${t("settings.snippet_comment")}`}
        </pre>
      </Section>

      <Section title={t("settings.cors")} hint={t("settings.cors_hint")}>
        <Toggle
          label={t("settings.allow_origins")}
          checked={config.server.allow_cors}
          onChange={(allow_cors) => patchServer({ allow_cors })}
        />
        {config.server.allow_cors && (
          <>
            {config.server.cors_origins.length === 0 ? (
              <p className="field-hint">{t("settings.no_origins")}</p>
            ) : (
              <div className="chips">
                {config.server.cors_origins.map((origin) => (
                  <span key={origin} className="chip chip-done">
                    {origin}
                    <button
                      className="chip-remove"
                      aria-label={`Remove ${origin}`}
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
            <div className="controls">
              <Field label={t("field.allowed_origin")} hint={t("field.allowed_origin_hint")}>
                <input
                  value={originDraft}
                  placeholder="http://localhost:3000"
                  onChange={(e) => setOriginDraft(e.currentTarget.value)}
                  onKeyDown={(e) => e.key === "Enter" && addOrigin()}
                />
              </Field>
              <div className="field-actions">
                <Button onClick={addOrigin} disabled={busy || !originDraft.trim()}>
                  {t("settings.add_origin")}
                </Button>
              </div>
            </div>
          </>
        )}
      </Section>

      <Section title={t("settings.budgets")} hint={t("settings.budgets_hint")}>
        {snapshot.budgets.length > 0 && (
          <table className="table">
            <thead>
              <tr>
                <th>{t("budget.covers")}</th>
                <th>{t("budget.window")}</th>
                <th>{t("budget.limit")}</th>
                <th>{t("budget.spent")}</th>
                <th>{t("budget.when")}</th>
                <th>{t("budget.on")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {snapshot.budgets.map((status) => {
                const b = status.budget;
                const over = status.used >= 1;
                const scope = budgetScopeText(t, b.scope);
                return (
                  <tr key={b.id} className={over ? "row-warn" : ""}>
                    <td>{scope}</td>
                    <td>{t(b.period === "day" ? "period.day" : "period.month")}</td>
                    <td className="mono">{money(b.limit.currency, b.limit.amount)}</td>
                    <td>
                      <span className="mono">{costText(status.spent)}</span>{" "}
                      {over ? (
                        <Badge tone="danger">{t("budget.used_up")}</Badge>
                      ) : (
                        <span className="muted">{Math.round(status.used * 100)}%</span>
                      )}
                    </td>
                    <td>
                      <Select<ModelClass | "reject">
                        ariaLabel={t("budget.when")}
                        value={b.on_exceeded.action === "degrade" ? b.on_exceeded.to : "reject"}
                        onChange={(next) =>
                          patchBudget(b.id, {
                            on_exceeded:
                              next === "reject"
                                ? { action: "reject" }
                                : { action: "degrade", to: next },
                          })
                        }
                        options={[
                          { value: "reject", label: t("budget.reject") },
                          ...CLASSES.map((c) => ({
                            value: c as ModelClass,
                            label: t("budget.degrade", { id: virtualId(c) }),
                          })),
                        ]}
                      />
                    </td>
                    <td>
                      <input
                        type="checkbox"
                        aria-label={t("budget.on")}
                        checked={b.enabled}
                        onChange={(e) => patchBudget(b.id, { enabled: e.currentTarget.checked })}
                      />
                    </td>
                    <td>
                      <Button kind="ghost" onClick={() => removeBudget(b.id)}>
                        {t("common.delete")}
                      </Button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        <div className="controls">
          <Field label={t("budget.covers")}>
            <Select
              ariaLabel={t("budget.covers")}
              value={budgetDraft.scope}
              onChange={(scope) => setBudgetDraft({ ...budgetDraft, scope })}
              options={[
                { value: "global", label: t("scope.global") },
                ...CLASSES.map((c) => ({ value: `class:${c}`, label: virtualId(c) })),
                ...config.providers.map((p) => ({
                  value: `provider:${p.id}`,
                  label: t("scope.provider", { id: p.name }),
                })),
              ]}
            />
          </Field>
          <Field label={t("budget.window")}>
            <Segment<BudgetPeriod>
              ariaLabel={t("budget.window")}
              value={budgetDraft.period}
              onChange={(period) => setBudgetDraft({ ...budgetDraft, period })}
              options={[
                { value: "day", label: t("period.day") },
                { value: "month", label: t("period.month") },
              ]}
            />
          </Field>
          <NumberField
            label={t("budget.limit")}
            hint={t("budget.currency_hint")}
            min={0}
            value={budgetDraft.amount}
            onCommit={(amount) => setBudgetDraft({ ...budgetDraft, amount: amount ?? 0 })}
          />
          <Field label={t("field.currency")}>
            <input
              value={budgetDraft.currency}
              onChange={(e) =>
                setBudgetDraft({ ...budgetDraft, currency: e.currentTarget.value.toUpperCase() })
              }
            />
          </Field>
          <div className="field-actions">
            <Button onClick={addBudget} disabled={busy || budgetDraft.amount <= 0}>
              {t("budget.add")}
            </Button>
          </div>
        </div>
      </Section>

      <Section title={t("settings.advanced")} hint={t("settings.advanced_hint")}>
        <h3 style={{ margin: "6px 0 0", fontSize: 11, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.05em" }}>
          {t("settings.aliases")}
        </h3>
        <p className="field-hint">{t("settings.aliases_hint")}</p>
        {Object.entries(config.routing.client_aliases).length > 0 && (
          <table className="table">
            <tbody>
              {Object.entries(config.routing.client_aliases).map(([from, to]) => (
                <tr key={from}>
                  <td>
                    <code>{from}</code>
                  </td>
                  <td>{virtualId(to)}</td>
                  <td>
                    <Button kind="ghost" onClick={() => removeAlias(from)}>
                      {t("common.remove")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <div className="controls">
          <Field label={t("settings.f_model_id")}>
            <input
              value={aliasDraft.from}
              placeholder="claude-opus-4-1-20250805"
              onChange={(e) => setAliasDraft({ ...aliasDraft, from: e.currentTarget.value })}
              onKeyDown={(e) => e.key === "Enter" && addAlias()}
            />
          </Field>
          <Field label={t("settings.f_class")}>
            <Select<ModelClass>
              ariaLabel={t("settings.f_class")}
              value={aliasDraft.to}
              onChange={(to) => setAliasDraft({ ...aliasDraft, to })}
              options={CLASSES.map((c) => ({ value: c, label: virtualId(c) }))}
            />
          </Field>
          <div className="field-actions">
            <Button onClick={addAlias} disabled={busy || !aliasDraft.from.trim()}>
              {t("settings.add_alias")}
            </Button>
          </div>
        </div>

        <h3 style={{ margin: "10px 0 0", fontSize: 11, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.05em" }}>
          {t("settings.election")}
        </h3>
        <p className="field-hint">{t("settings.election_hint")}</p>
        <div className="controls">
          <NumberField
            label={t("settings.price_weight")}
            hint={t("settings.price_weight_hint")}
            min={0}
            value={config.routing.scoring.price_weight}
            onCommit={(v) => patchScoring({ price_weight: v ?? 0 })}
          />
          <NumberField
            label={t("settings.latency_weight")}
            min={0}
            value={config.routing.scoring.latency_weight}
            onCommit={(v) => patchScoring({ latency_weight: v ?? 0 })}
          />
          <NumberField
            label={t("settings.ref_input")}
            hint={t("settings.ref_input_hint")}
            min={0}
            integer
            value={config.routing.scoring.reference_input_tokens}
            onCommit={(v) => patchScoring({ reference_input_tokens: v ?? 0 })}
          />
          <NumberField
            label={t("settings.ref_output")}
            min={0}
            integer
            value={config.routing.scoring.reference_output_tokens}
            onCommit={(v) => patchScoring({ reference_output_tokens: v ?? 0 })}
          />
        </div>

        <h3 style={{ margin: "10px 0 0", fontSize: 11, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.05em" }}>
          {t("settings.log")}
        </h3>
        {logs === null ? (
          <Button kind="ghost" onClick={loadLogs}>
            {t("settings.show_log")}
          </Button>
        ) : (
          <>
            <pre className="log-view">{logs.join("\n")}</pre>
            <Button kind="ghost" onClick={() => setLogs(null)}>
              {t("settings.stop_follow")}
            </Button>
          </>
        )}
      </Section>
    </>
  );
}

/** A budget's scope as a localized phrase. */
function budgetScopeText(
  t: ReturnType<typeof useI18n>["t"],
  scope: BudgetScope,
): string {
  switch (scope.kind) {
    case "global":
      return t("scope.global");
    case "provider":
      return t("scope.provider", { id: scope.id });
    case "class":
      return virtualId(scope.class);
  }
}
