/**
 * Transient operation feedback, bottom-right, auto-dismissed. Banners stay
 * for state a user must act on (exposed gateway, failed saves); toasts are
 * for confirmations the user would otherwise have to take on faith — a
 * copy that worked, an import that landed.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useI18n } from "../i18n";
import { createToastDedup } from "../toastDedup";

/** The tones a toast can take. `info` is the neutral one: a notice that is
 *  neither praise nor failure — a deep link pointing somewhere, a state the
 *  app is describing — which used to have to borrow `ok`'s green and be read
 *  as a success. Tone is presentation, not copy: the dictionaries grow no
 *  new keys for it. */
type ToastKind = "ok" | "error" | "info";

type Toast = { id: number; kind: ToastKind; text: string; timer: number };

/**
 * How long each kind stays up. The dedup window is deliberately the same
 * number: "this exact message is already on screen" is true exactly while its
 * toast is displayed, and not a moment longer. An `info` toast lives as long
 * as an `ok` — both are transient confirmations, differing only in colour.
 */
const TOAST_LIFETIME_MS: Record<ToastKind, number> = { ok: 3200, error: 6000, info: 3200 };

const ToastContext = createContext<(kind: ToastKind, text: string) => void>(() => {});

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(0);
  const timers = useRef(new Map<number, number>());
  // The dedup ledger lives as long as the provider does: a remount starts with
  // an empty screen, so it must start with an empty ledger too.
  const dedup = useMemo(() => createToastDedup(() => Date.now()), []);
  useEffect(() => () => {
    for (const timer of timers.current.values()) window.clearTimeout(timer);
    timers.current.clear();
  }, []);
  const { t } = useI18n();

  const notify = useCallback((kind: ToastKind, text: string) => {
    const lifetime = TOAST_LIFETIME_MS[kind];
    // Dedup: restating a toast that is still inside its lifetime (same kind,
    // same text) refreshes that toast instead of stacking an identical one —
    // the same guard cc-switch ships over sonner, where mashing "check for
    // updates" piled up identical popups. Callers keep calling
    // notify(kind, text) exactly as before.
    const { id, reused } = dedup.claim(kind, text, () => nextId.current++, lifetime);
    if (reused) {
      // Retire the previous dismissal timer so exactly one timer owns the
      // toast's lifetime. Deleting before re-inserting below also moves the id
      // to the back of the timers Map — deliberately, because the refresh
      // moves the toast to the back of the display list too (see the state
      // update), and the eviction plus display cap only pair up safely while
      // both orders agree on who is oldest.
      const previous = timers.current.get(id);
      if (previous !== undefined) window.clearTimeout(previous);
      timers.current.delete(id);
    }
    const timer = window.setTimeout(() => {
      timers.current.delete(id);
      setToasts((current) => current.filter((t) => t.id !== id));
    }, lifetime);
    timers.current.set(id, timer);
    // Eviction owns cleanup too; otherwise an invisible toast still fires its
    // timer after the page/provider unmounts. No side effects in state updaters.
    if (timers.current.size > 4) {
      const oldest = timers.current.keys().next().value;
      if (oldest !== undefined) {
        window.clearTimeout(timers.current.get(oldest));
        timers.current.delete(oldest);
      }
    }
    setToasts((current) => [
      // A reused id may or may not still be rendered: a manual dismiss or an
      // earlier display cap can have dropped the toast while its dedup window
      // was still open. Either way it ends up last — the same React key, so
      // the same DOM node is kept and refreshed rather than remounted, and it
      // is never stacked next to itself. For a fresh id the filter is a no-op
      // and this is exactly the append it replaces.
      ...current.filter((t) => t.id !== id).slice(-3),
      { id, kind, text, timer },
    ]);
  }, []);

  const dismiss = (toast: Toast) => {
    window.clearTimeout(toast.timer);
    timers.current.delete(toast.id);
    setToasts((current) => current.filter((t) => t.id !== toast.id));
  };

  return (
    <ToastContext.Provider value={notify}>
      {children}
      <div className="toast-host" role="status" aria-live="polite">
        {toasts.map((toast) => (
          /*
           * The message is the name; "Dismiss" is the tooltip.
           *
           * `aria-label` replaces the content for the accessibility tree, so
           * with one on the button the live region announced "Dismiss" — for
           * every toast in the app. A copy confirmation, a failed check-in, the
           * outcome of a cutover: all of them were announced as the word
           * "Dismiss", and a screen-reader user was told nothing about what had
           * happened.
           */
          <button type="button"
            key={toast.id}
            className={`toast toast-${toast.kind}`}
            onClick={() => dismiss(toast)}
            title={t("common.dismiss")}
          >
            {toast.text}
          </button>
        ))}
      </div>
    </ToastContext.Provider>
  );
}

/** Fire a transient confirmation or error message. */
export function useToast() {
  return useContext(ToastContext);
}
