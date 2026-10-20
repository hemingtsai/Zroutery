/** Small presentational building blocks shared by the panels. */
import {
  cloneElement,
  createContext,
  Fragment,
  isValidElement,
  useCallback,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
} from "react";
import * as RadixSelect from "@radix-ui/react-select";
import { useI18n } from "./i18n";

/**
 * Transient operation feedback, bottom-right, auto-dismissed. Banners stay
 * for state a user must act on (exposed gateway, failed saves); toasts are
 * for confirmations the user would otherwise have to take on faith — a
 * copy that worked, an import that landed.
 */
type Toast = { id: number; kind: "ok" | "error"; text: string; timer: number };

const ToastContext = createContext<(kind: "ok" | "error", text: string) => void>(() => {});

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(0);
  const { t } = useI18n();

  const notify = useCallback((kind: "ok" | "error", text: string) => {
    const id = nextId.current++;
    const timer = window.setTimeout(() => {
      setToasts((current) => current.filter((t) => t.id !== id));
    }, kind === "ok" ? 3200 : 6000);
    setToasts((current) => [...current.slice(-3), { id, kind, text, timer }]);
  }, []);

  const dismiss = (toast: Toast) => {
    window.clearTimeout(toast.timer);
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
          <button
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

/**
 * A modal question the user must answer before something happens. Used for
 * everything irreversible — removals, token regeneration — because a
 * one-click destructive action is one mis-click away from data loss.
 */
export type ConfirmRequest = {
  title: string;
  body?: string;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
};

/**
 * The open modal overlays, innermost last.
 *
 * Two modal surfaces can be open at once — a confirmation raised from inside a
 * drawer — and each of them pulls the focus back when it lands outside itself.
 * With two roots that are not nested, that is a fight: the confirm focuses its
 * button, the drawer pulls it back, the confirm pulls it again, and the
 * recursion only ends when the stack does.
 *
 * The stack says which one is on top, and a guard that is not the top stands
 * down. That is the whole model, and it does not depend on the two surfaces
 * being nested in the DOM — which they are not, and must not be: a
 * confirmation raised from a drawer is rendered by the page, and moving it
 * into the drawer to make the DOM agree would couple two components that have
 * no reason to know about each other.
 */
const overlayStack: symbol[] = [];

function pushOverlay(id: symbol) {
  overlayStack.push(id);
}

function popOverlay(id: symbol) {
  const at = overlayStack.lastIndexOf(id);
  if (at >= 0) overlayStack.splice(at, 1);
}

/** The overlay currently on top, if any. */
function topOverlay(): symbol | undefined {
  return overlayStack[overlayStack.length - 1];
}

/**
 * The focus guard every modal surface shares.
 *
 * Called with the surface's own root. The surface claims the focus only when
 * it is the top overlay: a modal above it is responsible for the focus, and
 * the surface below must not pull it back.
 */
function claimFocus(root: HTMLElement | null, id: symbol, event: FocusEvent) {
  if (!root) return;
  if (topOverlay() !== id) return;
  const target = event.target;
  if (!(target instanceof Node)) return;
  // The nearest dialog ancestor of the focus target owns it. If that is not
  // us, the focus belongs to a modal we are not.
  const owner =
    target instanceof Element
      ? target.closest('[role="dialog"],[role="alertdialog"]')
      : null;
  if (owner && owner !== root) return;
  if (!root.contains(target)) root.focus();
}

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
        ref={panelRef}
        role="alertdialog"
        aria-modal
        aria-label={request.title}
        tabIndex={-1}
      >
        <h2 className="confirm-title">{request.title}</h2>
        {request.body && <p className="confirm-body">{request.body}</p>}
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

/**
 * A select, drawn by us. Native <select> menus are rendered by the OS, so
 * their open state cannot follow the design system across Windows and macOS —
 * the closed box is the only part we ever control, and the moment it opens
 * the interface stops looking like itself. This wrapper owns the visuals and
 * leaves keyboard navigation, focus and ARIA to the headless primitives.
 *
 * Radix rejects empty-string values, so "no choice" is expressed by passing
 * value={null} (the trigger shows the placeholder) and using a real sentinel
 * in the options where a "none" entry is needed.
 */
export function Select<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  placeholder,
  disabled,
  wide,
}: {
  value: T | null;
  onChange: (value: T) => void;
  options: { value: T; label: ReactNode }[];
  ariaLabel?: string;
  placeholder?: string;
  disabled?: boolean;
  /** Stretch to the enclosing field instead of the widest option. */
  wide?: boolean;
}) {
  /*
   * The trigger is named by its field and describes its value.
   *
   * It used to be named by `aria-label` alone, and an `aria-label` replaces the
   * content for the accessibility tree — so the selected option's text was in
   * the content and nowhere in the announcement. A screen reader said "Tier,
   * combobox, collapsed" and the user had to open every dropdown to learn what
   * was actually selected.
   *
   * So the value is now the trigger's description, and the description points
   * at the node Radix already renders for it. The name stays the field's.
   *
   * The first attempt gave the trigger a *name* built from both, by adding a
   * visually hidden copy of the field's label. That read well as a name and
   * duplicated the field's label in the page: the visible one and the hidden
   * one were both there, and a screen reader reading the page aloud said the
   * word twice. A description says the same thing and says it once.
   */
  const valueId = useId();
  return (
    <RadixSelect.Root
      /*
       * `null` becomes the empty string, and the difference matters.
       *
       * Radix treats `undefined` as "uncontrolled" and keeps its own state, so
       * a select that is handed `null` shows the placeholder once and then
       * keeps whatever was last chosen — the "add a model" combobox displayed
       * the model that had just been added, and stopped reading as an
       * add-control. The empty string is controlled *and* is what Radix treats
       * as "nothing selected", so the placeholder comes back when the value
       * does.
       *
       * A caller that wants an empty-string *option* — a real "no tier"
       * choice — must use a sentinel of its own, which is what the catalogue
       * row does. Radix cannot show an option whose value is the empty string
       * and show the placeholder at the same time, so the two cannot share
       * the mark.
       */
      value={value ?? ""}
      onValueChange={(next) => onChange(next as T)}
      disabled={disabled}
    >
      <RadixSelect.Trigger
        className={`select-trigger ${wide ? "select-wide" : ""}`}
        aria-label={ariaLabel}
        aria-describedby={valueId}
      >
        <RadixSelect.Value id={valueId} placeholder={placeholder ?? "—"} />
        <RadixSelect.Icon className="select-icon" aria-hidden>
          <svg width="10" height="6" viewBox="0 0 10 6">
            <path
              d="M1 1l4 4 4-4"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        </RadixSelect.Icon>
      </RadixSelect.Trigger>
      <RadixSelect.Portal>
        <RadixSelect.Content className="select-menu" position="popper" sideOffset={4}>
          <RadixSelect.Viewport className="select-viewport">
            {options.map((option) => (
              <RadixSelect.Item key={option.value} value={option.value} className="select-item">
                <RadixSelect.ItemText>{option.label}</RadixSelect.ItemText>
                <RadixSelect.ItemIndicator className="select-indicator" aria-hidden>
                  <span className="select-indicator-dot" />
                </RadixSelect.ItemIndicator>
              </RadixSelect.Item>
            ))}
          </RadixSelect.Viewport>
        </RadixSelect.Content>
      </RadixSelect.Portal>
    </RadixSelect.Root>
  );
}

/**
 * A group of panels behind one strip of tab headers, for a page section that
 * holds more than the eye should track at once.
 *
 * Built on the `segment` look so it reads as the same control as the segmented
 * choices above, but it is a real tablist rather than a radio group: the panels
 * are reachable by the keyboard (Left/Right, Home/End), the active tab is the
 * only one in the tab order, and each panel is named by its header so a screen
 * reader announces "routing, tab 2 of 3" instead of reading the whole page.
 *
 * `Segment` stays a radio group on purpose: choosing between options is one
 * decision, and this is a change of subject.
 */
export function Tabs({
  value,
  onChange,
  tabs,
  ariaLabel,
}: {
  value: string;
  onChange: (value: string) => void;
  tabs: { value: string; label: ReactNode; content: ReactNode }[];
  ariaLabel?: string;
}) {
  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = tabs.length - 1;
    let next: number | null = null;
    if (event.key === "ArrowRight") next = index === last ? 0 : index + 1;
    else if (event.key === "ArrowLeft") next = index === 0 ? last : index - 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    if (next === null) return;
    event.preventDefault();
    onChange(tabs[next].value);
    // Move focus with the selection, so the keyboard user lands on the header
    // rather than back at the top of the page.
    const headers = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]');
    headers?.[next]?.focus();
  };
  return (
    <div className="tabs">
      <div className="segment" role="tablist" aria-label={ariaLabel}>
        {tabs.map((tab, index) => (
          <button
            key={tab.value}
            type="button"
            role="tab"
            id={`tab-${tab.value}`}
            aria-controls={`panel-${tab.value}`}
            aria-selected={tab.value === value}
            // Roving tabindex: one stop for the whole strip.
            tabIndex={tab.value === value ? 0 : -1}
            className="segment-item"
            data-active={tab.value === value}
            onClick={() => onChange(tab.value)}
            onKeyDown={(event) => onKeyDown(event, index)}
          >
            {tab.label}
          </button>
        ))}
      </div>
      {tabs.map((tab) =>
        tab.value === value ? (
          <div
            key={tab.value}
            role="tabpanel"
            id={`panel-${tab.value}`}
            aria-labelledby={`tab-${tab.value}`}
            tabIndex={0}
            className="tab-panel"
          >
            {tab.content}
          </div>
        ) : null,
      )}
    </div>
  );
}

/**
 * A titled block that folds away, for supporting detail the reader only wants
 * when they go looking for it.
 *
 * This is a button and a region rather than a bare `<details>`: the native one
 * carries a browser-drawn marker and default type that match neither this
 * stylesheet nor the rest of the page, and its open/closed state is invisible
 * to assistive technology as "expanded". `aria-expanded` and the chevron carry
 * the state; Enter and Space both work because it is a real button.
 */
export function Disclosure({
  title,
  count,
  defaultOpen = false,
  children,
}: {
  title: ReactNode;
  /** Optional trailing count, shown on the header rather than in the body. */
  count?: ReactNode;
  defaultOpen?: boolean;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const bodyId = `disclosure-${useId()}`;
  return (
    <div className="disclosure" data-open={open}>
      <button
        type="button"
        className="disclosure-head"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((previous) => !previous)}
      >
        <span className="disclosure-chevron" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        <span className="disclosure-title">{title}</span>
        {count !== undefined && <span className="disclosure-count">{count}</span>}
      </button>
      {/*
        Kept mounted and collapsed by a 0fr grid row rather than unmounted, so
        the height can be transitioned. An unmounted body gives the opening a
        fade and the closing nothing at all, which reads as two different
        components rather than one thing opening.

        `inert` is what makes a collapsed-but-present body correct rather than
        merely invisible: without it every control inside stays in the tab order
        while clipped to nothing, so keyboard users tab into a panel they cannot
        see. React 19 takes `inert` as a boolean, and it also takes the element
        out of the accessibility tree, which `aria-hidden` alone would not do.
      */}
      <div className="disclosure-panel" id={bodyId} inert={!open}>
        <div className="disclosure-clip">
          <div className="disclosure-body">{children}</div>
        </div>
      </div>
    </div>
  );
}

/**
 * Two or three mutually exclusive choices, as a quiet segmented control: the
 * active segment is marked by a two-pixel rule under it and nothing else —
 * the professional-application switch, not the web-form dropdown.
 */
export function Segment<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
}: {
  value: T;
  onChange: (value: T) => void;
  options: { value: T; label: ReactNode }[];
  ariaLabel?: string;
}) {
  /*
   * `role="radiogroup"` is a promise, not a label.
   *
   * The ARIA pattern requires arrow keys to move *and select* within a radio
   * group, and a roving tabindex so the group is one stop in the tab sequence.
   * Without them this was a row of buttons wearing a role it did not honour:
   * a screen reader would announce "radio, 1 of 3" and then arrow keys would do
   * nothing, which is worse than saying nothing. `Tabs` already implemented
   * exactly this, and this borrows its shape.
   */
  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = options.length - 1;
    let next: number | null = null;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") next = index === last ? 0 : index + 1;
    else if (event.key === "ArrowLeft" || event.key === "ArrowUp") next = index === 0 ? last : index - 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    if (next === null) return;
    event.preventDefault();
    onChange(options[next].value);
    const items = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="radio"]');
    items?.[next]?.focus();
  };

  /*
   * Roving tabindex: one stop for the whole group, and the one stop is
   * wherever the value currently is, so Tab lands on the selected option.
   *
   * Unless there is no selected option. A value that matches nothing left
   * every radio with `tabIndex={-1}`, and a group whose every member is
   * un-focusable is a group the keyboard skips entirely — a radiogroup that
   * cannot be reached. The first option takes the stop in that case, which
   * is where the arrow-key handler would take it anyway.
   */
  const selected = options.findIndex((option) => option.value === value);
  const stopIndex = selected >= 0 ? selected : 0;

  return (
    <div className="segment" role="radiogroup" aria-label={ariaLabel}>
      {options.map((option, index) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={option.value === value}
          className="segment-item"
          data-active={option.value === value}
          tabIndex={index === stopIndex ? 0 : -1}
          onClick={() => onChange(option.value)}
          onKeyDown={(event) => onKeyDown(event, index)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

/**
 * A small anchored menu: the desktop pattern for global, infrequent controls.
 * Opens under its trigger, closes on outside click or Escape.
 */
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
      <button
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
          {title && <div className="menu-title" id={titleId}>{title}</div>}
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
    <button
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

export function Card({
  title,
  actions,
  children,
  tone,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  tone?: "warn" | "danger";
}) {
  return (
    <section className={`card ${tone ? `card-${tone}` : ""}`}>
      {(title || actions) && (
        <header className="card-head">
          {title && <h2>{title}</h2>}
          {actions && <div className="row gap">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

/**
 * A page's opening line. The page name itself lives in the main bar; repeating
 * it bigger in the content is a web-app habit, so only the summary line and
 * the quiet actions render here.
 */
export function PageHead({
  lede,
  actions,
}: {
  lede?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="page-head">
      <p className="lede">{lede}</p>
      {actions && <div className="page-head-actions">{actions}</div>}
    </div>
  );
}

/** A band inside a page: label + hint, above whatever the band contains. */
export function Section({
  title,
  hint,
  actions,
  children,
}: {
  title: ReactNode;
  hint?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="section">
      <div className="section-head">
        <span className="section-title">{title}</span>
        {actions}
      </div>
      {hint && <p className="section-hint">{hint}</p>}
      {children}
    </section>
  );
}

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
export function Drawer({
  title,
  onClose,
  children,
}: {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
}) {
  const panel = useRef<HTMLElement>(null);
  const restoreTo = useRef<Element | null>(null);
  const overlayId = useRef<symbol | null>(null);
  if (overlayId.current === null) overlayId.current = Symbol("drawer");
  const titleId = useId();

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
    const onKey = (e: KeyboardEvent) => {
      // Only the top overlay answers Escape, and only the top overlay traps
      // the keyboard. A confirmation raised from inside this drawer is a
      // separate overlay: Escape there closes the confirmation and leaves the
      // drawer open, which is what one keypress should do.
      if (topOverlay() !== overlayId.current) return;
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const root = panel.current;
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
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
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
          <button className="linky" onClick={onClose} title="Close (Esc)" aria-label="Close">
            ×
          </button>
        </header>
        <div className="drawer-body">{children}</div>
      </aside>
    </>
  );
}

/** Facts about one thing, as label/value rows. */
export function KeyValue({ rows }: { rows: [ReactNode, ReactNode][] }) {
  return (
    <div className="kv">
      {rows.map(([key, value], i) => (
        <div className="kv-row" key={i}>
          <span className="kv-key">{key}</span>
          <span className="kv-val">{value}</span>
        </div>
      ))}
    </div>
  );
}

/**
 * A status, as a small dot and a word. The dot is deliberately tiny: status
 * is information, not decoration, and full-width pills shout.
 *
 * `state` is the status in words, for a screen reader. A dot is a colour, and
 * a colour is invisible to one — so a caller that means something by the
 * colour has to say it here. The word is visually hidden, because the design
 * is the design and the dot is the design's way of saying it.
 *
 * It is a caller-supplied string rather than a word chosen from the tone
 * because the tones have no single meaning: "ok" is a request that succeeded
 * on the activity timeline and a model that is simply enabled on the models
 * page, and one word cannot be right for both.
 */
export function StatusDot({
  tone,
  label,
  state,
}: {
  tone: "ok" | "warn" | "danger" | "off";
  label?: string;
  state?: string;
}) {
  const cls =
    tone === "ok" ? "dot-ok" : tone === "warn" ? "dot-warn" : tone === "danger" ? "dot-danger" : "";
  if (label) {
    return (
      <span className="dot-label">
        <span className={`dot ${cls}`} />
        {label}
      </span>
    );
  }
  if (state) {
    return (
      <span className="dot-label">
        <span className={`dot ${cls}`} />
        <span className="visually-hidden">{state}</span>
      </span>
    );
  }
  return <span className={`dot ${cls}`} aria-hidden />;
}

export function Button({
  children,
  onClick,
  kind = "default",
  disabled,
  title,
  type = "button",
  ariaLabel,
  buttonRef,
}: {
  children: ReactNode;
  onClick?: () => void;
  kind?: "default" | "primary" | "danger" | "ghost";
  disabled?: boolean;
  title?: string;
  type?: "button" | "submit";
  ariaLabel?: string;
  buttonRef?: React.Ref<HTMLButtonElement>;
}) {
  return (
    <button
      ref={buttonRef}
      type={type}
      className={`btn btn-${kind}`}
      onClick={onClick}
      disabled={disabled}
      title={title}
      aria-label={ariaLabel}
    >
      {children}
    </button>
  );
}

/**
 * A labelled control, with its hint as a description rather than as part of
 * the name.
 *
 * The hint used to sit inside the `<label>`, which is what made the control's
 * accessible name "label + the whole hint sentence". A screen reader then
 * announced a two-sentence name and never a description, so the part that
 * explains what to put in the field was in the name instead of being
 * available as the description it is.
 *
 * The fix has to leave the DOM exactly as it was. The hint stays inside the
 * label, so it is excluded from the name with `aria-hidden` — and the same
 * text is repeated in a visually hidden element *outside* the label, which is
 * what the control is then described by. The element is absolutely positioned,
 * so it takes no space and the field measures exactly as it did before.
 *
 * Moving the hint out of the label instead would have been the tidier
 * markup, and it changed the flex row the field is laid out in: the layout
 * harness measures the real geometry of these boxes, and it was right to
 * refuse.
 */
export function Field({
  label,
  hint,
  children,
  wide,
  danger,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
  wide?: boolean;
  /** Render the hint in danger tone — the label of a validation problem. */
  danger?: boolean;
}) {
  const hintId = useId();

  const control = (() => {
    if (!isValidElement(children) || children.type === Fragment) return children;
    const element = children as React.ReactElement<{ "aria-describedby"?: string }>;
    const existing = element.props["aria-describedby"];
    return cloneElement(element, {
      "aria-describedby": hint ? [existing, hintId].filter(Boolean).join(" ") : existing,
    });
  })();

  return (
    <>
      <label className={`field ${wide ? "field-wide" : ""}`}>
        <span className="field-label">{label}</span>
        {control}
        {hint && (
          <span
            aria-hidden
            className={danger ? "field-hint field-hint-danger" : "field-hint"}
          >
            {hint}
          </span>
        )}
      </label>
      {hint && (
        <span id={hintId} className="visually-hidden">
          {hint}
        </span>
      )}
    </>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  hint,
  ariaLabel,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  hint?: string;
  /** Accessible name for the switch itself, independent of the language. */
  ariaLabel?: string;
}) {
  /*
   * The hint is a description, not part of the name.
   *
   * It used to sit inside the `<label>`, which made the switch's accessible
   * name the label and the whole hint sentence together — so a switch
   * announced its explanation instead of its name, and the name had to be
   * repeated in an `aria-label` to survive at all.
   *
   * The DOM is unchanged: the visible hint is excluded from the name with
   * `aria-hidden`, and the same text is repeated in a visually hidden element
   * outside the label, which is what the switch is described by. The element
   * is absolutely positioned, so the switch measures exactly as it did.
   */
  const hintId = useId();
  return (
    <>
      <label className="toggle">
        <input
          type="checkbox"
          checked={checked}
          aria-label={ariaLabel}
          aria-describedby={hint ? hintId : undefined}
          onChange={(e) => onChange(e.currentTarget.checked)}
        />
        <span className="toggle-label">
          <span className="toggle-text">{label}</span>
          {hint && (
            <span aria-hidden className="toggle-hint">
              {hint}
            </span>
          )}
        </span>
      </label>
      {hint && (
        <span id={hintId} className="visually-hidden">
          {hint}
        </span>
      )}
    </>
  );
}

/**
 * Text input that reports its value when the user is done with it.
 *
 * Every commit round trips through Rust and rewrites the configuration file, so
 * doing that per keystroke is both wasteful and a good way to save half-typed
 * URLs. Enter commits, Escape reverts, blur commits.
 */
export function TextField({
  label,
  hint,
  value,
  onCommit,
  placeholder,
  wide,
  readOnly,
  password,
  error,
  ariaLabel,
}: {
  label: string;
  hint?: string;
  value: string;
  onCommit: (value: string) => void;
  placeholder?: string;
  wide?: boolean;
  readOnly?: boolean;
  password?: boolean;
  /** Inline problem with the current value; shown under the input in danger tone. */
  error?: string | null;
  /** Accessible name, for inputs whose visible label is generic. */
  ariaLabel?: string;
}) {
  const [draft, setDraft] = useState(value);
  // Adopt values that changed underneath us, e.g. after a save elsewhere.
  useEffect(() => setDraft(value), [value]);

  const commit = () => {
    if (draft !== value) onCommit(draft);
  };

  return (
    <Field label={label} hint={error ?? hint} wide={wide} danger={Boolean(error)}>
      <input
        className={error ? "input-error" : undefined}
        type={password ? "password" : "text"}
        autoComplete={password ? "off" : undefined}
        value={draft}
        placeholder={placeholder}
        readOnly={readOnly}
        aria-label={ariaLabel}
        onChange={(e) => setDraft(e.currentTarget.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            commit();
            e.currentTarget.blur();
          } else if (e.key === "Escape") {
            setDraft(value);
          }
        }}
      />
    </Field>
  );
}

/** Same idea as [`TextField`] for numbers, with clamping on commit. */
export function NumberField({
  label,
  hint,
  value,
  onCommit,
  min,
  max,
  placeholder,
  integer,
  ariaLabel,
}: {
  label: string;
  hint?: string;
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  /** Round committed values to whole numbers (counts, ports, seconds). */
  integer?: boolean;
  /** Accessible name, for inputs whose visible label is generic. */
  ariaLabel?: string;
}) {
  return (
    <Field label={label} hint={hint}>
      <CompactNumber
        value={value}
        onCommit={onCommit}
        min={min}
        max={max}
        placeholder={placeholder}
        integer={integer}
        ariaLabel={ariaLabel}
      />
    </Field>
  );
}

/**
 * A bare [`NumberField`]-style input for tight spots like table cells, where a
 * labelled field will not fit. Commits on blur or Enter, reverts on Escape;
 * saving half-typed numbers per keystroke would persist "1" on the way to "10".
 */
export function CompactNumber({
  value,
  onCommit,
  min,
  max,
  placeholder,
  ariaLabel,
  integer,
}: {
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  ariaLabel?: string;
  /** Round committed values to whole numbers (counts, ports, seconds). */
  integer?: boolean;
}) {
  const text = value === null ? "" : String(value);
  const [draft, setDraft] = useState(text);
  useEffect(() => setDraft(text), [text]);

  const commit = () => {
    if (draft.trim() === "") {
      if (value !== null) onCommit(null);
      return;
    }
    const parsed = Number(draft);
    if (!Number.isFinite(parsed)) {
      setDraft(text);
      return;
    }
    let next = integer ? Math.round(parsed) : parsed;
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    if (next !== value) onCommit(next);
    setDraft(String(next));
  };

  return (
    <input
      type="number"
      className="tiny"
      min={min}
      max={max}
      step={integer ? 1 : "any"}
      placeholder={placeholder}
      aria-label={ariaLabel}
      value={draft}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          commit();
          e.currentTarget.blur();
        } else if (e.key === "Escape") {
          setDraft(text);
        }
      }}
    />
  );
}

/** Bare input that commits on blur or Enter, reverts on Escape. No label — use with setting-row. */
export function CommitInput({
  value,
  onCommit,
  placeholder,
  type = "text",
  ariaLabel,
}: {
  value: string;
  onCommit: (value: string) => void;
  placeholder?: string;
  type?: "text" | "password";
  /**
   * Required in practice.
   *
   * Unlike [`Field`], which wraps its child in a `<label>` and so names it, this
   * is used bare inside `.setting-row` — a div whose title is a `<span>`, which
   * names nothing. Without this the control announces as an unlabelled text box
   * and the reader has no way to know which setting they are in. The visible
   * `.setting-title` is passed as the name, so the two cannot drift.
   */
  ariaLabel: string;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);

  const commit = () => {
    if (draft !== value) onCommit(draft);
  };

  return (
    <input
      type={type}
      value={draft}
      aria-label={ariaLabel}
      placeholder={placeholder}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          commit();
          e.currentTarget.blur();
        } else if (e.key === "Escape") {
          setDraft(value);
        }
      }}
    />
  );
}

/** Same as CommitInput but for numbers. */
export function CommitNumber({
  value,
  onCommit,
  min,
  max,
  integer,
  ariaLabel,
}: {
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  integer?: boolean;
  /** Required in practice — see [`CommitInput`]. */
  ariaLabel: string;
}) {
  const text = value === null ? "" : String(value);
  const [draft, setDraft] = useState(text);
  useEffect(() => setDraft(text), [text]);

  const commit = () => {
    if (draft.trim() === "") {
      if (value !== null) onCommit(null);
      return;
    }
    const parsed = Number(draft);
    if (!Number.isFinite(parsed)) {
      setDraft(text);
      return;
    }
    let next = integer ? Math.round(parsed) : parsed;
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    if (next !== value) onCommit(next);
    setDraft(String(next));
  };

  return (
    <input
      type="number"
      className="tiny"
      aria-label={ariaLabel}
      min={min}
      max={max}
      step={integer ? 1 : "any"}
      value={draft}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          commit();
          e.currentTarget.blur();
        } else if (e.key === "Escape") {
          setDraft(text);
        }
      }}
    />
  );
}

export function Badge({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: "neutral" | "ok" | "warn" | "danger" | "fast" | "standard" | "reasoning" | "frontier";
}) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}

export function Banner({
  tone,
  children,
  actions,
}: {
  tone: "info" | "warn" | "danger";
  children: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div
      className={`banner banner-${tone}`}
      role={tone === "info" ? "status" : "alert"}
    >
      <div>{children}</div>
      {actions && <div className="row gap">{actions}</div>}
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="empty">{children}</p>;
}

export function ms(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  if (value < 1000) return `${Math.round(value)} ms`;
  return `${(value / 1000).toFixed(2)} s`;
}

export function num(value: number): string {
  return value.toLocaleString();
}
