/**
 * Two or three mutually exclusive choices, as a quiet segmented control: the
 * active segment is marked by a two-pixel rule under it and nothing else —
 * the professional-application switch, not the web-form dropdown.
 */
import type { ReactNode } from "react";

export function Segment<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  className,
}: {
  value: T;
  onChange: (value: T) => void;
  options: { value: T; label: ReactNode }[];
  ariaLabel?: string;
  /** Layout modifier, e.g. `segment-compact` for three short labels. */
  className?: string;
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
    <div
      className={className ? `segment ${className}` : "segment"}
      role="radiogroup"
      aria-label={ariaLabel}
    >
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
