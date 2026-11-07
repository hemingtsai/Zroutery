import { describe, expect, it, vi } from "vitest";
import {
  commitInputKeys,
  drawerEscapeCloses,
  numberDraftCommit,
  type CommitKeysEvent,
} from "./components";

type KeyInit = { key: string; isComposing?: boolean; keyCode?: number };
type Keys = ReturnType<typeof commitInputKeys>;

const fire = (keys: Keys, init: KeyInit): { prevented: boolean } => {
  let prevented = false;
  const e: CommitKeysEvent = { ...init, preventDefault: () => (prevented = true) };
  keys.onKeyDown(e);
  return { prevented };
};

/*
 * The harnesses mirror the wiring inside the components: every edit or value
 * write-back re-renders, each event is answered by the handlers of the
 * latest render, and the blur that Enter dispatches runs the blur handler of
 * that same render — as the browser does inside the keydown.
 *
 * The vitest environment here is node (no jsdom in the dependency tree), so
 * the contract is exercised at the exact code the components run, not
 * through a rendered tree.
 */
function textHarness(value: string, onCommit: (draft: string) => void) {
  let draft = value;
  let focused = true;
  const skipNextBlur = { current: false };
  let keys: Keys;
  const rerender = () => {
    keys = commitInputKeys({
      dirty: draft !== value,
      commit: () => {
        if (draft !== value) onCommit(draft);
      },
      restore: () => {
        draft = value;
        rerender();
      },
      blur: () => {
        focused = false;
        keys.onBlur();
      },
      skipNextBlur,
    });
  };
  rerender();
  return {
    type: (text: string) => {
      draft = text;
      rerender();
    },
    /** The parent wrote the committed value back (the save landed). */
    writtenBack: (next: string) => {
      value = next;
      rerender();
    },
    key: (init: KeyInit) => fire(keys, init),
    focus: () => {
      focused = true;
      keys.onFocus();
    },
    blurAway: () => {
      focused = false;
      keys.onBlur();
    },
    get draft() {
      return draft;
    },
    get focused() {
      return focused;
    },
  };
}

function numberHarness(
  value: number | null,
  onCommit: (next: number | null) => void,
  limits: { min?: number; max?: number; integer?: boolean } = {},
) {
  let draft = value === null ? "" : String(value);
  let focused = true;
  const skipNextBlur = { current: false };
  let keys: Keys;
  const text = () => (value === null ? "" : String(value));
  const rerender = () => {
    keys = commitInputKeys({
      dirty: draft !== text(),
      commit: () => {
        const result = numberDraftCommit(draft, value, limits);
        if (result.kind === "commit") onCommit(result.value);
        draft = result.canonical;
        rerender();
      },
      restore: () => {
        draft = text();
        rerender();
      },
      blur: () => {
        focused = false;
        keys.onBlur();
      },
      skipNextBlur,
    });
  };
  rerender();
  return {
    type: (text: string) => {
      draft = text;
      rerender();
    },
    writtenBack: (next: number | null) => {
      value = next;
      rerender();
    },
    key: (init: KeyInit) => fire(keys, init),
    focus: () => {
      focused = true;
      keys.onFocus();
    },
    blurAway: () => {
      focused = false;
      keys.onBlur();
    },
    get draft() {
      return draft;
    },
    get focused() {
      return focused;
    },
  };
}

describe("one Enter is one commit intent", () => {
  it("TextField commits exactly once and leaves the field", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("ab");
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith("ab");
    expect(f.focused).toBe(false);
  });

  it("CommitInput commits exactly once and leaves the field", () => {
    const onCommit = vi.fn();
    const f = textHarness("x", onCommit);
    f.type("xy");
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith("xy");
    expect(f.focused).toBe(false);
  });

  it("CompactNumber commits exactly once and leaves the field", () => {
    const onCommit = vi.fn();
    const f = numberHarness(5, onCommit);
    f.type("6");
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith(6);
    expect(f.focused).toBe(false);
  });

  it("CommitNumber commits exactly once and leaves the field", () => {
    const onCommit = vi.fn();
    const f = numberHarness(null, onCommit, { min: 0 });
    f.type("3");
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith(3);
    expect(f.focused).toBe(false);
  });
});

describe("blur does not repeat the intent", () => {
  it("the blur Enter causes commits nothing further (text)", () => {
    const onCommit = vi.fn();
    // Wired as the components are: the value prop has not been written back
    // when the blur arrives, so the draft still looks dirty.
    let value = "a";
    let draft = "a";
    const skipNextBlur = { current: false };
    let keys!: Keys;
    const rerender = () => {
      keys = commitInputKeys({
        dirty: draft !== value,
        commit: () => {
          if (draft !== value) onCommit(draft);
        },
        restore: () => {
          draft = value;
        },
        blur: () => keys.onBlur(),
        skipNextBlur,
      });
    };
    rerender();
    draft = "ab";
    rerender();
    keys.onKeyDown({ key: "Enter", preventDefault: () => {} });
    // One commit — the old wiring made this two, the second from the blur
    // its own Enter dispatched.
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("the blur Enter causes commits nothing further (number)", () => {
    const onCommit = vi.fn();
    const f = numberHarness(5, onCommit);
    f.type("6");
    f.key({ key: "Enter" });
    // One commit — the old wiring made this two — and the draft is left
    // canonical.
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(f.draft).toBe("6");
    // Once the value is written back, blurring the clean field does nothing.
    f.writtenBack(6);
    f.focus();
    f.blurAway();
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("a plain blur still commits the dirty draft once", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("ab");
    f.blurAway();
    expect(onCommit).toHaveBeenCalledTimes(1);
  });

  it("after the value is written back the field is clean", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("ab");
    f.key({ key: "Enter" });
    f.writtenBack("ab");
    f.focus();
    f.blurAway();
    expect(onCommit).toHaveBeenCalledTimes(1);
  });
});

describe("a refused save leaves the draft retryable", () => {
  it("text: Enter again commits the same draft again", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("ab");
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(1);
    // The save is refused; the value prop never becomes "ab", so the draft
    // must still count as dirty.
    f.focus();
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(2);
    expect(f.draft).toBe("ab");
  });

  it("number: Enter again commits the same draft again", () => {
    const onCommit = vi.fn();
    const f = numberHarness(5, onCommit);
    f.type("6");
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(1);
    f.focus();
    f.key({ key: "Enter" });
    expect(onCommit).toHaveBeenCalledTimes(2);
    expect(f.draft).toBe("6");
  });
});

describe("an IME composition owns its Enter", () => {
  it("text: isComposing and keyCode 229 neither commit nor blur", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("ab");
    f.key({ key: "Enter", isComposing: true });
    expect(onCommit).not.toHaveBeenCalled();
    expect(f.focused).toBe(true);
    f.key({ key: "Enter", keyCode: 229 });
    expect(onCommit).not.toHaveBeenCalled();
    expect(f.focused).toBe(true);
  });

  it("number: isComposing Enter neither commits nor blurs", () => {
    const onCommit = vi.fn();
    const f = numberHarness(5, onCommit);
    f.type("6");
    f.key({ key: "Enter", isComposing: true });
    expect(onCommit).not.toHaveBeenCalled();
    expect(f.focused).toBe(true);
    f.key({ key: "Enter", keyCode: 229 });
    expect(onCommit).not.toHaveBeenCalled();
    expect(f.focused).toBe(true);
  });
});

describe("Escape restores before it closes", () => {
  it("a dirty text field consumes Escape: restore, no commit, event marked", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("ab");
    const { prevented } = f.key({ key: "Escape" });
    expect(f.draft).toBe("a");
    expect(onCommit).not.toHaveBeenCalled();
    expect(prevented).toBe(true);
    // The drawer around the field hears a marked event and stays open.
    expect(
      drawerEscapeCloses({ key: "Escape", defaultPrevented: true }, true),
    ).toBe(false);
    // Any other key the field let through never closes the layer around it —
    // select-all, shortcuts and typing all bubble unprevented.
    expect(drawerEscapeCloses({ key: "a", defaultPrevented: false }, true)).toBe(
      false,
    );
    expect(drawerEscapeCloses({ key: "Enter", defaultPrevented: false }, true)).toBe(
      false,
    );
  });

  it("a clean text field leaves Escape alone, and the drawer closes", () => {
    const onCommit = vi.fn();
    const f = textHarness("a", onCommit);
    f.type("a");
    const { prevented } = f.key({ key: "Escape" });
    expect(prevented).toBe(false);
    expect(
      drawerEscapeCloses({ key: "Escape", defaultPrevented: false }, true),
    ).toBe(true);
  });

  it("a dirty number field restores and consumes; a clean one bubbles", () => {
    const onCommit = vi.fn();
    const f = numberHarness(5, onCommit);
    f.type("9");
    const { prevented } = f.key({ key: "Escape" });
    expect(f.draft).toBe("5");
    expect(onCommit).not.toHaveBeenCalled();
    expect(prevented).toBe(true);
    expect(
      drawerEscapeCloses({ key: "Escape", defaultPrevented: true }, true),
    ).toBe(false);
    // The draft is canonical now: the next Escape belongs to the drawer.
    const again = f.key({ key: "Escape" });
    expect(again.prevented).toBe(false);
    expect(
      drawerEscapeCloses({ key: "Escape", defaultPrevented: false }, true),
    ).toBe(true);
  });

  it("the drawer close respects the overlay stack and IME composition", () => {
    expect(drawerEscapeCloses({ key: "Escape", defaultPrevented: false }, false)).toBe(
      false,
    );
    expect(
      drawerEscapeCloses({ key: "Escape", defaultPrevented: false, isComposing: true }, true),
    ).toBe(false);
    expect(
      drawerEscapeCloses({ key: "Escape", defaultPrevented: false, keyCode: 229 }, true),
    ).toBe(false);
  });
});

describe("numberDraftCommit", () => {
  it("commits a parsed number and canonicalizes the draft", () => {
    expect(numberDraftCommit("6", 5, {})).toEqual({
      kind: "commit",
      value: 6,
      canonical: "6",
    });
  });

  it("rounds and clamps when the limits say so", () => {
    expect(numberDraftCommit("7.6", 5, { integer: true })).toEqual({
      kind: "commit",
      value: 8,
      canonical: "8",
    });
    expect(numberDraftCommit("0", 5, { min: 1 })).toEqual({
      kind: "commit",
      value: 1,
      canonical: "1",
    });
    expect(numberDraftCommit("99", 5, { max: 10 })).toEqual({
      kind: "commit",
      value: 10,
      canonical: "10",
    });
  });

  it("treats an empty draft as clearing, and junk as a restore", () => {
    expect(numberDraftCommit("", 5, {})).toEqual({
      kind: "commit",
      value: null,
      canonical: "",
    });
    expect(numberDraftCommit("  ", 5, {})).toEqual({
      kind: "commit",
      value: null,
      canonical: "",
    });
    expect(numberDraftCommit("abc", 5, {})).toEqual({
      kind: "invalid",
      canonical: "5",
    });
  });

  it("keeps a draft that parses to the current value", () => {
    expect(numberDraftCommit("5.0", 5, {})).toEqual({
      kind: "same",
      canonical: "5",
    });
  });
});
