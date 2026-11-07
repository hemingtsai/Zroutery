/** The route pool: one tier's draggable row strip, shared with the simple home. */
import { useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { modelName, type ModelRow, type ModelTier } from "../../api";
import { useI18n } from "../../i18n";
import { Button, Disclosure, StatusDot } from "../../components";

const SHOWN_ROUTES = 4;
export function RoutePool({
  tier,
  members,
  firstLabel,
  cooling,
  providerName,
  busy,
  onReorder,
}: {
  /** Names the pool in reorder writes; absent together with `onReorder` only. */
  tier?: ModelTier;
  members: ModelRow[];
  firstLabel: ReactNode;
  cooling: Set<string>;
  providerName: (providerId: string) => string | undefined;
  busy?: boolean;
  /**
   * Turns the pool editable. The auto-review preview leaves it off — its
   * editing home is the drawer, which carries the priority numbers and the
   * enable checkboxes this row cannot show.
   */
  onReorder?: (
    tier: ModelTier,
    order: { provider_id: string; upstream_model: string }[],
  ) => void;
}) {
  const { t } = useI18n();
  const head = members.slice(0, SHOWN_ROUTES);
  const tail = members.slice(SHOWN_ROUTES);

  /*
   * The pool was read-only text until the tier order became editable in place.
   * Two input paths share one writer, because they must not be able to
   * disagree: a pointer drag, and the arrow buttons a keyboard or screen
   * reader actually drives. Both hand `onReorder` the full desired order; the
   * page turns that into a priority rewrite (applyTierOrder), so the numbers
   * on the Models page and the order here can never drift apart.
   *
   * During a drag the DOM keeps the original order and only paints the intent
   * (a dimmed row, a rule where the drop would land). Reordering the array
   * live would redraw mid-gesture from a snapshot that the next save could
   * invalidate; on drop the array moves once, through a save, and a cancelled
   * drag is exactly a no-op.
   */
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  const rowRefs = useRef<(HTMLElement | null)[]>([]);
  const reorder = onReorder;
  const interactive = members.length > 1 && !busy && reorder !== undefined;

  // `to` is an insertion index over the original array. Dropping before the
  // dragged row itself, or right after it, is the order it already has.
  const orderAfter = (from: number, to: number) => {
    const sequence = members.map((_, i) => i).filter((i) => i !== from);
    sequence.splice(to > from ? to - 1 : to, 0, from);
    return sequence.map((i) => ({
      provider_id: members[i].model.provider_id,
      upstream_model: members[i].model.upstream_model,
    }));
  };

  const swap = (from: number, delta: -1 | 1) => {
    const to = from + delta;
    if (to < 0 || to >= members.length) return;
    const sequence = members.map((_, i) => i);
    [sequence[from], sequence[to]] = [sequence[to], sequence[from]];
    if (!reorder || tier === undefined) return;
    reorder(
      tier,
      sequence.map((i) => ({
        provider_id: members[i].model.provider_id,
        upstream_model: members[i].model.upstream_model,
      })),
    );
  };

  // Which row's top edge the pointer is above. Rows render in index order for
  // the whole gesture, so rect order is index order and the midpoint scan is
  // the whole answer.
  const hitIndex = (y: number) => {
    for (let i = 0; i < members.length; i++) {
      const el = rowRefs.current[i];
      if (!el) continue;
      const rect = el.getBoundingClientRect();
      if (y < rect.top + rect.height / 2) return i;
    }
    return members.length;
  };

  const startDrag = (from: number) => (e: ReactPointerEvent) => {
    if (!interactive || e.button !== 0) return;
    const startY = e.clientY;
    // A drag begins only after the pointer commits to moving; a click (or the
    // press that starts a button click on the arrows) never becomes one.
    let started = false;
    const move = (ev: PointerEvent) => {
      if (!started) {
        if (Math.abs(ev.clientY - startY) < 4) return;
        started = true;
        setDragFrom(from);
      }
      ev.preventDefault();
      setDropAt(hitIndex(ev.clientY));
    };
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (!started) return;
      const to = hitIndex(ev.clientY);
      setDragFrom(null);
      setDropAt(null);
      if ((to !== from && to !== from + 1) && reorder && tier !== undefined) {
        reorder(tier, orderAfter(from, to));
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  // A line that would leave the order unchanged is not shown: the absence of
  // the rule is the honest "this drop does nothing".
  const dropLine =
    dragFrom !== null && dropAt !== null && dropAt !== dragFrom && dropAt !== dragFrom + 1
      ? dropAt
      : null;

  const line = (r: ModelRow, index: number) => (
    <div
      className={[
        "flow-route",
        interactive ? "reorderable" : "",
        dragFrom === index ? "dragging" : "",
        dropLine === index ? "drop-before" : "",
        dropLine === members.length && index === members.length - 1 ? "drop-after" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      key={r.id}
      onPointerDown={startDrag(index)}
      ref={(el) => {
        rowRefs.current[index] = el;
      }}
      title={interactive ? t("routing.drag_hint") : undefined}
    >
      <span className={index === 0 ? "flow-src" : "flow-src muted"}>
        {index === 0 ? firstLabel : t("routing.fallback_n", { n: index })}
      </span>
      <span className="flow-arrow" aria-hidden>{"→"}</span>
      <span className="flow-target">
        <StatusDot
          tone={cooling.has(r.id) ? "warn" : "ok"}
          state={cooling.has(r.id) ? t("status.cooling") : undefined}
        />
        {modelName(r.model)}
        <span className="flow-provider">{providerName(r.model.provider_id)}</span>
      </span>
      {interactive && (
        <span className="row-move">
          <Button
            kind="ghost"
            ariaLabel={t("routing.move_up", { model: modelName(r.model) })}
            disabled={index === 0}
            onClick={() => swap(index, -1)}
          >
            ↑
          </Button>
          <Button
            kind="ghost"
            ariaLabel={t("routing.move_down", { model: modelName(r.model) })}
            disabled={index === members.length - 1}
            onClick={() => swap(index, 1)}
          >
            ↓
          </Button>
        </span>
      )}
    </div>
  );
  return (
    <>
      {head.map(line)}
      {tail.length > 0 && (
        <Disclosure title={t("routing.more_fallbacks", { n: tail.length })}>
          {tail.map((r, i) => line(r, i + SHOWN_ROUTES))}
        </Disclosure>
      )}
    </>
  );
}

/**
 * How Default routing is configured. Strategy, failover, and what happens to
 * a model id nobody recognises.
 */
