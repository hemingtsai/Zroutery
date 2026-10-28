import { useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, money, type ModelEntry, type NewApiAnnouncement, type NewApiPrice, type Provider, type Snapshot } from "../api";
import { Button, Field, Section, Select, useToast } from "../components";
import { useI18n } from "../i18n";

/** Public panel metadata. Choosing a panel or group never changes routing prices. */
export default function NewApiPanel({ provider, models, busy, run }: {
  provider: Provider;
  models: ModelEntry[];
  busy: boolean;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const accounts = provider.accounts ?? [];
  const [accountId, setAccountId] = useState(
    () => (accounts.find((account) => account.enabled) ?? accounts[0])?.account_id ?? "",
  );
  const selectedAccount = accounts.find((account) => account.account_id === accountId);
  const panelUrl = selectedAccount?.maintenance.panel_url?.trim() || provider.base_url;
  const [announcements, setAnnouncements] = useState<NewApiAnnouncement[] | null>(null);
  const [prices, setPrices] = useState<NewApiPrice[] | null>(null);
  const [announcementError, setAnnouncementError] = useState<string | null>(null);
  const [pricingError, setPricingError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [group, setGroup] = useState("default");
  const generation = useRef(0);

  useEffect(() => {
    generation.current += 1;
    setAnnouncements(null);
    setPrices(null);
    setAnnouncementError(null);
    setPricingError(null);
    setLoading(false);
    return () => { generation.current += 1; };
  }, [provider.id, provider.base_url, accountId, panelUrl]);

  const readPanel = async () => {
    const current = ++generation.current;
    setLoading(true);
    setAnnouncementError(null);
    setPricingError(null);
    const [announcementResult, pricingResult] = await Promise.allSettled([
      api.fetchNewapiAnnouncements(provider.id, accountId || null),
      api.previewNewapiPricing(provider.id, accountId || null),
    ]);
    if (current !== generation.current) return;
    if (announcementResult.status === "fulfilled") {
      setAnnouncements(announcementResult.value);
    } else {
      setAnnouncementError(errorText(announcementResult.reason));
    }
    if (pricingResult.status === "fulfilled") {
      setPrices(pricingResult.value);
      const groups = [...new Set(pricingResult.value.map((price) => price.group))];
      setGroup((previous) => groups.includes(previous) ? previous : groups.includes("default") ? "default" : groups[0] ?? "default");
    } else {
      setPricingError(errorText(pricingResult.reason));
    }
    setLoading(false);
  };

  const groups = useMemo(() => [...new Set((prices ?? []).map((price) => price.group))], [prices]);
  const groupPrices = useMemo(() => (prices ?? []).filter((price) => price.group === group), [prices, group]);
  const matched = useMemo(() => {
    const names = new Set(models.map((model) => model.upstream_model));
    return groupPrices.filter((price) => names.has(price.model));
  }, [models, groupPrices]);
  const applicable = matched.filter((price) => price.input_per_mtok != null && price.output_per_mtok != null);

  return (
    <Section title={t("providers.panel_info")} hint={t("providers.panel_info_hint")}
      actions={<Button kind="ghost" disabled={loading || busy} onClick={() => void readPanel()}>
        {loading ? t("common.loading") : t("providers.panel_read")}
      </Button>}>
      {accounts.length > 0 && (
        <Field label={t("providers.panel_account")} hint={t("providers.panel_account_hint")}>
          <Select ariaLabel={t("providers.panel_account")} value={accountId} disabled={loading || busy}
            options={accounts.map((account) => ({ value: account.account_id, label: account.account_id }))}
            onChange={setAccountId} />
        </Field>
      )}
      <p className="field-hint mono panel-address">{panelUrl}</p>
      {announcements === null && prices === null && !announcementError && !pricingError && (
        <p className="field-hint">{t("providers.panel_read_hint")}</p>
      )}
      {announcementError && <p className="field-error" role="status">{t("providers.panel_announcements_failed", { error: announcementError })}</p>}
      {announcements !== null && (
        announcements.length === 0 ? <p className="field-hint">{t("providers.panel_no_announcements")}</p> :
        <div className="list">
          {announcements.map((announcement, index) => (
            <div className="list-row panel-announcement" key={`${announcement.id ?? index}-${index}`}>
              <div className="row-main">
                <span className="row-title">{announcement.title || t("providers.panel_announcement")}</span>
                {announcement.content && <span className="row-sub panel-announcement-content">{announcement.content}</span>}
              </div>
            </div>
          ))}
        </div>
      )}
      {pricingError && <p className="field-error" role="status">{t("providers.panel_prices_failed", { error: pricingError })}</p>}
      {prices !== null && (
        <>
          {groups.length > 0 && <Field label={t("providers.panel_group")} hint={t("providers.panel_group_hint")}>
            <Select ariaLabel={t("providers.panel_group")} value={group} disabled={loading || busy}
              options={groups.map((value) => ({ value, label: value }))} onChange={setGroup} />
          </Field>}
          <p className="field-hint">{t("providers.panel_prices", { matched: matched.length, total: groupPrices.length, group })}</p>
          {matched.length > 0 && <div className="table-scroll panel-price-table">
            <table className="table">
              <caption className="visually-hidden">{t("providers.panel_price_caption")}</caption>
              <thead><tr>
                <th scope="col">{t("field.model_name")}</th>
                <th scope="col">{t("field.input")} / 1M</th>
                <th scope="col">{t("field.output")} / 1M</th>
                <th scope="col">{t("providers.panel_cache_read")} / 1M</th>
                <th scope="col">{t("providers.panel_cache_write")} / 1M</th>
                <th scope="col">{t("providers.panel_per_request")}</th>
              </tr></thead>
              <tbody>{matched.map((price) => <tr key={price.model}>
                <td className="mono">{price.model}</td>
                {[price.input_per_mtok, price.output_per_mtok, price.cache_read_per_mtok, price.cache_write_per_mtok, price.per_request].map((value, index) => (
                  <td className="mono" key={index}>{value == null ? t("common.dash") : money("USD", value)}</td>
                ))}
              </tr>)}</tbody>
            </table>
          </div>}
          <div className="controls">
            <Button kind="primary" disabled={busy || loading || pricingError !== null || applicable.length === 0}
              onClick={async () => {
                const ok = await run(() => api.importNewapiPricing(provider.id, accountId || null, group));
                if (ok) notify("ok", t("providers.panel_prices_imported"));
              }}>{t("providers.panel_apply_prices")}</Button>
          </div>
        </>
      )}
    </Section>
  );
}
