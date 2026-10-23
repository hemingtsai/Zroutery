"""Every command the interface calls, against every command the backend registers.

A control the dashboard offers and the backend cannot serve is the same defect
as a provider kind a build cannot use: the operator is told the thing works,
and it does not. The OpenCode provider kinds were found by reading; this finds
the rest of them by counting, and keeps finding them afterwards.

Two failures, and they are different in kind.

**A command the interface calls that the backend does not register at all.**
This is an instant error, wherever it is reached, and it is unambiguous: the
name is called and nothing answers it.

**A command the interface calls that is registered only under a feature, with
no availability flag guarding the call.** The command exists in some builds
and not in others, so the caller has to ask which build it is in before it
calls — and the flags the snapshot publishes are how it asks. A call that
mentions no flag is a call that will be made in a build where the command does
not exist.

The second failure is a naming check, so it is deliberately narrow: the
documentation above the call has to name one of the flags the snapshot
publishes. It does not have to name the *right* one — that is a reviewer's
judgement, and a gate that guesses would be a gate that lies.
"""
import argparse
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI_API = os.path.join(ROOT, "ui", "src", "api.ts")
BACKEND = os.path.join(ROOT, "src-tauri", "src")

# The flags the snapshot publishes, and what each one says.
AVAILABILITY_FLAGS = (
    "ml_available",
    "account_available",
    "account_maintenance_available",
    "api_checkin_available",
    "token_capture_available",
    "opencode_available",
)

# How far above a call to look for the documentation that guards it. Long
# enough for a paragraph, short enough that it cannot reach the entry above.
DOC_LOOKBACK = 14


def read(path: str) -> str:
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def calls_in_api(source: str) -> list[tuple[str, str]]:
    """Every command the interface calls, with the documentation above it.

    The result is a list of `(command, guard)` pairs, where the guard is the
    text of the comment above the call or an empty string.
    """
    lines = source.split("\n")
    found: list[tuple[str, str]] = []
    for index, line in enumerate(lines):
        match = re.search(r'invoke(?:<[^>]*>)?\(\s*"([a-z0-9_]+)"', line)
        if not match:
            continue
        name = match.group(1)
        window = lines[max(0, index - DOC_LOOKBACK) : index]
        found.append((name, "\n".join(window)))
    return found


def commands_in_backend() -> dict[str, tuple[str, str | None]]:
    """Every `#[tauri::command]` function, with the feature gate above it."""
    registered: dict[str, tuple[str, str | None]] = {}
    for root, _dirs, files in os.walk(BACKEND):
        for name in files:
            if not name.endswith(".rs"):
                continue
            path = os.path.join(root, name)
            lines = read(path).split("\n")
            gate: str | None = None
            for index, line in enumerate(lines):
                stripped = line.strip()
                if stripped.startswith("#[cfg(feature"):
                    gate = stripped
                elif stripped.startswith("#[") or stripped.startswith("//"):
                    continue
                elif stripped.startswith("pub async fn ") or stripped.startswith("pub fn "):
                    found = re.match(
                        r"(?:pub async fn|pub fn)\s+([a-z0-9_]+)", stripped
                    )
                    if found:
                        registered[found.group(1)] = (os.path.basename(path), gate)
                    gate = None
                elif stripped:
                    gate = None
    return registered


def audit(api_source: str, backend: dict[str, tuple[str, str | None]]) -> list[str]:
    problems: list[str] = []
    for name, guard in calls_in_api(api_source):
        if name not in backend:
            problems.append(
                "the interface calls `%s` and the backend registers no such command" % name
            )
            continue
        _path, gate = backend[name]
        if gate is None:
            continue
        if not any(flag in guard for flag in AVAILABILITY_FLAGS):
            problems.append(
                "the interface calls `%s`, which is registered only under %s, and the "
                "documentation above the call names no availability flag — so a build "
                "without the feature would call a command that does not exist"
                % (name, gate)
            )
    return problems


def self_test() -> int:
    """Prove the gate can see a failure.

    A harness that cannot distinguish caught from missed reports confidence it
    never measured, which is how this project's failing test names were lost
    five times.
    """
    api = read(UI_API)
    backend = commands_in_backend()

    # The real thing must be clean.
    problems = audit(api, backend)
    if problems:
        print("self-test: the real tree is not clean:")
        for problem in problems:
            print("  %s" % problem)
        return 1

    # And a command with no registration must be seen.
    broken = api.replace('invoke<Snapshot>("get_snapshot")', 'invoke<Snapshot>("get_snapshotx")')
    if not audit(broken, backend):
        print("self-test: a command with no registration was NOT seen")
        return 1

    # And a feature-gated call with no guard must be seen.
    guarded = audit(api, backend)
    stripped = api.replace("`snapshot.ml_available`", "`nothing`")
    found = audit(stripped, backend)
    if len(found) <= len(guarded):
        print("self-test: a feature-gated call with no guard was NOT seen")
        return 1

    print("self-test: the gate sees a missing command and an unguarded feature-gated call")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    api = read(UI_API)
    backend = commands_in_backend()
    calls = calls_in_api(api)
    print(
        "the interface calls %d commands; the backend registers %d"
        % (len({name for name, _ in calls}), len(backend))
    )
    problems = audit(api, backend)
    if problems:
        print()
        print("FAIL: %d problem(s)" % len(problems))
        for problem in problems:
            print("  %s" % problem)
        return 1
    print("PASS: every command the interface calls is registered, and every")
    print("      feature-gated one is guarded by an availability flag.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
