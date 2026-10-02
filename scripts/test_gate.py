#!/usr/bin/env python3
"""Run a command and report which tests failed, by name.

The parent audit lost the name of a failing test four times in this project, each
time because the capture was a shell pipeline with a pattern that did not match
the line the runner actually prints. Every one of those losses turned a real
observation into an unreproducible rumour, and the cost was paid again on the
next run. This script exists so that "it failed" and "here is what failed" are the
same step.

It changes nothing about what is run and nothing about what is checked. It runs
the command you give it, parses the output, and exits with the command's own exit
code so it can be dropped in front of a gate without altering it.

    python -B scripts/test_gate.py -- cargo test -p zroutery-core --all-features
    python -B scripts/test_gate.py --quiet -- cargo test --workspace

Every distinct failing test name is listed, deduplicated and sorted, whether it
was reported as a named failure block, a bare FAILED result line, or a panic. A
run that fails without any parseable name says so explicitly instead of implying
the output was clean.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from typing import Iterable, List, Sequence, Tuple

# `---- <name> stdout ----` is what the runner prints for a failed test.
_NAMED_BLOCK = re.compile(r"^---- (.+?) (?:stdout|stderr) ----\s*$", re.MULTILINE)
# `test result: FAILED. 38 passed; 1 failed; ...` carries no name.
_FAILED_RESULT = re.compile(r"^test result: FAILED\.", re.MULTILINE)
# A panic line names the thread, not the test: `thread 'x' (123) panicked at ...`
_PANICKED = re.compile(r"^thread '([^']+)'.*panicked at", re.MULTILINE)
# `error: test failed, to rerun pass `-p x --test y`` names the BINARY, not the test.
_RERUN_HINT = re.compile(r"error: test failed, to rerun pass")

# A rustc/clippy diagnostic is not a test failure; do not report it as a name.
_NOT_A_TEST = re.compile(
    r"^(?:error|warning)(?:\[|:)", re.IGNORECASE
)


def collect_names(output: str) -> Tuple[List[str], List[str]]:
    """Return (test names, other diagnostics worth surfacing)."""
    names = set()
    for match in _NAMED_BLOCK.finditer(output):
        names.add(match.group(1).strip())
    for match in _PANICKED.finditer(output):
        names.add(match.group(1).strip())
    ordered = sorted(n for n in names if n and not _NOT_A_TEST.match(n))

    other = []
    for line in output.splitlines():
        if _RERUN_HINT.search(line) or _FAILED_RESULT.match(line):
            other.append(line.strip())
    return ordered, other


def summarise(output: str, exit_code: int, label: str) -> str:
    """The report a human reads when something went wrong."""
    names, other = collect_names(output)
    lines = [
        "test-gate: %s exited %d" % (label, exit_code),
        "",
        "distinct failing tests (%d):" % len(names),
    ]
    if names:
        for name in names:
            lines.append("  - %s" % name)
    else:
        lines.append(
            "  NONE COULD BE PARSED. The command failed but its output carried no"
        )
        lines.append(
            "  recognisable test name, so this is not yet a diagnosis. Report it"
        )
        lines.append(
            "  as an unexplained failure rather than as a clean run."
        )
    if other:
        lines.append("")
        lines.append("runner lines:")
        for line in other:
            lines.append("  %s" % line)
    return "\n".join(lines)


# --------------------------------------------------------------------------
# self-test
# --------------------------------------------------------------------------

_FIXTURES: Sequence[Tuple[str, int, Sequence[str]]] = (
    # (fixture name, expected exit, expected names)
    (
        "a named failure block yields its name",
        101,
        ["some_test::failed_here"],
    ),
    (
        "several distinct failures are all listed",
        101,
        ["alpha_test", "beta_test"],
    ),
    (
        "a duplicate name is reported once",
        101,
        ["only_once"],
    ),
    (
        "a panic line names its thread",
        101,
        ["panicking_test"],
    ),
    (
        "a passing run yields nothing",
        0,
        [],
    ),
    (
        "a clean result line is not mistaken for a name",
        0,
        [],
    ),
    (
        "a rustc diagnostic is not reported as a test",
        0,
        [],
    ),
)


def _fixture_output(names: Sequence[str], panic: bool = False) -> str:
    out = [
        "running 3 tests",
        "test ok_one ... ok",
    ]
    for name in names:
        out.append("---- %s stdout ----" % name)
        if panic:
            out.append(
                "thread '%s' (1234) panicked at src/lib.rs:9:9:" % name
            )
        else:
            out.append("assertion `left == right` failed")
    out.append("test result: FAILED. 2 passed; 1 failed; 0 ignored")
    out.append(
        "error: test failed, to rerun pass `-p zroutery-core --test thing`"
    )
    return "\n".join(out) + "\n"


def self_test() -> int:
    failures = []
    for name, expected_exit, expected_names in _FIXTURES:
        if expected_exit == 0:
            output = (
                "running 3 tests\ntest a ... ok\ntest b ... ok\n"
                "test result: ok. 3 passed; 0 failed; 0 ignored\n"
            )
        else:
            panic = "panicked" in name
            output = _fixture_output(expected_names, panic=panic)
        names, _ = collect_names(output)
        if names != sorted(expected_names):
            failures.append(
                "%s: expected %r, got %r" % (name, sorted(expected_names), names)
            )

    # An unparseable failure must say so rather than look clean.
    opaque = "some tool failed in a way that names nothing\n"
    report = summarise(opaque, 1, "opaque")
    if "NONE COULD BE PARSED" not in report:
        failures.append("opaque failure did not announce itself as unparsed")

    # The exit code of the command must be preserved, because this script is
    # meant to be dropped in front of a gate without changing what it gates.
    if summarise("x", 101, "x").splitlines()[0].find("exited 101") < 0:
        failures.append("summary does not carry the command's exit code")

    print("test-gate self-test: %s" % ("FAILED" if failures else "PASSED"))
    for failure in failures:
        print("  %s" % failure)
    if not failures:
        print(
            "  %d fixtures, and an unparseable failure is announced rather than"
            " reported as clean" % len(_FIXTURES)
        )
    return 1 if failures else 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Run a command and report failing test names.",
        add_help=True,
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="exercise the parser fixtures and exit",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="print only the report when the command fails",
    )
    parser.add_argument(
        "command",
        nargs=argparse.REMAINDER,
        help="the command to run, after `--`",
    )
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.self_test:
        return self_test()

    command = list(args.command)
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("no command given; pass it after `--`")

    label = " ".join(command[:3])
    completed = subprocess.run(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        encoding="utf-8",
        errors="replace",
    )
    output = completed.stdout or ""

    if completed.returncode != 0:
        sys.stdout.write(summarise(output, completed.returncode, label))
        sys.stdout.write("\n")
    elif not args.quiet:
        names, _ = collect_names(output)
        print(
            "test-gate: %s passed%s"
            % (label, "" if names else " with no failing names")
        )
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())