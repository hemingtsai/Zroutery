#!/usr/bin/env python3
"""Narrow-window overflow test: the shell must not scroll sideways at 820x560.

The desktop window is resizable down to 820x560, and that size is where the
stylesheet used to break: a 200px sidebar and 32px of content padding left
556px for tables that need more, so the *page* scrolled horizontally and the
sidebar and the main bar slid out of view with it. A horizontal scrollbar on the
whole window is the one layout defect that cannot be seen at 1200px, which is the
window size the other two harnesses use.

This test renders the real built UI at exactly 820x560 with the same stubbed
Tauri bridge as ``ui_layout_test.py``, walks every page by ``data-page``, and
fails on any document wider than the viewport. Tables are allowed to scroll;
the document is not. The offending elements are printed so a failure names the
rule that broke rather than only the number.

Usage: python3 scripts/ui_overflow_test.py [--width 820 --height 560]
Requires: `pnpm --dir ui build` (dist/) and any Chromium based browser.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import ui_layout_test as layout  # noqa: E402  (the harness is shared on purpose)

DEFAULT_WIDTH = 820
DEFAULT_HEIGHT = 560

PAGES = (
    "overview",
    "providers",
    "models",
    "routing",
    "accounts",
    "activity",
    "takeover",
    "cutover",
    "settings",
)

# One pixel of rounding is not a defect; a page wider than the window is.
TOLERANCE = 1

MEASURE = """
(() => {
  const width = window.innerWidth;
  const doc = document.documentElement;
  const content = document.querySelector(".content");
  const page = document.querySelector(".page");
  const offenders = [];
  for (const el of document.querySelectorAll(".page *")) {
    const box = el.getBoundingClientRect();
    if (box.width > 0 && box.right > width + %d) {
      // A table inside its own scroller is the intended answer, not an
      // overflow: skip anything that has a scrolling ancestor.
      let parent = el.parentElement;
      let scrolled = false;
      while (parent && parent !== document.body) {
        const style = getComputedStyle(parent);
        if (/(auto|scroll|hidden)/.test(style.overflowX)) {
          scrolled = true;
          break;
        }
        parent = parent.parentElement;
      }
      if (scrolled) continue;
      const name = el.tagName.toLowerCase();
      const cls = typeof el.className === "string" ? el.className.trim().split(/\\s+/).join(".") : "";
      offenders.push(name + (cls ? "." + cls : "") + " right=" + Math.round(box.right));
    }
  }
  return {
    innerWidth: width,
    documentWidth: Math.max(doc.scrollWidth, document.body.scrollWidth),
    // `overflow-x: hidden` does not reduce `scrollWidth`: it clips the paint
    // and leaves the full content width reported here. So comparing these two
    // is how the harness tells "the table scrolls" from "the table is cut off".
    contentWidth: content ? content.scrollWidth : 0,
    contentBox: content ? content.clientWidth : 0,
    pageWidth: page ? page.scrollWidth : 0,
    pageBox: page ? page.clientWidth : 0,
    offenders: offenders.slice(0, 8),
  };
})()
""" % TOLERANCE

# A table wider than the window must be absorbed by a scrolling ancestor, not
# clipped by `.content`'s `overflow-x: hidden`.
#
# The fixture's tables happen to fit 660px of content at 820x560, so measuring
# them proves the page is fine today but not that the containment rule works. The
# probe widens the page's first table to 1200px and re-measures: `scrollWidth` is
# reported for clipped content too, so if no ancestor scrolls, `.content` grows.
# The table's own width is returned as well, so a probe that did nothing cannot
# be read as a pass.
PROBE = """
(() => {
  const table = document.querySelector(".page table");
  if (!table) return { skipped: true };
  const content = document.querySelector(".content");
  const before = content.scrollWidth;
  const previous = table.style.minWidth;
  table.style.minWidth = "1200px";
  const forced = table.getBoundingClientRect().width;
  const after = content.scrollWidth;
  table.style.minWidth = previous;
  return {
    skipped: false,
    before: before,
    after: after,
    forcedWidth: Math.round(forced),
    box: content.clientWidth,
  };
})()
"""


def launch(
    profile: str, debugging_port: int, browser: str, url: str, width: int, height: int
) -> subprocess.Popen:
    return subprocess.Popen(
        [
            browser,
            "--headless=old",
            "--no-sandbox",
            "--disable-gpu",
            "--disable-software-rasterizer",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-sync",
            "--hide-scrollbars",
            "--window-size=%d,%d" % (width, height),
            "--remote-debugging-port=%d" % debugging_port,
            "--user-data-dir=%s" % profile,
            url,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def measure(devtools: layout.DevTools) -> dict:
    return devtools.evaluate(MEASURE)


def main() -> int:
    width = DEFAULT_WIDTH
    height = DEFAULT_HEIGHT
    for index, argument in enumerate(sys.argv[1:]):
        if argument == "--width" and index + 2 < len(sys.argv):
            width = int(sys.argv[index + 2])
        if argument == "--height" and index + 2 < len(sys.argv):
            height = int(sys.argv[index + 2])

    if not os.path.exists(os.path.join(layout.DIST, "index.html")):
        print("ui/dist is missing; run `pnpm --dir ui build` first")
        return 2
    browser = layout.find_chromium()
    if not browser:
        print("no Chromium based browser found; the overflow test did not run")
        return 2

    stage = tempfile.mkdtemp(prefix="zr-overflow-")
    shutil.copytree(layout.DIST, stage, dirs_exist_ok=True)
    index = os.path.join(stage, "index.html")
    with open(index, encoding="utf-8") as handle:
        page = handle.read()
    page = page.replace(
        "</body>",
        layout.HARNESS.replace("__SNAPSHOT__", json.dumps(layout.SNAPSHOT)) + "</body>",
    )
    with open(index, "w", encoding="utf-8") as handle:
        handle.write(page)

    server, port = layout.serve(stage)
    profile = tempfile.mkdtemp(prefix="zr-overflow-profile-")
    debugging_port = layout.free_port()
    url = "http://127.0.0.1:%d/" % port
    browser_proc = launch(profile, debugging_port, browser, url, width, height)
    failures = 0
    try:
        socket_url = layout.page_target(debugging_port, url)
        if not socket_url:
            print("the browser never exposed its page; the test did not run")
            return 2
        devtools = layout.DevTools(socket_url)
        try:
            devtools.call(
                "Emulation.setDeviceMetricsOverride",
                {
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": 1,
                    "mobile": False,
                },
            )
            devtools.wait_for(
                "document.querySelector('.nav-item') !== null", "the dashboard shell"
            )
            for name in PAGES:
                devtools.click(devtools.element('[data-page="%s"]' % name))
                # Let the page's entrance animation finish so a transform is not
                # measured mid-flight.
                time.sleep(0.2)
                result = measure(devtools)
                over = result["documentWidth"] - result["innerWidth"]
                clipped = result["contentWidth"] - result["contentBox"]
                if over > TOLERANCE or clipped > TOLERANCE:
                    failures += 1
                    print(
                        "  FAIL %-9s document %dpx in a %dpx window (+%dpx), "
                        "content %dpx wide in %dpx (+%dpx)"
                        % (
                            name,
                            result["documentWidth"],
                            result["innerWidth"],
                            over,
                            result["contentWidth"],
                            result["contentBox"],
                            clipped,
                        )
                    )
                    for offender in result["offenders"]:
                        print("         %s" % offender)
                else:
                    print(
                        "  ok   %-9s document %dpx in a %dpx window, content fits %dpx"
                        % (name, result["documentWidth"], result["innerWidth"], result["contentBox"])
                    )

            # The containment probe, on the first page that has a table.
            probed = False
            for name in PAGES:
                devtools.click(devtools.element('[data-page="%s"]' % name))
                time.sleep(0.15)
                if not devtools.evaluate(
                    "document.querySelector('.page table') !== null"
                ):
                    continue
                probe = devtools.evaluate(PROBE)
                probed = True
                print("  ..   probe on the %s page" % name)
                if probe["forcedWidth"] < 1000:
                    print(
                        "  FAIL the probe did not widen the table (%dpx): the "
                        "containment check proved nothing" % probe["forcedWidth"]
                    )
                    failures += 1
                elif probe["after"] > probe["box"] + TOLERANCE:
                    print(
                        "  FAIL a 1200px table widened .content to %dpx in a %dpx "
                        "column, so it is clipped rather than scrolled"
                        % (probe["after"], probe["box"])
                    )
                    failures += 1
                else:
                    print(
                        "  ok   a 1200px table is contained: .content stayed %dpx"
                        % probe["box"]
                    )
                break
            if not probed:
                print("  FAIL no page rendered a table to probe with")
                failures += 1
        finally:
            devtools.close()
    finally:
        browser_proc.kill()
        try:
            browser_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            browser_proc.kill()
        server.shutdown()
        shutil.rmtree(profile, ignore_errors=True)
        shutil.rmtree(stage, ignore_errors=True)

    print()
    if failures:
        print("%d check(s) failed at %dx%d" % (failures, width, height))
        return 1
    print("all %d pages fit %dx%d with no horizontal page scroll" % (len(PAGES), width, height))
    return 0

if __name__ == "__main__":
    sys.exit(main())
