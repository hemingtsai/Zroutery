/**
 * The right-hand detail surface. Clicking a row opens its whole story here
 * instead of navigating — the interaction model of a desktop application
 * rather than a page-based admin tool.
 *
 * The veil is not the modal boundary: `aria-modal` promises that the rest of
 * the page is unreachable, so focus starts inside the drawer, Tab is kept
 * inside it, focus that lands outside is pulled back, and closing returns
 * focus to whatever opened it.
 *
 * The background is isolated rather than marked `inert`: the shell is a
 * *sibling* of the drawer, and `inert` on it stops Chromium from hit testing
 * the drawer's own controls as well, which leaves the visible drawer unusable
 * by mouse. The focus guard below gives the same keyboard guarantee without
 * that side effect.
 */
import { useEffect, useId, useRef, type ReactNode } from "react";
import { useI18n } from "../i18n";
import { useSaveStatus } from "../saveQueue";
import {
  claimFocus,
  isInsidePortalledOverlay,
  popOverlay,
  pushOverlay,
  topOverlay,
} from "./overlay";

/**
 * Whether this drawer answers an Escape itself, or yields.
 *
 * Only the top overlay closes: a confirmation raised from inside the drawer
 * is a separate overlay, and Escape there belongs to the confirmation.
 * An IME composition owns its keys too — the Escape that cancels a candidate
 * must not close the layer the composer is typing into.
 *
 * And a dirty commit input inside the drawer consumes the Escape first: it
 * restores its own draft and marks the event with `preventDefault`, and the
 * close yields to that mark. The next Escape finds the draft clean and
 * closes from here. Exported because the contract is worth asserting
 * directly; the drawer's keydown listener is a thin wrapper over it.
 */
export function drawerEscapeCloses(
  e: { key: string; defaultPrevented: boolean; isComposing?: boolean; keyCode?: number | undefined },
  isTop: boolean,
): boolean {
  if (e.key !== "Escape") return false;
  if (!isTop || e.isComposing || e.keyCode === 229) return false;
  return !e.defaultPrevented;
}

export function Drawer({
  title,
  onClose,
  children,
  showSaveStatus = true,
}: {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  showSaveStatus?: boolean;
}) {
  const panel = useRef<HTMLElement>(null);
  const restoreTo = useRef<Element | null>(null);
  const overlayId = useRef<symbol | null>(null);
  if (overlayId.current === null) overlayId.current = Symbol("drawer");
  const titleId = useId();
  const { t } = useI18n();
  /*
   * Every drawer edits the one configuration, so every drawer carries the
   * same save truth at its foot — the surface the edits just happened on,
   * where "did it land?" is asked. The top busybar keeps answering for the
   * window at large and for screen readers; this line answers locally, and
   * says nothing at all until a first save gives it something to say.
   */
  const saveStatus = useSaveStatus();

  useEffect(() => {
    const id = overlayId.current!;
    pushOverlay(id);
    const previous = document.activeElement;
    restoreTo.current = previous instanceof HTMLElement ? previous : null;
    const first = panel.current?.querySelector<HTMLElement>(
      "input, select, textarea, button:not([disabled]), [tabindex]:not([tabindex='-1'])",
    );
    (first ?? panel.current)?.focus();
    // Nothing behind the drawer may hold focus while it is open — unless a
    // modal is open on top of it, in which case that modal owns the focus and
    // the two must not pull it back and forth.
    const keepInside = (event: FocusEvent) => {
      claimFocus(panel.current, id, event);
    };
    document.addEventListener("focusin", keepInside);
    return () => {
      document.removeEventListener("focusin", keepInside);
      popOverlay(id);
      const target = restoreTo.current;
      if (target instanceof HTMLElement && target.isConnected) target.focus();
    };
  }, []);

  useEffect(() => {
    /*
     * Escape closes in the bubble phase, on purpose. A commit input inside
     * the drawer answers Escape first, at the React root: a dirty draft
     * restores itself and marks the event with `preventDefault`, and this
     * close yields to the mark — the first Escape restores the field, the
     * next one, finding the draft clean, closes the drawer. Handled in the
     * capture phase, the drawer would close first and the restore would
     * never run.
     */
    const onEscape = (e: KeyboardEvent) => {
      if (!drawerEscapeCloses(e, topOverlay() === overlayId.current)) return;
      onClose();
    };
    // Bubble, not capture: the close must come after the fields inside have
    // had their say (see above).
    window.addEventListener("keydown", onEscape);
    const onKey = (e: KeyboardEvent) => {
      // Only the top overlay traps the keyboard.
      if (e.key !== "Tab") return;
      const root = panel.current;
      if (!root) return;
      // A portalled overlay that is open owns the keyboard while it is open —
      // Radix traps the focus inside a select's own menu. The drawer's trap
      // must not reach in and take it back, or Shift+Tab from the first option
      // would throw the user out of the menu into the drawer behind it.
      if (isInsidePortalledOverlay(document.activeElement ?? root)) return;
      const focusable = Array.from(
        root.querySelectorAll<HTMLElement>(
          "a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
        ),
      ).filter((el) => el.offsetParent !== null || el === document.activeElement);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || !root.contains(active))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    // The Tab trap stays in the capture phase: it has to win against whatever
    // would move the focus out of a modal surface.
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onEscape);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [onClose]);

  return (
    <>
      <div className="drawer-veil" onClick={onClose} aria-hidden />
      <aside
        ref={panel}
        className="drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
      >
        <header className="drawer-head">
          <div className="drawer-title" id={titleId}>
            {title}
          </div>
          <button type="button" className="linky" onClick={onClose} title={t("common.close")} aria-label={t("common.close")}>
            ×
          </button>
        </header>
        <div className="drawer-body">{children}</div>
        {showSaveStatus && saveStatus.phase !== "idle" && (
          <footer
            className={`drawer-foot${
              saveStatus.phase === "error" ? " drawer-foot-error" : ""
            }`}
            role="status"
          >
            {saveStatus.phase === "saving" && t("drawer.saving")}
            {saveStatus.phase === "saved" &&
              t("drawer.saved", {
                time: new Date(saveStatus.at).toLocaleTimeString([], { hour12: false }),
              })}
            {saveStatus.phase === "error" &&
              t("drawer.save_failed", { err: saveStatus.message })}
          </footer>
        )}
      </aside>
    </>
  );
}
