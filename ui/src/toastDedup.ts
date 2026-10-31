/**
 * Deduplication for the toast host, extracted from ToastProvider so it can be
 * unit-tested without React. It knows nothing about timers, state or the DOM:
 * the provider keeps owning scheduling and rendering, this module only answers
 * "is the same message already on screen, and under which id?".
 *
 * Why dedup at all: identical feedback triggered twice in a row is a texture
 * problem, not a correctness one. cc-switch — the reference project this comes
 * from — ships the same guard over sonner because mashing "check for updates"
 * stacked five identical "already up to date" popups. The message was read the
 * first time; a growing pile of copies makes a routine click look like the app
 * panicking. The same hazard lives here: copy buttons, refresh actions and
 * import retries all fire the same short string on every click. The fix is to
 * refresh the toast already on screen (same id, same node, timer restarted)
 * instead of stacking a new one.
 */

/**
 * One decision per notify(kind, text): render under `id`, either freshly
 * minted through the caller's counter (reused: false) or taken from the toast
 * already showing the same kind:text (reused: true).
 */
export type ToastDedupClaim = { id: number; reused: boolean };

export type ToastDedup = {
  /**
   * Resolve a notify into the id its toast should render under. `lifetimeMs`
   * is how long the toast stays up; while that window is open, a notify with
   * the same kind:text claims the live toast's id again (the caller is
   * expected to restart that toast's dismissal timer alongside, so the two
   * clocks cannot drift apart). `nextId` is called only when a genuinely new
   * toast is needed.
   */
  claim(
    kind: string,
    text: string,
    nextId: () => number,
    lifetimeMs: number,
  ): ToastDedupClaim;
};

/**
 * Build a dedup ledger. `now` is injected rather than reaching for Date.now so
 * a test can advance time by hand and expiry stays exact instead of slept for.
 */
export function createToastDedup(now: () => number): ToastDedup {
  // key -> the id of the toast showing it, plus the clock reading at which its
  // lifetime ends. The key is `kind:text` without escaping, which is safe
  // because kind comes from a fixed enum ("ok" | "error" | "info") that can never
  // contain the ":" separator; if kinds ever become free-form text, this needs
  // a separator the message cannot produce.
  const active = new Map<string, { id: number; until: number }>();

  return {
    claim(kind, text, nextId, lifetimeMs) {
      const key = `${kind}:${text}`;
      const at = now();

      // Expired entries are deleted, not merely ignored: without the sweep the
      // map would grow by one entry per distinct message ever shown and never
      // shrink for the whole session. One pass per notify keeps it bounded by
      // the messages that are actually alive.
      for (const [expiredKey, entry] of active) {
        if (entry.until <= at) active.delete(expiredKey);
      }

      const live = active.get(key);
      if (live) {
        // Restart the window from now, not from the first appearance: a toast
        // that keeps being re-triggered stays up until the LAST trigger's
        // lifetime runs out, matching the dismissal timer the provider resets
        // alongside.
        live.until = at + lifetimeMs;
        return { id: live.id, reused: true };
      }

      const id = nextId();
      active.set(key, { id, until: at + lifetimeMs });
      return { id, reused: false };
    },
  };
}
