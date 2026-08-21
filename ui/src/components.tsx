/** Small presentational building blocks shared by the panels. */
import { useEffect, useState, type ReactNode } from "react";

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
}: {
  label: string;
  hint?: string;
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
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
    let next = Math.round(parsed);
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    if (next !== value) onCommit(next);
    setDraft(String(next));
  };

  return (
    <Field label={label} hint={hint}>
      <input
        type="number"
        min={min}
        max={max}
        value={draft}
        placeholder={placeholder}
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
    </Field>
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
    <div className={`banner banner-${tone}`} role={tone === "info" ? "status" : "alert"}>
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
