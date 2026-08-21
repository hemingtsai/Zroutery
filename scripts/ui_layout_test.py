#!/usr/bin/env python3
"""Layout regression test for the dashboard's form controls.

Renders the *real* built UI in headless Chromium with a stubbed Tauri bridge,
then measures every control with `getBoundingClientRect` and asserts that inputs,
selects and buttons that share a row also share a top edge and a height.

That is the bug this guards against: native selects pick their own height in
WebKit, and fields whose hint text wraps used to push their control off the row's
baseline.

Usage: python3 scripts/ui_layout_test.py
Requires: `pnpm --dir ui build` (dist/) and any Chromium based browser.
"""

from __future__ import annotations

import functools
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DIST = os.path.join(ROOT, "ui", "dist")
EXPECTED_CONTROL_HEIGHT = 28.0
TOLERANCE = 0.6

CHROMIUM_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Helium.app/Contents/MacOS/Helium",
]

# A snapshot shaped like the Rust one, with enough variety to exercise the rows:
# fields with and without hints, selects, number inputs, buttons.
SNAPSHOT = {
    "config": {
        "server": {
            "host": "127.0.0.1",
            "port": 8787,
            "require_auth": True,
            "auth_token": "zr-layout-test",
            "autostart": True,
            "allow_cors": False,
            "log_limit": 500,
        },
        "routing": {
            "strategy": "priority",
            "failover": True,
            "max_attempts": 3,
            "break_after_failures": 3,
            "cooldown_secs": 60,
            "unknown_model_fallback": None,
            "client_aliases": {"claude-opus-4-1-20250805": "haiku"},
            "match_claude_names": True,
        },
        "providers": [
            {
                "id": "deepseek",
                "name": "DeepSeek",
                "kind": "openai_compatible",
                "base_url": "https://api.deepseek.com/v1",
                "key_ref": "provider:deepseek",
                "extra_headers": {},
                "enabled": True,
                "timeout_secs": 600,
                "connect_timeout_secs": 15,
                "anthropic_version": None,
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
                "enabled": True,
                "timeout_secs": 600,
                "connect_timeout_secs": 15,
                "anthropic_version": None,
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
                "id": "deepseek-v4-pro",
                "provider_id": "deepseek",
                "upstream_model": "deepseek-v4-pro",
                "class": "sonnet",
                "priority": 0,
                "weight": 1,
                "enabled": True,
                "supports_tools": True,
                "supports_vision": False,
                "supports_thinking": False,
                "display_name": None,
                "aliases": [],
                "max_output_tokens": None,
            },
            {
                "id": "unclassified-thing",
                "provider_id": "anthropic",
                "upstream_model": "mystery",
                "class": None,
                "priority": 0,
                "weight": 1,
                "enabled": True,
                "supports_tools": True,
                "supports_vision": False,
                "supports_thinking": False,
                "display_name": None,
                "aliases": [],
                "max_output_tokens": None,
            },
        ],
    },
    "issues": [],
    "blocking": False,
    "server": {
        "running": True,
        "address": "127.0.0.1:8787",
        "base_url": "http://127.0.0.1:8787",
        "host": "127.0.0.1",
        "port": 8787,
        "require_auth": True,
        "token": "zr-layout-test",
        "exposed": False,
    },
    "keys": {"deepseek": True, "anthropic": False},
    "health": [
        {
            "model_id": "deepseek-v4-pro",
            "consecutive_failures": 0,
            "total_success": 3,
            "total_failure": 1,
            "avg_latency_ms": 812.5,
            "cooldown_remaining_secs": 0,
            "last_error": None,
        }
    ],
    "summary": {
        "since": "2026-01-01T00:00:00Z",
        "requests": 4,
        "failures": 1,
        "input_tokens": 120,
        "output_tokens": 64,
        "per_model": [
            {
                "model_id": "deepseek-v4-pro",
                "requests": 4,
                "failures": 1,
                "input_tokens": 120,
                "output_tokens": 64,
                "reasoning_tokens": 12,
                "cached_tokens": 0,
                "avg_latency_ms": 812.5,
            }
        ],
    },
    "recent": [
        {
            "id": "req_1",
            "at": "2026-01-01T00:00:00Z",
            "ingress": "anthropic",
            "requested_model": "sonnet-class",
            "resolved_model": "deepseek-v4-pro",
            "provider_name": "DeepSeek",
            "stream": True,
            "status": 200,
            "ok": True,
            "error": None,
            "latency_ms": 940,
            "ttft_ms": 210,
            "usage": {
                "input_tokens": 30,
                "output_tokens": 16,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "reasoning_tokens": 3,
            },
            "attempts": 1,
        }
    ],
    "warning": None,
    "config_path": "/tmp/config.json",
    "version": "0.1.0",
}

HARNESS = """
<script>
  // Stub the Tauri bridge so the real dashboard renders in a plain browser.
  const SNAPSHOT = __SNAPSHOT__;
  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb) => cb,
    invoke: (cmd) =>
      Promise.resolve(cmd === "fetch_provider_models" ? ["mock-a", "mock-b"] : SNAPSHOT),
  };

  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  function measureRows(tab) {
    const rows = [];
    // Any row that holds labelled fields has to line up, whatever the row is
    // called, so this keeps working if the markup is renamed.
    document.querySelectorAll(".controls, .row").forEach((row, index) => {
      if (row.querySelector(":scope > .field") === null) return;
      const items = [];
      for (const child of row.children) {
        // A field wraps its control; an action group wraps its buttons.
        const controls = child.matches(".field")
          ? child.querySelectorAll("input, select")
          : child.querySelectorAll("button, input, select");
        for (const el of controls) {
          const r = el.getBoundingClientRect();
          if (r.width === 0 && r.height === 0) continue;
          items.push({
            group: child.className,
            tag: el.tagName.toLowerCase() + (el.type ? ":" + el.type : ""),
            label: (child.querySelector(".field-label")?.textContent || el.textContent || "").trim().slice(0, 24),
            hasHint: !!child.querySelector(".field-hint"),
            top: Math.round(r.top * 100) / 100,
            height: Math.round(r.height * 100) / 100,
          });
        }
      }
      if (items.length > 1) rows.push({ tab, index, items });
    });
    return rows;
  }

  (async () => {
    const out = [];
    for (let i = 0; i < 60 && document.querySelector(".field") === null; i++) await wait(50);
    const tabs = [...document.querySelectorAll('[role="tab"]')];
    for (const tab of tabs) {
      tab.click();
      await wait(150);
      out.push(...measureRows(tab.textContent.trim()));
    }
    await fetch("/report", { method: "POST", body: JSON.stringify(out) });
  })();
</script>
"""


def find_chromium() -> str | None:
    for name in ("google-chrome", "chromium", "chromium-browser"):
        found = shutil.which(name)
        if found:
            return found
    for path in CHROMIUM_CANDIDATES:
        if os.path.exists(path):
            return path
    return None


REPORT: dict[str, object] = {}
REPORTED = threading.Event()


def serve(directory: str) -> tuple[http.server.ThreadingHTTPServer, int]:
    """Static file server that also collects the page's measurement report."""

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            REPORT["rows"] = json.loads(self.rfile.read(length) or b"[]")
            self.send_response(204)
            self.end_headers()
            REPORTED.set()

    handler = functools.partial(Handler, directory=directory)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_port


def main() -> int:
    if not os.path.exists(os.path.join(DIST, "index.html")):
        print("ui/dist is missing; run `pnpm --dir ui build` first")
        return 2
    browser = find_chromium()
    if not browser:
        print("no Chromium based browser found, skipping the layout test")
        return 0

    stage = tempfile.mkdtemp(prefix="zr-layout-")
    shutil.copytree(DIST, stage, dirs_exist_ok=True)
    index = os.path.join(stage, "index.html")
    with open(index) as fh:
        page = fh.read()
    page = page.replace(
        "</body>", HARNESS.replace("__SNAPSHOT__", json.dumps(SNAPSHOT)) + "</body>"
    )
    with open(index, "w") as fh:
        fh.write(page)

    server, port = serve(stage)
    profile = tempfile.mkdtemp(prefix="zr-profile-")
    browser_proc = subprocess.Popen(
        [
            browser,
            # `--headless=old`: the new mode still starts a GPU process, which is
            # not available in a plain shell session.
            "--headless=old",
            "--no-sandbox",
            "--disable-gpu",
            "--disable-software-rasterizer",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--hide-scrollbars",
            "--window-size=1200,1000",
            f"--user-data-dir={profile}",
            f"http://127.0.0.1:{port}/",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        reported = REPORTED.wait(timeout=60)
    finally:
        browser_proc.terminate()
        try:
            browser_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            browser_proc.kill()
        server.shutdown()
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(stage, ignore_errors=True)

    if not reported:
        print("the page never reported measurements; the dashboard failed to render")
        return 1
    rows = REPORT.get("rows") or []
    if not rows:
        print("no multi control rows were found, nothing to check")
        return 1

    failures = 0
    for row in rows:
        tops = {item["top"] for item in row["items"]}
        heights = {item["height"] for item in row["items"]}
        aligned = max(tops) - min(tops) <= TOLERANCE
        uniform = max(heights) - min(heights) <= TOLERANCE
        correct = abs(max(heights) - EXPECTED_CONTROL_HEIGHT) <= TOLERANCE
        status = "ok  " if aligned and uniform and correct else "FAIL"
        if status == "FAIL":
            failures += 1
        labels = ", ".join(
            f"{i['tag']}{'*' if i['hasHint'] else ''}" for i in row["items"]
        )
        print(
            f"  {status} {row['tab']:<10} row {row['index']}: "
            f"{len(row['items'])} controls [{labels}] "
            f"top spread {max(tops) - min(tops):.2f}px, heights {sorted(heights)}"
        )

    print()
    if failures:
        print(f"{failures} of {len(rows)} rows are misaligned")
        return 1
    print(f"all {len(rows)} control rows share one baseline and height (* = has hint text)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
