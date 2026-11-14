#!/usr/bin/env python3
"""Record deterministic second-reader evidence for verified documents."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERIFIED = Path("docs/verified.json")
SENTENCE_RE = re.compile(r"(?<=[.!?])\s+")


def sentences(root: Path, doc: str) -> list[str]:
    text = (root / doc).read_text(encoding="utf-8")
    found: list[str] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("#", ">", "|", "```", "-", "*")):
            continue
        for sentence in SENTENCE_RE.split(stripped):
            sentence = sentence.strip()
            if len(sentence) >= 40 and ("`" in sentence or re.search(r"\d", sentence)):
                found.append(sentence)
    return found


def sample(root: Path, doc: str, count: int = 3) -> list[str]:
    candidates = sentences(root, doc)
    ranked = sorted(candidates, key=lambda sentence: hashlib.sha256(f"{doc}\n{sentence}".encode()).hexdigest())
    return ranked[: min(5, max(3, count))]


def author(root: Path, doc: str) -> str:
    result = subprocess.run(
        ["git", "log", "-1", "--format=%an", "--", doc], cwd=root, text=True, capture_output=True
    )
    return result.stdout.strip() if result.returncode == 0 else ""


def plan(root: Path = ROOT) -> dict[str, list[str]]:
    data = json.loads((root / VERIFIED).read_text(encoding="utf-8"))
    return {doc: sample(root, doc) for doc in sorted(data)}


def check(root: Path = ROOT) -> list[str]:
    data = json.loads((root / VERIFIED).read_text(encoding="utf-8"))
    errors: list[str] = []
    for doc, entry in data.items():
        reviewer = entry.get("reviewed_by")
        reviewed_at = entry.get("reviewed_at")
        samples = entry.get("sample")
        if not reviewer:
            errors.append(f"{doc}: missing reviewed_by")
        if not reviewed_at:
            errors.append(f"{doc}: missing reviewed_at")
        else:
            try:
                dt.date.fromisoformat(reviewed_at)
            except (TypeError, ValueError):
                errors.append(f"{doc}: reviewed_at is not YYYY-MM-DD")
        if not isinstance(samples, list) or not 3 <= len(samples) <= 5:
            errors.append(f"{doc}: sample must contain 3-5 sentences")
            continue
        text = (root / doc).read_text(encoding="utf-8")
        for sentence in samples:
            if sentence not in text:
                errors.append(f"{doc}: sample sentence is no longer in the document")
        last_author = author(root, doc)
        if reviewer and last_author and reviewer == last_author:
            errors.append(f"{doc}: reviewed_by equals the last author")
    return errors


def record(root: Path, reviewer: str) -> int:
    path = root / VERIFIED
    data = json.loads(path.read_text(encoding="utf-8"))
    for doc in data:
        data[doc]["reviewed_by"] = reviewer
        data[doc]["reviewed_at"] = dt.date.today().isoformat()
        data[doc]["sample"] = sample(root, doc)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"docs-review: recorded {reviewer} on {len(data)} document(s)")
    return 0


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-review-") as temporary:
        root = Path(temporary)
        (root / "docs").mkdir()
        (root / "docs" / "guide.md").write_text(
            "The value is one (`src/sample.rs::value`).\nThe limit is 32 (`src/sample.rs::limit`).\nRetries are zero (`src/sample.rs::retries`).\n",
            encoding="utf-8",
        )
        (root / "docs" / "verified.json").write_text(json.dumps({"docs/guide.md": {}}), encoding="utf-8")
        record(root, "second-reader")
        if check(root):
            print(f"docs-review self-test: recorded evidence rejected ({check(root)})", file=sys.stderr)
            return 1
        data = json.loads((root / "docs" / "verified.json").read_text(encoding="utf-8"))
        data["docs/guide.md"]["sample"] = ["not in the document one", "not in the document two", "not in the document three"]
        (root / "docs" / "verified.json").write_text(json.dumps(data), encoding="utf-8")
        if not any("no longer" in error for error in check(root)):
            print("docs-review self-test: missing sample was NOT caught", file=sys.stderr)
            return 1
        print("docs-review: self-test passed (records samples, rejects a missing sample)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--plan", action="store_true")
    parser.add_argument("--record", metavar="REVIEWER")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=str(ROOT))
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    root = Path(args.root)
    if args.record:
        return record(root, args.record)
    if args.plan:
        print(json.dumps(plan(root), indent=2, sort_keys=True))
        return 0
    errors = check(root)
    if errors:
        for error in errors:
            print(f"docs-review: {error}", file=sys.stderr)
        print("fix: run `python3 -B scripts/docs_review.py --record second-reader` after sampling", file=sys.stderr)
        return 1
    print(f"docs-review: {len(json.loads((root / VERIFIED).read_text(encoding='utf-8')))} document(s) have recorded review evidence")
    return 0


if __name__ == "__main__":
    sys.exit(main())
