/**
 * A live Tauri bridge for previewing the dashboard against real backend state.
 *
 * ## Why this exists
 *
 * `mock.html` renders the application against invented fixtures. That is enough
 * to check layout and impossible to use for asking "what does this page look
 * like with *my* providers, *my* history and *my* account?" — the answer needs
 * the backend's own documents, not a fixture written to match them.
 *
 * The desktop binary owns its state over Tauri IPC, which a browser cannot
 * speak. This module installs the same `window.__TAURI_INTERNALS__` shape and
 * forwards every command to a local preview server (`src-tauri/examples/
 * preview.rs`). That server builds the real `Desktop` over a read-only copy of
 * the running application's configuration directory, so every answer comes from
 * the same Rust code that answers the desktop window.
 *
 * ## What it is not
 *
 * It is not the desktop app. The server is opened over a copy of the data
 * directory, deliberately: a preview must not write the configuration the
 * running application is using, and a page that can rotate a key from a second
 * process is worse than no preview. Commands that would mutate anything are
 * refused by the preview server; the page still renders the control and the
 * refusal is shown as an ordinary error toast.
 *
 * The gateway listener is not started, so the dashboard renders the server as
 * stopped. That is the one intentional difference from the running window: a
 * second listener on the same port is not something a preview may do.
 *
 * ## Running it
 *
 *     cargo run -p zroutery --example preview
 *     pnpm --dir ui dev
 *     # then open http://localhost:5174/preview.html
 *
 * `?backend=http://127.0.0.1:8786` names another preview server. Neither this
 * file nor `preview.html` is referenced by the production build: `vite build`
 * only follows `index.html`.
 */

type Json = unknown;

const backend = (() => {
  const named = new URLSearchParams(window.location.search).get("backend");
  return (named && named.trim()) || "http://127.0.0.1:8786";
})();

const callbacks = new Map<number, (payload: any) => void>();
let nextCallback = 1;

async function call(cmd: string, args?: Json): Promise<Json> {
  // Clipboard writes are a webview capability in the real app; a browser has
  // its own. Keeping this here means the copy buttons still do what they say.
  if (cmd === "copy_text") {
    const text = String((args as { text?: unknown })?.text ?? "");
    await navigator.clipboard.writeText(text);
    return null;
  }

  // Events have no source in a preview: nothing else is writing this copy, so
  // a listener that never fires is the honest answer. The application polls
  // its own snapshots and does not depend on these events for correctness.
  if (cmd === "plugin:event|listen") return nextCallback++;
  if (cmd === "plugin:event|unlisten") return null;

  const response = await fetch(`${backend}/invoke`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cmd, args: args ?? {} }),
  });
  const body = (await response.json().catch(() => null)) as
    | { ok?: boolean; data?: Json; error?: string }
    | null;
  if (!response.ok || !body || body.ok !== true) {
    throw new Error(body?.error ?? `live-preview: ${cmd} failed (${response.status})`);
  }
  return body.data ?? null;
}

(window as any).__TAURI_INTERNALS__ = {
  invoke(cmd: string, args?: Json): Promise<Json> {
    return call(cmd, args);
  },
  transformCallback(cb: (payload: any) => void): number {
    const id = nextCallback++;
    callbacks.set(id, cb);
    return id;
  },
  unregisterCallback(id: number): void {
    callbacks.delete(id);
  },
  convertFileSrc(path: string): string {
    return path;
  },
};

(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener() {
    // Mirrors the mock bridge: the real plugin owns a set per event name, and
    // nothing in this preview registers one.
  },
};

document.title = "Zroutery · 实时预览（只读）";
console.info(`[live-preview] forwarding Tauri commands to ${backend}`);
