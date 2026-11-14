/**
 * A small anchored menu: the desktop pattern for global, infrequent controls.
 * Opens under its trigger, closes on outside click or Escape.
 *
 * The menu behaviour — arrow keys that wrap, Home/End, typeahead, outside
 * click, Escape, focus moving in on open and back to the trigger on close —
 * is Radix's (@radix-ui/react-dropdown-menu). A role is a promise about the
 * behaviour, and the arrow keys are the behaviour; hand-rolling them a second
 * time next to the select's was drift waiting to happen. What this file keeps
 * is the surface's own classes (`bar-action` / `popover` / `menu-item`) and
 * the collision-safe positioning Radix adds for free (a menu inside a narrow
 * page column flips instead of clipping).
 */
import { useCallback, useState, type ReactNode } from "react";
import * as MenuPrimitive from "@radix-ui/react-dropdown-menu";

export function Popover({
  trigger,
  title,
  ariaLabel,
  children,
}: {
  trigger: ReactNode;
  title?: ReactNode;
  ariaLabel?: string;
  children: ReactNode | ((close: () => void) => ReactNode);
}) {
  const [open, setOpen] = useState(false);
  const close = useCallback(() => setOpen(false), []);

  return (
    <MenuPrimitive.Root open={open} onOpenChange={setOpen}>
      <MenuPrimitive.Trigger type="button" className="bar-action" aria-label={ariaLabel}>
        {trigger}
      </MenuPrimitive.Trigger>
      <MenuPrimitive.Portal>
        <MenuPrimitive.Content
          className="popover"
          side="bottom"
          align="end"
          sideOffset={6}
          aria-label={title === undefined && ariaLabel !== undefined ? ariaLabel : undefined}
        >
          {title && (
            <div className="menu-title" role="none">
              {title}
            </div>
          )}
          {typeof children === "function" ? children(close) : children}
        </MenuPrimitive.Content>
      </MenuPrimitive.Portal>
    </MenuPrimitive.Root>
  );
}

/**
 * One row inside a popover menu; the dot marks the current choice.
 *
 * A row that marks a choice is a `menuitemradio`, and it says so with
 * `aria-checked`. A `menuitem` with a dot beside it announces as an ordinary
 * item: the current theme in the Appearance menu was chosen by a mark nobody
 * could hear and by a text colour nobody could see. The dot itself is the
 * `.menu-radio` span the theme compiler keys its `menu.radio` part on; the
 * `active` class keeps the host stylesheet's mark working unchanged.
 */
export function MenuItem({
  active,
  onClick,
  children,
}: {
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <MenuPrimitive.Item
      className={`menu-item ${active ? "active" : ""}`}
      role={active === undefined ? "menuitem" : "menuitemradio"}
      aria-checked={active === undefined ? undefined : active}
      onSelect={onClick}
    >
      <span className="menu-radio" aria-hidden />
      {children}
    </MenuPrimitive.Item>
  );
}
