#!/usr/bin/env python3
"""RETIRED as a CI gate.  The scanner below is kept intact and still runs.

What this used to assert
------------------------

Node 7E-2F shipped a binary-level boundary: the shipped desktop executable had
to contain **none** of ~32 activation symbols and no evidence that the ``ml``
module was compiled in at all.  Its companion source-level gates asserted that
the serving path never called a training entry point and that ``router.rs`` must
not contain the string ``dataset``.

Why it is retired
-----------------

The claim was that the desktop app could not install or activate an ML model.
That was true, and true *because* the desktop app compiles no ML stack:
``src-tauri`` declares ``zroutery-core`` with no features.

It is retired rather than fixed because the claim it made was never the property
anyone needed protected.  A binary cannot route on a model it does not contain,
so "this binary contains no model" was a statement about packaging, not about
safety.  Meanwhile the prohibition it enforced is exactly what prevented
Zroutery from ever producing the evidence that would justify enabling ML, and the
companion source gates actively forbade the closed loop from ever reaching
production.

ADR-0006 reverses that prohibition.  The desktop application still compiles no
ML and ``ml_routing.enabled`` still defaults to off, so the shipped product's
behaviour is unchanged.  What changed is that ML is no longer *forbidden* from
the desktop path, and therefore the binary no longer has to be scanned to prove
its absence.

What replaced it
----------------

The properties that actually mattered are behavioural now, and are checked where
they can be observed rather than inferred from a string scan:

* a model serves only after ``ml::promotion::PromotionGate`` promotes it, and the
  promotion digest is stored beside it;
* any fault, missing model, or selection outside the executable plan falls back
  to the deterministic plan the router already computed;
* promotion and rollback are durable, audited, and rollback restores the model a
  promotion replaced;
* a model's influence on a served request is recorded on the routing decision
  itself, in ``RouteDecision::ml_ranking``.

Those live in ``crates/zroutery-core/tests/ml_closed_loop_test.rs`` and
``crates/zroutery-core/src/ml/serving.rs``.

If ML is ever enabled in the desktop package, the check to add is not "the
binary contains no model".  It is "a promoted model can be loaded, served,
observed and rolled back in the desktop process" — a runtime assertion, not a
string scan.

Deliberate behaviour of the retired entry point
-----------------------------------------------

``main()`` prints this notice and exits ``0`` for every invocation, including
``--exe``.  A retired gate must not be able to fail a build: left wired into CI
with a passing exit code it would falsely suggest the check still runs, and left
failing it would block every release on an assertion nobody believes any more.
``--self-test`` still genuinely exercises the scanner against its fixtures,
because a scanner that is never exercised rots, and this one may be revived.

The scanner implementation, the forbidden-symbol list and ``self_test`` are
otherwise unchanged, so a future decision to re-enable the check is a change to
``main()`` and nothing else.

Original scope of the scanner, kept for reference
-------------------------------------------------

``[profile.release]`` sets ``strip = true`` and ``lto = true``, so the ``.pdb``
symbol table is not part of the artifact and this scan never reads one.  A
missing *string* is therefore not a disassembly proof that a type was never
instantiated.  What a clean result did prove is concrete and worth stating
precisely: if ``src/ml/activation.rs`` had been compiled into the desktop
shell, then its ``assert!``/``panic!`` location strings, its ``Display`` and
``Debug`` format strings, and its type names would all be present in ``.rdata``.

That is why the scanner has a **positive control**.  A scanner that finds
nothing is worthless if the scanner is broken.  Every real scan must first
demonstrate that it can find strings which are certainly in the binary, or the
verdict is reported as ``INCONCLUSIVE``.  It never reports "clean" on the
strength of a scan it has not shown to work.

Usage
-----
    python -B scripts/desktop_artifact_test.py --self-test
    python -B scripts/desktop_artifact_test.py --exe src-tauri/target/release/zroutery.exe
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path
from typing import Iterable, Sequence

# The forbidden set, transcribed from
# crates/zroutery-core/tests/activation_test.rs::
# nothing_in_the_shipped_product_can_name_this_module.  Keep these in step; the
# cross-check in `_cross_check_against_rust_test` enforces that mechanically
# rather than trusting this comment.
FORBIDDEN_SYMBOLS: tuple[str, ...] = (
    "ml::activation",
    "activation::",
    "ActivationStore",
    "ActivationRequest",
    "ActivationOutcome",
    "ActivationError",
    "ActivationAudit",
    "ActivationPointer",
    "ActivationEntry",
    "ActivationTrace",
    "ActivationKind",
    "ActivationStage",
    "ActiveSnapshot",
    "SnapshotId",
    "SnapshotFile",
    "PointerFile",
    "PointerEntryFile",
    "PendingActivation",
    "PointerDisagreement",
    "ACTIVATION_POINTER_NAME",
    "ACTIVATION_ROLE",
    "ACTIVATION_SNAPSHOT_SCHEMA_VERSION",
    "POINTER_TMP_NAME",
    "SNAPSHOTS_DIR_NAME",
    "SNAPSHOT_FILE_SUFFIX",
    "ACTIVATION_LOCK_NAME",
    "JOURNAL_DIR_NAME",
    "activation_plan_event_id",
    "activation_applied_event_id",
    "snapshot_id_for",
    "snapshot_checksum",
    "pointer_checksum",
)

# Names specific to the activation module.  A generic word like "activation" is
# deliberately absent: the decision-contract code uses the word for a
# mathematical activation function, and a scan that flagged those would be
# measuring the wrong thing.
#
# The second group is evidence that the ml module was *compiled in at all*,
# which is a strictly stronger statement than "no activation symbol appears".
# Rust embeds `file!()` paths as string literals, and the ml module's whole
# surface lives behind `#[cfg(feature = "ml")]`, so if the desktop shell had
# pulled the feature in, these would be present.
ML_MODULE_EVIDENCE: tuple[str, ...] = (
    "src/ml/",
    "ml/activation.rs",
    "zroutery_core::ml",
    "zroutery_core::ml::activation",
    "zroutery_core::ml::activation::ActivationStore",
    "zroutery_core::ml::journal",
    "zroutery_core::ml::model_identity",
    "zroutery_core::ml::dataset",
)

# Strings that are certainly in *this* binary, whatever the build flags, read
# from `src-tauri/tauri.conf.json` at run time rather than hardcoded.
#
# `tauri-codegen` generates a `tauri::utils::config::Config { ... }` struct
# literal into the crate, so every string field it keeps becomes a string
# literal in `.rdata`.  `identifier` and `security.csp` are both among them,
# and both are load-bearing at run time: the identifier names the IPC scheme
# and the user's data directory, and the CSP is applied to every served frame.
#
# Note what is deliberately *not* here: `shortDescription`/`longDescription`.
# Those are bundle metadata consumed by the bundler CLI, not by the running
# app, and they are not embedded.  An earlier draft of this file asserted
# them and the positive control correctly refused the run; that is the control
# doing its job, and it is why the strings are sourced rather than guessed.
REQUIRED_CONFIG_FIELDS: tuple[tuple[str, ...], ...] = (
    ("identifier",),
    ("app", "security", "csp"),
)

# The canary appended to a copy of the artifact to prove the scanner works on
# these exact bytes.  Chosen to be absent from the real image (asserted below)
# and unlikely to be generated by anything else.
SCAN_CANARY = "ZROUTERY-ARTIFACT-SCAN-CANARY-8f3a1c97"

# Reported, but not gating.  Useful evidence that the image is a real Tauri app
# rather than something else entirely.
INFORMATIONAL_CONTROL_STRINGS: tuple[str, ...] = (
    "Zroutery",
    "http://ipc.localhost",
    "WebView2",
)

PE_SIGNATURE_OFFSET = 0x3C
PE_SIGNATURE = b"PE\x00\x00"
IMAGE_FILE_MACHINE_AMD64 = 0x8664
IMAGE_FILE_MACHINE_I386 = 0x014C


class GateFailure(Exception):
    """A gate condition that did not hold.  Always fatal, never a warning."""


# ---------------------------------------------------------------------------
# PE inspection
# ---------------------------------------------------------------------------


class PeImage:
    """A read-only view of a Windows executable, parsed far enough to be sure
    we are holding one.

    Parsing is deliberately explicit rather than delegated: a scanner that
    silently accepted a non-PE file could report a clean result for a
    truncated read or an empty placeholder, which is exactly the vacuous pass
    this gate exists to avoid.
    """

    def __init__(self, path: Path, data: bytes) -> None:
        self.path = path
        self.data = data

    @classmethod
    def read(cls, path: Path) -> "PeImage":
        try:
            data = path.read_bytes()
        except FileNotFoundError as error:
            raise GateFailure(f"artifact does not exist: {path}") from error
        except OSError as error:
            raise GateFailure(f"artifact is not readable: {path}: {error}") from error

        if not data:
            raise GateFailure(f"artifact is empty: {path}")
        if len(data) < 0x40:
            raise GateFailure(
                f"artifact is too small to be a PE image ({len(data)} bytes): {path}"
            )
        if data[:2] != b"MZ":
            raise GateFailure(
                f"artifact does not start with the MZ DOS header: {path}"
            )
        return cls(path, data)

    def machine(self) -> int:
        offset = struct.unpack_from("<I", self.data, PE_SIGNATURE_OFFSET)[0]
        if offset + len(PE_SIGNATURE) > len(self.data):
            raise GateFailure(
                f"artifact's PE header offset {offset} is past the end of the "
                f"{len(self.data)}-byte file: {self.path}"
            )
        if self.data[offset : offset + len(PE_SIGNATURE)] != PE_SIGNATURE:
            raise GateFailure(
                f"artifact has no PE signature at offset {offset}: {self.path}"
            )
        return struct.unpack_from("<H", self.data, offset + 4)[0]

    def describe(self) -> str:
        machine = self.machine()
        if machine == IMAGE_FILE_MACHINE_AMD64:
            arch = "x86-64"
        elif machine == IMAGE_FILE_MACHINE_I386:
            arch = "x86"
        else:
            raise GateFailure(
                f"artifact targets machine 0x{machine:04X}, which is neither "
                f"x86-64 nor i386: {self.path}"
            )
        return f"{len(self.data):,} bytes, {arch} PE image"

    def contains(self, needle: str) -> bool:
        """True when *needle* occurs as ASCII or as UTF-16LE.

        Both encodings are checked because a Windows binary may hold its
        strings either way: Rust literals land as UTF-8 in `.rdata`, while
        anything drawn from a wide-character resource or registry path can
        appear as UTF-16LE.
        """
        ascii_form = needle.encode("ascii", errors="ignore")
        if ascii_form and ascii_form in self.data:
            return True
        utf16_form = needle.encode("utf-16-le")
        return utf16_form in self.data

    def offsets_of(self, needle: str) -> list[int]:
        """Every byte offset at which *needle* occurs, for reporting."""
        found: list[int] = []
        for encoding in ("ascii", "utf-16-le"):
            form = needle.encode(encoding, errors="ignore")
            if not form:
                continue
            start = 0
            while True:
                index = self.data.find(form, start)
                if index < 0:
                    break
                found.append(index)
                start = index + 1
        return found


# ---------------------------------------------------------------------------
# The scan
# ---------------------------------------------------------------------------


def scan(image: PeImage, needles: Sequence[str]) -> list[tuple[str, list[int]]]:
    """Return the subset of *needles* that occur in *image*, with offsets."""
    return [
        (needle, offsets)
        for needle in needles
        if (offsets := image.offsets_of(needle))
    ]


def _config_value(config: dict, path: tuple[str, ...]) -> str:
    node: object = config
    for key in path:
        if not isinstance(node, dict) or key not in node:
            raise GateFailure(
                "the control string "
                + ".".join(path)
                + " is absent from tauri.conf.json, so this gate can no longer "
                "prove its own scanner works"
            )
        node = node[key]
    if not isinstance(node, str) or not node:
        raise GateFailure(
            "the control string "
            + ".".join(path)
            + " in tauri.conf.json is not a non-empty string"
        )
    return node


def load_control_strings(root: Path) -> list[tuple[str, str]]:
    """The (label, value) pairs the artifact must contain to certify a scan.

    Read from the committed Tauri config rather than hardcoded, so the control
    cannot silently rot away from the product it is checking.
    """
    import json

    config_path = root / "src-tauri" / "tauri.conf.json"
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise GateFailure(f"cannot read {config_path}") from error
    except json.JSONDecodeError as error:
        raise GateFailure(f"{config_path} is not valid JSON: {error}") from error
    return [
        (".".join(path), _config_value(config, path))
        for path in REQUIRED_CONFIG_FIELDS
    ]


def run_positive_control(image: PeImage, controls: Sequence[tuple[str, str]]) -> tuple[bool, list[str]]:
    """Prove the scanner works on *this* file before trusting a negative.

    Two independent demonstrations, both of which must hold:

    1. **Canary.** The scan is re-run against a copy of this exact artifact
       with :data:`SCAN_CANARY` appended.  Finding it proves the scanner
       really is searching the bytes of the file under test, rather than
       reporting a confident negative because of a truncated read, a wrong
       offset, or an encoding mistake.  This half cannot be satisfied by a
       stale copy of the file.
    2. **Product strings.** Strings that `tauri-codegen` is structurally
       obliged to embed are located in the unmodified image, proving the
       image is the artifact this gate was written for.

    Returns ``(held, problems)``.  ``held is False`` means *inconclusive*, and
    is never reported as clean.
    """
    problems: list[str] = []

    if image.contains(SCAN_CANARY):
        problems.append(
            f"the scan canary {SCAN_CANARY!r} is already present in the real "
            "image, so it cannot distinguish a working scan from a coincidence"
        )
    else:
        probe = PeImage(
            image.path,
            image.data + SCAN_CANARY.encode("ascii") + b"\x00",
        )
        if not probe.contains(SCAN_CANARY):
            problems.append(
                "the scanner failed to find the canary appended to a copy of "
                "this artifact, so it cannot be trusted to find anything at all"
            )

    for label, value in controls:
        if not image.contains(value):
            problems.append(
                f"the embedded config string {label}={value!r} is not in the image"
            )
    return (not problems), problems


def cross_check_against_rust_test(root: Path) -> list[str]:
    """Confirm the forbidden list here still matches the Rust boundary test.

    If 7E-2F's list grows or shrinks, this gate would otherwise keep reporting
    on a stale set.  Returning a non-empty list means the transcription needs
    updating, which is a hard failure: a gate that silently audits the wrong
    symbols is worse than no gate.
    """
    test_path = (
        root
        / "crates"
        / "zroutery-core"
        / "tests"
        / "activation_test.rs"
    )
    if not test_path.is_file():
        return [f"cannot cross-check the symbol list: {test_path} is missing"]

    text = test_path.read_text(encoding="utf-8", errors="replace")
    marker = "fn nothing_in_the_shipped_product_can_name_this_module()"
    start = text.find(marker)
    if start < 0:
        return [f"cannot find {marker} in {test_path}"]

    # Take only the `let symbols = [ ... ];` block that follows. Scanning a
    # fixed-size window instead would sweep up the test's own assertion
    # messages and report them as symbols.
    declaration = text.find("let symbols = [", start)
    if declaration < 0:
        return [f"cannot find the `let symbols = [...]` list in {test_path}"]
    end = text.find("];", declaration)
    if end < 0:
        return [f"the `let symbols = [...]` list in {test_path} is not terminated"]

    listed = {
        line.strip().rstrip(",").strip('"')
        for line in text[declaration:end].splitlines()
        if line.strip().startswith('"') and line.strip().rstrip(",").strip('"')
    }
    if not listed:
        return [f"the `let symbols = [...]` list in {test_path} parsed as empty"]
    expected = set(FORBIDDEN_SYMBOLS)
    drifted = sorted(expected.symmetric_difference(listed))
    if drifted:
        return [
            "the forbidden symbol list has drifted from "
            f"{test_path}: {' '.join(drifted)}"
        ]
    return []


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


def _format_hits(hits: Iterable[tuple[str, list[int]]]) -> str:
    return "\n".join(
        f"      {needle}  at byte offsets "
        + ", ".join(f"0x{offset:X}" for offset in offsets[:6])
        + (" ..." if len(offsets) > 6 else "")
        for needle, offsets in hits
    )


def report(
    image: PeImage,
    control_held: bool,
    control_problems: Sequence[str],
    controls: Sequence[tuple[str, str]],
    forbidden_hits: Sequence[tuple[str, list[int]]],
    evidence_hits: Sequence[tuple[str, list[int]]],
    informational: Sequence[tuple[str, list[int]]],
) -> None:
    print(f"desktop-artifact: {image.path}")
    print(f"  image:    {image.describe()}")
    print(f"  scanned:  {len(FORBIDDEN_SYMBOLS)} activation symbols, "
          f"{len(ML_MODULE_EVIDENCE)} ml-module markers, ASCII and UTF-16LE")

    print("  positive control (must hold for any negative to mean anything):")
    print(f"      {'found' if not any('canary' in p for p in control_problems) else 'FAILED':<8}"
          f" scan canary, appended to a copy of this artifact")
    for label, value in controls:
        state = "found" if image.contains(value) else "MISSING"
        print(f"      {state:<8} {label} = {value[:60]!r}")
    for needle, _ in informational:
        state = "found" if image.contains(needle) else "absent"
        print(f"      {state:<8} {needle!r} (informational)")

    print("  forbidden activation symbols in the image:")
    if forbidden_hits:
        print(_format_hits(forbidden_hits))
    else:
        print("      none")

    print("  ml-module evidence in the image:")
    if evidence_hits:
        print(_format_hits(evidence_hits))
    else:
        print("      none")

    print()
    if not control_held:
        print("desktop-artifact: INCONCLUSIVE - the positive control did not "
              "hold, so the negative results above prove nothing.")
        for problem in control_problems:
            print(f"  {problem}")
        return
    if forbidden_hits or evidence_hits:
        print("desktop-artifact: FAILED - the shipped desktop binary names the "
              "activation module or carries evidence that the ml module was "
              "compiled in.")
        return
    print("desktop-artifact: PASSED")
    print("  The built artifact contains none of the 32 forbidden activation "
          "symbols and none of the ml-module markers, in ASCII or UTF-16LE,")
    print("  while the scanner demonstrably located the control strings in the "
          "same file.")
    print("  This is string-level evidence about the shipped image. The release")
    print("  profile sets strip = true, so no .pdb symbol table was consulted; "
          "the claim is that no")
    print("  location, format, Display or type-name literal from the ml module "
          "survived into the binary.")


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------


def _synthetic_pe(payload: bytes) -> bytes:
    """A byte-for-byte well-formed minimal x86-64 PE header around *payload*."""
    header = bytearray(0x200)
    header[0:2] = b"MZ"
    struct.pack_into("<I", header, PE_SIGNATURE_OFFSET, 0x80)
    header[0x80:0x84] = PE_SIGNATURE
    struct.pack_into("<H", header, 0x84, IMAGE_FILE_MACHINE_AMD64)
    return bytes(header) + payload


def self_test() -> int:
    """Exercise the scanner against fixtures whose answer is known.

    A gate that has never been shown to fail is not a gate.  Each fixture below
    is built here in memory, so this runs anywhere with no artifact present.
    """
    failures: list[str] = []

    def check(label: str, condition: bool) -> None:
        if not condition:
            failures.append(label)

    # A clean image that still carries the control strings.
    control_values = [("identifier", "app.zroutery.desktop")]
    clean = PeImage(
        Path("<self-test:clean>"),
        _synthetic_pe(b"app.zroutery.desktop\x00Zroutery\x00"),
    )
    held, problems = run_positive_control(clean, control_values)
    check("positive control must hold on a clean fixture", held)
    check("no control problem may be reported on a clean fixture", not problems)
    check("clean fixture must have no forbidden symbols", not scan(clean, FORBIDDEN_SYMBOLS))
    check("clean fixture must have no ml evidence", not scan(clean, ML_MODULE_EVIDENCE))

    # The control must FAIL when the product string is absent. This is the
    # fixture that caught the original draft's bad assumption about
    # shortDescription, so it earns its place.
    without = PeImage(Path("<self-test:no-control>"), _synthetic_pe(b"Zroutery\x00"))
    held, problems = run_positive_control(without, control_values)
    check("positive control must fail without the product string", not held)
    check("the missing product string must be reported", bool(problems))

    # One forbidden symbol, ASCII.
    ascii_bad = PeImage(
        Path("<self-test:ascii>"),
        _synthetic_pe(
            b"app.zroutery.desktop\x00Zroutery\x00"
            b"panic\x00in activation::ActivationStore\x00"
        ),
    )
    ascii_hits = [needle for needle, _ in scan(ascii_bad, FORBIDDEN_SYMBOLS)]
    check(
        "an ASCII activation symbol must be detected",
        "ActivationStore" in ascii_hits,
    )
    check(
        "the ASCII hit must be located",
        bool(ascii_bad.offsets_of("ActivationStore")),
    )

    # One forbidden symbol, UTF-16LE: the encoding Windows uses for wide
    # strings, which a naive ASCII-only scan would miss entirely.
    utf16_bad = PeImage(
        Path("<self-test:utf16>"),
        _synthetic_pe(
            b"app.zroutery.desktop\x00Zroutery\x00"
            + "ActivationStore".encode("utf-16-le")
        ),
    )
    utf16_hits = [needle for needle, _ in scan(utf16_bad, FORBIDDEN_SYMBOLS)]
    check(
        "a UTF-16LE activation symbol must be detected",
        "ActivationStore" in utf16_hits,
    )

    # ml-module evidence with none of the 32 symbols present at all, so the
    # two checks cannot be confused for one another.
    evidence_bad = PeImage(
        Path("<self-test:evidence>"),
        _synthetic_pe(
            b"app.zroutery.desktop\x00Zroutery\x00"
            b"crates/zroutery-core/src/ml/activation.rs\x00"
        ),
    )
    check("ml fixture must have no forbidden symbols", not scan(evidence_bad, FORBIDDEN_SYMBOLS))
    check(
        "ml source-path evidence must be detected",
        bool(scan(evidence_bad, ML_MODULE_EVIDENCE)),
    )

    # An image that is not a PE at all must be refused, not scanned.
    for label, blob in (
        ("empty", b""),
        ("text", b"this is not a binary at all"),
        ("short", b"MZ"),
        ("no pe signature", bytearray(b"MZ" + b"\x00" * 0x3E)),
    ):
        try:
            PeImage(Path(f"<self-test:{label}>"), blob).machine()
        except (GateFailure, struct.error, IndexError):
            continue
        failures.append(f"{label} fixture must be refused as a PE image")

    # A PE header pointing past the end of the file.
    truncated = bytearray(_synthetic_pe(b"payload"))
    struct.pack_into("<I", truncated, PE_SIGNATURE_OFFSET, 0x7FFFFFFF)
    try:
        PeImage(Path("<self-test:truncated>"), bytes(truncated)).machine()
    except (GateFailure, struct.error, IndexError):
        pass
    else:
        failures.append("a PE offset past the end of the file must be refused")

    for failure in failures:
        print(f"desktop-artifact self-test FAILED: {failure}")
    if failures:
        print("desktop-artifact self-test: FAILED")
        return 1
    print("desktop-artifact self-test: PASSED")
    print("  The scanner detects a forbidden symbol in ASCII and in UTF-16LE,")
    print("  detects ml-module evidence independently of the symbol list, and")
    print("  refuses empty, non-PE and out-of-range inputs.")
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def default_artifact(root: Path) -> Path:
    return root / "src-tauri" / "target" / "release" / "zroutery.exe"


def main(argv: Sequence[str] | None = None) -> int:
    """Retired entry point.

    Reports the retirement and succeeds.  The scanner itself is untouched and
    ``self_test`` still exercises it, so a future decision to re-enable the
    check is a change to this function and nothing else.
    """
    parser = argparse.ArgumentParser(
        description="RETIRED: the desktop binary is no longer required to be ML-free."
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="exercise the scanner against in-memory fixtures and exit",
    )
    parser.add_argument(
        "--exe",
        type=Path,
        default=None,
        help="accepted and ignored; the scan is not performed",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="accepted and ignored",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    print("desktop-artifact: RETIRED - reporting and exiting 0.")
    print()
    print("  This gate used to require the shipped desktop executable to contain")
    print("  no ML symbol at all. That was a statement about packaging: a binary")
    print("  cannot route on a model it does not contain. It is retired by")
    print("  ADR-0006, which reverses the prohibition that made it true.")
    print()
    print("  Unchanged: the desktop app still compiles no ML, and")
    print("  ml_routing.enabled still defaults to off. Product behaviour is the")
    print("  same. What changed is that ML is no longer forbidden from the")
    print("  desktop path, so the binary no longer has to be scanned to prove")
    print("  its absence.")
    print()
    print("  Replaced by behavioural checks:")
    print("    crates/zroutery-core/tests/ml_closed_loop_test.rs")
    print("    crates/zroutery-core/src/ml/serving.rs")
    print()
    print("  Run with --self-test to still exercise the scanner's fixtures.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
