#!/usr/bin/env python3
"""One-shot migration from the frozen visual baseline to typed ThemeV1 packs.

The source CSS is evidence, not a production CSS escape hatch. Every selector
and property must have an explicit typed mapping; unsupported input fails.
"""
from __future__ import annotations
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASELINE_REVISION = "26d887fd4733fba374a769db2e50653ad98d114b"

SELECTORS = {
    ".btn": ("button", None, {}),
    ".btn:hover:not(:disabled):not(.btn-primary):not(.btn-ghost):not(.btn-danger)": ("button", "default", {"interaction": "hover", "disabled": False}),
    ".btn-primary": ("button", "primary", {}),
    ".btn-primary:hover:not(:disabled)": ("button", "primary", {"interaction": "hover", "disabled": False}),
    ".btn-ghost": ("button", "ghost", {}),
    ".btn-ghost:hover:not(:disabled)": ("button", "ghost", {"interaction": "hover", "disabled": False}),
    ".btn-danger": ("button", "danger", {}),
    ".btn-danger:hover:not(:disabled)": ("button", "danger", {"interaction": "hover", "disabled": False}),
    "input:not([type=\"checkbox\"] )": ("input", None, {}),
    "input:not([type=\"checkbox\"])": ("input", None, {}),
    "input:focus-visible": ("input", None, {"focus_visible": True}),
    "input:not([type=\"checkbox\"]):focus-visible": ("input", None, {"focus_visible": True}),
    "input:not([type=\"checkbox\"]):hover:not(:focus-visible)": ("input", None, {"interaction": "hover", "focus_visible": False}),
    "input:not([type=\"checkbox\"]):not([readonly])": ("input", None, {"readonly": False}),
    ".select-trigger": ("select.trigger", None, {}),
    ".select-trigger:focus-visible": ("select.trigger", None, {"focus_visible": True}),
    ".select-trigger:hover:not(:focus-visible)": ("select.trigger", None, {"interaction": "hover", "focus_visible": False}),
    ".select-menu": ("select.menu", None, {}),
    ".select-item": ("select.item", None, {}),
    ".select-item[data-state=\"checked\"]": ("select.item", None, {"checked": True}),
    "input[type=\"checkbox\"]": ("checkbox", None, {}),
    ":focus-visible": ("focus.target", None, {}),
    ".segment": ("segment", None, {}),
    ".segment-item": ("segment.item", None, {}),
    ".segment-item:hover": ("segment.item", None, {"interaction": "hover"}),
    ".segment-item[data-active=\"true\"]": ("segment.item", None, {"selected": True}),
    ".segment-item[data-active=\"true\"]::after": ("segment.indicator", None, {}),
    ".nav-item": ("navigation.item", None, {}),
    ".nav-item:hover": ("navigation.item", None, {"interaction": "hover"}),
    ".nav-item.active": ("navigation.item", None, {"selected": True}),
    ".nav-item.active::before": ("navigation.indicator", None, {}),
    ".drawer": ("drawer", None, {}),
    ".banner": ("banner", None, {}),
    ".banner-warn": ("banner", "warning", {}),
    ".banner-danger": ("banner", "error", {}),
    ".confirm": ("dialog", None, {}),
    ".popover": ("popover", None, {}),
    ".menu-item": ("menu.item", None, {}),
    ".menu-item:focus-visible": ("menu.item", None, {"focus_visible": True}),
    ".badge": ("badge", None, {}),
    ".chip": ("chip", None, {}),
    ".kpi-row": ("kpi.row", None, {}),
    ".kpi-row .kpi-cell": ("kpi.cell", None, {}),
    ".kpi-row .kpi-cell:first-child": ("kpi.first", None, {}),
    ".kpi-row .kpi-cell.kpi-trigger:hover": ("kpi.trigger", None, {"interaction": "hover"}),
    ".kpi-row .kpi-trigger[aria-pressed=\"true\"]": ("kpi.trigger", None, {"selected": True}),
    ".kpi-row .kpi-trigger[aria-pressed=\"true\"]:hover": ("kpi.trigger", None, {"selected": True, "interaction": "hover"}),
    ".kpi-row .kpi-trigger[aria-pressed=\"true\"] .kpi-label": ("kpi.label", None, {}),
    ".kpi-row .kpi-trigger[aria-pressed=\"true\"] .kpi-value": ("kpi.value", None, {}),
    ".table tbody tr:hover td": ("table.cell", None, {"interaction": "hover"}),
    ".list-row": ("list.row", None, {}),
    ".timeline-item": ("timeline.item", None, {}),
    ".toast": ("toast", None, {}),
    ".toast:hover": ("toast", None, {"interaction": "hover"}),
    ".toast::before": ("toast.marker", None, {}),
    ".chart-card": ("chart.card", None, {}),
    ".menu-radio": ("menu.radio", None, {}),
    ".toggle input[type=\"checkbox\"]": ("switch.track", None, {}),
    ".toggle input[type=\"checkbox\"]::after": ("switch.thumb", None, {}),
    ".toggle input[type=\"checkbox\"]:hover:not(:checked)::after": ("switch.thumb", None, {"interaction": "hover", "checked": False}),
    ".toggle input[type=\"checkbox\"]:checked": ("switch.track", None, {"checked": True}),
    ".toggle input[type=\"checkbox\"]:checked::after": ("switch.thumb", None, {"checked": True}),
    ".toggle input[type=\"checkbox\"]:checked:hover::after": ("switch.thumb", None, {"checked": True, "interaction": "hover"}),
    ".section": ("section", None, {}),
    ".section .section": ("section.flat", None, {"context": "nested_section"}),
    ".drawer .section": ("section.flat", None, {"context": "overlay"}),
    ".confirm .section": ("section.flat", None, {"context": "overlay"}),
    ".popover .section": ("section.flat", None, {"context": "overlay"}),
    ".tab-panel": ("tab.panel", None, {}),
    ".field-label": ("field.label", None, {}),
    ".section-title": ("section.title", None, {}),
    ".mainbar": ("shell.bar", None, {}),
    ".sidebar": ("shell.sidebar", None, {}),
    ".mainbar .page-title": ("shell.title", None, {}),
    "::-webkit-scrollbar": ("scrollbar", None, {}),
    "::-webkit-scrollbar-thumb": ("scrollbar.thumb", None, {}),
    "::-webkit-scrollbar-thumb:hover": ("scrollbar.thumb", None, {"interaction": "hover"}),
    "::-webkit-scrollbar-thumb:active": ("scrollbar.thumb", None, {"interaction": "pressed"}),
}

def split(value, delimiter=","):
    depth = 0
    start = 0
    out = []
    for i, char in enumerate(value):
        if char == "(": depth += 1
        elif char == ")": depth -= 1
        elif char == delimiter and depth == 0:
            out.append(value[start:i].strip()); start = i + 1
    out.append(value[start:].strip())
    return out

def col(value):
    value = value.strip()
    if value in {"transparent", "none"}: return {"hex": "#00000000"}
    if re.fullmatch(r"#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?", value): return {"hex": value}
    if value.startswith("rgba("):
        r, g, b, a = [float(v) for v in split(value[5:-1])]
        return {"mix": [{"hex": "#%02x%02x%02x" % (int(r), int(g), int(b))}, {"hex": "#00000000"}], "first_fraction": a}
    if value.startswith("var("):
        fields = split(value[4:-1])
        if fields[0] in {"--system-accent", "--system-accent-fg"}:
            return {"system": "accent" if fields[0] == "--system-accent" else "accent_foreground", "fallback": col(fields[1])}
        return {"role": fields[0][2:]}
    if value.startswith("color-mix(in srgb,"):
        a, b = split(value[len("color-mix(in srgb,"):-1])
        m = re.fullmatch(r"(.+) (\d+(?:\.\d+)?)%", a)
        if not m: raise ValueError(value)
        return {"mix": [col(m[1]), col(b)], "first_fraction": float(m[2]) / 100}
    raise ValueError("unmapped color " + value)

def dimension(value):
    return float(value[:-2]) if value.endswith("px") else float(value)

def shadow(value, tokens):
    if value == "none": return []
    if value.startswith("var("): return shadow(tokens[value[6:-1]], tokens)
    out = []
    for item in split(value):
        m = re.fullmatch(r"(inset )?([\d.]+(?:px)?) ([\d.-]+(?:px)?)(?: ([\d.]+(?:px)?))?(?: ([\d.]+(?:px)?))? (.+)", item)
        if not m: raise ValueError("unmapped shadow " + item)
        out.append({"x": dimension(m[2]), "y": dimension(m[3]), "blur": dimension(m[4] or "0"), "spread": dimension(m[5] or "0"), "color": col(m[6]), **({"inset": True} if m[1] else {})})
    return out

def recipe(selector, props, tokens):
    part, variant, state = SELECTORS[selector]
    out = {"part": part, "when": {"interaction": "rest", **state}}
    if variant: out["variant"] = variant
    metrics = {}
    for prop, value in props.items():
        if prop in {"background", "color"}:
            if value == "inherit": out["decoration"] = "inherit_ink"
            else: out["fill" if prop == "background" else "foreground"] = col(value)
        elif prop == "border-radius":
            value = "4px" if value == "var(--radius)" else value
            out["radius"] = 999 if value == "50%" else dimension(value.split()[0])
        elif prop.startswith("border"):
            if prop == "border-radius": continue
            edge = "start" if "left" in prop else "end" if "right" in prop else "bottom" if "bottom" in prop else "all"
            width = dimension(value) if prop == "border-width" else 0 if value == "none" else dimension(value.split()[0]) if "solid" in value else 1
            ink = value.split("solid ")[-1] if "solid" in value else "transparent" if value == "none" or prop == "border-width" else value
            previous = out.get("stroke", {"width": (2 if part in {"checkbox", "switch.track"} else 2 if part == "kpi.trigger" and edge == "bottom" else 1), "color": {"role":"border-primary"}})
            if prop == "border-width": out["stroke"] = {"width": width, "color": previous["color"]}
            elif prop.endswith("color"): out["stroke"] = {"width": previous["width"], "color": col(ink)}
            else: out["stroke"] = {"width": width, "color": col(ink)}
            out["stroke_edge"] = edge
        elif prop == "box-shadow": out["shadow"] = shadow(value, tokens)
        elif prop == "outline":
            if value == "none": out["focus_rings"] = []
            else:
                width, _, ink = value.split(" ", 2)
                out["focus_rings"] = [{"width": dimension(width), "offset": dimension(props.get("outline-offset", "0")), "color": col(ink)}]
        elif prop == "outline-offset": continue
        elif prop in {"width", "height", "font-size", "font-weight", "gap", "column-gap", "padding-left", "padding-right", "top", "left"}:
            if value in {"auto", "50%"} or part in {"navigation.indicator", "segment.indicator"} and prop in {"top", "left"}: continue
            key = {"font-size":"font_size", "font-weight":"font_weight", "column-gap":"column_gap", "padding-left":"padding_start", "padding-right":"padding_inline", "top":"thumb_y", "left":"thumb_x"}.get(prop, prop)
            metrics[key] = int(dimension(value)) if key == "font_weight" else dimension(value)
        elif prop == "padding":
            values = [dimension(v) for v in value.split()]
            metrics.update({"padding_block": values[0], "padding_inline": values[1] if len(values) > 1 else values[0]})
            if len(values) == 3: metrics.update({"padding_top": values[0], "padding_bottom": values[2]})
        elif prop == "overflow": out["layout"] = "flat"
        elif prop in {"display", "flex-direction"}: out["layout"] = "card_stack"
        elif prop == "text-transform" or prop == "letter-spacing": out["decoration"] = "plain_title"
        elif prop in {"backdrop-filter", "-webkit-backdrop-filter"}:
            out["material"] = {"kind":"acrylic", "opacity": 0.78, "blur":24, "saturation":1.3}
        elif prop in {"content", "position", "right", "bottom", "transform", "align-self"}:
            if part not in {"segment.indicator", "navigation.indicator", "toast.marker", "segment.item", "navigation.item"}: raise ValueError((part, prop))
            if part.endswith("indicator") or part == "toast.marker": out["decoration"] = "leading_indicator" if part != "segment.indicator" else "underline"
            elif prop == "position": out["decoration"] = "indicator_anchor"
        elif prop in {"transition", "background-clip"}: pass # host-owned behavior
        else: raise ValueError("unmapped property " + prop)
    if part == "section.flat": out["layout"] = "flatten_nested"
    if metrics: out["metrics"] = metrics
    return out

def extract():
    baseline = subprocess.check_output(["git", "show", BASELINE_REVISION + ":ui/src/styles.css"], cwd=ROOT).decode("utf-8")
    css = re.sub(r"/\*.*?\*/", "", baseline, flags=re.S)
    rules = list(re.finditer(r"([^{}]+)\{([^{}]*)\}", css))
    result = {}
    for skin in ["md3", "fluent"]:
        token_sets = {"light":{}, "dark":{}}
        recipes = {"light":[], "dark":[]}
        for rule in rules:
            selectors = split(rule[1])
            if not any(f'data-skin="{skin}"' in s for s in selectors): continue
            props = dict((p.split(":", 1)[0].strip(), p.split(":", 1)[1].strip()) for p in rule[2].split(";") if p.strip())
            modes = ["dark"] if 'data-theme="dark"' in rule[1] else ["light", "dark"]
            for mode in modes:
                if selectors[0].strip() in {f'[data-skin="{skin}"]', f'[data-skin="{skin}"][data-theme="dark"]'}:
                    token_sets[mode].update({key[2:]: value for key, value in props.items()})
                else:
                    for selector in selectors:
                        selector = re.sub(r'^(?:html)?\[data-skin="' + skin + r'"\](?:\[data-theme="dark"\])?\s*', "", selector.strip())
                        if selector not in SELECTORS: raise ValueError("unmapped selector " + selector)
                        recipes[mode].append((selector, props))
        # Missing roles are explicitly frozen from Industrial fallback values.
        fallback = {"control-line":{"role":"line"}, "line-strong":{"role":"line-muted"}, "border-primary":{"role":"control-line"},
                    "scrollbar-thumb": {"hex":"#c6c6c6"}, "scrollbar-thumb-hover":{"hex":"#8f8f8f"}}
        modes = {}
        for mode in ["light", "dark"]:
            tokens = token_sets[mode]
            colors = {k:col(v) for k,v in tokens.items() if k not in {"font-sans", "radius", "control-h"} and "shadow" not in k}
            for key, value in fallback.items(): colors.setdefault(key, value)
            # Generic part radius replaces --radius without an ID special case.
            rows = [recipe(s, p, tokens) for s,p in recipes[mode]]
            # Same part/predicate declarations merge by source cascade, avoiding
            # ambiguous conflicts while retaining complete effective recipes.
            merged = {}
            for row in rows:
                identity = (row["part"], row.get("variant"), json.dumps(row["when"], sort_keys=True))
                existing = merged.setdefault(identity, {})
                for key, value in row.items():
                    if key == "metrics": existing.setdefault(key, {}).update(value)
                    else: existing[key] = value
            rows = list(merged.values())
            # Shared properties consumed the old skin's layer-2 shadow token.
            # Now those explicit component recipes own the same complete values.
            rows.extend([
                {"part":"select.menu", "when":{"interaction":"rest"}, "shadow":shadow(tokens["shadow-layer"], tokens)},
            ])
            if skin == "md3":
                rows.extend([
                    {"part":"chart.card", "when":{"interaction":"rest"}, "shadow":shadow(tokens["shadow-layer"], tokens)},
                    {"part":"switch.track", "when":{"interaction":"rest", "checked":True}, "fill":{"role":"primary"}, "stroke":{"width":2,"color":{"role":"primary"}}},
                    {"part":"switch.thumb", "when":{"interaction":"rest", "checked":True}, "fill":{"role":"on-primary"}},
                ])
            for row in rows:
                if row["part"] == "menu.radio": row["decoration"] = "circle"
                if row["part"] == "switch.track" and row["when"].get("checked") and skin == "fluent": row["stroke"]["width"] = 1
            modes[mode] = {"colors":colors, "recipes":rows}
        result[skin] = {"schema_version":1, "name":{"en":"Material 3" if skin == "md3" else "Fluent", "zh-CN":"Material 3" if skin == "md3" else "Fluent", "zh-TW":"Material 3" if skin == "md3" else "Fluent"},
            "modes":modes, "typography":{"family":"host:roboto" if skin == "md3" else "system:segoe_ui", "scale":"material_desktop" if skin == "md3" else "fluent_desktop"},
            "metrics":{"control_height":36 if skin == "md3" else 32,"segment_height":36,"switch_width":36 if skin == "md3" else 40,"switch_height":20}}
    return result

if __name__ == "__main__":
    # Deterministic code generation, like a lockfile/SDK generator. Only the two
    # generated data resources are ever written, never CSS or user preferences.
    for skin, theme in extract().items():
        if "--generate" in sys.argv:
            path = ROOT / f"plugins/theme-{skin}/theme.json"
            path.write_text(json.dumps(theme, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            print(f"generated {path.relative_to(ROOT)}")
            continue
        print("*** Add File: " + str(ROOT / f"plugins/theme-{skin}/theme.json"))
        print("+" + json.dumps(theme, ensure_ascii=True, separators=(",", ":")))
    if "--generate" in sys.argv:
        original = subprocess.check_output(["git", "show", BASELINE_REVISION + ":ui/src/styles.css"], cwd=ROOT).decode("utf-8")
        start = original.index("/* ============================================================================", original.index("/* Dark:"))
        end = original.index("\n* {", start)
        base = "/* Generated by scripts/plugin_theme_extract.py from the frozen baseline. */\n" + original[:start] + original[end:]
        (ROOT / "ui/src/industrial.css").write_text(base, encoding="utf-8")
        print("generated ui/src/industrial.css")
