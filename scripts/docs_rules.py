#!/usr/bin/env python3
"""Check that the rule registry and the executed documentation steps agree."""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RULES = Path("docs/rules.json")

sys.path.insert(0, str(ROOT / "scripts"))
from docs_contracts import STEPS  # noqa: E402


def page_rows(root: Path) -> dict[str, tuple[str, str, str]]:
    page = root / "docs" / "development" / "reference" / "documentation-rules.md"
    rows: dict[str, tuple[str, str, str]] = {}
    if not page.is_file():
        return rows
    for line in page.read_text(encoding="utf-8").splitlines():
        match = re.match(r"^\| (DOC-\d{2}) \| ([^|]*) \| ([^|]*) \| ([^|]*) \|$", line)
        if match:
            rows[match.group(1)] = (
                match.group(2).strip().replace("`", ""),
                match.group(3).strip().replace("`", ""),
                match.group(4).strip(),
            )
    return rows


def files_errors(root: Path) -> list[str]:
    errors = []
    hook = root / ".githooks" / "pre-commit"
    if not hook.is_file() or not (hook.stat().st_mode & 0o111):
        errors.append("pre-commit hook is missing or not executable")
    else:
        text = hook.read_text(encoding="utf-8")
        for script in ("scripts/docs_contracts.py",):
            if script not in text:
                errors.append(f"pre-commit hook does not invoke {script}")
    ci = root / ".github" / "workflows" / "ci.yml"
    if not ci.is_file():
        errors.append("CI workflow is missing")
    else:
        active = [line for line in ci.read_text(encoding="utf-8").splitlines() if not line.lstrip().startswith("#")]
        if not any("scripts/docs_contracts.py" in line for line in active):
            errors.append("CI has no active run step invoking scripts/docs_contracts.py")
    return errors



def validate(data: dict, step_names: set[str], root: Path | None = None) -> list[str]:
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
    if root is not None:
        errors.extend(files_errors(root))
        rows = page_rows(root)
        for rule in rules:
            identifier = rule.get("id")
            row = rows.get(identifier)
            if row is None:
                errors.append(f"rules page is missing {identifier}")
                continue
            page_rule, page_enforced, page_type = row
            expected_enforced = ", ".join(rule.get("enforced_by", [])) if rule.get("type") == "machine" else rule.get("manual", "")
            if page_rule != rule.get("rule"):
                errors.append(f"{identifier}: rules page text differs from rules.json")
            if page_enforced != expected_enforced:
                errors.append(f"{identifier}: rules page enforced_by `{page_enforced}` differs from `{expected_enforced}`")
            if page_type != rule.get("type"):
                errors.append(f"{identifier}: rules page type `{page_type}` differs from `{rule.get('type')}`")
        for identifier in sorted(set(rows) - {rule.get("id") for rule in rules}):
            errors.append(f"rules page has unknown {identifier}")
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
    with tempfile.TemporaryDirectory(prefix="zr-rules-") as temporary:
        root = Path(temporary)
        hook = root / ".githooks" / "pre-commit"
        hook.parent.mkdir(parents=True)
        hook.write_text("python3 scripts/docs_contracts.py\n", encoding="utf-8")
        hook.chmod(0o755)
        ci = root / ".github" / "workflows" / "ci.yml"
        ci.parent.mkdir(parents=True)
        ci.write_text("jobs:\n  docs:\n    steps:\n      - run: python3 scripts/docs_contracts.py\n", encoding="utf-8")
        page = root / "docs" / "development" / "reference" / "documentation-rules.md"
        page.parent.mkdir(parents=True)
        page.write_text("| ID | Rule | Enforced by | Type |\n|---|---|---|---|\n| DOC-01 | Rule | gate | machine |\n", encoding="utf-8")
        data = {"rules": [{"id": "DOC-01", "rule": "Rule", "type": "machine", "enforced_by": ["gate"]}]}
        if validate(data, {"gate"}, root):
            print("docs-rules self-test: valid page/hook/CI rejected", file=sys.stderr)
            return 1
        page.write_text("| ID | Rule | Enforced by | Type |\n|---|---|---|---|\n| DOC-01 | Drifted | gate | machine |\n", encoding="utf-8")
        if not validate(data, {"gate"}, root):
            print("docs-rules self-test: page text drift was NOT caught", file=sys.stderr)
            return 1
        page.write_text("| ID | Rule | Enforced by | Type |\n|---|---|---|---|\n| DOC-01 | Rule | gate | machine |\n", encoding="utf-8")
        hook.write_text("# empty hook\n", encoding="utf-8")
        if not files_errors(root):
            print("docs-rules self-test: empty hook was NOT caught", file=sys.stderr)
            return 1
        hook.write_text("python3 scripts/docs_contracts.py\n", encoding="utf-8")
        ci.write_text("jobs:\n  docs:\n    steps:\n      # - run: python3 scripts/docs_contracts.py\n", encoding="utf-8")
        if not files_errors(root):
            print("docs-rules self-test: commented-out CI step was NOT caught", file=sys.stderr)
            return 1
    print("docs-rules: self-test passed (coverage, page rows, hook content, active CI step)")
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
    errors = validate(data, set(steps), ROOT)
    if errors:
        for error in errors:
            print(f"docs-rules: {error}", file=sys.stderr)
        return 1
    machine = sum(1 for rule in data["rules"] if rule.get("type") == "machine")
    print(f"docs-rules: {len(data['rules'])} rules; {machine} machine, {len(steps)} executed gates covered")
    return 0


if __name__ == "__main__":
    sys.exit(main())
