/**
 * The add-provider submission: a DOM-free save-queue unit with its outcome
 * taxonomy, exported and unit-tested (Providers.test.ts). The form calls
 * it; the page never touches it.
 */
import { emptyProvider, slugify, type AppConfig, type Provider, type ProviderKind } from "../../api";
import { defaultBaseUrl, nameFromBaseUrl } from "./kindMeta";

/**
 * One submit intent's draft: what the three fields hold when it is made.
 */
export type AddProviderDraft = {
  name: string;
  baseUrl: string;
  kind: ProviderKind;
};

/**
 * How a submit intent ended.
 *
 * `added` is the only outcome that may clear the draft, fold the form and
 * open the new provider's drawer — the failure the review's O5 recorded was
 * exactly those effects running on a save nobody waited for. `duplicate`
 * covers both a name already on the page and the race where a queued save
 * lands on a document that has the id by now: the form stays open, and the
 * drawer of the *existing* provider must not open over it. `write_failed` is
 * a save the backend refused or the queue dropped — including the stale
 * rejection when the configuration changed underneath the save — and the
 * draft is kept as typed for a retry.
 */
export type AddProviderOutcome =
  | { kind: "added"; id: string }
  | { kind: "duplicate"; id: string }
  | { kind: "name_required" }
  | { kind: "write_failed" }
  | { kind: "ignored" };

/**
 * The add form's submission as one unit, the way `saveQueue` is.
 *
 * `run` turns one intent into at most one `save` call and reports how it
 * ended; the form applies the outcome to its state. A second intent while one
 * is still in flight is ignored — one press, one write, never a queue of
 * duplicates. Extracted from the component so the failure paths stay testable
 * without a DOM.
 */
export function createAddProviderSubmission(
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>,
) {
  let inFlight = false;
  return {
    /** Whether an intent is mid-save; the form's button waits on this. */
    inFlight: () => inFlight,
    async run(
      draft: AddProviderDraft,
      providers: Provider[],
    ): Promise<AddProviderOutcome> {
      if (inFlight) return { kind: "ignored" };
      const name = draft.name.trim() || nameFromBaseUrl(draft.baseUrl);
      if (!name) return { kind: "name_required" };
      const id = slugify(name);
      if (providers.some((p) => p.id === id)) return { kind: "duplicate", id };
      inFlight = true;
      try {
        // `committed` separates "the queue ran and the write landed" from
        // "the queue ran and found the provider already there": a mutate that
        // returns null also resolves true, and only the first is an add.
        let committed = false;
        const ok = await save((cfg) => {
          // Re-checked against the document being committed: a save that
          // queues behind another one rebases onto the newer configuration,
          // and the id may have been taken in between.
          if (cfg.providers.some((p) => p.id === id)) return null;
          const next = structuredClone(cfg);
          next.providers.push(
            emptyProvider(
              id,
              name,
              draft.kind,
              draft.baseUrl.trim() || defaultBaseUrl(draft.kind),
            ),
          );
          committed = true;
          return next;
        });
        if (!ok) return { kind: "write_failed" };
        if (!committed) return { kind: "duplicate", id };
        return { kind: "added", id };
      } finally {
        inFlight = false;
      }
    },
  };
}
