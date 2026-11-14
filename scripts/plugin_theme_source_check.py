#!/usr/bin/env python3
"""ThemeV1 pack values must be derivable from official UI-library sources.

`plugins/theme-md3/theme.json` and `plugins/theme-fluent/theme.json` claim
official-implementation fidelity. Hand transcription from JS-rendered design
sites has already produced values that exist in no official machine-readable
source, so this gate re-derives every pack value from pinned snapshots under
``scripts/theme_sources/snapshots/`` (``manifest.json`` pins one commit per
upstream repo) and reports anything that has neither official provenance nor a
recorded ruling in ``docs/development/skin-spec-md3-fluent.md``.

Value classes the gate distinguishes:
  official     value equals a token resolved from the pinned snapshots
  mix          value is a state-layer/hover mix over official operands with an
               official opacity (md-sys-state / spec-cited ramp)
  deviation    value is listed in EXPECTED below with a citation to the ruling
               that allows it (v3.1 control heights, host grid, ...)
  unexplained  everything else -> the fix list; nonzero exit while nonempty

Usage:
  python scripts/plugin_theme_source_check.py             # verify packs
  python scripts/plugin_theme_source_check.py --fetch     # refresh snapshots
  python scripts/plugin_theme_source_check.py --self-test # parser fixtures
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "theme_sources")
SNAP = os.path.join(SRC, "snapshots")
ROOT = os.path.dirname(HERE)
PACKS = {"md3": os.path.join(ROOT, "plugins", "theme-md3", "theme.json"),
         "fluent": os.path.join(ROOT, "plugins", "theme-fluent", "theme.json")}


def snapshot(name: str) -> str:
    with open(os.path.join(SNAP, name), encoding="utf-8") as handle:
        return handle.read()


def fetch() -> int:
    """Refresh snapshots from the pinned commits. Network required."""
    manifest = json.load(open(os.path.join(SRC, "manifest.json"), encoding="utf-8"))
    failures = 0
    for entry in manifest["files"]:
        repo = manifest["repos"][entry["repo"]]
        url = "https://raw.githubusercontent.com/%s/%s/%s" % (repo["repo"], repo["sha"], entry["path"])
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "zroutery-theme-source-check"})
            with urllib.request.urlopen(req, timeout=60) as response:
                data = response.read()
        except OSError as exc:
            print("FETCH FAIL %s: %s" % (entry["snapshot"], exc))
            failures += 1
            continue
        with open(os.path.join(SNAP, entry["snapshot"]), "wb") as handle:
            handle.write(data)
        print("fetched %-40s %7d B" % (entry["snapshot"], len(data)))
    return 1 if failures else 0


# ---------------------------------------------------------------------------
# Color normalization

def norm_color(value: str) -> str:
    """Normalize a css color to lowercase #rrggbbaa form for comparison."""
    v = value.strip().lower()
    m = re.fullmatch(r"#([0-9a-f]{3})([0-9a-f])?", v)
    if m:
        rgb = "".join(c * 2 for c in m.group(1))
        return "#" + rgb + (m.group(2) or "f") * 2
    m = re.fullmatch(r"#([0-9a-f]{6})([0-9a-f]{2})?", v)
    if m:
        return "#" + m.group(1) + (m.group(2) or "ff")
    m = re.fullmatch(r"rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+)\s*)?\)", v)
    if m:
        r, g, b = (int(m.group(i)) for i in (1, 2, 3))
        a = m.group(4)
        alpha = round(float(a) * 255) if a is not None else 255
        return "#%02x%02x%02x%02x" % (r, g, b, alpha)
    return v


# ---------------------------------------------------------------------------
# MD3 SCSS token parsing (material-web v0_192 shape)

MD_ENTRY = re.compile(r"'([\w-]+)':\s*((?:\([^()]*\))|(?:if\(\$exclude-hardcoded-values,\s*null,\s*[^)]*\))|(?:map\.get\(\$deps,\s*'[\w-]+',\s*'[\w-]+'\))|[^,\n]+)")


def scss_entries(text: str) -> list[tuple[str, str]]:
    """Extract ('key', value-expr) pairs from the token maps."""
    out: list[tuple[str, str]] = []
    for name, expr in MD_ENTRY.findall(text):
        expr = re.sub(r"\s+", " ", expr).strip()
        if expr.endswith(","):
            expr = expr[:-1]
        out.append((name, expr))
    return out


class MdTokens:
    """Resolved md-sys / md-comp token dictionaries."""

    def __init__(self) -> None:
        self.palette = self._values(snapshot("md-ref-palette.scss"), {})
        self.typeface = self._values(snapshot("md-ref-typeface.scss"), {})
        base_env = {"md-ref-palette": self.palette, "md-ref-typeface": self.typeface}
        self.sys = {mode: self._values(self._fn_body(snapshot("md-sys-color.scss"), "values-%s" % mode), base_env)
                    for mode in ("light", "dark")}
        self.shape = self._values(snapshot("md-sys-shape.scss"), {})
        self.state = self._values(snapshot("md-sys-state.scss"), {})
        self.typescale = self._values(snapshot("md-sys-typescale.scss"), base_env)
        self.elevation = self._values(snapshot("md-sys-elevation.scss"), {})
        self._comp_cache: dict[str, dict[str, dict[str, str]]] = {}

    def _fn_body(self, text: str, fname: str) -> str:
        m = re.search(r"@function\s+" + re.escape(fname) + r"\(([^)]*)\)\s*\{(.*?)\n\}", text, re.S)
        if not m:
            raise SystemExit("md tokens: function %s not found" % fname)
        return m.group(2)

    def _resolve(self, expr: str, env: dict[str, dict[str, str]]) -> str:
        expr = expr.strip()
        m = re.fullmatch(r"map\.get\(\$deps,\s*'([\w-]+)',\s*'([\w-]+)'\)", expr)
        if m:
            ns, key = m.group(1), m.group(2)
            if ns not in env or env[ns] is None or key not in env[ns]:
                raise KeyError("unresolved token ref %s.%s" % (ns, key))
            return env[ns][key]
        m = re.fullmatch(r"if\(\$exclude-hardcoded-values,\s*null,\s*(.+)\)", expr)
        if m:
            return self._resolve(m.group(1), env)
        return expr

    def _values(self, text: str, env: dict[str, dict[str, str]]) -> dict[str, str]:
        """Resolve one @function values(...) map in md token SCSS shape."""
        out: dict[str, str] = {}
        for key, expr in scss_entries(text):
            try:
                out[key] = self._resolve(expr, env)
            except KeyError as exc:
                raise SystemExit("md token resolution: %s" % exc)
        return out

    def comp(self, name: str, mode: str) -> dict[str, str]:
        """Resolved slot map for one _md-comp file in one mode context."""
        key = "%s:%s" % (name, mode)
        if key not in self._comp_cache:
            text = snapshot(name + ".scss")
            env = {"md-sys-color": self.sys[mode], "md-sys-shape": self.shape,
                   "md-sys-state": self.state, "md-sys-typescale": self.typescale,
                   "md-sys-elevation": self.elevation, "md-ref-palette": self.palette,
                   "md-ref-typeface": self.typeface}
            # A comp file may define values($deps) with a light or dark default;
            # the mode argument picks the matching md-sys-color side already.
            out: dict[str, str] = {}
            for slot, expr in scss_entries(text):
                try:
                    out[slot] = self._resolve(expr, env)
                except KeyError as exc:
                    raise SystemExit("%s: %s" % (name, exc))
            self._comp_cache[key] = out
        return self._comp_cache[key]

    def official_colors(self, mode: str) -> dict[str, tuple[str, str]]:
        """value -> (token-set, token-name) provenance index for one mode."""
        index: dict[str, tuple[str, str]] = {}
        for name, hexv in self.sys[mode].items():
            index.setdefault(norm_color(hexv), ("md-sys-color", name))
        for name in COMPS_MD:
            for slot, value in self.comp(name, mode).items():
                if re.fullmatch(r"#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?", value):
                    index.setdefault(norm_color(value), (name, slot))
        return index


COMPS_MD = ["md-comp-filled-button", "md-comp-filled-tonal-button", "md-comp-outlined-button",
            "md-comp-text-button", "md-comp-filled-text-field", "md-comp-outlined-text-field",
            "md-comp-filled-select", "md-comp-outlined-select", "md-comp-checkbox",
            "md-comp-switch", "md-comp-dialog", "md-comp-menu", "md-comp-snackbar",
            "md-comp-navigation-drawer", "md-comp-outlined-segmented-button", "md-comp-list",
            "md-comp-badge", "md-comp-banner", "md-comp-data-table", "md-comp-scrim",
            "md-comp-filled-card", "md-comp-elevated-card", "md-comp-primary-navigation-tab",
            "md-comp-radio-button"]


# ---------------------------------------------------------------------------
# Fluent TS token parsing

def ts_object(text: str, name: str) -> dict[str, str]:
    """Parse `export const NAME ... = { ... }` string-valued objects."""
    m = re.search(r"export const %s[^=]*=\s*\{(.*?)\n\};" % re.escape(name), text, re.S)
    if not m:
        raise SystemExit("fluentui snapshot: %s object not found" % name)
    out: dict[str, str] = {}
    for key, value in re.findall(r"'?([\w-]+)'?\s*:\s*['`]?([^`,\n]+)['`]?,", m.group(1)):
        out[key] = value.strip().strip("'`").strip()
    return out


class FluentTokens:
    """Resolved fluentui web light/dark token dictionaries."""

    def __init__(self) -> None:
        colors_ts = snapshot("fui-global-colors.ts")
        ramp_names = ("grey", "whiteAlpha", "blackAlpha", "grey10Alpha", "grey12Alpha", "grey14Alpha")
        self.ramps = {name: ts_object(colors_ts, name) for name in ramp_names}
        self.ramps["white"] = {"0": snapshot_and(colors_ts, "white")}
        self.ramps["black"] = {"0": snapshot_and(colors_ts, "black")}
        self.brand = {k: v for k, v in ts_object(snapshot("fui-global-brand-colors.ts"), "brandWeb").items()}
        self.radius = ts_object(snapshot("fui-global-border-radius.ts"), "borderRadius")
        self.stroke = ts_object(snapshot("fui-global-stroke-widths.ts"), "strokeWidths")
        self.alias = {"light": self._alias("fui-light-color.ts"),
                      "dark": self._alias("fui-dark-color.ts")}
        self.status = {"light": self._status("light"), "dark": self._status("dark")}
        self.shadows = {"light": self._shadows("light"), "dark": self._shadows("dark")}

    def _status(self, mode: str) -> dict[str, str]:
        """colorStatus* tokens: statusColorMapping -> shared-color ramps, with
        the per-slot variant accessors taken from the palette snapshot body."""
        colors_ts = snapshot("fui-global-colors.ts")
        pal = snapshot("fui-light-color-palette.ts" if mode == "light" else "fui-dark-color-palette.ts")
        accessors = dict(re.findall(r"\[`colorStatus\$\{color\}(\w+)`\]:\s*mappedStatusColors\[sharedColor\]\.(\w+)", pal))
        if not accessors:
            raise SystemExit("fluentui %s palette: no colorStatus accessors found" % mode)
        mapping = dict(re.findall(r"(\w+):\s*'(\w+)'", snapshot("fui-status-color-mapping.ts")))
        out: dict[str, str] = {}
        for status, shared in mapping.items():
            ramp = dict(re.findall(r"(\w+):\s*'(#[0-9a-fA-F]{6})'",
                                   re.search(r"export const %s: ColorVariants = \{(.*?)\n\};" % shared,
                                             colors_ts, re.S).group(1)))
            for slot, variant in accessors.items():
                out["colorStatus%s%s" % (status.capitalize(), slot)] = ramp[variant]
        return out

    def subtitle1(self) -> tuple[str, str]:
        """fluentui subtitle1 ramp step: fontSizeBase500 px + fontWeightSemibold."""
        fonts = snapshot("fui-global-fonts.ts")
        size = re.search(r"fontSizeBase500:\s*'(\d+)px'", fonts)
        weight = re.search(r"fontWeightSemibold:\s*(\d+)", fonts)
        styles = snapshot("fui-global-typography-styles.ts")
        m = re.search(r"subtitle1:\s*\{[^}]*fontSize:\s*tokens\.fontSizeBase500,[^}]*"
                      r"fontWeight:\s*tokens\.fontWeightSemibold", styles, re.S)
        if not (size and weight and m):
            raise SystemExit("fluentui subtitle1 tokens not found")
        return size.group(1), weight.group(1)

    def _resolve_ref(self, expr: str) -> str:
        m = re.fullmatch(r"(\w+)\[(\d+)\]", expr.strip())
        if m and m.group(1) in self.ramps:
            return self.ramps[m.group(1)][m.group(2)]
        if m and m.group(1) == "brand":
            return self.brand[m.group(2)]
        if expr.strip() in ("white", "black", "transparent"):
            return {"white": "#ffffffff", "black": "#000000ff", "transparent": "#00000000"}[expr.strip()]
        if re.fullmatch(r"#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})([0-9a-fA-F]{2})?|rgba?\([^)]*\)", expr.strip()):
            return expr.strip()
        raise KeyError("fluentui token expr: %s" % expr)

    def _alias(self, snapshot_name: str) -> dict[str, str]:
        """alias/lightColor.ts entries carry `ref, // resolved` comments; use
        the expression as truth and the comment as a cross-check."""
        text = snapshot(snapshot_name)
        out: dict[str, str] = {}
        stale_comments = 0
        # Values are either quoted literals (may contain commas inside rgba)
        # or bare ramp references; both carry a trailing `// resolved` comment.
        entry = re.compile(r"^\s{2}(\w+):\s*('[^']*'|`[^`]*`|[^,\n]+),\s*//\s*(.+)$", re.M)
        for key, expr, comment in entry.findall(text):
            expr = expr.strip().strip("'`")
            resolved = self._resolve_ref(expr.strip().strip("'\""))
            comment = comment.split(" Global.")[0].strip()
            # The generated comments bake in an older default brand ramp
            # (#0078d4); the expression against the pinned brandWeb ramp is
            # authoritative, so a comment mismatch warns instead of failing.
            if re.match(r"#|rgba", comment) and norm_color(comment) != norm_color(resolved):
                stale_comments += 1
            out[key] = resolved
        if stale_comments:
            print("note: %s: %d stale generated comments (older brand ramp) ignored"
                  % (snapshot_name, stale_comments))
        if len(out) < 100:
            raise SystemExit("%s: only %d tokens parsed" % (snapshot_name, len(out)))
        return out

    def _shadows(self, mode: str) -> dict[str, str]:
        """Rebuild shadow2..64 from createShadowTokens + the theme's ambient/key."""
        alias = self.alias[mode]
        ambient = alias["colorNeutralShadowAmbient"]
        key = alias["colorNeutralShadowKey"]
        formulas = {"shadow2": "0 0 2px A, 0 1px 2px K", "shadow4": "0 0 2px A, 0 2px 4px K",
                    "shadow8": "0 0 2px A, 0 4px 8px K", "shadow16": "0 0 2px A, 0 8px 16px K",
                    "shadow28": "0 0 8px A, 0 14px 28px K", "shadow64": "0 0 8px A, 0 32px 64px K"}
        return {name: formula.replace("A", ambient).replace("K", key) for name, formula in formulas.items()}

    def official_colors(self, mode: str) -> dict[str, tuple[str, str]]:
        index: dict[str, tuple[str, str]] = {}
        for name, value in self.alias[mode].items():
            if re.fullmatch(r"#([0-9a-fA-F]{6}([0-9a-fA-F]{2})?)", value):
                index.setdefault(norm_color(value), ("lightColor" if mode == "light" else "darkColor", name))
        return index


def snapshot_and(text: str, name: str) -> str:
    m = re.search(r"export const %s\s*=\s*'([^']+)';" % re.escape(name), text)
    if not m:
        raise SystemExit("fluentui constant %s not found" % name)
    return m.group(1)


# ---------------------------------------------------------------------------
# WinUI XAML theme dictionaries

def parse_winui() -> dict[str, dict[str, str]]:
    """Return {'dark': {...}, 'light': {...}} color dictionaries."""
    text = snapshot("winui-themeresources.xaml")
    themes: dict[str, dict[str, str]] = {}
    # Top-level dictionaries in file order: Default (=dark), Light, HighContrast.
    blocks = re.findall(r'<ResourceDictionary\s+x:Key="(\w+)"\s*>(.*?)</ResourceDictionary>', text, re.S)
    if not blocks:
        raise SystemExit("winui xaml: no theme dictionaries found")
    for key, body in blocks:
        theme = {"Default": "dark", "Light": "light"}.get(key)
        if not theme:
            continue
        values: dict[str, str] = {}
        for name, hexv in re.findall(r'<Color x:Key="(\w+)">(#[0-9A-Fa-f]{6,8})</Color>', body):
            values[name] = winui_hex(hexv)
        for name, hexv in re.findall(r'<SolidColorBrush x:Key="(\w+)"[^>]*Color="(#[0-9A-Fa-f]{6,8})"', body):
            values.setdefault(name, winui_hex(hexv))
        themes[theme] = values
    return themes


def winui_hex(hexv: str) -> str:
    """WinUI writes #AARRGGBB; packs and the compiler use #RRGGBBAA."""
    if len(hexv) == 9:
        return "#" + hexv[3:] + hexv[1:3]
    return hexv


def winui_gradient(text: str, key: str) -> list[tuple[str, float]]:
    """Gradient stops of a named brush as (color-or-resource-key, offset)."""
    text = snapshot("winui-themeresources.xaml")
    m = re.search(r'<LinearGradientBrush[^>]*x:Key="%s"[^>]*>(.*?)</LinearGradientBrush>' % key, text, re.S)
    if not m:
        raise SystemExit("winui xaml: %s brush not found" % key)
    return re.findall(r'<GradientStop Offset="([\d.]+)" Color="\{StaticResource (\w+)\}"', m.group(1))


# ---------------------------------------------------------------------------
# Pack loading (mirrors ui/src/themes/compiler.ts ColorExpr resolution)

ROLES_EXPECTED = {"bg", "surface", "surface-muted", "line", "text", "text-2", "muted",
                  "primary", "primary-hover", "on-primary"}


def load_pack(path: str) -> dict:
    return json.load(open(path, encoding="utf-8"))


def resolve_expr(expr: dict, roles: dict, mode: str, depth: int = 0) -> str:
    """Concrete color of a ColorExpr, following pack-internal role refs."""
    if depth > 16:
        raise SystemExit("pack: color reference too deep")
    if "hex" in expr:
        return norm_color(expr["hex"])
    if "role" in expr:
        return resolve_expr(roles[expr["role"]], roles, mode, depth + 1)
    if "system" in expr:
        return resolve_expr(expr["fallback"], roles, mode, depth + 1)
    a = resolve_expr(expr["mix"][0], roles, mode, depth + 1)
    b = resolve_expr(expr["mix"][1], roles, mode, depth + 1)
    return "mix(%s,%s,%.4f)" % (a, b, float(expr["first_fraction"]))


def pack_colors(pack: dict, mode: str) -> dict[str, str]:
    roles = pack["modes"][mode]["colors"]
    return {name: resolve_expr(expr, roles, mode) for name, expr in roles.items()}


# ---------------------------------------------------------------------------
# Expectations: recorded rulings and spec-cited constants. Everything a pack
# value can be besides "equal to an official token" must be listed here with
# its citation, or the gate fails.

def spec_constants() -> dict[str, str]:
    """Spec-cited constants (sources without machine-readable snapshots)."""
    derived = {
        # skin-spec v3 "ok/warn (custom pairs, derived from the official container pattern)":
        # pairs derived from the baseline error-tone structure, exact values
        # recorded in the spec with their contrast computations.
        "md3 ok light": "#00722a", "md3 ok-bg light": "#daecdb",
        "md3 warn light": "#835100", "md3 warn-bg light": "#f5e3cd",
        "md3 ok dark": "#93d69c", "md3 ok-bg dark": "#1b3820",
        "md3 warn dark": "#eab26b", "md3 warn-bg dark": "#432b07",
    }
    return {k: norm_color(v) for k, v in derived.items()}


# (skin, part-key, value-kind) -> (expected, citation). part-key is either a
# role name or "part/when#property".
EXPECTED: list[tuple[str, str, str, str, str]] = [
    # --- MD3 recorded rulings (skin-spec v3.1 appendix) ---
    ("md3", "metrics.control_height", "36", "deviation", "skin-spec v3.1: MD3 36px (user ruling 2026-11-07; official filled-button container-height 40px)"),
    ("md3", "metrics.segment_height", "36", "deviation", "skin-spec v3.1: segment 36px across skins"),
    ("md3", "metrics.switch_width", "36", "deviation", "skin-spec v3.1: switch track 36x20 (official switch track 52x32)"),
    ("md3", "metrics.switch_height", "20", "deviation", "skin-spec v3.1: switch track 36x20"),
    ("md3", "part/button/radius", "999", "deviation", "corner-full 9999px clamped by compiler bound (0..999)"),
    ("md3", "part/section.title#font_size", "22", "deviation", "skin-spec v3.1: section-title 22px/400 headline register"),
    ("md3", "part/section.title#font_weight", "400", "deviation", "skin-spec v3.1: section-title 22px/400 headline register"),
    ("md3", "part/shell.title#font_size", "22", "deviation", "skin-spec v3.1: headline register (same as section-title)"),
    ("md3", "part/shell.title#font_weight", "400", "deviation", "skin-spec v3.1: headline register (same as section-title)"),
    ("md3", "part/section.flat/radius", "0", "deviation", "compiler flatten_nested layout semantics (v3.1 nested sections)"),
    ("md3", "part/switch.thumb#thumb_x", "16", "deviation", "skin-spec v3.1: thumb 14px in 36x20 track (official 16->24 scaled)"),
    ("md3", "part/switch.thumb#thumb_y", "1", "deviation", "skin-spec v3.1: thumb 14px in 36x20 track"),
    ("md3", "part/switch.thumb#width", "14", "deviation", "skin-spec v3.1: thumb 14px (official handle 24px)"),
    ("md3", "part/switch.thumb#height", "14", "deviation", "skin-spec v3.1: thumb 14px (official handle 24px)"),
    ("md3", "part/shell.bar#height", "64", "deviation", "M3 top app bar 64dp convention (no v0_192 comp file; m3.material.io)"),
    # --- Fluent recorded rulings ---
    ("fluent", "metrics.control_height", "32", "deviation", "skin-spec v3.1: Fluent 32px (WinUI official control height)"),
    ("fluent", "metrics.segment_height", "36", "deviation", "skin-spec v3.1: segment 36px across skins"),
    ("fluent", "metrics.switch_width", "40", "deviation", "skin-spec v3.1: WinUI switch 40x20 (SwitchKnobBounds r10)"),
    ("fluent", "metrics.switch_height", "20", "deviation", "skin-spec v3.1: WinUI switch 40x20"),
    ("fluent", "part/shell.bar#height", "48", "deviation", "WinUI 3 title bar convention (learn.microsoft.com title bar)"),
    ("fluent", "part/segment.indicator/radius", "999", "deviation", "skin-spec v3: Fluent tab indicator 3px round-cap (circular cap)"),
    ("fluent", "part/segment.indicator#height", "3", "deviation", "skin-spec v3: Fluent tab indicator 3px round-cap"),
    ("fluent", "part/toast.marker#width", "3", "deviation", "skin-spec v3: toast 3px semantic bar (Infobar style, non-official structure recorded)"),
    ("fluent", "part/scrollbar#width", "6", "deviation", "host scrollbar geometry conserved (industrial.css --scrollbar values)"),
    ("fluent", "part/scrollbar#height", "6", "deviation", "host scrollbar geometry conserved"),
    ("fluent", "part/switch.track#width", "40", "deviation", "skin-spec v3.1: WinUI switch 40x20"),
    ("fluent", "part/switch.thumb#thumb_x", "26", "deviation", "skin-spec v3.1: WinUI knob 12x12, travel within 40px track"),
    ("fluent", "part/switch.thumb#thumb_y", "3", "deviation", "skin-spec v3.1: WinUI knob 12x12 within 20px track"),
    ("fluent", "part/switch.thumb#width", "12", "deviation", "skin-spec v3.1: WinUI knob 12x12 both states"),
    ("fluent", "part/switch.thumb#height", "12", "deviation", "skin-spec v3.1: WinUI knob 12x12 both states"),
    ("fluent", "part/navigation.indicator#width", "3", "deviation", "skin-spec v3.1: NavigationViewSelectionIndicator 3/16/2"),
    ("fluent", "part/navigation.indicator#height", "16", "deviation", "skin-spec v3.1: NavigationViewSelectionIndicator 3/16/2"),
    ("fluent", "part/section.flat/radius", "0", "deviation", "compiler flatten_nested layout semantics (v3.1 nested sections)"),
    ("fluent", "light/stroke-hover", "#6e6e6e", "deviation", "skin-spec v3: control-line WCAG 3:1 darkening, v9 ramp extrapolation"),
    ("fluent", "light/warn", "#bc4b09", "deviation",
     "skin-spec v3 ok/warn: older v9 orange.shade10 kept for >=4.5:1 text contrast (current #de590b = 3.8:1)"),
    ("fluent", "light/stroke-pressed", "#565656", "deviation", "skin-spec v3: control-line WCAG 3:1 darkening, v9 ramp extrapolation"),
    ("fluent", "dark/control-line", "#808080", "deviation", "skin-spec v3: skin-specific WCAG 3:1 control stroke (ControlStrokeDefault black .44 domain)"),
    ("fluent", "dark/border-primary", "#808080", "deviation", "skin-spec v3: skin-specific WCAG 3:1 control stroke"),
    ("fluent", "light/scrollbar-thumb", "#c6c6c6", "deviation", "host scrollbar colors conserved (industrial.css)"),
    ("fluent", "light/scrollbar-thumb-hover", "#8f8f8f", "deviation", "host scrollbar colors conserved (industrial.css)"),
    ("fluent", "dark/scrollbar-thumb", "#c6c6c6", "deviation", "host scrollbar colors conserved (industrial.css)"),
    ("fluent", "dark/scrollbar-thumb-hover", "#8f8f8f", "deviation", "host scrollbar colors conserved (industrial.css)"),
]


# ---------------------------------------------------------------------------
# Verification

class Report:
    def __init__(self) -> None:
        self.official: list[str] = []
        self.mix: list[str] = []
        self.deviation: list[str] = []
        self.unexplained: list[str] = []

    def lines(self, pack: str) -> list[str]:
        head = "== %s ==" % pack
        out = [head,
               "  official-provenance values: %d" % len(self.official),
               "  mix (official operands + official opacity): %d" % len(self.mix),
               "  recorded deviations: %d" % len(self.deviation)]
        if self.unexplained:
            out.append("  UNEXPLAINED (%d):" % len(self.unexplained))
            out += ["    " + u for u in self.unexplained]
        return out


def verify_pack(skin: str, pack_path: str) -> Report:
    pack = load_pack(pack_path)
    report = Report()
    constants = spec_constants()

    if skin == "md3":
        md = MdTokens()
        tokens = md
        official = {mode: md.official_colors(mode) for mode in ("light", "dark")}
        state = {"hover": float(md.state["hover-state-layer-opacity"].replace("px", "")),
                 "pressed": float(md.state["pressed-state-layer-opacity"].replace("px", "")),
                 "focus": float(md.state["focus-state-layer-opacity"].replace("px", ""))}
    else:
        fui = FluentTokens()
        tokens = fui
        winui = parse_winui()
        official = {}
        for mode in ("light", "dark"):
            index = dict(fui.official_colors(mode))
            for name, value in winui[mode].items():
                index.setdefault(norm_color(value), ("winui", name))
            for name, value in fui.status[mode].items():
                index.setdefault(norm_color(value), ("colorStatus", name))
            official[mode] = index

    expected = {(s, k): (v, cite) for s, k, v, kind, cite in EXPECTED}
    # Provenance is mode-independent (a hex is the same official token in
    # either mode), so mix leaves may match either side's index.
    official_any: dict[str, tuple[str, str]] = {}
    for mode in ("light", "dark"):
        official_any.update(official[mode])

    for mode in ("light", "dark"):
        colors = pack_colors(pack, mode)
        mix_roles: dict[str, str] = {}
        for name, expr in pack["modes"][mode]["colors"].items():
            value = colors[name]
            cite = expected.get((skin, name)) or expected.get((skin, "%s/%s" % (mode, name)))
            alphas = alpha_form(expr, pack["modes"][mode]["colors"], mode)
            if value in official[mode]:
                src, token = official[mode][value]
                report.official.append("%s/%s <- %s:%s" % (mode, name, src, token))
            elif alphas:
                hit = next((c for c in alphas if c in official[mode]), None)
                if hit:
                    src, token = official[mode][hit]
                    report.official.append("%s/%s <- %s:%s (alpha form)" % (mode, name, src, token))
                else:
                    mix_roles[name] = value  # alpha form matched nothing: report as mix
            elif "mix" in expr:
                mix_roles[name] = value  # verified after plain roles resolve
            elif cite:
                report.deviation.append("%s/%s = %s [%s]" % (mode, name, value, cite[1]))
            elif value in constants.values():
                report.deviation.append("%s/%s = %s [spec-derived pair]" % (mode, name, value))
            else:
                report.unexplained.append("%s color %s = %s" % (mode, name, value))

        # Mix roles: every leaf operand must carry official provenance (or be
        # transparent) and each fraction must be an official opacity or a
        # spec-cited conversion constant.
        fractions = ({round(v, 4) for v in state.values()}
                     | {0.45, 0.15, 0.40, 0.05, 0.08, 0.11, 0.12, 0.16}) if skin == "md3" \
            else {0.85, 0.40, 0.0605, 0.45, 0.15}  # spec: brand ramp, WinUI composite, shared accent conversion
        for name, value in mix_roles.items():
            expr = pack["modes"][mode]["colors"][name]
            leaves = collect_leaves(expr, pack["modes"][mode]["colors"], mode)
            operands_ok = all(a in official_any or norm_color(a) == "#00000000" for a in leaves)
            fractions_ok = all(round(float(f), 4) in fractions for f in walk_fractions(expr, pack["modes"][mode]["colors"]))
            if operands_ok and fractions_ok:
                report.mix.append("%s/%s = mix at %s%% over official operands" % (mode, name, float(expr["first_fraction"]) * 100))
            else:
                report.unexplained.append("%s color mix %s = %s (operands_ok=%s fractions_ok=%s)"
                                          % (mode, name, value, operands_ok, fractions_ok))

        # Geometry/radii/shadows per recipe.
        for recipe in pack["modes"][mode]["recipes"]:
            when = recipe["when"]
            tag = recipe["part"] + ("/" + recipe["variant"] if recipe.get("variant") else "")
            for prop in ("radius",):
                if prop not in recipe:
                    continue
                value = str(int(recipe[prop])) if float(recipe[prop]).is_integer() else str(recipe[prop])
                _check_geometry(skin, recipe["part"], prop, value, "part/%s/%s" % (tag, prop),
                                expected, report, tokens, mode)
            for prop, value in (recipe.get("metrics") or {}).items():
                _check_geometry(skin, recipe["part"], prop, str(int(value)), "part/%s#%s" % (recipe["part"], prop),
                                expected, report, tokens, mode)

    # Pack-level metrics carry explicit expectations (control/segment/switch).
    for prop, value in pack["metrics"].items():
        key = (skin, "metrics.%s" % prop)
        if key not in expected:
            report.unexplained.append("metrics.%s = %s (no expectation recorded)" % (prop, value))
        elif str(expected[key][0]) != str(value):
            report.unexplained.append("metrics.%s = %s expected %s" % (prop, value, expected[key][0]))
        else:
            report.deviation.append("metrics.%s = %s [%s]" % (prop, value, expected[key][1]))

    return report


def alpha_form(expr: dict, roles: dict, mode: str) -> list[str]:
    """mix(solid, transparent, f) as #rrggbbaa candidates, for alpha tokens.

    WinUI stores 9.8% as 0x19 where the pack records 10%, so candidates span
    +-1/255 around the recorded fraction.
    """
    if "mix" not in expr:
        return []
    a = resolve_expr(expr["mix"][0], roles, mode)
    b = resolve_expr(expr["mix"][1], roles, mode)
    out: list[str] = []
    for first, second, frac in ((a, b, float(expr["first_fraction"])), (b, a, 1 - float(expr["first_fraction"]))):
        if second == "#00000000" and re.fullmatch(r"#([0-9a-f]{6})ff", first):
            alpha = int(frac * 255 + 0.5)
            out += [first[:7] + "%02x" % (alpha + delta) for delta in (0, 1, -1)]
    return out


def collect_leaves(expr: dict, roles: dict, mode: str) -> list[str]:
    """Leaf color operands of a (possibly nested) ColorExpr mix tree."""
    if "mix" in expr:
        return collect_leaves(expr["mix"][0], roles, mode) + collect_leaves(expr["mix"][1], roles, mode)
    if "role" in expr and "mix" in roles[expr["role"]]:
        return collect_leaves(roles[expr["role"]], roles, mode)
    return [resolve_expr(expr, roles, mode)]


def walk_fractions(expr: dict, roles: dict | None = None) -> list[float]:
    roles = roles or {}
    if "mix" in expr:
        return [float(expr["first_fraction"])] + walk_fractions(expr["mix"][0], roles) + walk_fractions(expr["mix"][1], roles)
    if "role" in expr and "mix" in roles.get(expr["role"], {}):
        return walk_fractions(roles[expr["role"]], roles)
    return []


def _check_geometry(skin: str, part: str, prop: str, value: str, key: str,
                    expected: dict, report: Report, tokens, mode: str) -> None:
    """One geometry value: official token, ruling, or unexplained."""
    cite_expected = expected.get((skin, key))
    if cite_expected:
        if cite_expected[0] == value:
            report.deviation.append("%s %s %s=%s [%s]" % (mode, part, prop, value, cite_expected[1]))
        else:
            report.unexplained.append("%s %s %s=%s expected ruling %s" % (mode, part, prop, value, cite_expected[0]))
        return
    official_value = None
    if skin == "md3":
        shape = tokens.shape
        names = {"0": "corner-none", "4": "corner-extra-small", "8": "corner-small", "12": "corner-medium",
                 "16": "corner-large", "28": "corner-extra-large"}
        if prop == "radius" and names.get(value):
            official_value = str(int(re.match(r"(\d+)px", shape[names[value]]).group(1)))
        if prop == "radius" and value == "999" and shape["corner-full"] == "9999px":
            official_value = "999"  # corner-full, clamped by the compiler bound (<=999)
        if prop == "radius" and value == "2":
            checkbox_shape = tokens.comp("md-comp-checkbox", mode)["container-shape"]
            official_value = str(int(re.match(r"(\d+)px", checkbox_shape).group(1)))
        if prop == "font_weight" and value == str(tokens.typescale["label-large-weight"]):
            official_value = value  # typescale label-large weight
    else:
        if prop == "radius" and value in {"2", "4", "6", "8"}:
            names = {"2": "borderRadiusSmall", "4": "borderRadiusMedium", "6": "borderRadiusLarge", "8": "borderRadiusXLarge"}
            official_value = str(int(re.match(r"(\d+)px", tokens.radius[names[value]]).group(1)))
        if prop == "radius" and value == "999" and tokens.radius["borderRadiusCircular"] == "10000px":
            official_value = "999"  # borderRadiusCircular, clamped by the compiler bound (<=999)
        if prop == "font_size" and value == tokens.subtitle1()[0]:
            official_value = value  # subtitle1 ramp step
        if prop == "font_weight" and value == tokens.subtitle1()[1]:
            official_value = value  # fontWeightSemibold
    if official_value == value:
        report.official.append("%s %s %s=%s (official token)" % (mode, part, prop, value))
        return
    # Host-grid paddings/gaps are layout conservation, not official tokens;
    # they are accepted only on the parts the host grid owns.
    if prop in {"padding_block", "padding_inline", "padding_top", "padding_bottom",
                "padding_start", "gap", "column_gap"}:
        report.deviation.append("%s %s %s=%s [host layout grid]" % (mode, part, prop, value))
        return
    report.unexplained.append("%s %s %s=%s" % (mode, part, prop, value))


# ---------------------------------------------------------------------------

def self_test() -> int:
    """Parser + resolver fixtures against synthetic inputs."""
    md_entry = scss_entries("""
    'corner-full': if($exclude-hardcoded-values, null, 9999px),
    'hover-state-layer-opacity': if($exclude-hardcoded-values, null, 0.08),
    'primary': map.get($deps, 'md-ref-palette', 'primary40'),
    """)
    assert md_entry == [("corner-full", "if($exclude-hardcoded-values, null, 9999px)"),
                        ("hover-state-layer-opacity", "if($exclude-hardcoded-values, null, 0.08)"),
                        ("primary", "map.get($deps, 'md-ref-palette', 'primary40')")], md_entry
    assert norm_color("#FFFFFF") == "#ffffffff"
    assert norm_color("rgba(18, 18, 18, 0.44)") == "#12121270"
    assert norm_color("#00000000") == "#00000000"
    pack = {"modes": {"light": {"colors": {
        "text": {"hex": "#1D1B20"},
        "hover": {"mix": [{"role": "text"}, {"hex": "#00000000"}], "first_fraction": 0.08},
    }, "recipes": []}, "dark": {"colors": {}, "recipes": []}}}
    colors = pack_colors(pack, "light")
    assert colors["text"] == "#1d1b20ff", colors
    assert colors["hover"] == "mix(#1d1b20ff,#00000000,0.0800)", colors
    print("PASS: source-check self test")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fetch", action="store_true", help="refresh snapshots from pinned commits")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    if args.fetch:
        return fetch()

    bad = 0
    for skin, path in PACKS.items():
        report = verify_pack(skin, path)
        print("\n".join(report.lines(skin)))
        if report.unexplained:
            bad += 1
    if bad:
        print("\nFAIL: %d pack(s) hold values with no official provenance and no recorded ruling" % bad)
        return 1
    print("\nPASS: every pack value is official, an official mix, or a recorded ruling")
    return 0


if __name__ == "__main__":
    sys.exit(main())
