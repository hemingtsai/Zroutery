/**
 * Hand a freshly unfolded add form to the reader.
 *
 * An unfolded form lands wherever it is mounted, so the page effect that
 * opens one calls this with the element the form is mounted on: the landing
 * scrolls into the viewport — a no-op when the reader is already there,
 * which they are, because the head button they just pressed is only reachable
 * at the top of the page — and then the form's first input takes the focus,
 * so the next keystroke types into the form instead of hunting for a field
 * that opened below the fold. Scroll before focus, so the focus call's own
 * scroll-into-view only corrects the last few pixels.
 *
 * `firstControl` is a selector inside the landing, because which control is
 * first is the page's knowledge. A null landing means the form folded before
 * the effect ran — there is nothing left to hand over.
 *
 * Shared with the provider page's entry, which unfolds the same way, and
 * unit-tested from the catalogue page; whether the landing really intersects
 * the viewport is the interaction harness's question, which runs an actual
 * browser.
 */
export function revealAddFormLanding(
  landing: HTMLElement | null,
  firstControl: string,
): void {
  if (!landing) return;
  landing.scrollIntoView({ block: "nearest" });
  landing.querySelector<HTMLElement>(firstControl)?.focus();
}
