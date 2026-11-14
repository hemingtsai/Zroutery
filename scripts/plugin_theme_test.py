#!/usr/bin/env python3
"""Real computed-style parity of the shipped theme matrix against a frozen baseline.

Runs only local synthetic pages in an isolated browser profile. Tests actual
hover, pressed, checked+hover and keyboard focus at 820x560, both color modes,
plus the unchanged Industrial base. Native accent is a separately scoped gate.

The baseline was the pre-plugin hand CSS at commit 26d887f while the ThemeV1
migration was being compared. Since the 2026-11-14
official-source re-pin (skin-spec v3.2) the baseline is the accepted shipped
stylesheet itself, frozen by `--refreeze` into
docs/development/plugin-theme-baseline-v3.2.css. Differences therefore mean
the bundle's visuals drifted from the last accepted state — a pack edit, a
compiler change, a class change — and a reviewer re-freezes only with the
change that bought it. Pack values themselves are governed separately by
scripts/plugin_theme_source_check.py.

This new baseline checks regression against the accepted v3.2 visuals; zero
differences does not prove parity with the old CSS. Use --legacy-baseline to
retain that separate comparison (the initial run reported 822 differences).

Usage: python scripts/plugin_theme_test.py [--refreeze | --legacy-baseline]
"""
from __future__ import annotations
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path
import ui_layout_test as browser

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target/plugin-temp/theme-parity"
BASELINE = ROOT / "docs/development/plugin-theme-baseline-v3.2.css"
PROPERTIES = ["background-color", "color", "border-top-color", "border-left-color", "border-right-color", "border-bottom-color",
    "border-top-width", "border-left-width", "border-right-width", "border-bottom-width", "border-radius", "box-shadow",
    "outline", "outline-offset", "width", "height", "padding-top", "padding-bottom", "padding-left", "padding-right",
    "font-size", "font-weight", "font-family", "gap", "column-gap", "text-transform", "letter-spacing", "backdrop-filter",
    "left", "top", "content", "position", "transform", "align-self", "background-clip"]

FIXTURE = r"""
<div class="shell"><nav class="sidebar"><button class="nav-item active">Navigation 長翻譯</button></nav>
<div class="main"><header class="mainbar"><h1 class="page-title">Theme matrix</h1></header><main class="content"><div class="page">
<section class="section"><h2 class="section-title">Section</h2><div class="controls">
<button class="btn">Default</button><button class="btn btn-primary">Primary</button><button class="btn btn-ghost">Ghost</button><button class="btn btn-danger">Danger</button><button class="btn" disabled>Disabled</button>
<label class="field"><span class="field-label">Input</span><input value="input"></label><input readonly value="readonly">
<input type="checkbox"><label class="toggle"><input type="checkbox"></label></div>
<div class="segment"><button class="segment-item">Tab</button><button class="segment-item" data-active="true">Selected</button></div>
<div class="select-trigger" tabindex="0">Select</div><div class="select-menu" style="position:relative"><div class="select-item" data-state="checked" tabindex="0">Checked</div></div>
<section class="section">Nested section</section><div class="kpi-row"><button class="kpi-cell kpi-trigger" aria-pressed="true"><span class="kpi-label">KPI</span><span class="kpi-value">12</span></button><div class="kpi-cell">Other</div></div>
<table class="table"><tbody><tr><td>Table cell</td></tr></tbody></table><button class="list-row">List</button><div class="timeline-item" tabindex="0">Timeline</div><div class="badge">Badge</div><div class="chip">Chip</div><div class="banner banner-warn">Warning</div><div class="banner banner-danger">Danger</div><div class="chart-card">Chart card</div></section>
<div class="tab-panel"><section class="section">Panel</section></div>
<div class="popover" style="position:relative;top:0;right:auto"><button class="menu-item">Menu item</button><span class="menu-radio"></span><section class="section">Overlay section</section></div>
<div class="drawer" style="position:relative;inset:auto;transform:none"><section class="section">Drawer section</section></div>
<div class="confirm" style="position:relative;inset:auto;transform:none"><section class="section">Dialog</section></div>
<div class="toast" style="position:relative;inset:auto;transform:none">Toast</div>
</div></main></div></div>
"""

SAMPLES = [
    (".btn:not(.btn-primary):not(.btn-ghost):not(.btn-danger):not(:disabled)", None), (".btn-primary", None), (".btn-ghost", None), (".btn-danger", None), (".btn:disabled", None),
    ("input:not([type=checkbox]):not([readonly])", None), ("input[readonly]", None), ("input[type=checkbox]:not(.toggle input)", None),
    (".toggle input", None), (".toggle input", "::after"), (".segment", None), (".segment-item[data-active]", None), (".segment-item[data-active]", "::after"),
    (".nav-item", None), (".nav-item", "::before"), (".select-trigger", None), (".select-menu", None), (".select-item", None),
    (".section", None), (".section .section", None), (".section-title", None), (".field-label", None), (".kpi-row", None), (".kpi-cell", None),
    (".kpi-label", None), (".kpi-value", None), (".table td", None), (".list-row", None), (".timeline-item", None), (".toast", None), (".toast", "::before"),
    (".chart-card", None), (".menu-radio", None), (".badge", None), (".chip", None), (".banner-warn", None), (".banner-danger", None),
    (".popover", None), (".menu-item", None), (".drawer", None), (".confirm", None), (".tab-panel", None), (".mainbar", None), (".sidebar", None), (".page-title", None),
]

def main():
    argv = sys.argv[1:]
    assets = list((ROOT / "ui/dist/assets").glob("index-*.css"))
    if len(assets) != 1: raise RuntimeError("build ui first")
    compiled = assets[0].read_text(encoding="utf-8")
    if "--refreeze" in argv:
        BASELINE.write_text(
            "<!-- Frozen shipped stylesheet (industrial + compiled ThemeV1 packs).\n"
            "     Regenerate only with the reviewed change that shifted the\n"
            "     visuals: pnpm --dir ui build && python scripts/plugin_theme_test.py --refreeze -->\n"
            + compiled, encoding="utf-8")
        print(f"refrozen: {BASELINE.relative_to(ROOT)} ({len(compiled):,} B)")
        return 0
    baseline = (subprocess.check_output(
        ["git", "show", "26d887fd4733fba374a769db2e50653ad98d114b:ui/src/styles.css"], cwd=ROOT
    ).decode("utf-8") if "--legacy-baseline" in argv else BASELINE.read_text(encoding="utf-8"))
    TARGET.mkdir(parents=True, exist_ok=True)
    for kind, css in [("baseline", baseline), ("plugin", compiled)]:
        (TARGET / f"{kind}.html").write_text(f'<!doctype html><html><head><meta charset="utf-8"><style>{css}</style><style>*,*::before,*::after{{transition:none!important;animation:none!important}}</style></head><body>{FIXTURE}</body></html>', encoding="utf-8")
    executable = browser.find_chromium()
    if not executable: return 2
    server, port = browser.serve(str(TARGET))
    failures = []
    measurements = {}
    profile = tempfile.mkdtemp(prefix="plugin-theme-", dir=ROOT / "target/plugin-temp")
    try:
        debugging = browser.free_port()
        process = subprocess.Popen([executable, "--headless", "--disable-gpu", "--no-first-run", "--no-default-browser-check", f"--remote-debugging-port={debugging}", f"--user-data-dir={profile}", f"http://127.0.0.1:{port}/baseline.html"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            dt = browser.DevTools(browser.page_target(debugging, f"http://127.0.0.1:{port}/"))
            dt.call("Page.enable")
            dt.call("Emulation.setDeviceMetricsOverride", {"width":820,"height":560,"deviceScaleFactor":1,"mobile":False})
            for skin in ["industrial", "md3", "fluent"]:
                for mode in ["light", "dark"]:
                    for kind in ["baseline", "plugin"]:
                        dt.call("Page.navigate", {"url":f"http://127.0.0.1:{port}/{kind}.html"})
                        dt.wait_for("document.querySelector('.toggle input') && document.readyState==='complete'", "fixture", timeout=30.0)
                        dt.evaluate("document.documentElement.dataset.theme=%s; document.documentElement.style.setProperty('--system-accent','#0078d4');document.documentElement.style.setProperty('--system-accent-fg','#ffffff');" % json.dumps(mode))
                        if skin != "industrial":
                            # Both sides use the shipped attribute interface;
                            # industrial styles are global and need no mark.
                            attribute = "skin" if kind == "baseline" and "--legacy-baseline" in argv else "pluginTheme"
                            reference = skin if attribute == "skin" else "org.zroutery.theme." + skin
                            dt.evaluate("document.documentElement.dataset[%s]=%s" % (json.dumps(attribute), json.dumps(reference)))
                        for state in ["rest", "hover", "pressed", "focus", "checked", "checked_hover"]:
                            for selector, pseudo in SAMPLES:
                                dt.evaluate("document.activeElement?.blur(); for(const el of document.querySelectorAll('input[type=checkbox]'))el.checked=%s" % ("true" if state.startswith("checked") else "false"))
                                point = dt.element(selector)
                                dt.call("Input.dispatchMouseEvent", {"type":"mouseMoved","x":point["x"] if state in ["hover","pressed","checked_hover"] else 0,"y":point["y"] if state in ["hover","pressed","checked_hover"] else 0})
                                if state == "pressed": dt.call("Input.dispatchMouseEvent", {"type":"mousePressed","x":point["x"],"y":point["y"],"button":"left","clickCount":1})
                                if state == "focus": dt.evaluate("document.querySelector(%s).focus()" % json.dumps(selector))
                                values = dt.evaluate("(() => {const s=getComputedStyle(document.querySelector(%s),%s);return Object.fromEntries(%s.map(p=>[p,s.getPropertyValue(p)]));})()" % (json.dumps(selector), json.dumps(pseudo), json.dumps(PROPERTIES)))
                                if state == "pressed": dt.call("Input.dispatchMouseEvent", {"type":"mouseReleased","x":point["x"],"y":point["y"],"button":"left","clickCount":1})
                                key = (skin, mode, state, selector, pseudo)
                                if kind == "baseline": measurements[key] = values
                                else:
                                    for prop, expected in measurements[key].items():
                                        # The fixture's uniform sample layout and
                                        # actual color/shape are compared; inherited
                                        # page-relative offsets are not theme facts.
                                        if prop in ["top", "left", "width", "height"] and pseudo is None and selector not in [".toggle input", ".mainbar", ".btn-primary", ".select-trigger"]: continue
                                        if prop.endswith("color") and prop.startswith("border-") and values.get(prop.replace("color","width")) == "0px": continue
                                        if values[prop] != expected: failures.append({"skin":skin,"mode":mode,"state":state,"selector":selector,"pseudo":pseudo,"property":prop,"expected":expected,"actual":values[prop]})
            dt.close()
        finally:
            try: process.kill()
            except Exception: pass
            try: process.wait(timeout=10)
            except Exception: pass
            try: dt.close()
            except Exception: pass
            server.shutdown()
    finally:
        import shutil, time
        for _ in range(5):
            try: shutil.rmtree(profile, ignore_errors=True); break
            except Exception: time.sleep(0.2)
    report = {"cases":len(measurements),"failures":failures}
    (TARGET / "result.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    for failure in failures[:30]: print(json.dumps(failure, ensure_ascii=True))
    print(f"plugin-theme-parity: {len(measurements)} component/state samples, {len(failures)} differences")
    return 1 if failures else 0

if __name__ == "__main__": sys.exit(main())
