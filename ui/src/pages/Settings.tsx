/**
 * The settings page shell: the tab strip, the ConfirmDialog around the token
 * regeneration, and the PageHead.
 *
 * The subject panes live in `pages/settings/` — the General and gateway
 * panes were carved out of this file by subject when it grew too large to
 * read as one file. The shell keeps the tab state, the token-reveal state
 * and the CORS-draft state, so they survive the tab switches.
 */
import { useEffect, useState } from "react";
import { type AppConfig, type Snapshot } from "../api";
import { ConfirmDialog, PageHead, Tabs, type ConfirmRequest } from "../components";
import type { UiMode } from "../mode";
import { useI18n } from "../i18n";
import type { Skin } from "../skin";
import PluginManager from "../plugins/manager";
import { GatewayPane } from "./settings/gateway";
import { GeneralPane } from "./settings/general";

export default function Settings({
  snapshot,
  save,
  run,
  busy,
  themePref,
  onThemePref,
  skin,
  onSkinChange,
  mode,
  onModeChange,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
  themePref: "system" | "light" | "dark";
  onThemePref: (pref: "system" | "light" | "dark") => void;
  skin: Skin;
  onSkinChange: (id: Skin) => void;
  mode: UiMode;
  onModeChange: (mode: UiMode) => void;
}) {
  const { t } = useI18n();
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [originError, setOriginError] = useState<string | null>(null);
  const [originDraft, setOriginDraft] = useState("");
  const [revealed, setRevealed] = useState<string | null>(null);
  const [tab, setTab] = useState<"general" | "gateway" | "plugins">("general");
  const simpleOnly = mode === "simple";

  const generalPane = (
    <GeneralPane
      snapshot={snapshot}
      save={save}
      mode={mode}
      onModeChange={onModeChange}
      themePref={themePref}
      onThemePref={onThemePref}
      skin={skin}
      onSkinChange={onSkinChange}
      simpleOnly={simpleOnly}
    />
  );

  useEffect(() => {
    if (simpleOnly && tab === "gateway") setTab("general");
  }, [simpleOnly, tab]);

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      <PageHead lede={t("settings.lede")} />

      <Tabs
        value={simpleOnly && tab === "gateway" ? "general" : tab}
        onChange={(next) => setTab(next as typeof tab)}
        ariaLabel={t("settings.tab_group")}
        tabs={
          simpleOnly
            ? [
                { value: "general", label: t("settings.tab_general"), content: generalPane },
                { value: "plugins", label: t("plugins.title"), content: <PluginManager /> },
              ]
            : [
                { value: "general", label: t("settings.tab_general"), content: generalPane },
                {
                  value: "gateway",
                  label: t("settings.tab_gateway"),
                  content: (
                    <GatewayPane
                      snapshot={snapshot}
                      save={save}
                      run={run}
                      busy={busy}
                      setConfirm={setConfirm}
                      revealed={revealed}
                      setRevealed={setRevealed}
                      originError={originError}
                      setOriginError={setOriginError}
                      originDraft={originDraft}
                      setOriginDraft={setOriginDraft}
                    />
                  ),
                },
                { value: "plugins", label: t("plugins.title"), content: <PluginManager /> },
              ]
        }
      />
    </>
  );
}
