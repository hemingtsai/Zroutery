#!/usr/bin/env python3
"""Fail when a change touches a source that a document cites, without changing it.

The affected set is derived from the documents' own source anchors plus the
manifest triggers. There is no `Docs-Impact: none` escape: if a trigger is
wrong, the manifest or the citation is fixed in the same change.

Usage:
    python3 -B scripts/docs_impact.py --base <rev> --head HEAD
    python3 -B scripts/docs_impact.py --base <rev> --head HEAD --json
    python3 -B scripts/docs_impact.py --self-test
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import subprocess
import sys
import tempfile
from pathlib import Path

import docs_sources

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = Path("docs/docs.manifest.json")


def git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, check=False)


def changed_files(root: Path, base: str, head: str) -> list[str]:
    result = git(root, "diff", "--name-only", f"{base}..{head}")
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "git diff failed")
    return [line for line in result.stdout.splitlines() if line.strip()]


def load_manifest(root: Path) -> dict:
    return json.loads((root / MANIFEST).read_text(encoding="utf-8"))


def evaluate(root: Path, base: str, head: str) -> dict:
    manifest = load_manifest(root)
    changed = changed_files(root, base, head)
    changed_set = set(changed)
    required: dict[str, str] = {}
    reverse = docs_sources.reverse_index(root)
    for path in changed:
        for doc in reverse.get(path, []):
            required.setdefault(doc, path)
    for pattern, docs in manifest.get("triggers", {}).items():
        if any(fnmatch.fnmatch(path, pattern) for path in changed):
            for doc in docs:
                required.setdefault(doc, f"trigger {pattern}")
    generated = {
        entry["path"]
        for entry in manifest.get("docs", [])
        if entry.get("generated") and "path" in entry
    }
    required = {doc: why for doc, why in required.items() if doc not in generated}
    missing = sorted(doc for doc in required if doc not in changed_set)
    return {
        "base": base,
        "head": head,
        "changed": changed,
        "required": dict(sorted(required.items())),
        "missing": missing,
    }


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-docs-impact-") as temporary:
        root = Path(temporary)
        git(root, "init", "-q")
        git(root, "config", "user.email", "test@example.com")
        git(root, "config", "user.name", "Test")
        (root / "src").mkdir()
        (root / "docs").mkdir()
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
        (root / "docs" / "docs.manifest.json").write_text(
            json.dumps({"docs": [{"path": "docs/rule.md"}], "pending": [], "ignore": [], "triggers": {}}),
            encoding="utf-8",
        )
        (root / "docs" / "rule.md").write_text(
            "> Authority: normative\n\nThe value is one (`src/sample.rs::value`).\n", encoding="utf-8"
        )
        git(root, "add", ".")
        git(root, "commit", "-qm", "base")
        base = git(root, "rev-parse", "HEAD").stdout.strip()
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        git(root, "add", ".")
        git(root, "commit", "-qm", "code only")
        head = git(root, "rev-parse", "HEAD").stdout.strip()
        result = evaluate(root, base, head)
        if result["missing"] != ["docs/rule.md"]:
            print(f"docs-impact self-test: expected the cited document to be required, got {result}", file=sys.stderr)
            return 1
        (root / "docs" / "rule.md").write_text(
            "> Authority: normative\n\nThe value is two (`src/sample.rs::value`).\n", encoding="utf-8"
        )
        git(root, "add", ".")
        git(root, "commit", "-qm", "doc updated")
        head = git(root, "rev-parse", "HEAD").stdout.strip()
        result = evaluate(root, base, head)
        if result["missing"]:
            print(f"docs-impact self-test: updated document still reported missing: {result}", file=sys.stderr)
            return 1
        print("docs-impact: self-test passed (a cited source requires its document; updating it clears the gate)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    if not args.base:
        print("docs-impact: --base is required", file=sys.stderr)
        return 2
    if set(args.base) == {"0"}:
        print("docs-impact: first push has no base; no range to check")
        return 0
    root = Path(args.root)
    try:
        result = evaluate(root, args.base, args.head)
    except (OSError, json.JSONDecodeError, RuntimeError) as error:
        print(f"docs-impact: cannot evaluate {args.base}..{args.head}: {error}", file=sys.stderr)
        return 3
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        for path in result["changed"]:
            print(f"changed: {path}")
        for doc, why in result["required"].items():
            marker = "missing" if doc in result["missing"] else "updated"
            print(f"{marker}: {doc} (required by {why})")
    if result["missing"]:
        for doc in result["missing"]:
            print(
                f"::error file={doc}::documentation is affected by this change but was not updated; read the cited source and update the document",
                file=sys.stderr,
            )
        print(f"docs-impact: {len(result['missing'])} document(s) must change", file=sys.stderr)
        return 1
    print("docs-impact: every affected document changed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
