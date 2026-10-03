#!/usr/bin/env python3
"""Layout regression test for the dashboard's form controls.

Renders the *real* built UI in headless Chromium with a stubbed Tauri bridge,
drives that browser over the DevTools protocol, and measures every control with
`getBoundingClientRect`. Controls that share a container are expected to share a
top edge, and every control kind is expected at its own height: native inputs,
selects, the Radix combobox trigger and a plain action button all take
`--control-h`, while a Segment's items carry the 36px `.segment-item` declares.

That is the bug this guards against: native selects pick their own height in
WebKit, and fields whose hint text wraps used to push their control off the row's
baseline.

Coverage is declared, not assumed. `SURFACES` names the pages and drawers this
harness promises to measure, and the run fails when a declared surface yields no
controls, or fewer of a required kind than its declaration demands. A surface
that silently disappears -- a drawer that no longer opens, a select that is no
longer rendered -- therefore cannot be reported as a pass.

Usage: python3 scripts/ui_layout_test.py
Requires: `pnpm --dir ui build` (dist/) and any Chromium based browser.
"""

from __future__ import annotations

from dataclasses import dataclass
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

# The design system's control height (styles.css `--control-h`). Native inputs,
# selects, the Radix trigger and a plain action button all take it. A Segment's
# items are the exception and declare 36px in `.segment-item`; one number cannot
# be the expectation for every kind, so the expectation is per kind.
EXPECTED_CONTROL_HEIGHT = 30.0
EXPECTED_SEGMENT_HEIGHT = 36.0
KIND_HEIGHTS = {
    "native": EXPECTED_CONTROL_HEIGHT,
    "combobox": EXPECTED_CONTROL_HEIGHT,
    "action": EXPECTED_CONTROL_HEIGHT,
    "segment": EXPECTED_SEGMENT_HEIGHT,
}
# Controls on one flex line share a top, give or take an action group's offset;
# the next line starts a whole control lower.
TOLERANCE = 0.6
LINE_GAP = 30.0

CHROMIUM_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Helium.app/Contents/MacOS/Helium",
]


def chromium_candidates() -> list[str]:
    """Return common browser locations for the current operating system."""
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

# A snapshot shaped like the Rust one, with enough variety to exercise the rows:
# fields with and without hints, selects, number inputs, buttons.
SNAPSHOT = {
    "config": {
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
            "strategy": "balanced",
            "failover": True,
            "max_attempts": 3,
            "break_after_failures": 3,
            "cooldown_secs": 60,
            "unknown_model_fallback": None,
            "client_aliases": {"claude-opus-4-1-20250805": "fast"},
            "match_claude_names": True,
            # The balanced strategy is what shows the election card, so the layout
            # harness has to exercise it.
            "scoring": {
                "price_weight": 0.5,
                "latency_weight": 0.5,
                "reference_input_tokens": 1000,
                "reference_output_tokens": 500,
            },
            "elect_on_start": True,
            "naming_style": "anthropic",
            "capability_filter": True,
            "strict_capability_filter": False,
            "policies": {"policies": [], "default_policy": None, "clients": []},
        },
        "classifier": {
            "enabled": False,
            "strategy": "priority",
            "failover": True,
            "max_attempts": 2,
            "candidates": [],
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
        "window": {
            "launch_on_login": False,
            "silent_start": False,
            "keep_in_tray": True,
        },
        "vision": {
            "enabled": False,
            "model": None,
            "placeholder": "[Unsupported Image]",
        },
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
                "balance": {"preset": "sub2api", "custom": None},
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
        ],
        "budgets": [
            {
                "id": "budget_global",
                "scope": {"kind": "global"},
                "period": "day",
                "limit": {"currency": "CNY", "amount": 20.0},
                "on_exceeded": {"action": "reject"},
                "enabled": True,
            }
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
                "aliases": ["deepseek-v4-pro"],
                "max_output_tokens": None,
                "pricing": {
                    "currency": "CNY",
                    "input_per_mtok": 2.0,
                    "output_per_mtok": 8.0,
                    "cache_read_per_mtok": 0.5,
                    "cache_write_per_mtok": None,
                },
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
    },
    "exposed_ids": ["deepseek-deepseek-chat", "anthropic-mystery"],
    "issues": [],
    "blocking": False,
    "server": {
        "running": True,
        "address": "127.0.0.1:8787",
        "base_url": "http://127.0.0.1:8787",
        "host": "127.0.0.1",
        "port": 8787,
        "require_auth": True,
        "token_hint": "zr-\u2026test",
        "exposed": False,
    },
    "keys": {"deepseek": True, "anthropic": False},
    "health": [
        {
            "model_id": "deepseek-deepseek-chat",
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
        "cost": {"CNY": 0.75},
        "per_model": [
            {
                "model_id": "deepseek-deepseek-chat",
                "requests": 4,
                "failures": 1,
                "input_tokens": 120,
                "output_tokens": 64,
                "reasoning_tokens": 12,
                "cached_tokens": 0,
                "cost": {"CNY": 0.75},
                "avg_latency_ms": 812.5,
            }
        ],
        "per_kind": [],
    },
    "recent": [
        {
            "id": "req_1",
            "at": "2026-01-01T00:00:00Z",
            "ingress": "anthropic",
            "kind": "main",
            "requested_model": "sonnet-class",
            "resolved_model": "deepseek-deepseek-chat",
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
            "cost": {"currency": "CNY", "amount": 0.42},
            "attempts": 1,
        }
    ],
    "warning": None,
    "config_path": "/tmp/config.json",
    "version": "0.3.0",
    "election": {
        "decided_at": "2026-01-01T00:00:00Z",
        "scoring": {
            "price_weight": 0.5,
            "latency_weight": 0.5,
            "reference_input_tokens": 1000,
            "reference_output_tokens": 500,
        },
        "tiers": {
            "standard": {
                "tier": "standard",
                "priced": True,
                "note": None,
                "ranked": [
                    {
                        "model_id": "deepseek-deepseek-chat",
                        "score": 0.82,
                        "latency_ms": 640,
                        "price": {"currency": "CNY", "amount": 0.006},
                        "note": "primary: 640 ms, 0.0060 CNY per reference request",
                    }
                ],
            }
        },
    },
    "budgets": [
        {
            "budget": {
                "id": "budget_global",
                "scope": {"kind": "global"},
                "period": "day",
                "limit": {"currency": "CNY", "amount": 20.0},
                "on_exceeded": {"action": "reject"},
                "enabled": True,
            },
            "spent": {"currency": "CNY", "amount": 4.5},
            "used": 0.225,
        }
    ],
    "balances": {
        "deepseek": {
            "checked_at": "2026-01-01T00:00:00Z",
            "balance": {"currency": "USD", "remaining": 7.25, "total": 20.0, "used": 12.75},
            "error": None,
        }
    },
}

# The page script. It installs the Tauri stub before the bundle runs and then
# exposes one measurement function; the Python side drives the browser and calls
# it. Nothing is asserted in here, so a broken measurement cannot pass by
# staying silent -- the caller sees exactly the four counts it asked for.
HARNESS = """
<script>
  // Stub the Tauri bridge so the real dashboard renders in a plain browser.
  const SNAPSHOT = __SNAPSHOT__;
  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb) => cb,
    invoke: (cmd) =>
      Promise.resolve(cmd === "fetch_provider_models" ? ["mock-a", "mock-b"] : SNAPSHOT),
  };

  // A control is anything the design system sizes as a control. The two that a
  // query for `input, select` missed are buttons carrying an ARIA role: the
  // Radix Select trigger (role=combobox) and each Segment item (role=radio).
  // A checkbox is excluded on purpose: a toggle's box is the switch itself, not
  // a --control-h control.
  const CONTROL_SELECTOR = 'input, select, [role="combobox"], [role="radio"]';
  // An action group holds plain <button>s, which are controls too.
  const ACTION_SELECTOR = 'button, input, select, [role="combobox"], [role="radio"]';
  // Containers whose children are expected to line up on a baseline.
  const CONTAINER_SELECTOR = '.controls, .row, .setting-row';
  const EXCLUDED_SELECTOR = 'input[type="checkbox"]';

  function kindOf(el) {
    if (el.matches('[role="combobox"]')) return "combobox";
    if (el.matches('[role="radio"]')) return "segment";
    if (el.tagName.toLowerCase() === "button") return "action";
    return "native";
  }

  // A field's own control, or every control inside a plain group. The element
  // itself counts when it is the control: the Models add row has no `.field`,
  // its select, input and button are direct children of `.controls`.
  function controlsIn(box) {
    const isField = box.matches(".field, .setting-left, .setting-right");
    const selector = isField ? CONTROL_SELECTOR : ACTION_SELECTOR;
    const found = [];
    if (box.matches(selector) && !box.matches(EXCLUDED_SELECTOR)) found.push(box);
    for (const el of box.querySelectorAll(selector)) {
      if (el.matches(EXCLUDED_SELECTOR)) continue;
      found.push(el);
    }
    return found;
  }

  window.__zrScan = (scopeSelector, surface) => {
    const scope = document.querySelector(scopeSelector);
    const result = { surface, rows: [], controls: 0, kinds: {} };
    if (!scope) return result;
    scope.querySelectorAll(CONTAINER_SELECTOR).forEach((row, index) => {
      const items = [];
      for (const child of row.children) {
        // Which flex line the box landed on, and where its control sits. The
        // box, not the control, decides the line: a control misplaced inside
        // its own box must not look like a line of its own.
        const groupTop = child.getBoundingClientRect().top;
        for (const el of controlsIn(child)) {
          const r = el.getBoundingClientRect();
          if (r.width === 0 && r.height === 0) continue;
          items.push({
            kind: kindOf(el),
            group: child.className,
            tag: el.tagName.toLowerCase() + (el.type ? ":" + el.type : ""),
            label: (
              child.querySelector(".field-label")?.textContent ||
              el.getAttribute("aria-label") ||
              el.textContent ||
              ""
            ).trim().slice(0, 24),
            hasHint: !!(child.querySelector(".field-hint") || child.querySelector(".setting-desc")),
            groupTop: Math.round(groupTop * 100) / 100,
            top: Math.round(r.top * 100) / 100,
            height: Math.round(r.height * 100) / 100,
          });
        }
      }
      if (items.length === 0) return;
      for (const item of items) {
        result.controls += 1;
        result.kinds[item.kind] = (result.kinds[item.kind] || 0) + 1;
      }
      result.rows.push({ surface, index, items });
    });
    return result;
  };

  window.__zrHarnessReady = true;
</script>
"""


@dataclass(frozen=True)
class Surface:
    """One page or drawer this harness promises to measure.

    ``required`` is a per-kind minimum, so a surface cannot pass by measuring
    some other kind of control. ``drawer`` is the index of the ``Edit`` button
    that mounts the surface, when it is not simply on the page.
    """

    id: str
    label: str
    required: dict[str, int]
    nav: int
    anchor: str
    scope: str = ".page"
    drawer: int | None = None


# Declared coverage. A surface listed here must yield at least ``required``
# controls of each named kind or the run fails; the list, not the DOM, decides
# what "covered" means.
#
# Overview and Activity are not declared because they render no control
# container at all, and the Routing *page* is not declared because its fields
# are mounted on demand -- the two drawers it mounts are declared instead, which
# is exactly the surface the page-level walk used to step over.
SURFACES = (
    Surface(
        "models",
        "Models page",
        {"combobox": 1, "native": 1, "action": 1},
        nav=1,
        anchor=".list-row",
    ),
    Surface(
        "providers",
        "Providers page",
        {"segment": 2, "native": 2, "action": 1},
        nav=2,
        anchor=".list-row",
    ),
    Surface(
        "settings",
        "Settings page",
        {"combobox": 2, "segment": 3, "native": 4, "action": 1},
        nav=5,
        anchor=".setting-row",
    ),
    Surface(
        "routing-default",
        "Routing default drawer",
        {"combobox": 2, "native": 1},
        nav=3,
        anchor=".flow-row",
        scope=".drawer",
        drawer=0,
    ),
    Surface(
        "routing-auto",
        "Routing auto drawer",
        {"combobox": 2, "native": 1},
        nav=3,
        anchor=".flow-row",
        scope=".drawer",
        drawer=1,
    ),
)


@dataclass
class Verdict:
    """Whether one row of controls is laid out correctly."""

    passed: bool
    lines: int
    worst_spread: float
    heights: set[float]


def judge(row: dict) -> Verdict:
    """Check one measured row.

    A row of fields may wrap onto several flex lines, so alignment is judged per
    line. Lines are found by clustering the *box* tops: boxes on one line share a
    top, or sit `--label-h + --field-gap` lower for an action group, while the
    next line starts at least a full control's height further down.

    Clustering on the boxes rather than on the controls is the point: a control
    that is misplaced inside its own box does not move the box, so it stays inside
    its line's cluster and shows up as a spread instead of forming a line of its
    own and looking fine.

    Height is judged per kind, because a Segment's items are 36px by design and
    comparing them against `--control-h` would either fail a correct UI or force
    the segment to be ignored.
    """
    lines: list[list[dict]] = []
    for item in sorted(row["items"], key=lambda i: i["groupTop"]):
        if lines and item["groupTop"] - lines[-1][0]["groupTop"] <= LINE_GAP:
            lines[-1].append(item)
        else:
            lines.append([item])

    heights = {item["height"] for item in row["items"]}
    spreads = [
        max(i["top"] for i in line) - min(i["top"] for i in line) for line in lines
    ]
    aligned = all(spread <= TOLERANCE for spread in spreads)

    kinds: dict[str, list[float]] = {}
    for item in row["items"]:
        kinds.setdefault(item["kind"], []).append(item["height"])
    sized = True
    for kind, values in kinds.items():
        expected = KIND_HEIGHTS[kind]
        if max(values) - min(values) > TOLERANCE:
            sized = False
        if any(abs(value - expected) > TOLERANCE for value in values):
            sized = False

    return Verdict(aligned and sized, len(lines), max(spreads), heights)


def coverage_failures(measured: dict[str, dict], surfaces=SURFACES) -> list[str]:
    """Return every declared surface that did not yield what it declared.

    This is the hole the harness used to have: measurement that finds nothing is
    indistinguishable from measurement that passes, so a surface that silently
    stopped rendering reported success by staying quiet.
    """
    problems: list[str] = []
    for surface in surfaces:
        result = measured.get(surface.id)
        if result is None:
            problems.append("%s was never measured" % surface.label)
            continue
        kinds = result.get("kinds") or {}
        if not result.get("controls"):
            problems.append("%s measured zero controls" % surface.label)
        for kind, minimum in surface.required.items():
            found = kinds.get(kind, 0)
            if found < minimum:
                problems.append(
                    "%s measured %d %s control(s), declared minimum %d"
                    % (surface.label, found, kind, minimum)
                )
    return problems


def self_test() -> int:
    """Prove the checker still rejects what it is meant to reject.

    The browser half cannot be asserted about, but the verdict and the coverage
    gate are pure and the regressions worth catching are easy to state as numbers.
    """
    def row(items: list[tuple[float, float, float]], kind: str = "native") -> dict:
        return {
            "items": [
                {
                    "kind": kind,
                    "groupTop": g,
                    "top": t,
                    "height": h,
                    "tag": "input",
                    "hasHint": False,
                }
                for g, t, h in items
            ]
        }

    cases: list[tuple[str, dict, bool]] = [
        # One line, everything where it belongs.
        ("aligned single line", row([(100, 118, 30), (100, 118, 30)]), True),
        # Two lines: the second starts 56px lower, and each is internally aligned.
        (
            "wrapped but aligned",
            row([(100, 118, 30), (100, 118, 30), (156, 174, 30), (156, 174, 30)]),
            True,
        ),
        # The bug this test was written for: hint text pushed one control 17px up
        # while its field box stayed put.
        ("control off its line", row([(100, 118, 30), (100, 101, 30)]), False),
        # A native select that picked its own height.
        ("mixed heights", row([(100, 118, 30), (100, 118, 32)]), False),
        # Uniform but wrong: every control 32px, so `--control-h` was not applied.
        ("uniformly wrong height", row([(100, 118, 32), (100, 118, 32)]), False),
        # An action group sits lower than the fields but its button lines up.
        ("action group on the same line", row([(100, 118, 30), (118, 118, 30)]), True),
        # A misplaced control on the second line must not hide behind the first.
        (
            "second line broken",
            row([(100, 118, 30), (156, 174, 30), (156, 190, 30)]),
            False,
        ),
        # A Segment is 36px by design, and that is not a defect.
        (
            "segment items at the segment height",
            row([(100, 118, 36), (100, 118, 36)], kind="segment"),
            True,
        ),
        # ...unless one item falls back to the field height.
        (
            "segment item at the field height",
            row([(100, 118, 36), (100, 118, 30)], kind="segment"),
            False,
        ),
        # A combobox trigger carries --control-h like the input it replaces.
        (
            "combobox trigger at the control height",
            row([(100, 118, 30), (100, 118, 30)], kind="combobox"),
            True,
        ),
        (
            "combobox trigger off the control height",
            row([(100, 118, 34), (100, 118, 34)], kind="combobox"),
            False,
        ),
    ]

    failures = 0
    for name, case, expected in cases:
        got = judge(case).passed
        ok = got == expected
        failures += not ok
        print(f"  {'ok  ' if ok else 'FAIL'} {name}: expected {expected}, got {got}")

    # The coverage gate: a declared surface that yields nothing, or the wrong
    # kind, must fail even though there is no row to judge.
    def measured(surface: str, controls: int, kinds: dict[str, int]) -> dict:
        return {surface: {"surface": surface, "rows": [], "controls": controls, "kinds": kinds}}

    one = (Surface("only", "Only surface", {"combobox": 2, "native": 1}, nav=0, anchor=".x"),)
    coverage_cases: list[tuple[str, dict, bool]] = [
        (
            "declared surface fully measured",
            measured("only", 3, {"combobox": 2, "native": 1}),
            True,
        ),
        (
            "declared surface yields zero controls",
            measured("only", 0, {}),
            False,
        ),
        (
            "declared surface is missing entirely",
            {},
            False,
        ),
        (
            "declared surface loses a required kind",
            measured("only", 3, {"native": 3}),
            False,
        ),
    ]
    for name, results, expected in coverage_cases:
        got = not coverage_failures(results, one)
        ok = got == expected
        failures += not ok
        print(f"  {'ok  ' if ok else 'FAIL'} {name}: expected {expected}, got {got}")

    print()
    total = len(cases) + len(coverage_cases)
    if failures:
        print(f"{failures} self test(s) failed")
        return 1
    print(f"all {total} self tests passed")
    return 0


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


def serve(directory: str) -> tuple[http.server.ThreadingHTTPServer, int]:
    """Static file server for the staged build."""

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_args):
            pass

    handler = functools.partial(Handler, directory=directory)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, server.server_port


def free_port() -> int:
    """Reserve a port for the browser's DevTools endpoint."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def page_target(port: int, expected_prefix: str, timeout: float = 30.0) -> str | None:
    """Wait for the page the browser opened to expose its DevTools socket."""
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


class ProtocolError(Exception):
    """The DevTools conversation did not produce what was asked for."""


class WebSocket:
    """Just enough of RFC 6455 to speak the DevTools protocol, no dependencies.

    Text frames only, with masked client frames and a pong for every ping. The
    server side is never masked, so the reader skips that branch but still
    handles the length encodings and continuation frames.
    """

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
    """The handful of DevTools calls this harness needs."""

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


def measure_surfaces(devtools: DevTools) -> dict[str, dict]:
    """Walk every declared surface, opening drawers with real input."""
    devtools.wait_for(
        "document.querySelector('.nav-item') !== null", "the dashboard shell"
    )
    measured: dict[str, dict] = {}
    for surface in SURFACES:
        devtools.click(devtools.element(".nav-item", surface.nav))
        devtools.wait_for(
            "document.querySelector(%s) !== null" % json.dumps(surface.anchor),
            surface.anchor,
        )
        if surface.drawer is not None:
            devtools.click(devtools.element(".section-head button.linky", surface.drawer))
            devtools.wait_for(
                "document.querySelector('.drawer') !== null", "drawer %d" % surface.drawer
            )
        measured[surface.id] = devtools.evaluate(
            "window.__zrScan(%s, %s)" % (json.dumps(surface.scope), json.dumps(surface.id))
        )
        if surface.drawer is not None:
            devtools.click(devtools.element(".drawer-head button"))
            devtools.wait_for(
                "document.querySelector('.drawer') === null", "drawer to close"
            )
    return measured


def report(measured: dict[str, dict]) -> int:
    """Print every measured surface and row; fail on a geometry or coverage hole."""
    failures = 0
    for surface in SURFACES:
        result = measured.get(surface.id) or {"rows": [], "controls": 0, "kinds": {}}
        kinds = ", ".join(
            "%s %d" % (kind, count) for kind, count in sorted(result["kinds"].items())
        )
        print(
            "  %-24s %d controls (%s), %d rows"
            % (surface.label, result["controls"], kinds or "none", len(result["rows"]))
        )
        for row in result["rows"]:
            verdict = judge(row)
            status = "ok  " if verdict.passed else "FAIL"
            if not verdict.passed:
                failures += 1
            labels = ", ".join(
                "%s:%s%s"
                % (i["kind"], i["tag"], "*" if i["hasHint"] else "")
                for i in row["items"]
            )
            wrapped = ", %d lines" % verdict.lines if verdict.lines > 1 else ""
            print(
                "    %s row %d: %d controls [%s] top spread %.2fpx, heights %s%s"
                % (
                    status,
                    row["index"],
                    len(row["items"]),
                    labels,
                    verdict.worst_spread,
                    sorted(verdict.heights),
                    wrapped,
                )
            )
        print()

    for problem in coverage_failures(measured):
        print("  FAIL %s" % problem)
        failures += 1

    print()
    if failures:
        print("%d check(s) failed" % failures)
        return 1
    total = sum(result["controls"] for result in measured.values())
    print(
        "all %d declared surfaces measured, %d controls on one baseline per line "
        "and at their own kind's height (* = has hint text)" % (len(SURFACES), total)
    )
    return 0


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()
    if not os.path.exists(os.path.join(DIST, "index.html")):
        print("ui/dist is missing; run `pnpm --dir ui build` first")
        return 2
    browser = find_chromium()
    if not browser:
        print("no Chromium based browser found; the layout test did not run")
        return 2

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
    debugging_port = free_port()
    url = "http://127.0.0.1:%d/" % port
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
            # The harness drives the page instead of trusting it to report
            # itself, so it needs the DevTools socket. Nothing sends an Origin
            # header, which is what Chrome's origin check would reject.
            "--remote-debugging-port=%d" % debugging_port,
            f"--user-data-dir={profile}",
            url,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        socket_url = page_target(debugging_port, url)
        if not socket_url:
            print("the browser exposed no DevTools page target; the layout test did not run")
            return 1
        devtools = DevTools(socket_url)
        try:
            measured = measure_surfaces(devtools)
        finally:
            devtools.close()
    except (ProtocolError, OSError, ValueError, socket.timeout) as error:
        print("the layout test could not measure the dashboard: %s" % error)
        return 1
    finally:
        browser_proc.terminate()
        try:
            browser_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            browser_proc.kill()
        server.shutdown()
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(stage, ignore_errors=True)

    return report(measured)


if __name__ == "__main__":
    sys.exit(main())
