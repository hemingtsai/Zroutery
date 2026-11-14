#!/usr/bin/env python3
"""Check that the three README files describe the same product.

The three files are translations, not independent documents: their heading
structure, code-block count, image count and source anchors must match. The
prose is allowed to differ; the shape is not.

Usage:
    python3 -B scripts/docs_parity.py --check
    python3 -B scripts/docs_parity.py --self-test
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

import docs_sources

ROOT = Path(__file__).resolve().parents[1]
READMES = ("README.md", "README.zh-CN.md", "README.zh-TW.md")
HEADING_RE = re.compile(r"^(#{1,6})\s+", re.MULTILINE)
FENCE_RE = re.compile(r"^```", re.MULTILINE)
IMAGE_RE = re.compile(r"!\[[^\]]*\]\(")


def shape(text: str) -> dict:
    anchors = sorted({(path, symbol) for path, symbol, _start, _end, _line in docs_sources.citations(text)})
    return {
        "headings": [len(match.group(1)) for match in HEADING_RE.finditer(text)],
        "fences": len(FENCE_RE.findall(text)),
        "images": len(IMAGE_RE.findall(text)),
        "anchors": anchors,
    }


def compare(root: Path) -> list[str]:
    shapes = {}
    for name in READMES:
        path = root / name
        if not path.is_file():
            return [f"{name}: missing"]
        shapes[name] = shape(path.read_text(encoding="utf-8"))
    base = shapes[READMES[0]]
    errors = []
    for name in READMES[1:]:
        for key in ("headings", "fences", "images"):
            if shapes[name][key] != base[key]:
                errors.append(f"{name}: {key} differ from {READMES[0]} ({shapes[name][key]} vs {base[key]})")
        if shapes[name]["anchors"] != base["anchors"]:
            only_here = sorted(set(shapes[name]["anchors"]) - set(base["anchors"]))
            missing = sorted(set(base["anchors"]) - set(shapes[name]["anchors"]))
            errors.append(
                f"{name}: source anchors differ from {READMES[0]} (extra: {only_here or 'none'}, missing: {missing or 'none'})"
            )
    return errors


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-docs-parity-") as temporary:
        root = Path(temporary)
        good = "# Title\n\n## A\n\n```sh\nrun\n```\n\n![img](x.png)\n\n`Cargo.toml::rust-version`\n"
        for name in READMES:
            (root / name).write_text(good, encoding="utf-8")
        if compare(root):
            print(f"docs-parity self-test: identical files rejected ({compare(root)})", file=sys.stderr)
            return 1
        (root / READMES[2]).write_text(good + "\n## Extra\n", encoding="utf-8")
        if not compare(root):
            print("docs-parity self-test: a different heading structure was NOT caught", file=sys.stderr)
            return 1
        print("docs-parity: self-test passed (accepts matching translations, rejects a different structure)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    errors = compare(Path(args.root))
    if errors:
        for error in errors:
            print(f"docs-parity: {error}", file=sys.stderr)
        return 1
    print("docs-parity: the three READMEs have matching headings, fences, images and source anchors")
    return 0


if __name__ == "__main__":
    sys.exit(main())
