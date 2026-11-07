/** The five price fields, plus the "is anything priced" test they share. */
import { emptyPricing, type ModelEntry, type Pricing } from "../../api";
import { useI18n } from "../../i18n";
import { Button, NumberField, TextField } from "../../components";

/**
 * The four numbers that make up a price. Clearing input and output removes the
 * price entirely, which is how a model goes back to being unpriced.
 */
export function PriceFields({
  pricing,
  onChange,
}: {
  pricing: Pricing | null;
  /** Mutates the model being committed; see `Models.update`. */
  onChange: (mutate: (model: ModelEntry) => void) => void;
}) {
  const { t } = useI18n();
  const current = pricing ?? emptyPricing();

  /**
   * One field of the price, applied to the price as it is in the config being
   * committed. Rebuilding the object from the rendered `current` would bring
   * back the other three fields as they were before an in-flight save.
   */
  const patch = (change: Partial<Pricing>) => {
    onChange((model) => {
      const previous = model.pricing;
      const next = { ...(previous ?? emptyPricing()), ...change };
      // Naming a currency on its own does not make a model priced; the price
      // exists once one of the numbers does.
      model.pricing = change.currency !== undefined
        ? isPriced(next) ? next : previous
        : isPriced(next) ? next : null;
    });
  };

  return (
    <div className="controls">
      <TextField
        label={t("field.currency")}
        ariaLabel={t("field.currency")}
        hint={t("field.currency_hint")}
        value={current.currency}
        onCommit={(currency) => patch({ currency: currency.trim().toUpperCase() || "USD" })}
      />
      <NumberField
        label={t("field.input")}
        ariaLabel={t("models.input_price")}
        hint={t("field.input_hint")}
        min={0}
        value={current.input_per_mtok}
        onCommit={(v) => patch({ input_per_mtok: v ?? 0 })}
      />
      <NumberField
        label={t("field.output")}
        ariaLabel={t("models.output_price")}
        hint={t("field.output_hint")}
        min={0}
        value={current.output_per_mtok}
        onCommit={(v) => patch({ output_per_mtok: v ?? 0 })}
      />
      <NumberField
        label={t("models.cache_read")}
        hint={t("models.cache_read_hint")}
        min={0}
        placeholder={t("models.price_same_as_input")}
        value={current.cache_read_per_mtok}
        onCommit={(v) => patch({ cache_read_per_mtok: v })}
      />
      <NumberField
        label={t("models.cache_write")}
        hint={t("models.cache_write_hint")}
        min={0}
        placeholder={t("models.price_not_billed")}
        value={current.cache_write_per_mtok}
        onCommit={(v) => patch({ cache_write_per_mtok: v })}
      />
      {pricing && (
        <div className="field-actions">
          <Button
            kind="ghost"
            onClick={() =>
              onChange((model) => {
                model.pricing = null;
              })
            }
          >
            {t("models.clear_price")}
          </Button>
        </div>
      )}
    </div>
  );
}

/** Whether these numbers mean the model is priced at all. */
function isPriced(pricing: Pricing): boolean {
  return (
    pricing.input_per_mtok > 0 ||
    pricing.output_per_mtok > 0 ||
    pricing.cache_read_per_mtok !== null ||
    pricing.cache_write_per_mtok !== null
  );
}
