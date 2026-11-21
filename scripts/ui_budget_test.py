#!/usr/bin/env python3
"""Frontend size budget: the built bundle stays inside a stated ceiling.

The desktop binary embeds ``ui/dist`` at compile time, so a chunk that doubles is
paid twice: once on the update the user downloads, and again on every start when
the webview parses it. Neither shows up in a test that only asks whether the page
renders, and a reviewer reading a diff cannot see it either.

So this gate measures the shipped files, not the source tree: total JavaScript,
total CSS, and the largest single chunk, each against a budget with headroom over
the build that was current when the number was written. The budgets are ceilings,
not targets; raising one is a deliberate commit that says which feature bought
the space.

Fonts are excluded on purpose. They are subset per glyph range and several
megabytes in total, loaded on demand by ``unicode-range`` rather than parsed at
startup, so folding them in would make the number a statement about coverage the
interface may never render.

Usage: python3 scripts/ui_budget_test.py [--dist ui/dist] [--self-test]
Requires: ``pnpm --dir ui build`` (dist/ exists).
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
DEFAULT_DIST = os.path.join(REPO, "ui", "dist")

# Bytes. The build that set these wrote 547 kB of JavaScript across four chunks,
# 129 kB of CSS across two files, and a 272 kB entry; the headroom absorbs one
# more component without a conversation, and stops absorbing after that.
#
# Raised from 700_000 B when the import drawer learned to find history sources on
# this machine and remember the ones used before: the list, its two labels and
# the discovery call are what bought the 6 kB, and nothing else may spend it
# without saying so here.
#
# About 2 kB of that went to the year heatmap and the lede: sizing cells to the
# measured panel, month names under the grid, the reading line that makes the
# measure selector visible, and naming imported history in the period. A
# further ~1.2 kB went to merging imported history into the headline and curve
# and to the two notes that say what the split still excludes. The build is at
# 704,010 B, so roughly 2 kB remains.
# The staged history workflow adds cancellable preview/publication, saved-preview
# recovery, range controls and three-locale feedback. Request diagnostics now
# distinguish inbound/outbound protocols and per-attempt declared model identity;
# the six task doors and exact effort-evidence editor are reachable in basic mode.
# Measured before this slice: 704,010 B; after: about 715 kB (+~1.6%). This feature
# allowance is bounded at 726 kB; CSS and largest-chunk ceilings are unchanged.
# 720,000 -> 726,000: batch A of the 2026-11-06 task-experience review (A1
# add-provider save discipline, A2 input/IME/Esc contract, A3 save-feedback
# scoping, A4 add-form landing, A5 price sub-row) costs ~5.1 kB of shipped JS.
# 726,000 -> 742,000: M2b imported history browse/manage (Activity drawer):
# query/paging filters, fact table, filtered export, scoped/all clear controls,
# and synced i18n. Measured at 733,590 B after this slice.
# 742,000 -> 848,000: the interactive components' behaviour layer moved to
# Radix primitives (dialog, dropdown-menu, popover, toast, tabs, toggle-group),
# replacing the hand-rolled focus traps, keyboard nav and overlay stack;
# confirm/drawer/menu/toast/tabs/segment keep their classes and public props,
# and the theme packs still own every visual via the ThemeV1 compiler. The
# deleted hand-rolled logic did not offset the primitives. Measured at
# 830,381 B with the Radix set installed; react-select 2.3.7 -> 2.4.1 (the
# in-Dialog keyboard fix) brought the final build to 799,803 B. CSS and
# largest-chunk ceilings are unchanged.
JS_TOTAL_MAX = 848_000
# 180,000 -> 190,000: the 2026-11-21 theme batches bought it. Full interaction
# state coverage in both plugin packs (pressed/disabled/checked/hover recipes
# for buttons, checkbox, selects, menus, navigation, segments, tables); MD3
# dynamic color, where eleven container/neutral roles each emit a baseline
# literal and a relative-color oklch() declaration; the ThemeV1 motion surface
# (per-skin duration/easing variables plus selection transitions on
# navigation/segment/KPI parts); and the nav icon slot rules. Measured at
# 187,494 B after the batch; JS and largest-chunk ceilings are unchanged.
CSS_TOTAL_MAX = 190_000
LARGEST_CHUNK_MAX = 320_000

# What the report prints, largest first. Enough to name the file that grew
# without turning the job log into a directory listing.
SHOWN = 5


class Budget:
    """One measured total and its ceiling."""

    def __init__(self, name: str, limit: int) -> None:
        self.name = name
        self.limit = limit
        self.total = 0
        self.files: list[tuple[str, int]] = []

    def add(self, relative: str, size: int) -> None:
        self.total += size
        self.files.append((relative, size))

    def over(self) -> bool:
        return self.total > self.limit


def measure(dist: str) -> tuple[Budget, Budget, Budget]:
    """Walk ``dist`` and return the JavaScript, CSS and largest-chunk budgets."""
    if not os.path.isdir(dist):
        raise SystemExit(
            f"no built frontend at {dist}; run `pnpm --dir ui build` first"
        )

    javascript = Budget("JavaScript", JS_TOTAL_MAX)
    stylesheet = Budget("CSS", CSS_TOTAL_MAX)
    largest = Budget("largest chunk", LARGEST_CHUNK_MAX)

    found = 0
    for root, _dirs, files in os.walk(dist):
        for name in sorted(files):
            if not name.endswith((".js", ".css")):
                continue
            path = os.path.join(root, name)
            size = os.path.getsize(path)
            relative = os.path.relpath(path, dist).replace(os.sep, "/")
            found += 1
            if name.endswith(".js"):
                javascript.add(relative, size)
                if size > largest.total:
                    largest.total = size
                    largest.files = [(relative, size)]
            else:
                stylesheet.add(relative, size)

    if found == 0:
        raise SystemExit(
            f"{dist} holds no .js or .css; the build did not produce assets"
        )
    return javascript, stylesheet, largest


def report(dist: str) -> tuple[list[str], int]:
    """The budget table, and how many budgets it went over."""
    javascript, stylesheet, largest = measure(dist)
    lines = [f"frontend size budget ({os.path.basename(dist)}/)"]
    over = 0
    for budget in (javascript, stylesheet, largest):
        lines.append(
            f"  {budget.name}: {budget.total:,} B of {budget.limit:,} B"
        )
        for relative, size in sorted(budget.files, key=lambda item: -item[1])[:SHOWN]:
            lines.append(f"    {size:>9,} B  {relative}")
        if budget.over():
            over += 1
            lines.append(
                f"::error::{budget.name} is {budget.total:,} B, "
                f"over the {budget.limit:,} B budget"
            )
    if over == 0:
        lines.append("PASS: the shipped bundle is inside its size budget")
    else:
        lines.append(
            "Raise a budget only with the feature that needs it, and say so in "
            "the commit that raises it."
        )
    return lines, over


def check(dist: str, out=sys.stdout) -> int:
    """Print one table and return 1 when a budget was exceeded."""
    lines, over = report(dist)
    for line in lines:
        print(line, file=out)
    return 1 if over else 0


def self_test() -> int:
    """Two synthetic trees: one inside budget, one over it."""
    root = tempfile.mkdtemp(prefix="zr-budget-")
    try:
        inside = os.path.join(root, "inside", "assets")
        over = os.path.join(root, "over", "assets")
        os.makedirs(inside)
        os.makedirs(over)
        for name, size in (("index-abc.js", 1000), ("vendor-def.js", 500), ("app.css", 200)):
            with open(os.path.join(inside, name), "wb") as handle:
                handle.write(b"x" * size)
        with open(os.path.join(over, "index-abc.js"), "wb") as handle:
            handle.write(b"x" * (LARGEST_CHUNK_MAX + 1))

        quiet = open(os.devnull, "w", encoding="utf-8")
        try:
            if check(os.path.dirname(inside), quiet) != 0:
                print("FAIL: a tree inside every budget was rejected", file=sys.stderr)
                return 1
            if check(os.path.dirname(over), quiet) != 1:
                print("FAIL: a tree over the largest-chunk budget passed", file=sys.stderr)
                return 1
        finally:
            quiet.close()
        print("PASS: budget fixtures")
        return 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dist", default=DEFAULT_DIST)
    # One table, two readers: the job log and the artifact. Writing the file
    # here rather than tee-ing the console keeps the CI step a single command
    # that means the same thing in PowerShell and in bash.
    parser.add_argument(
        "--report",
        metavar="PATH",
        help="also write the table to PATH, pass or fail",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    lines, over = report(args.dist)
    for line in lines:
        print(line)
    if args.report:
        with open(args.report, "w", encoding="utf-8") as handle:
            for line in lines:
                print(line, file=handle)
    return 1 if over else 0


if __name__ == "__main__":
    sys.exit(main())
