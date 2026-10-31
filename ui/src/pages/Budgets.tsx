import { useState } from "react";
import { TIERS, money, tierName, type AppConfig, type Budget, type BudgetPeriod, type BudgetScope, type ModelTier, type Snapshot } from "../api";
import { Badge, Button, CommitNumber, ConfirmDialog, Field, Section, Select, Toggle, type ConfirmRequest } from "../components";
import { useI18n } from "../i18n";

/** The single budget editor belongs to usage, not application appearance. */
export default function Budgets({ snapshot, save, busy }: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState({ scope: "global", period: "day" as BudgetPeriod, amount: 0, currency: "USD" });
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  // The add row unfolds from the section head; the table is the subject.
  const [adding, setAdding] = useState(false);
  const scope = (value: string): BudgetScope => {
    if (value.startsWith("provider:")) return { kind: "provider", id: value.slice(9) };
    if (value.startsWith("tier:")) return { kind: "tier", tier: value.slice(5) as ModelTier };
    return { kind: "global" };
  };
  const patch = (id: string, change: Partial<Budget>) => void save((config) => {
    const budget = config.budgets.find((budget) => budget.id === id);
    if (!budget) return null;
    Object.assign(budget, change);
    return config;
  });
  return <Section
    title={t("settings.budgets")}
    hint={t("settings.budgets_hint")}
    actions={
      <button type="button" className="linky" onClick={() => setAdding((open) => !open)}>
        {adding ? t("common.collapse") : t("budget.add")}
      </button>
    }
  >
    <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    <div className="table-scroll"><table className="table">
      <caption className="table-caption">{t("settings.budgets")}</caption>
      <thead><tr>
        <th scope="col">{t("budget.covers")}</th><th scope="col">{t("budget.window")}</th><th scope="col">{t("budget.limit")}</th>
        <th scope="col">{t("budget.spent")}</th><th scope="col">{t("budget.when")}</th><th scope="col">{t("budget.on")}</th><th scope="col">{t("common.actions")}</th>
      </tr></thead>
      <tbody>{snapshot.budgets.map(({ budget, spent, used }) => <tr key={budget.id}>
        <td>{budget.scope.kind === "global" ? t("scope.global") : budget.scope.kind === "tier" ? tierName(budget.scope.tier, t) : providerName(budget.scope, snapshot)}</td>
        <td>{t(budget.period === "day" ? "period.day" : "period.month")}</td>
        <td>{money(budget.limit.currency, budget.limit.amount)}</td>
        <td>{money(spent.currency, spent.amount)} {used >= 1 && <Badge tone="danger">{t("budget.used_up")}</Badge>}</td>
        <td><Select value={budget.on_exceeded.action === "reject" ? "reject" : budget.on_exceeded.to} ariaLabel={t("budget.when")}
          onChange={(next) => patch(budget.id, { on_exceeded: next === "reject" ? { action: "reject" } : { action: "degrade", to: next as ModelTier } })}
          options={[{ value: "reject", label: t("budget.reject") }, ...TIERS.map((tier) => ({ value: tier, label: t("budget.degrade", { id: tierName(tier, t) }) }))]} /></td>
        <td><Toggle label={t("budget.on")} checked={budget.enabled} onChange={(enabled) => patch(budget.id, { enabled })} /></td>
        <td><Button kind="ghost" disabled={busy} onClick={() => setConfirm({ title: t("confirm.remove_budget"), danger: true, confirmLabel: t("common.delete"),
          onConfirm: () => void save((config) => { config.budgets = config.budgets.filter((item) => item.id !== budget.id); return config; }) })}>{t("common.delete")}</Button></td>
      </tr>)}</tbody>
    </table></div>
    {adding && <div className="controls">
      <Field label={t("budget.covers")}><Select value={draft.scope} onChange={(scope) => setDraft({ ...draft, scope })} ariaLabel={t("budget.covers")}
        options={[{ value: "global", label: t("scope.global") }, ...TIERS.map((tier) => ({ value: `tier:${tier}`, label: tierName(tier, t) })), ...snapshot.config.providers.map((provider) => ({ value: `provider:${provider.id}`, label: provider.name }))]} /></Field>
      <Field label={t("budget.window")}><Select value={draft.period} onChange={(period) => setDraft({ ...draft, period })} ariaLabel={t("budget.window")}
        options={[{ value: "day", label: t("period.day") }, { value: "month", label: t("period.month") }]} /></Field>
      <Field label={t("budget.limit")}><CommitNumber value={draft.amount} min={0} ariaLabel={t("budget.limit")} onCommit={(amount) => setDraft({ ...draft, amount: amount ?? 0 })} /></Field>
      <Field label={t("field.currency")}><input aria-label={t("field.currency")} value={draft.currency} onChange={(event) => setDraft({ ...draft, currency: event.target.value.toUpperCase() })} /></Field>
      <div className="field-actions"><Button disabled={busy || draft.amount <= 0 || !draft.currency.trim()} onClick={() => void save((config) => {
        config.budgets.push({ id: "", scope: scope(draft.scope), period: draft.period, limit: { amount: draft.amount, currency: draft.currency.trim() }, on_exceeded: { action: "reject" }, enabled: true });
        return config;
      }).then((ok) => { if (ok) { setDraft({ ...draft, amount: 0 }); setAdding(false); } })}>{t("budget.add")}</Button></div>
    </div>}
  </Section>;
}

function providerName(scope: { kind: "provider"; id: string }, snapshot: Snapshot): string {
  return snapshot.config.providers.find((provider) => provider.id === scope.id)?.name ?? scope.id;
}
