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
    result = git("diff", "--name-only", f"{commit}..HEAD", "--", *sources)
    if result.returncode != 0:
        return [f"git diff failed: {result.stderr.strip()}"]
    return [line for line in result.stdout.splitlines() if line]


def check() -> tuple[list[str], list[str]]:
    data = json.loads((ROOT / VERIFIED).read_text(encoding="utf-8"))
    stale: list[str] = []
    errors: list[str] = []
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
        if changed:
            stale.append(f"{doc}: {len(changed)} source(s) changed after {commit}: {', '.join(changed)}")
    return errors, stale


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    errors, stale = check()
    for error in errors:
        print(f"freshness: {error}", file=sys.stderr)
    for item in stale:
        print(f"freshness: STALE {item}")
    if errors or (args.check and stale):
        return 1
    print(f"freshness: {len(json.loads((ROOT / VERIFIED).read_text()) )} documents; no source changed after its review commit")
    return 0


if __name__ == "__main__":
    sys.exit(main())
