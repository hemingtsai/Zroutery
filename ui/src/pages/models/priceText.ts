/**
 * Price-line formatting for the catalogue, pure and unit-tested: the states
 * a reader must be able to tell apart — no price configured, a configured
 * zero, a one-sided price — own the line and the title behind it.
 */
import type { Pricing } from "../../api";
import type { TKey } from "../../i18n";

/**
 * A price as the operator typed it, not as the binary double stored it.
 *
 * Per-token rates are decimal sentences ("4", "0.02", "1.47") that a double
 * smears with a binary tail — `String` of the stored value printed
 * `1.4700000000000002` straight into the row. Twelve significant digits
 * cover every rate a person enters and shave the tail off; whatever
 * survives is printed in full decimal, because `String` reaches for
 * exponent notation below a millionth and at 1e21 and a price line never
 * carries an "e".
 */
export function formatPriceNumber(value: number): string {
  if (!Number.isFinite(value)) return String(value);
  if (value === 0) return "0";
  const cleaned = Number(value.toPrecision(12));
  const raw = String(cleaned);
  if (!raw.includes("e")) return raw;
  // Expand the exponent by hand: take the mantissa's digits and move the
  // decimal point `exponent` places from where the mantissa kept it.
  const [mantissa, exponent] = raw.split("e");
  const shift = Number(exponent);
  const negative = mantissa.startsWith("-");
  const digits = mantissa.replace(/[-+.]/g, "");
  const dot = mantissa.indexOf(".");
  const point = (dot < 0 ? digits.length : dot - (negative ? 1 : 0)) + shift;
  const body =
    point <= 0
      ? `0.${"0".repeat(-point)}${digits}`
      : point >= digits.length
        ? `${digits}${"0".repeat(point - digits.length)}`
        : `${digits.slice(0, point)}.${digits.slice(point)}`;
  return `${negative ? "-" : ""}${body}`;
}

/**
 * The sentence a model row prints under its name for its price, and the raw
 * sentence its `title` carries when the printed numbers were trimmed.
 *
 * Three states the review's O4 found blurred together, kept apart here:
 *
 * - no `pricing` at all — the row says so ("未配置价格" / "No price
 *   configured") rather than going blank or printing a zero it never
 *   measured;
 * - a price whose both sides are zero — a real, configured zero (a free
 *   model), printed as the zero it is;
 * - a zero beside a configured rate — that is the side a price was never
 *   entered into (the editor's unset default), so it says "not set"
 *   instead of counterfeiting a measured zero.
 *
 * The per-million unit is stated once, at the end, in every state. The
 * `title` repeats the sentence with the stored values untrimmed, and is
 * omitted when there is nothing behind the printed numbers to reveal.
 */
export function modelPriceText(
  pricing: Pricing | null,
  t: (key: TKey, params?: Record<string, string | number>) => string,
): { line: string; title: string | null } {
  if (!pricing) return { line: t("models.price_missing"), title: null };
  // One side of the price, as printed (`raw: false`) or as stored. A
  // non-zero always prints; a zero prints only when it is the whole price —
  // beside a configured rate it means "never entered", not "known free".
  const side = (value: number, other: number, raw: boolean): string => {
    if (value !== 0) return raw ? String(value) : formatPriceNumber(value);
    return other !== 0 ? t("models.price_side_unset") : "0";
  };
  const input = side(pricing.input_per_mtok, pricing.output_per_mtok, false);
  const output = side(pricing.output_per_mtok, pricing.input_per_mtok, false);
  const line = t("models.price_cell", {
    input,
    output,
    currency: pricing.currency,
  });
  const inputRaw = side(pricing.input_per_mtok, pricing.output_per_mtok, true);
  const outputRaw = side(pricing.output_per_mtok, pricing.input_per_mtok, true);
  const title =
    input === inputRaw && output === outputRaw
      ? null
      : t("models.price_cell", {
          input: inputRaw,
          output: outputRaw,
          currency: pricing.currency,
        });
  return { line, title };
}
