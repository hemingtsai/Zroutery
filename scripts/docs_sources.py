#!/usr/bin/env python3
"""Resolve source anchors written in documentation.

A factual sentence in an active normative/reference document cites the symbol
that defines it, for example:

    `crates/zroutery-core/src/config/server.rs::ServerConfig::max_body_bytes`

This script is the mechanical half of the rule: it proves the cited file and
symbol still exist, and it builds the reverse index used by `docs_impact.py`.
It cannot prove that the sentence interprets the symbol correctly; a second
reader does that.

Usage:
    python3 -B scripts/docs_sources.py --check
    python3 -B scripts/docs_sources.py --index
    python3 -B scripts/docs_sources.py --self-test
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

# A code span that looks like a source anchor: a path, one or more `::`
# segments, and an optional line range.
ANCHOR_RE = re.compile(
    r"`([A-Za-z0-9_./-]+(?:::[A-Za-z0-9_.-]+)+)(?:#L(\d+)(?:-L(\d+))?)?`"
)
# A plain file plus line range, for non-symbol anchors.
LINE_RE = re.compile(r"`([A-Za-z0-9_./-]+\.(?:rs|toml|json|yml|yaml|py|md|ts|tsx|css))#L(\d+)(?:-L(\d+))?`")
SKIP_PREFIXES = ("http://", "https://", "mailto:")
PLACEHOLDER_PATHS = {"path", "file", "src"}


def load_manifest(root: Path) -> dict:
    path = root / MANIFEST
    return json.loads(path.read_text(encoding="utf-8"))


def active_docs(root: Path) -> list[Path]:
    manifest = load_manifest(root)
    docs = []
    for entry in manifest.get("docs", []):
        if entry.get("generated") or entry.get("type") == "adr":
            continue
        path = entry.get("path")
        if path:
            docs.append(root / path)
    return docs


def citations(text: str) -> list[tuple[str, str | None, int | None, int | None, int]]:
    """Return (path, symbol, start, end, line_number) for every anchor."""
    found: list[tuple[str, str | None, int | None, int | None, int]] = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        if any(prefix in line for prefix in SKIP_PREFIXES):
            continue
        for match in ANCHOR_RE.finditer(line):
            anchor, start, end = match.group(1), match.group(2), match.group(3)
            path, _, symbol = anchor.partition("::")
            if path in PLACEHOLDER_PATHS or "<" in anchor or symbol in {"symbol", "name"}:
                continue
            found.append((path, symbol, _int(start), _int(end), line_number))
        for match in LINE_RE.finditer(line):
            path, start, end = match.group(1), match.group(2), match.group(3)
            if path in PLACEHOLDER_PATHS:
                continue
            found.append((path, None, _int(start), _int(end), line_number))
    return found


def _int(value: str | None) -> int | None:
    return int(value) if value is not None else None


def check_citation(root: Path, path: str, symbol: str | None, start: int | None, end: int | None) -> str | None:
    target = root / path
    if not target.is_file():
        return f"missing file `{path}`"
    try:
        text = target.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        return f"cannot read `{path}`: {error}"
    lines = text.count("\n") + 1
    if start is not None:
        if start < 1 or start > lines:
            return f"`{path}` has {lines} lines, citation points at line {start}"
        if end is not None and (end < start or end > lines):
            return f"`{path}` line range {start}-{end} is outside 1-{lines}"
    if symbol:
        for segment in symbol.split("::"):
            if segment not in text:
                return f"symbol `{segment}` not found in `{path}`"
    return None


def check_document(root: Path, doc: Path) -> list[str]:
    try:
        text = doc.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        return [f"{doc}: cannot read: {error}"]
    errors = []
    for path, symbol, start, end, line_number in citations(text):
        problem = check_citation(root, path, symbol, start, end)
        if problem:
            try:
                relative = doc.relative_to(root)
            except ValueError:
                relative = doc
            errors.append(f"{relative}:{line_number}: {problem}")
    return errors


def reverse_index(root: Path) -> dict[str, list[str]]:
    index: dict[str, list[str]] = {}
    for doc in active_docs(root):
        try:
            text = doc.read_text(encoding="utf-8")
        except OSError:
            continue
        try:
            relative = str(doc.relative_to(root))
        except ValueError:
            relative = str(doc)
        seen = set()
        for path, _symbol, _start, _end, _line in citations(text):
            if path in seen:
                continue
            seen.add(path)
            index.setdefault(path, []).append(relative)
    return {key: sorted(value) for key, value in sorted(index.items())}


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-docs-sources-") as temporary:
        root = Path(temporary)
        (root / "crates").mkdir()
        source = root / "crates" / "sample.rs"
        source.write_text("pub struct Sample {}\nimpl Sample { pub fn value(&self) -> u8 { 1 } }\n", encoding="utf-8")
        (root / "docs").mkdir()
        (root / "docs" / "docs.manifest.json").write_text(
            json.dumps({"docs": [{"path": "docs/good.md"}, {"path": "docs/bad.md"}]}), encoding="utf-8"
        )
        (root / "docs" / "good.md").write_text(
            "`crates/sample.rs::Sample::value`\n", encoding="utf-8"
        )
        (root / "docs" / "bad.md").write_text(
            "`crates/sample.rs::Sample::missing`\n", encoding="utf-8"
        )
        problems = []
        problems += check_document(root, root / "docs" / "good.md")
        problems += check_document(root, root / "docs" / "bad.md")
        if len(problems) != 1 or "missing" not in problems[0]:
            print(f"docs-sources self-test: FAILED ({problems})", file=sys.stderr)
            return 1
        index = reverse_index(root)
        if index.get("crates/sample.rs") != ["docs/bad.md", "docs/good.md"]:
            print(f"docs-sources self-test: index mismatch ({index})", file=sys.stderr)
            return 1
        print("docs-sources: self-test passed (resolves anchors, rejects missing symbols, builds the reverse index)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--index", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    root = Path(args.root)
    if args.index:
        print(json.dumps(reverse_index(root), indent=2, sort_keys=True))
        return 0
    docs = active_docs(root)
    errors = []
    anchors = 0
    for doc in docs:
        try:
            text = doc.read_text(encoding="utf-8")
        except OSError as error:
            errors.append(f"{doc}: cannot read: {error}")
            continue
        anchors += len(citations(text))
        errors.extend(check_document(root, doc))
    if errors:
        for error in errors:
            print(f"docs-sources: {error}", file=sys.stderr)
        print(f"docs-sources: {len(errors)} unresolved anchor(s) in {len(docs)} document(s)", file=sys.stderr)
        return 1
    print(f"docs-sources: {anchors} anchor(s) resolved in {len(docs)} active document(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
