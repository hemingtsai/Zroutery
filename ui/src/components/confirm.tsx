/**
 * A modal question the user must answer before something happens. Used for
 * everything irreversible — removals, token regeneration — because a
 * one-click destructive action is one mis-click away from data loss.
 *
 * The modal behaviour is Radix's (@radix-ui/react-dialog): portal, veil,
 * focus trap and restore, Escape layering (a confirmation raised from inside
 * a drawer is a nested layer, and only the top one answers Escape). What this
 * file owns is the surface's own classes, the alertdialog role, the
 * security-exclusion mark the theme compiler keys on, and focusing the
 * confirm button rather than the first tabbable control — the safe default
 * for a dialog whose whole point is a deliberate choice.
 */
import { useRef, type ReactNode } from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { useI18n } from "../i18n";
import { Button } from "./atoms";

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
  const { t } = useI18n();
  /*
   * Focus comes back out when the dialog closes — to whatever held it when
   * the request first appeared. Captured during the open transition's render,
   * before Radix's FocusScope moves the focus in: effects run too late, the
   * focus is already inside the dialog by then. The transition is tracked in
   * a ref because the request object can be replaced while the dialog stays
   * open, and re-capturing then would target an element inside the dialog.
   */
  const restoreTo = useRef<Element | null>(null);
  const wasOpen = useRef(false);
  if (request) {
    if (!wasOpen.current) {
      wasOpen.current = true;
      restoreTo.current =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
  } else {
    wasOpen.current = false;
  }

  return (
    <DialogPrimitive.Root
      open={request !== null}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="drawer-veil" aria-hidden />
        <DialogPrimitive.Content
          className="confirm"
          tabIndex={-1}
          /*
           * The security root keeps its mark: the theme compiler excludes
           * every recipe from a data-host-security subtree, so a confirmation
           * of a destructive action never borrows an external skin's colours.
           */
          data-host-security="true"
          role="alertdialog"
          onCloseAutoFocus={(event) => {
            const target = restoreTo.current;
            restoreTo.current = null;
            if (target instanceof HTMLElement && target.isConnected) target.focus();
            event.preventDefault();
          }}
        >
          <DialogPrimitive.Title className="confirm-title">
            {request?.title}
          </DialogPrimitive.Title>
          {request?.body && <div className="confirm-body">{request.body}</div>}
          <div className="confirm-actions">
            <Button kind="ghost" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button
              autoFocus
              kind={request?.danger ? "danger" : "primary"}
              onClick={() => {
                request?.onConfirm();
                onClose();
              }}
            >
              {request?.confirmLabel ?? request?.title}
            </Button>
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
