import { createContext, useContext, useLayoutEffect } from "react";
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
 * What a task is for, and so what its result may claim.
 *
 * A `"write"` commits the configuration: it is the only kind the save
 * vocabulary may describe. An `"observe"` re-reads state or records a reading
 * — the startup snapshot, a balance refresh, an account probe, a usage-history
 * import — so it moves what the pages show without rewriting the
 * configuration, and it may not claim a save however it ends. Both still share
 * the one serial chain: an observation that applied an older document over a
 * fresher write would roll the rebase point back, so the ordering protection
 * and the feedback honesty are the same mechanism, not two.
 */
export type TaskKind = "write" | "observe";

/**
 * What the queue does to the shell while it works.
 *
 * The queue owns ordering and the answer each caller receives; rendering is the
 * shell's business, so every state change goes through these hooks. The kind
 * travels with each event so the shell can keep its save vocabulary for the
 * tasks that earned it.
 */
export type SaveQueueHooks = {
  /** A task is starting. `kind` says whether a save is starting with it. */
  begin: (kind: TaskKind) => void;
  /** Record a task's committed snapshot as the page's data and the rebase point. */
  apply: (snapshot: Snapshot, kind: TaskKind) => void;
  /**
   * A task failed. `dropped` is how many edits were waiting behind a failed
   * *write* and are now discarded; zero means the failure was this caller's
   * alone. An observation holds nobody's place, so its failures never drop
   * anything and always arrive with zero.
   */
  fail: (cause: unknown, dropped: number, kind: TaskKind) => void;
  /** The chain is idle again. `kind` is the verdict the chain may end with. */
  finish: (kind: TaskKind) => void;
};

export type SaveQueue = {
  /**
   * Run one task, resolving true only when its own task completed. A write
   * also answers for what queued behind it, as before; an observation answers
   * for itself alone. `kind` is `"write"` unless the caller says otherwise —
   * every read and every observation says so.
   */
  run: (task: SaveTask, kind?: TaskKind) => Promise<boolean>;
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
  type Entry = { task: SaveTask; kind: TaskKind; answer: () => void; drop: () => void };
  const queued: Entry[] = [];
  // The verdict the chain now running may end with. A chain opened by an
  // observation stays observational until a write joins it.
  let chainKind: TaskKind = "write";
  let tail: Promise<boolean> = Promise.resolve(true);

  /**
   * Run one entry to completion. A failed write ends the chain — everything
   * still waiting behind it is discarded and told — while a failed observation
   * unsettles nothing and lets the chain go on.
   */
  const runEntry = async (entry: Entry): Promise<boolean> => {
    try {
      hooks.apply(await entry.task(), entry.kind);
      entry.answer();
      return true;
    } catch (e) {
      entry.drop();
      if (entry.kind === "write") {
        // A failed write invalidates the edits waiting behind it: the queue is
        // dropped and each waiting caller is told its save did not land.
        const dropped = queued.splice(0, queued.length);
        for (const waiting of dropped) waiting.drop();
        hooks.fail(e, dropped.length, "write");
        throw e;
      }
      hooks.fail(e, 0, "observe");
      return false;
    }
  };

  const run = (task: SaveTask, kind: TaskKind = "write"): Promise<boolean> => {
    if (running) {
      // The task may still be queued when the in-flight save fails; carry that
      // failure to this caller rather than answering before the attempt.
      let finish: (ok: boolean) => void = () => {};
      const settled = new Promise<boolean>((resolve) => {
        finish = resolve;
      });
      if (kind === "write" && chainKind !== "write") {
        // A write joined an observation's chain: the surface is saving from
        // this moment, and the chain may end with the write's verdict.
        chainKind = "write";
        hooks.begin("write");
      }
      queued.push({ task, kind, answer: () => finish(true), drop: () => finish(false) });
      return settled;
    }

    running = true;
    chainKind = kind;
    hooks.begin(kind);
    const previous = tail;
    const outcome = (async () => {
      // Wait for the save before this one so the queue stays a single chain and
      // this caller's result is its own task's result.
      await previous.catch(() => false);
      let openerOk = false;
      let chainFailed = false;
      try {
        // The caller's own task first, then every edit that queued behind it.
        // Each task reads the latest configuration when it runs, so later
        // edits rebase on earlier ones, and a queued caller is answered only
        // once its own task ran.
        openerOk = await runEntry({ task, kind, answer: () => {}, drop: () => {} });
        for (let entry = queued.shift(); entry; entry = queued.shift()) {
          await runEntry(entry);
        }
      } catch {
        // A failed write reported itself and told every caller behind it; the
        // chain simply ends without a success to report.
        chainFailed = true;
      } finally {
        running = false;
        hooks.finish(chainKind);
      }
      // A write answers for its chain: an edit queued behind it may have
      // landed or been dropped, and the caller must not be told "saved" while
      // that is still open. An observation answers for itself alone — its
      // reading applied or it did not, whatever happened to the edits that
      // queued behind it.
      return kind === "observe" ? openerOk : !chainFailed;
    })();
    tail = outcome;
    return outcome;
  };

  return { run };
}

/**
 * What the last save looked like, phrased for a status line.
 *
 * The top busybar answers "is the window working?" for the whole surface and
 * for screen readers. It cannot answer "did *the edit I just made* land?",
 * because it sits a window away from where the edit happened. This is the
 * same queue's truth, carried to where the eyes are: a drawer's foot.
 */
export type SaveStatus =
  | { phase: "idle" }
  | { phase: "saving" }
  | { phase: "saved"; at: number }
  | { phase: "error"; message: string };

export const SaveStatusContext = createContext<SaveStatus>({ phase: "idle" });

/** Read the save truth. Consumers re-render on every phase change, which is the point. */
export function useSaveStatus(): SaveStatus {
  return useContext(SaveStatusContext);
}

/**
 * The queue's truth, phrased as the verdict a mounted surface may speak.
 *
 * The queue reports kinds; this decides what each means for the words on
 * screen. Only a write claims "saved" or "save failed" — an observation that
 * ends, either way, leaves the last write's verdict exactly where it was, so a
 * balance refresh can no longer re-age a stamp it did not earn. A freshly
 * mounted surface inherits nothing: `reset` blanks the verdict and keeps the
 * outcome of any write that began before the mount quiet, so the next thing a
 * drawer says about saving is about its own work.
 */
export type SaveVerdict = {
  begin: (kind: TaskKind) => void;
  fail: (message: string, kind: TaskKind) => void;
  finish: (kind: TaskKind) => void;
  /** A drawer opened: what it says about saving starts from nothing. */
  reset: () => void;
};

export function createSaveVerdict(
  speak: (status: SaveStatus) => void,
  now: () => number = Date.now,
): SaveVerdict {
  let phase: SaveStatus = { phase: "idle" };
  // True while the write in flight began before the surface now open did: its
  // outcome belongs to a surface that is gone, and this one stays silent about
  // it rather than wearing another drawer's verdict.
  let inherited = false;

  const say = (next: SaveStatus) => {
    phase = next;
    speak(next);
  };

  return {
    begin: (kind) => {
      if (kind !== "write") return;
      inherited = false;
      say({ phase: "saving" });
    },
    fail: (message, kind) => {
      if (kind !== "write" || inherited) return;
      say({ phase: "error", message });
    },
    finish: (kind) => {
      // A failed write already spoke its error, and the error stands until
      // something new happens; a successful write stamps the moment.
      if (kind !== "write" || inherited || phase.phase === "error") return;
      say({ phase: "saved", at: now() });
    },
    reset: () => {
      inherited = true;
      say({ phase: "idle" });
    },
  };
}

/**
 * Where mounted surfaces ask for a blank verdict. The shell provides the
 * answer; the default is silence, so a surface rendered without the shell
 * (a test, a story) simply inherits nothing to clear.
 */
export const SaveFeedbackResetContext = createContext<() => void>(() => {});

/**
 * First thing inside a drawer's body: nothing an earlier surface saved or
 * failed survives into this one. Opening or re-targeting a drawer starts its
 * foot from idle, and the next verdict it shows is its own.
 */
export function SaveFeedbackReset(): null {
  const reset = useContext(SaveFeedbackResetContext);
  // A layout effect, so the inherited verdict is gone before the first paint
  // rather than one frame after it.
  useLayoutEffect(() => {
    reset();
  }, [reset]);
  return null;
}
