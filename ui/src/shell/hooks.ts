/**
 * The shell's effect-shaped behaviours, extracted so App.tsx reads as wiring
 * rather than as a wall of listeners. Each hook owns one concern and takes
 * the callbacks it needs; none of them hold state of their own.
 */
import { useEffect } from "react";
import { api, deepLinkCurrent, errorText, onDeepLink, onGatewayStateChanged } from "../api";
import { applySystemAccent } from "../accent";
import type { Page } from "../navigation";
import { pageForLink } from "./nav";

export function useSystemAccent(themePref: unknown): void {
  useEffect(() => {
    let active = true;
    const readAccent = async () => {
      try {
        const accent = await api.systemAccent();
        if (active) applySystemAccent(accent, document.documentElement.style);
      } catch {
        if (active) applySystemAccent(null, document.documentElement.style);
      }
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") void readAccent();
    };
    const systemTheme = window.matchMedia("(prefers-color-scheme: dark)");
    const onFocus = () => void readAccent();
    void readAccent();
    document.addEventListener("visibilitychange", onVisibility);
    systemTheme.addEventListener("change", onFocus);
    window.addEventListener("focus", onFocus);
    return () => {
      active = false;
      document.removeEventListener("visibilitychange", onVisibility);
      systemTheme.removeEventListener("change", onFocus);
      window.removeEventListener("focus", onFocus);
    };
  }, [themePref]);
}

/*
 * A `zroutery://` link opens the page it names.
 *
 * The desktop shell brings the window forward and hands the URL over; this
 * is where the URL becomes a page. The shell cannot do it — it does not
 * know what the pages are called, and it has to keep working when the
 * window does not exist yet — and the window cannot be the one that waits
 * for a link, because on Windows and Linux a link *starts* the application.
 *
 * A link that names a page this build does not have opens none. The shell
 * has already brought the window forward by then, so the operator sees the
 * dashboard rather than a page that does not exist.
 */
export function useDeepLink(onNavigate: (page: Page) => void): void {
  // Runs once on mount, exactly as when it lived in the shell: onNavigate is
  // the page setState wrapper, and re-subscribing to the desktop event would
  // be churn, not correctness.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    const open = (url: string) => {
      const target = pageForLink(url);
      if (!target) return;
      onNavigate(target);
    };

    void onDeepLink(open)
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch(() => {
        // No event bridge (a plain browser): there are no links to open.
      });

    // And the link that *started* this application, which was announced before
    // there was a listener for it.
    void deepLinkCurrent()
      .then((url) => {
        if (!cancelled && url) open(url);
      })
      .catch(() => {
        // No such command in a build without the desktop shell.
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}

/*
 * Alt+1..9 jumps straight to the page at that position in the nav.
 *
 * A desktop utility is navigated from the keyboard as much as from the
 * pointer, and the nav is a fixed list, so the position *is* the address.
 * The position that counts is the one the sidebar is showing: the shortcut
 * names the visible entry at that index, the same promise the tooltips
 * make. Matched by `code` rather than `key`, because Alt is a dead key on
 * some layouts — Option+1 on a Mac keyboard types "¡", and a shortcut that
 * only worked on US layouts would be a shortcut that mostly did not work.
 * Plain digits are left alone — they are typing — and so are every other
 * modifier combination, which belongs to the OS and the webview.
 */
export function useAltDigitNav(
  entries: { id: Page }[],
  onNavigate: (page: Page) => void,
): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
      const digit = /^Digit([1-9])$/.exec(e.code);
      if (!digit) return;
      const target = entries[Number(digit[1]) - 1];
      if (!target) return;
      e.preventDefault();
      onNavigate(target.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [entries, onNavigate]);
}

// Overview and Activity show live state; their pages poll the counters
// only. A failure is reported rather than swallowed: silence would look
// like an idle proxy.
export function useActivityPolling(
  taskPage: string,
  onSample: (activity: Awaited<ReturnType<typeof api.activity>>) => void,
  onError: (message: string | null) => void,
): void {
  useEffect(() => {
    if (taskPage !== "activity" && taskPage !== "overview") return;
    let cancelled = false;
    let pending = false;
    const poll = async () => {
      if (pending || document.visibilityState === "hidden") return;
      pending = true;
      try {
        const activity = await api.activity();
        if (cancelled) return;
        onError(null);
        onSample(activity);
      } catch (e) {
        if (!cancelled) onError(errorText(e));
      } finally {
        pending = false;
      }
    };
    const timer = setInterval(poll, 2000);
    const onVisibility = () => { if (document.visibilityState === "visible") void poll(); };
    document.addEventListener("visibilitychange", onVisibility);
    void poll();
    return () => {
      cancelled = true;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [taskPage, onSample, onError]);
}

/**
 * Subscribe to the snapshot-refresh triggers: the tray announcing a gateway
 * state change, the window regaining focus, the window becoming visible.
 * The coalescing itself lives with the shell's refresh callback; this hook
 * only owns the listeners.
 */
export function useSnapshotRefreshTriggers(refresh: () => Promise<void>): void {
  // The tray can start or stop the gateway while this window sits on another
  // page, so it announces the change and the window re-reads the state instead
  // of trusting the snapshot it happened to load.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onGatewayStateChanged(() => {
      void refresh();
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch(() => {
        // No event bridge (a plain browser): visibility and focus below still
        // refresh the snapshot.
      });
    const onFocus = () => void refresh();
    const onVisibility = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      unlisten?.();
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh]);
}
