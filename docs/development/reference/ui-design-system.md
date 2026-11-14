# UI Design System Reference

> Authority: reference · Owner: frontend · Last verified: 2026-11-14 · Verified by: `pnpm --dir ui build && pnpm --dir ui test`

This page covers the tokens, skins, primitives and gates of the dashboard. It is
a reference, not a pixel specification; the shipped values are in the stylesheet.

## Tokens and skins

- The stylesheet is the source of truth for tokens
  (`ui/src/styles.css`); `--control-h` is the control-height token and differs by
  skin.
- A theme preference is applied separately from a skin: `applyTheme` sets the
  light/dark preference (`ui/src/theme.ts::applyTheme`); `applySkin` sets the
  skin attribute (`ui/src/skin.ts::applySkin`).
- The system accent is read from the desktop and applied as a CSS variable; a
  null accent leaves the skin's own accent
  (`ui/src/accent.ts::applySystemAccent`).
- Theme, skin and accent are presentation preferences. They are not part of the
  configuration document and do not change routing.

## Shell and primitives

- The six task doors are declared once in `NAV`
  (`ui/src/shell/nav.ts::NAV`). The helper pages (`accounts`, `routing`,
  `cutover`) are tabs inside a task door, not extra doors.
- `Tabs` is a real tablist: arrow keys, Home/End, roving tabindex, and only the
  selected panel is mounted (`ui/src/components/tabs.tsx::Tabs`).
- `Drawer` is the overlay for editing; it restores focus to its opener and keeps
  the focus inside itself while open
  (`ui/src/components/drawer.tsx::Drawer`).
- Save feedback is carried by the save queue, not by a page-local spinner
  (`ui/src/saveQueue.ts::createSaveQueue`).

## Language

- English, Simplified Chinese and Traditional Chinese dictionaries are separate
  modules (`ui/src/i18n/en.ts::en`, `ui/src/i18n/zh.ts::zh`,
  `ui/src/i18n.zhTW.ts::zhTW`).
- The provider falls back to English for a missing key
  (`ui/src/i18n.tsx::I18nProvider`). A key added to one dictionary must be added
  to the other two; a missing key renders English, which is a silent regression
  for a Chinese reader.

## Gates

- The production bundle is measured after `pnpm --dir ui build` with ceilings for
  total JavaScript, total CSS and the largest chunk
  (`scripts/ui_budget_test.py::Budget`). The ceilings are in that script; this
  page does not copy them.
- The layout harness measures control height and row alignment per control kind
  (`scripts/ui_layout_test.py::main`). It is a local gate, not a CI gate; see
  [verification.md](../verification.md).
- The interaction harness drives the mock bridge with real clicks and keys
  (`scripts/ui_interaction_test.py::main`); it is also local-only.
- The overflow gate runs at the minimum window size
  (`scripts/ui_overflow_test.py::main`).

## Invariants

1. Three locale dictionaries stay key-aligned; a missing key is a bug even though
   the fallback hides it.
2. A structural UI change updates the layout/interaction fixtures in the same
   change; a fixture that still passes after the structure changed is measuring
   the old page.
3. A new control uses the shared primitives and the `--control-h` token rather
   than a local height.
4. A change to the shipped bundle is checked against the budget, not estimated.
5. No design-system change reaches the product without a `pnpm --dir ui build`
   and the relevant local harness.

## Not implemented

- No screenshot/visual-regression gate; layout and interaction are behavioural
  checks, not pixel comparisons.
- Layout and interaction are not in CI; a green CI run does not cover them.
- No icon set beyond the inline SVG/mark used by the shell.

## Sources

- `ui/src/styles.css::--control-h`
- `ui/src/theme.ts::applyTheme`
- `ui/src/skin.ts::applySkin`
- `ui/src/accent.ts::applySystemAccent`
- `ui/src/shell/nav.ts::NAV`
- `ui/src/components/tabs.tsx::Tabs`
- `ui/src/components/drawer.tsx::Drawer`
- `ui/src/saveQueue.ts::createSaveQueue`
- `ui/src/i18n.tsx::I18nProvider`
- `ui/src/i18n/en.ts::en`
- `ui/src/i18n/zh.ts::zh`
- `ui/src/i18n.zhTW.ts::zhTW`
- `scripts/ui_budget_test.py::Budget`
- `scripts/ui_layout_test.py::main`
- `scripts/ui_interaction_test.py::main`
- `scripts/ui_overflow_test.py::main`
