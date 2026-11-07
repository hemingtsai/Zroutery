/** The leaf presentational pieces: a button, a badge, a status dot, and the
 * page/section furniture that only ever wrap what a page hands them. */
import { useId, type ReactNode } from "react";

export function Button({
  children,
  onClick,
  kind = "default",
  disabled,
  title,
  type = "button",
  ariaLabel,
  buttonRef,
  "aria-pressed": ariaPressed,
}: {
  children: ReactNode;
  onClick?: () => void;
  kind?: "default" | "primary" | "danger" | "ghost";
  disabled?: boolean;
  title?: string;
  type?: "button" | "submit";
  ariaLabel?: string;
  buttonRef?: React.Ref<HTMLButtonElement>;
  "aria-pressed"?: boolean;
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
      aria-pressed={ariaPressed}
    >
      {children}
    </button>
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

export function num(value: number): string {
  return value.toLocaleString();
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
  role,
  live,
}: {
  title: ReactNode;
  hint?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  /** Announce this section's content as a status, for a result that arrives. */
  role?: "status" | "alert";
  /** Shorthand for `role="status"`. */
  live?: boolean;
}) {
  const titleId = useId();
  return (
    <section
      className="section"
      // A named region: a section whose title is a span is not exposed as one
      // at all, so a screen-reader user cannot jump between the parts of a
      // page. The title names it.
      aria-labelledby={titleId}
      role={role ?? (live ? "status" : undefined)}
    >
      <div className="section-head">
        <h2 className="section-title" id={titleId}>
          {title}
        </h2>
        {actions}
      </div>
      {hint && <p className="section-hint">{hint}</p>}
      {children}
    </section>
  );
}
