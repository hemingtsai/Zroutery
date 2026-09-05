/** Small presentational building blocks shared by the panels. */
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import * as RadixSelect from "@radix-ui/react-select";

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
  return (
    <RadixSelect.Root
      value={value ?? undefined}
      onValueChange={(next) => onChange(next as T)}
      disabled={disabled}
    >
      <RadixSelect.Trigger className={`select-trigger ${wide ? "select-wide" : ""}`} aria-label={ariaLabel}>
        <RadixSelect.Value placeholder={placeholder ?? "—"} />
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
  return (
    <div className="segment" role="radiogroup" aria-label={ariaLabel}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={option.value === value}
          className="segment-item"
          data-active={option.value === value}
          onClick={() => onChange(option.value)}
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
  const titleId = useId();

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
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
        className="bar-action"
        aria-expanded={open}
        aria-label={ariaLabel}
        onClick={() => setOpen(!open)}
      >
        {trigger}
      </button>
      {open && (
        <div className="popover" role="menu" aria-labelledby={title ? titleId : undefined}>
          {title && <div className="menu-title" id={titleId}>{title}</div>}
          {typeof children === "function" ? children(() => setOpen(false)) : children}
        </div>
      )}
    </div>
  );
}

/** One row inside a popover menu; the dot marks the current choice. */
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
    <button className={`menu-item ${active ? "active" : ""}`} role="menuitem" onClick={onClick}>
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
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <>
      <div className="drawer-veil" onClick={onClose} aria-hidden />
      <aside className="drawer" role="dialog" aria-modal>
        <header className="drawer-head">
          <div className="drawer-title">{title}</div>
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
 */
export function StatusDot({
  tone,
  label,
}: {
  tone: "ok" | "warn" | "danger" | "off";
  label?: string;
}) {
  const cls =
    tone === "ok" ? "dot-ok" : tone === "warn" ? "dot-warn" : tone === "danger" ? "dot-danger" : "";
  if (!label) return <span className={`dot ${cls}`} aria-hidden />;
  return (
    <span className="dot-label">
      <span className={`dot ${cls}`} />
      {label}
    </span>
  );
}

export function Button({
  children,
  onClick,
  kind = "default",
  disabled,
  title,
  type = "button",
}: {
  children: ReactNode;
  onClick?: () => void;
  kind?: "default" | "primary" | "danger" | "ghost";
  disabled?: boolean;
  title?: string;
  type?: "button" | "submit";
}) {
  return (
    <button
      type={type}
      className={`btn btn-${kind}`}
      onClick={onClick}
      disabled={disabled}
      title={title}
    >
      {children}
    </button>
  );
}

export function Field({
  label,
  hint,
  children,
  wide,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
  wide?: boolean;
}) {
  return (
    <label className={`field ${wide ? "field-wide" : ""}`}>
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  hint?: string;
}) {
  return (
    <label className="toggle">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.currentTarget.checked)}
      />
      <span>
        {label}
        {hint && <em className="field-hint"> {hint}</em>}
      </span>
    </label>
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
}: {
  label: string;
  hint?: string;
  value: string;
  onCommit: (value: string) => void;
  placeholder?: string;
  wide?: boolean;
  readOnly?: boolean;
  password?: boolean;
}) {
  const [draft, setDraft] = useState(value);
  // Adopt values that changed underneath us, e.g. after a save elsewhere.
  useEffect(() => setDraft(value), [value]);

  const commit = () => {
    if (draft !== value) onCommit(draft);
  };

  return (
    <Field label={label} hint={hint} wide={wide}>
      <input
        type={password ? "password" : "text"}
        autoComplete={password ? "off" : undefined}
        value={draft}
        placeholder={placeholder}
        readOnly={readOnly}
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

export function Badge({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: "neutral" | "ok" | "warn" | "danger" | "opus" | "sonnet" | "haiku";
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
