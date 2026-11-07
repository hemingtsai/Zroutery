import { describe, expect, it, vi } from "vitest";
import { formatPriceNumber, modelPriceText, revealAddFormLanding } from "./Models";
import type { Pricing } from "../api";
import type { TKey } from "../i18n";

/**
 * The open handoff behind the review's O2 fix (batch A4): the page-head
 * entry unfolds the add form, and the form must answer by putting itself
 * where the reader is — scroll the landing into the viewport, then give its
 * first input the focus, so the next keystroke types into the form instead
 * of hunting for a field that opened below the fold.
 *
 * Whether the landing really intersects the viewport is the interaction
 * harness's question — it runs an actual browser against a 600-model
 * catalogue and must not be answered here with mocks. This unit pins the
 * behavior the pages' open effects perform, and the order they perform it
 * in: scroll first, focus last, both optional, nothing thrown.
 */

/** A landing with a scroll recorder and a first control that records focus. */
function landingHarness() {
  const focus = vi.fn();
  const scrollIntoView = vi.fn();
  const landing = {
    scrollIntoView,
    querySelector: vi.fn(() => ({ focus }) as unknown as HTMLElement),
  } as unknown as HTMLElement;
  return { landing, scrollIntoView, focus };
}

describe("revealAddFormLanding", () => {
  it("opens on the form: scrolls the landing in, then focuses its first control", () => {
    const { landing, scrollIntoView, focus } = landingHarness();

    revealAddFormLanding(landing, "input");

    // Nearest, not start: a landing already on screen must not be moved.
    // The reader pressed the head button at the top of the page, which is
    // where the landing now mounts.
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest" });
    expect(landing.querySelector).toHaveBeenCalledWith("input");
    expect(focus).toHaveBeenCalledTimes(1);
    // Scroll first, so the focus call's own correction has the last word.
    expect(scrollIntoView.mock.invocationCallOrder[0]).toBeLessThan(
      focus.mock.invocationCallOrder[0],
    );
  });

  it("is a no-op when the form folded before the effect ran", () => {
    expect(() => revealAddFormLanding(null, "input")).not.toThrow();
  });

  it("still scrolls when no first control matches yet", () => {
    const scrollIntoView = vi.fn();
    const landing = {
      scrollIntoView,
      querySelector: vi.fn(() => null),
    } as unknown as HTMLElement;

    revealAddFormLanding(landing, "input");

    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest" });
  });
});

/**
 * The A5 price line: the row's price moved under the name as a muted
 * sub-line, and the states the review's O4 found blurred together must stay
 * apart — no price configured, a configured zero, and the side of a
 * one-sided price that was never entered. The unit sentence is the
 * dictionary's ("models.price_cell"), so the fake below speaks it verbatim
 * and the assertions pin which values the function feeds which slot.
 */

/** The price-line keys, in the source dictionary's words. */
const PRICE_DICT: Partial<Record<TKey, string>> = {
  "models.price_cell": "输入 {input} · 输出 {output} · {currency}/百万",
  "models.price_missing": "未配置价格",
  "models.price_side_unset": "未配置",
};

const fakeT = ((key: TKey, params?: Record<string, string | number>) => {
  let text = PRICE_DICT[key] ?? key;
  for (const [name, value] of Object.entries(params ?? {})) {
    text = text.replaceAll(`{${name}}`, String(value));
  }
  return text;
}) as (key: TKey, params?: Record<string, string | number>) => string;

/** A config-shaped price: token rates in USD, cache sides unset. */
function pricing(input: number, output: number): Pricing {
  return {
    currency: "USD",
    input_per_mtok: input,
    output_per_mtok: output,
    cache_read_per_mtok: null,
    cache_write_per_mtok: null,
  };
}

describe("formatPriceNumber", () => {
  it("prints the decimal that was configured, not the binary tail", () => {
    expect(formatPriceNumber(1.4700000000000002)).toBe("1.47");
    expect(formatPriceNumber(0.30000000000000004)).toBe("0.3");
  });

  it("keeps meaningful small precision", () => {
    expect(formatPriceNumber(0.02)).toBe("0.02");
    expect(formatPriceNumber(0.001)).toBe("0.001");
    expect(formatPriceNumber(0.0000001)).toBe("0.0000001");
  });

  it("prints zero as zero", () => {
    expect(formatPriceNumber(0)).toBe("0");
    expect(formatPriceNumber(-0)).toBe("0");
  });

  it("prints large values in full and never in exponent notation", () => {
    expect(formatPriceNumber(1234567.89)).toBe("1234567.89");
    expect(formatPriceNumber(1e21)).toBe("1000000000000000000000");
    expect(formatPriceNumber(2.5e21)).toBe("2500000000000000000000");
  });
});

describe("modelPriceText", () => {
  it("announces an unconfigured price instead of going blank or printing zero", () => {
    expect(modelPriceText(null, fakeT)).toEqual({ line: "未配置价格", title: null });
  });

  it("prints a configured all-zero price as the known zero it is", () => {
    const view = modelPriceText(pricing(0, 0), fakeT);
    expect(view.line).toBe("输入 0 · 输出 0 · USD/百万");
    expect(view.title).toBeNull();
  });

  it("labels the side a price was never entered for instead of printing zero", () => {
    expect(modelPriceText(pricing(4, 0), fakeT).line).toBe(
      "输入 4 · 输出 未配置 · USD/百万",
    );
    expect(modelPriceText(pricing(0, 16), fakeT).line).toBe(
      "输入 未配置 · 输出 16 · USD/百万",
    );
  });

  it("prints both sides of a two-sided price, stating the unit once", () => {
    const view = modelPriceText(pricing(4, 16), fakeT);
    expect(view.line).toBe("输入 4 · 输出 16 · USD/百万");
    expect(view.title).toBeNull();
  });

  it("carries the full stored value in the title when the display was trimmed", () => {
    const view = modelPriceText(pricing(1.4700000000000002, 16), fakeT);
    expect(view.line).toBe("输入 1.47 · 输出 16 · USD/百万");
    expect(view.title).toBe("输入 1.4700000000000002 · 输出 16 · USD/百万");
  });
});
