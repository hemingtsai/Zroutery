import { describe, expect, it } from "vitest";
import {
  createSaveQueue,
  createSaveVerdict,
  type SaveQueueHooks,
  type SaveStatus,
  type TaskKind,
} from "./saveQueue";
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

/**
 * The wiring the shell uses: the queue feeds the verdict, and every word the
 * verdict speaks is recorded. The clock is injected so a stamp's moment is
 * exact, and the kinds are recorded so a test can see which claims the chain
 * offered.
 */
function wiredVerdict(now: () => number) {
  const statuses: SaveStatus[] = [];
  const verdict = createSaveVerdict((s) => statuses.push(s), now);
  const begins: TaskKind[] = [];
  const finishes: TaskKind[] = [];
  const applied: string[] = [];
  const failures: Array<{ cause: unknown; dropped: number; kind: TaskKind }> = [];
  const queue = createSaveQueue({
    begin: (kind) => {
      begins.push(kind);
      verdict.begin(kind);
    },
    apply: (snap, kind) => {
      applied.push(`${kind}:${revisionOf(snap)}`);
    },
    fail: (cause, dropped, kind) => {
      failures.push({ cause, dropped, kind });
      verdict.fail(String(cause), kind);
    },
    finish: (kind) => {
      finishes.push(kind);
      verdict.finish(kind);
    },
  });
  return {
    queue,
    verdict,
    statuses,
    begins,
    finishes,
    applied,
    failures,
    /** The verdict a drawer's foot would be showing right now. */
    last: (): SaveStatus => statuses[statuses.length - 1] ?? { phase: "idle" },
  };
}

describe("save feedback scope", () => {
  it("the startup snapshot loads the pages and claims no save", async () => {
    const w = wiredVerdict(() => 1000);
    const boot = w.queue.run(async () => snapshot("boot"), "observe");
    // No "saving" flash while the read runs.
    expect(w.last()).toEqual({ phase: "idle" });
    expect(w.begins).toEqual(["observe"]);
    expect(await boot).toBe(true);
    // The pages get their data, and the verdict never spoke a save word.
    expect(w.applied).toEqual(["observe:boot"]);
    expect(w.statuses).toEqual([]);
    expect(w.last()).toEqual({ phase: "idle" });
  });

  it("a balance refresh answers as an observation and never re-ages a save", async () => {
    const w = wiredVerdict(() => 5000);
    // A write first, so there is a stamp an observation must not touch.
    expect(await w.queue.run(async () => snapshot("edit"))).toBe(true);
    expect(w.last()).toEqual({ phase: "saved", at: 5000 });
    const spoken = w.statuses.length;

    expect(await w.queue.run(async () => snapshot("balances"), "observe")).toBe(true);
    // The reading applied, and the stamp is still the write's own — not
    // re-stamped, not wiped, not turned into a fresh "saved".
    expect(w.applied).toContain("observe:balances");
    expect(w.last()).toEqual({ phase: "saved", at: 5000 });
    expect(w.statuses.length).toBe(spoken);
  });

  it("a failed refresh keeps the last reading and holds nobody's place", async () => {
    const w = wiredVerdict(() => 5000);
    const boom = w.queue.run(async () => {
      throw new Error("the panel refused");
    }, "observe");
    // An edit queued behind the failing observation must still land.
    const edit = w.queue.run(async () => snapshot("edit"));

    expect(await boom).toBe(false);
    expect(await edit).toBe(true);
    // The failed reading replaced nothing: the last valid value stands.
    expect(w.applied).toEqual(["write:edit"]);
    expect(w.failures).toEqual([
      { cause: new Error("the panel refused"), dropped: 0, kind: "observe" },
    ]);
    // The drawer is not told a save failed; the write's own verdict is.
    expect(w.statuses).toEqual([{ phase: "saving" }, { phase: "saved", at: 5000 }]);
  });

  it("a freshly opened drawer inherits nothing from the surface before it", () => {
    const w = wiredVerdict(() => 5000);

    // A write is in flight when another entity's drawer mounts. (A write's
    // begin fires the moment it joins the queue, so a begin the drawer never
    // saw is exactly one that began before its mount.)
    w.verdict.begin("write");
    w.verdict.reset();
    expect(w.last()).toEqual({ phase: "idle" });
    w.verdict.finish("write");
    // The outcome belongs to the surface that is gone: this one stays silent.
    expect(w.last()).toEqual({ phase: "idle" });

    // Nor does a verdict earned before the mount survive it.
    w.verdict.begin("write");
    w.verdict.finish("write");
    expect(w.last()).toEqual({ phase: "saved", at: 5000 });
    w.verdict.reset();
    expect(w.last()).toEqual({ phase: "idle" });

    // From the drawer's own first write on, the verdict is its own again.
    w.verdict.begin("write");
    expect(w.last()).toEqual({ phase: "saving" });
    w.verdict.finish("write");
    expect(w.last()).toEqual({ phase: "saved", at: 5000 });
  });

  it("the write path still answers saving, then saved", async () => {
    const w = wiredVerdict(() => 4242);
    const done = w.queue.run(async () => snapshot("edit"));
    expect(w.last()).toEqual({ phase: "saving" });
    expect(await done).toBe(true);
    expect(w.last()).toEqual({ phase: "saved", at: 4242 });
    expect(w.begins).toEqual(["write"]);
    expect(w.finishes).toEqual(["write"]);
  });

  it("a failed write still says so and keeps the error over later silence", async () => {
    const w = wiredVerdict(() => 5000);
    const boom = w.queue.run(async () => {
      throw new Error("disk full");
    });
    const queued = w.queue.run(async () => snapshot("second"));
    expect(await boom).toBe(false);
    expect(await queued).toBe(false);
    expect(w.last()).toEqual({ phase: "error", message: "Error: disk full" });

    // An observation after the failure neither clears nor re-ages it.
    expect(await w.queue.run(async () => snapshot("balances"), "observe")).toBe(true);
    expect(w.last()).toEqual({ phase: "error", message: "Error: disk full" });
  });

  it("a write queued behind an observation still gets its saving, then saved", async () => {
    const w = wiredVerdict(() => 777);
    const hold = deferred<Snapshot>();
    const observe = w.queue.run(() => hold.promise, "observe");
    const edit = w.queue.run(async () => snapshot("edit"));
    // The write's intent is visible the moment it joins the chain, not only
    // once the reading ahead of it finishes.
    expect(w.last()).toEqual({ phase: "saving" });
    expect(w.begins).toEqual(["observe", "write"]);

    hold.resolve(snapshot("balances"));
    expect(await observe).toBe(true);
    expect(await edit).toBe(true);
    expect(w.applied).toEqual(["observe:balances", "write:edit"]);
    expect(w.last()).toEqual({ phase: "saved", at: 777 });
    expect(w.finishes).toEqual(["write"]);
  });
});
