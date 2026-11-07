/** One row of the model list, and the status vocabulary it can draw. */
import { memo } from "react";
import { useI18n } from "../../i18n";
import { StatusDot } from "../../components";

/** The status vocabulary a list row can draw. Mirrors `StatusDot`'s prop. */
export type ListTone = "ok" | "warn" | "danger" | "off";

/**
 * One row of the model list, memoised.
 *
 * This is the only element on the page that appears once per configured model,
 * so it is the only one worth stopping: it takes every string it prints already
 * resolved and only two changing values of its own (`selected`, and the status
 * that depends on it), both computed by the list above. With the add-a-model
 * field holding a draft that changes on every keystroke, a row that re-rendered
 * with its parent turned four hundred rows of DOM into the price of typing a
 * model name.
 *
 * The markup is the row's own button — a component boundary is not a node,
 * so wrapping the list's rows in this component draws no extra wrapper.
 */
export const ModelListRow = memo(function ModelListRow({
  id,
  name,
  status,
  providerLine,
  price,
  priceTitle,
  latency,
  selected,
  onOpen,
}: {
  id: string;
  name: string;
  status: ListTone;
  providerLine: string;
  price: string;
  priceTitle: string | null;
  latency: string;
  selected: boolean;
  onOpen: (id: string) => void;
}) {
  const { t } = useI18n();
  return (
    <button type="button"
      className={`list-row ${selected ? "selected" : ""}`}
      onClick={() => onOpen(id)}
      /*
       * The name carries the state as well, because the row's own content is
       * replaced by it in the accessibility tree: the dot beside the model is
       * a colour, and a colour is invisible to a screen reader. Without this
       * the row announced only "Open <id>", and whether the model was enabled
       * was nowhere in the sentence. The price line rides along for the same
       * reason — it is one of the row's lines now, so it is part of the
       * sentence, including the word that says no price was configured.
       */
      aria-label={`${t("models.open_row", { id: name })} — ${providerLine} — ${price} — ${
        status === "off" ? t("status.disabled") : t("status.enabled")
      }`}
    >
      <StatusDot
        tone={status}
        state={status === "off" ? t("status.disabled") : t("status.enabled")}
      />
      <div className="row-main">
        {/*
         * A model id can be long, and an id that is longer than the row makes
         * the row wider than the page. The text is clipped rather than
         * truncated away: it is still in the document, and the row's own
         * accessible name carries it in full, so nothing is lost but the
         * pixels.
         */}
        <span className="row-title truncate" title={name}>
          {name}
        </span>
        <span className="row-sub">{providerLine}</span>
        {/*
         * The price as the name block's second sub-line (A5), where the
         * 820px window cannot press it into a 60px column that wrapped it
         * over three or four lines. It always says something — a model with
         * no pricing announces that rather than a blank — the numbers carry
         * no binary tail, and the stored value behind a trimmed number is
         * one hover away in the title.
         */}
        <span className="row-sub" title={priceTitle ?? undefined}>
          {price}
        </span>
      </div>
      <span className="col-num">{latency}</span>
    </button>
  );
});
