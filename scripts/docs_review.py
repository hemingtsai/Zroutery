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
# A list item carries facts as often as a paragraph does, and the sample is the
# only mechanical coverage the review gate gets; skipping bullets meant a rewrite
# inside a bullet list was never sampled.
BULLET_RE = re.compile(r"^(?:[-*+]|\d+[.)])\s+")


def document_sha(root: Path, doc: str) -> str:
    """The content a review covered, so an edit after it is visible.

    Line endings are normalized to the POSIX form git stores, so a checkout
    on any host hashes to the value any other host recorded.
    """
    raw = (root / doc).read_bytes().replace(b"\r\n", b"\n")
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def sentences(root: Path, doc: str) -> list[str]:
    text = (root / doc).read_text(encoding="utf-8")
    found: list[str] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("#", ">", "|", "```")):
            continue
        stripped = BULLET_RE.sub("", stripped)
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
        reviewed_sha = entry.get("reviewed_sha")
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
        if not reviewed_sha:
            errors.append(f"{doc}: missing reviewed_sha; record the review against the document content")
        elif reviewed_sha != document_sha(root, doc):
            errors.append(f"{doc}: document changed since its review; record the review again")
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


def record(root: Path, reviewer: str, docs: list[str] | None = None, note: str | None = None) -> int:
    path = root / VERIFIED
    data = json.loads(path.read_text(encoding="utf-8"))
    targets = list(data) if docs is None else docs
    unknown = [doc for doc in targets if doc not in data]
    if unknown:
        print(f"docs-review: no ledger entry for {', '.join(unknown)}", file=sys.stderr)
        return 1
    for doc in targets:
        data[doc]["reviewed_by"] = reviewer
        data[doc]["reviewed_at"] = dt.date.today().isoformat()
        if note:
            data[doc]["reviewed_note"] = note
        data[doc]["reviewed_sha"] = document_sha(root, doc)
        data[doc]["sample"] = sample(root, doc)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"docs-review: recorded {reviewer} on {len(targets)} document(s)")
    return 0


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-review-") as temporary:
        root = Path(temporary)
        (root / "docs").mkdir()
        (root / "docs" / "guide.md").write_text(
            "The value is one (`src/sample.rs::value`).\n"
            "The limit is 32 (`src/sample.rs::limit`).\n"
            "Retries are zero (`src/sample.rs::retries`).\n"
            "- The retry budget is six (`src/sample.rs::retries`).\n",
            encoding="utf-8",
        )
        (root / "docs" / "verified.json").write_text(json.dumps({"docs/guide.md": {}}), encoding="utf-8")
        if not any("retry budget is six" in sentence for sentence in sentences(root, "docs/guide.md")):
            print("docs-review self-test: a bullet sentence was NOT sampled", file=sys.stderr)
            return 1
        (root / "docs" / "bullets.md").write_text(
            "# Bullets\n\n"
            "- The retry budget is six (`src/sample.rs::retries`).\n"
            "- The cache budget is two (`src/sample.rs::limit`).\n"
            "- The token budget is one (`src/sample.rs::value`).\n",
            encoding="utf-8",
        )
        bullet_sample = sample(root, "docs/bullets.md")
        if len(bullet_sample) != 3 or not all("budget is" in sentence for sentence in bullet_sample):
            print(f"docs-review self-test: bullets did NOT reach the sample ({bullet_sample})", file=sys.stderr)
            return 1
        record(root, "second-reader", note="model=fixture; session=selftest")
        recorded = json.loads((root / "docs" / "verified.json").read_text(encoding="utf-8"))
        if recorded["docs/guide.md"].get("reviewed_note") != "model=fixture; session=selftest":
            print("docs-review self-test: the review note was NOT recorded", file=sys.stderr)
            return 1
        if check(root):
            print(f"docs-review self-test: recorded evidence rejected ({check(root)})", file=sys.stderr)
            return 1
        with open(root / "docs" / "guide.md", "a", encoding="utf-8") as handle:
            handle.write("Yes is blocked.\n")
        if not any("changed since" in error for error in check(root)):
            print("docs-review self-test: a document edited after review was NOT caught", file=sys.stderr)
            return 1
        record(root, "second-reader")
        if check(root):
            print(f"docs-review self-test: re-recorded evidence rejected ({check(root)})", file=sys.stderr)
            return 1
        data = json.loads((root / "docs" / "verified.json").read_text(encoding="utf-8"))
        data["docs/guide.md"]["sample"] = ["not in the document one", "not in the document two", "not in the document three"]
        (root / "docs" / "verified.json").write_text(json.dumps(data), encoding="utf-8")
        if not any("no longer" in error for error in check(root)):
            print("docs-review self-test: missing sample was NOT caught", file=sys.stderr)
            return 1
        print("docs-review: self-test passed (records samples, rejects a missing sample and a document edited after review)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--plan", action="store_true")
    parser.add_argument("--record", metavar="REVIEWER")
    parser.add_argument("--doc", action="append", default=None, help="document path to record; repeatable")
    parser.add_argument("--note", default=None, help="reviewer agent, model and session, recorded with the review")
    parser.add_argument("--all", action="store_true", help="record every verified document (migration only)")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", default=str(ROOT))
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    root = Path(args.root)
    if args.record:
        if not args.note:
            print(
                'docs-review: --record needs --note "model=<model>; session=<session>" '
                "so the review says which session performed it",
                file=sys.stderr,
            )
            return 2
        if args.doc:
            return record(root, args.record, docs=args.doc, note=args.note)
        if args.all:
            return record(root, args.record, note=args.note)
        print("docs-review: --record needs --doc <path> (repeatable), or --all for a migration", file=sys.stderr)
        return 2
    if args.plan:
        print(json.dumps(plan(root), indent=2, sort_keys=True))
        return 0
    errors = check(root)
    if errors:
        for error in errors:
            print(f"docs-review: {error}", file=sys.stderr)
        print("fix: run `python3 -B scripts/docs_review.py --record <reviewer> --doc <path>` after reading the document", file=sys.stderr)
        return 1
    print(f"docs-review: {len(json.loads((root / VERIFIED).read_text(encoding='utf-8')))} document(s) have recorded review evidence")
    return 0


if __name__ == "__main__":
    sys.exit(main())
