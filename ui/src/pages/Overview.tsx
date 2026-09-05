import { api, tierMembers, modelRows, TIERS, virtualId, type Snapshot } from "../api";
import { StatusDot, useToast } from "../components";
import { useI18n } from "../i18n";

/**
 * Overview answers three questions and stops: what is connected, where
 * requests go, what happened just now. No hero copy, no counters wall — the
 * information is the layout, set in type and separated by rules.
 *
 * The routing picture is client-agnostic: "Default" and "Auto review" are
 * route purposes, not applications. Which client asked is Activity's
 * business — there it is data, here it would be an assumption.
 */
export default function Overview({
  snapshot,
  onNavigate,
}: {
  snapshot: Snapshot;
  onNavigate: (page: "models" | "providers" | "routing" | "activity" | "settings") => void;
}) {
  const { config, server, recent } = snapshot;
  const { t } = useI18n();
  const notify = useToast();

  const rows = modelRows(snapshot);
  const providers = config.providers.filter((p) => p.enabled);
  const models = rows.filter(
    (r) => r.model.enabled && providers.some((p) => p.id === r.model.provider_id && p.enabled),
  );

  const tierRoutes = TIERS
    .map((tier) => ({ tier, members: tierMembers(rows, config.providers, tier) }))
    .filter((r) => r.members.length > 0);

  const reviewCandidates = config.classifier.enabled
    ? config.classifier.candidates
        .filter((c) => c.enabled)
        .map((c) => rows.find((r) => r.id === c.model || r.model.aliases.includes(c.model)))
        .filter((r): r is (typeof rows)[number] => Boolean(r))
    : [];

  return (
    <>
      <div className="page-head">
        <p className="lede hang">
          <StatusDot tone={server.running ? "ok" : "danger"} />
          {server.running ? (
            <>
              {t("overview.gateway")}{" "}
              <span className="mono">{server.host}:{server.port}</span>
            </>
          ) : (
            t("overview.gateway_stopped")
          )}
          <span className="stat-inline">
            {t("overview.counts", { models: models.length, providers: providers.length })}
          </span>
        </p>
        <div className="page-head-actions text-actions">
          <button
            className="linky"
            onClick={() => api.copy(`http://${server.host}:${server.port}`).then(() => notify("ok", t("toast.copied")))}
          >
            {t("action.copy_base_url")}
          </button>
          <span className="text-actions-sep">·</span>
          <button
            className="linky"
            onClick={() => api.copyToken().then(() => notify("ok", t("toast.copied")))}
          >
            {t("action.copy_token")}
          </button>
        </div>
      </div>

      <section className="section">
        <div className="section-head">
          <span className="section-title">{t("nav.routing")}</span>
          <button className="linky" onClick={() => onNavigate("routing")}>
            {t("overview.configure")}
          </button>
        </div>

        <div className="flow">
          <div className="flow-row">
            <div className="flow-kind">
              <span className="flow-kind-name">{t("route.default")}</span>
              <span className="flow-kind-hint">{t("route.default_hint")}</span>
            </div>
            <div className="flow-routes">
              {tierRoutes.length === 0 ? (
                <span className="flow-empty">{t("route.no_tier")}</span>
              ) : (
                tierRoutes.map(({ tier, members }) => (
                  <span className="flow-route" key={tier}>
                    <span className="flow-src mono">{virtualId(tier, config.routing.naming_style)}</span>
                    <span className="flow-arrow" aria-hidden>{"→"}</span>
                    <span className="flow-target">
                      {members[0].id}
                      <span className="flow-provider">
                        {providers.find((p) => p.id === members[0].model.provider_id)?.name}
                      </span>
                      {members.length > 1 && (
                        <span className="flow-meta">+{members.length - 1}</span>
                      )}
                    </span>
                  </span>
                ))
              )}
            </div>
          </div>

          <div className="flow-row">
            <div className="flow-kind">
              <span className="flow-kind-name">{t("route.auto_review")}</span>
              <span className="flow-kind-hint">
                {config.classifier.enabled
                  ? t("route.review_hint_on")
                  : t("route.review_hint_off")}
              </span>
            </div>
            <div className="flow-routes">
              {config.classifier.enabled ? (
                reviewCandidates.length > 0 ? (
                  <span className="flow-route">
                    <span className="flow-src">{t("route.check")}</span>
                    <span className="flow-arrow" aria-hidden>{"→"}</span>
                    <span className="flow-target">
                      {reviewCandidates[0].id}
                      <span className="flow-provider">
                        {
                          providers.find((p) => p.id === reviewCandidates[0].model.provider_id)
                            ?.name
                        }
                      </span>
                      {reviewCandidates.length > 1 && (
                        <span className="flow-meta">+{reviewCandidates.length - 1}</span>
                      )}
                    </span>
                  </span>
                ) : (
                  <span className="flow-empty">{t("route.no_candidate")}</span>
                )
              ) : (
                <span className="flow-empty">{t("route.off")}</span>
              )}
            </div>
          </div>
        </div>
      </section>

      {providers.length === 0 ? (
        <div className="empty-state">
          <p>{t("overview.no_providers")}</p>
          <button className="linky" onClick={() => onNavigate("providers")}>
            {t("overview.add_provider")}
          </button>
        </div>
      ) : (
        <section className="section">
          <div className="section-head">
            <span className="section-title">{t("overview.recent")}</span>
            <button className="linky" onClick={() => onNavigate("activity")}>
              {t("overview.all_activity")}
            </button>
          </div>
          {recent.length === 0 ? (
            <p className="empty">{t("overview.no_requests")}</p>
          ) : (
            <div className="timeline">
              {recent.slice(0, 5).map((r) => (
                <span className="timeline-item" key={r.id} style={{ cursor: "default" }}>
                  <span className="timeline-time">
                    {new Date(r.at).toLocaleTimeString([], { hour12: false })}
                  </span>
                  <span className="timeline-kind">
                    {r.kind === "auto_mode" ? t("kind.review") : t("kind.main")}
                  </span>
                  <span className="timeline-main-cell">
                    <StatusDot tone={r.ok ? "ok" : "danger"} />
                    <span className="timeline-model">
                      {r.resolved_model ?? r.requested_model}
                    </span>
                    {r.provider_name && (
                      <span className="timeline-provider">{r.provider_name}</span>
                    )}
                  </span>
                  <span className="timeline-meta">
                    <span>{Math.round(r.latency_ms)} ms</span>
                  </span>
                </span>
              ))}
            </div>
          )}
        </section>
      )}
    </>
  );
}
