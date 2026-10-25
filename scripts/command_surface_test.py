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

**A registered command that no capability grants.** Tauri only gates an
application's own commands when the application declares an app manifest, and
`src-tauri/build.rs` now does — so every `#[tauri::command]` in
`generate_handler!` needs its own `allow-<command>` entry in
`src-tauri/capabilities/`. Nothing connects the two files at build time. A
missing grant compiles, ships, and fails as a button that does nothing, which is
the same defect as a control the backend cannot serve, moved one layer out. The
reverse difference is checked too: a grant for a command nobody registers guards
nothing, and a reader counting grants would be counting a surface that is not
real.
"""
import argparse
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI_API = os.path.join(ROOT, "ui", "src", "api.ts")
BACKEND = os.path.join(ROOT, "src-tauri", "src")
CAPABILITIES = os.path.join(ROOT, "src-tauri", "capabilities")

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


def registered_handlers() -> list[str]:
    """Every command `tauri::generate_handler!` registers, in order.

    This is the list that runs, and it is narrower than
    `commands_in_backend()`: that one finds every `#[tauri::command]` function
    in the tree, including ones no handler mentions. The capability file has to
    match this one, because a grant for a command that is never registered is
    a permission that guards nothing, and a registered command with no grant is
    a command the webview cannot call.
    """
    source = read(os.path.join(BACKEND, "lib.rs"))
    match = re.search(r"generate_handler!\[(.*?)\]\)", source, re.S)
    if not match:
        return []
    names: list[str] = []
    for line in match.group(1).split("\n"):
        line = line.strip().rstrip(",")
        if not line or line.startswith("//") or line.startswith("#["):
            continue
        found = re.match(r"[a-z0-9_]+::([a-z0-9_]+)$", line)
        if found:
            names.append(found.group(1))
    return names


def granted_permissions() -> dict[str, set[str]]:
    """The permissions each capability grants, by capability identifier.

    Only the application commands are of interest here. A plugin permission
    (`core:event:allow-listen`) names a plugin command, not one of ours, so it
    is not part of the comparison — but an application permission
    (`allow-get-snapshot`) is, and it is only a real permission when
    `src-tauri/build.rs` declares an app manifest, because that is what
    generates it.
    """
    granted: dict[str, set[str]] = {}
    for root, _dirs, files in os.walk(CAPABILITIES):
        for name in sorted(files):
            if not name.endswith(".json"):
                continue
            path = os.path.join(root, name)
            try:
                document = json.loads(read(path))
            except json.JSONDecodeError as error:
                raise SystemExit("cannot read %s: %s" % (path, error))
            identifier = document.get("identifier") or name[: -len(".json")]
            entries = document.get("permissions") or []
            commands = set()
            for entry in entries:
                if not isinstance(entry, str):
                    continue
                if ":" in entry:
                    # A plugin permission. `plugin:default` and
                    # `core:default` are sets, not single commands.
                    continue
                if entry.startswith("allow-"):
                    commands.add(entry[len("allow-") :].replace("-", "_"))
            granted[identifier] = commands
    return granted


def audit_capabilities(handlers: list[str], granted: dict[str, set[str]]) -> list[str]:
    """The difference between the registered commands and the granted ones.

    Both directions matter, and they are different defects.

    **A registered command with no grant.** The command runs, and the webview
    cannot reach it. Nothing fails at build time — the capability file and the
    handler list are separate files with nothing connecting them — so the
    symptom is a button that quietly does nothing.

    **A grant for a command that is not registered.** The permission guards
    nothing. It is not a hole, but it is a claim about the surface that the
    tree does not back, and a reader counting grants to decide how wide the
    command surface is would be counting a number that is not real.
    """
    problems: list[str] = []
    all_granted: set[str] = set()
    for commands in granted.values():
        all_granted |= commands
    for handler in handlers:
        if handler not in all_granted:
            problems.append(
                "the backend registers `%s` and no capability grants it, so the "
                "webview cannot call it" % handler
            )
    for command in sorted(all_granted - set(handlers)):
        problems.append(
            "a capability grants `%s`, which no handler registers, so the grant "
            "guards nothing" % command
        )
    return problems


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
    handlers = registered_handlers()
    granted = granted_permissions()

    # The real thing must be clean, in both halves.
    problems = audit(api, backend) + audit_capabilities(handlers, granted)
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

    # And a registered command with no grant must be seen, in both directions.
    if not audit_capabilities(handlers + ["a_command_nobody_registers"], granted):
        print("self-test: a registered command with no grant was NOT seen")
        return 1
    widened = {name: set(commands) for name, commands in granted.items()}
    widened["default"].add("a_command_nobody_registers")
    if not audit_capabilities(handlers, widened):
        print("self-test: a grant for an unregistered command was NOT seen")
        return 1

    print("self-test: the gate sees a missing command, an unguarded feature-gated")
    print("           call, and a capability/handler difference in both directions")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    api = read(UI_API)
    backend = commands_in_backend()
    handlers = registered_handlers()
    granted = granted_permissions()
    calls = calls_in_api(api)
    print(
        "the interface calls %d commands; the backend registers %d; %d are in "
        "the handler"
        % (len({name for name, _ in calls}), len(backend), len(handlers))
    )
    problems = audit(api, backend) + audit_capabilities(handlers, granted)
    if problems:
        print()
        print("FAIL: %d problem(s)" % len(problems))
        for problem in problems:
            print("  %s" % problem)
        return 1
    print("PASS: every command the interface calls is registered, every")
    print("      feature-gated one is guarded by an availability flag, and the")
    print("      capabilities grant exactly the commands the handler registers.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
