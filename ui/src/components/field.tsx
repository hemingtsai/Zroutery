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
import {
  cloneElement,
  Fragment,
  isValidElement,
  useId,
  type ReactNode,
} from "react";

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
  disabled,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  hint?: string;
  /** Accessible name for the switch itself, independent of the language. */
  ariaLabel?: string;
  disabled?: boolean;
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
          disabled={disabled}
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
