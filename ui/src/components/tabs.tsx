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
import type { ReactNode } from "react";

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
            // Only the selected tab points at a panel, because only the
            // selected tab has one. The other panels are not in the document
            // at all, and a reference to an element that is not there is a
            // reference to nothing.
            aria-controls={tab.value === value ? `panel-${tab.value}` : undefined}
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
