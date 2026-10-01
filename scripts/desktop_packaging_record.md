# Desktop packaging gate: what was actually observed

Node: TEST-PACKAGING. Gate: desktop-packaging-reachability-rollback.
Revision tested: `170c580` (`docs(workflow): close the float transport gap at its root`).
Platform: Windows 11, x86-64, non-elevated shell (`WIN-OKB96PM7IIF\InfinityNeko`).

This file records what ran and what came back. It is not a claim; every number
here was printed by a command on this revision.

## Why this file is not under `docs/`

`docs/**` is read-only for this node, and `scripts/orch_docs_test.py` validates
the records under `docs/development` against a schema. A packaging observation
is not an orchestration record, so it lives beside the scripts that produce it.

## Gate 1: the Tauri/package build

Two commands, both from the worktree root:

```
pnpm install --frozen-lockfile
node node_modules\@tauri-apps\cli\tauri.js build --bundles msi nsis
```

`pnpm install` was required: a fresh worktree has no `ui/node_modules` and no
`ui/dist`, and `beforeBuildCommand` is `pnpm ui:build`. `--frozen-lockfile`
keeps the fetch to exactly what `pnpm-lock.yaml` already requires. It finished
in 10.8 s, reusing 69 packages from the store at `E:\.pnpm-store\v11` and
downloading none.

The build ran **739.0 s** (12 min 19 s) and exited **0**. The Rust release
profile is the long pole: `lto = true`, `codegen-units = 1`, `panic = "abort"`
over Tauri/wry, bundled SQLite and reqwest.

Artifacts produced, with the sizes and hashes actually recorded:

| Artifact | Size (bytes) | SHA-256 |
| --- | --- | --- |
| `target/release/zroutery.exe` | 17,675,264 | `607A2279BC5B146BC788A1FC02AF3EB5DAA71ACF81B1B1E386F55D1FF9BC102C` |
| `target/release/bundle/msi/Zroutery_0.8.0_x64_en-US.msi` | 13,873,152 | `7257C9107513DDEF7FF47BC1E08FE3421224C7EF55B3B1B98779DA15137D46B8` |
| `target/release/bundle/nsis/Zroutery_0.8.0_x64-setup.exe` | 11,762,956 | `596A33D723552598C6D7FF2C877C2D7C6CFDEB9CA0B5ADCEB78D56E0F5F2D03F` |

### Why the bundle target set is restricted to `msi, nsis`

`src-tauri/tauri.conf.json` declares `bundle.targets` as
`["app", "dmg", "msi", "nsis"]`. On Windows, `app` produces a macOS `.app`
bundle and `dmg` produces a macOS disk image; neither has a Windows producer.
The Tauri CLI agrees independently of this node's judgement — on this platform
it prints its own accepted values as `[possible values: msi, nsis]`.

So `--bundles msi nsis` removes the two formats this platform cannot produce,
for the structural reason that they are macOS bundle formats, and keeps both
Windows-native installers. Both Windows installers were built. The restriction
was made on the command line, and `tauri.conf.json` was **not** edited: narrowing
the committed config to hide an untested platform would defeat the point of the
gate. The `app` and `dmg` targets remain untested by this node, and are
recorded here as untested.

`pnpm build` would otherwise have been the entry point. Two notes for the next
person: `pnpm tauri build --bundles msi,nsis` fails, because pnpm mangles the
comma into a single invalid value; and `--bundles msi nsis` is also rejected by
pnpm. Calling the CLI's `tauri.js` directly avoids both.

## Gate 2: artifact-level reachability

```
python -B scripts/desktop_artifact_test.py --self-test
python -B scripts/desktop_artifact_test.py --exe target/release/zroutery.exe
```

Both exited **0**. The gate scans the built executable itself, not the source
tree, and it refuses to certify a negative it has not shown to work.

What it scanned: the **32 forbidden activation symbols** transcribed from
`crates/zroutery-core/tests/activation_test.rs`, each in ASCII **and**
UTF-16LE, plus **8 ml-module markers** (`src/ml/`, `ml/activation.rs`,
`zroutery_core::ml`, `zroutery_core::ml::activation`,
`zroutery_core::ml::activation::ActivationStore`, `zroutery_core::ml::journal`,
`zroutery_core::ml::model_identity`, `zroutery_core::ml::dataset`).

**Result: none of the 32 symbols and none of the 8 markers occur in the image,
in either encoding.**

Two things make that negative worth something:

**The symbol list cannot drift from the Rust gate.** The script parses the
`let symbols = [...]` list out of
`nothing_in_the_shipped_product_can_name_this_module` at run time and fails if
it differs from the transcribed set. An earlier draft scanned a fixed-size
window instead and swept up the test's own assertion messages; the scoping bug
was caught before it could report a vacuous pass.

**The scanner is proved to work before its verdict is believed.** Two
independent controls, both of which must hold:

* a canary string appended to a *copy of the artifact under test* is located by
  the same search, so the negative cannot come from a truncated read, a wrong
  offset or an encoding mistake;
* `identifier` and `app.security.csp` are read from `src-tauri/tauri.conf.json`
  and located in the unmodified image. `tauri-codegen` emits the config as a
  Rust struct literal, so both are string literals in `.rdata`.

The control earned its place during development. A first draft asserted
`shortDescription` was embedded; it is not — it is bundle metadata consumed by
the bundler CLI, never by the running app. The gate reported **INCONCLUSIVE**
and exited 1 instead of certifying the negative, which is exactly the behaviour
a control exists for. The control strings are now read from the config rather
than guessed.

### What the negative does and does not prove

`[profile.release]` sets `strip = true` and `lto = true`, so the `.pdb` symbol
table is not part of the artifact and this scan never reads one. A missing
*string* is therefore not a disassembly proof that a type was never
instantiated.

It is sharper than it first appears, though, and the artifact was checked
directly for that. Rust keeps `file!()`/panic-location literals as ordinary
string data regardless of `strip`, and the real binary contains **582 embedded
`.rs` source-path literals**, of which 16 are `crates\zroutery-core\src\...`
paths — including `server\mod.rs` and `router.rs`. **Zero** contain an `ml`
segment, and neither `src/ml/` nor `src\ml\` appears anywhere in the image.

So the evidence is concrete: had `src/ml/activation.rs` been compiled into the
desktop shell, its path, its panic locations, its `Display`/`Debug` format
strings and its type names would be in `.rdata`. They are not.

A deliberate non-claim: nothing here says the `ml` feature is reachable, or
that the desktop app could use it. Node 7E-2F established that the activation
installer must stay unreachable from the shipped product, and this gate is
about that. Wiring `ml` into the desktop shell was deliberately not done.

## Gate 3: install, reinstall, uninstall

```
python -B scripts/desktop_installer_test.py \
    --installer target/release/bundle/nsis/Zroutery_0.8.0_x64-setup.exe \
    --built-exe target/release/zroutery.exe
```

Exited **0**. Executed, not described:

1. pre-flight — no existing install;
2. `Zroutery_0.8.0_x64-setup.exe /S` → **exit 0**; installed
   `zroutery.exe` at 17,675,264 bytes, sha256
   `fc94c36b692ec415e7e6ac0daf0c096e160462f475452ae28a09b3b59c836e11`;
   exactly 1 uninstall registry entry;
3. **reinstall**: the same installer `/S` over that install → **exit 0**;
   installed sha256 **unchanged**; install directory still holds exactly
   `uninstall.exe`, `zroutery-headless.exe`, `zroutery.exe` — no duplicate
   executable, no `.tmp`/`.old` leftovers; still exactly 1 registry entry; no
   `zroutery.exe` process left running;
4. `uninstall.exe /S` → **exit 0**; executable gone, registry entry gone,
   install directory gone.

### A false assertion, and what replaced it

The first run of this gate **failed**, and the failure was in the gate, not the
product. It asserted that the installed executable hash-equals
`target/release/zroutery.exe`. It does not, and it should not: the two files
have the same length and differ in exactly **3 bytes**, at `0xA004EF`.

Those bytes are the `__TAURI_BUNDLE_TYPE_VAR_` marker that `tauri build` rewrites
once per bundle (`tauri-utils/src/platform.rs:349-360`). With both bundles
built, the copy left in `target/release` carries the msi patch (`UNK`/`MSI`)
and the NSIS installer embeds the nsis patch (`NSS`). Hash-equality would have
been asserting something false about Tauri.

The gate now asserts what is actually true and still meaningful: the installed
file is the size the build produced, and it differs from `target/release` *only*
in the bundle-type marker tag, which is named in the output as
`UNK -> NSS at 0xA004D7`. The invariant that carries the gate is the one in
step 3 — every install of this installer writes the same bytes.

A second gate bug was found the same way: the uninstaller is asynchronous,
removing files before its Add/Remove Programs entry, and the gate read the hive
too early and reported a leftover that was not one. It now polls for the real
state and still fails if an entry survives 120 s.

### MSI: attempted, and blocked

```
msiexec /i target\release\bundle\msi\Zroutery_0.8.0_x64_en-US.msi /qn /l*v <log>
```

Exited **1603**. The log says why:

> Error 1925. You do not have sufficient privileges to complete this
> installation for all users of the machine.

The MSI is a per-machine install and this shell is not elevated, so the MSI
**install/reinstall leg could not be executed here**. It is recorded as
blocked, not passed. The MSI was still *built* (gate 1), which is the part this
node can prove on this machine. Post-attempt the machine was confirmed clean: no
MSI registration in either registry view, no install directory, no running
process.

The install/reinstall/uninstall gate is carried by the NSIS artifact, which is a
real Windows installer and genuinely reinstallable without elevation.

## Re-running this

```
pnpm install --frozen-lockfile
node node_modules\@tauri-apps\cli\tauri.js build --bundles msi nsis
python -B scripts/desktop_artifact_test.py --self-test
python -B scripts/desktop_artifact_test.py --exe target/release/zroutery.exe
python -B scripts/desktop_installer_test.py \
    --installer target/release/bundle/nsis/Zroutery_0.8.0_x64-setup.exe \
    --built-exe target/release/zroutery.exe
```

The installer gate installs and removes software under
`%LOCALAPPDATA%\Zroutery` and writes an `HKCU` uninstall entry; it cleans up
after itself, but it is not a read-only operation.
