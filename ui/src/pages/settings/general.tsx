/**
 * The General pane of the settings page: appearance (mode, language, theme,
 * skin), the window knobs, the balance fetch, and the About facts that only
 * remain for simple mode.
 *
 * Cut out of `pages/Settings.tsx` by subject when the page grew too large to
 * read as one file; the shell in `pages/Settings.tsx` renders this next to
 * the gateway pane.
 */
import { api, type AppConfig, type Snapshot } from "../../api";
import {
  Button,
  CommitNumber,
  Section,
  Segment,
  Select,
} from "../../components";
import { useI18n } from "../../i18n";
import type { UiMode } from "../../mode";
import type { Skin } from "../../skin";
import { themeEntries } from "../../themes/registry";

export function GeneralPane({
  snapshot,
  save,
  mode,
  onModeChange,
  themePref,
  onThemePref,
  skin,
  onSkinChange,
  simpleOnly,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  mode: UiMode;
  onModeChange: (mode: UiMode) => void;
  themePref: "system" | "light" | "dark";
  onThemePref: (pref: "system" | "light" | "dark") => void;
  skin: Skin;
  onSkinChange: (id: Skin) => void;
  simpleOnly: boolean;
}) {
  const { t, lang, setLang } = useI18n();
  const { config } = snapshot;

  const patchWindow = (patch: Partial<AppConfig["window"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.window, patch);
      return next;
    });
  };

  const patchBalances = (patch: Partial<AppConfig["balances"]>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      Object.assign(next.balances, patch);
      return next;
    });
  };

  return (
    <>
      <Section title={t("appearance.title")}>
        {/*
          * The mode row, first in Appearance: the interface mode is the
          * broadest of the appearance preferences, and it now lives only
          * here — the sidebar foot no longer carries a switch. Both modes
          * see it, so a geek reader can step down to simple the same way a
          * simple reader steps up.
          */}
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("mode.toggle_aria")}</span>
            <span className="setting-desc">{t("mode.settings_hint")}</span>
          </div>
          <div className="setting-right">
            <Segment<UiMode>
              ariaLabel={t("mode.toggle_aria")}
              value={mode}
              onChange={onModeChange}
              options={[
                { value: "simple", label: t("mode.simple") },
                { value: "geek", label: t("mode.geek") },
              ]}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.language")}</span>
          </div>
          <div className="setting-right">
            <Segment
              ariaLabel={t("settings.language")}
              value={lang}
              onChange={setLang}
              options={[
                { value: "zh", label: t("lang.zh") },
                { value: "zh-TW", label: t("lang.zh_tw") },
                { value: "en", label: t("lang.en") },
              ]}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.theme")}</span>
          </div>
          <div className="setting-right">
            <Select<"system" | "light" | "dark">
              ariaLabel={t("settings.theme")}
              value={themePref}
              onChange={onThemePref}
              options={[
                { value: "system", label: t("theme.system") },
                { value: "light", label: t("theme.light") },
                { value: "dark", label: t("theme.dark") },
              ]}
            />
          </div>
        </div>
        {/*
          * The skin axis sits beside the theme axis: a skin re-inks the same
          * information structure, and a theme flips it light or dark, so the
          * two dropdowns read as one pair at the foot of the appearance
          * section.
          */}
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.skin")}</span>
          </div>
          <div className="setting-right">
            <Select<Skin>
              ariaLabel={t("settings.skin")}
              value={skin}
              onChange={onSkinChange}
              options={[
                { value: "industrial", label: t("skin.industrial") },
                ...themeEntries().filter((entry) => entry.enabled).map((entry) => ({
                  value: entry.id,
                  label: entry.name[lang === "zh" ? "zh-CN" : lang],
                })),
              ]}
            />
          </div>
        </div>
      </Section>

      <Section title={t("settings.window")}>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("win.launch_on_login")}</span>
            <span className="setting-desc">{t("win.launch_on_login_hint")}</span>
          </div>
          <div className="setting-right">
            <input
              type="checkbox"
              aria-label={t("win.launch_on_login")}
              checked={config.window.launch_on_login}
              onChange={(e) => patchWindow({ launch_on_login: e.currentTarget.checked })}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("win.silent_start")}</span>
            <span className="setting-desc">{t("win.silent_start_hint")}</span>
          </div>
          <div className="setting-right">
            <input
              type="checkbox"
              aria-label={t("win.silent_start")}
              checked={config.window.silent_start}
              onChange={(e) => patchWindow({ silent_start: e.currentTarget.checked })}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("win.keep_in_tray")}</span>
            <span className="setting-desc">{t("win.keep_in_tray_hint")}</span>
          </div>
          <div className="setting-right">
            <input
              type="checkbox"
              aria-label={t("win.keep_in_tray")}
              checked={config.window.keep_in_tray}
              onChange={(e) => patchWindow({ keep_in_tray: e.currentTarget.checked })}
            />
          </div>
        </div>
        {/*
          The two ways to leave, next to the setting that decides what closing
          the window does.

          Neither is reachable from the window's own close button once
          keep-in-tray is on, and the tray is not a place a first-time user
          looks: the box says "closing keeps it running", so the way to actually
          stop it has to be here. `quit_app` is the same code path the tray's
          Quit item runs — the proxy is stopped before the process ends — which
          is why it is a button rather than a bare window close.
        */}
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("win.window_actions")}</span>
            <span className="setting-desc">{t("win.window_actions_hint")}</span>
          </div>
          <div className="setting-right">
            <Button kind="ghost" onClick={() => void api.hide()}>
              {t("win.hide_to_tray")}
            </Button>
            <Button kind="danger" onClick={() => void api.quit()}>
              {t("win.quit")}
            </Button>
          </div>
        </div>
      </Section>

      <Section title={t("settings.balances")} hint={t("settings.balances_hint")}>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.balance_auto")}</span>
            <span className="setting-desc">{t("settings.balance_auto_hint")}</span>
          </div>
          <div className="setting-right">
            <input
              type="checkbox"
              aria-label={t("settings.balance_auto")}
              checked={config.balances.enabled}
              onChange={(e) => patchBalances({ enabled: e.currentTarget.checked })}
            />
          </div>
        </div>
        <div className="setting-row">
          <div className="setting-left">
            <span className="setting-title">{t("settings.balance_interval")}</span>
            <span className="setting-desc">{t("settings.balance_interval_hint")}</span>
          </div>
          <div className="setting-right">
            <CommitNumber
              min={60}
              max={86400}
              integer
              value={config.balances.interval_secs}
              ariaLabel={t("settings.balance_interval")}
              onCommit={(v) => patchBalances({ interval_secs: v ?? 900 })}
            />
          </div>
        </div>
      </Section>

      {simpleOnly && (
        /*
          * The about section, in simple mode only: the gateway pane it
          * complements is hidden there, and with it the one place that named
          * the configuration file. Version and path are facts, not knobs, so
          * the section reads and changes nothing.
          */
        <Section title={t("settings.about")} hint={t("settings.about_note")}>
          <div className="kv-row">
            <span className="kv-key">{t("settings.version")}</span>
            <span className="kv-val mono">v{snapshot.version}</span>
          </div>
          <div className="kv-row">
            <span className="kv-key">{t("settings.config_path")}</span>
            <span className="kv-val mono">{snapshot.config_path}</span>
          </div>
        </Section>
      )}
    </>
  );
}
