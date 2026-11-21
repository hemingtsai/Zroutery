"""Pack a Zroutery plugin source directory into a .zrp archive.

Usage: python plugins/pack.py <plugin-name> [more-names...]

Each plugin directory (plugins/<name>/) must contain zroutery.plugin.json plus
its declared resources. Only manifest-referenced files with package-legal
suffixes (.mjs/.json) are packed; README/LICENSE stay out of the archive.
The archive layout matches what `Package::from_archive` accepts: flat entries,
no directory records.
"""
import json
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
LEGAL_SUFFIXES = (".mjs", ".json")


def referenced_files(manifest_path: Path) -> list[str]:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    names = {"zroutery.plugin.json"}
    runtime = manifest.get("runtime") or {}
    for entry in (runtime.get("entries") or {}).values():
        if entry:
            names.add(entry)
    for section in ("themes", "transport_profiles"):
        for item in manifest.get("contributions", {}).get(section) or []:
            names.add(item["file"])
    schema = manifest.get("options_schema")
    if schema:
        names.add(schema)
    return sorted(names)


def pack(name: str) -> Path:
    directory = ROOT / name
    manifest_path = directory / "zroutery.plugin.json"
    if not manifest_path.is_file():
        raise SystemExit(f"missing manifest: {manifest_path}")
    version = json.loads(manifest_path.read_text(encoding="utf-8"))["version"]
    out = directory / "dist" / f"{name}-{version}.zrp"
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as archive:
        for name in referenced_files(manifest_path):
            data = (directory / name).read_bytes()
            info = zipfile.ZipInfo(name)
            info.external_attr = 0o100644 << 16  # regular file, 0644
            archive.writestr(info, data)
    return out


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    for plugin_name in sys.argv[1:]:
        print(pack(plugin_name))
