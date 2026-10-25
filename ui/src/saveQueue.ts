import type { Snapshot } from "./api";

/**
 * One save, as a task that resolves to the snapshot it committed.
 *
 * Tasks are updaters against the newest committed configuration, not against
 * the snapshot the caller happened to hold: by the time a queued edit runs, the
 * save ahead of it may have changed the document it is editing.
 */
export type SaveTask = () => Promise<Snapshot>;

/**
 * What the queue does to the shell while it works.
 *
 * The queue owns ordering and the answer each caller receives; rendering is the
 * shell's business, so every state change goes through these hooks.
 */
export type SaveQueueHooks = {
  /** A save is starting: clear the previous error and mark the window busy. */
  begin: () => void;
  /** Record a committed snapshot as both the page's data and the rebase point. */
  apply: (snapshot: Snapshot) => void;
  /**
   * A save failed. `dropped` is how many edits were waiting behind it and are
   * now discarded; zero means the failure was this caller's alone.
   */
  fail: (cause: unknown, dropped: number) => void;
  /** The chain is idle again, whether it succeeded or failed. */
  finish: () => void;
};

export type SaveQueue = {
  /** Run one save, resolving true only when its own task committed. */
  run: (task: SaveTask) => Promise<boolean>;
};

/**
 * Serialise saves so the configuration file is written in the order the user
 * made the edits.
 *
 * Every save round trips through Rust and rewrites the config file, so two
 * overlapping saves could interleave and land out of order. Tasks arriving
 * mid-flight are queued in order and run one after another.
 *
 * A caller's promise resolves only when *its own* task has run: answering a
 * queued save with `true` before it ran would report an edit as saved that the
 * queue might still drop, and a failed in-flight save resolves every waiting
 * caller with `false` instead of discarding them silently.
 */
export function createSaveQueue(hooks: SaveQueueHooks): SaveQueue {
  let running = false;
  const queued: Array<{ task: SaveTask; answer: () => void; drop: () => void }> = [];
  let tail: Promise<boolean> = Promise.resolve(true);

  const run = (task: SaveTask): Promise<boolean> => {
    if (running) {
      // The task may still be queued when the in-flight save fails; carry that
      // failure to this caller rather than answering before the attempt.
      let finish: (ok: boolean) => void = () => {};
      const settled = new Promise<boolean>((resolve) => {
        finish = resolve;
      });
      queued.push({ task, answer: () => finish(true), drop: () => finish(false) });
      return settled;
    }

    running = true;
    hooks.begin();
    const previous = tail;
    const outcome = (async () => {
      // Wait for the save before this one so the queue stays a single chain and
      // this caller's result is its own task's result.
      await previous.catch(() => false);
      try {
        hooks.apply(await task());
        // Drain every edit that arrived while this one was in flight. Each task
        // reads the latest configuration when it runs, so later edits rebase on
        // earlier ones, and a queued caller is answered only once its own task
        // ran.
        while (queued.length > 0) {
          const entry = queued.shift()!;
          try {
            hooks.apply(await entry.task());
            entry.answer();
          } catch (e) {
            entry.drop();
            throw e;
          }
        }
        return true;
      } catch (e) {
        // A failed save invalidates the edits waiting behind it: the queue is
        // dropped and each waiting caller is told its save did not land.
        const dropped = queued.splice(0, queued.length);
        for (const entry of dropped) entry.drop();
        hooks.fail(e, dropped.length);
        return false;
      } finally {
        running = false;
        hooks.finish();
      }
    })();
    tail = outcome;
    return outcome;
  };

  return { run };
}
