#!/usr/bin/env python3
"""Print a workspace receipt a parent agent can verify before accepting a child.

The receipt is derived only from files the child is required to read: the
repository guidance (`AGENTS.md`) and the machine state
(`docs/development/state.json`). A child that returns the receipt has opened
both; a child that cannot reproduce `agents_sha256`, `state_sha256`,
`dod_quote` or the state provenance has not. The receipt proves exposure, not
compliance: gates and review still judge the work itself.

Usage:
    python3 -B scripts/agent_brief.py
    python3 -B scripts/agent_brief.py --root <repo> --self-test
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AGENTS = Path("AGENTS.md")
STATE = Path("docs/development/state.json")
DOD_HEADING = "## Definition of done"


def git(root: Path, *args: str) -> str:
    result = subprocess.run(["git", *args], cwd=root, text=True, capture_output=True)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or f"git {' '.join(args)} failed")
    return result.stdout.strip()


def file_sha(root: Path, path: Path) -> str:
    return "sha256:" + hashlib.sha256((root / path).read_bytes()).hexdigest()


def section_quote(text: str, heading: str, seed: str) -> tuple[int, str]:
    """Return one (line number, line) from a `##` section, chosen by `seed`."""
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        if line.strip() == heading:
            start = index + 1
            break
    if start is None:
        candidates = [(number, line) for number, line in enumerate(lines, start=1) if line.strip()]
    else:
        candidates = []
        for offset in range(start, len(lines)):
            line = lines[offset]
            if line.startswith("## "):
                break
            if line.strip() and not line.lstrip().startswith(("```", ">")):
                candidates.append((offset + 1, line))
    if not candidates:
        raise ValueError(f"no quotable line found under {heading!r}")
    digest = hashlib.sha256(seed.encode()).hexdigest()
    number, line = candidates[int(digest[:8], 16) % len(candidates)]
    return number, line.strip()


def receipt(root: Path = ROOT) -> dict:
    agents = root / AGENTS
    state_path = root / STATE
    if not agents.is_file():
        raise FileNotFoundError(f"{AGENTS} is missing")
    if not state_path.is_file():
        raise FileNotFoundError(f"{STATE} is missing")
    state = json.loads(state_path.read_text(encoding="utf-8"))
    for key in ("as_of", "head", "branch"):
        if key not in state:
            raise ValueError(f"{STATE} is missing `{key}`")
    gates = state.get("gates", [])
    agents_text = agents.read_text(encoding="utf-8")
    head = git(root, "rev-parse", "--short", "HEAD")
    branch = git(root, "branch", "--show-current")
    seed = f"{head}:{state['head']}:{state['as_of']}"
    dod_line, dod_text = section_quote(agents_text, DOD_HEADING, seed)
    return {
        "head": head,
        "branch": branch,
        "state_as_of": state["as_of"],
        "state_head": state["head"],
        "recorded_gates": sum(1 for gate in gates if gate.get("source") == "recorded"),
        "asserted_gates": sum(1 for gate in gates if gate.get("source") == "asserted"),
        "agents_sha256": file_sha(root, AGENTS),
        "state_sha256": file_sha(root, STATE),
        "dod_line": dod_line,
        "dod_quote": dod_text,
    }


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-brief-") as temporary:
        root = Path(temporary)
        (root / "docs" / "development").mkdir(parents=True)
        (root / AGENTS).write_text(
            "# Repository guidance\n\n## Definition of done\n\nRun the gates before committing.\n",
            encoding="utf-8",
        )
        (root / STATE).write_text(
            json.dumps(
                {
                    "as_of": "2026-11-14",
                    "head": "abc1234",
                    "branch": "dev",
                    "gates": [
                        {"name": "demo", "status": "pass", "command": "true", "source": "recorded"},
                        {"name": "other", "status": "unverified", "command": "true", "source": "asserted"},
                    ],
                }
            ),
            encoding="utf-8",
        )
        git(root, "init", "-q", "-b", "main")
        git(root, "config", "user.email", "test@example.com")
        git(root, "config", "user.name", "Test")
        git(root, "add", ".")
        git(root, "commit", "-qm", "fixture")
        first = receipt(root)
        second = receipt(root)
        if first != second:
            print("agent-brief self-test: the receipt is not deterministic", file=sys.stderr)
            return 1
        if first["state_as_of"] != "2026-11-14" or first["state_head"] != "abc1234":
            print("agent-brief self-test: state provenance is wrong", file=sys.stderr)
            return 1
        if first["dod_line"] < 1 or "gates" not in first["dod_quote"]:
            print("agent-brief self-test: the definition-of-done quote is wrong", file=sys.stderr)
            return 1
        if (first["recorded_gates"], first["asserted_gates"]) != (1, 1):
            print("agent-brief self-test: gate provenance is wrong", file=sys.stderr)
            return 1
        (root / AGENTS).write_text("# changed\n\n## Definition of done\n\nDifferent.\n", encoding="utf-8")
        if receipt(root)["agents_sha256"] == first["agents_sha256"]:
            print("agent-brief self-test: a changed AGENTS.md kept its hash", file=sys.stderr)
            return 1
        (root / AGENTS).unlink()
        try:
            receipt(root)
        except FileNotFoundError:
            pass
        else:
            print("agent-brief self-test: a missing AGENTS.md was not rejected", file=sys.stderr)
            return 1
        print("agent-brief: self-test passed (deterministic receipt, state provenance, hash and missing-file rejection)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    try:
        print(json.dumps(receipt(Path(args.root)), indent=2, sort_keys=True))
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        print(f"agent-brief: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
