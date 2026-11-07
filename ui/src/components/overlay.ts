/**
 * The open modal overlays, innermost last.
 *
 * Two modal surfaces can be open at once — a confirmation raised from inside a
 * drawer — and each of them pulls the focus back when it lands outside itself.
 * With two roots that are not nested, that is a fight: the confirm focuses its
 * button, the drawer pulls it back, the confirm pulls it again, and the
 * recursion only ends when the stack does.
 *
 * The stack says which one is on top, and a guard that is not the top stands
 * down. That is the whole model, and it does not depend on the two surfaces
 * being nested in the DOM — which they are not, and must not be: a
 * confirmation raised from a drawer is rendered by the page, and moving it
 * into the drawer to make the DOM agree would couple two components that have
 * no reason to know about each other.
 *
 * Module-private to the components directory: the stack is the one piece of
 * mutable shared state the drawer and the confirm dialog have, and both sides
 * import it from here rather than owning a copy.
 */
const overlayStack: symbol[] = [];

export function pushOverlay(id: symbol) {
  overlayStack.push(id);
}

export function popOverlay(id: symbol) {
  const at = overlayStack.lastIndexOf(id);
  if (at >= 0) overlayStack.splice(at, 1);
}

/** The overlay currently on top, if any. */
export function topOverlay(): symbol | undefined {
  return overlayStack[overlayStack.length - 1];
}

/**
 * The focus guard every modal surface shares.
 *
 * Called with the surface's own root. The surface claims the focus only when
 * it is the top overlay: a modal above it is responsible for the focus, and
 * the surface below must not pull it back.
 */
export function claimFocus(root: HTMLElement | null, id: symbol, event: FocusEvent) {
  if (!root) return;
  if (topOverlay() !== id) return;
  const target = event.target;
  if (!(target instanceof Node)) return;
  // A portalled overlay — a select's menu, a popover — is not this surface's
  // to own. Radix renders one outside the surface's own root, and it manages
  // the focus while it is open. A surface that pulled the focus back out of it
  // would make the overlay unusable: the arrow key would move the selection,
  // the focus would be pulled away, and the menu would sit open with the
  // keyboard pointing at nothing.
  if (isInsidePortalledOverlay(target)) return;
  // The nearest dialog ancestor of the focus target owns it. If that is not
  // us, the focus belongs to a modal we are not.
  const owner =
    target instanceof Element
      ? target.closest('[role="dialog"],[role="alertdialog"]')
      : null;
  if (owner && owner !== root) return;
  if (!root.contains(target)) root.focus();
}

/**
 * Whether a focus target is inside an overlay Radix portalled to the document.
 *
 * The marker is Radix's own: the popper's content wrapper carries
 * `data-radix-popper-content-wrapper`, and it is the one attribute that says
 * "this element is somewhere else on the page, on purpose".
 */
export function isInsidePortalledOverlay(target: Node): boolean {
  return (
    target instanceof Element &&
    target.closest("[data-radix-popper-content-wrapper]") !== null
  );
}
