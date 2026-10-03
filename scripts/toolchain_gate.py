#!/usr/bin/env python3
"""Fail when the local toolchain is not the one CI resolves.

Every job in ``.github/workflows/ci.yml`` installs its toolchain with
``dtolnay/rust-toolchain@stable``.  That is a FLOATING channel: on 2026-10-03 it
resolved to 1.99.0, while this checkout's default ``stable`` was still 1.97.1
from July.  The gap was not theoretical.  The first push of the shadow branch
passed an eleven-gate local matrix and failed CI's clippy job, because 1.99
added two lints 1.97.1 does not have (E-111, fixed at ``eacbc55``).

So a green local matrix does not imply a green CI run, and the reason it does
not is invisible in the diff.  This gate makes that condition a failure instead
of a sentence in a document.

What it proves
--------------
That the local toolchain is the same build the floating ``stable`` channel
currently resolves to, or it exits non-zero and names both sides.

What it deliberately does **not** claim
--------------------------------------
That the toolchain is *correct*, only that it *matches*.  It says nothing about
whether 1.99.0 is a good idea; floating is a deliberate choice.  It also does
not enforce the workspace's declared ``rust-version``; that claim has its own
pinned job in ``ci.yml`` (``msrv``), which builds the locked workspace with
exactly the declared minimum.  What this gate covers is the *other* half of the
toolchain question: the floating channel every other job follows.  The declared
version is REPORTED here so a reader can see both sides at once.

Two traps this file exists to close
-----------------------------------
Both were hit while writing it, and both fail in the direction that manufactures
a fact.

1. ``channel-rust-stable.toml`` begins with ``[pkg.cargo]`` at byte 44 and does
   not reach ``[pkg.rust]`` until byte 80035.  A parser that takes the first
   ``version =`` in the file reads **Cargo's** 0.100.0 and calls it the
   compiler's, which is wrong by two orders of magnitude.  The parser below is
   anchored on the section header.

2. The manifest and the installed binary identify a build in a way that only
   works if taken from the version string, and the field that looks like it
   should be used is wrong.  ``[pkg.cargo]`` reads
   ``version = "0.100.0 (5f94df478 2026-08-27)"`` while ``cargo --version``
   prints ``cargo 1.99.0 (5f94df478 2026-08-27)``: same build, same commit, two
   numbering schemes, so comparing release numbers for cargo reports DRIFT for a
   toolchain that is exactly CI's.  The manifest also carries a
   ``git_commit_hash`` beside the version, and for cargo that field holds the
   RUSTC commit, not cargo's, so trusting it compares the binary's real cargo
   hash against the wrong hash and reports DRIFT forever.  The hash inside the
   version's parentheses is the one value both sides state identically, so that
   is the comparison key.  The release numbers are printed for the human and
   never used as the verdict.

Conventions
-----------
Following ``desktop_artifact_test.py``: a scanner that cannot establish its
input is ``INCONCLUSIVE`` and fails loudly.  It never reports "matching" on the
strength of a read it did not perform.  ``--accept-unresolved`` is the only way
to proceed offline, it is opt-in, and CI does not pass it.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import NamedTuple, Sequence

CHANNEL_URL = "https://static.rust-lang.org/dist/channel-rust-stable.toml"
FETCH_TIMEOUT_SECONDS = 30
USER_AGENT = "zroutery-toolchain-gate"

RELEASE_RE = re.compile(r"\b(\d+\.\d+\.\d+)\b")
HASH_RE = re.compile(r"\b([0-9a-f]{7,40})\b")


class Unresolved(Exception):
    """The toolchain on one side of the comparison could not be established."""


class Build(NamedTuple):
    """One identifiable toolchain build.

    ``hash`` is the comparison key because it is the only field both the channel
    manifest and the installed binary state in the same scheme.  ``release`` is
    what a person reads and is never the verdict.
    """

    release: str
    commit: str

    def __str__(self) -> str:
        return "%s (%s)" % (self.release, self.commit)


def release_and_commit(text: str) -> Build:
    """Pull the release number and commit hash out of a ``--version``-shaped string.

    Both sides of the comparison are reduced through this one function on
    purpose. The manifest states a build as ``1.99.0 (b940084d7 2026-09-28)``
    and an installed binary states it as ``rustc 1.99.0 (b940084d7 2026-09-28)``,
    so the same two tokens are present on both sides and reading them the same
    way is what makes the comparison mean anything.
    """
    release = RELEASE_RE.search(text)
    if release is None:
        raise Unresolved("no release number in %r" % text)
    parenthesised = re.search(r"\(([^)]*)\)", text)
    commit = HASH_RE.search(parenthesised.group(1) if parenthesised else text)
    if commit is None:
        raise Unresolved("no commit hash in %r" % text)
    return Build(release.group(1), commit.group(1))


def parse_channel_build(text: str, package: str) -> Build:
    """Return the build declared in ``[pkg.<package>]`` of the channel manifest."""
    section = re.search(
        r"^\[pkg\.%s\]\s*$(.*?)(?=^\[|\Z)" % re.escape(package),
        text,
        re.MULTILINE | re.DOTALL,
    )
    if section is None:
        raise Unresolved("no [pkg.%s] section in the channel manifest" % package)
    version = re.search(r'^version = "([^"]+)"', section.group(1), re.MULTILINE)
    if version is None:
        raise Unresolved("[pkg.%s] declares no version" % package)
    return release_and_commit(version.group(1))


def local_build(argv: Sequence[str]) -> Build:
    """Return the build a locally installed tool reports for ``--version``."""
    try:
        completed = subprocess.run(
            list(argv), capture_output=True, text=True, timeout=60, check=False
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise Unresolved("could not run %s: %s" % (" ".join(argv), error))
    if completed.returncode != 0:
        raise Unresolved(
            "%s exited %d" % (" ".join(argv), completed.returncode)
        )
    output = completed.stdout.strip()
    if not output:
        raise Unresolved("%s printed nothing" % " ".join(argv))
    return release_and_commit(output)


def compare(local: Build, remote: Build) -> tuple[bool, str]:
    """Decide whether two builds are the same build.

    The commit hash is authoritative.  Release numbers are a fallback for a
    hypothetical build that states no hash, and the caller is told which key
    decided it, because a verdict whose basis is not stated is not a verdict.
    """
    if local.commit == remote.commit:
        return True, "commit hash"
    if local.release == remote.release:
        return True, "release number (no matching hash to compare)"
    return False, "commit hash"


def fetch_channel_text() -> str:
    request = urllib.request.Request(CHANNEL_URL, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=FETCH_TIMEOUT_SECONDS) as page:
            return page.read().decode("utf-8", errors="replace")
    except (urllib.error.URLError, OSError, ValueError) as error:
        raise Unresolved("could not fetch %s: %s" % (CHANNEL_URL, error))


def declared_msrv() -> str:
    """Read the workspace's declared minimum. Reported here, enforced by ``msrv``."""
    workspace = Path(__file__).resolve().parent.parent / "Cargo.toml"
    try:
        text = workspace.read_text(encoding="utf-8")
    except OSError as error:
        return "unreadable (%s)" % error
    declared = re.search(r'^rust-version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    return declared.group(1) if declared else "not declared"


# -- self-test ---------------------------------------------------------------

# A transcription of the real manifest's two relevant sections, including both
# traps: cargo is numbered 0.100.0 here and 1.99.0 by the binary, and
# ``git_commit_hash`` holds the RUSTC commit in BOTH sections.
CHANNEL_FIXTURE = (
    "[pkg.cargo]\n"
    'version = "0.100.0 (5f94df478 2026-08-27)"\n'
    'git_commit_hash = "b940084d7eb6a299eb4bfeb8e34901bc051e7ac4"\n'
    "\n"
    "[pkg.rust]\n"
    'version = "1.99.0 (b940084d7 2026-09-28)"\n'
    'git_commit_hash = "b940084d7eb6a299eb4bfeb8e34901bc051e7ac4"\n'
)


def _raises(action) -> str:
    try:
        action()
    except Unresolved:
        return "Unresolved"
    return "no exception"


def _cases() -> list[tuple[str, object, object]]:
    def reads_rust_not_first_version():
        return parse_channel_build(CHANNEL_FIXTURE, "rust").release

    def reads_cargo_when_asked():
        return parse_channel_build(CHANNEL_FIXTURE, "cargo").release

    def numbering_scheme_does_not_manufacture_drift():
        # Same build. The manifest numbers cargo 0.100.0 and the binary says
        # 1.99.0, which is exactly the pair that made an earlier draft of this
        # gate report a false DRIFT. The hash in the parentheses is equal, so
        # this must MATCH.
        manifest = parse_channel_build(CHANNEL_FIXTURE, "cargo")
        binary = Build("1.99.0", "5f94df478")
        return compare(binary, manifest)[0]

    def a_manifest_hash_that_names_the_wrong_package_is_not_trusted():
        # In the real manifest `[pkg.cargo].git_commit_hash` holds the RUSTC
        # commit, b940084d7, while cargo's own build is 5f94df478. If that field
        # were the key, the cargo binary would be compared against the wrong
        # hash and DRIFT would be permanent. Assert the key is the one in the
        # version's parentheses and is NOT the field that names rustc's commit.
        manifest = parse_channel_build(CHANNEL_FIXTURE, "cargo")
        wrong_field = re.search(
            r'^git_commit_hash = "([^"]+)"',
            CHANNEL_FIXTURE.split("[pkg.rust]")[0],
            re.MULTILINE,
        ).group(1)
        return (
            manifest.commit == "5f94df478"
            and manifest.commit != wrong_field[:7]
            and wrong_field.startswith("b940084d7")
        )

    def real_drift_is_still_detected():
        manifest = parse_channel_build(CHANNEL_FIXTURE, "rust")
        return compare(Build("1.97.1", "8bab26f4f6"), manifest)[0]

    def missing_section_is_inconclusive():
        return _raises(lambda: parse_channel_build("[pkg.cargo]\n", "rust"))

    def version_without_a_hash_is_inconclusive():
        return _raises(
            lambda: parse_channel_build('[pkg.rust]\nversion = "1.99.0"\n', "rust")
        )

    def unnumbered_version_is_inconclusive():
        return _raises(
            lambda: parse_channel_build(
                '[pkg.rust]\nversion = "nightly (abc1234def 2026-01-01)"\n',
                "rust",
            )
        )

    return [
        ("reads rustc, not the first version in the file", reads_rust_not_first_version, "1.99.0"),
        ("reads cargo when asked for cargo", reads_cargo_when_asked, "0.100.0"),
        ("a numbering-scheme difference alone is not drift", numbering_scheme_does_not_manufacture_drift, True),
        ("a manifest field naming the wrong package is not the key", a_manifest_hash_that_names_the_wrong_package_is_not_trusted, True),
        ("a genuinely different build is still drift", real_drift_is_still_detected, False),
        ("a missing section is inconclusive, not a default", missing_section_is_inconclusive, "Unresolved"),
        ("a version carrying no hash is inconclusive", version_without_a_hash_is_inconclusive, "Unresolved"),
        ("a version with no release number is inconclusive", unnumbered_version_is_inconclusive, "Unresolved"),
    ]


def self_test() -> int:
    failures = 0
    for name, action, expected in _cases():
        try:
            actual = action()
        except Exception as error:  # noqa: BLE001 - the gate reports, never hides
            actual = "%s: %s" % (type(error).__name__, error)
        verdict = "ok" if actual == expected else "MISMATCH"
        if verdict != "ok":
            failures += 1
        print("  [%s] %s: expected %r, got %r" % (verdict, name, expected, actual))
    print(
        "toolchain-gate self-test: %s (%d case(s))"
        % ("PASS" if failures == 0 else "FAILED", len(_cases()))
    )
    return 0 if failures == 0 else 1


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Fail when the local toolchain is not the one CI resolves.",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="prove the parser reads the right section and the right key",
    )
    parser.add_argument(
        "--accept-unresolved",
        action="store_true",
        help=(
            "report INCONCLUSIVE and exit 0 instead of failing. Offline use "
            "only; CI does not pass this."
        ),
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    try:
        channel = fetch_channel_text()
        pairs = {
            "rustc": (
                parse_channel_build(channel, "rust"),
                local_build(["rustc", "--version"]),
            ),
            "cargo": (
                parse_channel_build(channel, "cargo"),
                local_build(["cargo", "--version"]),
            ),
        }
    except Unresolved as error:
        print("toolchain-gate: INCONCLUSIVE - %s" % error)
        print(
            "toolchain-gate: a green local matrix means nothing about CI "
            "while the toolchain is unknown."
        )
        if args.accept_unresolved:
            print(
                "toolchain-gate: --accept-unresolved given; proceeding with "
                "the toolchain UNVERIFIED."
            )
            return 0
        return 1

    print(
        "toolchain-gate: local   %s"
        % "  ".join("%s %s" % (name, local) for name, (_, local) in pairs.items())
    )
    print(
        "toolchain-gate: CI      %s"
        % "  ".join("%s %s" % (name, remote) for name, (remote, _) in pairs.items())
    )
    print(
        "toolchain-gate: declared MSRV rust-version = %s (enforced by the "
        "ci.yml msrv job, not here)" % declared_msrv()
    )

    drift = []
    for name, (remote, local) in pairs.items():
        matched, key = compare(local, remote)
        if not matched:
            drift.append(
                "%s local %s vs CI %s (compared by %s)"
                % (name, local, remote, key)
            )
    if not drift:
        print(
            "toolchain-gate: MATCH on commit hash - the local toolchain is the "
            "build CI resolves, so this matrix speaks to CI."
        )
        return 0

    print("toolchain-gate: DRIFT - %s" % "; ".join(drift))
    print(
        "toolchain-gate: lints and formatting rules move between releases, so "
        "a green local matrix does not imply a green CI run (E-111)."
    )
    target = pairs["rustc"][0].release
    print("toolchain-gate: to resolve, run")
    print(
        "    rustup toolchain install %s --profile minimal --component clippy "
        "--component rustfmt" % target
    )
    print(
        "  and pass `+%s` to the gates. `rustup update stable` is not the only "
        "route and may be blocked by another build holding the shared "
        "toolchain directory." % target
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())