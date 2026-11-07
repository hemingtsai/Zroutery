/**
 * The Auto Mode drawer: the classifier pool editor (candidates, presets,
 * behaviour) with per-candidate patch/remove/move/add.
 */
import { useMemo } from "react";
import {
  ZEN_JEV_MODEL,
  applyZenJevPreset,
  isZenJevProvider,
  modelRows,
  moveCandidatePriority,
  nextCandidatePriority,
  orderCandidatesByPriority,
  previewId,
  type AppConfig,
  type ClassifierCandidate,
  type RoutingStrategy,
  type Snapshot,
} from "../../api";
import { useI18n } from "../../i18n";
import { SaveFeedbackReset } from "../../saveQueue";
import { Badge, Button, CompactNumber, Drawer, Field, KeyValue, NumberField, Section, Select, Toggle, useToast } from "../../components";

export function AutoModeDrawer({
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
      <SaveFeedbackReset />
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
