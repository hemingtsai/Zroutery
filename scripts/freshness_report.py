#!/usr/bin/env python3
"""Report documents whose cited sources changed after their review baseline."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERIFIED = Path("docs/verified.json")


def isolated_git_env() -> dict[str, str]:
    """Environment for a self-test's fixture repository.

    A pre-commit hook inherits GIT_DIR, GIT_INDEX_FILE and GIT_WORK_TREE; a
    self-test that shells out to git would otherwise commit its fixtures into
    the caller's repository instead of its temporary one.
    """
    return {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}

def git(root: Path, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=root, text=True, capture_output=True, env=env)


def changed_since(root: Path, commit: str, sources: list[str], env: dict[str, str] | None = None) -> list[str]:
    result = git(root, "diff", "--name-only", commit, "--", *sources, env=env)
    if result.returncode != 0:
        return [f"git diff failed: {result.stderr.strip()}"]
    return [line for line in result.stdout.splitlines() if line]


def worktree_paths(root: Path, env: dict[str, str] | None = None) -> set[str]:
    paths = set()
    for args in (("diff", "--name-only"), ("diff", "--cached", "--name-only")):
        result = git(root, *args, env=env)
        if result.returncode == 0:
            paths.update(line for line in result.stdout.splitlines() if line)
    return paths


def commit_files(root: Path, rev: str, env: dict[str, str] | None = None) -> set[str]:
    result = git(root, "show", "--name-only", "--format=", rev, env=env)
    return {line for line in result.stdout.splitlines() if line} if result.returncode == 0 else set()


def reviewed_in_same_change(
    root: Path,
    commit: str,
    doc: str,
    source: str,
    env: dict[str, str] | None = None,
) -> bool:
    work = worktree_paths(root, env)
    if source in work and doc in work:
        return True
    result = git(root, "log", "--format=%H", f"{commit}..HEAD", "--", source, env=env)
    if result.returncode != 0 or not result.stdout.strip():
        return False
    return all(doc in commit_files(root, rev, env) for rev in result.stdout.splitlines() if rev)


def check(
    root: Path = ROOT,
    env: dict[str, str] | None = None,
) -> tuple[list[str], list[str], list[str]]:
    data = json.loads((root / VERIFIED).read_text(encoding="utf-8"))
    stale: list[str] = []
    errors: list[str] = []
    same_change: list[str] = []
    for doc, entry in data.items():
        commit = entry.get("reviewed_commit")
        if not commit:
            errors.append(f"{doc}: missing reviewed_commit")
            continue
        if git(root, "cat-file", "-e", f"{commit}^{{commit}}", env=env).returncode != 0:
            errors.append(f"{doc}: reviewed_commit {commit} is not a commit")
            continue
        sources = list(entry.get("sources", {}))
        if not sources:
            continue
        changed = changed_since(root, commit, sources, env)
        unresolved = [source for source in changed if not reviewed_in_same_change(root, commit, doc, source, env)]
        if unresolved:
            stale.append(f"{doc}: {len(unresolved)} source(s) changed after {commit}: {', '.join(unresolved)}")
        elif changed:
            same_change.append(doc)
    return errors, stale, same_change


def summary(count: int, same_change: list[str]) -> str:
    noun = "document" if count == 1 else "documents"
    if same_change:
        return f"freshness: {count} {noun}; every post-review source change is covered by a same-change document edit"
    return f"freshness: {count} {noun}; no source changed after its review commit"


def fixture(temporary: str) -> tuple[Path, dict[str, str], str]:
    root = Path(temporary)
    env = isolated_git_env()
    (root / "src").mkdir()
    (root / "docs").mkdir()
    (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
    (root / "docs" / "guide.md").write_text("value is one\n", encoding="utf-8")
    (root / "docs" / "verified.json").write_text(
        json.dumps({"docs/guide.md": {"reviewed_commit": "pending", "sources": {"src/sample.rs": "sha256:old"}}}),
        encoding="utf-8",
    )
    git(root, "init", "-q", env=env)
    git(root, "config", "user.email", "test@example.com", env=env)
    git(root, "config", "user.name", "Test", env=env)
    git(root, "add", ".", env=env)
    git(root, "commit", "-qm", "base", env=env)
    base = git(root, "rev-parse", "HEAD", env=env).stdout.strip()
    (root / "docs" / "verified.json").write_text(
        json.dumps({"docs/guide.md": {"reviewed_commit": base, "sources": {"src/sample.rs": "sha256:old"}}}),
        encoding="utf-8",
    )
    git(root, "add", "docs/verified.json", env=env)
    git(root, "commit", "-qm", "record", env=env)
    return root, env, base


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, env, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        (root / "docs" / "guide.md").write_text("value is two\n", encoding="utf-8")
        git(root, "add", ".", env=env)
        git(root, "commit", "-qm", "source and doc", env=env)
        errors, stale, same_change = check(root, env)
        if errors or stale or "docs/guide.md" not in same_change:
            print("freshness self-test: same-commit review was not accepted", file=sys.stderr)
            return 1
        if "same-change document edit" not in summary(1, same_change):
            print("freshness self-test: same-change summary is wrong", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, env, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        git(root, "add", "src/sample.rs", env=env)
        git(root, "commit", "-qm", "source only", env=env)
        errors, stale, _ = check(root, env)
        if errors or not stale:
            print("freshness self-test: source-only change was NOT stale", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, env, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        git(root, "add", "src/sample.rs", env=env)
        git(root, "commit", "-qm", "source only", env=env)
        (root / "docs" / "guide.md").write_text("value is two\n", encoding="utf-8")
        git(root, "add", "docs/guide.md", env=env)
        git(root, "commit", "-qm", "doc later", env=env)
        errors, stale, _ = check(root, env)
        if errors or not stale:
            print("freshness self-test: late doc without baseline advance was NOT stale", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, env, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        git(root, "add", "src/sample.rs", env=env)
        git(root, "commit", "-qm", "source commit", env=env)
        source = git(root, "rev-parse", "HEAD", env=env).stdout.strip()
        (root / "docs" / "verified.json").write_text(
            json.dumps({"docs/guide.md": {"reviewed_commit": source, "sources": {"src/sample.rs": "sha256:new"}}}),
            encoding="utf-8",
        )
        (root / "docs" / "guide.md").write_text("value is two\n", encoding="utf-8")
        git(root, "add", "docs/verified.json", "docs/guide.md", env=env)
        git(root, "commit", "-qm", "advance baseline and document", env=env)
        errors, stale, same_change = check(root, env)
        if errors or stale or same_change:
            print("freshness self-test: advanced baseline was not accepted", file=sys.stderr)
            return 1
        if summary(1, same_change) != "freshness: 1 document; no source changed after its review commit":
            print("freshness self-test: advanced-baseline summary is wrong", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="zr-freshness-") as temporary:
        root, env, _ = fixture(temporary)
        (root / "src" / "sample.rs").write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
        (root / "docs" / "guide.md").write_text("value is two\n", encoding="utf-8")
        errors, stale, same_change = check(root, env)
        if errors or stale or "docs/guide.md" not in same_change:
            print("freshness self-test: worktree same-change was not accepted", file=sys.stderr)
            return 1

    print("freshness self-test: same-commit and worktree accepted; source-only, late-doc and unadvanced baseline rejected")
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
    print(summary(count, same_change))
    return 0


if __name__ == "__main__":
    sys.exit(main())
