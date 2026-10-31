import { useState } from "react";
import type { BindingPreview } from "./api";
import { Badge, Button, Drawer, KeyValue, Toggle } from "./components";
import { useI18n } from "./i18n";

/** Review references/verified identity only, never token bytes. */
export function AccountBindingReview({ preview, busy, publish, discard }: {
  preview: BindingPreview;
  busy: boolean;
  publish: (confirmed: boolean) => void;
  discard: () => void;
}) {
  const { t } = useI18n();
  const [confirmed, setConfirmed] = useState(false);
  return <Drawer title={t("binding.review")} onClose={discard}>
    <Badge tone={preview.verification === "verified" ? "ok" : "warn"}>
      {t(preview.verification === "verified" ? "binding.verified" : "binding.pending")}
    </Badge>
    {preview.identity && <KeyValue rows={[
      [t("binding.remote_user"), `${preview.identity.user_id} · ${preview.identity.username ?? ""}`],
      [t("binding.group"), preview.identity.group ?? t("common.unknown")],
      [t("binding.source"), preview.identity.source],
      [t("binding.panel"), preview.identity.panel_root],
    ]} />}
    {preview.reason && <p className="field-hint">{preview.reason}</p>}
    {preview.identity_changed && <>
      <p className="field-error">{t("binding.changed")}</p>
      <Toggle label={t("binding.confirm_change")} checked={confirmed} onChange={setConfirmed} />
    </>}
    <p className="field-hint">{t("binding.history_kept")}</p>
    <div className="controls">
      <Button kind="primary" disabled={busy || (preview.identity_changed && !confirmed)} onClick={() => publish(confirmed)}>{t("binding.publish")}</Button>
      <Button kind="ghost" disabled={busy} onClick={discard}>{t("common.cancel")}</Button>
    </div>
  </Drawer>;
}
