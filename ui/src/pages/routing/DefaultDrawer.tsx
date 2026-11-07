/**
 * The default-routing drawer: strategy, failover, election weights, tier
 * naming and client aliases; plus the rectifier table the routing page's
 * Auto review section renders.
 */
import { useState } from "react";
import { TIERS, type AppConfig, type Election, type ModelTier, type NamingStyle, type RectifierConfig, type RoutingStrategy, type Snapshot } from "../../api";
import { ms, virtualId } from "../../api";
import { useI18n, type TKey } from "../../i18n";
import { SaveFeedbackReset } from "../../saveQueue";
import { Badge, Button, CommitNumber, Drawer, Field, NumberField, Section, Segment, Select, Toggle } from "../../components";

export const DEFAULT_RECTIFIER: RectifierConfig = {
  enabled: true,
  thinking_signature: true,
  media_fallback: true,
  thinking_budget: true,
  thinking_fallback: true,
  codex_metadata: true,
  codex_auto: true,
};

/**
 * The rectifier switches, in the order they are tried.
 *
 * `keyof Omit<RectifierConfig, "enabled">` is the master switch, drawn on its
 * own above these, so the table cannot accidentally offer it twice.
 */
export const RECTIFIERS = [
  {
    field: "thinking_signature",
    label: "rectifier.thinking_signature",
    hint: "rectifier.thinking_signature_hint",
  },
  {
    field: "media_fallback",
    label: "rectifier.media_fallback",
    hint: "rectifier.media_fallback_hint",
  },
  {
    field: "thinking_budget",
    label: "rectifier.thinking_budget",
    hint: "rectifier.thinking_budget_hint",
  },
  {
    field: "codex_metadata",
    label: "rectifier.codex_metadata",
    hint: "rectifier.codex_metadata_hint",
  },
  {
    field: "codex_auto",
    label: "rectifier.codex_auto",
    hint: "rectifier.codex_auto_hint",
  },
  {
    field: "thinking_fallback",
    label: "rectifier.thinking_fallback",
    hint: "rectifier.thinking_fallback_hint",
  },
] as const satisfies readonly {
  field: Exclude<keyof RectifierConfig, "enabled">;
  label: TKey;
  hint: TKey;
}[];

export const TIER_HINT_KEY: Record<ModelTier, "tier.hint.fast" | "tier.hint.standard" | "tier.hint.reasoning" | "tier.hint.frontier"> = {
  fast: "tier.hint.fast",
  standard: "tier.hint.standard",
  reasoning: "tier.hint.reasoning",
  frontier: "tier.hint.frontier",
};

/** The strategy select's own labels, reused when a hint must name a strategy. */
export const STRATEGY_LABEL: Record<RoutingStrategy, "strategy.priority" | "strategy.weighted_random" | "strategy.round_robin" | "strategy.lowest_latency" | "strategy.balanced"> = {
  priority: "strategy.priority",
  weighted_random: "strategy.weighted_random",
  round_robin: "strategy.round_robin",
  lowest_latency: "strategy.lowest_latency",
  balanced: "strategy.balanced",
};
export function DefaultDrawer({
  snapshot,
  busy,
  onClose,
  onSave,
  onElection,
}: {
  snapshot: Snapshot;
  busy: boolean;
  onClose: () => void;
  onSave: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  onElection: () => Promise<boolean>;
}) {
  const routing = snapshot.config.routing;
  const { t } = useI18n();
  const [aliasDraft, setAliasDraft] = useState<{ from: string; to: ModelTier }>({
    from: "",
    to: "standard",
  });

  const patch = (mutate: Partial<AppConfig["routing"]>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.routing, mutate);
      return next;
    });
  };

  /**
   * Scoring is a nested object, so it cannot go through `patch`, which assigns
   * onto `routing` itself. Kept as its own writer rather than a special case
   * inside `patch` so that "a field on routing" and "a field inside routing's
   * scoring" stay distinguishable at the call site.
   */
  const patchScoring = (patch: Partial<AppConfig["routing"]["scoring"]>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.routing.scoring, patch);
      return next;
    });
  };

  const addAlias = () => {
    const from = aliasDraft.from.trim();
    if (!from) return;
    patch({ client_aliases: { ...routing.client_aliases, [from]: aliasDraft.to } });
    setAliasDraft({ from: "", to: aliasDraft.to });
  };

  const removeAlias = (from: string) => {
    const next = { ...routing.client_aliases };
    delete next[from];
    patch({ client_aliases: next });
  };

  return (
    <Drawer title={t("routing.default_drawer")} onClose={onClose}>
      <SaveFeedbackReset />
      <div className="controls">
        <Field label={t("field.strategy")}>
          <Select<RoutingStrategy>
            ariaLabel={t("field.strategy")}
            value={routing.strategy}
            disabled={busy}
            onChange={(strategy) => patch({ strategy })}
            options={[
              { value: "priority", label: t("strategy.priority") },
              { value: "weighted_random", label: t("strategy.weighted_random") },
              { value: "round_robin", label: t("strategy.round_robin") },
              { value: "lowest_latency", label: t("strategy.lowest_latency") },
              { value: "balanced", label: t("strategy.balanced") },
            ]}
          />
        </Field>
        <NumberField
          label={t("field.max_attempts")}
          hint={t("field.max_attempts_hint")}
          min={1}
          max={10}
          integer
          value={routing.max_attempts}
          onCommit={(max_attempts) => patch({ max_attempts: max_attempts ?? 3 })}
        />
        <Field label={t("field.unknown_ids")}>
          <Select<ModelTier | "unset">
            ariaLabel={t("field.unknown_ids")}
            value={routing.unknown_model_fallback ?? "unset"}
            onChange={(next) =>
              patch({ unknown_model_fallback: next === "unset" ? null : next })
            }
            options={[
              { value: "unset", label: t("routing.unknown_404") },
              ...TIERS.map((c) => ({
                value: c as ModelTier,
                label: t("routing.unknown_serve", { id: virtualId(c, snapshot.config.routing.naming_style) }),
              })),
            ]}
          />
        </Field>
      </div>
      <div className="grid-two">
        <Toggle
          label={t("routing.failover")}
          hint={t("routing.failover_hint")}
          checked={routing.failover}
          onChange={(failover) => patch({ failover })}
        />
        <Toggle
          label={t("routing.claude_names")}
          hint={t("routing.claude_names_hint")}
          checked={routing.match_claude_names}
          onChange={(match_claude_names) => patch({ match_claude_names })}
        />
      </div>
      <p className="field-hint">{t("routing.aliases_note")}</p>

      {routing.strategy === "balanced" && (
        <Section
          title={t("routing.election")}
          hint={t("routing.election_hint")}
          actions={
            <Button kind="ghost" onClick={onElection} disabled={busy}>
              {t("routing.rerun")}
            </Button>
          }
        >
          <Toggle
            label={t("routing.elect_on_start")}
            checked={routing.elect_on_start}
            onChange={(elect_on_start) => patch({ elect_on_start })}
          />
          {/*
            The four weights that decide the election, here rather than on the
            settings page. They were the reason a hint had to name a path in
            another page — "only meaningful for the balanced strategy (Routing →
            default → edit)" — because the strategy that makes them meaningful
            sits above them in this drawer and the numbers did not. The heading
            repeats itself because a reader who arrives at the weights by
            scrolling has not necessarily registered that the whole section only
            exists for one strategy.
          */}
          <h3 className="section-sub">{t("settings.election_weights")}</h3>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.price_weight")}</span>
              <span className="setting-desc">{t("settings.price_weight_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={0}
                value={routing.scoring.price_weight}
                ariaLabel={t("settings.price_weight")}
                onCommit={(v) => patchScoring({ price_weight: v ?? 0 })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.latency_weight")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={0}
                value={routing.scoring.latency_weight}
                ariaLabel={t("settings.latency_weight")}
                onCommit={(v) => patchScoring({ latency_weight: v ?? 0 })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.ref_input")}</span>
              <span className="setting-desc">{t("settings.ref_input_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={0}
                integer
                value={routing.scoring.reference_input_tokens}
                ariaLabel={t("settings.ref_input")}
                onCommit={(v) => patchScoring({ reference_input_tokens: v ?? 0 })}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("settings.ref_output")}</span>
            </div>
            <div className="setting-right">
              <CommitNumber
                min={0}
                integer
                value={routing.scoring.reference_output_tokens}
                ariaLabel={t("settings.ref_output")}
                onCommit={(v) => patchScoring({ reference_output_tokens: v ?? 0 })}
              />
            </div>
          </div>
          <ElectionResult election={snapshot.election} namingStyle={snapshot.config.routing.naming_style} />
        </Section>
      )}

      {/*
        Tier naming and the client aliases, together and here, because both
        decide what id a client may ask for. They override `match_claude_names`
        above — the aliases hint says so, and it can now point at the control it
        means instead of describing a rule three fields away.
      */}
      <Section title={t("settings.tier_naming_section")}>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.naming_style")}</span>
            <span className="setting-desc">{t("settings.naming_style_hint")}</span>
          </div>
          <div className="setting-right">
            <Segment<NamingStyle>
              ariaLabel={t("settings.naming_style")}
              className="segment-compact"
              value={routing.naming_style}
              onChange={(naming_style) => patch({ naming_style })}
              options={[
                { value: "internal", label: t("settings.naming_internal") },
                { value: "anthropic", label: t("settings.naming_anthropic") },
                { value: "openai", label: t("settings.naming_openai") },
              ]}
            />
          </div>
        </div>
      </Section>

      <Section title={t("settings.aliases")} hint={t("settings.aliases_hint")}>
        {Object.entries(routing.client_aliases).length > 0 && (
          <table className="table">
            <tbody>
              {Object.entries(routing.client_aliases).map(([from, to]) => (
                <tr key={from}>
                  <td>
                    <code>{from}</code>
                  </td>
                  <td>{virtualId(to, routing.naming_style)}</td>
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
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.f_model_id")}</span>
          </div>
          <div className="setting-right">
            <input
              aria-label={t("settings.f_model_id")}
              value={aliasDraft.from}
              placeholder="claude-opus-4-1-20250805"
              onChange={(e) => setAliasDraft({ ...aliasDraft, from: e.currentTarget.value })}
              onKeyDown={(e) => e.key === "Enter" && addAlias()}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.f_class")}</span>
          </div>
          <div className="setting-right">
            <Select<ModelTier>
              ariaLabel={t("settings.f_class")}
              value={aliasDraft.to}
              onChange={(to) => setAliasDraft({ ...aliasDraft, to })}
              options={TIERS.map((c) => ({ value: c, label: virtualId(c, routing.naming_style) }))}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left" />
          <div className="setting-right">
            <Button onClick={addAlias} disabled={busy || !aliasDraft.from.trim()}>
              {t("settings.add_alias")}
            </Button>
          </div>
        </div>
      </Section>
    </Drawer>
  );
}

/** What the last election decided, per tier, with the numbers behind it. */
export function ElectionResult({ election, namingStyle }: { election: Election | null; namingStyle: NamingStyle }) {
  const { t } = useI18n();
  if (!election) {
    return <p className="empty">{t("routing.no_election")}</p>;
  }
  const tierOutcomes = TIERS.map((tier) => election.tiers[tier]).filter(
    (c): c is NonNullable<typeof c> => c !== undefined,
  );
  if (tierOutcomes.length === 0) return <p className="empty">{t("routing.no_election_tiers")}</p>;
  return (
    <>
      {tierOutcomes.map((outcome) => (
        <div key={outcome.tier}>
          <div className="row gap" style={{ marginBottom: 4 }}>
            <span className="mono">{virtualId(outcome.tier, namingStyle)}</span>
            {outcome.note && <span className="muted">{outcome.note}</span>}
          </div>
          <table className="table">
            <tbody>
              {outcome.ranked.map((r, place) => (
                <tr key={r.model_id} className={r.score === null ? "row-warn" : ""}>
                  <td>
                    {place === 0 && r.score !== null ? <Badge tone="ok">{t("routing.primary")}</Badge> : ""}{" "}
                    <span className="mono">{r.model_id}</span>
                  </td>
                  <td className="muted mono">{ms(r.latency_ms)}</td>
                  <td className="muted">{r.note ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ))}
    </>
  );
}

/**
 * Auto Mode as a product surface: no "classifier" jargon, no candidate
 * plumbing visible — a switch, a pool, and where it fails over.
 */
