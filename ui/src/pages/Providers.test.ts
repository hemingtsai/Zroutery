import { describe, expect, it, vi } from "vitest";
import type { AppConfig, Provider } from "../api";
import { createAddProviderSubmission, type AddProviderDraft } from "./Providers";

/**
 * The add form's submission covers the failure paths the experience review's
 * O5 recorded: a save the backend refuses used to fall through to the success
 * path, so the draft was wiped, the form folded and no drawer opened. This
 * unit is the part of the form that decides; the component applies the
 * outcome to its state, and only an `added` outcome clears anything or opens
 * a drawer.
 */

/** A config the fake queue hands the mutate; only the field the mutate reads. */
function configWith(providers: Provider[]): AppConfig {
  return { providers } as unknown as AppConfig;
}

/** A stand-in row: the submission only ever reads `id` from the page's list. */
function row(id: string): Provider {
  return { id } as unknown as Provider;
}

function draft(over: Partial<AddProviderDraft> = {}): AddProviderDraft {
  return {
    name: "DeepSeek Relay",
    baseUrl: "https://api.deepseek.com/v1",
    kind: "openai_compatible",
    ...over,
  };
}

type Mutate = (config: AppConfig) => AppConfig | null;

/**
 * A save that behaves like the real queue: the mutate runs against a fresh
 * copy of the committed document, a mutate with nothing to change answers
 * `true` without a write, and `writeOk: false` is `save_config` refusing.
 */
function queueLikeSave(existing: Provider[], writeOk: boolean) {
  const mutates: Mutate[] = [];
  const written: Provider[] = [];
  const save = async (mutate: Mutate): Promise<boolean> => {
    mutates.push(mutate);
    const next = mutate(configWith([...existing]));
    if (!next) return true;
    if (!writeOk) return false;
    written.push(...next.providers);
    return true;
  };
  return { save, mutates, written };
}

describe("AddProviderForm submission", () => {
  it("a refused save is a write failure: one write attempted, nothing committed, never an add", async () => {
    const { save, mutates, written } = queueLikeSave([], false);
    const outcome = await createAddProviderSubmission(save).run(draft(), []);

    expect(outcome).toEqual({ kind: "write_failed" });
    expect(mutates).toHaveLength(1);
    expect(written).toHaveLength(0);
  });

  it("a committed add reports the new id — the form clears and opens that entity", async () => {
    const { save, written } = queueLikeSave([], true);
    const outcome = await createAddProviderSubmission(save).run(
      draft({ baseUrl: "" }),
      [],
    );

    expect(outcome).toEqual({ kind: "added", id: "deepseek-relay" });
    expect(written).toHaveLength(1);
    expect(written[0]).toMatchObject({
      id: "deepseek-relay",
      name: "DeepSeek Relay",
      kind: "openai_compatible",
      base_url: "https://api.openai.com/v1",
    });
  });

  it("one intent, one write: a second press while the save is in flight is ignored", async () => {
    let release!: () => void;
    const save = vi.fn((mutate: Mutate) => {
      mutate(configWith([])); // the mutate runs before the write, as in App.save
      return new Promise<boolean>((resolve) => {
        release = () => resolve(true);
      });
    });
    const submission = createAddProviderSubmission(save);

    const first = submission.run(draft(), []);
    const second = await submission.run(draft(), []);
    release();

    expect(await first).toEqual({ kind: "added", id: "deepseek-relay" });
    expect(second).toEqual({ kind: "ignored" });
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("an id taken while the save queued is a duplicate, not an add: the existing entity is not opened", async () => {
    const { save, written } = queueLikeSave([row("deepseek-relay")], true);
    const outcome = await createAddProviderSubmission(save).run(draft(), []);

    expect(outcome).toEqual({ kind: "duplicate", id: "deepseek-relay" });
    expect(written).toHaveLength(0);
  });

  it("an id already on the page never reaches the save", async () => {
    const save = vi.fn(async () => true);
    const outcome = await createAddProviderSubmission(save).run(
      draft(),
      [row("deepseek-relay")],
    );

    expect(outcome).toEqual({ kind: "duplicate", id: "deepseek-relay" });
    expect(save).not.toHaveBeenCalled();
  });

  it("an empty draft never reaches the save", async () => {
    const save = vi.fn(async () => true);
    const outcome = await createAddProviderSubmission(save).run(
      draft({ name: "  ", baseUrl: "   " }),
      [],
    );

    expect(outcome).toEqual({ kind: "name_required" });
    expect(save).not.toHaveBeenCalled();
  });
});
