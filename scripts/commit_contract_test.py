#!/usr/bin/env python3
"""Validate Zroutery commit subjects and Node evidence trailers.

The validator is intentionally standard-library-only so the same implementation
runs in the pull-request workflow and in a local checkout.  ``--self-test``
exercises the contract fixtures; ``--base/--head`` validates the non-merge
commits introduced by a change range.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Iterable

# The nine types are an allowlist.  In particular, vague words are not aliases
# for a generic change type.
TYPES = (
    "feat",
    "fix",
    "refactor",
    "test",
    "perf",
    "docs",
    "build",
    "ci",
    "chore",
)

# Module scopes are the finite union of the established repository boundaries
# and the current Node contract modules.  Compatibility scopes are retained so
# existing commits such as fix(tauri) and feat(provider) remain auditable;
# ``workflow`` is an explicit orchestration/documentation boundary, not a
# wildcard.
MODULE_SCOPES = (
    # Established repository boundaries.
    "core",
    "router",
    "protocol",
    "policy",
    "runtime",
    "ml",
    "account",
    "provider",
    "integration",
    "tauri",
    "workflow",
    "repo",
    # Current Node module boundaries.
    "routing",
    "observation",
    "stats",
    "newapi",
    "migration",
    "takeover",
    "server",
    "decision",
    "shadow",
    "reward",
    "replay",
    "dataset",
    "model",
    "ui",
    "docs",
    "tests",
)

# These are the finite, current Node stage identifiers.  Do not replace this
# with ``stage-[0-9]+`` or another unbounded pattern.
STAGE_SCOPES = (
    "7e1",
    "7e2a",
    "7e2b",
    "7e2c",
    "7e2d",
    "7e2e",
    "7e2f",
    "7e3",
    "7f",
    "7g",
    "7h",
)

SCOPES = MODULE_SCOPES + STAGE_SCOPES
FORBIDDEN_TYPES = frozenset(
    {"misc", "update", "change", "stuff", "work", "tmp", "final", "done"}
)
TRAILER_KEYS = ("Node", "Gate", "Status")
STATUSES = frozenset({"DONE", "PARTIAL", "BLOCKED", "FAILED"})

# The complete subject is capped at 72 characters.  The summary still receives
# separate checks for case, punctuation, imperative form, and vagueness.
SUBJECT_LIMIT = 72
TYPE_PATTERN = "|".join(re.escape(value) for value in TYPES)
SCOPE_PATTERN = "|".join(re.escape(value) for value in SCOPES)
SUBJECT_RE = re.compile(
    rf"^(?P<type>{TYPE_PATTERN})\((?P<scope>{SCOPE_PATTERN})\): "
    r"(?P<summary>[a-z][^\r\n]*)$"
)

# A bounded lexical check is the only deterministic way to reject a past-tense
# or otherwise non-imperative summary without pretending to understand English.
# It includes the verbs used by the documented examples and common engineering
# actions; callers should add a verb here only when it has the same imperative
# meaning.
IMPERATIVE_VERBS = frozenset(
    {
        "accept",
        "add",
        "align",
        "allow",
        "apply",
        "assert",
        "audit",
        "avoid",
        "build",
        "calculate",
        "capture",
        "change",
        "choose",
        "clean",
        "clarify",
        "close",
        "commit",
        "compare",
        "complete",
        "configure",
        "consolidate",
        "construct",
        "copy",
        "correct",
        "cover",
        "create",
        "decode",
        "deduplicate",
        "declare",
        "define",
        "delete",
        "deny",
        "derive",
        "design",
        "disable",
        "disconnect",
        "document",
        "drop",
        "emit",
        "enable",
        "encode",
        "enforce",
        "ensure",
        "evaluate",
        "exclude",
        "expose",
        "extract",
        "fail",
        "filter",
        "find",
        "fix",
        "format",
        "generate",
        "guard",
        "handle",
        "harden",
        "implement",
        "improve",
        "include",
        "increase",
        "initialize",
        "inject",
        "install",
        "isolate",
        "keep",
        "label",
        "lint",
        "lock",
        "maintain",
        "make",
        "map",
        "mark",
        "measure",
        "merge",
        "migrate",
        "model",
        "move",
        "normalize",
        "observe",
        "omit",
        "order",
        "parse",
        "pass",
        "persist",
        "pin",
        "plan",
        "preserve",
        "prevent",
        "print",
        "provide",
        "publish",
        "read",
        "record",
        "recompute",
        "reconcile",
        "reduce",
        "refactor",
        "refresh",
        "reject",
        "remove",
        "rename",
        "repair",
        "replace",
        "report",
        "resolve",
        "restore",
        "retain",
        "return",
        "right-align",
        "route",
        "run",
        "save",
        "scale",
        "select",
        "send",
        "separate",
        "serialize",
        "set",
        "simplify",
        "sort",
        "split",
        "start",
        "stop",
        "store",
        "stream",
        "submit",
        "support",
        "swap",
        "switch",
        "test",
        "tighten",
        "track",
        "transform",
        "translate",
        "unify",
        "update",
        "upgrade",
        "use",
        "validate",
        "verify",
        "wire",
        "write",
    }
)

# Exact phrases that communicate no auditable engineering intent.  More
# specific uses such as ``update 7e-2 decision learning status`` remain valid.
VAGUE_SUMMARIES = frozenset(
    {
        "all changes",
        "all work",
        "change code",
        "change stuff",
        "cleanup",
        "done",
        "final implementation",
        "finish everything",
        "fix stuff",
        "fixes",
        "improvements",
        "large refactor",
        "make tests pass",
        "misc cleanup",
        "misc fixes",
        "minor fixes",
        "stuff",
        "temporary fix",
        "tmp",
        "update code",
        "update project",
        "various changes",
        "work",
    }
)
GENERIC_OBJECTS = frozenset(
    {
        "all",
        "change",
        "changes",
        "cleanup",
        "code",
        "docs",
        "everything",
        "fix",
        "fixes",
        "implementation",
        "improvement",
        "improvements",
        "misc",
        "project",
        "stuff",
        "tests",
        "things",
        "updates",
        "work",
    }
)

# These markers are report/process content, not a subject summary.  A concise
# ``Verification:`` body may still name commands; the subject may not carry the
# report or a transcript.
SUBJECT_REPORT_RE = re.compile(
    r"(?ix)"
    r"(?:\b(?:test\s+transcripts?|test\s+output|test\s+results|gate\s+report|"
    r"gate\s+summary|gate\s+result|roadmap|next\s+stage|unblock\s+condition|"
    r"accepted\s+commits|regression\s+ledger)\b)"
    r"|(?:\b(?:stage\s+[0-9]+|tests?\s*:\s*[0-9]+|risks?\s*:|implementation\s*:|"
    r"scope\s*:|roadmap\s*:|node\s*:|gate\s*:|status\s*:|"
    r"verification\s*:|why\s*:|what\s*:))"
    r"|(?:\b(?:gate|status|tests?)\s+(?:report|summary|result|pass|passed|failed)\b)"
    r"|(?:\bcargo\s+(?:test|check|clippy)\b)"
    r"|(?:\b(?:PASS|FAILED)\s+(?:RESULT|STATUS|REPORT)\b)"
)

# Preserve the old workflow's useful protection against copying process reports
# into the body, while exempting the exact ``Gate: value`` trailer form.
BODY_REPORT_RE = re.compile(
    r"(?im)"
    r"(?:\[WORKFLOW\]|\bStage\s+[0-9]+|^\s*Roadmap\s*:|^\s*Next Stage\s*:|"
    r"^\s*Tests\s*:\s*[0-9]+|^\s*Risks\s*:|^\s*Scope\s*:|"
    r"^\s*Implementation\s*:|"
    r"^\s*Gate\s+[^:\r\n]+[—-]\s*PASS\b)"
)

TRAILER_LINE_RE = re.compile(r"^(?P<key>Node|Gate|Status):(?P<value>.*)$")
ANY_EVIDENCE_LINE_RE = re.compile(r"^(?:Node|Gate|Status)\s*:", re.IGNORECASE)
NODE_VALUE_RE = re.compile(r"^[A-Z0-9]+(?:-[A-Z0-9]+)*$")
GATE_VALUE_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
STATUS_VALUE_RE = re.compile(rf"^(?:{'|'.join(sorted(STATUSES))})$")


def _normalise_message(message: str) -> str:
    return message.replace("\r\n", "\n").replace("\r", "\n")


def _is_vague(summary: str) -> bool:
    """Return whether a summary is one of the deliberately vague forms."""

    normalised = " ".join(summary.lower().split())
    if normalised in VAGUE_SUMMARIES:
        return True

    tokens = re.findall(r"[a-z0-9]+", normalised)
    if not tokens:
        return True

    first = tokens[0]
    if first in {"misc", "stuff", "work", "tmp", "final", "done"}:
        # These words are not useful primary intents even when paired with a
        # generic noun (for example, ``tmp parser``).
        return True

    if first in {"update", "change"}:
        remainder = tokens[1:]
        if not remainder or all(token in GENERIC_OBJECTS for token in remainder):
            return True

    if len(tokens) <= 2 and tokens[-1] in GENERIC_OBJECTS:
        return True

    if len(tokens) <= 2 and any(token in {"stuff", "work"} for token in tokens):
        return True

    return False


def validate_subject(subject: str) -> list[str]:
    """Validate one commit subject and return human-readable errors."""

    if not subject:
        return ["subject is empty"]
    if "\n" in subject or "\r" in subject:
        return ["subject must be a single line"]
    if len(subject) > SUBJECT_LIMIT:
        return [
            f"subject is {len(subject)} characters; the complete subject limit is "
            f"{SUBJECT_LIMIT}"
        ]

    match = SUBJECT_RE.fullmatch(subject)
    if match is None:
        prefix = subject.split("(", 1)[0]
        errors: list[str] = []
        if prefix in FORBIDDEN_TYPES:
            errors.append(
                f"type {prefix!r} is forbidden; use one of: {', '.join(TYPES)}"
            )
        errors.append(
            "subject must match <type>(<scope>): <summary> with a whitelisted "
            "type and scope"
        )
        return errors

    summary = match.group("summary")
    errors = []
    if summary != summary.strip():
        errors.append("summary must not have leading or trailing whitespace")
    if summary.endswith("."):
        errors.append("summary must not end with a period")
    if summary.endswith(("!", "?")):
        errors.append("summary must use a plain imperative statement")
    if SUBJECT_REPORT_RE.search(summary):
        errors.append(
            "summary must not contain test transcripts, gate reports, roadmap, "
            "stage, or pass/fail evidence"
        )
    if _is_vague(summary):
        errors.append("summary is vague and does not describe an auditable intent")
    first_word = summary.split(None, 1)[0].lower()
    if first_word not in IMPERATIVE_VERBS:
        errors.append(
            "summary must begin with an imperative verb "
            f"(got {first_word!r})"
        )
    return errors


def _trailer_errors(message: str) -> list[str]:
    """Validate an optional, exact Node/Gate/Status trailer block.

    Ordinary repository commits may omit evidence trailers.  If any one of the
    three evidence keys appears, the final block must contain all three in the
    documented order, with exact values and a blank line before it.
    """

    lines = message.split("\n")
    # Ignore trailing blank lines when locating the final block.
    end = len(lines) - 1
    while end >= 0 and not lines[end].strip():
        end -= 1
    if end < 0:
        return []

    evidence_indices = [
        index
        for index, line in enumerate(lines[: end + 1])
        if ANY_EVIDENCE_LINE_RE.match(line)
    ]
    if not evidence_indices:
        return []

    expected_start = end - len(TRAILER_KEYS) + 1
    expected_indices = list(range(max(expected_start, 0), end + 1))
    errors: list[str] = []

    if expected_start < 0 or evidence_indices != expected_indices:
        errors.append(
            "Node/Gate/Status trailers must form one final block in the order "
            "Node, Gate, Status"
        )
        # Still inspect every evidence line so the diagnostic is useful.
        for index in evidence_indices:
            errors.extend(_trailer_value_errors(lines[index]))
        return errors

    block = lines[expected_start : end + 1]
    keys: list[str] = []
    for line in block:
        match = TRAILER_LINE_RE.fullmatch(line)
        if match is None:
            errors.append(
                "Node/Gate/Status trailers must use exact 'Key: value' lines"
            )
            continue
        keys.append(match.group("key"))
        errors.extend(_trailer_value_errors(line))

    if keys != list(TRAILER_KEYS):
        errors.append(
            "Node/Gate/Status trailers must be exactly Node, Gate, Status"
        )
    if expected_start == 0 or lines[expected_start - 1].strip():
        errors.append("Node/Gate/Status trailers must follow a blank line")

    return errors


def _trailer_value_errors(line: str) -> list[str]:
    match = TRAILER_LINE_RE.fullmatch(line)
    if match is None:
        return [f"invalid evidence trailer line: {line!r}"]
    key = match.group("key")
    value = match.group("value")
    if not value.startswith(" "):
        return [f"{key} trailer must contain exactly one non-empty value"]
    value = value[1:]
    if not value or value != value.strip():
        return [f"{key} trailer must contain exactly one non-empty value"]
    if key == "Node":
        if NODE_VALUE_RE.fullmatch(value) is None:
            return [
                "Node trailer must be an uppercase hyphenated identifier "
                "(for example COMMIT-CONTRACT or 7E-1A)"
            ]
    elif key == "Gate":
        if GATE_VALUE_RE.fullmatch(value) is None:
            return [
                "Gate trailer must be a lowercase hyphenated identifier "
                "(for example commit-contract)"
            ]
    elif key == "Status" and STATUS_VALUE_RE.fullmatch(value) is None:
        return ["Status trailer must be one of DONE, PARTIAL, BLOCKED, FAILED"]
    return []


def validate_message(message: str) -> list[str]:
    """Validate a complete commit message."""

    normalised = _normalise_message(message)
    lines = normalised.split("\n")
    subject = lines[0] if lines else ""
    errors = validate_subject(subject)

    if BODY_REPORT_RE.search("\n".join(lines[1:])):
        errors.append(
            "body must not contain roadmap, stage, implementation, or gate-report "
            "transcript markers; use concise Why/What/Verification content"
        )
    errors.extend(_trailer_errors(normalised))
    return errors


def _fixture_messages() -> tuple[list[tuple[str, str]], list[tuple[str, str]]]:
    boundary_prefix = "feat(core): add "
    boundary_subject = boundary_prefix + "x" * (SUBJECT_LIMIT - len(boundary_prefix))
    valid = [
        (
            "module subject",
            "feat(ml): add calibrated decision distribution",
        ),
        (
            "72-character boundary",
            boundary_subject,
        ),
        (
            "stage subject",
            "feat(7e2a): add candidate-masked decision distribution",
        ),
        (
            "workflow subject",
            "ci(workflow): align commit lint with node evidence",
        ),
        (
            "mark imperative compatibility subject",
            "docs(workflow): mark first implementation batch running",
        ),
        (
            "accept imperative compatibility subject",
            "fix(workflow): accept mark as imperative verb",
        ),
        (
            "tauri compatibility scope",
            "fix(tauri): harden config permissions",
        ),
        (
            "provider compatibility scope",
            "feat(provider): add provider health checks",
        ),
        (
            "integration compatibility scope",
            "test(integration): cover provider failover",
        ),
        (
            "node trailers",
            "fix(shadow): prevent duplicate routing planning\n\n"
            "Why: Keep the production plan authoritative.\n"
            "What: Record one decision identity.\n"
            "Verification: Add a deterministic fixture.\n\n"
            "Node: COMMIT-CONTRACT\n"
            "Gate: commit-contract\n"
            "Status: DONE",
        ),
        (
            "partial status trailers",
            "test(7e1): add deterministic replay coverage\n\n"
            "Node: 7E-1A\n"
            "Gate: model-identity-replay\n"
            "Status: PARTIAL",
        ),
    ]
    invalid = [
        (
            "past tense and period",
            "feat(ml): added some new model stuff.",
        ),
        (
            "forbidden vague type",
            "misc(core): fix the parser",
        ),
        (
            "unknown stage scope",
            "fix(7e9): prevent duplicate routing planning",
        ),
        (
            "unknown stage-shaped scope",
            "fix(stage-1): prevent duplicate routing planning",
        ),
        (
            "uppercase summary",
            "fix(core): Repair fallback logic",
        ),
        (
            "trailing period",
            "fix(core): repair fallback logic.",
        ),
        (
            "vague update",
            "chore(workflow): update project",
        ),
        (
            "test transcript in subject",
            "test(ml): add test transcript and gate report",
        ),
        (
            "long subject",
            "feat(ml): add a very long summary that exceeds the documented "
            "seventy two character limit for a complete commit subject",
        ),
        (
            "bad status",
            "fix(core): repair fallback logic\n\n"
            "Node: COMMIT-CONTRACT\n"
            "Gate: commit-contract\n"
            "Status: PASS",
        ),
        (
            "incomplete trailers",
            "fix(core): repair fallback logic\n\n"
            "Node: COMMIT-CONTRACT\n"
            "Status: DONE",
        ),
        (
            "trailer order",
            "fix(core): repair fallback logic\n\n"
            "Gate: commit-contract\n"
            "Node: COMMIT-CONTRACT\n"
            "Status: DONE",
        ),
        (
            "malformed gate value",
            "fix(core): repair fallback logic\n\n"
            "Node: COMMIT-CONTRACT\n"
            "Gate: commit contract\n"
            "Status: DONE",
        ),
        (
            "lowercase node value",
            "fix(core): repair fallback logic\n\n"
            "Node: commit-contract\n"
            "Gate: commit-contract\n"
            "Status: DONE",
        ),
    ]
    return valid, invalid


def _run_fixtures() -> int:
    valid, invalid = _fixture_messages()
    failures = 0

    for name, message in valid:
        errors = validate_message(message)
        if errors:
            failures += 1
            print(f"FAIL valid fixture {name!r}: {'; '.join(errors)}")
        else:
            print(f"PASS valid fixture: {name}")

    for name, message in invalid:
        errors = validate_message(message)
        if not errors:
            failures += 1
            print(f"FAIL invalid fixture {name!r}: unexpectedly accepted")
        else:
            print(f"PASS invalid fixture: {name} ({errors[0]})")

    # Every legal final state is accepted, not just the happy path.
    for status in sorted(STATUSES):
        message = (
            "fix(core): repair fallback logic\n\n"
            "Node: COMMIT-CONTRACT\n"
            "Gate: commit-contract\n"
            f"Status: {status}"
        )
        errors = validate_message(message)
        if errors:
            failures += 1
            print(f"FAIL legal status {status}: {'; '.join(errors)}")
        else:
            print(f"PASS legal status fixture: {status}")

    # Exercise every allowed scope and explicit forbidden scope.  This is a
    # matrix test, not a wildcard assertion.
    for scope in SCOPES:
        errors = validate_subject(f"ci({scope}): align commit contract")
        if errors:
            failures += 1
            print(f"FAIL allowed scope {scope!r}: {'; '.join(errors)}")
    forbidden_scopes = ("7e9", "7e2z", "stage-1", "misc", "unknown")
    for scope in forbidden_scopes:
        errors = validate_subject(f"ci({scope}): align commit contract")
        if not errors:
            failures += 1
            print(f"FAIL forbidden scope {scope!r}: unexpectedly accepted")
    print(
        f"scope matrix: {len(SCOPES)} allowed, "
        f"{len(forbidden_scopes)} forbidden examples checked"
    )
    print(f"fixture summary: {len(valid)} valid, {len(invalid)} invalid")
    return 1 if failures else 0


def _git_output(*args: str, strip: bool = False) -> str:
    output = subprocess.check_output(
        ["git", *args],
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return output.strip() if strip else output.rstrip("\r\n")


def _validate_range(base: str, head: str) -> int:
    rev_list = _git_output("rev-list", "--no-merges", f"{base}..{head}", strip=True)
    if not rev_list:
        print("commit-contract: no non-merge commits in range")
        return 0

    failed = False
    for sha in rev_list.splitlines():
        message = _git_output("log", "-1", "--format=%B", sha)
        errors = validate_message(message)
        subject = message.splitlines()[0] if message.splitlines() else ""
        if errors:
            failed = True
            print(f"::error title=commit {sha}::subject/body rejected: {subject!r}")
            for error in errors:
                print(f"::error title=commit {sha}::{error}")
    if failed:
        print("commit-lint: FAILED")
        return 1
    print(f"commit-lint: all {len(rev_list.splitlines())} non-merge commits valid")
    return 0


def main(argv: Iterable[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run deterministic valid/invalid and scope-matrix fixtures",
    )
    parser.add_argument("--base", help="base Git revision for a pull-request range")
    parser.add_argument("--head", default="HEAD", help="head Git revision (default: HEAD)")
    args = parser.parse_args(list(argv) if argv is not None else None)

    status = 0
    if args.self_test or args.base is None:
        status |= _run_fixtures()
    if args.base is not None:
        try:
            status |= _validate_range(args.base, args.head)
        except (OSError, subprocess.CalledProcessError) as error:
            print(f"commit-contract: unable to inspect Git range: {error}", file=sys.stderr)
            status = 1
    return status


if __name__ == "__main__":
    raise SystemExit(main())
