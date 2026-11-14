#!/usr/bin/env python3
"""Check that every architecture invariant names executable evidence or untested.

The table is the claim ledger: each row must have a test anchor under
`crates/zroutery-core/tests/` (or a unit-test anchor) or an explicit `untested`
with a reason. A source anchor alone is not enough.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import docs_sources

ROOT = Path(__file__).resolve().parents[1]
ARCH = Path("docs/development/architecture.md")
ROW_RE = re.compile(r"^\|\s*(\d+)\s*\|")
ANCHOR_RE = re.compile(r"`([^`]+::[^`]+)`")


def rows(text: str) -> list[tuple[int, str, str]]:
    in_table = False
    parsed: list[tuple[int, str, str]] = []
    for line in text.splitlines():
        if line.strip() == "### Invariant evidence":
            in_table = True
            continue
        if in_table and line.startswith("## "):
            break
        if not in_table:
            continue
        match = ROW_RE.match(line)
        if not match:
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) < 3:
            continue
        parsed.append((int(match.group(1)), cells[1], cells[2]))
    return parsed


def check(root: Path) -> tuple[list[str], int, int]:
    text = (root / ARCH).read_text(encoding="utf-8")
    parsed = rows(text)
    errors: list[str] = []
    if len(parsed) != 15:
        errors.append(f"{ARCH}: expected 15 invariant rows, found {len(parsed)}")
    numbers = [number for number, _, _ in parsed]
    if numbers != list(range(1, 16)):
        errors.append(f"{ARCH}: invariant numbers must be 1..15, found {numbers}")

    tested = 0
    untested = 0
    for number, _title, evidence in parsed:
        if "untested" in evidence:
            untested += 1
            if "—" not in evidence and "-" not in evidence:
                errors.append(f"{ARCH}: invariant {number} is untested without a reason")
            continue
        anchors = ANCHOR_RE.findall(evidence)
        if not anchors:
            errors.append(f"{ARCH}: invariant {number} has neither a test anchor nor untested")
            continue
        for anchor in anchors:
            path, symbol = anchor.rsplit("::", 1)
            problem = docs_sources.check_citation(root, path, symbol, None, None)
            if problem:
                errors.append(f"{ARCH}: invariant {number}: {problem}")
        tested += 1
    return errors, tested, untested


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--root", default=str(ROOT))
    args = parser.parse_args(argv)
    errors, tested, untested = check(Path(args.root))
    if errors:
        for error in errors:
            print(f"invariant-check: {error}", file=sys.stderr)
        return 1
    print(f"invariant-check: 15 invariants, {tested} with test anchors, {untested} explicitly untested")
    return 0


if __name__ == "__main__":
    sys.exit(main())
