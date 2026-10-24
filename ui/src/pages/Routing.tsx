import { useMemo, useState, type ReactNode } from "react";
import {
  TIERS,
  ZEN_JEV_MODEL,
  applyZenJevPreset,
  isZenJevProvider,
  tierMembers,
  modelRows,
  moveCandidatePriority,
  nextCandidatePriority,
  orderCandidatesByPriority,
  previewId,
  virtualId,
  type AppConfig,
  type ClassifierCandidate,
  type Election,
  type ModelTier,
  type ModelRow,
  type NamingStyle,
  type RoutingStrategy,
  type Snapshot,
} from "../api";
import { api } from "../api";
import MlRouting from "./MlRouting";
import {
  Badge,
  Button,
  CompactNumber,
  CommitInput,
  CommitNumber,
  Disclosure,
  Drawer,
  Field,
  KeyValue,
  NumberField,
  PageHead,
  Section,
  Segment,
  Tabs,
  Select,
  StatusDot,
  Toggle,
  ms,
  useToast,
} from "../components";
import { useI18n } from "../i18n";

/**
 * Route rows shown before the pool's long tail folds away.
 *
 * A tier holds every enabled model of that class, and a real configuration
 * holds dozens. The head of the list — who answers, who is next — is the part
 * read on every visit; the tail matters the day an outage walks the pool to
 * the bottom, and it is exactly as reachable one click down. Unfolded, the
 * page was forty rows of "备选 5" that pushed the next tier off the screen.
 */
const SHOWN_ROUTES = 4;

/**
 * Routing shows relationships first and configuration second. The page is
 * the flow — what a request of each purpose reaches — and every knob lives
 * behind Edit, because "how it is configured" is only interesting once
 * "where it goes" is already visible.
 *
 * Two purposes exist today: Default (every ordinary request) and Auto Mode
 * (permission side queries). Applications are a future dimension and are
 * deliberately absent until the backend can mean them.
 */
export default function Routing({
  snapshot,
  save,
  run,
  busy,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
}) {
  const { config, health } = snapshot;
  const { t } = useI18n();
  const [edit, setEdit] = useState<"default" | "auto" | null>(null);

  /*
   * The route picture, derived from the configuration rather than from the
   * render.
   *
   * The page's own state is which tab is open and which drawer is mounted,
   * and neither of those changes what a request reaches — but both change far
   * more often than the configuration does. Everything below is keyed on the
   * configuration, so switching tabs or opening a drawer costs one render of
   * the picture rather than another pass over the model list.
   */
  const rows = useMemo(
    () => modelRows(snapshot),
    [snapshot.config.models, snapshot.exposed_ids],
  );

  /**
   * The providers, by id.
   *
   * Every route in the flow asked the provider list for a name by scanning
   * it, and the vision fallback asked the same question once per model: one
   * render cost O(models x providers). One map answers both.
   */
  const providerById = useMemo(
    () => new Map(config.providers.map((p) => [p.id, p])),
    [config.providers],
  );

  const cooling = useMemo(
    () =>
      new Set(
        health.filter((h) => h.cooldown_remaining_secs > 0).map((h) => h.model_id),
      ),
    [health],
  );

  const tierRoutes = useMemo(
    () => TIERS.map((tier) => ({ tier, members: tierMembers(rows, config.providers, tier) })),
    [rows, config.providers],
  );

  const patchVision = (patch: Partial<AppConfig["vision"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.vision, patch);
      return next;
    });
  };

  /*
   * The models the vision fallback is allowed to choose from.
   *
   * Three conditions, not one: a model has to be enabled, its provider has to be
   * enabled, and it has to actually accept images. The third is the one that
   * matters here — offering a model that cannot see as the thing that describes
   * images would make the fallback substitute one failure for another.
   */
  const visionCapable = useMemo(
    () =>
      rows.filter(
        (r) =>
          r.model.enabled &&
          r.model.capabilities.vision &&
          providerById.get(r.model.provider_id)?.enabled,
      ),
    [rows, providerById],
  );

  // The pool in the order the router would try it. Every strategy pool is
  // ranked by priority, so the page follows the numbers rather than the array.
  const classifierCandidates = useMemo(() => {
    if (!config.classifier.enabled) return [];
    return orderCandidatesByPriority(
      config.classifier.candidates
        .filter((c) => c.enabled)
        .map((c) => ({
          priority: c.priority,
          model: c.model,
          row: rows.find((r) => r.id === c.model || r.model.aliases.includes(c.model)),
        }))
        .filter((c): c is typeof c & { row: ModelRow } => Boolean(c.row)),
    );
  }, [config.classifier.enabled, config.classifier.candidates, rows]);

  const [tab, setTab] = useState<"ml" | "main" | "auto">(snapshot.ml_available ? "ml" : "main");

  // The two route bodies, hoisted so the tab strip below can place them
  // without the two subjects and the tab shell duplicating any markup.
  const mainRoutes = (
    <Section
      title={t("route.default")}
      hint={t("routing.default_hint")}
      actions={
        <button type="button" className="linky" onClick={() => setEdit("default")}>
          {t("routing.edit")}
        </button>
      }
    >
      <div className="list">
        <div className="flow">
          {tierRoutes.map(({ tier, members }) => (
            <div className="flow-row" key={tier}>
              <div className="flow-kind">
                <span className="flow-kind-name mono">{virtualId(tier, config.routing.naming_style)}</span>
                <span className="flow-kind-hint">{t(TIER_HINT_KEY[tier])}</span>
              </div>
              <div className="flow-routes">
                {members.length === 0 ? (
                  <span className="flow-empty">
                    {t("routing.tier_empty", { id: virtualId(tier, config.routing.naming_style) })}
                  </span>
                ) : (
                  <RoutePool
                    members={members}
                    firstLabel={t("routing.request")}
                    cooling={cooling}
                    providerName={(id: string) => providerById.get(id)?.name}
                  />
                )}
              </div>
            </div>
          ))}
        </div>
      </div>
    </Section>
  );

  /*
   * Vision fallback, here rather than on the settings page.
   *
   * It is not an appearance or a machine preference: when the chosen model
   * cannot see, a different model is asked what the image contains and its
   * answer is sent in place of the image. That is a decision about what a
   * request contains, made before the request is routed, and it belongs on the
   * page whose subject is what happens to a request.
   */
  const visionFallback = (
    <Section title={t("vision.title")} hint={t("vision.hint")}>
      <div className="setting-row">
        <div className="setting-left">
          <span className="setting-title">{t("vision.enable")}</span>
        </div>
        <div className="setting-right">
          <input
            type="checkbox"
            aria-label={t("vision.enable")}
            checked={config.vision.enabled}
            onChange={(e) => patchVision({ enabled: e.currentTarget.checked })}
          />
        </div>
      </div>
      {config.vision.enabled && (
        <>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("vision.model")}</span>
              <span className="setting-desc">{t("vision.model_hint")}</span>
            </div>
            <div className="setting-right">
              <Select
                ariaLabel={t("vision.model")}
                value={config.vision.model}
                onChange={(model) => patchVision({ model: model || null })}
                placeholder="—"
                options={visionCapable.map((r) => ({ value: r.id, label: r.id }))}
              />
            </div>
          </div>
          <div className="setting-row">
            <div className="setting-left">
              <span className="setting-title">{t("vision.placeholder")}</span>
              <span className="setting-desc">{t("vision.placeholder_hint")}</span>
            </div>
            <div className="setting-right">
              <CommitInput
                value={config.vision.placeholder}
                ariaLabel={t("vision.placeholder")}
                onCommit={(placeholder) =>
                  placeholder.trim() && patchVision({ placeholder })
                }
              />
            </div>
          </div>
          {visionCapable.length === 0 && (
            <p className="field-hint">{t("vision.model_required")}</p>
          )}
        </>
      )}
    </Section>
  );

  const autoReview = (
    <Section
      title={t("route.auto_review")}
      hint={config.classifier.enabled
        ? t("routing.review_hint_on")
        : t("routing.review_hint_off")}
      actions={
        <button type="button" className="linky" onClick={() => setEdit("auto")}>
          {t("routing.edit")}
        </button>
      }
    >
      <div className="list">
        <div className="flow">
          <div className="flow-row">
            <div className="flow-kind">
              <span className="flow-kind-name">{t("routing.side_query")}</span>
              <span className="flow-kind-hint">
                {config.classifier.enabled
                  ? t("route.review_hint_on")
                  : t("route.review_hint_off")}
              </span>
            </div>
            <div className="flow-routes">
              {config.classifier.enabled ? (
                classifierCandidates.length > 0 ? (
                  <RoutePool
                    members={classifierCandidates.map((c) => c.row)}
                    firstLabel={
                      config.classifier.strategy === "priority"
                        ? t("routing.verdict")
                        : t("routing.next")
                    }
                    cooling={cooling}
                    providerName={(id) => providerById.get(id)?.name}
                  />
                ) : (
                  <span className="flow-empty">{t("routing.review_no_candidate")}</span>
                )
              ) : (
                <span className="flow-empty">{t("routing.review_off")}</span>
              )}
            </div>
          </div>
        </div>
      </div>
    </Section>
  );

return (
    <>
      <PageHead lede={t("routing.lede")} />

      {/*
        Three subjects, one screen. The learned model, the ordinary tier routes
        and the classifier pool each answer a different question, and stacked
        they made a reader scroll past two of them to reach the third. As tabs
        each is one click from any other and the page stops growing downward.
        A build without the learning stack has no first tab, so it opens on the
        ordinary routes instead.
      */}
      <Tabs
        value={tab}
        onChange={(next) => setTab(next as "ml" | "main" | "auto")}
        ariaLabel={t("routing.tab_group")}
        tabs={[
          ...(snapshot.ml_available
            ? [
                {
                  value: "ml",
                  label: t("routing.tab_ml"),
                  content: <MlRouting available={snapshot.ml_available} snapshot={snapshot} save={save} />,
                },
              ]
            : []),
          {
            value: "main",
            label: t("routing.tab_main"),
            content: (
              <>
                {mainRoutes}
                {visionFallback}
              </>
            ),
          },
          { value: "auto", label: t("routing.tab_auto"), content: autoReview },
        ]}
      />

      {edit === "default" && (
        <DefaultDrawer
          snapshot={snapshot}
          busy={busy}
          onClose={() => setEdit(null)}
          onSave={(mutate) =>
            save((cfg) => {
              const next = mutate(cfg);
              return next;
            })
          }
          onElection={() => run(api.runElection)}
        />
      )}
      {edit === "auto" && (
        <AutoModeDrawer
          snapshot={snapshot}
          busy={busy}
          onClose={() => setEdit(null)}
          onSave={(mutate) => save(mutate)}
        />
      )}
    </>
  );
}

const TIER_HINT_KEY: Record<ModelTier, "tier.hint.fast" | "tier.hint.standard" | "tier.hint.reasoning" | "tier.hint.frontier"> = {
  fast: "tier.hint.fast",
  standard: "tier.hint.standard",
  reasoning: "tier.hint.reasoning",
  frontier: "tier.hint.frontier",
};

/**
 * One pool of route lines: the head shown flat, the long tail folded.
 *
 * The first line is labelled by the pool's own word — "请求" for a tier,
 * "判定" for the review pool — and the folded tail keeps its absolute
 * fallback numbers, so a row inside the disclosure still says which place
 * in the pool it holds rather than counting from where the fold began.
 */
function RoutePool({
  members,
  firstLabel,
  cooling,
  providerName,
}: {
  members: ModelRow[];
  firstLabel: ReactNode;
  cooling: Set<string>;
  providerName: (providerId: string) => string | undefined;
}) {
  const { t } = useI18n();
  const head = members.slice(0, SHOWN_ROUTES);
  const tail = members.slice(SHOWN_ROUTES);
  const line = (r: ModelRow, index: number) => (
    <span className="flow-route" key={r.id}>
      <span className={index === 0 ? "flow-src" : "flow-src muted"}>
        {index === 0 ? firstLabel : t("routing.fallback_n", { n: index })}
      </span>
      <span className="flow-arrow" aria-hidden>{"→"}</span>
      <span className="flow-target">
        <StatusDot
          tone={cooling.has(r.id) ? "warn" : "ok"}
          state={cooling.has(r.id) ? t("status.cooling") : undefined}
        />
        {r.id}
        <span className="flow-provider">{providerName(r.model.provider_id)}</span>
      </span>
    </span>
  );
  return (
    <>
      {head.map(line)}
      {tail.length > 0 && (
        <Disclosure title={t("routing.more_fallbacks", { n: tail.length })}>
          {tail.map(line)}
        </Disclosure>
      )}
    </>
  );
}

/**
 * How Default routing is configured. Strategy, failover, and what happens to
 * a model id nobody recognises.
 */
function DefaultDrawer({
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
function ElectionResult({ election, namingStyle }: { election: Election | null; namingStyle: NamingStyle }) {
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
function AutoModeDrawer({
  snapshot,
  busy,
  onClose,
  onSave,
}: {
  snapshot: Snapshot;
  busy: boolean;
  onClose: () => void;
  onSave: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
}) {
  const classifier = snapshot.config.classifier;
  const { t } = useI18n();
  const notify = useToast();
  const rows = useMemo(
    () => modelRows(snapshot),
    [snapshot.config.models, snapshot.exposed_ids],
  );

  const patch = (mutate: Partial<AppConfig["classifier"]>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.classifier, mutate);
      return next;
    });
  };

  const patchCandidate = (model: string, mutate: Partial<ClassifierCandidate>) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      const candidate = next.classifier.candidates.find((c) => c.model === model);
      if (!candidate) return null;
      Object.assign(candidate, mutate);
      return next;
    });
  };

  const removeCandidate = (model: string) => {
    void onSave((cfg) => {
      const next = structuredClone(cfg);
      const index = next.classifier.candidates.findIndex((c) => c.model === model);
      if (index < 0) return null;
      next.classifier.candidates.splice(index, 1);
      return next;
    });
  };

  const moveCandidate = (model: string, delta: -1 | 1) => {
    void onSave((cfg) => {
      const moved = moveCandidatePriority(cfg.classifier.candidates, model, delta);
      if (!moved) return null;
      const next = structuredClone(cfg);
      next.classifier.candidates = moved;
      return next;
    });
  };

  const addCandidate = (model: string) => {
    if (!model) return;
    void onSave((cfg) => {
      if (cfg.classifier.candidates.some((c) => c.model === model)) return null;
      const next = structuredClone(cfg);
      next.classifier.candidates.push({
        model,
        priority: nextCandidatePriority(next.classifier.candidates),
        enabled: true,
      });
      return next;
    });
  };

  /**
   * Whether the free Zen Jev preset is already in the pool.
   *
   * The pool names a model by exposed id or alias, so the check covers both;
   * the provider is matched by signature because the operator may have made one
   * already and named it something else.
   */
  const jevNames = useMemo(() => {
    const provider = snapshot.config.providers.find(isZenJevProvider);
    if (!provider) return null;
    const names = new Set([previewId(provider.id, ZEN_JEV_MODEL)]);
    const model = snapshot.config.models.find(
      (m) => m.provider_id === provider.id && m.upstream_model === ZEN_JEV_MODEL,
    );
    for (const alias of model?.aliases ?? []) names.add(alias);
    return names;
  }, [snapshot.config.providers, snapshot.config.models]);
  const jevInPool = jevNames
    ? classifier.candidates.some((c) => jevNames.has(c.model))
    : false;

  const addZenJev = async () => {
    const ok = await onSave((cfg) => applyZenJevPreset(cfg));
    if (ok) notify("ok", t("routing.jev_added"));
  };

  /*
   * The pool, and the models around it.
   *
   * Three questions were answered by walking: which models are not in the
   * pool (once per model), whether a pool entry names a model that exists
   * (once per entry, and twice per row), and where an entry sits in the pool
   * (once per entry, and twice per row). The answers are the same ones the
   * walks gave — a set of names, a set of known ids, and a place in the pool
   * — and they cost one pass over the configuration each instead of one pass
   * per row.
   */
  const candidateModels = useMemo(
    () => new Set(classifier.candidates.map((c) => c.model)),
    [classifier.candidates],
  );
  const knownIds = useMemo(() => {
    const known = new Set<string>();
    for (const row of rows) {
      known.add(row.id);
      for (const alias of row.model.aliases) known.add(alias);
    }
    return known;
  }, [rows]);

  /**
   * The candidates that answer with a probability rather than with text.
   *
   * A decision model is in the pool by name and looks like every other row in
   * it, but it is asked a typed question and answers in its own protocol. The
   * add menu and the row say which ones those are, because the choice between
   * a chat model and a decision model is a choice between two ways a verdict
   * arrives — and the row explaining the two is a few lines below them.
   */
  const decisionIds = useMemo(() => {
    const kinds = new Map(snapshot.config.providers.map((p) => [p.id, p.kind]));
    const ids = new Set<string>();
    for (const row of rows) {
      if (kinds.get(row.model.provider_id) !== "decision") continue;
      ids.add(row.id);
      for (const alias of row.model.aliases) ids.add(alias);
    }
    return ids;
  }, [rows, snapshot.config.providers]);
  const candidatePlaces = useMemo(() => {
    const places = new Map<string, number>();
    classifier.candidates.forEach((c, index) => {
      if (!places.has(c.model)) places.set(c.model, index);
    });
    return places;
  }, [classifier.candidates]);

  const available = useMemo(
    () =>
      rows.filter(
        (r) =>
          !candidateModels.has(r.id) &&
          // The pool may name the model by an alias. Offering the exposed id
          // again would add the same model twice, and the backend does not
          // reject duplicate candidates.
          !r.model.aliases.some((alias) => candidateModels.has(alias)),
      ),
    [rows, candidateModels],
  );
  const ordered = useMemo(
    () => orderCandidatesByPriority(classifier.candidates),
    [classifier.candidates],
  );

  return (
    <Drawer title={t("route.auto_review")} onClose={onClose}>
      <Toggle
        label={t("routing.route_review")}
        hint={t("routing.route_review_hint")}
        checked={classifier.enabled}
        onChange={(enabled) => patch({ enabled })}
      />

      <div className="controls">
        <Field label={t("field.strategy")}>
          <Select<RoutingStrategy>
            ariaLabel={t("field.strategy")}
            value={classifier.strategy}
            disabled={busy}
            onChange={(strategy) => patch({ strategy })}
            options={[
              { value: "priority", label: t("strategy.priority") },
              { value: "weighted_random", label: t("strategy.weighted_random") },
              { value: "round_robin", label: t("strategy.round_robin") },
              { value: "lowest_latency", label: t("strategy.lowest_latency") },
            ]}
          />
        </Field>
        <NumberField
          label={t("field.max_attempts")}
          min={1}
          max={10}
          integer
          value={classifier.max_attempts}
          onCommit={(max_attempts) => patch({ max_attempts: max_attempts ?? 2 })}
        />
      </div>
      <Toggle
        label={t("routing.review_failover")}
        hint={t("routing.review_failover_hint")}
        checked={classifier.failover}
        onChange={(failover) => patch({ failover })}
      />

      <Section title={t("routing.pool")} hint={t("routing.pool_hint")}>
        {classifier.candidates.length === 0 ? (
          <p className="empty">{t("routing.no_candidates")}</p>
        ) : (
          <table className="table">
            <tbody>
              {ordered.map((c) => {
                const known = knownIds.has(c.model);
                const place = candidatePlaces.get(c.model) ?? -1;
                return (
                  <tr key={c.model} className={known ? "" : "row-warn"}>
                    <td className="mono">
                      {c.model}
                      {decisionIds.has(c.model) && (
                        <>
                          {" "}
                          <Badge tone="neutral">
                            {t("routing.decision_candidate")}
                          </Badge>
                        </>
                      )}
                      {!known && (
                        <>
                          {" "}
                          <Badge tone="warn">{t("routing.not_configured")}</Badge>
                        </>
                      )}
                    </td>
                    <td>
                      <CompactNumber
                        ariaLabel={t("routing.priority_for", { model: c.model })}
                        value={c.priority}
                        min={0}
                        integer
                        onCommit={(v) => patchCandidate(c.model, { priority: v ?? 0 })}
                      />
                    </td>
                    <td>
                      <input
                        type="checkbox"
                        aria-label={t("routing.enable_for", { model: c.model })}
                        checked={c.enabled}
                        onChange={(e) =>
                          patchCandidate(c.model, { enabled: e.currentTarget.checked })
                        }
                      />
                    </td>
                    <td>
                      <div className="row gap">
                        <Button
                          kind="ghost"
                          ariaLabel={t("routing.move_up", { model: c.model })}
                          disabled={place < 1}
                          onClick={() => moveCandidate(c.model, -1)}
                        >
                          ↑
                        </Button>
                        <Button
                          kind="ghost"
                          ariaLabel={t("routing.move_down", { model: c.model })}
                          disabled={
                            place < 0 || place >= classifier.candidates.length - 1
                          }
                          onClick={() => moveCandidate(c.model, 1)}
                        >
                          ↓
                        </Button>
                        <Button kind="ghost" onClick={() => removeCandidate(c.model)}>
                          {t("common.remove")}
                        </Button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {available.length > 0 && (
          <div className="controls">
            <Select
              ariaLabel={t("routing.add_model")}
              value={null}
              placeholder={t("routing.add_model")}
              onChange={(model) => addCandidate(model)}
              options={available.map((r) => ({
                value: r.id,
                label: decisionIds.has(r.id)
                  ? `${r.id} · ${t("routing.decision_candidate")}`
                  : r.id,
              }))}
            />
          </div>
        )}
        {!jevInPool && (
          <div className="controls">
            <Button kind="primary" disabled={busy} onClick={() => void addZenJev()}>
              {t("routing.add_jev")}
            </Button>
          </div>
        )}
        {!jevInPool && <p className="field-hint">{t("routing.add_jev_hint")}</p>}
      </Section>

      <Section title={t("routing.behaviour")}>
        <KeyValue
          rows={[
            [t("routing.detection"), t("routing.automatic")],
            [
              t("routing.verdict_parsing"),
              <span>
                <code>{t("routing.verdict_only")}</code>
                {/* A decision model in the pool answers with a probability
                    rather than a tag, so the parsing is not the whole of how a
                    verdict arrives. */}
                <span className="muted"> · {t("routing.verdict_only_hint")}</span>
              </span>,
            ],
            [t("routing.no_verdict"), t("routing.fails_closed")],
          ]}
        />
        <p className="field-hint">{t("routing.fail_closed_note")}</p>
      </Section>
    </Drawer>
  );
}
