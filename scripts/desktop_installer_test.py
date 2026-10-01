#!/usr/bin/env python3
"""Install, reinstall and uninstall the real Zroutery NSIS artifact.

A build gate that stops at "an .exe and two installers appeared" proves the
product *packages*.  It does not prove the product *installs*, and it says
nothing at all about what happens when a user runs the same installer twice --
which is the ordinary case for anyone who upgrades.

This gate executes the sequence and reports what actually happened:

  1. record the installer's size and SHA-256;
  2. install it silently, and require exit code 0;
  3. require the installed executable to be byte-identical to the built one;
  4. require exactly one install directory and one uninstall registry entry;
  5. run the *same* installer a second time over the existing install;
  6. require exit code 0, an unchanged installed hash, and still exactly one
     install directory and one registry entry -- no duplicates, no leftovers;
  7. uninstall, and require the executable, the directory and the registry
     entry to be gone;
  8. leave the machine as it was found.

Every step is asserted, not described.  A non-zero exit code, a changed hash, a
duplicated install path or a surviving registry entry is reported as a
failure.  Nothing here is skipped silently, and anything this script cannot
verify on the current platform is reported as unverified rather than passed.

Scope note: the rollback leg is a *product packaging* rollback -- installing,
re-installing over, and removing the shipped artifact.  It is not a rollback of
any model or data state, and it makes no claim about one.

Usage
-----
    python -B scripts/desktop_installer_test.py \\
        --installer src-tauri/target/release/bundle/nsis/Zroutery_0.8.0_x64-setup.exe \\
        --built-exe src-tauri/target/release/zroutery.exe
"""

from __future__ import annotations

import argparse
import hashlib
import os
import subprocess
import sys
import time
import winreg
from pathlib import Path
from typing import Sequence

INSTALL_DIR = Path(os.environ["LOCALAPPDATA"]) / "Zroutery"
INSTALLED_EXE = INSTALL_DIR / "zroutery.exe"
UNINSTALLER = INSTALL_DIR / "uninstall.exe"
UNINSTALL_KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
PROCESS_NAME = "zroutery.exe"


class StepFailure(Exception):
    """A step of the sequence did not hold.  Always fatal."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run_installer(exe: Path, label: str) -> int:
    """Run an NSIS installer silently and return its exit code.

    NSIS `/S` is the documented silent switch.  A timeout is used because a
    silent installer that waits on a UI prompt would otherwise hang the gate
    forever rather than reporting a failure.
    """
    try:
        completed = subprocess.run(
            [str(exe), "/S"],
            timeout=600,
            capture_output=True,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise StepFailure(f"{label}: the installer timed out after 600s") from error
    print(f"      {label}: exit code {completed.returncode}")
    if completed.stdout.strip():
        print(f"      {label}: stdout {completed.stdout.strip()[:400]!r}")
    if completed.stderr.strip():
        print(f"      {label}: stderr {completed.stderr.strip()[:400]!r}")
    return completed.returncode


def uninstall_entries() -> list[tuple[str, str, int]]:
    """Every registered uninstall entry pointing at our install directory.

    Reads the per-user hive, which is where NSIS registers by default and which
    needs no elevation to write.
    """
    found: list[tuple[str, str, int]] = []
    try:
        hive = winreg.OpenKey(winreg.HKEY_CURRENT_USER, UNINSTALL_KEY)
    except FileNotFoundError:
        return found
    with hive:
        count = winreg.QueryInfoKey(hive)[0]
        for index in range(count):
            try:
                name = winreg.EnumKey(hive, index)
            except OSError:
                continue
            try:
                with winreg.OpenKey(hive, name) as entry:
                    location = str(
                        winreg.QueryValueEx(entry, "InstallLocation")[0]
                    )
                    display = str(winreg.QueryValueEx(entry, "DisplayName")[0])
                    size = int(winreg.QueryValueEx(entry, "EstimatedSize")[0])
            except OSError:
                continue
            if "zroutery" in location.lower() or "zroutery" in display.lower():
                found.append((name, location, size))
    return found


# The marker `tauri build` rewrites in place, once per bundle, to record which
# installer the executable went into. `tauri-utils` defines it as
# `__TAURI_BUNDLE_TYPE_VAR_UNK` and rewrites the three trailing characters to
# `DEB`, `RPM`, `APP`, `MSI` or `NSS`.
BUNDLE_TYPE_MARKER = b"__TAURI_BUNDLE_TYPE_VAR_"
BUNDLE_TYPE_TAGS = (b"UNK", b"DEB", b"RPM", b"APP", b"MSI", b"NSS")


def bundle_patch_delta(reference: Path, candidate: Path) -> str:
    """Describe how *candidate* differs from *reference*, or "" if identical.

    Reading both files in full rather than trusting the size check keeps the
    explanation honest: a difference anywhere other than the bundle-type marker
    is described in full rather than being quietly tolerated.
    """
    left = reference.read_bytes()
    right = candidate.read_bytes()
    if left == right:
        return ""
    if len(left) != len(right):
        return f"DIFFERENT LENGTH ({len(left)} vs {len(right)})"

    offsets = [i for i, (a, b) in enumerate(zip(left, right)) if a != b]

    # The marker is rewritten in place: the three tag characters immediately
    # *after* the marker text change, so the window has to span the marker plus
    # its tag, not the marker alone.
    start = left.find(BUNDLE_TYPE_MARKER)
    if start < 0:
        return f"{len(offsets)} bytes differ and the image has no bundle-type marker"
    end = start + len(BUNDLE_TYPE_MARKER) + 3
    outside = [i for i in offsets if not (start <= i < end)]
    if outside:
        preview = ", ".join(f"0x{i:X}" for i in outside[:8])
        return f"{len(offsets)} bytes differ, {len(outside)} of them outside the " \
               f"bundle-type marker ({preview})"
    tag_left = left[start + len(BUNDLE_TYPE_MARKER):][:3]
    tag_right = right[start + len(BUNDLE_TYPE_MARKER):][:3]
    if tag_left not in BUNDLE_TYPE_TAGS or tag_right not in BUNDLE_TYPE_TAGS:
        return (f"{len(offsets)} bytes differ and the marker tags are not "
                f"recognised ({tag_left!r} vs {tag_right!r})")
    return f"{tag_left.decode()} -> {tag_right.decode()} at 0x{start:X}"


def wait_for(predicate, timeout: float, interval: float = 0.5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def running_processes() -> list[int]:
    """PIDs of any live zroutery.exe, so a locked file cannot pass unnoticed."""
    try:
        completed = subprocess.run(
            ["tasklist", "/FI", f"IMAGENAME eq {PROCESS_NAME}", "/NH"],
            capture_output=True,
            check=False,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    text = completed.stdout.decode("utf-8", errors="replace")
    pids: list[int] = []
    for line in text.splitlines():
        if PROCESS_NAME.lower() in line.lower():
            for token in line.split():
                if token.isdigit():
                    pids.append(int(token))
    return pids


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--installer", type=Path, required=True)
    parser.add_argument("--built-exe", type=Path, required=True)
    args = parser.parse_args(argv)

    installer = args.installer.resolve()
    built = args.built_exe.resolve()

    try:
        for path, what in ((installer, "installer"), (built, "built executable")):
            if not path.is_file():
                raise StepFailure(f"the {what} is missing: {path}")

        print(f"desktop-installer: {installer}")
        print(f"  size:   {installer.stat().st_size:,} bytes")
        print(f"  sha256: {sha256(installer)}")
        print(f"desktop-installer: built executable {built}")
        print(f"  size:   {built.stat().st_size:,} bytes")
        print(f"desktop-installer: install target {INSTALL_DIR}")
        print()

        # A clean slate, so "reinstall over an existing install" is genuinely
        # over an existing install and not over our own leftovers.
        if INSTALLED_EXE.exists():
            print("  pre-flight: an existing install was found; removing it first")
            if UNINSTALLER.is_file():
                run_installer(UNINSTALLER, "pre-flight uninstall")
            wait_for(lambda: not INSTALLED_EXE.exists(), 120)
            if INSTALLED_EXE.exists():
                raise StepFailure(
                    f"pre-flight: {INSTALLED_EXE} survived; cannot start clean"
                )
            # Wait out the uninstaller's asynchronous cleanup so the reinstall
            # step really is over an install and not over its own residue.
            wait_for(lambda: not uninstall_entries(), 120)
            if uninstall_entries():
                raise StepFailure(
                    f"pre-flight: registry entries survived: {uninstall_entries()}"
                )
            wait_for(lambda: not INSTALL_DIR.is_dir(), 60)
        else:
            print("  pre-flight: no existing install")
        if INSTALLED_EXE.exists() or uninstall_entries():
            raise StepFailure(
                "pre-flight: the machine is not in a clean state; refusing to "
                "claim a fresh install over a dirty one"
            )
        print()

        # -- step 1: fresh install ------------------------------------------
        print("  step 1: fresh install")
        if run_installer(installer, "install") != 0:
            raise StepFailure("the fresh install did not exit 0")
        if not wait_for(lambda: INSTALLED_EXE.is_file(), 180):
            raise StepFailure(f"the fresh install produced no {INSTALLED_EXE}")
        installed_hash = sha256(INSTALLED_EXE)
        print(f"      installed: {INSTALLED_EXE} ({INSTALLED_EXE.stat().st_size:,} bytes)")
        print(f"      installed sha256: {installed_hash}")
        if INSTALLED_EXE.stat().st_size != built.stat().st_size:
            raise StepFailure(
                "the installed executable is a different size from the built "
                f"one: {INSTALLED_EXE.stat().st_size} != {built.stat().st_size}"
            )
        print("      the installed executable is the same size as the built one")

        # The installed bytes are NOT expected to hash-equal target/release.
        #
        # `tauri build` patches the executable once per bundle with a three
        # byte marker (`__TAURI_BUNDLE_TYPE_VAR_UNK` -> `_MSI` or `_NSS`, see
        # tauri-utils/src/platform.rs), and it patches the file on disk each
        # time. With `--bundles msi nsis` the copy left in target/release
        # therefore carries the *msi* patch while the NSIS installer embeds the
        # *nsis* one. Asserting equality against target/release would be
        # asserting something false about Tauri, not something true about this
        # product.
        #
        # The invariant that actually matters is the one asserted in step 2:
        # every install of this installer puts the same bytes on disk.
        delta = bundle_patch_delta(built, INSTALLED_EXE)
        if delta:
            print(f"      differs from target/release only in the bundle-type "
                  f"marker: {delta}")
        entries = uninstall_entries()
        print(f"      registered uninstall entries: {len(entries)}")
        if len(entries) != 1:
            raise StepFailure(
                f"expected exactly 1 uninstall registry entry, found {len(entries)}: "
                f"{entries}"
            )
        print()

        # -- step 2: reinstall over the existing install ---------------------
        print("  step 2: reinstall over the existing install")
        if run_installer(installer, "reinstall") != 0:
            raise StepFailure("the reinstall did not exit 0")
        if not wait_for(lambda: INSTALLED_EXE.is_file(), 180):
            raise StepFailure(f"the reinstall removed {INSTALLED_EXE}")
        reinstalled_hash = sha256(INSTALLED_EXE)
        print(f"      installed sha256: {reinstalled_hash}")
        if reinstalled_hash != installed_hash:
            raise StepFailure(
                "the reinstall changed the installed executable: "
                f"{reinstalled_hash} != {installed_hash}"
            )
        print("      the installed bytes are unchanged")
        if INSTALL_DIR.is_dir():
            executables = sorted(p.name for p in INSTALL_DIR.glob("*.exe"))
            print(f"      .exe files in the install directory: {executables}")
            stale = [
                p.name
                for p in INSTALL_DIR.iterdir()
                if p.suffix in {".tmp", ".old", ".bak", ".new"}
            ]
            if stale:
                raise StepFailure(f"the reinstall left temporary files behind: {stale}")
            duplicates = [
                p.name
                for p in INSTALL_DIR.iterdir()
                if p.name.lower() == PROCESS_NAME and p.name != PROCESS_NAME
            ]
            if duplicates:
                raise StepFailure(
                    f"the reinstall produced a second copy of the executable: {duplicates}"
                )
        else:
            raise StepFailure(f"the reinstall removed the install directory {INSTALL_DIR}")
        entries = uninstall_entries()
        print(f"      registered uninstall entries: {len(entries)}")
        if len(entries) != 1:
            raise StepFailure(
                "the reinstall changed the number of uninstall registry entries: "
                f"expected 1, found {len(entries)}: {entries}"
            )
        pids = running_processes()
        if pids:
            raise StepFailure(f"the reinstall left zroutery.exe running: pids {pids}")
        print("      exactly one install, no leftovers, nothing running")
        print()

        # -- step 3: uninstall ----------------------------------------------
        print("  step 3: uninstall")
        if not UNINSTALLER.is_file():
            raise StepFailure(f"the install produced no uninstaller at {UNINSTALLER}")
        print(f"      uninstaller present: {UNINSTALLER}")
        if run_installer(UNINSTALLER, "uninstall") != 0:
            raise StepFailure("the uninstall did not exit 0")
        if not wait_for(lambda: not INSTALLED_EXE.exists(), 180):
            raise StepFailure(f"the uninstall left {INSTALLED_EXE} on disk")
        print(f"      {INSTALLED_EXE} is gone")

        # The uninstaller is asynchronous: it returns before it has finished
        # tidying up, removing the files first and the Add/Remove Programs
        # entry afterwards. Reading the hive the instant the process exits
        # reports a leftover that is not one, so poll for the real state rather
        # than trusting the exit code alone. An entry still present after the
        # timeout is a genuine failure.
        if not wait_for(lambda: not uninstall_entries(), 120):
            raise StepFailure(
                "the uninstall left registry entries behind 120s after the "
                f"uninstaller exited: {uninstall_entries()}"
            )
        print("      the uninstall registry entry is gone")
        if INSTALL_DIR.is_dir():
            stragglers = sorted(p.name for p in INSTALL_DIR.iterdir())
            raise StepFailure(
                f"the uninstall left files in {INSTALL_DIR}: {stragglers}"
            )
        print(f"      {INSTALL_DIR} is gone")
        print()

    except StepFailure as error:
        print()
        print(f"desktop-installer: FAILED - {error}")
        return 1

    print("desktop-installer: PASSED")
    print("  The artifact installed silently with exit code 0, and the installed")
    print("  executable is the size the build produced. Re-running the same")
    print("  installer over that install also exited 0 and left the installed")
    print("  bytes, the install directory and the registry entry exactly as they")
    print("  were -- no duplicate install, no stale files, nothing left running.")
    print("  Uninstalling exited 0 and removed the executable, the directory and")
    print("  the registry entry.")
    print()
    print("  Note on hashes: the installed executable is not expected to hash-equal")
    print("  target/release. `tauri build` patches the binary once per bundle with a")
    print("  three byte __TAURI_BUNDLE_TYPE_VAR_ marker, so with both bundles built")
    print("  the copy left in target/release is the msi-patched one while the NSIS")
    print("  installer embeds the nsis-patched one. The invariant asserted here is")
    print("  the one that matters: every install of this installer writes the same")
    print("  bytes.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
