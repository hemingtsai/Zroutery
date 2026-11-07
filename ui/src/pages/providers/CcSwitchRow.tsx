/** One checkbox row of the CC Switch import preview table. */
import { tierName, type CcProviderDraft } from "../../api";
import { useI18n } from "../../i18n";
import { Badge } from "../../components";

export function CcSwitchRow({
  draft,
  checked,
  onSelect,
}: {
  draft: CcProviderDraft;
  checked: boolean;
  onSelect: (id: string, on: boolean) => void;
}) {
  const { t, lang } = useI18n();
  // The backend decides this with the same rule the import command applies, so
  // a disabled row really is one the import would skip.
  const alreadyId = draft.already_imported ? draft.target_id : null;
  return (
    <tr className={draft.already_imported ? "row-warn" : ""}>
      <td>
        <input
          type="checkbox"
          aria-label={t("cc.import_row", { name: draft.name })}
          checked={checked}
          disabled={draft.already_imported}
          title={alreadyId ? `${t("cc.already")} (${alreadyId})` : undefined}
          onChange={(e) => onSelect(draft.source_id, e.currentTarget.checked)}
        />
      </td>
      <td>
        {draft.name}
        {draft.is_current && (
          <>
            {" "}
            <Badge tone="ok">{t("cc.active")}</Badge>
          </>
        )}
        {draft.already_imported && (
          <>
            {" "}
            <Badge tone="neutral">{t("cc.already")}</Badge>
          </>
        )}
      </td>
      <td className="muted mono">{draft.base_url}</td>
      <td className="muted">
        {draft.models.length === 0
          ? "—"
          : draft.models
              .map((m) =>
                m.tier
                  ? // The brackets are full-width in Chinese and half-width in
                    // English, because a full-width bracket in an English
                    // sentence is a Chinese mark in the wrong place.
                    lang === "zh"
                    ? `${m.upstream_model}（${tierName(m.tier, t)}）`
                    : `${m.upstream_model} (${tierName(m.tier, t)})`
                  : m.upstream_model,
              )
              .join(", ")}
      </td>
    </tr>
  );
}
