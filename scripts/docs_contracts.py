#!/usr/bin/env python3
"""Run every documentation contract behind one command.

CI, the Tier 1 table and the state snapshot all name this entry point. Freshness
and state self-checking are wired here, not left as orphan scripts.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def run(args: list[str]) -> None:
    command = [sys.executable, "-B", *args]
    print("==> " + " ".join(args), flush=True)
    result = subprocess.run(command, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(result.returncode)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="HEAD")
    args = parser.parse_args(argv)

    run(["scripts/docs_sources.py", "--self-test"])
    run(["scripts/docs_sources.py", "--check"])
    run(["scripts/docs_check.py", "--self-test"])
    run(["scripts/docs_check.py"])
    run(["scripts/markdown_check.py", "--self-test"])
    run(["scripts/markdown_check.py"])
    run(["scripts/docs_parity.py", "--self-test"])
    run(["scripts/docs_parity.py", "--check"])
    run(["scripts/state_check.py", "--self-test"])
    run(["scripts/state_check.py"])
    run(["scripts/current_state.py", "--check"])
    run(["scripts/invariant_check.py"])
    run(["scripts/coverage_check.py", "--self-test"])
    run(["scripts/coverage_check.py"])
    run(["scripts/freshness_report.py", "--check"])
    run(["scripts/docs_impact.py", "--self-test"])

    base = args.base.strip()
    if not base or set(base) == {"0"}:
        print("==> docs_impact: no base revision; range check skipped", flush=True)
    else:
        if subprocess.run(["git", "cat-file", "-e", f"{base}^{{commit}}"], cwd=ROOT).returncode != 0:
            print(f"docs-contracts: base {base} is not a commit", file=sys.stderr)
            return 1
        run(["scripts/docs_impact.py", "--base", base, "--head", args.head])
    print("docs-contracts: all documentation contracts passed", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
