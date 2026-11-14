#!/usr/bin/env python3
"""Check that every public Core module has an explicit documentation owner.

The module list is derived from `crates/zroutery-core/src/lib.rs`; the ledger is
`docs/coverage.json`. A module is owned, covered elsewhere with a named place, or
explicitly unowned with a reason. Silence is not an allowed status.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

import docs_sources

ROOT = Path(__file__).resolve().parents[1]
LIB = Path("crates/zroutery-core/src/lib.rs")
LEDGER = Path("docs/coverage.json")
MOD_RE = re.compile(r"^(?:pub )?mod ([a-z_]+);", re.MULTILINE)
STATUSES = {"owned", "covered_elsewhere", "unowned"}


def cited_by(root: Path) -> dict[str, set[str]]:
    manifest = json.loads((root / "docs" / "docs.manifest.json").read_text(encoding="utf-8"))
    by_doc: dict[str, set[str]] = {}
    for entry in manifest.get("docs", []):
        path = entry.get("path")
        if not path or entry.get("type") in {"adr", "index", "fact"}:
            continue
        text = (root / path).read_text(encoding="utf-8")
        by_doc[path] = {citation[0] for citation in docs_sources.citations(text)}
    return by_doc


def module_paths(module: str) -> tuple[str, str]:
    return (f"crates/zroutery-core/src/{module}.rs", f"crates/zroutery-core/src/{module}/")


def validate(root: Path) -> tuple[list[str], dict[str, int]]:
    lib = (root / LIB).read_text(encoding="utf-8")
    modules = MOD_RE.findall(lib)
    data = json.loads((root / LEDGER).read_text(encoding="utf-8"))
    errors: list[str] = []
    if data.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    if data.get("derived_from") != str(LIB):
        errors.append(f"derived_from must be {LIB}")
    entries = data.get("modules", [])
    citations = cited_by(root)
    seen: dict[str, dict] = {}
    counts = {status: 0 for status in STATUSES}
    for index, entry in enumerate(entries):
        name = entry.get("module")
        if name in seen:
            errors.append(f"module `{name}` is listed twice")
        seen[name] = entry
        status = entry.get("status")
        if status not in STATUSES:
            errors.append(f"module `{name}`: status {status!r} is not {sorted(STATUSES)}")
            continue
        counts[status] += 1
        if status == "owned":
            owner = entry.get("owner")
            if not owner or not (root / owner).is_file():
                errors.append(f"module `{name}`: owner `{owner}` does not exist")
                continue
            file_path, dir_path = module_paths(name)
            if not any(path == file_path or path.startswith(dir_path) for path in citations.get(owner, set())):
                errors.append(f"module `{name}`: owner `{owner}` does not cite {file_path} or {dir_path}")
        elif status == "covered_elsewhere":
            if not entry.get("where"):
                errors.append(f"module `{name}`: covered_elsewhere needs a `where`")
        elif status == "unowned":
            if not entry.get("reason"):
                errors.append(f"module `{name}`: unowned needs a `reason`")
    missing = sorted(set(modules) - set(seen))
    extra = sorted(set(seen) - set(modules))
    for name in missing:
        errors.append(f"module `{name}` has no coverage entry")
    for name in extra:
        errors.append(f"module `{name}` is not a public module in {LIB}")
    return errors, counts


def self_test() -> int:
    lib = "pub mod alpha;\npub mod beta;\n"
    good = {
        "schema_version": 1,
        "derived_from": "crates/zroutery-core/src/lib.rs",
        "modules": [
            {"module": "alpha", "status": "owned", "owner": "README.md"},
            {"module": "beta", "status": "covered_elsewhere", "where": "internal"},
        ],
    }
    if validate_text(lib, good):
        print(f"coverage-check self-test: good ledger rejected ({validate_text(lib, good)})", file=sys.stderr)
        return 1
    bad = json.loads(json.dumps(good))
    bad["modules"].pop()
    if not validate_text(lib, bad):
        print("coverage-check self-test: missing module was NOT caught", file=sys.stderr)
        return 1
    print("coverage-check: self-test passed (accepts a complete ledger, rejects a missing module)")
    return 0


def validate_text(lib: str, data: dict) -> list[str]:
    errors: list[str] = []
    modules = MOD_RE.findall(lib)
    seen = set()
    for entry in data.get("modules", []):
        seen.add(entry.get("module"))
    for name in sorted(set(modules) - seen):
        errors.append(f"module `{name}` has no coverage entry")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    errors, counts = validate(ROOT)
    if errors:
        for error in errors:
            print(f"coverage-check: {error}", file=sys.stderr)
        return 1
    print(
        f"coverage-check: {sum(counts.values())} top-level modules "
        f"({counts['owned']} owned, {counts['covered_elsewhere']} covered elsewhere, {counts['unowned']} unowned)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
