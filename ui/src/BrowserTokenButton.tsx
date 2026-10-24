/**
 * Get a NewAPI account's panel token with a headed browser.
 *
 * The panel keeps its dashboard credential in memory (or an HttpOnly cookie),
 * so the token cannot be typed from anywhere except the panel's own token page.
 * This control opens the account's isolated browser on the sign-in page, waits
 * for the page to hand a durable token back, and lets the backend verify and
 * store it. The token never reaches this component: the view has no field that
 * could carry one, and the figure on screen is only a phase.
 */
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type Snapshot, type TokenCaptureView } from "./api";
import { Badge, Button, useToast } from "./components";
import { describeBackendError, useI18n } from "./i18n";

/** Phases that are still waiting on the browser. */
const LIVE = new Set(["opening", "waiting", "needs_verification", "stored"]);
/** How often an open capture is asked what happened. */
const POLL_MS = 1500;

export function BrowserTokenButton({
  providerId,
  accountId,
  run,
  busy,
}: {
  providerId: string;
  accountId: string;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const [view, setView] = useState<TokenCaptureView | null>(null);
  const [starting, setStarting] = useState(false);

  /**
   * Take one report and act on the phase.
   *
   * `stored` is the one phase that needs a second call: the secret is already in
   * the credential store, but the account declaration has to be pointed at it,
   * and the backend reads the configuration as it is now rather than a copy the
   * dashboard held while the browser was open.
   */
  const apply = useCallback(
    async (next: TokenCaptureView) => {
      if (next.phase === "stored") {
        try {
          const done = await api.completeTokenCapture(providerId, accountId);
          setView(done);
          if (done.phase === "captured") {
            notify("ok", t("token_capture.done"));
            await run(api.snapshot);
          } else if (done.phase === "failed") {
            notify("error", done.message ?? t("token_capture.failed"));
          }
        } catch (e) {
          notify("error", describeBackendError(t, errorText(e)));
        }
        return;
      }
      setView(next);
      if (next.phase === "captured") {
        await run(api.snapshot);
      } else if (next.phase === "failed" || next.phase === "unsupported") {
        notify("error", next.message ?? t("token_capture.failed"));
      } else if (next.phase === "cancelled") {
        notify("ok", t("token_capture.cancelled"));
      }
    },
    [providerId, accountId, notify, t, run],
  );

  // A drawer reopened while a capture is running shows where it is, without
  // replaying a notification for a phase that finished before it mounted.
  useEffect(() => {
    let alive = true;
    api
      .tokenCaptureStatus(providerId, accountId)
      .then((next) => {
        if (alive) setView(next);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [providerId, accountId]);

  const phase = view?.phase ?? "idle";
  const live = LIVE.has(phase);

  useEffect(() => {
    if (!live) return;
    const timer = window.setInterval(() => {
      api
        .tokenCaptureStatus(providerId, accountId)
        .then(apply)
        .catch(() => {});
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [live, providerId, accountId, apply]);

  const start = async () => {
    setStarting(true);
    try {
      await apply(await api.startTokenCapture(providerId, accountId));
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setStarting(false);
    }
  };

  const cancel = async () => {
    try {
      setView(await api.cancelTokenCapture(providerId, accountId));
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    }
  };

  return (
    <>
      {!live && phase !== "captured" && (
        <Button kind="ghost" disabled={busy || starting} onClick={() => void start()}>
          {starting ? t("token_capture.starting") : t("token_capture.start")}
        </Button>
      )}
      {live && (
        <>
          <span className="muted">
            {t(`token_capture.phase.${phase}` as Parameters<typeof t>[0])}
          </span>
          <Button kind="ghost" onClick={() => void cancel()}>
            {t("common.cancel")}
          </Button>
        </>
      )}
      {phase === "captured" && <Badge tone="ok">{t("token_capture.captured")}</Badge>}
      {phase === "failed" && <Badge tone="danger">{t("token_capture.failed")}</Badge>}
      {/*
        A failure's reason stays on the page. It used to live only in a toast
        that dismissed itself in six seconds — long enough to see that
        something failed, far too short to read *why*, and the reason was the
        one fact needed to fix it (an expired session, a rejected mint, a
        verification refusal each name a different next step).
      */}
      {(phase === "failed" || phase === "unsupported") && view?.message && (
        <span className="field-hint field-hint-danger">{view.message}</span>
      )}
      {phase === "needs_verification" && view?.message && (
        <span className="field-hint">{view.message}</span>
      )}
    </>
  );
}
