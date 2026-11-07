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
import { useId, useState, type ReactNode } from "react";

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
