#!/usr/bin/env python3
"""Validate state.json without optional dependencies.

The schema file is the editor contract; this script enforces the same shape,
enums and cross-links by hand. A missing jsonschema package is irrelevant: the
checker never skips a check because an optional import is absent.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STATE = Path("docs/development/state.json")
SCHEMA = Path("docs/development/state.schema.json")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
HEAD_RE = re.compile(r"^[0-9a-f]{7,40}$")
STATUSES = {"pass", "fail", "unverified", "unrun", "retired"}
BLOCKER_STATES = {"partial", "blocked", "ready", "done", "open"}
GATE_KEYS = {"name", "status", "command", "detail", "quarantine"}
BLOCKER_KEYS = {"id", "status", "summary", "blocked_by"}


def git(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=root, text=True, capture_output=True)


def schema_consistency(schema: dict) -> list[str]:
    errors: list[str] = []
    gates = schema["properties"]["gates"]["items"]["properties"]
    blockers = schema["properties"]["blockers"]["items"]["properties"]
    checks = [
        ("gate keys", GATE_KEYS, set(gates)),
        ("gate statuses", STATUSES, set(gates["status"]["enum"])),
        ("blocker keys", BLOCKER_KEYS, set(blockers)),
        ("blocker statuses", BLOCKER_STATES, set(blockers["status"]["enum"])),
    ]
    for label, code, declared in checks:
        if code != declared:
            errors.append(f"schema {label} differ from the checker: code={sorted(code)} schema={sorted(declared)}")
    return errors


def validate(state: dict, schema: dict, root: Path = ROOT, git_check: bool = True, schema_check: bool = True) -> list[str]:
    errors: list[str] = []
    if schema_check:
        errors.extend(schema_consistency(schema))
    allowed_top = set(schema.get("properties", {}))
    for key in sorted(set(state) - allowed_top):
        errors.append(f"unknown top-level key `{key}`")
    required = set(schema.get("required", []))
    missing = required - set(state)
    if missing:
        errors.append(f"missing keys: {sorted(missing)}")
    if state.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    as_of = state.get("as_of")
    if not isinstance(as_of, str) or not DATE_RE.match(as_of):
        errors.append("as_of must be YYYY-MM-DD")
    else:
        today = dt.date.today()
        if dt.date.fromisoformat(as_of) > today:
            errors.append(f"as_of {as_of} is in the future (today {today})")
    head = state.get("head")
    if not isinstance(head, str) or not HEAD_RE.match(head):
        errors.append("head must be a 7-40 character lowercase hex commit")
    elif git_check:
        if git(root, "cat-file", "-e", f"{head}^{{commit}}").returncode != 0:
            errors.append(f"head {head} is not a commit")
        elif git(root, "merge-base", "--is-ancestor", head, "HEAD").returncode != 0:
            errors.append(f"head {head} is not an ancestor of HEAD")
        else:
            committed = git(root, "show", "-s", "--format=%cs", head).stdout.strip()
            if DATE_RE.match(as_of or "") and committed and as_of < committed:
                errors.append(f"as_of {as_of} predates head commit date {committed}")
    if not isinstance(state.get("branch"), str) or not state["branch"]:
        errors.append("branch must be a non-empty string")

    gates = state.get("gates", [])
    if not isinstance(gates, list):
        errors.append("gates must be an array")
        gates = []
    names = set()
    for index, gate in enumerate(gates):
        if not isinstance(gate, dict):
            errors.append(f"gates[{index}]: must be an object")
            continue
        for key in sorted(set(gate) - GATE_KEYS):
            errors.append(f"gates[{index}]: unknown key `{key}`")
        name = gate.get("name")
        if not isinstance(name, str) or not name or name in names:
            errors.append(f"gates[{index}]: name is empty or duplicated")
        names.add(name)
        if gate.get("status") not in STATUSES:
            errors.append(f"gates[{index}]: status {gate.get('status')!r} is not {sorted(STATUSES)}")
        if not isinstance(gate.get("command"), str) or not gate["command"]:
            errors.append(f"gates[{index}]: command is empty")
        if "detail" in gate and not isinstance(gate["detail"], str):
            errors.append(f"gates[{index}]: detail must be a string")
        if "quarantine" in gate and not isinstance(gate["quarantine"], bool):
            errors.append(f"gates[{index}]: quarantine must be a boolean")

    blockers = state.get("blockers", [])
    if not isinstance(blockers, list):
        errors.append("blockers must be an array")
        blockers = []
    blocker_ids = set()
    for index, blocker in enumerate(blockers):
        if not isinstance(blocker, dict):
            errors.append(f"blockers[{index}]: must be an object")
            continue
        for key in sorted(set(blocker) - BLOCKER_KEYS):
            errors.append(f"blockers[{index}]: unknown key `{key}`")
        identifier = blocker.get("id")
        if not isinstance(identifier, str) or not identifier or identifier in blocker_ids:
            errors.append(f"blockers[{index}]: id is empty or duplicated")
        blocker_ids.add(identifier)
        if blocker.get("status") not in BLOCKER_STATES:
            errors.append(f"blockers[{index}]: status {blocker.get('status')!r} is not {sorted(BLOCKER_STATES)}")
        if not isinstance(blocker.get("summary"), str) or not blocker["summary"]:
            errors.append(f"blockers[{index}]: summary is empty")
        if "blocked_by" in blocker and (
            not isinstance(blocker["blocked_by"], list) or not all(isinstance(item, str) for item in blocker["blocked_by"])
        ):
            errors.append(f"blockers[{index}]: blocked_by must be an array of strings")

    for index, blocker in enumerate(blockers):
        if not isinstance(blocker, dict):
            continue
        for dependency in blocker.get("blocked_by", []):
            if dependency not in blocker_ids:
                errors.append(f"blockers[{index}]: blocked_by `{dependency}` does not exist")
            if dependency == blocker.get("id"):
                errors.append(f"blockers[{index}]: blocked_by itself")

    seen: set[str] = set()
    visiting: set[str] = set()
    graph = {b.get("id"): b.get("blocked_by", []) for b in blockers if isinstance(b, dict)}

    def visit(node: str) -> None:
        if node in visiting:
            errors.append(f"blockers: cycle through `{node}`")
            return
        if node in seen:
            return
        visiting.add(node)
        for dependency in graph.get(node, []):
            visit(dependency)
        visiting.remove(node)
        seen.add(node)

    for node in graph:
        visit(str(node))

    if not isinstance(state.get("flaky"), list) or not all(isinstance(item, str) for item in state["flaky"]):
        errors.append("flaky must be an array of strings")
    green = state.get("last_full_green")
    if green is not None and (not isinstance(green, str) or not DATE_RE.match(green)):
        errors.append("last_full_green must be null or YYYY-MM-DD")
    return errors


def self_test() -> int:
    good = {
        "schema_version": 1,
        "as_of": dt.date.today().isoformat(),
        "head": "26d887f",
        "branch": "dev",
        "gates": [{"name": "tests", "status": "pass", "command": "cargo test"}],
        "blockers": [{"id": "7F", "status": "partial", "summary": "x"}],
        "flaky": [],
        "last_full_green": None,
    }
    schema = {"properties": {key: {} for key in good}, "required": list(good)}
    if validate(good, schema, git_check=False, schema_check=False):
        print(f"state-check self-test: good state rejected ({validate(good, schema, git_check=False, schema_check=False)})", file=sys.stderr)
        return 1
    bad = json.loads(json.dumps(good))
    bad["gates"][0]["status"] = "green"
    bad["blockers"][0]["blocked_by"] = ["missing"]
    if not validate(bad, schema, git_check=False, schema_check=False):
        print("state-check self-test: bad state was NOT caught", file=sys.stderr)
        return 1
    real = json.loads((ROOT / SCHEMA).read_text(encoding="utf-8"))
    drift = json.loads(json.dumps(real))
    drift["properties"]["gates"]["items"]["properties"]["status"]["enum"].append("green")
    if not schema_consistency(drift):
        print("state-check self-test: schema drift was NOT caught", file=sys.stderr)
        return 1
    print("state-check: self-test passed (rejects an invalid enum, a missing dependency and schema drift)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    state = json.loads((ROOT / STATE).read_text(encoding="utf-8"))
    schema = json.loads((ROOT / SCHEMA).read_text(encoding="utf-8"))
    errors = validate(state, schema)
    if errors:
        for error in errors:
            print(f"state-check: {error}", file=sys.stderr)
        return 1
    print(f"state-check: {len(state['gates'])} gates, {len(state['blockers'])} blockers; schema and blockers valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
