/**
 * Two or three mutually exclusive choices, as a quiet segmented control: the
 * active segment is marked by a two-pixel rule under it and nothing else —
 * the professional-application switch, not the web-form dropdown.
 *
 * The radiogroup behaviour is Radix's (@radix-ui/react-toggle-group,
 * type="single"): `role="radiogroup"` with `role="radio"` children, a roving
 * tabindex so the group is one stop in the tab sequence — landing on the
 * selected option, or the first one when nothing is selected — and arrow keys
 * that move and select. A group that announced "radio" without honouring the
 * pattern was worse than saying nothing.
 */
import type { ReactNode } from "react";
import * as ToggleGroupPrimitive from "@radix-ui/react-toggle-group";

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
  return (
    <ToggleGroupPrimitive.Root
      type="single"
      className={className ? `segment ${className}` : "segment"}
      aria-label={ariaLabel}
      value={value}
      onValueChange={(next) => {
        // Radix deselects on a second click; a segmented control with no
        // choice made is not a state this app has, so an empty value is
        // ignored and the selection stays.
        if (next) onChange(next as T);
      }}
    >
      {options.map((option) => (
        <ToggleGroupPrimitive.Item
          key={option.value}
          value={option.value}
          className="segment-item"
          // The theme compiler keys the segment indicator on this attribute.
          data-active={option.value === value}
        >
          {option.label}
        </ToggleGroupPrimitive.Item>
      ))}
    </ToggleGroupPrimitive.Root>
  );
}
