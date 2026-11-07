/**
 * A small anchored menu: the desktop pattern for global, infrequent controls.
 * Opens under its trigger, closes on outside click or Escape.
 */
import { useEffect, useId, useRef, useState, type ReactNode } from "react";

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
  const anchor = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(false);
  const titleId = useId();

  /*
   * A menu has to behave like a menu.
   *
   * This popover claimed `role="menu"` with `role="menuitem"` children and
   * nothing else: a screen reader announced "menu" and then the arrow keys did
   * what arrow keys do everywhere else, which is move the page. A role is a
   * promise about the behaviour, and the arrow keys are the behaviour.
   *
   * So the arrow keys are here now: Down and Up move between items and wrap,
   * Home and End jump to the ends, Escape closes and gives focus back to the
   * trigger that opened it, and Tab leaves the menu rather than walking
   * through it. Focus moves into the menu when it opens, because a menu that
   * opens with the focus still on its trigger is a menu the keyboard cannot
   * reach.
   */
  useEffect(() => {
    if (open) {
      if (!wasOpen.current) {
        wasOpen.current = true;
        const first = menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]');
        (first ?? triggerRef.current)?.focus();
      }
    } else if (wasOpen.current) {
      wasOpen.current = false;
      // Only when the menu closes by itself. A click on a menu item moves
      // focus to whatever that item did, and yanking it back to the trigger
      // would undo that.
      if (document.activeElement === document.body) triggerRef.current?.focus();
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        return;
      }
      if (e.key === "Tab") {
        // Tabbing out of a menu closes it, which is what every other menu
        // does. The menu does not trap the keyboard.
        setOpen(false);
        return;
      }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
      const items = Array.from(
        menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [],
      );
      if (items.length === 0) return;
      e.preventDefault();
      const active = document.activeElement;
      const at = items.indexOf(active as HTMLElement);
      let next: number;
      if (e.key === "Home") next = 0;
      else if (e.key === "End") next = items.length - 1;
      else if (e.key === "ArrowDown") next = at < 0 ? 0 : (at + 1) % items.length;
      else next = at < 0 ? items.length - 1 : (at - 1 + items.length) % items.length;
      items[next].focus();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="popover-anchor" ref={anchor}>
      <button type="button"
        ref={triggerRef}
        className="bar-action"
        aria-expanded={open}
        aria-haspopup="menu"
        aria-label={ariaLabel}
        onClick={() => setOpen(!open)}
      >
        {trigger}
      </button>
      {open && (
        <div
          className="popover"
          ref={menuRef}
          role="menu"
          aria-labelledby={title ? titleId : undefined}
        >
          {title && (
            <div className="menu-title" id={titleId} role="none">
              {title}
            </div>
          )}
          {typeof children === "function" ? children(() => setOpen(false)) : children}
        </div>
      )}
    </div>
  );
}

/**
 * One row inside a popover menu; the dot marks the current choice.
 *
 * A row that marks a choice is a `menuitemradio`, and it says so with
 * `aria-checked`. A `menuitem` with a dot beside it announces as an ordinary
 * item: the current theme in the Appearance menu was chosen by a mark nobody
 * could hear and by a text colour nobody could see.
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
    <button type="button"
      className={`menu-item ${active ? "active" : ""}`}
      role={active === undefined ? "menuitem" : "menuitemradio"}
      aria-checked={active === undefined ? undefined : active}
      onClick={onClick}
    >
      <span className="menu-radio" aria-hidden />
      {children}
    </button>
  );
}
