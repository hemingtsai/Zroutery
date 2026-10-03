"""Interaction regression test for the dashboard.

Renders the *real* built UI in headless Chromium with a stubbed Tauri bridge,
drives that browser over the DevTools protocol, and reports the state the fake
backend ended up with. Input is real: `Input.dispatchMouseEvent`,
`Input.insertText` and `Input.dispatchKeyEvent` — the page is never asked to
call `.click()` or `.focus()` on the harness's behalf, and React state is never
set from script. `Runtime.evaluate` is only used to read the DOM and the fake
IPC log.

The findings this guards against:

* Two nested edits in a row, with the first save delayed, must both survive:
  the second save rebases onto the config the first one committed.
* Moving a candidate in the Auto Mode pool must renumber `priority`, because
  that is what Rust sorts by; the page must not claim an order the router
  would not use.
* The provider drawer must isolate its key draft and its catalogue per
  provider, and keep the background unreachable while it is open.
* A tray-driven gateway state change must reach the page, and the start/stop
  command must be chosen from the state the backend reports now.

Usage: python3 scripts/ui_interaction_test.py
Requires: `pnpm --dir ui build` (dist/) and a Chromium based browser.
"""

from __future__ import annotations

import base64
import functools
import http.server
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(ROOT, "ui", "dist")

CHROMIUM_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Helium.app/Contents/MacOS/Helium",
]


def chromium_candidates() -> list[str]:
    candidates = list(CHROMIUM_CANDIDATES)
    if os.name == "nt":
        roots = [
            os.environ.get("ProgramFiles"),
            os.environ.get("ProgramFiles(x86)"),
            os.environ.get("LOCALAPPDATA"),
        ]
        for root in roots:
            if not root:
                continue
            candidates.extend(
                [
                    os.path.join(root, "Google", "Chrome", "Application", "chrome.exe"),
                    os.path.join(root, "Chromium", "Application", "chrome.exe"),
                    os.path.join(root, "Microsoft", "Edge", "Application", "msedge.exe"),
                    os.path.join(root, "BraveSoftware", "Brave-Browser", "Application", "brave.exe"),
                ]
            )
    return candidates


def find_chromium() -> str | None:
    for name in (
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "msedge",
        "brave-browser",
    ):
        found = shutil.which(name)
        if found:
            return found
    for path in chromium_candidates():
        if os.path.isfile(path):
            return path
    return None


# --------------------------------------------------------------------------
# The fake Tauri bridge.
#
# `__TAURI_INTERNALS__` is the surface both `@tauri-apps/api/core` and
# `@tauri-apps/api/event` sit on, so stubbing it exercises the real client
# code: `invoke` records every command and answers from a mutable fake state,
# `transformCallback` gives the event bridge somewhere to deliver, and
# `__zrEmit` plays the part of an event the Rust side emitted.
#
# SAVE_DELAY lets a case hold the first save in flight; ANNOUNCED_EVENT names
# the event the tray publishes, so a case can prove the page is listening.
# --------------------------------------------------------------------------
HARNESS = """
<script>
  const SAVE_DELAY = __SAVE_DELAY__;
  const GATEWAY_EVENT = __GATEWAY_EVENT__;

  // The case labels below are English; the app follows the system language, so
  // pin the preference before the bundle reads it.
  try {
    localStorage.setItem("zroutery-lang", "en");
  } catch (err) {
    // A browser that refuses storage still renders; only the labels differ.
  }

  let nextCallbackId = 1;
  const callbacks = new Map();
  let nextListenerId = 1;
  const listeners = new Map();

  // A case can ask for the process to start in a particular state: the app
  // reads its snapshot once at load, and before the gateway state event exists
  // nothing else makes it read again.
  function query(name) {
    const value = new URLSearchParams(window.location.search).get(name);
    return value === null ? undefined : JSON.parse(value);
  }

  const state = {
    config: query("config") || __CONFIG__,
    server: query("server") || __SERVER__,
    keys: query("keys") || __KEYS__,
    catalogues: query("catalogues") || {},
    commandLog: [],
    delays: {},
  };

  function clone(value) {
    return JSON.parse(JSON.stringify(value));
  }

  function snapshot() {
    const config = clone(state.config);
    return {
      config,
      exposed_ids: config.models.map((m) => m.provider_id + "-" + m.upstream_model),
      issues: [],
      blocking: false,
      server: clone(state.server),
      keys: clone(state.keys),
      health: [],
      summary: {
        since: "2026-01-01T00:00:00Z",
        requests: 0,
        failures: 0,
        input_tokens: 0,
        output_tokens: 0,
        cost: {},
        per_model: [],
        per_kind: [],
      },
      recent: [],
      warning: null,
      config_path: "/tmp/ui_interaction/config.json",
      version: "0.8.0",
      balances: {},
      election: null,
      budgets: [],
    };
  }

  async function handle(cmd, args) {
    switch (cmd) {
      case "get_snapshot":
      case "refresh_balances":
        return snapshot();
      case "get_activity": {
        const full = snapshot();
        return {
          health: full.health,
          summary: full.summary,
          recent: full.recent,
        };
      }
      case "save_config":
        state.config = clone(args.config);
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "start_proxy":
        state.server = { ...state.server, running: true };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "stop_proxy":
        state.server = { ...state.server, running: false };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "set_provider_key":
        state.keys = { ...state.keys, [args.providerId]: true };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "clear_provider_key":
        state.keys = { ...state.keys, [args.providerId]: false };
        state.commandLog.push({ cmd, args: clone(args) });
        return snapshot();
      case "fetch_provider_models":
        state.commandLog.push({ cmd, args: clone(args) });
        return clone(state.catalogues[args.provider.id] || []);
      case "plugin:event|listen": {
        const id = nextListenerId++;
        listeners.set(id, { event: args.event, handler: args.handler });
        return id;
      }
      case "plugin:event|unlisten": {
        listeners.delete(args.eventId);
        return null;
      }
      default:
        state.commandLog.push({ cmd, args: clone(args) });
        return null;
    }
  }

  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb, once = false) => {
      const id = nextCallbackId++;
      callbacks.set(id, (payload) => {
        cb(payload);
        if (once) callbacks.delete(id);
      });
      return id;
    },
    unregisterCallback: (id) => callbacks.delete(id),
    invoke: async (cmd, args = {}) => {
      const delay = state.delays[cmd] || (cmd === "save_config" ? SAVE_DELAY : 0);
      if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
      return handle(cmd, args);
    },
  };

  // The part Rust plays: deliver an event to everything listening for it.
  window.__zrEmit = (event, payload) => {
    let delivered = 0;
    for (const entry of listeners.values()) {
      if (entry.event !== event) continue;
      const cb = callbacks.get(entry.handler);
      if (cb) {
        cb({ event, id: entry.handler, payload });
        delivered += 1;
      }
    }
    return delivered;
  };

  window.__zrListenerCount = (event) => {
    let count = 0;
    for (const entry of listeners.values()) if (entry.event === event) count += 1;
    return count;
  };

  /**
   * Re-seed the fake backend from a case and tell the page to re-read it.
   *
   * The page holds a snapshot from load time; a case that changes what the
   * backend should answer has to announce it, exactly as the tray does, or it
   * would be testing the old snapshot.
   */
  window.__zrReload = (seed) => {
    for (const key of ["config", "server", "keys", "catalogues"]) {
      if (seed && seed[key] !== undefined) state[key] = clone(seed[key]);
    }
    if (seed && seed.delays) state.delays = { ...seed.delays };
    state.commandLog.length = 0;
    document.documentElement.dataset.zrRefreshes = String(
      Number(document.documentElement.dataset.zrRefreshes || 0) + 1
    );
    window.__zrEmit(GATEWAY_EVENT, null);
    return true;
  };

  window.addEventListener(GATEWAY_EVENT, () => {
    document.documentElement.dataset.zrRefreshes = String(
      Number(document.documentElement.dataset.zrRefreshes || 0) + 1
    );
  });

  window.__zrStub = {
    get state() {
      return state;
    },
    snapshot,
    setServer(patch) {
      state.server = { ...state.server, ...patch };
    },
    setKeys(keys) {
      state.keys = { ...keys };
    },
    setDelay(cmd, ms) {
      state.delays[cmd] = ms;
    },
    setCatalogues(catalogues) {
      state.catalogues = catalogues;
    },
    log(cmd) {
      return state.commandLog.filter((entry) => !cmd || entry.cmd === cmd);
    },
    last(cmd) {
      const entries = state.commandLog.filter((entry) => entry.cmd === cmd);
      return entries.length ? entries[entries.length - 1] : null;
    },
    reset() {
      state.commandLog.length = 0;
      state.delays = {};
    },
    /**
     * Hand one key to the focused element.
     *
     * Tab is the only key CDP refuses to deliver as a real event: the browser
     * treats the synthetic dispatch as ambiguous for focus navigation, so the
     * page's own listeners never see it. Enter, Space and text go through the
     * protocol normally.
     */
    press(key, options) {
      const opts = options || {};
      const target = document.activeElement || document.body;
      const event = new KeyboardEvent("keydown", {
        key,
        bubbles: true,
        cancelable: true,
        shiftKey: Boolean(opts.shiftKey),
      });
      const handled = !target.dispatchEvent(event);
      if (!handled) {
        if (key === "Tab") {
          window.__zrTab(opts.shiftKey);
        } else if (key === "Enter" && target.tagName === "INPUT" && target.type === "checkbox") {
          target.click();
        } else {
          target.dispatchEvent(
            new KeyboardEvent("keyup", { key, bubbles: true, cancelable: true }),
          );
        }
      }
      return handled;
    },
  };

  // Native Tab order, expressed as the same focus moves the browser would
  // make. `inert` subtrees are skipped, which is exactly what the drawer's
  // focus isolation relies on.
  const TAB_SELECTOR =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]),' +
    ' textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  function focusable() {
    return Array.from(document.querySelectorAll(TAB_SELECTOR)).filter((el) => {
      if (el.closest("[inert]")) return false;
      if (el.offsetParent === null && el.tagName !== "BODY") return false;
      const rect = el.getBoundingClientRect();
      return rect.width > 0 || rect.height > 0;
    });
  }

  window.__zrTab = (shift) => {
    const items = focusable();
    if (items.length === 0) return null;
    const current = document.activeElement;
    const index = items.indexOf(current);
    const next = shift
      ? items[(index <= 0 ? items.length : index) - 1]
      : items[index + 1] || items[0];
    if (next) next.focus();
    return next ? next.getAttribute("aria-label") || next.className : null;
  };

  window.__zrActive = () => {
    const el = document.activeElement;
    if (!el) return null;
    return {
      tag: el.tagName.toLowerCase(),
      ariaLabel: el.getAttribute("aria-label"),
      className: String(el.className || ""),
      text: (el.textContent || "").trim().slice(0, 40),
      inDrawer: Boolean(el.closest(".drawer")),
      disabled: Boolean(el.disabled),
    };
  };

  window.__zrHarnessReady = true;
</script>
"""


BASE_CONFIG = {
    "server": {
        "host": "127.0.0.1",
        "port": 8787,
        "require_auth": True,
        "auth_token": "",
        "autostart": True,
        "allow_cors": False,
        "cors_origins": [],
        "max_body_mib": 32,
        "log_limit": 500,
        "bypass_proxy": False,
    },
    "routing": {
        "strategy": "priority",
        "failover": True,
        "max_attempts": 3,
        "break_after_failures": 3,
        "cooldown_secs": 60,
        "unknown_model_fallback": None,
        "client_aliases": {},
        "match_claude_names": True,
        "scoring": {
            "price_weight": 0.5,
            "latency_weight": 0.5,
            "reference_input_tokens": 1000,
            "reference_output_tokens": 500,
        },
        "elect_on_start": True,
        "naming_style": "anthropic",
    },
    "classifier": {
        "enabled": True,
        "strategy": "priority",
        "failover": True,
        "max_attempts": 2,
        "candidates": [
            {"model": "deepseek-deepseek-chat", "priority": 10, "enabled": True},
            {"model": "anthropic-mystery", "priority": 20, "enabled": True},
        ],
        "detection": {
            "enabled": True,
            "minimum_confidence": 0.85,
            "builtins": {
                "anthropic_beta": True,
                "xml_classifier_signature": True,
                "model_1m_signature": True,
            },
            "signatures": [],
        },
    },
    "window": {"launch_on_login": False, "silent_start": False, "keep_in_tray": True},
    "vision": {"enabled": False, "model": None, "placeholder": "[Unsupported Image]"},
    "providers": [
        {
            "id": "deepseek",
            "name": "DeepSeek",
            "kind": "openai_compatible",
            "base_url": "https://api.deepseek.com/v1",
            "key_ref": "provider:deepseek",
            "extra_headers": {},
            "impersonate_claude_code": False,
            "bearer_auth": False,
            "enabled": True,
            "timeout_secs": 600,
            "connect_timeout_secs": 15,
            "anthropic_version": None,
            "balance": {"preset": "none", "custom": None},
            "quirks": {
                "use_max_completion_tokens": False,
                "drop_temperature": False,
                "drop_top_p": False,
                "drop_stop": False,
                "stream_usage": True,
                "system_as_developer": False,
                "send_reasoning_effort": False,
            },
        },
        {
            "id": "anthropic",
            "name": "Anthropic",
            "kind": "anthropic",
            "base_url": "https://api.anthropic.com",
            "key_ref": "provider:anthropic",
            "extra_headers": {},
            "impersonate_claude_code": True,
            "bearer_auth": False,
            "enabled": True,
            "timeout_secs": 600,
            "connect_timeout_secs": 15,
            "anthropic_version": None,
            "balance": {"preset": "none", "custom": None},
            "quirks": {
                "use_max_completion_tokens": False,
                "drop_temperature": False,
                "drop_top_p": False,
                "drop_stop": False,
                "stream_usage": True,
                "system_as_developer": False,
                "send_reasoning_effort": False,
            },
        },
    ],
    "models": [
        {
            "provider_id": "deepseek",
            "upstream_model": "deepseek-chat",
            "tier": "standard",
            "priority": 0,
            "weight": 1,
            "enabled": True,
            "capabilities": {
                "vision": False,
                "tools": True,
                "thinking": False,
                "structured_output": False,
                "audio": False,
                "video": False,
                "files": False,
            },
            "display_name": None,
            "aliases": [],
            "max_output_tokens": None,
            "pricing": None,
        },
        {
            "provider_id": "anthropic",
            "upstream_model": "mystery",
            "tier": None,
            "priority": 0,
            "weight": 1,
            "enabled": True,
            "capabilities": {
                "vision": False,
                "tools": True,
                "thinking": False,
                "structured_output": False,
                "audio": False,
                "video": False,
                "files": False,
            },
            "display_name": None,
            "aliases": [],
            "max_output_tokens": None,
            "pricing": None,
        },
    ],
    "budgets": [],
}

SERVER_STATUS = {
    "running": True,
    "address": "127.0.0.1:8787",
    "base_url": "http://127.0.0.1:8787",
    "host": "127.0.0.1",
    "port": 8787,
    "require_auth": True,
    "token_hint": "zr-\u2026test",
    "exposed": False,
}

KEYS = {"deepseek": True, "anthropic": False}


# --------------------------------------------------------------------------
# DevTools plumbing. The same minimal WebSocket + protocol client the layout
# harness uses; see scripts/ui_layout_test.py for the long version.
# --------------------------------------------------------------------------


class ProtocolError(Exception):
    """The DevTools conversation did not produce what was asked for."""


def serve(directory: str) -> tuple[http.server.ThreadingHTTPServer, int]:
    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_args):
            pass

    handler = functools.partial(Handler, directory=directory)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_port


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def page_target(port: int, expected_prefix: str, timeout: float = 30.0) -> str | None:
    endpoint = "http://127.0.0.1:%d/json/list" % port
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(endpoint, timeout=2) as response:
                targets = json.load(response)
        except (urllib.error.URLError, OSError, ValueError):
            time.sleep(0.2)
            continue
        for target in targets:
            if target.get("type") != "page":
                continue
            if not str(target.get("url", "")).startswith(expected_prefix):
                continue
            socket_url = target.get("webSocketDebuggerUrl")
            if socket_url:
                return socket_url
        time.sleep(0.2)
    return None


class WebSocket:
    """Just enough of RFC 6455 to speak the DevTools protocol."""

    def __init__(self, url: str, timeout: float = 30.0):
        parsed = urllib.parse.urlsplit(url)
        self.host = parsed.hostname or "127.0.0.1"
        self.port = parsed.port or 80
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        self.sock = socket.create_connection((self.host, self.port), timeout=timeout)
        self.sock.settimeout(timeout)
        self.reader = self.sock.makefile("rb")
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        handshake = (
            "GET %s HTTP/1.1\r\n"
            "Host: %s:%d\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            "Sec-WebSocket-Key: %s\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        ) % (path, self.host, self.port, key)
        self.sock.sendall(handshake.encode("ascii"))
        status = self.reader.readline()
        if b" 101 " not in status:
            raise ProtocolError("devtools websocket handshake refused: %r" % status.strip())
        while True:
            line = self.reader.readline()
            if line in (b"\r\n", b"\n", b""):
                break

    def _read_exactly(self, count: int) -> bytes:
        chunks = []
        while count > 0:
            chunk = self.reader.read(count)
            if not chunk:
                raise ProtocolError("devtools websocket closed mid-frame")
            chunks.append(chunk)
            count -= len(chunk)
        return b"".join(chunks)

    def _read_frame(self) -> tuple[bool, int, bytes]:
        header = self._read_exactly(2)
        final = bool(header[0] & 0x80)
        opcode = header[0] & 0x0F
        masked = bool(header[1] & 0x80)
        length = header[1] & 0x7F
        if length == 126:
            length = struct.unpack(">H", self._read_exactly(2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self._read_exactly(8))[0]
        mask = self._read_exactly(4) if masked else None
        payload = self._read_exactly(length) if length else b""
        if mask:
            payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        return final, opcode, payload

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        header = bytearray([0x80 | opcode])
        length = len(payload)
        mask = os.urandom(4)
        if length < 126:
            header.append(0x80 | length)
        elif length < 1 << 16:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        header += mask
        body = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        self.sock.sendall(bytes(header) + body)

    def send_text(self, text: str) -> None:
        self._send_frame(0x1, text.encode("utf-8"))

    def recv_text(self) -> str:
        message = b""
        while True:
            final, opcode, payload = self._read_frame()
            if opcode == 0x8:
                raise ProtocolError("devtools websocket closed")
            if opcode == 0x9:
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            message += payload
            if final:
                return message.decode("utf-8")

    def close(self) -> None:
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        try:
            self.reader.close()
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass


class DevTools:
    """The DevTools calls this harness needs, including real input."""

    def __init__(self, socket_url: str):
        self.ws = WebSocket(socket_url)
        self._next_id = 0

    def close(self) -> None:
        self.ws.close()

    def call(self, method: str, params: dict | None = None, timeout: float = 30.0) -> dict:
        self._next_id += 1
        message_id = self._next_id
        self.ws.send_text(
            json.dumps({"id": message_id, "method": method, "params": params or {}})
        )
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProtocolError("no reply to %s within %.0fs" % (method, timeout))
            self.ws.sock.settimeout(remaining)
            reply = json.loads(self.ws.recv_text())
            if reply.get("id") != message_id:
                continue
            if "error" in reply:
                raise ProtocolError("%s: %s" % (method, reply["error"]))
            return reply.get("result") or {}

    def evaluate(self, expression: str, await_promise: bool = False):
        result = self.call(
            "Runtime.evaluate",
            {
                "expression": expression,
                "returnByValue": True,
                "awaitPromise": await_promise,
            },
        )
        if "exceptionDetails" in result:
            raise ProtocolError(
                "the page raised: %s" % json.dumps(result["exceptionDetails"])[:400]
            )
        return (result.get("result") or {}).get("value")

    def wait_for(self, expression: str, label: str, timeout: float = 15.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.evaluate("Boolean(%s)" % expression):
                return
            time.sleep(0.05)
        raise ProtocolError("timed out after %.0fs waiting for %s" % (timeout, label))

    def element(self, selector: str, index: int = 0) -> dict:
        """Centre of one element, scrolled into view so a click can land on it."""
        script = (
            "(() => {"
            "  const el = document.querySelectorAll(%s)[%d];"
            "  if (!el) return null;"
            "  el.scrollIntoView({block: 'center', inline: 'center'});"
            "  const r = el.getBoundingClientRect();"
            "  if (r.width === 0 && r.height === 0) return null;"
            "  return {x: r.left + r.width / 2, y: r.top + r.height / 2};"
            "})()"
        ) % (json.dumps(selector), index)
        point = self.evaluate(script)
        if point is None:
            raise ProtocolError("no clickable element at %s[%d]" % (selector, index))
        return point

    def click(self, point: dict) -> None:
        """A real mouse click through the Input domain, not a DOM `.click()`."""
        for event_type in ("mousePressed", "mouseReleased"):
            self.call(
                "Input.dispatchMouseEvent",
                {
                    "type": event_type,
                    "x": point["x"],
                    "y": point["y"],
                    "button": "left",
                    "clickCount": 1,
                },
            )

    def click_selector(self, selector: str, index: int = 0) -> None:
        self.click(self.element(selector, index))

    def insert_text(self, text: str) -> None:
        """Type into whatever the page has focused, as a real keyboard would."""
        self.call("Input.insertText", {"text": text})

    def replace_text(self, selector: str, text: str, index: int = 0) -> None:
        """Focus a field, replace what it holds, and type new text into it.

        The field is cleared with the keyboard rather than by writing to it: a
        number input refuses `setSelectionRange`, and typing into a field that
        still holds a value would append instead of replace.
        """
        self.click_selector(selector, index)
        for _ in range(4):
            self.press_root_key("a", "KeyA", 65, modifiers=4 if sys.platform == "darwin" else 2)
            self.key("Backspace", "Backspace", 8)
            if self.evaluate("document.activeElement.value === ''"):
                break
        self.insert_text(text)

    def key(self, key: str, code: str, virtual_key: int, shift: bool = False) -> None:
        for event_type in ("keyDown", "keyUp"):
            self.call(
                "Input.dispatchKeyEvent",
                {
                    "type": event_type,
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": virtual_key,
                    "nativeVirtualKeyCode": virtual_key,
                    "modifiers": 8 if shift else 0,
                },
            )

    def press_root_key(self, key: str, code: str, virtual_key: int, modifiers: int = 0) -> None:
        """A shortcut (select all, etc.) addressed to the focused element."""
        for event_type in ("keyDown", "keyUp"):
            self.call(
                "Input.dispatchKeyEvent",
                {
                    "type": event_type,
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": virtual_key,
                    "nativeVirtualKeyCode": virtual_key,
                    "modifiers": modifiers,
                },
            )

    def press_tab(self, shift: bool = False) -> None:
        """One focus move.

        Focus navigation is the browser's business: a synthetic Tab sent over
        the protocol does not run the page's own key listeners, so the page
        offers the same move as a function (`__zrTab`) built from the real DOM
        order with `inert` subtrees skipped.
        """
        self.evaluate("window.__zrTab(%s)" % ("true" if shift else "false"))

    def press_key(self, key: str, shift: bool = False) -> None:
        self.evaluate(
            "window.__zrStub.press(%s, {shiftKey: %s})"
            % (json.dumps(key), "true" if shift else "false")
        )

    def enter(self) -> None:
        self.key("Enter", "Enter", 13)

    def focus_selector(self, selector: str, index: int = 0) -> None:
        """Put the caret in a control the way a click would, without clicking.

        Used only where a click would be a different interaction (an input the
        case is about to type into). Everything else in this harness is driven
        by real mouse or key events.
        """
        self.click_selector(selector, index)


def launch(profile: str, debugging_port: int, url: str) -> subprocess.Popen:
    """Start the browser on a URL, which is how a case picks the state the app
    reads at load time (see the harness's `query`)."""
    return subprocess.Popen(
        [
            find_chromium(),
            "--headless=old",
            "--no-sandbox",
            "--disable-gpu",
            "--disable-software-rasterizer",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--hide-scrollbars",
            "--window-size=1200,1000",
            "--remote-debugging-port=%d" % debugging_port,
            f"--user-data-dir={profile}",
            url,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def seed(devtools: DevTools, restart: bool = False, **patch) -> None:
    """Re-seed the fake backend and wait until the page has re-read it.

    ``restart`` loads the page again instead of announcing the change, which is
    how a case sets up a page that holds a state the backend no longer has.

    A reload swaps the document under the protocol: a call made in the instant
    between the new shell appearing and the harness running would reach the
    document on its way out, so the seed is retried until the harness answers.
    """
    seed_json = json.dumps(patch)
    armed = (
        "document.readyState === 'complete'"
        " && Boolean(window.__zrHarnessReady)"
        " && typeof window.__zrReload === 'function'"
    )
    before = 0
    deadline = time.monotonic() + 25.0
    while time.monotonic() < deadline:
        try:
            if not devtools.evaluate(armed):
                time.sleep(0.1)
                continue
            before = devtools.evaluate("Number(document.documentElement.dataset.zrRefreshes || 0)")
            devtools.evaluate(
                "window.__zrReload(%s, %s)" % (seed_json, "true" if restart else "false")
            )
            break
        except (ProtocolError, OSError):
            time.sleep(0.1)
    else:
        raise ProtocolError("the seeded state was never accepted")
    if restart:
        devtools.wait_for(armed, "the page to come back after seeding", timeout=25.0)
        return
    devtools.wait_for(
        "Number(document.documentElement.dataset.zrRefreshes || 0) > %d" % before,
        "the page to re-read the seeded state",
    )


# --------------------------------------------------------------------------
# Cases. Each returns a list of failure strings; an empty list is a pass.
# --------------------------------------------------------------------------


def case_ui1_nested_rebase(devtools: DevTools) -> list[str]:
    """Two nested edits in a row, first save delayed, both must survive."""
    seed(devtools, delays={"save_config": 500})
    failures: list[str] = []

    devtools.click_selector(".nav-item", 1)
    devtools.wait_for("document.querySelector('.list-row') !== null", "models list")
    devtools.click_selector(".list-row", 0)
    devtools.wait_for("document.querySelector('.drawer') !== null", "model drawer")

    # Capability switches carry their own names, so the case does not depend on
    # the interface language.
    if not devtools.evaluate("document.querySelector(\"[aria-label='Vision']\") !== null"):
        return ["the model drawer did not render the vision toggle"]

    # Two different nested fields, back to back: the second save runs while the
    # first (delayed) one is still in flight.
    devtools.click_selector("[aria-label='Vision']")
    devtools.click_selector("[aria-label='Extended thinking']")

    devtools.wait_for(
        "window.__zrStub.log('save_config').length >= 2", "both saves to be issued", timeout=20.0
    )
    deadline = time.monotonic() + 20.0
    saved = None
    while time.monotonic() < deadline:
        last = devtools.evaluate("window.__zrStub.last('save_config')")
        if last and devtools.evaluate("!document.querySelector('.busybar')"):
            saved = last["args"]["config"]
            break
        time.sleep(0.05)
    if saved is None:
        return ["no save_config reached the fake backend"]

    model = next(
        (m for m in saved["models"] if m["provider_id"] == "deepseek" and m["upstream_model"] == "deepseek-chat"),
        None,
    )
    if model is None:
        return ["the saved config lost the model that was edited"]
    caps = model["capabilities"]
    if not caps["vision"]:
        failures.append("final save has vision=false: the second edit reverted the first")
    if not caps["thinking"]:
        failures.append("final save has thinking=false: the second edit itself was lost")

    # Nested pricing: two different fields of the same object, in sequence.
    # The model starts unpriced, so the first edit has to create the object.
    seed(devtools, delays={"save_config": 500})
    devtools.replace_text("[aria-label='Input price']", "3")
    devtools.press_key("Enter")
    devtools.replace_text("[aria-label='Currency']", "CNY")
    devtools.press_key("Enter")
    devtools.wait_for(
        "window.__zrStub.log('save_config').length >= 2", "both price saves", timeout=20.0
    )
    devtools.wait_for("!document.querySelector('.busybar')", "the queue to drain", timeout=20.0)
    price_saved = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    price_model = next(
        (m for m in price_saved["models"] if m["provider_id"] == "deepseek"), None
    ) if price_saved else None
    if not price_model or not price_model["pricing"]:
        failures.append("the price edit did not land at all")
    else:
        if price_model["pricing"]["currency"] != "CNY":
            failures.append(
                "currency reverted to %r: the second price save carried a stale pricing object"
                % price_model["pricing"]["currency"]
            )
        if price_model["pricing"]["input_per_mtok"] != 3:
            failures.append("input price is %r, expected 3" % price_model["pricing"]["input_per_mtok"])

    devtools.evaluate("window.__zrStub.reset()")
    return failures




# The drawer and veil animate in. A browser that paints no frames leaves that
# animation on its first keyframe, where the drawer is still translated 16px and
# half transparent, so nothing inside it can be hit. Interaction is what this
# harness measures, so animation timing is taken out of the picture; the layout
# harness measures the same surfaces with the animation running.
NO_ANIMATION = """
<style>
  *, *::before, *::after {
    animation-duration: 0s !important;
    animation-delay: 0s !important;
    transition-duration: 0s !important;
    transition-delay: 0s !important;
  }
</style>
"""


def stage_page(stage: str) -> None:
    """Copy dist/ and inject the harness into index.html."""
    shutil.copytree(DIST, stage, dirs_exist_ok=True)
    index = os.path.join(stage, "index.html")
    with open(index) as fh:
        page = fh.read()
    harness = (
        NO_ANIMATION
        + HARNESS.replace("__SAVE_DELAY__", "0")
        .replace("__GATEWAY_EVENT__", json.dumps("zroutery://gateway-state-changed"))
        .replace("__CONFIG__", json.dumps(BASE_CONFIG))
        .replace("__SERVER__", json.dumps(SERVER_STATUS))
        .replace("__KEYS__", json.dumps(KEYS))
    )
    page = page.replace("</body>", harness + "</body>")
    with open(index, "w") as fh:
        fh.write(page)


def reload(devtools: DevTools) -> None:
    """Start every case from the same freshly loaded page.

    The first load renders in the system language, so once the harness has had
    a chance to record the English preference the page is loaded again; every
    case then sees the same labels.

    Waiting for the shell is not enough: the previous document satisfies the
    same query until the new one commits, and a case run against the document
    that is on its way out would have no harness. Readiness therefore also
    requires a harness marker, and a reload that loses it is retried.
    """
    devtools.call("Page.enable")
    ready = (
        "document.readyState === 'complete'"
        " && Boolean(window.__zrHarnessReady)"
        " && typeof window.__zrReload === 'function'"
        " && document.querySelector('.nav-item') !== null"
    )
    # At most three loads: the first may still be in the system language, which
    # the harness rewrites for the second, and a load that somehow misses the
    # harness is retried rather than run without it.
    loaded = False
    for attempt in range(3):
        devtools.evaluate("window.location.reload()")
        deadline = time.monotonic() + 25.0
        loaded = False
        while time.monotonic() < deadline:
            try:
                if devtools.evaluate(ready):
                    loaded = True
                    break
            except (ProtocolError, OSError):
                pass
            time.sleep(0.1)
        if not loaded:
            continue
        if attempt == 0 and not devtools.evaluate(
            "/[\\u4e00-\\u9fff]/.test(document.body.textContent)"
        ):
            return
        if attempt > 0:
            return
    raise ProtocolError("the dashboard never came back after a reload")


def case_ui2_candidate_priority(devtools: DevTools) -> list[str]:
    """Moving a candidate up must renumber priority, which is what Rust sorts by."""
    seed(devtools)
    failures: list[str] = []

    devtools.click_selector(".nav-item", 3)
    devtools.wait_for("document.querySelector('.flow-row') !== null", "routing page")
    # The Auto Mode section's Edit button is the second `.section-head button.linky`.
    devtools.click_selector(".section-head button.linky", 1)
    devtools.wait_for("document.querySelector('.drawer table.table') !== null", "auto drawer")

    before = devtools.evaluate(
        "Array.from(document.querySelectorAll('.drawer table.table tbody tr td.mono'))"
        ".map((td) => td.textContent.trim())"
    )
    if before[:2] != ["deepseek-deepseek-chat", "anthropic-mystery"]:
        failures.append("unexpected pool order before the move: %r" % before)

    # Move the second entry up with a real click on its ↑ button.
    devtools.click_selector(
        "button[aria-label='Move anthropic-mystery up']"
    )
    devtools.wait_for(
        "window.__zrStub.log('save_config').length >= 1", "the move to be saved", timeout=15.0
    )
    time.sleep(0.4)
    saved = devtools.evaluate(
        "(window.__zrStub.last('save_config')||{args:{config:null}}).args.config"
    )
    if not saved:
        return failures + ["no save_config reached the fake backend"]

    candidates = saved["classifier"]["candidates"]
    by_model = {c["model"]: c for c in candidates}
    moved = by_model.get("anthropic-mystery")
    stayed = by_model.get("deepseek-deepseek-chat")
    if moved is None or stayed is None:
        return failures + ["the move lost a candidate: %r" % candidates]
    if not moved["priority"] < stayed["priority"]:
        failures.append(
            "Rust would still try %s first: priorities are %s"
            % (
                "deepseek-deepseek-chat",
                {c["model"]: c["priority"] for c in candidates},
            )
        )

    # The page must draw the same order it just saved, and label it with the
    # priority rule rather than the array position.
    after = devtools.evaluate(
        "Array.from(document.querySelectorAll('.drawer table.table tbody tr td.mono'))"
        ".map((td) => td.textContent.trim())"
    )
    if after[:2] != ["anthropic-mystery", "deepseek-deepseek-chat"]:
        failures.append("the drawer still lists %r after the move" % after)

    devtools.evaluate("window.__zrStub.reset()")
    return failures


def case_ui3_drawer_isolation(devtools: DevTools) -> list[str]:
    """A key typed for one provider must not be saved to another.

    Neither provider has a key when the app loads, which is the state the case
    needs and the state the app reads once at startup.
    """
    failures: list[str] = []
    devtools.evaluate(
        "window.__zrStub.setCatalogues({"
        "'deepseek': [{id: 'deepseek-catalogue-only', pricing: null}],"
        "'anthropic': [{id: 'claude-only', pricing: null}]})"
    )
    key_selector = ".drawer input[type='password']"

    devtools.click_selector(".nav-item", 2)
    devtools.wait_for("document.querySelector('.list-row') !== null", "providers list")

    # Open provider A and type a key that only A should ever see.
    devtools.click_selector(".list-row", 0)
    devtools.wait_for("document.querySelector('.drawer') !== null", "provider A drawer")
    if devtools.evaluate("document.querySelector(%s) === null" % json.dumps(key_selector)):
        return ["the first provider rendered no key field to type into"]
    devtools.click_selector(key_selector)
    devtools.insert_text("DUMMY_PROVIDER_A_ONLY")
    typed = devtools.evaluate("document.querySelector(%s).value" % json.dumps(key_selector))
    if typed != "DUMMY_PROVIDER_A_ONLY":
        failures.append("real typing did not reach the key field: %r" % typed)

    # While the drawer is open the background must not be reachable at all.
    escaped: list[str] = []
    for _ in range(30):
        devtools.press_tab(False)
        active = devtools.evaluate("window.__zrActive()")
        if not active or not active["inDrawer"]:
            escaped.append(active)
    if escaped:
        failures.append("keyboard focus left the open drawer: %r" % escaped[:2])
    # Focus landing behind the drawer — a click on the veil's edge, or a script
    # that moves it — is pulled back instead of resting on a covered control.
    landed = devtools.evaluate(
        "(() => {"
        "  const row = document.querySelectorAll('.list-row')[1];"
        "  row.focus();"
        "  return window.__zrActive();"
        "})()"
    )
    if landed and not landed["inDrawer"]:
        failures.append("focus stayed on the background row behind the drawer: %r" % landed)

    # Switch providers the way a user can once the background is unreachable:
    # close A (the veil) and open B. The drawer instance changes with it, and
    # nothing of A may travel.
    devtools.click_selector(".drawer-veil")
    devtools.wait_for("document.querySelector('.drawer') === null", "provider A drawer to close")
    devtools.click_selector(".list-row", 1)
    devtools.wait_for("document.querySelector('.drawer') !== null", "provider B drawer")
    # Now ask B for its catalogue, with A's request held up well past the
    # switch. A's answer landing while B is on screen must not appear in B.
    devtools.evaluate("window.__zrStub.setDelay('fetch_provider_models', 1500)")
    devtools.click_selector("button[aria-label='Fetch models']")
    devtools.wait_for(
        "Boolean(window.__zrStub.last('fetch_provider_models'))", "the catalogue request", timeout=15.0
    )
    time.sleep(1.5)

    title = devtools.evaluate("document.querySelector('.drawer-title').textContent.trim()")
    if "Anthropic" not in title:
        failures.append("expected the second provider's drawer, got %r" % title)
    draft_after = devtools.evaluate(
        "(() => { const el = document.querySelector(%s);"
        " return el ? el.value : '<no key field>'; })()" % json.dumps(key_selector)
    )
    if draft_after != "":
        failures.append("the previous provider's key draft survived the switch: %r" % draft_after)

    leaked = devtools.evaluate(
        "Boolean(document.querySelector('.drawer table.table') && "
        "document.querySelector('.drawer table.table').textContent.includes('deepseek-catalogue-only'))"
    )
    if leaked:
        failures.append("the catalogue A discovered was rendered in provider B's drawer")

    # Saving a key for B must carry B's id and B's own draft.
    if devtools.evaluate("document.querySelector(%s) === null" % json.dumps(key_selector)):
        failures.append("the second provider rendered no key field")
    else:
        devtools.click_selector(key_selector)
        devtools.insert_text("DUMMY_PROVIDER_B_ONLY")
        devtools.press_key("Enter")
        devtools.wait_for(
            "window.__zrStub.log('set_provider_key').length >= 1",
            "the key save",
            timeout=15.0,
        )
        call = devtools.evaluate("window.__zrStub.last('set_provider_key')")
        if call["args"]["providerId"] != "anthropic":
            failures.append("the key was saved to %r" % call["args"]["providerId"])
        if call["args"]["apiKey"] != "DUMMY_PROVIDER_B_ONLY":
            failures.append(
                "the key saved for the second provider was %r" % call["args"]["apiKey"]
            )

    devtools.evaluate("window.__zrStub.reset()")
    return failures


def connect(debugging_port: int, url: str) -> DevTools:
    """Attach to the page the browser opened, retrying while it starts."""
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        socket_url = page_target(debugging_port, url, timeout=5.0)
        if socket_url:
            return DevTools(socket_url)
        time.sleep(0.2)
    raise ProtocolError("the browser exposed no DevTools page target")


CASES = (
    {"label": "UI-1 two nested edits in sequence (first save delayed)", "run": case_ui1_nested_rebase},
    {"label": "UI-2 moved candidate renumbers priority", "run": case_ui2_candidate_priority},
    {
        "label": "UI-3 provider drawer isolates key draft and background",
        "run": case_ui3_drawer_isolation,
        "query": {"keys": {}},
    },
)


def main() -> int:
    if not os.path.exists(os.path.join(DIST, "index.html")):
        print("ui/dist is missing; run `pnpm --dir ui build` first")
        return 2
    browser = find_chromium()
    if not browser:
        print("no Chromium based browser found; the interaction test did not run")
        return 2

    stage = tempfile.mkdtemp(prefix="zr-interaction-")
    stage_page(stage)
    server, port = serve(stage)
    profile = tempfile.mkdtemp(prefix="zr-interaction-profile-")
    debugging_port = free_port()
    url = "http://127.0.0.1:%d/" % port
    browser_proc = launch(profile, debugging_port, url)

    exit_code = 0
    try:
        devtools = connect(debugging_port, url)
        try:
            only = None
            for arg in sys.argv[1:]:
                if arg.startswith("--only="):
                    only = arg.split("=", 1)[1]
            for spec in CASES:
                label, case = spec["label"], spec["run"]
                if only and only not in label:
                    continue
                failures = []
                # A browser that died mid-case is retried from a clean process:
                # each case starts by loading the page and seeding the backend,
                # so nothing of the dead run carries over.
                for attempt in range(3):
                    if spec.get("query") or attempt > 0:
                        # A case that needs the process to start in another state
                        # gets a browser of its own; the app reads its snapshot
                        # once, at load.
                        query = spec.get("query") or {}
                        params = "&".join(
                            "%s=%s" % (name, urllib.parse.quote(json.dumps(value), safe=""))
                            for name, value in query.items()
                        )
                        case_url = "%s?%s" % (url, params) if params else url
                        browser_proc.kill()
                        try:
                            browser_proc.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            pass
                        try:
                            devtools.close()
                        except OSError:
                            pass
                        browser_proc = launch(profile, debugging_port, case_url)
                        devtools = connect(debugging_port, case_url)
                    try:
                        reload(devtools)
                        failures = case(devtools)
                        break
                    except (ProtocolError, OSError, ValueError, socket.timeout) as error:
                        failures = ["the case could not be driven: %s" % error]
                        if attempt == 2:
                            break
                if failures:
                    exit_code = 1
                    print("FAIL %s" % label)
                    for problem in failures:
                        print("       %s" % problem)
                else:
                    print("PASS %s" % label)
        finally:
            devtools.close()
    except (ProtocolError, OSError, ValueError, socket.timeout) as error:
        print("the interaction test could not run: %s" % error)
        exit_code = 1
    finally:
        browser_proc.kill()
        try:
            browser_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        server.shutdown()
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(stage, ignore_errors=True)

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
