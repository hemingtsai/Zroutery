import { Component, useEffect, useState, type ReactNode } from "react";
import { api, errorText, type PluginUiView } from "../api";
import { Button, Select, Section } from "../components";
import { useI18n } from "../i18n";
import type { UiNode } from "../../../docs/development/plugin-api-v1";

class PluginBoundary extends Component<{ children: ReactNode; fallback: string }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() { return this.state.failed ? <p role="alert">{this.props.fallback}</p> : this.props.children; }
}
function Node({ node, fields, change, click }: {
  node: UiNode; fields: Record<string, string>; change: (id: string, value: string) => void; click: (id: string) => void;
}): ReactNode {
  switch (node.kind) {
    case "stack": return <div className="plugin-stack">{node.children.map((child, index) => <Node key={index} node={child} fields={fields} change={change} click={click} />)}</div>;
    case "text": return <p>{node.text}</p>;
    case "notice": return <p role="status" className={node.tone === "info" ? "field-hint" : "warn-text"}>{node.text}</p>;
    case "value": return <div className="kv-row"><span className="kv-key">{node.label}</span><span className="kv-val">{node.value}</span></div>;
    case "button": return <Button onClick={() => click(node.action_id)}>{node.label}</Button>;
    case "input": return <label className="field">{node.label}<input value={fields[node.field_id] ?? node.value} onChange={(e) => change(node.field_id, e.currentTarget.value)} /></label>;
    case "select": return <Select ariaLabel={node.label} value={fields[node.field_id] ?? node.value} options={[...node.options]} onChange={(v) => change(node.field_id, v)} />;
    case "table": return <div className="table-scroll"><table className="table"><thead><tr>{node.columns.map((c, i) => <th key={i}>{c}</th>)}</tr></thead><tbody>{node.rows.map((row, i) => <tr key={i}>{row.map((value, j) => <td key={j}>{value}</td>)}</tr>)}</tbody></table></div>;
  }
}
export function PluginPanel({ installationId, contributionId, title }: { installationId: string; contributionId: string; title: string }) {
  const { t } = useI18n();
  const [view, setView] = useState<PluginUiView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});
  useEffect(() => {
    let active = true;
    if (document.visibilityState === "hidden") return;
    void api.renderPluginUi(installationId, contributionId).then((next) => { if (active) setView(next); }).catch((e) => { if (active) setError(errorText(e)); });
    return () => { active = false; };
  }, [installationId, contributionId]);
  const click = async (id: string) => {
    if (!view?.actions[id]) return;
    const token = view.actions[id];
    setView({ ...view, actions: { ...view.actions, [id]: "" } }); // single-use in UI too; host remains authoritative
    try { await api.pluginUiAction(installationId, token, view.revision, fields); }
    catch (e) { setError(errorText(e)); }
  };
  return <Section title={title} hint={t("plugins.belongs_to", { id: installationId })}>
    <PluginBoundary fallback={t("plugins.ui_unavailable")}>
      {error ? <p role="alert">{t("plugins.ui_unavailable")} ({error})</p> : view ? <div className="plugin-panel"><Node node={view.node} fields={fields} change={(id, value) => setFields((current) => ({ ...current, [id]: value }))} click={(id) => void click(id)} /></div> : <p>{t("common.loading")}</p>}
    </PluginBoundary>
  </Section>;
}
