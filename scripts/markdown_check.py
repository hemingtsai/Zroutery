#!/usr/bin/env python3
"""Structural Markdown checks without a third-party parser.

Checks: fenced-code closure, a table header followed by a separator, table
continuity (a pipe row outside a table is an error), and column counts.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = Path("docs/docs.manifest.json")
FENCE_RE = re.compile(r"^\s*```")
SEPARATOR_RE = re.compile(r"^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?\s*$")


def cells(line: str) -> int:
    return len(line.strip().strip("|").split("|"))


def check_text(name: str, text: str) -> list[str]:
    errors: list[str] = []
    lines = text.splitlines()
    in_fence = False
    fence_line = 0
    in_table = False
    table_cells = 0
    index = 0
    while index < len(lines):
        line = lines[index]
        if FENCE_RE.match(line):
            if in_fence:
                in_fence = False
            else:
                in_fence = True
                fence_line = index + 1
            index += 1
            continue
        if in_fence:
            index += 1
            continue
        if line.lstrip().startswith("|"):
            if not in_table:
                if index + 1 >= len(lines) or not SEPARATOR_RE.match(lines[index + 1]):
                    errors.append(f"{name}:{index + 1}: table row without a separator after a header")
                    index += 1
                    continue
                table_cells = cells(line)
                if cells(lines[index + 1]) != table_cells:
                    errors.append(f"{name}:{index + 1}: table separator has a different column count")
                in_table = True
                index += 2
                continue
            if cells(line) != table_cells:
                errors.append(f"{name}:{index + 1}: table row has {cells(line)} columns, expected {table_cells}")
            index += 1
            continue
        in_table = False
        index += 1
    if in_fence:
        errors.append(f"{name}:{fence_line}: unclosed code fence")
    return errors


def active_documents(root: Path) -> list[Path]:
    manifest = json.loads((root / MANIFEST).read_text(encoding="utf-8"))
    paths: list[Path] = []
    for entry in manifest.get("docs", []):
        if "path" in entry:
            paths.append(root / entry["path"])
        elif "glob" in entry:
            paths.extend(sorted(root.glob(entry["glob"])))
    return paths


def check(root: Path) -> list[str]:
    errors: list[str] = []
    for path in active_documents(root):
        if path.suffix != ".md" or not path.is_file():
            continue
        errors.extend(check_text(str(path.relative_to(root)), path.read_text(encoding="utf-8")))
    return errors


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-md-") as temporary:
        root = Path(temporary)
        (root / "docs").mkdir()
        (root / "docs" / "good.md").write_text(
            "# T\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n```sh\nx\n```\n", encoding="utf-8"
        )
        (root / "docs" / "bad.md").write_text(
            "# T\n\n| A | B |\n| 1 | 2 |\n\n```sh\nx\n", encoding="utf-8"
        )
        good = check_text("good.md", (root / "docs" / "good.md").read_text())
        bad = check_text("bad.md", (root / "docs" / "bad.md").read_text())
        if good:
            print(f"markdown-check self-test: good document rejected ({good})", file=sys.stderr)
            return 1
        if len(bad) < 2:
            print(f"markdown-check self-test: malformed document not caught ({bad})", file=sys.stderr)
            return 1
        print("markdown-check: self-test passed (accepts a valid table/fence, rejects a missing separator and an unclosed fence)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    errors = check(ROOT)
    if errors:
        for error in errors:
            print(f"markdown-check: {error}", file=sys.stderr)
        return 1
    print("markdown-check: fenced blocks and tables are well formed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
