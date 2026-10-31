import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { compactCount, type UsageDay } from "../api";
import { Segment, num } from "../components";
import { useI18n } from "../i18n";

/**
 * The usage history as a picture.
 *
 * Three shapes of the same series, because they answer different questions:
 * the heatmap answers "when do I use this", the bars "how much each day", and
 * the line "is it going up". They share one measurement and one window, so
 * switching between them is a change of view and never of data.
 *
 * Hand-drawn SVG on purpose: the page already ships no charting library, and a
 * dependency that draws three static series would be larger than the rest of
 * the dashboard's runtime.
 */

/** One year of weeks and a day, so the heatmap's last column is complete. */
export const HEATMAP_DAYS = 371;

type ChartStyle = "heatmap" | "bars" | "line";

/**
 * The measures the chart can draw, one per KPI that has a daily series.
 *
 * The KPI row above this chart shows five figures, but only two of them exist
 * per day: the durable ledger buckets days by requests, failures and tokens,
 * so cost and cache reads are `StatsSummary` totals with no `UsageDay` column.
 * A "cost" or "cache hit" tier here would have to draw a series the ledger
 * does not keep, so the metric deliberately has no third tier — see the KPI
 * row in Activity.tsx, which makes the same call from its side of the wire.
 *
 * The metric is a controlled prop rather than local state for the same reason:
 * the KPI cells are the chart's gears, and one owner (Activity) keeps the
 * segment and the row from disagreeing about which measure is on.
 */
export type Metric = "requests" | "tokens";
/** Segment values are strings; the number is parsed where the window is cut. */
type RangeKey = "7" | "30" | "90" | "365";

/** A day under the pointer, and where the pointer is, in viewport pixels. */
type ChartHover = { day: UsageDay; x: number; y: number };

/** The one signature all three drawings use to feed the hover card. */
type OnHover = (day: UsageDay, x: number, y: number) => void;

/*
 * The viewport, read at module scope because the component below shadows
 * `window` with the chart's date window.
 */
const viewport = () => ({ width: window.innerWidth, height: window.innerHeight });

const measure = (day: UsageDay, metric: Metric): number =>
  metric === "requests" ? day.requests : day.input_tokens + day.output_tokens;

/** `2026-10-29` as local midnight, not as UTC midnight. */
function parseDay(day: string): Date {
  const [year, month, date] = day.split("-").map(Number);
  return new Date(year, month - 1, date);
}

/** A round axis maximum: 1, 2 or 5 times a power of ten. */
function niceMax(max: number): number {
  if (max <= 0) return 1;
  const magnitude = 10 ** Math.floor(Math.log10(max));
  const normalized = max / magnitude;
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return step * magnitude;
}

/**
 * Five shades cut on the non-zero quartiles.
 *
 * Equal fractions of the maximum would paint a year with one busy day almost
 * uniformly faint; quartiles of the days that saw traffic keep the shape of
 * the ordinary week visible.
 */
function heatLevels(values: number[]): (value: number) => 0 | 1 | 2 | 3 | 4 {
  const nonzero = values.filter((value) => value > 0).sort((a, b) => a - b);
  if (nonzero.length === 0) return () => 0;
  const at = (share: number) =>
    nonzero[Math.min(nonzero.length - 1, Math.floor(share * nonzero.length))];
  const first = at(0.25);
  const middle = at(0.5);
  const last = at(0.75);
  if (first === last) return (value) => (value === 0 ? 0 : 3);
  return (value) => {
    if (value === 0) return 0;
    if (value <= first) return 1;
    if (value <= middle) return 2;
    if (value <= last) return 3;
    return 4;
  };
}

export default function UsageCharts({
  days,
  metric,
  onMetricChange,
}: {
  days: UsageDay[] | null;
  /** The measure in force, owned by the page so the KPI row and this segment agree. */
  metric: Metric;
  onMetricChange: (metric: Metric) => void;
}) {
  const { t } = useI18n();
  const [style, setStyle] = useState<ChartStyle>("heatmap");
  const [range, setRange] = useState<RangeKey>("30");

  /*
   * The hover card, as one state: which day is under the pointer and where
   * the pointer is. A single singleton element renders from it, so a year of
   * cells costs one div however fast the pointer moves.
   *
   * It replaces the native SVG <title>. The system grey box takes about a
   * second to appear and offers no typesetting — one plain line in the
   * browser's font, while the rest of this panel sets its readings in mono
   * figures. (The comparison in mind is CC Switch's floating card, which
   * reads the way this card does.)
   *
   * Keyboard: the cells stay unfocusable, as they were under the title. Tab
   * stops on up to 371 read-only rects would flood the order, and the svg's
   * aria-label already carries the series summary, so exact per-day readings
   * remain pointer-only — the same reach the native title had.
   */
  const [hover, setHover] = useState<ChartHover | null>(null);
  const cardRef = useRef<HTMLDivElement | null>(null);

  /*
   * Where the card goes: below-right of the pointer by preference, flipping
   * to the other side when that corner would leave the viewport. The card's
   * size is measured off the mounted element (its width follows the widest
   * row) and written straight to it inside a layout effect, so the flip lands
   * before the paint instead of a frame after; the inline style on the card
   * holds the same preference unclamped, for the first render to refine.
   */
  useLayoutEffect(() => {
    const el = cardRef.current;
    if (!hover || !el) return;
    const { width, height } = viewport();
    const margin = 8;
    let left = hover.x + 12;
    let top = hover.y + 14;
    if (left + el.offsetWidth > width - margin) left = hover.x - el.offsetWidth - 12;
    if (top + el.offsetHeight > height - margin) top = hover.y - el.offsetHeight - 12;
    el.style.left = `${Math.max(margin, left)}px`;
    el.style.top = `${Math.max(margin, top)}px`;
  }, [hover]);

  const onHover: OnHover = (day, x, y) => setHover({ day, x, y });

  /*
   * One window rule for every chart style. The heatmap used to be pinned to a
   * full year regardless of the range selector — which painted 366 grey
   * squares under a two-day-old install and made the most common first look
   * at this page an exercise in finding the two dark cells. The grid columns
   * derive from the window's length either way, so a young install now draws
   * five columns instead of a wall of nothing.
   */
  const window = useMemo(() => {
    if (!days) return null;
    return days.slice(-Number(range));
  }, [days, range]);

  const weeks = useMemo(
    () => (window && style === "heatmap" ? heatmapWeeks(window) : null),
    [window, style],
  );

  const total = window ? window.reduce((sum, day) => sum + measure(day, metric), 0) : 0;
  const quiet = window !== null && total === 0;
  const canvas = chartSize(style, weeks?.length ?? 0);
  const metricLabel = metric === "requests" ? t("activity.metric_requests") : t("activity.metric_tokens");

  return (
    <div className="usage-charts">
      <div className="chart-toolbar">
        <Segment<ChartStyle>
          ariaLabel={t("activity.chart_style")}
          value={style}
          onChange={setStyle}
          options={[
            { value: "heatmap", label: t("activity.chart_heatmap") },
            { value: "bars", label: t("activity.chart_bars") },
            { value: "line", label: t("activity.chart_line") },
          ]}
        />
        <Segment<Metric>
          ariaLabel={t("activity.chart_metric")}
          value={metric}
          onChange={onMetricChange}
          options={[
            { value: "requests", label: t("activity.metric_requests") },
            { value: "tokens", label: t("activity.metric_tokens") },
          ]}
        />
        <Segment<RangeKey>
          ariaLabel={t("activity.chart_range")}
          value={range}
          onChange={setRange}
          options={[
            { value: "7", label: t("activity.range_7") },
            { value: "30", label: t("activity.range_30") },
            { value: "90", label: t("activity.range_90") },
            { value: "365", label: t("activity.range_365") },
          ]}
        />
      </div>

      {window === null ? (
        <p className="empty">{t("common.loading")}</p>
      ) : quiet ? (
        <p className="empty">{t("activity.chart_none")}</p>
      ) : (
        <figure className="chart-figure">
          <svg
            className="usage-chart"
            width={canvas.width}
            height={canvas.height}
            viewBox={`0 0 ${canvas.width} ${canvas.height}`}
            role="img"
            aria-label={t("activity.chart_aria", {
              metric: metricLabel,
              days: window.length,
              total: num(total),
            })}
            onMouseLeave={() => setHover(null)}
          >
            {weeks ? (
              <Heatmap weeks={weeks} metric={metric} onHover={onHover} />
            ) : style === "bars" ? (
              <Bars days={window} metric={metric} onHover={onHover} />
            ) : (
              <Line days={window} metric={metric} onHover={onHover} />
            )}
          </svg>
          {style === "heatmap" && (
            <figcaption className="chart-legend">
              <span>{t("activity.heat_less")}</span>
              {[0, 1, 2, 3, 4].map((level) => (
                <span key={level} className="chart-legend-cell" data-level={level} />
              ))}
              <span>{t("activity.heat_more")}</span>
            </figcaption>
          )}
        </figure>
      )}

      {/*
       * The card, rendered only while a chart is on the screen: a counter
       * reset can empty the window under a stationary pointer, and the card
       * must not outlive the cells that fed it. The rows repeat the day's
       * two readings with the measure the chart is drawing in full ink; the
       * labels are the same words the measure segment above uses.
       *
       * The portal is load-bearing: the panels around this chart carry the
       * page-in entrance animation, and a transformed ancestor turns
       * `position: fixed` into panel-relative positioning — the card would
       * land hundreds of pixels from the pointer (and the flip below would
       * measure the wrong space). `document.body` is the one transform-free
       * ancestor, the same place the toasts mount from.
       */}
      {window !== null &&
        !quiet &&
        hover !== null &&
        createPortal(
          <div
            className="chart-card"
            ref={cardRef}
            aria-hidden="true"
            style={{ left: hover.x + 12, top: hover.y + 14 }}
          >
            <div className="chart-card-day">{hover.day.day}</div>
            <div className="chart-card-row" data-active={metric === "requests" ? "" : undefined}>
              <span>{t("activity.metric_requests")}</span>
              <span className="chart-card-value">{num(hover.day.requests)}</span>
            </div>
            <div className="chart-card-row" data-active={metric === "tokens" ? "" : undefined}>
              <span>{t("activity.metric_tokens")}</span>
              <span className="chart-card-value">
                {num(hover.day.input_tokens + hover.day.output_tokens)}
              </span>
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}

/** The one place a layout's canvas is named, so the frame and the drawing agree. */
function chartSize(style: ChartStyle, weeks: number): { width: number; height: number } {
  if (style === "heatmap") {
    return {
      width: HEATMAP_LEFT + weeks * STEP - GAP,
      height: HEATMAP_TOP + 7 * STEP - GAP,
    };
  }
  return { width: PLOT_WIDTH, height: PLOT_HEIGHT };
}

const CELL = 11;
const GAP = 3;
const STEP = CELL + GAP;
const HEATMAP_LEFT = 34;
const HEATMAP_TOP = 16;

/**
 * The days as Monday-first columns.
 *
 * The first column is padded so every row is one weekday; without it the grid
 * would shift by a row depending on which day of the week the window ended,
 * and the labels down the left would name the wrong days.
 */
function heatmapWeeks(days: UsageDay[]): (UsageDay | null)[][] {
  // An empty window — no traffic since the counters were reset — has no first
  // day to anchor the weekday offset on. The grid is then nothing at all,
  // which is the honest picture of "no day has requests".
  if (days.length === 0) return [];
  const weekday = (parseDay(days[0].day).getDay() + 6) % 7;
  const cells: (UsageDay | null)[] = [
    ...Array.from({ length: weekday }, () => null),
    ...days,
  ];
  const weeks: (UsageDay | null)[][] = [];
  for (let index = 0; index < cells.length; index += 7) {
    weeks.push(cells.slice(index, index + 7));
  }
  return weeks;
}

const PLOT_WIDTH = 760;
const PLOT_HEIGHT = 190;
const PLOT_LEFT = 44;
const PLOT_RIGHT = 8;
const PLOT_TOP = 10;
const PLOT_BOTTOM = 22;

/** The Monday-first year grid, shaded by the day's quartile. */
function Heatmap({
  weeks,
  metric,
  onHover,
}: {
  weeks: (UsageDay | null)[][];
  metric: Metric;
  onHover: OnHover;
}) {
  const { lang } = useI18n();
  const days = weeks.flat().filter((day): day is UsageDay => day !== null);
  const values = days.map((day) => measure(day, metric));
  const level = heatLevels(values);

  const months = new Intl.DateTimeFormat(lang, { month: "short" });
  const weekdays = new Intl.DateTimeFormat(lang, { weekday: "short" });
  // 2024-01-01 was a Monday, which is the reference the padded rows need.
  const monday = new Date(2024, 0, 1);

  const monthLabels: { x: number; text: string }[] = [];
  let lastMonth = -1;
  weeks.forEach((week, column) => {
    const first = week.find((day): day is UsageDay => day !== null);
    if (!first) return;
    const date = parseDay(first.day);
    if (date.getMonth() === lastMonth) return;
    // A month starting in the last column or two would run off the canvas.
    if (column < weeks.length - 1) {
      monthLabels.push({ x: HEATMAP_LEFT + column * STEP, text: months.format(date) });
    }
    lastMonth = date.getMonth();
  });

  return (
    <>
      {monthLabels.map((label) => (
        <text key={`${label.x}-${label.text}`} className="chart-label" x={label.x} y={10}>
          {label.text}
        </text>
      ))}
      {[0, 2, 4].map((row) => (
        <text
          key={row}
          className="chart-label"
          x={0}
          y={HEATMAP_TOP + row * STEP + CELL / 2}
          dominantBaseline="middle"
        >
          {weekdays.format(new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + row))}
        </text>
      ))}
      {weeks.map((week, column) =>
        week.map((day, row) => {
          if (!day) return null;
          const value = measure(day, metric);
          return (
            <rect
              key={day.day}
              x={HEATMAP_LEFT + column * STEP}
              y={HEATMAP_TOP + row * STEP}
              width={CELL}
              height={CELL}
              data-level={level(value)}
              onMouseMove={(event) => onHover(day, event.clientX, event.clientY)}
            />
          );
        }),
      )}
    </>
  );
}

/** One bar per day over the selected window. */
function Bars({
  days,
  metric,
  onHover,
}: {
  days: UsageDay[];
  metric: Metric;
  onHover: OnHover;
}) {
  const values = days.map((day) => measure(day, metric));
  const top = niceMax(Math.max(...values));
  const plotWidth = PLOT_WIDTH - PLOT_LEFT - PLOT_RIGHT;
  const plotHeight = PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM;
  const slot = plotWidth / days.length;
  const barWidth = Math.max(1, slot - (days.length > 30 ? 1 : 3));

  return (
    <>
      <Axis top={top} plotHeight={plotHeight} />
      {/*
       * A transparent column per day is the hover target — the Line's hit-rect
       * precedent, with one deliberate difference of order. These sit BENEATH
       * the bars rather than over them, because the visible bar carries a CSS
       * `:hover` highlight and a column rect painted on top would eclipse the
       * pointer and kill the highlight wherever the bar is wide enough to have
       * one. Beneath, the bar still wins where it exists (and keeps its own
       * handler), and the column answers everywhere else — so a >30-day
       * window, whose shrink step narrows the bars to 1px, regains a target
       * the width of its slot, and the hover card reads every day again. The
       * data fed to the card is unchanged: same day, same readings.
       */}
      {days.map((day, index) => (
        <rect
          key={`hit-${day.day}`}
          x={PLOT_LEFT + index * slot}
          y={PLOT_TOP}
          width={slot}
          height={plotHeight}
          fill="transparent"
          onMouseMove={(event) => onHover(day, event.clientX, event.clientY)}
        />
      ))}
      {days.map((day, index) => {
        const value = measure(day, metric);
        const height = (value / top) * plotHeight;
        return (
          <rect
            key={day.day}
            className="chart-bar"
            x={PLOT_LEFT + index * slot + (slot - barWidth) / 2}
            y={PLOT_TOP + plotHeight - height}
            width={barWidth}
            height={Math.max(value > 0 ? 1 : 0, height)}
            onMouseMove={(event) => onHover(day, event.clientX, event.clientY)}
          />
        );
      })}
      <DayAxis days={days} slot={slot} plotHeight={plotHeight} />
    </>
  );
}

/** The same window as a line, so the direction is the point. */
function Line({
  days,
  metric,
  onHover,
}: {
  days: UsageDay[];
  metric: Metric;
  onHover: OnHover;
}) {
  const values = days.map((day) => measure(day, metric));
  const top = niceMax(Math.max(...values));
  const plotWidth = PLOT_WIDTH - PLOT_LEFT - PLOT_RIGHT;
  const plotHeight = PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM;
  const slot = plotWidth / days.length;
  const x = (index: number) => PLOT_LEFT + index * slot + slot / 2;
  const y = (value: number) => PLOT_TOP + plotHeight - (value / top) * plotHeight;
  const points = values.map((value, index) => `${x(index).toFixed(1)},${y(value).toFixed(1)}`);
  const floor = PLOT_TOP + plotHeight;

  return (
    <>
      <Axis top={top} plotHeight={plotHeight} />
      <path
        className="chart-area"
        d={`M${x(0).toFixed(1)},${floor} L${points.join(" L")} L${x(values.length - 1).toFixed(1)},${floor} Z`}
      />
      <polyline className="chart-line" points={points.join(" ")} />
      {days.length <= 30 &&
        values.map((value, index) => (
          <circle key={days[index].day} className="chart-dot" cx={x(index)} cy={y(value)} r={2} />
        ))}
      {/*
       * A transparent column per day is the hover target. A zero-radius dot
       * cannot be pointed at, and a two-pixel one is a difficult target even
       * when it is the thing the reader is aiming for.
       */}
      {days.map((day, index) => (
        <rect
          key={`hit-${day.day}`}
          x={PLOT_LEFT + index * slot}
          y={PLOT_TOP}
          width={slot}
          height={plotHeight}
          fill="transparent"
          onMouseMove={(event) => onHover(day, event.clientX, event.clientY)}
        />
      ))}
      <DayAxis days={days} slot={slot} plotHeight={plotHeight} />
    </>
  );
}

/** Three horizontal lines and their figures, shared by both plot layouts. */
function Axis({ top, plotHeight }: { top: number; plotHeight: number }) {
  const lines = [0, 0.5, 1];
  return (
    <>
      {lines.map((share) => {
        const y = PLOT_TOP + plotHeight - share * plotHeight;
        return (
          <g key={share}>
            <line
              className="chart-grid"
              x1={PLOT_LEFT}
              y1={y}
              x2={PLOT_WIDTH - PLOT_RIGHT}
              y2={y}
            />
            <text className="chart-label" x={PLOT_LEFT - 6} y={y} textAnchor="end" dominantBaseline="middle">
              {compactCount(top * share)}
            </text>
          </g>
        );
      })}
    </>
  );
}

/** Date labels under the plot, spaced so they never collide. */
function DayAxis({ days, slot, plotHeight }: { days: UsageDay[]; slot: number; plotHeight: number }) {
  const step = Math.max(1, Math.ceil(days.length / 6));
  return (
    <>
      {days.map((day, index) => {
        const last = index === days.length - 1;
        if (index % step !== 0 && !last) return null;
        // The final label would sit on top of the previous one when the step
        // does not divide the window; the week before it is close enough.
        if (last && days.length % step !== 0 && step > 1 && days.length > 10) {
          const previous = index - (days.length % step);
          if (index - previous < step / 2) return null;
        }
        return (
          <text
            key={day.day}
            className="chart-label"
            x={PLOT_LEFT + index * slot + slot / 2}
            y={PLOT_TOP + plotHeight + 15}
            textAnchor="middle"
          >
            {day.day.slice(5)}
          </text>
        );
      })}
    </>
  );
}
