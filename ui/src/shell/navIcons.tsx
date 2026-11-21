/**
 * The six nav icons, hand-drawn as inline SVG so every skin can show them
 * without a font or an icon-package dependency. Stroke paths in
 * `currentColor`: the icon inherits the nav item's ink, so skins style the
 * item and the icon follows. Industrial keeps its no-icon nav by hiding the
 * slot in CSS (`.nav-icon`); MD3's navigation drawer and Fluent's
 * NavigationView both expect a leading icon, and their packs unhide it.
 */
import type { JSX } from "react";

const stroke = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.75,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

function icon(paths: JSX.Element): JSX.Element {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" {...stroke}>
      {paths}
    </svg>
  );
}

export const NAV_ICONS: Record<string, JSX.Element> = {
  // Four quadrants: the overview is the whole board at a glance.
  overview: icon(
    <>
      <rect x="3.5" y="3.5" width="7" height="7" rx="1.5" />
      <rect x="13.5" y="3.5" width="7" height="7" rx="1.5" />
      <rect x="3.5" y="13.5" width="7" height="7" rx="1.5" />
      <rect x="13.5" y="13.5" width="7" height="7" rx="1.5" />
    </>,
  ),
  // A server rack: two bays with an indicator each.
  providers: icon(
    <>
      <rect x="3.5" y="4.5" width="17" height="6" rx="1.5" />
      <rect x="3.5" y="13.5" width="17" height="6" rx="1.5" />
      <path d="M7 7.5h.01M7 16.5h.01" strokeWidth="2.25" />
    </>,
  ),
  // Share-graph: one source fanning to two routes.
  models: icon(
    <>
      <circle cx="18" cy="5" r="2.5" />
      <circle cx="6" cy="12" r="2.5" />
      <circle cx="18" cy="19" r="2.5" />
      <path d="M8.2 10.8l7.6-4.6M8.2 13.2l7.6 4.6" />
    </>,
  ),
  // A desktop client with its stand.
  takeover: icon(
    <>
      <rect x="3" y="4.5" width="18" height="12" rx="1.5" />
      <path d="M9 20h6M12 16.5V20" />
    </>,
  ),
  // A metered column chart over a baseline.
  activity: icon(
    <>
      <path d="M4 19.5h16" />
      <path d="M7 19.5v-6M12 19.5V10M17 19.5V6.5" />
    </>,
  ),
  // Three slider tracks with beads: settings without a gear. Filled beads and
  // wide tracks survive the 18px raster — hairline strokes collapse to noise.
  settings: icon(
    <>
      <path d="M4 6.5h16M4 12h16M4 17.5h16" strokeWidth="2" />
      <circle cx="9" cy="6.5" r="2.3" fill="currentColor" stroke="none" />
      <circle cx="15.5" cy="12" r="2.3" fill="currentColor" stroke="none" />
      <circle cx="8" cy="17.5" r="2.3" fill="currentColor" stroke="none" />
    </>,
  ),
};
