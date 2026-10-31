import { describe, expect, it } from "vitest";
import { createToastDedup } from "./toastDedup";

/**
 * A clock the test advances by hand. The helper takes `now` as a parameter
 * instead of reaching for Date.now, so expiry is asserted exactly rather than
 * slept for — the same trick the save queue tests use with hand-settled
 * promises: the test stays the only source of time.
 */
function manualClock() {
  let at = 0;
  return {
    now: () => at,
    advance: (ms: number) => {
      at += ms;
    },
  };
}

/** The provider's id counter, faked: minted in order, starting at 0. */
function idCounter() {
  let next = 0;
  return { mint: () => next++, minted: () => next };
}

describe("toast dedup", () => {
  it("reuses the live toast's id while the same kind:text is inside its lifetime", () => {
    const clock = manualClock();
    const ids = idCounter();
    const dedup = createToastDedup(clock.now);

    const first = dedup.claim("ok", "Copied to clipboard", ids.mint, 3200);
    expect(first).toEqual({ id: 0, reused: false });

    // The copy button clicked twice fast: the second notify restates the first
    // toast instead of stacking an identical one on top of it.
    const second = dedup.claim("ok", "Copied to clipboard", ids.mint, 3200);
    expect(second).toEqual({ id: 0, reused: true });

    clock.advance(2000); // still inside the first toast's 3200 ms
    const third = dedup.claim("ok", "Copied to clipboard", ids.mint, 3200);
    expect(third).toEqual({ id: 0, reused: true });
    // No second toast was ever minted: one message, one id, however many
    // re-triggers happened while it was up.
    expect(ids.minted()).toBe(1);
  });

  it("mints a fresh toast once the previous one's lifetime has elapsed", () => {
    const clock = manualClock();
    const ids = idCounter();
    const dedup = createToastDedup(clock.now);

    dedup.claim("ok", "Copied to clipboard", ids.mint, 3200);

    // Exactly at the boundary the window is already closed: the toast was due
    // to leave the screen at this instant, so the next occurrence must not
    // resurrect the old id — it is a new event and gets a new toast.
    clock.advance(3200);
    const fresh = dedup.claim("ok", "Copied to clipboard", ids.mint, 3200);
    expect(fresh).toEqual({ id: 1, reused: false });
  });

  it("restarts the lifetime on every refresh, measuring from the last trigger", () => {
    const clock = manualClock();
    const ids = idCounter();
    const dedup = createToastDedup(clock.now);

    dedup.claim("error", "Save failed", ids.mint, 6000);
    clock.advance(4000);
    expect(dedup.claim("error", "Save failed", ids.mint, 6000).reused).toBe(true);
    // 8000 ms after the first show: past the original 6000, but only 4000 past
    // the refresh. The window restarted with the refresh — matching the
    // dismissal timer the provider restarts alongside, so the dedup window and
    // the toast's remaining time on screen cannot drift apart.
    clock.advance(4000);
    expect(dedup.claim("error", "Save failed", ids.mint, 6000).reused).toBe(true);

    clock.advance(6000); // now even the refreshed window has run out
    expect(dedup.claim("error", "Save failed", ids.mint, 6000)).toEqual({
      id: 1,
      reused: false,
    });
  });

  it("keeps kinds apart: the same text under a different kind is a different toast", () => {
    const clock = manualClock();
    const ids = idCounter();
    const dedup = createToastDedup(clock.now);

    const ok = dedup.claim("ok", "Same words", ids.mint, 3200);
    const error = dedup.claim("error", "Same words", ids.mint, 6000);
    expect(ok).toEqual({ id: 0, reused: false });
    expect(error).toEqual({ id: 1, reused: false });

    // Restating the ok toast must not latch onto the error toast that happens
    // to share its wording: an "ok" and an "error" with one text are two
    // different statements about the world, and each refreshes only its own.
    expect(dedup.claim("ok", "Same words", ids.mint, 3200)).toEqual({
      id: 0,
      reused: true,
    });
    expect(dedup.claim("error", "Same words", ids.mint, 6000)).toEqual({
      id: 1,
      reused: true,
    });
  });

  it("keeps messages apart: different text under the same kind is a different toast", () => {
    const clock = manualClock();
    const ids = idCounter();
    const dedup = createToastDedup(clock.now);

    const a = dedup.claim("ok", "Copied to clipboard", ids.mint, 3200);
    const b = dedup.claim("ok", "Import finished", ids.mint, 3200);
    expect(a).toEqual({ id: 0, reused: false });
    expect(b).toEqual({ id: 1, reused: false });

    // Both stay independently refreshable: re-triggering the copy action must
    // not refresh (or be absorbed by) the import toast that arrived after it.
    expect(dedup.claim("ok", "Copied to clipboard", ids.mint, 3200)).toEqual({
      id: 0,
      reused: true,
    });
    expect(dedup.claim("ok", "Import finished", ids.mint, 3200)).toEqual({
      id: 1,
      reused: true,
    });
  });
});
