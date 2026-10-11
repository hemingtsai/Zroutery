#!/usr/bin/env python3
"""Prove that a test would catch a change, by making the change and looking.

    python -B scripts/mutation_gate.py \
        --file crates/zroutery-core/src/account/checkin.rs \
        --from 'ResourceEffect::NoChange' --to 'ResourceEffect::Unobserved' \
        -- cargo test -p zroutery-core --all-features --test checkin_resource_event_test

    python -B scripts/mutation_gate.py --self-test

# Why this exists

`scripts/test_gate.py` closes the loss of a failing test's name for everything CI
runs: it parses the runner's output and prints which tests failed, so "it failed"
and "here is what failed" are the same step. That defect was closed after this
project lost the name four times, and it was lost a fifth time inside a hand-rolled
mutation harness that sat outside the wired path. Five mutations were reported as
missed when all five were being caught, and the only reason it was noticed is that
the numbers were impossible — five misses with no compile error.

So the gap is not the parser. It is that a harness asking "would this test notice?"
is left to capture failures by hand. This script is that harness, and it reuses
`test_gate.collect_names` rather than carrying a second set of patterns.

# The property that matters

**A missed mutation exits non-zero.** Without that, this script would report
"missed" and carry on, and a rumour would once again be recorded as an
observation. The failure of a negative control is the one result that must not pass
quietly, which is the same reasoning E-100 used when it proved the artifact gate
could fail before trusting it.

# Three further refusals, each earned

An ambiguous `--from` is refused rather than applied to the first match. This
repository's editing was corrupted more than once in one session by exactly that:
a replacement whose anchor appeared zero times, or twice, and the difference
between "skipped" and "silently wrong" was not visible in the output.

A mutation that fails to compile is reported as `compile-error` and never as
`caught`. A build failure is not evidence that a test noticed anything, and
counting it as a catch is how a mutation harness reports success without running a
single assertion.

The file is restored in a `finally`, so a command that hangs, crashes or is
interrupted still leaves the working tree as it found it.

# What it does not do

It does not decide which mutations are worth trying, and it does not prove a
change correct. It answers one narrow question — would this command have noticed?
— and answers it in a way that cannot be mistaken for a pass.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from typing import List, Sequence, Tuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from test_gate import collect_names  # noqa: E402  (path set above)


# What a mutation run concluded. Deliberately not a boolean: "missed" and
# "the mutation did not compile" are different results and collapsing them is how a
# harness reports a catch it never measured.
CAUGHT = "caught"
MISSED = "missed"
COMPILE_ERROR = "compile-error"
REFUSED = "refused"

# Matched against a lowercased copy of the output. That detail is not cosmetic: the
# first version of this list carried `syntax error` and missed Python's
# `SyntaxError:`, so a mutation that did not build was classified as `missed`. The
# self-test found it on its first run, which is the argument for having one.
_COMPILE_MARKERS = (
    # Rustc.
    "could not compile",
    "error[e0",
    "error[e1",
    "error[e2",
    "error[e3",
    "error[e4",
    "error[e5",
    "error[e6",
    "error[e7",
    "error[e9",
    "unclosed delimiter",
    "unexpected closing delimiter",
    "mismatched closing delimiter",
    "this file contains an unclosed",
    # Python, whose spelling is capitalised and whose variants all derive from it.
    "syntaxerror",
    "indentationerror",
    "taberror",
)


def _looks_like_compile_error(output: str) -> bool:
    lowered = output.lower()
    return any(marker in lowered for marker in _COMPILE_MARKERS)


def _run(command: Sequence[str]) -> Tuple[int, str]:
    completed = subprocess.run(
        list(command),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        encoding="utf-8",
        errors="replace",
    )
    return completed.returncode, completed.stdout or ""


def check_mutation(
    path: str,
    old: str,
    new: str,
    command: Sequence[str],
    label: str = "",
) -> Tuple[str, List[str], str]:
    """Apply `old` -> `new` in `path`, run `command`, and classify the result.

    Returns (verdict, failing test names, output). The file is always restored.
    """
    try:
        with open(path, "r", encoding="utf-8", newline="") as handle:
            original = handle.read()
    except OSError as error:
        return REFUSED, [], "could not read %s: %s" % (path, error)

    occurrences = original.count(old)
    if occurrences != 1:
        # The refusal that has earned its place. Zero means the anchor is stale;
        # more than one means the edit is ambiguous, and picking one silently is how
        # a source file gets corrupted.
        return (
            REFUSED,
            [],
            "the pattern to replace occurs %d times in %s; it must occur exactly once"
            % (occurrences, path),
        )

    mutated = original.replace(old, new, 1)
    try:
        with open(path, "w", encoding="utf-8", newline="") as handle:
            handle.write(mutated)
        code, output = _run(command)
    finally:
        # Restored unconditionally. A command that hangs, crashes or is interrupted
        # must still leave the tree as it was found, because the next thing anyone
        # does is look at `git status`.
        try:
            with open(path, "w", encoding="utf-8", newline="") as handle:
                handle.write(original)
        except OSError as error:  # pragma: no cover - only on a broken filesystem
            sys.stderr.write(
                "mutation-gate: FAILED TO RESTORE %s: %s\n" % (path, error)
            )

    names, _ = collect_names(output)

    if _looks_like_compile_error(output):
        # Not a catch. Nothing was asserted.
        return COMPILE_ERROR, names, output
    if names:
        return CAUGHT, names, output
    if code != 0:
        # The command failed and reported no name. That is the unparseable case
        # `test_gate.py` refuses to let pass silently, and it is not a catch either.
        return MISSED, [], output
    return MISSED, [], output


# A stand-in for a test runner, so the self-test can create a real caught/missed
# distinction without compiling anything. Prints the runner's line for whatever it
# is told to print, which is how the fixtures steer it.
_FAKE_RUNNER = """\
import os, sys
# The file's current contents decide what this fake runner reports.
text = open(sys.argv[1], encoding="utf-8").read()
if "TRIGGER" in text:
    # What a real runner prints for a named failure.
    sys.stdout.write("---- some::test::name stdout ----\\n")
    sys.stdout.write("thread 'some::test::name' panicked at src/lib.rs:1:1\\n")
    sys.stdout.write("test result: FAILED. 0 passed; 1 failed; 0 ignored\\n")
    sys.exit(101)
print("test result: ok. 1 passed; 0 failed; 0 ignored")
"""

# The probe has to import sys itself: `python -c` runs in a fresh namespace where
# `sys` is not bound, so a probe written without it raises NameError instead of
# SyntaxError and the fixture silently stops testing what it claims to. That is the
# second defect this self-test found on its first run, and it is the same shape as
# the first: a control that is not exercising the thing it names.
_COMPILE_PROBE = "import sys; compile(open(sys.argv[1]).read(), 'x', 'exec')"

_COMPILE_ERROR_FIXTURE = """\
fn main() {
    // Deliberately unbalanced: this is what a mutation that does not build looks
    // like, and counting it as a catch would report success without an assertion.
"""


def self_test() -> int:
    """Prove the script can tell caught from missed, and that a miss fails.

    Each case below is chosen so that a script which could not tell the difference
    would fail it. A self-test that only exercises the happy path proves nothing
    about a tool whose entire purpose is distinguishing outcomes.
    """
    failures: List[str] = []

    with tempfile.TemporaryDirectory(prefix="mutation-gate-selftest-") as workdir:
        runner = os.path.join(workdir, "runner.py")
        with open(runner, "w", encoding="utf-8") as handle:
            handle.write(_FAKE_RUNNER)

        target = os.path.join(workdir, "subject.rs")
        command = [sys.executable, runner, target]

        def write(contents: str) -> None:
            with open(target, "w", encoding="utf-8") as handle:
                handle.write(contents)

        # 1. A mutation the runner notices.
        write("let x = 1;\n")
        verdict, names, _ = check_mutation(
            target, "let x = 1;", "let x = 2; // TRIGGER", command
        )
        if verdict != CAUGHT:
            failures.append("a mutation the runner reports must be CAUGHT, got %s" % verdict)
        if not names:
            failures.append("a catch must name the failing test, got no names")

        # 2. A mutation the runner does not notice. This is the case that matters:
        #    it must be reported as missed rather than quietly passing.
        write("let x = 1;\n")
        verdict, _, _ = check_mutation(
            target, "let x = 1;", "let x = 2; // harmless", command
        )
        if verdict != MISSED:
            failures.append(
                "a mutation nothing notices must be MISSED, got %s — a harness that "
                "cannot see this one cannot be trusted about the others" % verdict
            )

        # 3. The file is restored, so the next case starts from a known state.
        with open(target, encoding="utf-8") as handle:
            after = handle.read()
        if after != "let x = 1;\n":
            failures.append("the mutated file was not restored: %r" % after)

        # 4. A pattern that occurs twice is refused, not applied to the first match.
        write("let x = 1;\nlet x = 1;\n")
        verdict, _, detail = check_mutation(target, "let x = 1;", "let y = 9;", command)
        if verdict != REFUSED:
            failures.append("an ambiguous pattern must be REFUSED, got %s" % verdict)
        if "exactly once" not in detail:
            failures.append("the refusal must say why: %r" % detail)

        # 5. A pattern that occurs zero times is refused too.
        verdict, _, _ = check_mutation(target, "not present", "x", command)
        if verdict != REFUSED:
            failures.append("a stale pattern must be REFUSED, got %s" % verdict)

        # 6. A mutation that does not compile is not a catch. A harness that counts
        #    a build failure as success reports confidence it never measured.
        write("let x = 1;\n")
        build = [sys.executable, "-c", _COMPILE_PROBE, target]
        verdict, _, _ = check_mutation(
            target,
            "let x = 1;",
            _COMPILE_ERROR_FIXTURE,
            build,
        )
        if verdict != COMPILE_ERROR:
            failures.append(
                "a mutation that does not compile must be COMPILE_ERROR, got %s — "
                "counting it as a catch would report success without an assertion" % verdict
            )

    for failure in failures:
        sys.stderr.write("mutation-gate self-test: %s\n" % failure)
    if failures:
        sys.stderr.write("mutation-gate: %d self-test failure(s)\n" % len(failures))
        return 1
    print("mutation-gate: self-test passed; caught, missed, refused and "
          "compile-error are distinguished")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Apply a change to one file, run a command, and report whether the "
            "command would have noticed. Exits non-zero when the mutation was "
            "missed, so a negative control cannot pass quietly."
        ),
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--file", help="the file to mutate")
    parser.add_argument("--from", dest="old", help="text to replace, occurring exactly once")
    parser.add_argument("--to", dest="new", help="replacement text")
    parser.add_argument("--label", default="", help="what this mutation is testing")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.self_test:
        return self_test()

    command = list(args.command)
    if command and command[0] == "--":
        command = command[1:]
    if not args.file or args.old is None or args.new is None or not command:
        parser.error(
            "need --file, --from, --to and a command after `--`; "
            "or --self-test"
        )

    what = args.label or "%s: %s -> %s" % (args.file, args.old, args.new)
    print("mutation-gate: %s" % what)
    verdict, names, output = check_mutation(args.file, args.old, args.new, command)

    if verdict == REFUSED:
        sys.stderr.write("mutation-gate: REFUSED — %s\n" % output.strip())
        return 2
    if verdict == COMPILE_ERROR:
        sys.stderr.write(
            "mutation-gate: COMPILE-ERROR — the mutation did not build, so nothing "
            "was asserted and this proves nothing.\n"
        )
        sys.stderr.write(output[-4000:])
        return 3
    if verdict == CAUGHT:
        print("mutation-gate: CAUGHT by %d test(s):" % len(names))
        for name in names:
            print("    %s" % name)
        return 0

    sys.stderr.write(
        "mutation-gate: MISSED — the command passed with the mutation applied and "
        "reported no failing test. The tests do not cover this behaviour, or the "
        "capture is broken. Either way this run is not evidence.\n"
    )
    sys.stderr.write(output[-4000:])
    return 1


if __name__ == "__main__":
    sys.exit(main())