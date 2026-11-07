/**
 * Every notification the shell can show, worst first, behind one ceiling.
 *
 * There used to be six independent slots and no ceiling: two poll failures,
 * a config warning, three configuration errors, an exposed listener and a
 * missing token could stack into six banners, which at 560px tall pushes the
 * page's own content off the screen. The ceiling is two visible, with the
 * rest behind one text action, so nothing is dropped and the page still
 * starts below the fold only once.
 *
 * `severity` orders them rather than JSX order: a missing auth token is a
 * warning and an exposed listener is a danger, and at a two-banner ceiling
 * the order decides which one an operator actually reads.
 */
import { useState, type ReactNode } from "react";
import { describeBackendError } from "../i18n";
import { useI18n } from "../i18n";
import { Banner } from "../components";
import type { Snapshot } from "../api";
import type { Page } from "../navigation";

/** How many notification banners fit above the first useful section. */
const MAX_BANNERS = 2;

type BannerInput = {
  error: string | null;
  onDismissError: () => void;
  pollError: string | null;
  onDismissPollError: () => void;
  snapshot: Snapshot;
  onNavigate: (page: Page) => void;
};

export function BannerStack({
  error,
  onDismissError,
  pollError,
  onDismissPollError,
  snapshot,
  onNavigate,
}: BannerInput) {
  const { t } = useI18n();
  const [showAll, setShowAll] = useState(false);
  const server = snapshot.server;

  const banners: {
    key: string;
    tone: "danger" | "warn";
    node: ReactNode;
    severity: number;
    /** Present when the notice is local state and dismissing it means anything. */
    dismiss?: () => void;
  }[] = [];
  if (error) {
    banners.push({
      key: "error",
      tone: "danger",
      severity: 0,
      node: describeBackendError(t, error),
      dismiss: onDismissError,
    });
  }
  for (const issue of snapshot.issues.filter((i) => i.severity === "error")) {
    banners.push({
      key: `issue:${issue.code}:${issue.subject ?? ""}`,
      tone: "danger",
      severity: 1,
      node: describeBackendError(t, issue.message),
    });
  }
  if (server.exposed) {
    banners.push({
      key: "exposed",
      tone: "danger",
      severity: 2,
      node: t("app.exposed", { host: server.host }),
    });
  }
  if (snapshot.accounting && (!snapshot.accounting.available || snapshot.accounting.error)) {
    banners.push({
      key: "accounting",
      tone: "danger",
      severity: 1,
      node: <>
        {t("accounting.blocked")}{" "}
        <button type="button" className="linky" onClick={() => onNavigate("activity")}>
          {t("accounting.inspect")}
        </button>
      </>,
    });
  }
  if (pollError) {
    banners.push({
      key: "poll",
      tone: "warn",
      severity: 3,
      node: t("app.live_stopped", { err: describeBackendError(t, pollError) }),
      // The next successful poll clears this server-side condition, but a
      // reader who has seen it can fold it away now instead of waiting.
      dismiss: onDismissPollError,
    });
  }
  if (snapshot.warning) {
    banners.push({
      key: "warning",
      tone: "warn",
      severity: 4,
      node: describeBackendError(t, snapshot.warning),
    });
  }
  if (!server.require_auth) {
    banners.push({ key: "no-auth", tone: "warn", severity: 5, node: t("app.no_auth") });
  }
  banners.sort((a, b) => a.severity - b.severity);
  const visibleBanners = showAll ? banners : banners.slice(0, MAX_BANNERS);
  const hiddenBanners = banners.length - MAX_BANNERS;

  return (
    <>
      {visibleBanners.map((banner) => (
        <Banner
          key={banner.key}
          tone={banner.tone}
          actions={
            banner.dismiss ? (
              <button type="button" className="linky" onClick={banner.dismiss}>
                {t("common.dismiss")}
              </button>
            ) : undefined
          }
        >
          {banner.node}
        </Banner>
      ))}
      {hiddenBanners > 0 && !showAll && (
        <p className="banner-overflow">
          <button
            type="button"
            className="linky"
            onClick={() => setShowAll(true)}
          >
            {t("app.more_notifications", { n: hiddenBanners })}
          </button>
        </p>
      )}
      {showAll && banners.length > MAX_BANNERS && (
        <p className="banner-overflow">
          <button
            type="button"
            className="linky"
            onClick={() => setShowAll(false)}
          >
            {t("app.fewer_notifications")}
          </button>
        </p>
      )}
    </>
  );
}
