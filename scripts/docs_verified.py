#!/usr/bin/env python3
"""Derive and verify the source-hash ledger in docs/verified.json."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import docs_sources

ROOT = Path(__file__).resolve().parents[1]
VERIFIED = Path("docs/verified.json")


def source_hash(root: Path, source: str) -> str | None:
    path = root / source
    if not path.is_file():
        return None
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def expected(root: Path) -> dict[str, dict[str, str]]:
    result: dict[str, dict[str, str]] = {}
    for doc in docs_sources.active_docs(root):
        try:
            text = doc.read_text(encoding="utf-8")
        except OSError:
            continue
        sources = sorted({path for path, _symbol, _start, _end, _line in docs_sources.citations(text)})
        sources = [source for source in sources if (root / source).is_file()]
        if not sources:
            continue
        relative = doc.relative_to(root).as_posix()
        result[relative] = {source: source_hash(root, source) or "" for source in sources}
    return result


def write(root: Path, today: str | None = None) -> int:
    path = root / VERIFIED
    data = json.loads(path.read_text(encoding="utf-8")) if path.is_file() else {}
    wanted = expected(root)
    for doc, sources in wanted.items():
        entry = data.setdefault(doc, {})
        entry["sources"] = sources
        entry["verified_at"] = today or dt.date.today().isoformat()
    for doc in list(data):
        if doc not in wanted:
            del data[doc]
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"docs-verified: wrote {len(wanted)} document(s), {sum(len(v) for v in wanted.values())} source(s)")
    return 0


def check(root: Path) -> list[str]:
    path = root / VERIFIED
    if not path.is_file():
        return ["docs/verified.json is missing"]
    data = json.loads(path.read_text(encoding="utf-8"))
    wanted = expected(root)
    errors: list[str] = []
    for doc, sources in wanted.items():
        entry = data.get(doc)
        if not entry:
            errors.append(f"{doc}: missing docs/verified.json entry")
            continue
        recorded = entry.get("sources", {})
        for source, digest in sources.items():
            if source not in recorded:
                errors.append(f"{doc}: cited source `{source}` is not in the ledger")
            elif recorded[source] != digest:
                errors.append(f"{doc}: source `{source}` hash is stale")
        for source in recorded:
            if source not in sources:
                errors.append(f"{doc}: ledger source `{source}` is no longer cited")
    for doc in data:
        if doc not in wanted:
            errors.append(f"{doc}: ledger entry has no active citations")
    return errors


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-verified-") as temporary:
        root = Path(temporary)
        (root / "docs").mkdir()
        (root / "src").mkdir()
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
        (root / "docs" / "docs.manifest.json").write_text(
            json.dumps({"docs": [{"path": "docs/guide.md"}]}), encoding="utf-8"
        )
        (root / "docs" / "guide.md").write_text("The value is one (`src/sample.rs::value`).\n", encoding="utf-8")
        if not check(root):
            print("docs-verified self-test: missing ledger was NOT caught", file=sys.stderr)
            return 1
        write(root, "2026-11-14")
        errors = check(root)
        if errors:
            print(f"docs-verified self-test: written ledger rejected ({errors})", file=sys.stderr)
            return 1
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        if not any("stale" in error for error in check(root)):
            print("docs-verified self-test: changed source was NOT stale", file=sys.stderr)
            return 1
        write(root, "2026-11-14")
        ledger = json.loads((root / "docs" / "verified.json").read_text(encoding="utf-8"))
        ledger["docs/guide.md"]["sources"]["src/extra.rs"] = "sha256:extra"
        (root / "docs" / "verified.json").write_text(json.dumps(ledger), encoding="utf-8")
        if not any("no longer cited" in error for error in check(root)):
            print("docs-verified self-test: extra ledger source was NOT caught", file=sys.stderr)
            return 1
        print("docs-verified: self-test passed (writes cited hashes, rejects stale and extra sources)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=str(ROOT))
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    root = Path(args.root)
    if args.write:
        return write(root)
    errors = check(root)
    if errors:
        for error in errors:
            print(f"docs-verified: {error}", file=sys.stderr)
        print(f"docs-verified: {len(errors)} ledger error(s); run --write after re-reading the sources", file=sys.stderr)
        return 1
    print(f"docs-verified: ledger covers {len(expected(root))} document(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
