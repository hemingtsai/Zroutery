/**
 * A modal question the user must answer before something happens. Used for
 * everything irreversible — removals, token regeneration — because a
 * one-click destructive action is one mis-click away from data loss.
 */
import { useEffect, useRef, type ReactNode } from "react";
import { useI18n } from "../i18n";
import { Button } from "./atoms";
import { claimFocus, popOverlay, pushOverlay, topOverlay } from "./overlay";

export type ConfirmRequest = {
  title: string;
  /** A string reads as one paragraph; a node (the release dialog's strategy
   * segment) renders inside the same body block. */
  body?: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
};

export function ConfirmDialog({
  request,
  onClose,
}: {
  request: ConfirmRequest | null;
  onClose: () => void;
}) {
  const confirmRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const restoreTo = useRef<Element | null>(null);
  const wasOpen = useRef(false);
  const overlayId = useRef<symbol | null>(null);
  if (overlayId.current === null) overlayId.current = Symbol("confirm");
  const { t } = useI18n();

  /*
   * Focus goes in when the dialog opens and comes back out when it closes.
   *
   * It used to only go in, and a keyboard user who dismissed the dialog — with
   * Escape, or with Cancel — was left with focus on the document body. The
   * next Tab then started from the top of the page, which is a long way from
   * the button they had just used, and nothing on screen says that happened.
   * `Drawer` has always done this; the dialog is now the same.
   *
   * The open/closed transition is tracked in a ref rather than read from the
   * effect's cleanup, because the cleanup also runs when the request object is
   * replaced while the dialog stays open — and restoring focus at that moment
   * would throw the user out of a dialog they were still using.
   */
  useEffect(() => {
    if (request) {
      if (wasOpen.current) return;
      wasOpen.current = true;
      const previous = document.activeElement;
      restoreTo.current = previous instanceof HTMLElement ? previous : null;
      confirmRef.current?.focus();
    } else if (wasOpen.current) {
      wasOpen.current = false;
      const target = restoreTo.current;
      if (target instanceof HTMLElement && target.isConnected) target.focus();
    }
  }, [request]);

  useEffect(() => {
    if (!request) return;
    const id = overlayId.current!;
    pushOverlay(id);
    const onKey = (e: KeyboardEvent) => {
      // Only the top overlay answers Escape. A confirmation raised from inside
      // a drawer would otherwise be dismissed together with the drawer, which
      // is two things happening from one keypress.
      if (e.key === "Escape" && topOverlay() === id) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      popOverlay(id);
    };
  }, [request, onClose]);

  /*
   * Nothing behind the dialog may hold focus while it is open, and Tab has to
   * cycle inside it rather than walk out into the page behind.
   *
   * Both halves are the `Drawer`'s, for the same reason: a modal that leaks
   * focus is not modal, and a screen-reader user who tabs past the dialog
   * lands on content they cannot see. The veil already marks the rest of the
   * page as inert to a pointer; this makes it inert to a keyboard too.
   */
  useEffect(() => {
    if (!request) return;
    const id = overlayId.current!;
    const keepInside = (event: FocusEvent) => {
      claimFocus(panelRef.current, id, event);
    };
    document.addEventListener("focusin", keepInside);
    const onTab = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      // The top overlay traps the keyboard. A dialog below one would otherwise
      // pull the focus back as the user tabbed out of the dialog above it.
      if (topOverlay() !== id) return;
      const root = panelRef.current;
      if (!root) return;
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
    window.addEventListener("keydown", onTab, true);
    return () => {
      document.removeEventListener("focusin", keepInside);
      window.removeEventListener("keydown", onTab, true);
    };
  }, [request]);

  if (!request) return null;

  return (
    <>
      <div className="drawer-veil" onClick={onClose} aria-hidden />
      <div
        className="confirm"
        data-host-security="true"
        ref={panelRef}
        role="alertdialog"
        aria-modal
        aria-label={request.title}
        tabIndex={-1}
      >
        <h2 className="confirm-title">{request.title}</h2>
        {request.body && <div className="confirm-body">{request.body}</div>}
        <div className="confirm-actions">
          <Button kind="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button
            buttonRef={confirmRef}
            kind={request.danger ? "danger" : "primary"}
            onClick={() => {
              request.onConfirm();
              onClose();
            }}
          >
            {request.confirmLabel ?? request.title}
          </Button>
        </div>
      </div>
    </>
  );
}
