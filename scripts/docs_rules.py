#!/usr/bin/env python3
"""Check that the rule registry and the executed documentation steps agree."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RULES = Path("docs/rules.json")

sys.path.insert(0, str(ROOT / "scripts"))
from docs_contracts import STEPS  # noqa: E402


def validate(data: dict, step_names: set[str]) -> list[str]:
    errors: list[str] = []
    rules = data.get("rules", [])
    seen = set()
    covered = set()
    for rule in rules:
        identifier = rule.get("id")
        if not identifier or identifier in seen:
            errors.append(f"rule id is empty or duplicated: {identifier!r}")
        seen.add(identifier)
        kind = rule.get("type")
        if kind not in {"machine", "manual"}:
            errors.append(f"{identifier}: type must be machine or manual")
        if kind == "machine":
            enforced = rule.get("enforced_by", [])
            if not enforced:
                errors.append(f"{identifier}: machine rule has no enforced_by")
            for step in enforced:
                if step not in step_names:
                    errors.append(f"{identifier}: enforced_by `{step}` is not an executed step")
                covered.add(step)
        elif kind == "manual":
            if not rule.get("manual"):
                errors.append(f"{identifier}: manual rule needs a `manual` description")
    missing = sorted(step_names - covered)
    for step in missing:
        errors.append(f"executed gate `{step}` has no registered rule")
    return errors


def self_test() -> int:
    data = {"rules": [{"id": "DOC-01", "type": "machine", "enforced_by": ["gate"]}]}
    if validate(data, {"gate"}):
        print("docs-rules self-test: valid registry rejected", file=sys.stderr)
        return 1
    bad = {"rules": [{"id": "DOC-01", "type": "machine", "enforced_by": ["gate"]}]}
    if not validate(bad, {"gate", "other"}):
        print("docs-rules self-test: unregistered step was NOT caught", file=sys.stderr)
        return 1
    print("docs-rules: self-test passed (accepts full coverage, rejects an unregistered step)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    data = json.loads((ROOT / RULES).read_text(encoding="utf-8"))
    steps = {step["name"]: step for step in STEPS if step.get("gate")}
    steps["documentation impact (range)"] = {"gate": "documentation impact (range)"}
    errors = validate(data, set(steps))
    if errors:
        for error in errors:
            print(f"docs-rules: {error}", file=sys.stderr)
        return 1
    machine = sum(1 for rule in data["rules"] if rule.get("type") == "machine")
    print(f"docs-rules: {len(data['rules'])} rules; {machine} machine, {len(steps)} executed gates covered")
    return 0


if __name__ == "__main__":
    sys.exit(main())
