/**
 * The key contract the four commit inputs share — TextField, CompactNumber,
 * CommitInput, CommitNumber — in one place so the four cannot drift:
 *
 * Enter is one commit intent. It commits and takes the focus out, and the
 * blur that the field's own Enter causes must not commit the same draft a
 * second time before the parent has written the value back — that echo used
 * to fire two saves per keypress. The echo is skipped by a mark, not by
 * pretending the baseline moved: a commit whose save fails never becomes the
 * value, so a draft judged against a moved baseline would look clean and
 * could never be retried. With the mark, a refused draft is still dirty and
 * the next Enter commits it again.
 *
 * An IME composition owns its keys: the Enter that confirms a candidate —
 * `isComposing`, or Safari's keyCode 229 — neither commits nor blurs, and
 * neither does the Escape that cancels one.
 *
 * Escape on a dirty draft restores the committed value and is consumed here
 * (`preventDefault`): the drawer around the field hears a marked event and
 * stays open, so the first Escape restores and the next one closes the
 * layer. A clean draft leaves the event alone, and it bubbles to the drawer.
 *
 * Exported because the contract is subtle enough to be worth unit-testing
 * directly. The components rebuild this per render with their own draft
 * state and commit rules; the tests drive the same wiring.
 */
import { useEffect, useRef, useState } from "react";
import { Field } from "./field";

export type CommitKeysEvent = {
  key: string;
  isComposing?: boolean;
  keyCode?: number | undefined;
  preventDefault(): void;
};

export function commitInputKeys(state: {
  /** Whether committing the draft now would change the value. */
  dirty: boolean;
  /** Hand the draft to the caller's onCommit, which still dedupes equal values. */
  commit: () => void;
  /** Put the committed value back into the draft. */
  restore: () => void;
  /** Take the focus out of the field (Enter hands the intent over). */
  blur: () => void;
  /**
   * Enter's mark on its own blur. A ref the component owns, so the flag
   * survives the re-render a commit causes and a refocus always starts a
   * fresh intent.
   */
  skipNextBlur: { current: boolean };
}) {
  return {
    onFocus: () => {
      // A fresh focus is a fresh intent; a stale mark must never eat a real
      // blur commit.
      state.skipNextBlur.current = false;
    },
    onBlur: () => {
      const skip = state.skipNextBlur.current;
      state.skipNextBlur.current = false;
      if (!skip) state.commit();
    },
    onKeyDown: (e: CommitKeysEvent) => {
      if (e.isComposing || e.keyCode === 229) return;
      if (e.key === "Enter") {
        state.skipNextBlur.current = true;
        state.commit();
        state.blur();
      } else if (e.key === "Escape" && state.dirty) {
        state.restore();
        e.preventDefault();
      }
    },
  };
}

/**
 * The commit decision both number inputs share: an empty draft clears,
 * junk restores, and a number clamps to the declared range — rounding first
 * when whole numbers were asked for — committing only when it differs from
 * the current value. `canonical` is the draft the field shows afterwards.
 */
export function numberDraftCommit(
  draft: string,
  value: number | null,
  limits: { min?: number; max?: number; integer?: boolean },
): { kind: "commit"; value: number | null; canonical: string } | { kind: "same"; canonical: string } | { kind: "invalid"; canonical: string } {
  if (draft.trim() === "") {
    return value !== null
      ? { kind: "commit", value: null, canonical: "" }
      : { kind: "same", canonical: "" };
  }
  const parsed = Number(draft);
  if (!Number.isFinite(parsed)) {
    return { kind: "invalid", canonical: String(value ?? "") };
  }
  let next = limits.integer ? Math.round(parsed) : parsed;
  if (limits.min !== undefined) next = Math.max(limits.min, next);
  if (limits.max !== undefined) next = Math.min(limits.max, next);
  return next !== value
    ? { kind: "commit", value: next, canonical: String(next) }
    : { kind: "same", canonical: String(next) };
}

/**
 * Text input that reports its value when the user is done with it.
 *
 * Every commit round trips through Rust and rewrites the configuration file, so
 * doing that per keystroke is both wasteful and a good way to save half-typed
 * URLs. Enter commits once and blurs — the blur its own Enter causes does not
 * repeat the commit, and a refused save leaves the draft dirty enough to
 * retry ([`commitInputKeys`] holds the contract, shared with the other three
 * commit inputs). Blur on its own commits. Escape restores the draft and is
 * consumed before it can close a drawer around the field.
 */
export function TextField({
  label,
  hint,
  value,
  onCommit,
  placeholder,
  wide,
  readOnly,
  password,
  error,
  ariaLabel,
}: {
  label: string;
  hint?: string;
  value: string;
  onCommit: (value: string) => void;
  placeholder?: string;
  wide?: boolean;
  readOnly?: boolean;
  password?: boolean;
  /** Inline problem with the current value; shown under the input in danger tone. */
  error?: string | null;
  /** Accessible name, for inputs whose visible label is generic. */
  ariaLabel?: string;
}) {
  const [draft, setDraft] = useState(value);
  // Adopt values that changed underneath us, e.g. after a save elsewhere.
  useEffect(() => setDraft(value), [value]);
  const inputRef = useRef<HTMLInputElement>(null);
  const skipNextBlur = useRef(false);
  const keys = commitInputKeys({
    dirty: draft !== value,
    commit: () => {
      if (draft !== value) onCommit(draft);
    },
    restore: () => setDraft(value),
    blur: () => inputRef.current?.blur(),
    skipNextBlur,
  });

  return (
    <Field label={label} hint={error ?? hint} wide={wide} danger={Boolean(error)}>
      <input
        ref={inputRef}
        className={error ? "input-error" : undefined}
        type={password ? "password" : "text"}
        autoComplete={password ? "off" : undefined}
        value={draft}
        placeholder={placeholder}
        readOnly={readOnly}
        aria-label={ariaLabel}
        // The dirty mark the drawer's Escape yield reads: Radix answers
        // Escape in the capture phase, before this handler can mark the
        // event, so the mark has to live on the element instead.
        data-commit-dirty={draft !== value ? "" : undefined}
        onChange={(e) => setDraft(e.currentTarget.value)}
        onFocus={keys.onFocus}
        onBlur={keys.onBlur}
        onKeyDown={keys.onKeyDown}
      />
    </Field>
  );
}

/** Same idea as [`TextField`] for numbers, with clamping on commit. */
export function NumberField({
  label,
  hint,
  value,
  onCommit,
  min,
  max,
  placeholder,
  integer,
  ariaLabel,
}: {
  label: string;
  hint?: string;
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  /** Round committed values to whole numbers (counts, ports, seconds). */
  integer?: boolean;
  /** Accessible name, for inputs whose visible label is generic. */
  ariaLabel?: string;
}) {
  return (
    <Field label={label} hint={hint}>
      <CompactNumber
        value={value}
        onCommit={onCommit}
        min={min}
        max={max}
        placeholder={placeholder}
        integer={integer}
        ariaLabel={ariaLabel}
      />
    </Field>
  );
}

/**
 * A bare [`NumberField`]-style input for tight spots like table cells, where a
 * labelled field will not fit. Commits on blur or Enter, reverts on Escape;
 * saving half-typed numbers per keystroke would persist "1" on the way to "10".
 * Enter commits once — its own blur does not repeat it — and Escape on a dirty
 * draft restores and is consumed; [`commitInputKeys`] holds the contract.
 */
export function CompactNumber({
  value,
  onCommit,
  min,
  max,
  placeholder,
  ariaLabel,
  integer,
}: {
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  ariaLabel?: string;
  /** Round committed values to whole numbers (counts, ports, seconds). */
  integer?: boolean;
}) {
  const text = value === null ? "" : String(value);
  const [draft, setDraft] = useState(text);
  useEffect(() => setDraft(text), [text]);
  const inputRef = useRef<HTMLInputElement>(null);
  const skipNextBlur = useRef(false);

  const commit = () => {
    const result = numberDraftCommit(draft, value, { min, max, integer });
    if (result.kind === "commit") onCommit(result.value);
    setDraft(result.canonical);
  };

  const keys = commitInputKeys({
    dirty: draft !== text,
    commit,
    restore: () => setDraft(text),
    blur: () => inputRef.current?.blur(),
    skipNextBlur,
  });

  return (
    <input
      ref={inputRef}
      type="number"
      className="tiny"
      min={min}
      max={max}
      step={integer ? 1 : "any"}
      placeholder={placeholder}
      aria-label={ariaLabel}
      // Dirty mark for the drawer's Escape yield (see TextField).
      data-commit-dirty={draft !== text ? "" : undefined}
      value={draft}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onFocus={keys.onFocus}
      onBlur={keys.onBlur}
      onKeyDown={keys.onKeyDown}
    />
  );
}

/** Bare input that commits on blur or Enter, reverts on Escape. No label — use with setting-row.
 * Enter commits once and Escape on a dirty draft is consumed ([`commitInputKeys`] holds the contract). */
export function CommitInput({
  value,
  onCommit,
  placeholder,
  type = "text",
  ariaLabel,
}: {
  value: string;
  onCommit: (value: string) => void;
  placeholder?: string;
  type?: "text" | "password";
  /**
   * Required in practice.
   *
   * Unlike [`Field`], which wraps its child in a `<label>` and so names it, this
   * is used bare inside `.setting-row` — a div whose title is a `<span>`, which
   * names nothing. Without this the control announces as an unlabelled text box
   * and the reader has no way to know which setting they are in. The visible
   * `.setting-title` is passed as the name, so the two cannot drift.
   */
  ariaLabel: string;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const inputRef = useRef<HTMLInputElement>(null);
  const skipNextBlur = useRef(false);
  const keys = commitInputKeys({
    dirty: draft !== value,
    commit: () => {
      if (draft !== value) onCommit(draft);
    },
    restore: () => setDraft(value),
    blur: () => inputRef.current?.blur(),
    skipNextBlur,
  });

  return (
    <input
      ref={inputRef}
      type={type}
      value={draft}
      aria-label={ariaLabel}
      placeholder={placeholder}
      // Dirty mark for the drawer's Escape yield (see TextField).
      data-commit-dirty={draft !== value ? "" : undefined}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onFocus={keys.onFocus}
      onBlur={keys.onBlur}
      onKeyDown={keys.onKeyDown}
    />
  );
}

/** Same as CommitInput but for numbers. */
export function CommitNumber({
  value,
  onCommit,
  min,
  max,
  integer,
  ariaLabel,
}: {
  value: number | null;
  onCommit: (value: number | null) => void;
  min?: number;
  max?: number;
  integer?: boolean;
  /** Required in practice — see [`CommitInput`]. */
  ariaLabel: string;
}) {
  const text = value === null ? "" : String(value);
  const [draft, setDraft] = useState(text);
  useEffect(() => setDraft(text), [text]);
  const inputRef = useRef<HTMLInputElement>(null);
  const skipNextBlur = useRef(false);

  const commit = () => {
    const result = numberDraftCommit(draft, value, { min, max, integer });
    if (result.kind === "commit") onCommit(result.value);
    setDraft(result.canonical);
  };

  const keys = commitInputKeys({
    dirty: draft !== text,
    commit,
    restore: () => setDraft(text),
    blur: () => inputRef.current?.blur(),
    skipNextBlur,
  });

  return (
    <input
      ref={inputRef}
      type="number"
      className="tiny"
      aria-label={ariaLabel}
      min={min}
      max={max}
      step={integer ? 1 : "any"}
      value={draft}
      // Dirty mark for the drawer's Escape yield (see TextField).
      data-commit-dirty={draft !== text ? "" : undefined}
      onChange={(e) => setDraft(e.currentTarget.value)}
      onFocus={keys.onFocus}
      onBlur={keys.onBlur}
      onKeyDown={keys.onKeyDown}
    />
  );
}
