import { useCallback, useEffect, useState } from "react";
import { api, errorText, type PluginInspection, type PluginInstallation } from "../api";
import { Button, ConfirmDialog, Section, type ConfirmRequest } from "../components";
import { useI18n } from "../i18n";
import { applyThemeRef, registerTheme, setThemeEnabled } from "../themes/registry";
import { loadThemeRef } from "../themes/preferences";
import { PluginPanel } from "./renderer";

export default function PluginManager() {
  const { t, lang } = useI18n();
  const [plugins, setPlugins] = useState<PluginInstallation[]>([]);
  const [inspection, setInspection] = useState<PluginInspection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const locale = lang === "zh" ? "zh-CN" : lang;
  const reload = useCallback(async () => {
    const rows = await api.plugins();
    setPlugins(rows);
    const themes = await api.pluginThemes();
    for (const theme of themes) {
      const plugin = rows.find((p) => p.plugin_id === theme.theme_id);
      if (plugin) registerTheme(plugin.manifest, theme.package_digest, theme.theme, plugin.enabled);
    }
    for (const plugin of rows) if (plugin.manifest.contributions.themes?.length) setThemeEnabled(plugin.plugin_id, plugin.enabled);
    applyThemeRef(loadThemeRef());
  }, []);
  useEffect(() => { let active = true; void reload().catch((e) => { if (active) setError(errorText(e)); }); return () => { active = false; }; }, [reload]);
  const run = async (work: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true); setError(null);
    try { await work(); await reload(); } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  };
  return <>
    <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    <Section title={t("plugins.title")} hint={t("plugins.hint")}>
      <div className="plugin-manager-actions">
        <Button disabled={busy} onClick={() => void run(async () => setInspection(await api.inspectPlugin()))}>{t("plugins.inspect")}</Button>
        <Button kind="ghost" disabled={busy} onClick={() => void run(api.safeAppearance)}>{t("plugins.safe_appearance")}</Button>
      </div>
      {error && <p role="alert" className="warn-text">{error}</p>}
      {inspection && <div data-host-security="true" className="plugin-stack">
        <strong>{inspection.manifest.name[locale]} · {inspection.manifest.version}</strong>
        <code>{inspection.package_digest}</code>
        <p>{t("plugins.declaration_not_grant")}</p>
        <ul className="plugin-permissions">{inspection.manifest.permissions.map((permission, index) => <li key={index}>
          <code>{permission.capability}</code> — {permission.reason[locale]}
          <div className="field-hint">{JSON.stringify(Object.fromEntries(Object.entries(permission).filter(([key]) => key !== "reason")))}</div>
        </li>)}</ul>
        <div className="plugin-manager-actions">
          <Button disabled={busy} onClick={() => void run(async () => {
            const previous = plugins.find((p) => p.plugin_id === inspection.manifest.id);
            if (previous) await api.updatePlugin(previous.installation_id, inspection.stage_id, inspection.package_digest);
            else await api.installPlugin(inspection.stage_id, inspection.package_digest);
            setInspection(null);
          })}>{t("plugins.install_disabled")}</Button>
          <Button kind="ghost" onClick={() => setInspection(null)}>{t("common.cancel")}</Button>
        </div>
      </div>}
      {plugins.length === 0 && <p className="field-hint">{t("plugins.none")}</p>}
      {plugins.map((plugin) => <div className="plugin-stack" key={plugin.installation_id}>
        <div className="kv-row"><span>{plugin.manifest.name[locale]} · {plugin.version}</span><span>{plugin.enabled ? t("common.enabled") : t("plugins.disabled")}</span></div>
        <code>{plugin.plugin_id} · {plugin.package_digest.slice(0, 16)}</code>
        {plugin.diagnostic && <p className="field-hint">{t("plugins.ui_unavailable")} ({plugin.diagnostic})</p>}
        <div className="plugin-manager-actions">
          <Button disabled={busy || (!!plugin.manifest.runtime && !plugin.sandbox_available && !(plugin.manifest.contributions.providers?.length || plugin.manifest.contributions.clients?.length || plugin.manifest.contributions.themes?.length))} onClick={() => plugin.enabled
            ? void run(() => api.disablePlugin(plugin.installation_id))
            : setConfirm({ title: t("plugins.grant_title"), body: <div><p>{t("plugins.declaration_not_grant")}</p><ul className="plugin-permissions">{plugin.manifest.permissions.map((p, index) => <li key={index}><code>{p.capability}</code> — {p.reason[locale]}<div>{JSON.stringify(Object.fromEntries(Object.entries(p).filter(([key]) => key !== "reason")))}</div></li>)}</ul></div>, confirmLabel: t("plugins.grant_enable"), onConfirm: () => void run(() => api.activatePlugin(plugin.installation_id, plugin.package_digest, [...plugin.manifest.permissions])) })}>
            {plugin.enabled ? t("plugins.disable") : t("plugins.enable")}
          </Button>
          {plugin.previous_digest && <Button kind="ghost" disabled={busy} onClick={() => void run(() => api.rollbackPlugin(plugin.installation_id))}>{t("plugins.rollback")}</Button>}
          <Button kind="danger" disabled={busy} onClick={() => setConfirm({ title: t("plugins.remove"), body: t("plugins.remove_hint"), danger: true, onConfirm: () => void run(() => api.removePlugin(plugin.installation_id)) })}>{t("plugins.remove")}</Button>
        </div>
        {plugin.enabled && plugin.manifest.contributions.ui?.filter((u) => u.slot === "settings.extension" || u.slot === "settings.extensions.page").map((u) => <PluginPanel key={u.id} installationId={plugin.installation_id} contributionId={u.id} title={u.title[locale]} />)}
      </div>)}
    </Section>
  </>;
}
