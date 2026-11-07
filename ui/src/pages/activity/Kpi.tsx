/**
 * One figure of the KPI row: a small muted label over a display-size value,
 * the row the usage page opens with. Whether a cell is a pure readout or a
 * chart control is the row's decision, not this one's.
 */

export function Kpi({
  label,
  value,
  title,
  help,
  selected,
  onSelect,
}: {
  label: string;
  value: string;
  /** The exact figure behind an abbreviated value, on hover. */
  title?: string;
  /** A formula note, rendered as a "?" beside the label. */
  help?: string;
  selected?: boolean;
  onSelect?: () => void;
}) {
  const body = (
    <>
      <span className="kpi-label">
        {label}
        {help && (
          // The "?" carries its own title, which also wins while the pointer
          // rests on it inside a titled cell — the formula is what a reader
          // parked on the mark is asking for, not the figure again. It stays
          // in the accessibility tree on purpose: the formula is content,
          // and a screen-reader user is the reader least likely to have it
          // already.
          <span className="kpi-help" title={help}>
            ?
          </span>
        )}
      </span>
      <span className="kpi-value">{value}</span>
    </>
  );
  if (onSelect === undefined) {
    return (
      <div className="kpi-cell" title={title}>
        {body}
      </div>
    );
  }
  return (
    <button
      type="button"
      className="kpi-cell kpi-trigger"
      aria-pressed={selected}
      title={title}
      onClick={onSelect}
    >
      {body}
    </button>
  );
}
