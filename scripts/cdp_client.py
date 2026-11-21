"""Shared minimal DevTools client for the UI harnesses.

`ui_layout_test.py`, `ui_interaction_test.py` and, through the layout module,
`ui_overflow_test.py` all drive the same headed browser over the DevTools
protocol. This module is the one hand-rolled WebSocket + protocol client they
import; it exists because two harnesses used to carry private copies that had
already started to drift.
"""
from __future__ import annotations

import base64
import functools
import http.server
import json
import os
import socket
import struct
import sys
import threading
import time
import urllib.error
import urllib.request

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

    def drag(self, start: dict, end: dict) -> None:
        """Move the pointer between two points using real DevTools input."""
        self.call(
            "Input.dispatchMouseEvent",
            {
                "type": "mousePressed",
                "x": start["x"],
                "y": start["y"],
                "button": "left",
                "clickCount": 1,
            },
        )
        steps = 8
        for step in range(1, steps + 1):
            fraction = step / steps
            self.call(
                "Input.dispatchMouseEvent",
                {
                    "type": "mouseMoved",
                    "x": start["x"] + (end["x"] - start["x"]) * fraction,
                    "y": start["y"] + (end["y"] - start["y"]) * fraction,
                    "button": "left",
                    "buttons": 1,
                },
            )
        self.call(
            "Input.dispatchMouseEvent",
            {
                "type": "mouseReleased",
                "x": end["x"],
                "y": end["y"],
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

    def tab_to_label(self, label: str, limit: int = 1000) -> None:
        """Navigate to a named control with the harness's native-order Tab path."""
        for _ in range(limit):
            active = self.evaluate("window.__zrActive()")
            if active and active["ariaLabel"] == label:
                return
            self.press_tab()
        raise ProtocolError("could not tab to control %r" % label)

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


TASK_DOORS = {
    "accounts": "providers",
    "routing": "models",
    "cutover": "takeover",
}
