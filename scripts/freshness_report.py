#!/usr/bin/env python3
"""Report documents whose cited sources changed after their review commit."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERIFIED = Path("docs/verified.json")


def git(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=ROOT, text=True, capture_output=True)


def changed_since(commit: str, sources: list[str]) -> list[str]:
    result = git("diff", "--name-only", commit, "--", *sources)
    if result.returncode != 0:
        return [f"git diff failed: {result.stderr.strip()}"]
    return [line for line in result.stdout.splitlines() if line]


def worktree_paths() -> set[str]:
    paths = set()
    for args in (("diff", "--name-only"), ("diff", "--cached", "--name-only")):
        result = git(*args)
        if result.returncode == 0:
            paths.update(line for line in result.stdout.splitlines() if line)
    return paths


def commit_files(rev: str) -> set[str]:
    result = git("show", "--name-only", "--format=", rev)
    return {line for line in result.stdout.splitlines() if line} if result.returncode == 0 else set()


def reviewed_in_same_change(commit: str, doc: str, source: str) -> bool:
    work = worktree_paths()
    if source in work and doc in work:
        return True
    result = git("log", "--format=%H", f"{commit}..HEAD", "--", source)
    if result.returncode != 0 or not result.stdout.strip():
        return False
    return all(doc in commit_files(rev) for rev in result.stdout.splitlines() if rev)


def check() -> tuple[list[str], list[str], list[str]]:
    data = json.loads((ROOT / VERIFIED).read_text(encoding="utf-8"))
    stale: list[str] = []
    errors: list[str] = []
    same_change: list[str] = []
    for doc, entry in data.items():
        commit = entry.get("reviewed_commit")
        if not commit:
            errors.append(f"{doc}: missing reviewed_commit")
            continue
        if git("cat-file", "-e", f"{commit}^{{commit}}").returncode != 0:
            errors.append(f"{doc}: reviewed_commit {commit} is not a commit")
            continue
        sources = list(entry.get("sources", {}))
        if not sources:
            continue
        changed = changed_since(commit, sources)
        unresolved = [source for source in changed if not reviewed_in_same_change(commit, doc, source)]
        if unresolved:
            stale.append(f"{doc}: {len(unresolved)} source(s) changed after {commit}: {', '.join(unresolved)}")
        elif changed:
            same_change.append(doc)
    return errors, stale, same_change


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
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
