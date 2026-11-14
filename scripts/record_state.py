#!/usr/bin/env python3
"""Record documentation gate status from actual runs into state.json."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STATE = Path("docs/development/state.json")


def expected(doc_results: list[dict]) -> tuple[str, str]:
    non_range = [item for item in doc_results if item["gate"] != "documentation impact (range)"]
    local = "pass" if all(item["status"] == "pass" for item in non_range) else "fail"
    if any(item["status"] == "fail" for item in doc_results):
        aggregate = "fail"
    elif any(item["status"] == "unverified" for item in doc_results):
        aggregate = "unverified"
    else:
        aggregate = "pass"
    return local, aggregate


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default=None)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--no-range", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)

    if not args.check:
        subprocess.run([sys.executable, "-B", "scripts/current_state.py", "--write"], cwd=ROOT, check=False)

    command = [sys.executable, "-B", "scripts/docs_contracts.py", "--json", "--head", args.head]
    if args.no_range:
        command.append("--no-range")
    if args.base is not None:
        command.extend(["--base", args.base])
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        print(f"record-state: docs_contracts produced no JSON: {error}", file=sys.stderr)
        return 1

    state = json.loads((ROOT / STATE).read_text(encoding="utf-8"))
    gates = {gate["name"]: gate for gate in state["gates"]}
    doc_results = [item for item in report["results"] if item["gate"]]
    local, aggregate_status = expected(doc_results)

    if args.check:
        mismatches = []
        for item in doc_results:
            gate = gates.get(item["gate"])
            if gate is None or gate.get("status") != item["status"]:
                mismatches.append(f"{item['gate']}: state={gate.get('status') if gate else 'missing'} run={item['status']}")
        local_gate = gates.get("documentation contracts (local)")
        if local_gate is None or local_gate.get("status") != local:
            mismatches.append(f"documentation contracts (local): state={local_gate.get('status') if local_gate else 'missing'} run={local}")
        aggregate = gates.get("documentation contracts")
        if aggregate is None or aggregate.get("status") != aggregate_status:
            mismatches.append(f"documentation contracts: state={aggregate.get('status') if aggregate else 'missing'} run={aggregate_status}")
        if mismatches:
            for mismatch in mismatches:
                print(f"record-state: state differs: {mismatch}", file=sys.stderr)
            print("fix: run `python3 -B scripts/record_state.py --no-range` and commit the state change", file=sys.stderr)
            return 1
        print(f"record-state: state matches the run ({len(doc_results)} gates)")
        return 0

    ran_at = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")
    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, text=True, capture_output=True).stdout.strip()
    for item in doc_results:
        gate = gates.get(item["gate"])
        if gate is None:
            gate = {"name": item["gate"], "command": ""}
            state["gates"].append(gate)
            gates[item["gate"]] = gate
        gate["status"] = item["status"]
        gate["command"] = item.get("command", "python3 -B scripts/docs_contracts.py")
        last = item.get("output", "").strip().splitlines()
        gate["detail"] = (last[-1][:200] if last else item["name"]) + (f" ({commit})" if commit else "")
        gate["ran_at"] = ran_at
        if commit:
            gate["commit"] = commit

    for name, status, detail in [
        ("documentation contracts (local)", local, "all non-range docs gates"),
        ("documentation contracts", aggregate_status, "all docs gates, including range"),
    ]:
        gate = gates.get(name)
        if gate is None:
            gate = {"name": name}
            state["gates"].append(gate)
            gates[name] = gate
        gate["status"] = status
        gate["command"] = "python3 -B scripts/docs_contracts.py"
        gate["detail"] = detail
        gate["ran_at"] = ran_at
        if commit:
            gate["commit"] = commit

    if args.dry_run:
        print(json.dumps({"local": local, "aggregate": aggregate_status, "gates": doc_results}, indent=2))
        return 0

    (ROOT / STATE).write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    subprocess.run([sys.executable, "-B", "scripts/current_state.py", "--write"], cwd=ROOT, check=False)
    print(f"record-state: local {local}, aggregate {aggregate_status} ({len(doc_results)} gates)")
    return 1 if aggregate_status == "fail" else 0


if __name__ == "__main__":
    sys.exit(main())
