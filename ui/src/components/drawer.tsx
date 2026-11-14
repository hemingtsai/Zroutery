/**
 * The right-hand detail surface. Clicking a row opens its whole story here
 * instead of navigating — the interaction model of a desktop application
 * rather than a page-based admin tool.
 *
 * The modal behaviour is Radix's (@radix-ui/react-dialog): portal, veil,
 * focus trap, focus restore to whatever opened the drawer, and Escape
 * layering — a confirmation raised from inside the drawer is a nested layer
 * and only the top one answers Escape. What this file owns is the surface's
 * own classes, the save-status foot, and the two escape yields that Radix's
 * capture-phase keydown would otherwise swallow: an IME composition owns its
 * keys, and a dirty commit input restores its draft before the layer closes.
 */
import { useRef, type ReactNode } from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { useI18n } from "../i18n";
import { useSaveStatus } from "../saveQueue";

/**
 * Whether this drawer answers an Escape itself, or yields.
 *
 * An IME composition owns its keys — the Escape that cancels a candidate must
 * not close the layer the composer is typing into.
 *
 * And a dirty commit input inside the drawer consumes the Escape first: it
 * restores its own draft and marks the event with `preventDefault`, and the
 * close yields to that mark. The next Escape finds the draft clean and closes
 * from here. Exported because the contract is worth asserting directly; the
 * drawer's `onEscapeKeyDown` is a thin wrapper over it.
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
  /*
   * Focus comes back to whatever held it when the drawer opened. Captured
   * during the mount render — before Radix's FocusScope moves the focus in,
   * because effects run after the focus has already moved.
   */
  const restoreTo = useRef<Element | null>(null);
  if (restoreTo.current === null) {
    restoreTo.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }
  const { t } = useI18n();
  /*
   * Every drawer edits the one configuration, so every drawer carries the
   * same save truth at its foot — the surface the edits just happened on,
   * where "did it land?" is asked. The top busybar keeps answering for the
   * window at large and for screen readers; this line answers locally, and
   * says nothing at all until a first save gives it something to say.
   */
  const saveStatus = useSaveStatus();

  return (
    <DialogPrimitive.Root
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="drawer-veil" aria-hidden />
        <DialogPrimitive.Content
          className="drawer"
          tabIndex={-1}
          onCloseAutoFocus={(event) => {
            const target = restoreTo.current;
            restoreTo.current = null;
            if (target instanceof HTMLElement && target.isConnected) target.focus();
            event.preventDefault();
          }}
          onEscapeKeyDown={(event) => {
            // Radix answers Escape in the capture phase on the document, so
            // this runs before a field's own keydown. The yields below keep
            // the drawer's contract: IME owns its keys, and a dirty commit
            // input restores its draft first — the input marks itself with
            // `data-commit-dirty`, and its restore still runs in its own
            // handler afterwards. The first Escape restores the field, the
            // next one, finding the draft clean, closes the drawer.
            if (!drawerEscapeCloses(event, true)) {
              event.preventDefault();
              return;
            }
            const active = document.activeElement;
            if (active instanceof HTMLElement && active.hasAttribute("data-commit-dirty")) {
              event.preventDefault();
            }
          }}
        >
          <header className="drawer-head">
            <DialogPrimitive.Title className="drawer-title">{title}</DialogPrimitive.Title>
            <DialogPrimitive.Close
              className="linky"
              title={t("common.close")}
              aria-label={t("common.close")}
            >
              ×
            </DialogPrimitive.Close>
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
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
