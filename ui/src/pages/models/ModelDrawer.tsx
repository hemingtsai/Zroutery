/**
 * The model detail drawer: summary, tier assignment, performance,
 * configuration (rename, aliases, capabilities, effort levels), price.
 *
 * All writes arrive as `onUpdate(mutate)` — the mutation receives the model
 * from the config being committed, never a captured object.
 */
import { useState, type ReactNode } from "react";
import {
  TIERS,
  contextText,
  modelName,
  ms,
  virtualId,
  type ModelTier,
  type ModelEntry,
  type ModelRow,
  type NamingStyle,
  type Provider,
  type Snapshot,
} from "../../api";
import { useI18n } from "../../i18n";
import { SaveFeedbackReset } from "../../saveQueue";
import {
  Badge,
  Button,
  Drawer,
  KeyValue,
  NumberField,
  Section,
  Select,
  StatusDot,
  TextField,
  Toggle,
} from "../../components";
import { PriceFields } from "./PriceFields";

/**
 * Reasoning-effort levels the gateway accepts, in picker order.
 *
 * Must match `zroutery_core::config::EFFORT_LEVELS`: the takeover writes these
 * as client variants, and a value outside this list is not encodable to any
 * dialect.
 */
export const EFFORT_LEVELS = ["none", "minimal", "low", "medium", "high", "xhigh", "max"] as const;

/**
 * One model's whole story. "Used for" answers where it sits in routing;
 * performance is what the proxy has observed, not a promise.
 */
export function ModelDrawer({
  row,
  provider,
  health,
  busy,
  namingStyle,
  onClose,
  onUpdate,
  onRename,
  onRemove,
}: {
  row: ModelRow;
  provider: Provider | undefined;
  health: Snapshot["health"][number] | undefined;
  busy: boolean;
  namingStyle: NamingStyle;
  onClose: () => void;
  /** Receives the model being committed, so a nested object is merged onto
   *  the latest one rather than a copy captured at render time. */
  onUpdate: (mutate: (model: ModelEntry) => void) => void;
  /** Renames the model; resolves to a reason it could not be done, or null. */
  onRename: (next: string) => Promise<string | null>;
  onRemove: () => void;
}) {
  const m = row.model;
  const { t } = useI18n();
  const [renameError, setRenameError] = useState<string | null>(null);
  const status = !m.enabled || provider?.enabled === false ? "off" : "ok";
  const dialect = provider?.kind === "anthropic" ? "anthropic"
    : provider?.api_surface === "responses" ? "open_a_i_responses" : "open_a_i";
  const evidence = m.reasoning_capability;
  const currentEvidence = evidence?.schema_version === 1
    && evidence.provider_id === provider?.id && evidence.upstream_model === m.upstream_model
    && evidence.dialect === dialect ? evidence : null;
  const displayedEfforts = currentEvidence?.effort_levels ?? m.effort_levels ?? [];
  // A decision model answers the Auto Mode's questions and cannot answer a
  // request that wants prose, so no tier exists for it to hold. The drawer
  // used to show the tier select disabled with a "—" in it — a control that
  // refused every choice it offered — and explained itself in a row below the
  // aliases, two screens away from the control it explained. The explanation
  // takes the tier row's own place instead, and the select is gone entirely:
  // a row that says why there is no choice does not also draw one.
  const tierLocked = provider?.kind === "decision";

  // The rows are built before the markup so that the tier row and its
  // explanation are alternatives, not one row and a patch on it: a conditional
  // inside the array literal is a tuple the compiler cannot type.
  const rows: [ReactNode, ReactNode][] = [
    [t("models.provider"), provider?.name ?? t("models.missing_provider")],
    [t("models.upstream"), <span className="mono">{m.upstream_model}</span>],
  ];
  if (tierLocked) {
    rows.push([
      t("models.tier_decision"),
      <span className="muted">{t("models.tier_decision_hint")}</span>,
    ]);
  } else {
    rows.push([
      t("models.tier"),
      <Select<ModelTier | "none">
        ariaLabel={t("models.tier_for", { model: m.upstream_model })}
        value={m.tier ?? "none"}
        disabled={busy}
        onChange={(next) =>
          onUpdate((model) => {
            model.tier = next === "none" ? null : next;
          })
        }
        options={[
          { value: "none", label: "—" },
          ...TIERS.map((c) => ({ value: c, label: virtualId(c, namingStyle) })),
        ]}
      />,
    ]);
  }
  rows.push(
    [
      t("common.enabled"),
      <Toggle
        label=""
        // The visible label is empty because the row beside it already
        // says "Enabled" — which left the switch itself with no name at
        // all, announcing as "checkbox, unchecked" and nothing more.
        ariaLabel={t("common.enabled")}
        checked={m.enabled}
        onChange={(enabled) =>
          onUpdate((model) => {
            model.enabled = enabled;
          })
        }
      />,
    ],
    [
      t("models.aliases"),
      m.aliases.length ? <span className="mono">{m.aliases.join(", ")}</span> : t("common.dash"),
    ],
  );

  return (
    <Drawer title={<><StatusDot tone={status} /> <span>{modelName(m)}</span></>} onClose={onClose}>
      {/* A drawer opened for one model starts with no verdict inherited from
          the surface before it. */}
      <SaveFeedbackReset />
      <KeyValue rows={rows} />

      <Section title={t("models.performance")}>
        {health ? (
          <KeyValue
            rows={[
              [t("models.avg_latency"), <span className="mono">{ms(health.avg_latency_ms)}</span>],
              [t("models.requests"), <span className="mono">{health.total_success + health.total_failure}</span>],
              [
                t("models.failures"),
                health.total_failure > 0 ? (
                  <Badge tone="danger">{health.total_failure}</Badge>
                ) : (
                  <span className="mono">0</span>
                ),
              ],
              [t("models.last_error"), health.last_error ?? t("common.dash")],
            ]}
          />
        ) : (
          <p className="empty">{t("models.no_traffic")}</p>
        )}
      </Section>

      <Section title={t("models.configuration")}>
        <div className="controls">
          <TextField
            label={t("models.upstream")}
            ariaLabel={t("models.upstream")}
            hint={t("models.f_model_name_hint")}
            value={m.upstream_model}
            onCommit={(v) => {
              void onRename(v).then((error) => setRenameError(error));
            }}
          />
          {renameError && <p className="field-hint field-hint-danger">{renameError}</p>}
          <TextField
            label={t("models.f_display")}
            hint={t("models.f_display_hint")}
            value={m.display_name ?? ""}
            placeholder={m.upstream_model}
            onCommit={(v) =>
              onUpdate((model) => {
                model.display_name = v || null;
              })
            }
          />
          <NumberField
            label={t("models.f_priority")}
            hint={t("models.f_priority_hint")}
            min={0}
            integer
            value={m.priority}
            onCommit={(priority) =>
              onUpdate((model) => {
                model.priority = priority ?? 0;
              })
            }
          />
          <NumberField
            label={t("models.f_weight")}
            hint={t("models.f_weight_hint")}
            min={1}
            integer
            value={m.weight}
            onCommit={(weight) =>
              onUpdate((model) => {
                model.weight = weight ?? 1;
              })
            }
          />
          <NumberField
            label={t("models.f_max_tokens")}
            hint={t("models.f_max_tokens_hint")}
            min={1}
            placeholder={t("models.price_unlimited")}
            integer
            value={m.max_output_tokens}
            onCommit={(max_output_tokens) =>
              onUpdate((model) => {
                model.max_output_tokens = max_output_tokens;
              })
            }
          />
          <NumberField
            label={t("models.f_context_window")}
            hint={
              m.context_window !== null
                ? `${contextText(m.context_window)} · ${t("models.f_context_window_hint")}`
                : t("models.f_context_window_hint")
            }
            min={1}
            placeholder={t("models.context_unknown")}
            integer
            value={m.context_window}
            onCommit={(context_window) =>
              onUpdate((model) => {
                model.context_window = context_window;
              })
            }
          />
          <div className="field-actions">
            <Button
              kind="ghost"
              onClick={() =>
                onUpdate((model) => {
                  model.context_window = 1_000_000;
                })
              }
            >
              {t("models.context_set_1m")}
            </Button>
          </div>
          <TextField
            label={t("models.aliases")}
            hint={t("models.f_aliases_hint")}
            value={m.aliases.join(", ")}
            onCommit={(v) =>
              onUpdate((model) => {
                model.aliases = v
                  .split(",")
                  .map((a) => a.trim())
                  .filter(Boolean);
              })
            }
            wide
          />
        </div>
        <div className="grid-two">
          <Toggle
            label={t("models.tool_use")}
            ariaLabel={t("models.tool_use")}
            checked={m.capabilities.tools}
            onChange={(v) =>
              onUpdate((model) => {
                model.capabilities.tools = v;
              })
            }
          />
          <Toggle
            label={t("models.vision")}
            ariaLabel={t("models.vision")}
            checked={m.capabilities.vision}
            onChange={(v) =>
              onUpdate((model) => {
                model.capabilities.vision = v;
              })
            }
          />
          <Toggle
            label={t("models.thinking")}
            ariaLabel={t("models.thinking")}
            checked={m.capabilities.thinking}
            onChange={(v) =>
              onUpdate((model) => {
                model.capabilities.thinking = v;
              })
            }
          />
        </div>
        <div className="field-label">{t("models.effort_levels")}</div>
        <div className="field-actions">
          {EFFORT_LEVELS.map((level) => {
            const declared = displayedEfforts.includes(level);
            return (
              <Button
                key={level}
                kind={declared ? "primary" : "ghost"}
                aria-pressed={declared}
                disabled={busy}
                onClick={() =>
                  onUpdate((model) => {
                    const current = currentEvidence?.effort_levels ?? model.effort_levels ?? [];
                    model.effort_levels = declared
                      ? current.filter((value) => value !== level)
                      : EFFORT_LEVELS.filter(
                          (candidate) =>
                            candidate === level || current.includes(candidate),
                         );
                    if (provider) {
                      model.reasoning_capability = {
                        schema_version: 1,
                        source: "manual model settings",
                        observed_at: Math.floor(Date.now() / 1000),
                        provider_id: provider.id,
                        upstream_model: model.upstream_model,
                        dialect,
                        effort_levels: [...model.effort_levels],
                        token_budget: currentEvidence?.token_budget ?? false,
                        effort_budgets: currentEvidence?.effort_budgets ?? {},
                        manual: true,
                      };
                    }
                  })
                }
              >
                {level}
              </Button>
            );
          })}
        </div>
        <p className="field-hint">{t("models.effort_levels_hint")}</p>
      </Section>

      <Section title={t("models.price")} hint={t("models.price_hint")}>
        <PriceFields
          pricing={m.pricing}
          onChange={(mutate) => onUpdate(mutate)}
        />
      </Section>

      <div>
        <Button kind="danger" disabled={busy} onClick={onRemove}>
          {t("models.remove")}
        </Button>
      </div>
    </Drawer>
  );
}
