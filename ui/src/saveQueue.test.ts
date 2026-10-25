import { describe, expect, it } from "vitest";
import { createSaveQueue, type SaveQueueHooks } from "./saveQueue";
import type { Snapshot } from "./api";

/**
 * A snapshot whose revision identifies it. The queue never reads inside the
 * document, so the fixture is deliberately only the field the assertions name.
 */
function snapshot(revision: string): Snapshot {
  return { config: { revision } } as unknown as Snapshot;
}

function revisionOf(snap: Snapshot): string {
  return (snap.config as unknown as { revision: string }).revision;
}

/** A promise the test settles by hand, so interleavings are exact. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function recordingHooks() {
  const applied: string[] = [];
  const failures: Array<{ cause: unknown; dropped: number }> = [];
  let begins = 0;
  let finishes = 0;
  const hooks: SaveQueueHooks = {
    begin: () => {
      begins += 1;
    },
    apply: (snap) => {
      applied.push(revisionOf(snap));
    },
    fail: (cause, dropped) => {
      failures.push({ cause, dropped });
    },
    finish: () => {
      finishes += 1;
    },
  };
  return {
    hooks,
    applied,
    failures,
    counts: () => ({ begins, finishes }),
  };
}

describe("save queue", () => {
  it("commits saves in order, each after the one before it finished", async () => {
    const { hooks, applied, counts } = recordingHooks();
    const queue = createSaveQueue(hooks);

    const first = deferred<Snapshot>();
    const firstRun = queue.run(() => first.promise);
    const secondRun = queue.run(async () => snapshot("second"));
    const thirdRun = queue.run(async () => snapshot("third"));

    // The two later edits are waiting; nothing but the first task may commit.
    expect(applied).toEqual([]);
    first.resolve(snapshot("first"));

    expect(await firstRun).toBe(true);
    expect(await secondRun).toBe(true);
    expect(await thirdRun).toBe(true);
    expect(applied).toEqual(["first", "second", "third"]);
    expect(counts()).toEqual({ begins: 1, finishes: 1 });
  });

  it("drops the queued edits and fails every caller when the in-flight save fails", async () => {
    const { hooks, applied, failures, counts } = recordingHooks();
    const queue = createSaveQueue(hooks);

    const first = deferred<Snapshot>();
    const firstRun = queue.run(() => first.promise);
    const queuedRuns = [
      queue.run(async () => snapshot("second")),
      queue.run(async () => snapshot("third")),
    ];

    const cause = new Error("the config file could not be replaced");
    first.reject(cause);

    expect(await firstRun).toBe(false);
    // A queued caller hears about the failure only after the attempt it was
    // waiting behind, and never before its own task had a chance to run.
    expect(await Promise.all(queuedRuns)).toEqual([false, false]);
    expect(applied).toEqual([]);
    expect(failures).toEqual([{ cause, dropped: 2 }]);
    expect(counts()).toEqual({ begins: 1, finishes: 1 });
  });

  it("answers a queued caller only after its own task has run", async () => {
    const { hooks } = recordingHooks();
    const queue = createSaveQueue(hooks);
    const events: string[] = [];

    const first = deferred<Snapshot>();
    const firstRun = queue.run(() => first.promise).then((ok) => {
      events.push(`first ${ok}`);
      return ok;
    });
    const secondRun = queue
      .run(async () => {
        events.push("second ran");
        return snapshot("second");
      })
      .then((ok) => {
        events.push(`second ${ok}`);
        return ok;
      });

    first.resolve(snapshot("first"));
    expect(await firstRun).toBe(true);
    expect(await secondRun).toBe(true);
    // The queued caller is answered inside the drain, before the in-flight
    // caller's own promise settles: the chain returns only after the queue is
    // empty, so the first caller hears about its success last. What matters is
    // that "second true" comes after "second ran" and never before it.
    expect(events).toEqual(["second ran", "second true", "first true"]);
  });
});
