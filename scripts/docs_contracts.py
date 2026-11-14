#!/usr/bin/env python3
"""Run every documentation contract behind one command.

`STEPS` is the single execution list. CI, the Tier 1 hook, the rule registry and
`record_state.py` all read it; a new gate must be added here and registered in
`docs/rules.json`.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TAIL = 2000

STEPS = [
    {"name": "docs_sources self-test", "gate": None, "args": ["scripts/docs_sources.py", "--self-test"]},
    {"name": "documentation sources", "gate": "documentation sources", "args": ["scripts/docs_sources.py", "--check"]},
    {"name": "docs_check self-test", "gate": None, "args": ["scripts/docs_check.py", "--self-test"]},
    {"name": "documentation check", "gate": "documentation check", "args": ["scripts/docs_check.py"]},
    {"name": "markdown self-test", "gate": None, "args": ["scripts/markdown_check.py", "--self-test"]},
    {"name": "markdown structure", "gate": "markdown structure", "args": ["scripts/markdown_check.py"]},
    {"name": "docs_parity self-test", "gate": None, "args": ["scripts/docs_parity.py", "--self-test"]},
    {"name": "documentation parity", "gate": "documentation parity", "args": ["scripts/docs_parity.py", "--check"]},
    {"name": "docs_rules self-test", "gate": None, "args": ["scripts/docs_rules.py", "--self-test"]},
    {"name": "rules registry", "gate": "rules registry", "args": ["scripts/docs_rules.py"]},
    {"name": "state_check self-test", "gate": None, "args": ["scripts/state_check.py", "--self-test"]},
    {"name": "state schema", "gate": "state schema", "args": ["scripts/state_check.py"]},
    {"name": "rendered state", "gate": "rendered state", "args": ["scripts/current_state.py", "--check"]},
    {"name": "invariant evidence", "gate": "invariant evidence", "args": ["scripts/invariant_check.py"]},
    {"name": "coverage self-test", "gate": None, "args": ["scripts/coverage_check.py", "--self-test"]},
    {"name": "module coverage", "gate": "module coverage", "args": ["scripts/coverage_check.py"]},
    {"name": "freshness", "gate": "freshness", "args": ["scripts/freshness_report.py", "--check"]},
    {"name": "docs_impact self-test", "gate": None, "args": ["scripts/docs_impact.py", "--self-test"]},
]


def run_step(step: dict, json_mode: bool = False) -> dict:
    command = [sys.executable, "-B", *step["args"]]
    if not json_mode:
        print("==> " + " ".join(step["args"]), flush=True)
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
    if not json_mode and result.stdout:
        print(result.stdout, end="", flush=True)
    if not json_mode and result.stderr:
        print(result.stderr, end="", file=sys.stderr, flush=True)
    return {
        "name": step["name"],
        "gate": step["gate"],
        "status": "pass" if result.returncode == 0 else "fail",
        "code": result.returncode,
        "command": "python3 -B " + " ".join(step["args"]),
        "output": (result.stdout + result.stderr)[-TAIL:],
    }


def resolve_base(value: str | None, no_range: bool, head: str) -> tuple[str | None, str]:
    if no_range:
        return None, "UNVERIFIED (range check disabled with --no-range)"
    if value == "":
        return None, "UNVERIFIED (no base revision supplied)"
    base = (value or "").strip()
    if base:
        if set(base) == {"0"}:
            return None, "UNVERIFIED (zero base)"
        if subprocess.run(["git", "cat-file", "-e", f"{base}^{{commit}}"], cwd=ROOT).returncode != 0:
            return "!", f"base {base} is not a commit"
        return base, ""
    for candidate in ("origin/dev", "HEAD~1"):
        result = subprocess.run(["git", "rev-parse", "--verify", candidate], cwd=ROOT, text=True, capture_output=True)
        if result.returncode == 0:
            base = result.stdout.strip()
            return base, f"default range {base[:12]}..{head}"
    return None, "UNVERIFIED (no default base)"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default=None)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--no-range", action="store_true")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--include-staged", action="store_true")
    args = parser.parse_args(argv)

    results = [run_step(step, args.json) for step in STEPS]
    base, note = resolve_base(args.base, args.no_range, args.head)
    if base == "!":
        print(f"docs-contracts: {note}", file=sys.stderr)
        return 1
    if args.base is not None and (args.base == "" or set(args.base.strip()) == {"0"}):
        print(f"::warning title=docs-range::{note}; pass an explicit base to verify the range", flush=True)
    if base:
        if not args.json:
            print(f"==> docs_impact: {note}", flush=True)
        result = subprocess.run(
            [sys.executable, "-B", "scripts/docs_impact.py", "--base", base, "--head", args.head]
            + (["--include-staged"] if args.include_staged else []),
            cwd=ROOT, text=True, capture_output=True,
        )
        if not args.json and result.stdout:
            print(result.stdout, end="", flush=True)
        if not args.json and result.stderr:
            print(result.stderr, end="", file=sys.stderr, flush=True)
        results.append({
            "name": "documentation impact (range)",
            "gate": "documentation impact (range)",
            "status": "pass" if result.returncode == 0 else "fail",
            "code": result.returncode,
            "command": f"python3 -B scripts/docs_impact.py --base {base} --head {args.head}",
            "output": (result.stdout + result.stderr)[-TAIL:],
        })
    else:
        if not args.json:
            print(f"==> docs_impact: {note}", flush=True)
        results.append({
            "name": "documentation impact (range)",
            "gate": "documentation impact (range)",
            "status": "unverified",
            "code": None,
            "command": "python3 -B scripts/docs_contracts.py --base <base> --head HEAD",
            "output": note,
        })

    failed = any(item["code"] not in (0, None) for item in results)
    if args.json:
        print(json.dumps({"base": base, "head": args.head, "results": results}, indent=2))
        return 1 if failed else 0

    state_path = ROOT / "docs" / "development" / "state.json"
    if state_path.is_file():
        state = json.loads(state_path.read_text(encoding="utf-8"))
        gates = {gate["name"]: gate for gate in state.get("gates", [])}
        mismatches = [
            f"{item['gate']}: state={gates.get(item['gate'], {}).get('status', 'missing')} run={item['status']}"
            for item in results
            if item.get("gate") and item["gate"] != "documentation impact (range)" and gates.get(item["gate"], {}).get("status") != item["status"]
        ]
        non_range = [item for item in results if item.get("gate") and item["gate"] != "documentation impact (range)"]
        local = "pass" if all(item["status"] == "pass" for item in non_range) else "fail"
        local_gate = gates.get("documentation contracts (local)", {})
        if local_gate.get("status") != local:
            mismatches.append(f"documentation contracts (local): state={local_gate.get('status', 'missing')} run={local}")
        if mismatches:
            for mismatch in mismatches:
                print(f"docs-contracts: recorded state differs: {mismatch}", file=sys.stderr)
            print("fix: run `python3 -B scripts/record_state.py --no-range` and commit the state change", file=sys.stderr)
            return 1
    if failed:
        print("docs-contracts: FAILED", file=sys.stderr)
        print("fix: update the affected document or trigger, then re-run the same command", file=sys.stderr)
        return 1
    range_unverified = any(
        item["gate"] == "documentation impact (range)" and item["status"] == "unverified"
        for item in results
    )
    if range_unverified:
        print("docs-contracts: local documentation contracts passed; range UNVERIFIED", flush=True)
    else:
        print("docs-contracts: all documentation contracts passed", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
