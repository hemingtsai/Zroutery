/**
 * A group of panels behind one strip of tab headers, for a page section that
 * holds more than the eye should track at once.
 *
 * Built on the `segment` look so it reads as the same control as the segmented
 * choices above, but it is a real tablist rather than a radio group. The
 * tablist behaviour — roving tabindex, Left/Right/Home/End moving focus with
 * the selection, panels named by their headers — is Radix's
 * (@radix-ui/react-tabs); the active panel is the only one mounted, as before.
 *
 * `Segment` stays a radio group on purpose: choosing between options is one
 * decision, and this is a change of subject.
 */
import type { ReactNode } from "react";
import * as TabsPrimitive from "@radix-ui/react-tabs";

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
  return (
    <TabsPrimitive.Root className="tabs" value={value} onValueChange={onChange}>
      <TabsPrimitive.List className="segment" aria-label={ariaLabel}>
        {tabs.map((tab) => (
          <TabsPrimitive.Trigger
            key={tab.value}
            value={tab.value}
            className="segment-item"
            // The layout/interaction harnesses address tabs and panels by
            // value; Radix computes its own ids and takes no override, so the
            // stable hooks are data attributes instead of element ids.
            data-tab={tab.value}
            // The theme compiler keys the segment indicator on this attribute.
            data-active={tab.value === value}
          >
            {tab.label}
          </TabsPrimitive.Trigger>
        ))}
      </TabsPrimitive.List>
      {tabs.map((tab) => (
        <TabsPrimitive.Content
          key={tab.value}
          value={tab.value}
          className="tab-panel"
          tabIndex={0}
          data-panel={tab.value}
        >
          {tab.content}
        </TabsPrimitive.Content>
      ))}
    </TabsPrimitive.Root>
  );
}
