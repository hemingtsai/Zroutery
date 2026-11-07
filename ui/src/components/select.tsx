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
import { useId, type ReactNode } from "react";
import * as RadixSelect from "@radix-ui/react-select";

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
