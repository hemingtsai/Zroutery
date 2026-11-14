#!/usr/bin/env python3
"""Create the next ADR skeleton."""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import docs_sources
DECISIONS = ROOT / "docs" / "development" / "decisions"


def next_number() -> int:
    numbers = [int(match.group(1)) for path in DECISIONS.glob("[0-9][0-9][0-9][0-9]-*.md") if (match := re.match(r"(\d{4})-", path.name))]
    return max(numbers, default=0) + 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--title", required=True)
    parser.add_argument("--status", default="proposed", choices=["accepted", "proposed"])
    parser.add_argument("--supersedes", default="none")
    parser.add_argument("--source", action="append", required=True, help="existing path or path::symbol")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    if args.supersedes != "none" and not re.match(r"^ADR-\d{4}$", args.supersedes):
        print("new-adr: --supersedes must be none or ADR-XXXX", file=sys.stderr)
        return 2
    for source in args.source:
        if "::" in source:
            path, symbol = source.rsplit("::", 1)
            problem = docs_sources.check_citation(ROOT, path, symbol, None, None)
            if problem:
                print(f"new-adr: source `{source}`: {problem}", file=sys.stderr)
                return 2
        elif not (ROOT / source).exists():
            print(f"new-adr: source `{source}` does not exist", file=sys.stderr)
            return 2
    number = next_number()
    slug = re.sub(r"[^a-z0-9]+", "-", args.title.lower()).strip("-") or "decision"
    path = DECISIONS / f"{number:04d}-{slug}.md"
    sources = "\n".join(f"- `{source}`" for source in args.source)
    body = f"""# ADR-{number:04d}: {args.title}

Status: {args.status}
Date: {dt.date.today().isoformat()}
Supersedes: {args.supersedes}

## Context

## Decision

## Consequences

## Sources

{sources}
"""
    if args.dry_run:
        print(path)
        print(body)
        return 0
    if path.exists():
        print(f"new-adr: {path} already exists", file=sys.stderr)
        return 1
    path.write_text(body, encoding="utf-8")
    print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
