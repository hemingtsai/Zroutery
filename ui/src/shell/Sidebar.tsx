/** The left navigation column: the brand, the six doors, nothing else. */
import { useI18n } from "../i18n";
import type { Snapshot } from "../api";
import type { Page } from "../navigation";
import { NAV } from "./nav";
import { NAV_ICONS } from "./navIcons";

export function Sidebar({
  snapshot,
  taskPage,
  unclassified,
  onNavigate,
}: {
  snapshot: Snapshot;
  taskPage: string;
  unclassified: unknown[];
  onNavigate: (page: Page) => void;
}) {
  const { t } = useI18n();
  return (
    <nav className="sidebar" aria-label={t("nav.main_aria")}>
      <div className="sidebar-brand">
        Zroutery<span className="mono">v{snapshot.version}</span>
      </div>

      {NAV.map((item, index) => (
        <button type="button"
          key={item.id}
          className={`nav-item ${taskPage === item.id ? "active" : ""}`}
          // Named as well as classed. The two layout harnesses drive the
          // shell with real clicks, and an ordinal is a promise that breaks
          // the moment a page is inserted: adding one silently repoints every
          // later index at a different page, which measures a healthy surface
          // and reports it as a pass. `data-page` survives the insert.
          data-page={item.id}
          // Which page is open is carried by the `active` class alone, and a
          // class is invisible to a screen reader: the nav read as eight
          // identical buttons with no indication of where you were.
          aria-current={taskPage === item.id ? "page" : undefined}
          // The shortcut is the nav's position, and the tooltip is the only
          // place a keyboard user can discover it without reading source.
          aria-keyshortcuts={`Alt+${index + 1}`}
          title={`Alt+${index + 1}`}
          onClick={() => onNavigate(item.id)}
        >
          <span className="nav-icon" aria-hidden="true">
            {NAV_ICONS[item.id]}
          </span>
          {t(item.labelKey)}
          {item.id === "models" && unclassified.length > 0 && (
            <span className="nav-count" title={t("nav.unclassified_title", { n: unclassified.length })}>
              {unclassified.length}
            </span>
          )}
        </button>
      ))}

      <div className="sidebar-spacer" />

      {/*
        * No mode switch here any more. It used to close the nav because the
        * mode decided how long this list was; since both modes keep the same
        * six doors (see the visibleNav note above) the switch is a plain
        * preference, and preferences belong in Settings — where the same
        * Segment already renders in the Appearance section for both modes.
        */}
    </nav>
  );
}
