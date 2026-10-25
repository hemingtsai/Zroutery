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
JS_TOTAL_MAX = 700_000
CSS_TOTAL_MAX = 180_000
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

    def report(self, out) -> None:
        print(f"  {self.name}: {self.total:,} B of {self.limit:,} B", file=out)
        for relative, size in sorted(self.files, key=lambda item: -item[1])[:SHOWN]:
            print(f"    {size:>9,} B  {relative}", file=out)


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


def check(dist: str, out=sys.stdout) -> int:
    """Print one table and return the number of budgets that were exceeded."""
    javascript, stylesheet, largest = measure(dist)
    print(f"frontend size budget ({os.path.basename(dist)}/)", file=out)
    over = 0
    for budget in (javascript, stylesheet, largest):
        budget.report(out)
        if budget.over():
            over += 1
            print(
                f"::error::{budget.name} is {budget.total:,} B, "
                f"over the {budget.limit:,} B budget",
                file=out,
            )
    if over == 0:
        print("PASS: the shipped bundle is inside its size budget", file=out)
        return 0
    print(
        "Raise a budget only with the feature that needs it, and say so in the "
        "commit that raises it.",
        file=out,
    )
    return 1


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
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    return check(args.dist)


if __name__ == "__main__":
    sys.exit(main())
