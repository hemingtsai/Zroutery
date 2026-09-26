#!/usr/bin/env python3
"""Validate the Zroutery orchestration records under ``docs/development``.

The validator is standard-library-only and deterministic, so the same
implementation runs locally and in CI.  It checks the record schema, the
node/DAG inventory, edge symmetry and acyclicity, source-reference paths,
evidence identifiers, the regression ledger, the ADR inventory, and the
relative Markdown links between the development documents.

``--self-test`` builds a throwaway tree and proves that each rejection rule
actually rejects; ``--root`` validates a specific checkout (default: the
repository containing this script).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from collections import deque
from collections.abc import Iterable
from pathlib import Path

# The canonical global Node state vocabulary.  It is a closed set: a new state
# requires a workflow decision, not a spelling variant.
LEGAL_STATES = frozenset(
    {
        "QUEUED",
        "READY",
        "RUNNING",
        "VALIDATING",
        "DONE",
        "PARTIAL",
        "BLOCKED",
        "FAILED",
        "REVALIDATE",
    }
)

# The record schema is exact: an added or missing field is a contract change.
FIELDS = frozenset(
    {
        "schema_version",
        "node_id",
        "display_name",
        "status",
        "implementation_state",
        "dependencies",
        "dependents",
        "required_gates",
        "blocked_by",
        "unblock_condition",
        "evidence",
        "source_references",
        "notes",
    }
)

LIST_FIELDS = (
    "dependencies",
    "dependents",
    "required_gates",
    "blocked_by",
    "evidence",
    "source_references",
    "notes",
)

EVIDENCE_ID = re.compile(r"\bE-\d{3}\b")
REGRESSION_ID = re.compile(r"\bREG-\d{3}\b")
ADR_FILE = re.compile(r"^(\d{4})-[a-z0-9-]+\.md$")
MARKDOWN_LINK = re.compile(r"\[[^\]]+\]\(([^)]+)\)")


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _development_dir(root: Path) -> Path:
    return root / "docs" / "development"


def _reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict:
    """Refuse a JSON object that repeats a key.

    ``json.loads`` keeps the last occurrence and reports nothing, so a record
    with two ``evidence`` arrays validates cleanly while silently ignoring one of
    them. A record is a contract, so an ambiguous one is not a record.
    """
    seen: dict[str, object] = {}
    for key, value in pairs:
        if key in seen:
            raise ValueError(f"duplicate key {key!r}")
        seen[key] = value
    return seen


def _load_records(root: Path, errors: list[str]) -> dict[str, dict]:
    status_dir = _development_dir(root) / "node-status"
    records: dict[str, dict] = {}
    for path in sorted(status_dir.glob("*.status.json")):
        try:
            data = json.loads(_read(path), object_pairs_hook=_reject_duplicate_keys)
        except (OSError, ValueError) as exc:
            errors.append(f"JSON parse {path.name}: {exc}")
            continue
        node_id = data.get("node_id")
        if set(data) != FIELDS:
            errors.append(f"schema fields {path.name}: {sorted(set(data) ^ FIELDS)}")
        if not isinstance(node_id, str) or node_id.lower() + ".status.json" != path.name:
            errors.append(f"filename/node_id mismatch: {path.name} vs {node_id!r}")
            continue
        if node_id in records:
            errors.append(f"duplicate node_id: {node_id}")
            continue
        records[node_id] = data
        if data.get("schema_version") != 1:
            errors.append(f"schema_version: {node_id}")
        if data.get("status") not in LEGAL_STATES:
            errors.append(f"illegal status: {node_id}={data.get('status')!r}")
        for field in LIST_FIELDS:
            if not isinstance(data.get(field), list):
                errors.append(f"not a list: {node_id}.{field}")
        for field in ("unblock_condition", "implementation_state", "display_name"):
            value = data.get(field)
            if not isinstance(value, str) or not value.strip():
                errors.append(f"empty {field}: {node_id}")
        if not isinstance(data.get("dependencies"), list) or not isinstance(
            data.get("blocked_by"), list
        ):
            continue
        if not set(data["blocked_by"]).issubset(set(data["dependencies"])):
            errors.append(f"blocked_by outside dependencies: {node_id}")
    return records


def _check_inventory(root: Path, records: dict[str, dict], errors: list[str]) -> None:
    dag = _read(_development_dir(root) / "dependency-dag.md")
    try:
        section = dag.split("## Complete node inventory", 1)[1].split(
            "## Critical-path edges", 1
        )[0]
    except IndexError:
        errors.append("dependency-dag.md: missing inventory or critical-path section")
        return
    table = set(re.findall(r"^\| `([A-Za-z0-9-]+)` \|", section, re.MULTILINE))
    if table != set(records):
        errors.append(
            "inventory mismatch "
            f"missing={sorted(table - set(records))} extra={sorted(set(records) - table)}"
        )
    states = dict(
        re.findall(r"^\| `([A-Za-z0-9-]+)` \| `([A-Z]+)` \|", section, re.MULTILINE)
    )
    for node_id, data in sorted(records.items()):
        if node_id in states and states[node_id] != data["status"]:
            errors.append(
                f"DAG state disagrees with record: {node_id} "
                f"dag={states[node_id]} record={data['status']}"
            )


def _check_edges(records: dict[str, dict], errors: list[str]) -> int:
    for node_id, data in sorted(records.items()):
        for dep in data["dependencies"]:
            if dep == node_id:
                errors.append(f"self edge: {node_id}")
            elif dep not in records:
                errors.append(f"unknown dependency: {node_id}->{dep}")
            elif node_id not in records[dep]["dependents"]:
                errors.append(f"missing reverse edge: {dep}->{node_id}")
        for dependent in data["dependents"]:
            if dependent == node_id:
                errors.append(f"self edge: {node_id}")
            elif dependent not in records:
                errors.append(f"unknown dependent: {node_id}->{dependent}")
            elif node_id not in records[dependent]["dependencies"]:
                errors.append(f"missing forward edge: {dependent}->{node_id}")

    indegree = {node_id: len(data["dependencies"]) for node_id, data in records.items()}
    queue = deque(sorted(node for node, degree in indegree.items() if degree == 0))
    visited = 0
    while queue:
        node_id = queue.popleft()
        visited += 1
        for dependent in records[node_id]["dependents"]:
            if dependent not in indegree:
                # Already reported as an unknown dependent; the cycle walk
                # must still produce a result instead of raising.
                continue
            indegree[dependent] -= 1
            if indegree[dependent] == 0:
                queue.append(dependent)
    if visited != len(records):
        errors.append("dependency graph contains a cycle")
    return sum(len(data["dependencies"]) for data in records.values())


def _check_references(root: Path, records: dict[str, dict], errors: list[str]) -> None:
    for node_id, data in sorted(records.items()):
        for ref in data["source_references"]:
            if not (root / ref).exists():
                errors.append(f"missing source reference: {node_id}->{ref}")


def _check_evidence(root: Path, records: dict[str, dict], errors: list[str]) -> int:
    registry = _read(_development_dir(root) / "evidence-registry.md")
    # Only the inventory rows define evidence; the prose also cites identifiers
    # when it discusses history, so a text-wide scan would report false repeats.
    defined = re.findall(r"^\| (E-\d{3}) \|", registry, re.MULTILINE)
    known = set(defined)
    if len(defined) != len(known):
        errors.append("evidence registry: repeated evidence row identifier")
    if not known:
        errors.append("evidence registry defines no evidence identifiers")
    for node_id, data in sorted(records.items()):
        for evidence in data["evidence"]:
            if not EVIDENCE_ID.fullmatch(evidence):
                errors.append(f"malformed evidence id: {node_id}->{evidence!r}")
            elif evidence not in known:
                errors.append(f"unknown evidence: {node_id}->{evidence}")
    return len(known)


def _check_regressions(root: Path, errors: list[str]) -> int:
    ledger = _read(_development_dir(root) / "regression-ledger.md")
    ids = REGRESSION_ID.findall(ledger)
    expected = {f"REG-{index:03d}" for index in range(1, len(set(ids)) + 1)}
    if set(ids) != expected:
        errors.append(f"regression ledger is not a contiguous sequence: {sorted(set(ids))}")
    return len(set(ids))


def _check_adrs(root: Path, errors: list[str]) -> int:
    decision_dir = _development_dir(root) / "decisions"
    numbers: list[int] = []
    for path in sorted(decision_dir.glob("*.md")):
        match = ADR_FILE.match(path.name)
        if not match:
            errors.append(f"unexpected ADR filename: {path.name}")
            continue
        numbers.append(int(match.group(1)))
    if numbers != list(range(1, len(numbers) + 1)):
        errors.append(f"ADR inventory is not contiguous from 0001: {numbers}")
    architecture = _read(_development_dir(root) / "architecture.md")
    roadmap = _read(_development_dir(root) / "roadmap.md")
    for number in numbers:
        label = f"ADR-{number:04d}"
        if label not in architecture and label not in roadmap:
            errors.append(f"{label} is not referenced by architecture.md or roadmap.md")
    return len(numbers)


def _check_links(root: Path, errors: list[str]) -> int:
    development = _development_dir(root)
    checked = 0
    for path in sorted(development.glob("*.md")):
        for target in MARKDOWN_LINK.findall(_read(path)):
            if target.startswith(("http://", "https://", "#", "mailto:")):
                continue
            checked += 1
            if not (path.parent / target.split("#", 1)[0]).exists():
                errors.append(f"broken Markdown link: {path.name} -> {target}")
    return checked


def validate(root: Path) -> tuple[list[str], dict[str, int]]:
    errors: list[str] = []
    development = _development_dir(root)
    for required in ("dependency-dag.md", "evidence-registry.md", "regression-ledger.md"):
        if not (development / required).exists():
            errors.append(f"missing required document: docs/development/{required}")
    if errors:
        return errors, {}

    records = _load_records(root, errors)
    if not records:
        errors.append("node-status directory contains no usable records")
        return errors, {}
    _check_inventory(root, records, errors)
    edges = _check_edges(records, errors)
    _check_references(root, records, errors)
    evidence = _check_evidence(root, records, errors)
    regressions = _check_regressions(root, errors)
    adrs = _check_adrs(root, errors)
    links = _check_links(root, errors)
    return errors, {
        "nodes": len(records),
        "edges": edges,
        "evidence": evidence,
        "regressions": regressions,
        "adrs": adrs,
        "links": links,
    }


def _record(
    node_id: str,
    status: str = "DONE",
    dependencies: Iterable[str] = (),
    dependents: Iterable[str] = (),
    blocked_by: Iterable[str] = (),
    evidence: Iterable[str] = ("E-001",),
    source_references: Iterable[str] = ("crates/zroutery-core/src/lib.rs",),
) -> dict:
    return {
        "schema_version": 1,
        "node_id": node_id,
        "display_name": node_id,
        "status": status,
        "implementation_state": "synthetic self-test record",
        "dependencies": list(dependencies),
        "dependents": list(dependents),
        "required_gates": ["synthetic gate"],
        "blocked_by": list(blocked_by),
        "unblock_condition": "synthetic unblock condition",
        "evidence": list(evidence),
        "source_references": list(source_references),
        "notes": ["synthetic self-test record"],
    }


def _fixture_tree(root: Path) -> None:
    development = _development_dir(root)
    (development / "node-status").mkdir(parents=True, exist_ok=True)
    (development / "decisions").mkdir(exist_ok=True)
    (root / "crates" / "zroutery-core" / "src").mkdir(parents=True, exist_ok=True)
    (root / "crates" / "zroutery-core" / "src" / "lib.rs").write_text("", encoding="utf-8")

    records = {
        "SYNTH-A": _record("SYNTH-A", dependents=["SYNTH-B"]),
        "SYNTH-B": _record("SYNTH-B", dependencies=["SYNTH-A"], dependents=["SYNTH-C"]),
        "SYNTH-C": _record("SYNTH-C", dependencies=["SYNTH-B"], blocked_by=["SYNTH-B"]),
    }
    for node_id, data in records.items():
        path = development / "node-status" / f"{node_id.lower()}.status.json"
        path.write_text(json.dumps(data, indent=2), encoding="utf-8")

    inventory = "\n".join(
        f"| `{node_id}` | `{data['status']}` | {', '.join(data['dependencies']) or '-'} |"
        for node_id, data in records.items()
    )
    (development / "dependency-dag.md").write_text(
        "# Dependency DAG\n\n"
        "## Complete node inventory\n\n"
        "| Node | State | Dependencies |\n|---|---|---|\n"
        f"{inventory}\n\n"
        "## Critical-path edges\n\nsynthetic\n",
        encoding="utf-8",
    )
    (development / "evidence-registry.md").write_text(
        "# Evidence Registry\n\n| E-001 | synthetic |\n", encoding="utf-8"
    )
    (development / "regression-ledger.md").write_text(
        "# Regression Ledger\n\nREG-001\n", encoding="utf-8"
    )
    (development / "architecture.md").write_text(
        "# Architecture\n\nADR-0001\n", encoding="utf-8"
    )
    (development / "roadmap.md").write_text("# Roadmap\n", encoding="utf-8")
    (development / "decisions" / "0001-synthetic.md").write_text(
        "# ADR-0001\n", encoding="utf-8"
    )


def _mutate(root: Path, node_id: str, mutate) -> list[str]:
    path = _development_dir(root) / "node-status" / f"{node_id.lower()}.status.json"
    data = json.loads(_read(path))
    mutate(data)
    path.write_text(json.dumps(data, indent=2), encoding="utf-8")
    errors, _ = validate(root)
    return errors


def _run_self_test() -> int:
    failures = 0

    def expect(name: str, errors: list[str], needle: str) -> None:
        nonlocal failures
        if any(needle in error for error in errors):
            print(f"PASS self-test fixture: {name}")
        else:
            failures += 1
            print(f"FAIL self-test fixture {name!r}: no {needle!r} in {errors}")

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        _fixture_tree(root)
        errors, counts = validate(root)
        if errors:
            failures += 1
            print(f"FAIL self-test baseline: {errors}")
        else:
            print(
                "PASS self-test baseline: "
                f"{counts['nodes']} nodes, {counts['edges']} edges, {counts['adrs']} ADRs"
            )

        cases = (
            ("illegal status", "SYNTH-C", lambda d: d.update(status="PASS"), "illegal status"),
            ("extra field", "SYNTH-C", lambda d: d.update(extra=1), "schema fields"),
            (
                "empty unblock condition",
                "SYNTH-C",
                lambda d: d.update(unblock_condition="  "),
                "empty unblock_condition",
            ),
            (
                "blocked_by outside dependencies",
                "SYNTH-C",
                lambda d: d.update(blocked_by=["SYNTH-MISSING"]),
                "blocked_by outside dependencies",
            ),
            (
                "asymmetric dependent",
                "SYNTH-C",
                lambda d: d.update(dependencies=[], blocked_by=[]),
                "missing forward edge",
            ),
            (
                "asymmetric dependency",
                "SYNTH-A",
                lambda d: d.update(dependents=[]),
                "missing reverse edge",
            ),
            (
                "unknown dependency",
                "SYNTH-B",
                lambda d: d.update(dependencies=["SYNTH-A", "SYNTH-GHOST"], dependents=["SYNTH-C", "SYNTH-GHOST"]),
                "unknown dependency",
            ),
            (
                "cycle",
                "SYNTH-C",
                lambda d: d.update(dependencies=["SYNTH-B", "SYNTH-A"], dependents=[]),
                "cycle",
            ),
            (
                "missing source reference",
                "SYNTH-C",
                lambda d: d.update(source_references=["crates/does-not-exist.rs"]),
                "missing source reference",
            ),
            (
                "unknown evidence",
                "SYNTH-C",
                lambda d: d.update(evidence=["E-999"]),
                "unknown evidence",
            ),
        )
        for name, node_id, mutate, needle in cases:
            _fixture_tree(root)
            expect(name, _mutate(root, node_id, mutate), needle)

        _fixture_tree(root)
        record = _development_dir(root) / "node-status" / "synth-c.status.json"
        # A duplicated key is invisible to a plain parse: the last one silently
        # wins, so this fixture has to be written as raw text.
        raw = record.read_text(encoding="utf-8")
        duplicate = raw.replace(
            '  "notes": [', '  "evidence": [\n    "E-001"\n  ],\n  "notes": [', 1
        )
        record.write_text(duplicate, encoding="utf-8")
        expect("duplicate key", validate(root)[0], "duplicate key 'evidence'")

        _fixture_tree(root)
        inventory = _development_dir(root) / "dependency-dag.md"
        text = inventory.read_text(encoding="utf-8").replace("| `SYNTH-C` |", "| `SYNTH-X` |")
        inventory.write_text(text, encoding="utf-8")
        expect("DAG inventory drift", validate(root)[0], "inventory mismatch")

        _fixture_tree(root)
        registry = _development_dir(root) / "evidence-registry.md"
        registry.write_text(
            "# Evidence Registry\n\n| E-001 | synthetic |\n| E-001 | duplicate |\n",
            encoding="utf-8",
        )
        expect("repeated evidence row", validate(root)[0], "repeated evidence row identifier")

        _fixture_tree(root)
        ledger = _development_dir(root) / "regression-ledger.md"
        ledger.write_text("# Regression Ledger\n\nREG-001\nREG-003\n", encoding="utf-8")
        expect("regression gap", validate(root)[0], "not a contiguous sequence")

        _fixture_tree(root)
        adr = _development_dir(root) / "decisions" / "0001-synthetic.md"
        adr.rename(_development_dir(root) / "decisions" / "0003-synthetic.md")
        expect("ADR numbering gap", validate(root)[0], "not contiguous from 0001")

        _fixture_tree(root)
        architecture = _development_dir(root) / "architecture.md"
        architecture.write_text("# Architecture\n", encoding="utf-8")
        expect("unreferenced ADR", validate(root)[0], "not referenced by architecture.md")

        _fixture_tree(root)
        roadmap = _development_dir(root) / "roadmap.md"
        roadmap.write_text("# Roadmap\n\n[gone](missing.md)\n", encoding="utf-8")
        expect("broken Markdown link", validate(root)[0], "broken Markdown link")

        _fixture_tree(root)
        (_development_dir(root) / "node-status" / "synth-c.status.json").unlink()
        expect("missing status record", validate(root)[0], "inventory mismatch")

    print(f"self-test summary: {failures} failure(s)")
    return 1 if failures else 0


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="prove that every rejection rule rejects a synthetic tree",
    )
    parser.add_argument(
        "--root",
        default=str(Path(__file__).resolve().parent.parent),
        help="repository root to validate (default: the repository of this script)",
    )
    args = parser.parse_args(list(argv) if argv is not None else None)

    status = 0
    if args.self_test:
        status |= _run_self_test()

    root = Path(args.root)
    if not _development_dir(root).is_dir():
        print(f"orch-docs: no docs/development under {root}", file=sys.stderr)
        return 1
    errors, counts = validate(root)
    if errors:
        print("orch-docs: FAILED")
        for error in errors:
            print(f" - {error}")
        status = 1
    else:
        print(
            "orch-docs: PASS; "
            f"{counts['nodes']} nodes; {counts['edges']} edges; "
            f"{counts['evidence']} evidence IDs; {counts['regressions']} regressions; "
            f"{counts['adrs']} ADRs; {counts['links']} development links"
        )
    return status


if __name__ == "__main__":
    raise SystemExit(main())
