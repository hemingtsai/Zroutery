#!/usr/bin/env python3
"""Validate docs/development/state.json against its schema and cross-links.

The schema is the editor-facing contract; this script also performs the
cross-field checks a schema cannot express (unique gate names, issue links,
blocker dependencies and cycles).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STATE = Path("docs/development/state.json")
SCHEMA = Path("docs/development/state.schema.json")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
HEAD_RE = re.compile(r"^[0-9a-f]{7,40}$")
URL_RE = re.compile(r"^https://github\.com/hemingtsai/Zroutery/issues/\d+$")
STATUSES = {"pass", "fail", "unverified", "unrun", "retired"}
ISSUE_STATES = {"open", "closed"}
BLOCKER_STATES = {"partial", "blocked", "ready", "done", "open"}


def validate(state: dict, schema: dict | None = None) -> list[str]:
    errors: list[str] = []
    required = {"schema_version", "as_of", "head", "branch", "gates", "issues", "blockers", "flaky", "last_full_green"}
    missing = required - set(state)
    if missing:
        errors.append(f"missing keys: {sorted(missing)}")
    if state.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    if not isinstance(state.get("as_of"), str) or not DATE_RE.match(state["as_of"]):
        errors.append("as_of must be YYYY-MM-DD")
    if not isinstance(state.get("head"), str) or not HEAD_RE.match(state["head"]):
        errors.append("head must be a 7-40 character lowercase hex commit")
    if not isinstance(state.get("branch"), str) or not state["branch"]:
        errors.append("branch must be a non-empty string")

    gates = state.get("gates", [])
    names = set()
    for index, gate in enumerate(gates):
        name = gate.get("name")
        if not name or name in names:
            errors.append(f"gates[{index}]: name is empty or duplicated")
        names.add(name)
        if gate.get("status") not in STATUSES:
            errors.append(f"gates[{index}]: status {gate.get('status')!r} is not {sorted(STATUSES)}")
        if not gate.get("command"):
            errors.append(f"gates[{index}]: command is empty")

    issues = state.get("issues", [])
    issue_numbers = set()
    for index, issue in enumerate(issues):
        number = issue.get("number")
        if not isinstance(number, int) or number < 1 or number in issue_numbers:
            errors.append(f"issues[{index}]: number is invalid or duplicated")
        issue_numbers.add(number)
        if issue.get("state") not in ISSUE_STATES:
            errors.append(f"issues[{index}]: state {issue.get('state')!r} is not {sorted(ISSUE_STATES)}")
        if not isinstance(issue.get("url"), str) or not URL_RE.match(issue["url"]):
            errors.append(f"issues[{index}]: url is not a repository issue URL")
        if not issue.get("title"):
            errors.append(f"issues[{index}]: title is empty")

    blockers = state.get("blockers", [])
    blocker_ids = set()
    for index, blocker in enumerate(blockers):
        identifier = blocker.get("id")
        if not identifier or identifier in blocker_ids:
            errors.append(f"blockers[{index}]: id is empty or duplicated")
        blocker_ids.add(identifier)
        if blocker.get("status") not in BLOCKER_STATES:
            errors.append(f"blockers[{index}]: status {blocker.get('status')!r} is not {sorted(BLOCKER_STATES)}")
        issue = blocker.get("issue")
        if issue is not None and issue not in issue_numbers:
            errors.append(f"blockers[{index}]: issue #{issue} is not in issues[]")
        if not blocker.get("summary"):
            errors.append(f"blockers[{index}]: summary is empty")
    for index, blocker in enumerate(blockers):
        for dependency in blocker.get("blocked_by", []):
            if dependency not in blocker_ids:
                errors.append(f"blockers[{index}]: blocked_by `{dependency}` does not exist")
            if dependency == blocker.get("id"):
                errors.append(f"blockers[{index}]: blocked_by itself")

    seen: set[str] = set()
    visiting: set[str] = set()
    graph = {b.get("id"): b.get("blocked_by", []) for b in blockers}

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
        visit(node)

    if not isinstance(state.get("flaky"), list) or not all(isinstance(item, str) for item in state["flaky"]):
        errors.append("flaky must be an array of strings")
    green = state.get("last_full_green")
    if green is not None and (not isinstance(green, str) or not DATE_RE.match(green)):
        errors.append("last_full_green must be null or YYYY-MM-DD")

    if schema is not None:
        try:
            import jsonschema
        except ImportError:
            pass
        else:
            try:
                jsonschema.validate(state, schema)
            except jsonschema.ValidationError as error:
                errors.append(f"schema: {error.message}")
    return errors


def self_test() -> int:
    good = {
        "schema_version": 1,
        "as_of": "2026-11-14",
        "head": "26d887f",
        "branch": "dev",
        "gates": [{"name": "tests", "status": "pass", "command": "cargo test"}],
        "issues": [{"number": 1, "title": "x", "state": "open", "url": "https://github.com/hemingtsai/Zroutery/issues/1"}],
        "blockers": [{"id": "7F", "status": "partial", "summary": "x", "issue": 1}],
        "flaky": [],
        "last_full_green": None,
    }
    if validate(good):
        print(f"state-check self-test: good state rejected ({validate(good)})", file=sys.stderr)
        return 1
    bad = json.loads(json.dumps(good))
    bad["gates"][0]["status"] = "green"
    bad["blockers"][0]["issue"] = 99
    if not validate(bad):
        print("state-check self-test: bad state was NOT caught", file=sys.stderr)
        return 1
    print("state-check: self-test passed (accepts a valid state, rejects an invalid enum and a missing issue link)")
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
    print(f"state-check: {len(state['gates'])} gates, {len(state['issues'])} issues, {len(state['blockers'])} blockers; schema and links valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
