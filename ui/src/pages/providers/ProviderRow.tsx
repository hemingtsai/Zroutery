/** The provider list row and the balance chip it shows. */
import { memo } from "react";
import { money, type BalanceStatus, type ProviderKind } from "../../api";
import { useI18n } from "../../i18n";
import { Badge, StatusDot } from "../../components";
import { kindLabelKey } from "./kindMeta";

/**
 * One provider in the list, memoised.
 *
 * The list is the only thing on this page that grows with the configuration,
 * and the row is the only thing in it that is drawn once per provider. It
 * takes every string it prints already resolved — the name, the endpoint, the
 * model count, the dialect — plus the values that change on their own: the
 * status dot, whether it is the open provider, and the balance reading
 * beside it.
 *
 * The markup is the row's own button, unchanged: a component boundary is not
 * a node, so the list draws exactly what it drew before.
 */
export const ProviderRow = memo(function ProviderRow({
  id,
  name,
  baseUrl,
  kind,
  modelCount,
  status,
  selected,
  showBalance,
  balance,
  held,
  nested,
  onOpen,
}: {
  id: string;
  name: string;
  baseUrl: string;
  kind: ProviderKind;
  modelCount: number;
  status: "ok" | "warn" | "danger" | "off";
  selected: boolean;
  showBalance: boolean;
  balance: BalanceStatus | undefined;
  /** The wallet check paused this provider; the drawer carries the release. */
  held?: boolean;
  /** Rendered under the ordinary provider it shares a host with. */
  nested?: boolean;
  onOpen: (id: string) => void;
}) {
  const { t } = useI18n();
  return (
    <button type="button"
      className={`list-row ${nested ? "row-nested" : ""} ${selected ? "selected" : ""}`}
      onClick={() => onOpen(id)}
      /*
       * The name carries the state as well, because the row's own content is
       * replaced by it in the accessibility tree: the dot beside the provider
       * is a colour, and a colour is invisible to a screen reader. Without
       * this the row announced only "Open <name>", and whether the provider
       * was switched on — or was on with no credential stored — was nowhere
       * in the sentence.
       */
      aria-label={`${t("providers.open_row", { name })} — ${
        held
          ? t("status.wallet_held")
          : status === "off"
            ? t("status.disabled")
            : status === "warn"
              ? t("status.no_key")
              : t("status.enabled")
      }`}
    >
      <StatusDot
        tone={held ? "danger" : status}
        state={
          held
            ? t("status.wallet_held")
            : status === "off"
              ? t("status.disabled")
              : status === "warn"
                ? t("status.no_key")
                : t("status.enabled")
        }
      />
      <div className="row-main">
        <span className="row-title truncate" title={name}>
          {name}
        </span>
        <span className="row-sub">
          <span className="mono">{baseUrl}</span>
          {modelCount > 0 && ` · ${t("count.models", { n: modelCount })}`}
          {` · ${t(kindLabelKey(kind))}`}
        </span>
      </div>
      {showBalance && <BalanceChip status={balance} />}
    </button>
  );
});


/** Balance state as one quiet chip: what is left, or that the check failed. */
function BalanceChip({ status }: { status: BalanceStatus | undefined }) {
  const { t } = useI18n();
  if (!status) return <span className="muted">—</span>;
  if (status.aggregate) {
    return <span className="muted">
      {Object.values(status.aggregate.by_currency).map((balance) => money(balance.currency, balance.remaining ?? 0)).join(" · ") || t("common.unknown")}
      {status.aggregate.partial && <Badge tone="warn">{t("binding.partial", { ok: status.aggregate.succeeded, n: status.aggregate.participating })}</Badge>}
    </span>;
  }
  if (status.error) return <span className="muted">{t("providers.balance_failed")}</span>;
  if (status.balance) {
    const amount = status.balance.remaining ?? status.balance.total;
    return (
      <span className="muted mono">
        {amount !== null ? money(status.balance.currency, amount) : ""}
      </span>
    );
  }
  return null;
}
