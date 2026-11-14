#!/usr/bin/env python3
"""Check documentation structure, links, sizes, banners and generated files.

Usage:
    python3 -B scripts/docs_check.py
    python3 -B scripts/docs_check.py --write-index
    python3 -B scripts/docs_check.py --self-test
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import sys
import tempfile
from pathlib import Path

import docs_sources
import current_state

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = Path("docs/docs.manifest.json")
INDEX = Path("docs/INDEX.md")
LINK_RE = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
CODE_PATH_RE = re.compile(r"`([A-Za-z0-9_./-]+/[A-Za-z0-9_./-]+/?)`")
ROOT_PREFIXES = ("crates/", "src-tauri/", "ui/", "scripts/", "docs/", "tests/")
ROOT_FILES = ("Cargo.toml", "package.json", "README.md", "README.zh-CN.md", "README.zh-TW.md", "AGENTS.md", "CONTRIBUTING.md")
SKIP_DIRS = {".git", "node_modules", "target", "dist"}
BANNER_TYPES = {"normative", "reference"}


def load_manifest(root: Path) -> dict:
    return json.loads((root / MANIFEST).read_text(encoding="utf-8"))


def expand(entries: list[dict], root: Path) -> list[dict]:
    expanded: list[dict] = []
    for entry in entries:
        if "path" in entry:
            expanded.append(entry)
        elif "glob" in entry:
            for path in sorted(root.glob(entry["glob"])):
                copy = dict(entry)
                copy.pop("glob", None)
                copy["path"] = str(path.relative_to(root))
                expanded.append(copy)
    return expanded


def ignored(relative: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatch(relative, pattern) for pattern in patterns)


def all_markdown(root: Path) -> list[str]:
    found = []
    for path in root.rglob("*.md"):
        relative = path.relative_to(root)
        if any(part in SKIP_DIRS for part in relative.parts):
            continue
        found.append(str(relative))
    return sorted(found)


def heading_slugs(text: str) -> set[str]:
    slugs = set()
    for line in text.splitlines():
        if not line.startswith("#"):
            continue
        heading = line.lstrip("#").strip().lower()
        heading = re.sub(r"[^\w\s\u4e00-\u9fff-]", "", heading)
        slugs.add(re.sub(r"\s+", "-", heading).strip("-"))
    return slugs


def resolve_redirect(relative: str, redirects: dict[str, str]) -> str | None:
    if relative in redirects:
        return redirects[relative]
    for old, new in redirects.items():
        if old.endswith("/*") and relative.startswith(old[:-1]):
            return new[:-1] + relative[len(old[:-1]):]
    return None


def check_code_paths(root: Path, doc: Path, text: str, redirects: dict[str, str]) -> list[str]:
    errors = []
    for match in CODE_PATH_RE.finditer(text):
        raw = match.group(1).split("#", 1)[0]
        if "::" in raw or "://" in raw:
            continue
        if not (raw.startswith(ROOT_PREFIXES) or raw in ROOT_FILES):
            continue
        target = root / raw
        if target.exists():
            continue
        # ADRs use the short `tests/...` name for the Core test directory.
        if raw.startswith("tests/") and (root / "crates" / "zroutery-core" / raw).exists():
            continue
        if resolve_redirect(raw, redirects) and (root / resolve_redirect(raw, redirects)).exists():
            continue
        errors.append(f"{doc.relative_to(root)}: code path `{raw}` does not exist and has no redirect")
    return errors


def check_redirects(root: Path, redirects: dict[str, str]) -> list[str]:
    errors = []
    for old, new in redirects.items():
        target = root / (new[:-1] if old.endswith("/*") else new)
        if old.endswith("/*"):
            if not target.is_dir():
                errors.append(f"docs/archive/redirects.json: directory target `{new}` does not exist")
        elif not target.is_file():
            errors.append(f"docs/archive/redirects.json: target `{new}` does not exist")
    exact = {new for old, new in redirects.items() if not old.endswith("/*")}
    prefixes = [new[:-1] for old, new in redirects.items() if old.endswith("/*")]
    for file in (root / "docs" / "archive").rglob("*"):
        if not file.is_file() or file.name in {"README.md", "redirects.json"}:
            continue
        relative = str(file.relative_to(root))
        if relative in exact or any(relative.startswith(prefix) for prefix in prefixes):
            continue
        errors.append(f"{relative}: archived file has no redirect mapping")
    return errors


def check_archive_links(root: Path, redirects: dict[str, str]) -> list[str]:
    errors = []
    for doc in sorted((root / "docs" / "archive").rglob("*.md")):
        text = doc.read_text(encoding="utf-8")
        for match in LINK_RE.finditer(text):
            target = match.group(1).split("#", 1)[0]
            if not target or "://" in target or target.startswith("mailto:"):
                continue
            if (doc.parent / target).exists() or (root / target).exists():
                continue
            mapped = resolve_redirect(target, redirects)
            if mapped and (root / mapped).exists():
                continue
            errors.append(f"{doc.relative_to(root)}: archived link `{target}` is broken")
    return errors


def check_links(root: Path, doc: Path, text: str, redirects: dict[str, str]) -> list[str]:
    errors = []
    for match in LINK_RE.finditer(text):
        target = match.group(1).strip()
        if target.startswith(("http://", "https://", "mailto:", "#")):
            continue
        path_part, _, anchor = target.partition("#")
        if not path_part:
            continue
        destination = (doc.parent / path_part).resolve()
        try:
            relative = destination.relative_to(root)
        except ValueError:
            relative = destination
        relative_target = str(relative)
        if not destination.exists():
            mapped = resolve_redirect(relative_target, redirects)
            if mapped and (root / mapped).exists():
                destination = root / mapped
                relative = mapped
            else:
                errors.append(f"{doc.relative_to(root)}: link target `{target}` does not exist")
                continue
        if anchor and destination.suffix == ".md":
            try:
                slugs = heading_slugs(destination.read_text(encoding="utf-8"))
            except OSError:
                slugs = set()
            if anchor not in slugs:
                errors.append(f"{doc.relative_to(root)}: anchor `#{anchor}` not found in `{relative}`")
    return errors


def check_active(root: Path, entry: dict, verified: dict, redirects: dict[str, str]) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []
    path = root / entry["path"]
    if not path.is_file():
        return [f"{entry['path']}: missing file"], warnings
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        return [f"{entry['path']}: cannot read: {error}"], warnings
    lines = text.count("\n") + 1
    limit = entry.get("max_lines")
    if limit and lines > limit:
        errors.append(f"{entry['path']}: {lines} lines exceeds the {limit}-line budget")
    type_ = entry.get("type", "reference")
    if type_ in BANNER_TYPES and "> Authority:" not in "\n".join(text.splitlines()[:12]):
        errors.append(f"{entry['path']}: missing the `> Authority:` banner")
    if entry.get("generated") and "generated by" not in "\n".join(text.splitlines()[:3]):
        errors.append(f"{entry['path']}: generated file lacks its generated-by header")
    errors.extend(check_links(root, path, text, redirects))
    errors.extend(check_code_paths(root, path, text, redirects))
    if type_ != "adr":
        errors.extend(docs_sources.check_document(root, path))
        anchors = len(docs_sources.citations(text))
        if anchors == 0 and type_ in BANNER_TYPES:
            errors.append(f"{entry['path']}: a normative or reference document must cite at least one source anchor")
        if type_ == "guide" and anchors == 0:
            errors.append(f"{entry['path']}: a guide must cite at least one source anchor")
        record = verified.get(entry["path"])
        if anchors and not record:
            errors.append(f"{entry['path']}: cites sources but has no docs/verified.json entry")
        if record:
            for source, recorded in record.get("sources", {}).items():
                actual = source_hash(root, source)
                if actual != recorded:
                    errors.append(
                        f"{entry['path']}: source `{source}` changed since verification; re-read and update docs/verified.json"
                    )
    return errors, warnings


def source_hash(root: Path, source: str) -> str | None:
    import hashlib

    path = root / source
    if not path.is_file():
        return None
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def render_index(root: Path, manifest: dict) -> str:
    docs = expand(manifest.get("docs", []), root)
    pending = manifest.get("pending", [])
    lines = [
        "<!-- generated by scripts/docs_check.py --write-index; do not edit -->",
        "",
        "# Documentation Index",
        "",
        "This file is generated from `docs/docs.manifest.json`. Do not edit it.",
        "",
        "## Legend",
        "",
        "`normative` rules win over `guide`; `fact` is generated current truth; `reference` describes one subsystem; `record`/`historical` are evidence, never current fact. Audits live in GitHub Issues.",
        "",
        "## Reading paths",
        "",
        "- **New agent:** `docs/development/current-state.md` → `AGENTS.md` → `docs/development/verification.md`.",
        "- **Human contributor:** `CONTRIBUTING.md` → `docs/development/architecture.md` → `docs/development/WORKFLOW.md`.",
        "- **Operator:** `docs/development/current-state.md` → `docs/development/verification.md` (Tier 2).",
        "- **History:** `docs/archive/README.md`.",
        "",
        "## Active documents",
        "",
        "| Document | Type | Authority | Owner | Summary |",
        "|---|---|---|---|---|",
    ]
    for entry in docs:
        link = os.path.relpath(entry["path"], "docs")
        lines.append(
            "| [{path}]({link}) | {type} | {authority} | {owner} | {summary} |".format(
                path=entry["path"],
                link=link,
                type=entry.get("type", ""),
                authority=entry.get("authority", ""),
                owner=entry.get("owner", ""),
                summary=entry.get("summary", ""),
            )
        )
    lines += [
        "",
        "## Pending migration",
        "",
        "These documents are still in the legacy shape. Each is either archived or rewritten to the source-verified format; the count must reach zero.",
        "",
    ]
    if pending:
        lines += ["| Document |", "|---|"]
        for path in pending:
            link = os.path.relpath(path, "docs")
            lines.append(f"| [{path}]({link}) |")
    else:
        lines.append("None. Every document is either current or archived.")
    lines += [
        "",
        "## Archive",
        "",
        "Historical documents live under `docs/archive/`. They are not current fact and are not read by default. See `docs/archive/README.md` for the index and redirects.",
        "",
        "## Issues",
        "",
        "Audits, findings, triage and risk discussions live in GitHub Issues. Use the `audit` and `finding/*` labels; `current-state.md` links open issues and carries at most a one-line status projection, never the issue body.",
        "",
        "## Health",
        "",
    ]
    try:
        verified = json.loads((root / "docs" / "verified.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        verified = {}
    active = len(docs)
    anchored = adr_anchored = ledgered = 0
    by_type: dict[str, int] = {}
    unanchored: list[str] = []
    for entry in docs:
        type_ = entry.get("type", "reference")
        by_type[type_] = by_type.get(type_, 0) + 1
        try:
            text = (root / entry["path"]).read_text(encoding="utf-8")
            count = len(docs_sources.citations(text))
        except OSError:
            count = 0
        if count:
            if type_ == "adr":
                adr_anchored += 1
            else:
                anchored += 1
        if entry["path"] in verified:
            ledgered += 1
        if not count and type_ not in {"index", "fact", "adr"}:
            unanchored.append(f"`{entry['path']}`")
    lines.append(f"- active documents: {active}")
    lines.append(f"- source-anchored (non-ADR): {anchored}")
    lines.append(f"- hash-ledgered: {ledgered}")
    lines.append(f"- guides: {by_type.get('guide', 0)}")
    lines.append(f"- ADRs: {by_type.get('adr', 0)} ({adr_anchored} with code anchors; decision records, not machine-verified)")
    lines.append(f"- generated/index: {by_type.get('fact', 0) + by_type.get('index', 0)}")
    lines.append(f"- pending migration: {len(pending)}")
    lines.append(f"- documents that must be anchored but are not: {', '.join(unanchored) if unanchored else 'none'}")
    lines.append("- source anchors are checked by `scripts/docs_sources.py`")
    lines.append("- links, sizes, freshness and code paths are checked by `scripts/docs_check.py`")
    return "\n".join(lines) + "\n"


def check_generated(root: Path, manifest: dict) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []
    try:
        state = current_state.load_state(root)
    except (OSError, json.JSONDecodeError) as error:
        return [f"cannot read docs/development/state.json: {error}"], warnings
    expected = current_state.render(state)
    target = root / current_state.TARGET
    if not target.is_file() or target.read_text(encoding="utf-8") != expected:
        errors.append("docs/development/current-state.md is stale; run `python3 -B scripts/current_state.py --write`")
    expected_index = render_index(root, manifest)
    index_path = root / INDEX
    if not index_path.is_file() or index_path.read_text(encoding="utf-8") != expected_index:
        errors.append("docs/INDEX.md is stale; run `python3 -B scripts/docs_check.py --write-index`")
    return errors, warnings


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="zr-docs-check-") as temporary:
        root = Path(temporary)
        (root / "docs" / "development").mkdir(parents=True)
        (root / "docs").mkdir(exist_ok=True)
        manifest = {
            "docs": [
                {"path": "docs/good.md", "type": "normative", "authority": "normative", "owner": "docs", "summary": "good", "max_lines": 10},
                {"path": "docs/bad.md", "type": "normative", "authority": "normative", "owner": "docs", "summary": "bad", "max_lines": 10},
            ],
            "pending": [],
            "ignore": [],
        }
        (root / MANIFEST).write_text(json.dumps(manifest), encoding="utf-8")
        (root / "src").mkdir()
        (root / "src" / "sample.rs").write_text("pub fn value() {}\n", encoding="utf-8")
        (root / "docs" / "good.md").write_text(
            "> Authority: normative · Owner: docs · Last verified: 2026-11-14 · Verified by: x\n\n`src/sample.rs::value`\n",
            encoding="utf-8",
        )
        (root / "docs" / "bad.md").write_text(
            "> Authority: normative\n\n[broken](missing.md)\n" + "\n".join(f"line {i}" for i in range(12)),
            encoding="utf-8",
        )
        errors, _ = check_active(root, manifest["docs"][0], {"docs/good.md": {"sources": {}}}, {})
        if errors:
            print(f"docs-check self-test: good document rejected ({errors})", file=sys.stderr)
            return 1
        errors, _ = check_active(root, manifest["docs"][1], {}, {})
        if not any("does not exist" in error for error in errors):
            print(f"docs-check self-test: broken link not caught ({errors})", file=sys.stderr)
            return 1
        if not any("max" in error or "budget" in error for error in errors):
            print(f"docs-check self-test: size budget not caught ({errors})", file=sys.stderr)
            return 1
        archive = root / "docs" / "archive"
        archive.mkdir()
        (archive / "bad.md").write_text("[broken](missing.md)\n", encoding="utf-8")
        if not check_archive_links(root, {}):
            print("docs-check self-test: a broken archived link was NOT caught", file=sys.stderr)
            return 1
        (archive / "bad.md").unlink()
        if resolve_redirect("docs/old/x.json", {"docs/old/*": "docs/new/*"}) != "docs/new/x.json":
            print("docs-check self-test: prefix redirect did not resolve", file=sys.stderr)
            return 1
        print("docs-check: self-test passed (accepts a valid document, rejects a broken link, a broken archived link and an over-budget file, resolves a prefix redirect)")
        return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default=str(ROOT))
    parser.add_argument("--write-index", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    root = Path(args.root)
    manifest = load_manifest(root)
    docs = expand(manifest.get("docs", []), root)
    if args.write_index:
        (root / INDEX).write_text(render_index(root, manifest), encoding="utf-8")
        print(f"docs-check: wrote {INDEX}")
        return 0
    errors: list[str] = []
    warnings: list[str] = []
    redirects: dict[str, str] = {}
    redirect_path = root / "docs" / "archive" / "redirects.json"
    if redirect_path.is_file():
        try:
            redirects = json.loads(redirect_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            errors.append(f"docs/archive/redirects.json: invalid JSON: {error}")
    verified_path = root / "docs" / "verified.json"
    verified = {}
    if verified_path.is_file():
        try:
            verified = json.loads(verified_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            errors.append(f"docs/verified.json: invalid JSON: {error}")
    errors.extend(check_redirects(root, redirects))
    errors.extend(check_archive_links(root, redirects))
    covered = {entry["path"] for entry in docs} | set(manifest.get("pending", []))
    patterns = manifest.get("ignore", [])
    for relative in all_markdown(root):
        if relative in covered or ignored(relative, patterns):
            continue
        errors.append(f"{relative}: not listed in docs/docs.manifest.json (active, pending or ignore)")
    for path in manifest.get("pending", []):
        if not (root / path).is_file():
            errors.append(f"{path}: pending entry does not exist")
    for entry in docs:
        found, noted = check_active(root, entry, verified, redirects)
        errors.extend(found)
        warnings.extend(noted)
    generated_errors, generated_warnings = check_generated(root, manifest)
    errors.extend(generated_errors)
    warnings.extend(generated_warnings)
    for warning in warnings:
        print(f"docs-check: warning: {warning}", file=sys.stderr)
    if errors:
        for error in errors:
            print(f"docs-check: {error}", file=sys.stderr)
        print(f"docs-check: {len(errors)} error(s) in {len(docs)} active document(s)", file=sys.stderr)
        return 1
    print(
        f"docs-check: {len(docs)} active document(s), {len(manifest.get('pending', []))} pending, "
        f"{len(warnings)} warning(s), all checks passed"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
