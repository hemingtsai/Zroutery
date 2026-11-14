#!/usr/bin/env python3
"""Report documents whose cited sources changed after their review baseline."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERIFIED = Path("docs/verified.json")


def git(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=root, text=True, capture_output=True)


def changed_since(root: Path, commit: str, sources: list[str]) -> list[str]:
    result = git(root, "diff", "--name-only", commit, "--", *sources)
    if result.returncode != 0:
        return [f"git diff failed: {result.stderr.strip()}"]
    return [line for line in result.stdout.splitlines() if line]


def worktree_paths(root: Path) -> set[str]:
    paths = set()
    for args in (("diff", "--name-only"), ("diff", "--cached", "--name-only")):
        result = git(root, *args)
        if result.returncode == 0:
            paths.update(line for line in result.stdout.splitlines() if line)
    return paths


def commit_files(root: Path, rev: str) -> set[str]:
    result = git(root, "show", "--name-only", "--format=", rev)
    return {line for line in result.stdout.splitlines() if line} if result.returncode == 0 else set()


def reviewed_in_same_change(root: Path, commit: str, doc: str, source: str) -> bool:
    work = worktree_paths(root)
    if source in work and doc in work:
        return True
    result = git(root, "log", "--format=%H", f"{commit}..HEAD", "--", source)
    if result.returncode != 0 or not result.stdout.strip():
        return False
    return all(doc in commit_files(root, rev) for rev in result.stdout.splitlines() if rev)


def check(root: Path = ROOT) -> tuple[list[str], list[str], list[str]]:
    data = json.loads((root / VERIFIED).read_text(encoding="utf-8"))
    stale: list[str] = []
    errors: list[str] = []
    same_change: list[str] = []
    for doc, entry in data.items():
        commit = entry.get("reviewed_commit")
        if not commit:
            errors.append(f"{doc}: missing reviewed_commit")
            continue
        if git(root, "cat-file", "-e", f"{commit}^{{commit}}").returncode != 0:
            errors.append(f"{doc}: reviewed_commit {commit} is not a commit")
            continue
        sources = list(entry.get("sources", {}))
        if not sources:
            continue
        changed = changed_since(root, commit, sources)
        unresolved = [source for source in changed if not reviewed_in_same_change(root, commit, doc, source)]
        if unresolved:
            stale.append(f"{doc}: {len(unresolved)} source(s) changed after {commit}: {', '.join(unresolved)}")
        elif changed:
            same_change.append(doc)
    return errors, stale, same_change


def fixture(temporary: str) -> tuple[Path, str]:
    root = Path(temporary)
    (root / "src").mkdir()
    (root / "docs").mkdir()
    (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
    (root / "docs" / "guide.md").write_text("value is one\n", encoding="utf-8")
    (root / "docs" / "verified.json").write_text(
        json.dumps({"docs/guide.md": {"reviewed_commit": "pending", "sources": {"src/sample.rs": "sha256:old"}}}),
        encoding="utf-8",
    )
    git(root, "init", "-q")
    git(root, "config", "user.email", "test@example.com")
    git(root, "config", "user.name", "Test")
    git(root, "add", ".")
    git(root, "commit", "-qm", "base")
    base = git(root, "rev-parse", "HEAD").stdout.strip()
    (root / "docs" / "verified.json").write_text(
        json.dumps({"docs/guide.md": {"reviewed_commit": base, "sources": {"src/sample.rs": "sha256:old"}}}),
        encoding="utf-8",
    )
    git(root, "add", "docs/verified.json")
    git(root, "commit", "-qm", "record")
    return root, base


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        (root / "docs" / "guide.md").write_text("value is two\n", encoding="utf-8")
        git(root, "add", ".")
        git(root, "commit", "-qm", "source and doc")
        errors, stale, same_change = check(root)
        if errors or stale or "docs/guide.md" not in same_change:
            print("freshness self-test: same-commit review was not accepted", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        git(root, "add", "src/sample.rs")
        git(root, "commit", "-qm", "source only")
        errors, stale, _ = check(root)
        if errors or not stale:
            print("freshness self-test: source-only change was NOT stale", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        git(root, "add", "src/sample.rs")
        git(root, "commit", "-qm", "source only")
        (root / "docs" / "guide.md").write_text("value is two\n", encoding="utf-8")
        git(root, "add", "docs/guide.md")
        git(root, "commit", "-qm", "doc later")
        errors, stale, _ = check(root)
        if errors or not stale:
            print("freshness self-test: late doc without baseline advance was NOT stale", file=sys.stderr)
            return 1

    print("freshness self-test: same-commit accepted; source-only and late-doc rejected")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    errors, stale, same_change = check()
    for error in errors:
        print(f"freshness: {error}", file=sys.stderr)
    for item in stale:
        print(f"freshness: STALE {item}")
    if errors or (args.check and stale):
        return 1
    count = len(json.loads((ROOT / VERIFIED).read_text()))
    if same_change:
        print(f"freshness: {count} documents; every post-review source change is covered by a same-change document edit")
    else:
        print(f"freshness: {count} documents; no source changed after its review commit")
    return 0


if __name__ == "__main__":
    sys.exit(main())
