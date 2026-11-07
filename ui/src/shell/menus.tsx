/**
 * The two main-bar menus: the gateway's state and actions, and the theme
 * choice. Both are popovers — global, infrequent controls, the desktop
 * pattern — and both were inline in the shell until the shell got too wide.
 */
import { api, errorText, gatewayAction, type Snapshot } from "../api";
import type { SaveTask, TaskKind } from "../saveQueue";
import { MenuItem, Popover, StatusDot, useToast } from "../components";
import { useI18n } from "../i18n";
import type { ThemePref } from "../theme";

type ServerState = Snapshot["server"];
type Run = (task: SaveTask, kind?: TaskKind) => Promise<boolean>;

/**
 * Start or stop the gateway from the state Rust reports right now.
 *
 * Deciding from the rendered `server.running` would run the wrong command
 * whenever the tray changed the state first, so the current status is loaded
 * and the command chosen from that. Lives beside the menu that invokes it.
 */
export async function toggleGateway(
  busy: boolean,
  run: Run,
  onError: (message: string) => void,
  applySnapshot: (snapshot: Snapshot) => void,
): Promise<void> {
  if (busy) return;
  let fresh: Snapshot;
  try {
    fresh = await api.snapshot();
  } catch (e) {
    onError(errorText(e));
    return;
  }
  applySnapshot(fresh);
  // Starting or stopping the gateway is a runtime action, not a
  // configuration write: the status dot and the menu answer it, and the save
  // vocabulary says nothing.
  await run(gatewayAction(fresh.server) === "start" ? api.start : api.stop, "observe");
}

export function GatewayMenu({
  server,
  onToggle,
}: {
  server: ServerState;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  return (
    <Popover
      title={t("settings.gateway")}
      trigger={
        <>
          <StatusDot tone={server.running ? "ok" : "danger"} />
          {/*
            * The dot is the state, and the dot is a colour. Without this
            * the button announced itself as "Gateway" and said nothing
            * about whether the proxy was running, which is the one
            * thing a person opening this menu needs to know.
            */}
          <span className="visually-hidden">
            {server.running ? t("status.running") : t("status.stopped")}
          </span>
          {t("settings.gateway")}
        </>
      }
    >
      {(close) => (
        <>
          {/*
            * A menu may only contain menu items, groups and
            * separators. The status rows and the rule are content, not
            * items, and saying so is what keeps the menu's structure
            * one a screen reader can walk.
            */}
          <div className="menu-row" role="none">
            <StatusDot tone={server.running ? "ok" : "danger"} />
            {t(server.running ? "status.running" : "status.stopped")}
          </div>
          {server.running && (
            <div className="menu-row" role="none">
              <span className="menu-key">{t("gw.address")}</span>
              <span className="mono">
                {server.host}:{server.port}
              </span>
            </div>
          )}
          <div className="menu-sep" role="separator" />
          <MenuItem
            onClick={() => {
              void api.copy(`http://${server.host}:${server.port}`).then(() =>
                notify("ok", t("toast.copied")),
              );
              close();
            }}
          >
            {t("gw.copy_address")}
          </MenuItem>
          <MenuItem
            onClick={() => {
              void api.copyToken().then(() => notify("ok", t("toast.copied")));
              close();
            }}
          >
            {t("action.copy_token")}
          </MenuItem>
          <div className="menu-sep" />
          <MenuItem
            onClick={() => {
              onToggle();
              close();
            }}
          >
            {t(server.running ? "gw.stop" : "gw.start")}
          </MenuItem>
        </>
      )}
    </Popover>
  );
}

export function AppearanceMenu({
  themePref,
  onThemePref,
}: {
  themePref: ThemePref;
  onThemePref: (next: ThemePref) => void;
}) {
  const { t } = useI18n();
  return (
    <Popover
      title={t("appearance.title")}
      ariaLabel={t("appearance.title")}
      trigger={
        <span aria-hidden className="glyph">
          {"◐"}
        </span>
      }
    >
      {(close) => (
        <>
          <MenuItem
            active={themePref === "system"}
            onClick={() => {
              onThemePref("system");
              close();
            }}
          >
            {t("theme.system")}
          </MenuItem>
          <MenuItem
            active={themePref === "light"}
            onClick={() => {
              onThemePref("light");
              close();
            }}
          >
            {t("theme.light")}
          </MenuItem>
          <MenuItem
            active={themePref === "dark"}
            onClick={() => {
              onThemePref("dark");
              close();
            }}
          >
            {t("theme.dark")}
          </MenuItem>
        </>
      )}
    </Popover>
  );
}
