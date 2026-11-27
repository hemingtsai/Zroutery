# UI Design System Reference

> Authority: reference · Owner: frontend · Last verified: 2026-11-27 · Verified by: `pnpm --dir ui build && pnpm --dir ui test`

This page covers the tokens, skins, primitives and gates of the dashboard. It is
a reference, not a pixel specification; the shipped values are in the stylesheet.

## Tokens and skins

- The stylesheet entry is `ui/src/styles.css`; it imports the generated
  `ui/src/industrial.css`, which carries the base tokens. ThemeV1 packs are
  compiled per theme and override tokens — for example `--control-h` comes from
  the pack's `metrics.control_height` (24-40px)
  (`ui/src/themes/compiler.ts::compileTheme`, `ui/vite.config.ts`).
- A pack can express MD3 dynamic color as a `tonal` color expression — the
  system accent's hue re-toned per role, emitted as a relative `oklch(from …)`
  color the webview resolves at paint time — and a navigation-rail item
  morphology where the active indicator wraps the icon slot
  (`ui/src/themes/compiler.ts::compileTheme`).
- A theme preference is applied separately from a skin: `applyTheme` sets
  `data-theme` (`ui/src/theme.ts::applyTheme`); `applyThemeRef` sets
  `data-plugin-theme` and removes the legacy `data-skin`
  (`ui/src/themes/registry.ts::applyThemeRef`).
- The system accent is read from the desktop and applied as a CSS variable; a
  null accent leaves the skin's own accent
  (`ui/src/accent.ts::applySystemAccent`).
- Theme, skin and accent are presentation preferences. They are not part of the
  configuration document and do not change routing.

## Shell and primitives

- The six task doors are declared once in `NAV`
  (`ui/src/shell/nav.ts::NAV`). The helper pages (`accounts`, `routing`,
  `cutover`) are tabs inside a task door, not extra doors. Each door carries a
  static inline icon from a fixed per-page map, not a component tree
  (`ui/src/shell/navIcons.tsx::NAV_ICONS`).
- An addressable helper page opens through its owning door first, then through
  its tab: the interaction harness maps the stable route names to their doors
  instead of treating them as doors themselves
  (`scripts/ui_interaction_test.py::open_page`).
- `Tabs` is a real tablist: arrow keys, Home/End, roving tabindex, and only the
  selected panel is mounted (`ui/src/components/tabs.tsx::Tabs`).
- `Drawer` is the overlay for editing; it restores focus to its opener and keeps
  the focus inside itself while open
  (`ui/src/components/drawer.tsx::Drawer`).
- Save feedback is carried by the save queue, not by a page-local spinner
  (`ui/src/saveQueue.ts::createSaveQueue`).
- Plugin-contributed UI is data-only: the host renders declared components, and
  privileged prompts are isolated under `[data-host-security]`
  (`ui/src/styles.css`, `ui/src/components/confirm.tsx`,
  `ui/src/plugins/renderer.tsx`).

## Language

The Auto review drawer uses the shared Toggle/NumberField controls for embedded
review and its total timeout, with localized labels, explicit data-sharing/cost
text and stable accessible names. Layout coverage requires both number inputs;
interaction coverage saves both settings without changing the model pool
(`ui/src/pages/routing/AutoModeDrawer.tsx::AutoModeDrawer`,
`scripts/ui_layout_test.py::SURFACES`,
`scripts/ui_interaction_test.py::case_ui2_candidate_priority`).

- English, Simplified Chinese and Traditional Chinese dictionaries are separate
  modules (`ui/src/i18n/en.ts::en`, `ui/src/i18n/zh.ts::zh`,
  `ui/src/i18n.zhTW.ts::zhTW`). A unit gate reconciles the placeholder
  variables every key interpolates across the three dictionaries; a missing or
  extra one renders literal braces to the user
  (`ui/src/i18n.placeholder.test.ts::variables`).
- The provider falls back to English for a missing key
  (`ui/src/i18n.tsx::I18nProvider`). A key added to one dictionary must be added
  to the other two; a missing key renders English, which is a silent regression
  for a Chinese reader.
- Plugin-contributed UI follows the system language, not the webview's: the
  worker locale maps the OS UI language to en, zh-CN or zh-TW and stays English
  for anything else until the composition layer passes an explicit per-view
  locale (`src-tauri/src/plugins/commands.rs::default_locale`).
- A plugin options proposal is validated by the host schema before storage; a
  refused save returns bounded field error codes to the plugin's own UI, which
  renders them — the host never displays raw option values
  (`src-tauri/src/plugins/commands.rs::invoke_plugin_ui_action`).
- A host without the restricted process answers a plugin worker call with
  `sandbox_unavailable`; the call is never run unsandboxed
  (`src-tauri/src/plugins/commands.rs::run_sandboxed_process`).
- Every worker launch runs under a fresh AppContainer profile deleted at exit;
  the reload command re-runs worker discovery, the sandbox probe and one
  ledger sweep without a restart
  (`src-tauri/src/plugins/commands.rs::reload_plugins`).

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
- The headed-browser harnesses share one hand-rolled DevTools client rather
  than carrying private copies that drift
  (`scripts/cdp_client.py::DevTools`).
- The overflow gate runs at the minimum window size
  (`scripts/ui_overflow_test.py::main`).
- The theme baseline gate compares computed styles against the frozen v3.2
  baseline (`scripts/plugin_theme_test.py::main`); it is local-only and is not a
  pixel diff.

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

## Failure modes

| Symptom | Cause | Handling |
|---|---|---|
| a language key is missing | dictionary drift | add the key to all three dictionaries (`ui/src/i18n.tsx`) |
| a lazy page stays loading | runtime chunk not fetched | reload; the overflow harness catches it (`scripts/ui_overflow_test.py`) |
| a token is overridden locally | inline style | use the token, not a literal (`ui/src/styles.css`) |

## Not implemented

- No pixel or screenshot regression gate; the theme baseline gate compares
  computed styles, and the plugin UI renderer fails closed instead of rendering
  untrusted components (`scripts/plugin_theme_test.py::main`,
  `src-tauri/src/plugins/commands.rs::render_plugin_ui`).
- Layout and interaction are not in CI; a green CI run does not cover them.
- No plugin-extensible icon set; the shell's navigation icons are a fixed
  per-page map (`ui/src/shell/navIcons.tsx::NAV_ICONS`).

## Sources

- `ui/src/styles.css::--control-h`
- `ui/src/industrial.css`
- `ui/src/themes/compiler.ts::compileTheme`
- `ui/src/themes/registry.ts::applyThemeRef`
- `ui/src/theme.ts::applyTheme`
- `ui/src/accent.ts::applySystemAccent`
- `ui/src/plugins/renderer.tsx`
- `scripts/plugin_theme_test.py::main`
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
