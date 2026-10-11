#!/usr/bin/env python3
"""Compile every feature combination a manifest actually permits, and refuse to pass quietly.

    python -B scripts/feature_matrix_gate.py
    python -B scripts/feature_matrix_gate.py --cargo "cargo +1.99.0"
    python -B scripts/feature_matrix_gate.py --self-test

# Why this exists

This project already has the defect on record. Evidence E-115: a CI row was red,
every gate run for the commit was green, and the cause was that a test file
referenced `zroutery_core::account::{...}` with no crate-level
`#![cfg(feature = "account")]` — so the file did not compile, and nothing compiled
it. E-115's own wording is that **the gates that were run were all
feature-enabled**, so the shape of the verification guaranteed it could not see
this. A defect that only exists when a feature is *off* is invisible to every gate
that runs with features on.

The same thing happened again while building the account maintenance surface, one
commit after the first. `Desktop::panel_adapter` and `panel_root` were gated on
`account-maint` while the account probe that calls them was gated on `newapi`, so
`--features newapi` alone — a perfectly valid, documentable build, and one the
feature's own manifest comment describes — did not compile. Every gate run for that
commit enabled `account-maint`, so all of them were green.

The two fixes that are *not* this script: widen the gate until the matrix passes
(which is what was done), and stop scheduling only feature-enabled builds. This
script is the second one.

# The matrix is derived, never written down

The combinations are computed from each manifest's own `[features]` table, so a
feature added next month is in the matrix the day it is added. A hand-written list
would be correct on the day it was written and would quietly stop covering the
next feature, which is the same decay as a test that is not wired to a pipeline.

A hand-written list would also have to encode which subsets are legal. It does not
have to: **cargo enables a feature's declared dependencies transitively**, so
`--features account-maint` already carries `newapi` and `account`. The only subsets
a person can actually reach are the ones *closed* under the declared feature
dependency relation, and those are exactly the subsets this script builds. For the
desktop crate that is nine of the sixteen subsets; the other seven are spellings of
combinations already covered.

# What it does not claim

`zroutery-core` does not currently compile without `ml`: `server/mod.rs` reads
`config.ml_routing` unconditionally, so the three `ml`-less subsets fail. That is
pre-existing and predates the account work, and it is **not this script's to fix** —
the fix is a `#[cfg]` decision about the learning stack, which is a different
subsystem's call.

So those combinations are listed as `KNOWN_UNSUPPORTED` rather than being dropped,
and the exemption is a **ratchet rather than an allowance**: if one of them ever
compiles, this gate fails and says the exemption is stale. An exemption that can
silently outlive its reason is worse than no exemption, because it reads as
coverage.

# The property that matters

**A failing combination exits non-zero, and a non-zero exit is not a warning.**
A gate that reports failures while returning success is worse than no gate, because
it is trusted — which is the argument this repository has now made twice about its
other validation tooling.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
import time
from typing import Dict, FrozenSet, List, Sequence, Tuple

# Verdicts. Not booleans: "the build failed" and "the exemption is stale" are
# different results, and collapsing them is how a gate reports a pass it never saw.
OK = "ok"
FAIL = "fail"
KNOWN_UNSUPPORTED = "known-unsupported"
RATCHET = "ratchet"

# Combinations that are known not to build, with the reason. Keyed by package name
# and the exact subset, so a neighbouring subset cannot inherit an exemption.
#
# Empty for the desktop crate: every combination its manifest permits must build.
KNOWN_UNSUPPORTED_MATRIX: Dict[str, Dict[FrozenSet[str], str]] = {
    "zroutery-core": {
        frozenset(): (
            "`zroutery-core` does not compile with no features: `server/mod.rs` reads "
            "`config.ml_routing` unconditionally. Pre-existing; predates the account "
            "work and is recorded rather than fixed here."
        ),
        frozenset({"account"}): (
            "Same cause: the unconditional `ml_routing` read is in shared server "
            "code, not in an account module."
        ),
        frozenset({"account", "newapi"}): (
            "Same cause. Reaching this subset means `newapi` was enabled without "
            "`ml`, which is legal as a command line and does not build."
        ),
    },
}

# A feature list longer than this is a signal the matrix has outgrown a power-set
# enumeration, and the honest response is to stop rather than to spend twenty
# minutes compiling 8192 combinations nobody asked about.
MAX_FEATURES_FOR_POWER_SET = 12

# The two packages whose matrices are walked, with the manifest each is read from.
PACKAGES: Sequence[Tuple[str, str]] = (
    ("zroutery", "src-tauri/Cargo.toml"),
    ("zroutery-core", "crates/zroutery-core/Cargo.toml"),
)


class ManifestError(Exception):
    """A manifest this script cannot read exactly.

    Raised rather than guessed at. A parser that skipped a line it did not
    recognise would report a matrix that was never this manifest's, and a gate
    reporting coverage it does not have is the failure this whole project has been
    burned by twice.
    """


_FEATURE_LINE = re.compile(r"^(?P<name>[A-Za-z0-9_.-]+)\s*=\s*\[(?P<body>.*)$")


def parse_features(manifest_path: str) -> Dict[str, List[str]]:
    """Read a manifest's `[features]` table.

    Returns a mapping of feature name to the local features it pulls in. Handles
    the TOML shapes that appear in this repository: single-line arrays, multi-line
    arrays, empty arrays, and whole-line comments.

    Deliberately strict about everything else. `default` is returned like any other
    feature and filtered by the caller.
    """
    with open(manifest_path, "r", encoding="utf-8") as handle:
        lines = handle.read().splitlines()

    features: Dict[str, List[str]] = {}
    in_section = False
    index = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        if stripped.startswith("[") and not stripped.startswith("[" * 2):
            in_section = stripped == "[features]"
            index += 1
            continue
        if not in_section:
            index += 1
            continue
        if not stripped or stripped.startswith("#"):
            index += 1
            continue

        match = _FEATURE_LINE.match(stripped)
        if not match:
            raise ManifestError(
                "%s line %d: cannot read %r inside [features]" % (manifest_path, index + 1, line)
            )

        name = match.group("name")
        body = match.group("body")
        while "]" not in body:
            index += 1
            if index >= len(lines):
                raise ManifestError(
                    "%s: feature %r opens an array that never closes" % (manifest_path, name)
                )
            body += lines[index]
        if "]" not in body:
            raise ManifestError(
                "%s: feature %r opens an array that never closes" % (manifest_path, name)
            )
        body = body[: body.index("]")]

        deps = []
        for raw in body.split(","):
            item = raw.strip()
            # A trailing comment on the last item, which is valid TOML and would
            # otherwise be read as part of the name.
            item = item.split("#", 1)[0].strip()
            if not item:
                continue
            if not (item.startswith('"') and item.endswith('"')):
                raise ManifestError(
                    "%s: feature %r lists %r, which is not a quoted string" % (manifest_path, name, item)
                )
            # The quotes are stripped here and not later. Reading them as part of
            # the name is the specific defect this script's own self-test found on
            # its first run: `"account"` was not the feature `account`, so no
            # dependency ever matched, every subset looked closed, and the matrix
            # came out as the full power set — sixteen combinations instead of the
            # eight a person can actually type. A parser that is wrong in the
            # direction of reporting *more* coverage is the dangerous direction.
            item = item[1:-1]
            # `zroutery-core/newapi` names a feature on another crate; only the
            # local half can be a member of this crate's feature set, so the crate
            # part is dropped and the name kept. Anything still outside the local
            # set is filtered by `closed_subsets`.
            item = item.split("/")[-1]
            item = item.removeprefix("dep:").lstrip("?")
            if item:
                deps.append(item)
        features[name] = deps
        index += 1

    if not features:
        raise ManifestError("%s has no [features] table" % manifest_path)
    return features


def closed_subsets(features: Dict[str, List[str]]) -> List[FrozenSet[str]]:
    """Every subset a person can actually reach with `--features`.

    A subset is reachable when it is closed under the feature dependency relation,
    because cargo enables a feature's declared dependencies transitively: asking for
    `account-maint` hands you `newapi` and `account` whether or not you named them.
    """
    names = sorted(n for n in features if n != "default")
    if len(names) > MAX_FEATURES_FOR_POWER_SET:
        raise ManifestError(
            "%d features is past the power-set enumeration limit of %d; narrow this "
            "gate or raise the limit deliberately"
            % (len(names), MAX_FEATURES_FOR_POWER_SET)
        )

    reachable: List[FrozenSet[str]] = []
    for mask in range(1 << len(names)):
        subset = frozenset(names[i] for i in range(len(names)) if mask & (1 << i))
        if _closure(subset, features) == subset:
            reachable.append(subset)
    reachable.sort(key=lambda s: (len(s), sorted(s)))
    return reachable


def _closure(subset: FrozenSet[str], features: Dict[str, List[str]]) -> FrozenSet[str]:
    """`subset` plus every feature its members pull in, transitively."""
    grown = set(subset)
    pending = list(subset)
    while pending:
        name = pending.pop()
        for dep in features.get(name, ()):
            # A dependency on another crate's feature lands here as a name that is
            # not in this table; it cannot be enabled locally and cannot make a
            # subset unclosed.
            if dep in features and dep not in grown:
                grown.add(dep)
                pending.append(dep)
    return frozenset(grown)


def _feature_argument(subset: FrozenSet[str]) -> str:
    return ",".join(sorted(subset))


def run_combination(
    cargo: Sequence[str],
    package: str,
    feature_argument: str,
    all_targets: bool,
) -> Tuple[int, str]:
    command = list(cargo) + ["check", "-p", package, "--no-default-features"]
    if feature_argument:
        command += ["--features", feature_argument]
    if all_targets:
        command.append("--all-targets")
    completed = subprocess.run(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding="utf-8", errors="replace"
    )
    return completed.returncode, completed.stdout or ""


def run_plain(
    cargo: Sequence[str],
    package: str,
    extra: Sequence[str],
    all_targets: bool,
) -> Tuple[int, str]:
    """`cargo check -p <package>` with the package's own defaults, plus `extra`."""
    command = list(cargo) + ["check", "-p", package] + list(extra)
    if all_targets:
        command.append("--all-targets")
    completed = subprocess.run(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding="utf-8", errors="replace"
    )
    return completed.returncode, completed.stdout or ""


def _first_errors(output: str, limit: int = 3) -> List[str]:
    """The `error` lines, so a failure says what failed rather than only that it did."""
    found = [line.strip() for line in output.splitlines() if line.strip().startswith("error")]
    return found[:limit]


def evaluate(
    cargo: Sequence[str],
    package: str,
    subset: FrozenSet[str],
    exemptions: Dict[FrozenSet[str], str],
    all_targets: bool,
) -> Tuple[str, str]:
    """Compile one combination and classify it.

    The classification that matters is the ratchet: a combination carrying an
    exemption that now builds is reported as `ratchet`, which the caller turns into
    a non-zero exit. The exemption has outlived its reason and must be removed.
    """
    argument = _feature_argument(subset)
    code, output = run_combination(cargo, package, argument, all_targets)
    label = "%s --no-default-features --features %s" % (package, argument or "(none)")

    if code == 0:
        if subset in exemptions:
            return RATCHET, (
                "%s builds, but it is listed as known-unsupported: %s\n"
                "    The exemption is stale. Remove it from KNOWN_UNSUPPORTED_MATRIX "
                "in scripts/feature_matrix_gate.py, and if the fix was deliberate, say "
                "so in the evidence registry." % (label, exemptions[subset])
            )
        return OK, ""
    if subset in exemptions:
        return KNOWN_UNSUPPORTED, exemptions[subset]
    return FAIL, "%s\n    %s" % (label, "\n    ".join(_first_errors(output)))


def check_package(
    cargo: Sequence[str],
    package: str,
    manifest_path: str,
    all_targets: bool,
    exemptions: Dict[FrozenSet[str], str] | None = None,
) -> Tuple[List[Tuple[str, str, str]], List[str]]:
    """Walk one package's matrix. Returns (rows, problems).

    `exemptions` defaults to this package's entry in `KNOWN_UNSUPPORTED_MATRIX` and
    is overridable so the self-test can exercise the exemption and ratchet paths
    without mutating the real table. The real run passes nothing.
    """
    features = parse_features(manifest_path)
    if exemptions is None:
        exemptions = KNOWN_UNSUPPORTED_MATRIX.get(package, {})
    rows: List[Tuple[str, str, str]] = []
    problems: List[str] = []

    for subset in closed_subsets(features):
        verdict, detail = evaluate(cargo, package, subset, exemptions, all_targets)
        rows.append(("%s --features %s" % (package, _feature_argument(subset) or "(none)"), verdict, detail))
        if verdict in (FAIL, RATCHET):
            problems.append("%s: %s\n    %s" % (verdict.upper(), rows[-1][0], detail))

    # The two spellings people actually type. Both are already members of the power
    # set above, but they are invoked without `--no-default-features`, so they are
    # checked in the form a contributor will actually run them in — and because a
    # plain `cargo check -p zroutery-core` resolves to *no* features, it is also the
    # combination an operator is most likely to type by accident. Carrying the
    # exemption through to this spelling is what caught that.
    plain_rows = (
        (["--all-features"], frozenset(n for n in features if n != "default")),
        ([], frozenset(features.get("default", []))),
    )
    for extra, subset in plain_rows:
        code, output = run_plain(cargo, package, extra, all_targets)
        label = "cargo check -p %s %s" % (package, " ".join(extra) if extra else "(default features)")
        if code == 0:
            if subset in exemptions:
                detail = (
                    "%s builds, but it is listed as known-unsupported: %s\n"
                    "    The exemption is stale. Remove it from KNOWN_UNSUPPORTED_MATRIX "
                    "in scripts/feature_matrix_gate.py." % (label, exemptions[subset])
                )
                rows.append((label, RATCHET, detail))
                problems.append("RATCHET: %s\n    %s" % (label, exemptions[subset]))
                continue
            rows.append((label, OK, ""))
        elif subset in exemptions:
            rows.append((label, KNOWN_UNSUPPORTED, exemptions[subset]))
        else:
            errors = _first_errors(output)
            rows.append((label, FAIL, "\n    ".join(errors)))
            problems.append("FAIL: %s\n    %s" % (label, "\n    ".join(errors)))

    return rows, problems


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

# A stand-in for cargo. It decides from the argument list, so a fixture can steer
# it without compiling anything: any feature argument containing "BOOM" fails,
# `FAKE_CARGO_FAIL_PLAIN` fails the spelling with no `--features` at all, and
# `FAKE_CARGO_ALWAYS_FAIL` fails everything.
_FAKE_CARGO = """\
import os, sys
args = sys.argv[1:]
if os.environ.get("FAKE_CARGO_ALWAYS_FAIL") == "1":
    sys.stdout.write("error[E0308]: mismatched types\\n")
    sys.exit(101)
if os.environ.get("FAKE_CARGO_FAIL_PLAIN") == "1" and "--features" not in args:
    sys.stdout.write("error[E0433]: cannot find `ml` in `zroutery_core`\\n")
    sys.exit(101)
if any("BOOM" in a for a in args):
    sys.stdout.write("error[E0599]: no method named `panel_adapter` found\\n")
    sys.exit(101)
print("    Checking zroutery v0.8.0")
"""

_FIXTURE_MANIFEST = """\
[package]
name = "fixture"

[features]
default = []
ml = []
account = []
newapi = ["account"]
account-maint = ["newapi", "zroutery-core/newapi"]
"""

# A line inside [features] that this parser has no reading for. It raises rather
# than skipping, because a skipped line is a combination that is silently missing
# from the matrix.
_UNREADABLE_MANIFEST = """\
[features]
account = []
this line is not a feature
"""


def self_test() -> int:
    """Prove the matrix is derived, the exemptions ratchet, and a failure is fatal.

    Each case is chosen so that a script which could not tell the difference would
    fail it. A self-test that only exercises the happy path proves nothing about a
    tool whose purpose is distinguishing outcomes.
    """
    failures: List[str] = []
    cargo = [sys.executable, "-c", _FAKE_CARGO]

    with tempfile.TemporaryDirectory(prefix="feature-matrix-selftest-") as workdir:
        manifest = os.path.join(workdir, "Cargo.toml")
        with open(manifest, "w", encoding="utf-8") as handle:
            handle.write(_FIXTURE_MANIFEST)

        # 1. The manifest is read, including a dependency on another crate.
        features = parse_features(manifest)
        if sorted(features) != ["account", "account-maint", "default", "ml", "newapi"]:
            failures.append("the fixture features were not read: %r" % sorted(features))
        if "account" not in features.get("newapi", []):
            failures.append("newapi must require account: %r" % features.get("newapi"))
        if "newapi" not in features.get("account-maint", []):
            failures.append("account-maint must require newapi: %r" % features.get("account-maint"))

        # 2. The matrix is the reachable subsets, not the power set. Six features
        #    minus `default` is five, so 32 subsets; only these are spellings a
        #    person can type, because cargo pulls dependencies in transitively.
        reachable = closed_subsets(features)
        if len(reachable) != 8:
            failures.append(
                "expected 8 reachable subsets for this fixture, got %d: %r"
                % (len(reachable), [sorted(s) for s in reachable])
            )
        for subset in reachable:
            if subset and "newapi" in subset and "account" not in subset:
                failures.append("%r is reachable but missing a transitive dependency" % sorted(subset))
            if "account-maint" in subset and "newapi" not in subset:
                failures.append("%r is reachable but missing newapi" % sorted(subset))

        # 3. An unreadable manifest raises rather than losing a feature.
        bad = os.path.join(workdir, "bad.toml")
        with open(bad, "w", encoding="utf-8") as handle:
            handle.write(_UNREADABLE_MANIFEST)
        try:
            parse_features(bad)
            failures.append(
                "an unreadable [features] line must raise — skipping it would report "
                "a matrix that is not this manifest's"
            )
        except ManifestError:
            pass

        # 4. A combination that fails is FAIL, and names what failed.
        verdict, detail = evaluate(cargo, "fixture", frozenset({"BOOM"}), {}, False)
        if verdict != FAIL:
            failures.append("a failing combination must be FAIL, got %s" % verdict)
        if "E0599" not in detail:
            failures.append("a failure must name the error, got %r" % detail)

        # 5. An exempt combination that fails is tolerated — and only then.
        verdict, _ = evaluate(
            cargo, "fixture", frozenset({"BOOM"}), {frozenset({"BOOM"}): "pre-existing"}, False
        )
        if verdict != KNOWN_UNSUPPORTED:
            failures.append("an exempt combination that fails must be tolerated, got %s" % verdict)

        # 6. The ratchet. An exemption whose combination now builds must fail the
        #    gate, or the exemption outlives its reason and reads as coverage.
        verdict, detail = evaluate(
            cargo, "fixture", frozenset({"ml"}), {frozenset({"ml"}): "pre-existing"}, False
        )
        if verdict != RATCHET:
            failures.append(
                "an exempt combination that now builds must RATCHET, got %s — a "
                "stale exemption would silently read as coverage" % verdict
            )
        if "stale" not in detail:
            failures.append("the ratchet must say the exemption is stale: %r" % detail)

        # 7. A plain build is OK, and one that fails with nothing to do with the
        #    matrix is still FAIL rather than an exemption appearing out of nowhere.
        verdict, _ = evaluate(cargo, "fixture", frozenset({"ml"}), {}, False)
        if verdict != OK:
            failures.append("a passing combination must be OK, got %s" % verdict)

        # 8. `check_package` turns one bad combination into a problem list, which
        #    is what the caller turns into a non-zero exit.
        rows, problems = check_package(cargo, "fixture", manifest, False)
        if not rows:
            failures.append("check_package returned no rows")
        if problems:
            failures.append("the clean fixture must produce no problems, got %r" % problems)
        if any(verdict == FAIL for _, verdict, _ in rows):
            failures.append("the clean fixture produced a FAIL row: %r" % rows)

        os.environ["FAKE_CARGO_ALWAYS_FAIL"] = "1"
        try:
            _, problems = check_package(cargo, "fixture", manifest, False)
        finally:
            del os.environ["FAKE_CARGO_ALWAYS_FAIL"]
        if not problems:
            failures.append(
                "check_package must report a problem when every combination fails — "
                "a gate that returns success while reporting failures is worse than none"
            )

        # 9. The plain spelling carries its exemption too. This is the case the
        #    first real run of this gate found: `cargo check -p zroutery-core`
        #    resolves to no features, so the plain spelling *is* the bare
        #    combination, and a gate that only exempted the spelled-out form would
        #    have reported it as a fresh failure on every run.
        os.environ["FAKE_CARGO_FAIL_PLAIN"] = "1"
        try:
            rows, problems = check_package(
                cargo,
                "fixture",
                manifest,
                False,
                exemptions={frozenset(): "bare build, recorded"},
            )
        finally:
            del os.environ["FAKE_CARGO_FAIL_PLAIN"]
        default_row = [entry for entry in rows if "(default features)" in entry[0]]
        if not default_row:
            failures.append("check_package did not check the default-features spelling")
        elif default_row[0][1] != KNOWN_UNSUPPORTED:
            failures.append(
                "the plain spelling must carry its exemption; with an empty default it "
                "resolves to the subset the matrix already exempted, and reporting it as "
                "%s would make the gate red for a combination it knows about"
                % default_row[0][1]
            )
        if [p for p in problems if "(default features)" in p]:
            failures.append("an exempted spelling must not become a problem: %r" % problems)

        # 10. The ratchet reaches the plain spelling too. `--all-features` on a
        #     package whose every combination is exempt is the case where a stale
        #     exemption is easiest to miss, because the row looks like the ordinary
        #     "everything on" build.
        rows, problems = check_package(
            cargo, "fixture", manifest, False, exemptions={frozenset(("ml", "account", "newapi", "account-maint")): "stale"}
        )
        all_features_row = [entry for entry in rows if "--all-features" in entry[0]]
        if not all_features_row:
            failures.append("check_package did not check the --all-features spelling")
        elif all_features_row[0][1] != RATCHET:
            failures.append(
                "an exempt --all-features spelling that now builds must RATCHET, got %s"
                % all_features_row[0][1]
            )
        if not problems:
            failures.append(
                "a ratchet must become a problem, or the gate exits zero on a stale exemption"
            )

    for failure in failures:
        sys.stderr.write("feature-matrix self-test: %s\n" % failure)
    if failures:
        sys.stderr.write("feature-matrix: %d self-test failure(s)\n" % len(failures))
        return 1
    print(
        "feature-matrix: self-test passed; the matrix is derived from the manifest, "
        "an exemption ratchets, and a failure is a problem"
    )
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Compile every feature combination the manifests permit, so a defect "
            "that only exists with a feature switched off cannot hide behind "
            "feature-enabled gates."
        )
    )
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument(
        "--cargo",
        default="cargo",
        help='the cargo command to use, e.g. "cargo +1.99.0" (default: cargo)',
    )
    parser.add_argument(
        "--package",
        action="append",
        help="limit to one package; repeatable. Default is every known package.",
    )
    parser.add_argument(
        "--lib-only",
        action="store_true",
        help="check the library only, skipping test and example targets (faster, shallower)",
    )
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.self_test:
        return self_test()

    cargo = args.cargo.split()
    if not cargo:
        parser.error("--cargo is empty")

    packages = [(name, path) for name, path in PACKAGES if not args.package or name in args.package]
    if not packages:
        parser.error("no known package matched %r" % (args.package,))

    all_targets = not args.lib_only
    started = time.time()
    total = 0
    problems: List[str] = []

    for package, manifest in packages:
        print("feature-matrix: %s (%s)" % (package, manifest))
        try:
            rows, package_problems = check_package(cargo, package, manifest, all_targets)
        except ManifestError as error:
            sys.stderr.write("feature-matrix: REFUSED — %s\n" % error)
            return 2
        problems.extend(package_problems)
        for label, verdict, detail in rows:
            total += 1
            marker = {
                OK: "ok",
                FAIL: "FAIL",
                KNOWN_UNSUPPORTED: "known-unsupported",
                RATCHET: "RATCHET",
            }[verdict]
            print("    %-16s %s" % (marker, label))
            if verdict == KNOWN_UNSUPPORTED and detail:
                print("                     why: %s" % detail)

    elapsed = time.time() - started
    print("feature-matrix: %d combinations in %.0fs" % (total, elapsed))

    if problems:
        sys.stderr.write("\n")
        for problem in problems:
            sys.stderr.write("feature-matrix: %s\n\n" % problem)
        sys.stderr.write(
            "feature-matrix: FAILED — %d problem(s). A gate that reports failures while "
            "exiting zero is worse than no gate, because it is trusted.\n" % len(problems)
        )
        return 1

    print(
        "feature-matrix: PASS — %d combinations; every one a person can type now "
        "compiles" % total
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())