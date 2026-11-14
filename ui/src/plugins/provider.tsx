import { useEffect, useState } from "react";
import { api, errorText, type Snapshot, type PluginInstallation } from "../api";
import { Button, Drawer, Section, Select, ConfirmDialog, type ConfirmRequest } from "../components";
import { useI18n } from "../i18n";

export function PluginProviderPicker({ run }: { run: (task: () => Promise<Snapshot>) => Promise<boolean> }) {
  const { t, lang } = useI18n();
  const [installed, setInstalled] = useState<PluginInstallation[]>([]);
  const [selected, setSelected] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { let active = true; void api.plugins().then((rows) => { if (active) setInstalled(rows.filter((p) => p.enabled)); }).catch((e) => { if (active) setError(errorText(e)); }); return () => { active = false; }; }, []);
  const locale = lang === "zh" ? "zh-CN" : lang;
  const options = installed.flatMap((plugin) => (plugin.manifest.contributions.providers ?? []).map((provider) => ({
    value: `${plugin.installation_id}/${provider.id}`, label: `${provider.name[locale]} · ${plugin.manifest.name[locale]}`,
  })));
  if (!options.length) return null;
  return <Section title={t("plugins.provider_add")} hint={t("plugins.provider_static")}>
    <div className="controls"><Select ariaLabel={t("plugins.provider_add")} value={selected || options[0].value} options={options} onChange={setSelected} />
      <Button disabled={busy} onClick={async () => {
        const [installation, contribution] = (selected || options[0].value).split("/");
        const label = options.find((o) => o.value === (selected || options[0].value))!.label;
        setBusy(true);
        try { await run(() => api.instantiatePluginProvider(installation, contribution, label)); }
        finally { setBusy(false); }
      }}>{t("plugins.provider_create")}</Button></div>
    {error && <p role="alert">{error}</p>}
  </Section>;
}

export function PluginProviderDrawer({ id, installationId, contributionId, name, snapshot, run, onClose }: {
  id: string; installationId: string; contributionId: string; name: string; snapshot: Snapshot;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>; onClose: () => void;
}) {
  const { t, lang } = useI18n();
  const locale = lang === "zh" ? "zh-CN" : lang;
  const [plugin, setPlugin] = useState<PluginInstallation | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  useEffect(() => { let active = true; void api.plugins().then((rows) => { if (active) setPlugin(rows.find((p) => p.installation_id === installationId) ?? null); }).catch((e) => { if (active) setError(errorText(e)); }); return () => { active = false; }; }, [installationId]);
  const descriptor = plugin?.manifest.contributions.providers?.find((p) => p.id === contributionId);
  return <Drawer title={name} onClose={onClose}>
    <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    <p>{t("plugins.provider_static")}</p>
    {error && <p role="alert">{error}</p>}
    {descriptor?.operations.map((operation) => <div className="kv-row" key={operation.id}><code>{operation.service.id}</code><span className="kv-val mono">{operation.base_url}</span></div>)}
    <div data-host-security="true" className="plugin-stack">
      {descriptor?.credentials.filter((slot) => slot.scope === "provider_instance").map((slot) => <div key={slot.id} className="plugin-stack">
        <label className="field">{slot.name[locale]}<input type="password" autoComplete="off" value={drafts[slot.id] ?? ""} onChange={(e) => setDrafts((current) => ({ ...current, [slot.id]: e.currentTarget.value }))} /></label>
        <div className="plugin-manager-actions"><Button disabled={!drafts[slot.id]?.trim()} onClick={async () => {
          const revision = snapshot.config.__config_revision;
          if (!revision) return;
          const value = drafts[slot.id];
          setDrafts((current) => ({ ...current, [slot.id]: "" }));
          await run(() => api.storePluginCredential(id, slot.id, value, revision));
        }}>{t("providers.save_key")}</Button>
        <Button kind="danger" onClick={() => setConfirm({ title: t("plugins.clear_slot"), body: `${name} · ${slot.name[locale]}`, danger: true,
          onConfirm: () => { const revision = snapshot.config.__config_revision; if (revision) void run(() => api.clearPluginCredential(id, slot.id, revision)); } })}>{t("plugins.clear_slot")}</Button></div>
      </div>)}
    </div>
    {descriptor?.credentials.some((slot) => slot.scope === "account") && <p>{t("plugins.account_unavailable")}</p>}
    <Button onClick={() => void run(() => api.setProviderEnabled(id, !(snapshot.config.providers.find((p) => p.id === id)?.enabled ?? false)))}>{t("common.enabled")}</Button>
  </Drawer>;
}
