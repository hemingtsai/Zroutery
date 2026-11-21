# DESIGN.md — Fluent theme plugin

> Design companion for [`theme.json`](theme.json) in this directory. Format
> follows the SKILL.md/DESIGN.md pair convention of
> [bergside/awesome-design-skills](https://github.com/bergside/awesome-design-skills)
> and the DESIGN.md distill format of
> [VoltAgent/awesome-design-md](https://github.com/VoltAgent/awesome-design-md).
> Every value below is measured from `theme.json`; the spec it follows is
> Microsoft's Fluent 2 as compiled in
> [SwePalm/fluent2-design-system](https://github.com/SwePalm/fluent2-design-system)
> (`tokens/colors.css`, sourced verbatim from `@fluentui/tokens`) and the
> Fluent 2 token reference in `fefogarcia/approved-skills`
> (`references/tokens.md`).

## Brand & mission

Give the desktop app the WinUI 11 treatment: a Mica-style layered window with
an acrylic sidebar, hairline elevation borders on hover, tiered Fluent shadows
on floating layers, and Segoe UI throughout. Fluent 2 is a two-layer token
system — global ramps and theme-aware aliases — and this theme mirrors that
split: raw ramp greys and brand blue are mixed only inside role definitions;
recipes consume roles, never raw hex (the one exception is the elevation-border
gradient, which the compiler models as `stroke_gradient`).

## Style foundations

### Layering (backgrounds)

| Layer | Light | Dark | Fluent source |
|---|---|---|---|
| `bg` (window) | `#f5f5f5` | `#202020` | WinUI `SolidBackgroundFillColorBase` (light `#F3F3F3` family / dark `#202020`) |
| `surface` (layer) | `#ffffff` | `#292929` | `colorNeutralBackground1` (webLight/webDark) |
| `surface-muted` | `#fafafa` | `#141414` | `colorNeutralBackground2` (light) / `colorNeutralBackground3` (dark) |
| `hover` | `#f5f5f5` | 6.05% white | `colorSubtleBackgroundHover` (grey-96 / rgba(255,255,255,0.06)) |
| `selected` | `#ebebeb` | `#383838` | subtle-fill selected family (≈8% black on white) |

### Foregrounds, strokes, status

| Host role | Light | Dark | Fluent source |
|---|---|---|---|
| `text` | `#242424` | `#ffffff` | `colorNeutralForeground1` (grey-14 / white) |
| `text-2` | `#424242` | `#d6d6d6` | `colorNeutralForeground2` (grey-26 / grey-84) |
| `muted` | `#616161` | `#adadad` | `colorNeutralForeground3` (grey-38 / grey-68) |
| `control-line` | `#8a8a8a` | `#808080` | WinUI `ControlStrokeColorSecondary` (≈46% black / 54% white), not the v9 web `colorNeutralStrokeAccessible` |
| `stroke-hover` / `stroke-pressed` | `#6e6e6e` / `#565656` | `#999999` / `#adadad` | WinUI control-stroke hover/pressed family |
| `divider` | 6% black | 8% white | `colorNeutralStroke2` family |
| `primary` | accent (fallback `#0f6cbd` = brand-80) | accent mix 85% over black ≈ `#0d5ca1` | `colorBrandBackground` family |
| `primary-hover` (light) | 85% accent over `text` ≈ `#1261a6` | accent (base) | rest = shade step, hover = base (dark `colorBrandBackgroundHover` = brand-80) |
| `ok` / `ok-bg` | `#0e700e` / `#f1faf1` | `#54b054` / `#052505` | green shade-10 / tint-60 (`colorPaletteGreen*`) |
| `warn` / `warn-bg` | `#bc4b09` / `#fff9f5` | `#faa06b` / `#4a1e04` | dark-orange shade-10 / tint-60 |
| `danger` / `danger-bg` | `#b10e1c` / `#fdf3f4` | `#dc626d` / `#3b0509` | cranberry shade-10 / tint-60 |

Light status values are byte-identical to the shared palette published in
`tokens/colors.css`.

### Shadow colors

| Mode | Ambient | Key |
|---|---|---|
| Light | rgba(0,0,0,0.12) | rgba(0,0,0,0.14) |
| Dark | rgba(0,0,0,0.24) | rgba(0,0,0,0.28) |

Byte-identical to `--colorNeutralShadowAmbient` / `--colorNeutralShadowKey` in
both modes.

### Typography

- Family: `system:segoe_ui` → `"Segoe UI Variable", "Segoe UI"` (Fluent 2
  primary typeface on Windows).
- Scale: `fluent_desktop` (host-defined).
- Page title (`shell.title`): 20px / 600 — Fluent's `subtitle` preset.

### Shape scale

| Radius | Parts |
|---|---|
| 2px (`borderRadiusSmall`) | checkbox |
| 4px (`borderRadiusMedium`) | buttons, inputs, select trigger/menu/item, menu items, popover, toast, banner, badge, chip, kpi cell, navigation items |
| 8px (`borderRadiusXLarge`) | dialog, drawer, section (card) |
| 999px (`borderRadiusCircular`) | switch track/thumb, segment indicator, menu radio dot |

### Elevation

| Tier | Geometry | Parts |
|---|---|---|
| shadow4 | `0 0 2px ambient, 0 4px 8px key` | toast |
| shadow8 | `0 0 2px ambient, 0 8px 16px key` | popover, select menu, drawer, kpi hover, chart card |
| shadow28-family | `0 0 8px ambient, 0 32px 64px key` | dialog |

Buttons additionally carry the WinUI `ControlElevationBorderBrush` on hover:
a 1px transparent border painted by a 180° gradient (16% → 6% black in light,
12% → 18% white in dark) around the fill, compiled through
`ui/src/themes/compiler.ts` `stroke_gradient`.

### Materials

Sidebar: `material.kind = "acrylic"` — 78% `bg` fill, `blur(24px)`,
`saturate(1.3)`, with a 1px `divider` end stroke. This is the compiler's one
acrylic slot (`shell.sidebar` only).

## Component families

| Part | Recipe |
|---|---|
| navigation.item | 4px radius; hover/selected = `subtle-secondary`; 3×16px `primary` leading indicator pill |
| segment | underline selection: 3px `primary` indicator, items carry `indicator_anchor` |
| switch | WinUI toggle: 40px track, 1px `control-line` stroke, 12px `muted` thumb, checked = `primary` track |
| focus.target | WinUI focus rect: 2px `text` outer ring at offset 0 + 1px inner ring (`surface` light, black dark) inset inside the fill |
| input | 1px stroke; hover deepens to `stroke-hover`; focus = 1px `primary` stroke + 2px inset top shadow |
| list.row | no fill; 12px inline padding; selection handled by nav/KPI recipes |
| scrollbar | 6px overlay; thumb = 24% text (hover 40%, pressed 55%), transparent track |

## Accessibility

- Foreground ramp matches the published alias table exactly, so every
  text-on-surface pair carries Fluent's tested contrast (grey-14 on white,
  grey-84 on grey-16, etc.).
- Focus is Fluent's two-ring focus rect: a 2px `text` outer ring (black light /
  white dark) with a 1px inner ring just inside the fill (white light / black
  dark) — `colorStrokeFocus2`/`colorStrokeFocus1` semantics.
- Status pairs keep Fluent's background1/foreground1 discipline (tint bg +
  shade fg), which holds ≥4.5:1 in both modes.
- Dark `primary` (85% accent + 15% black ≈ `#0d5ca1` at the fallback seed,
  within ΔRGB ≤ 4 of brand-70 `#115ea3`) under white ink measures ≈6.9:1; the
  hover fill (accent base) ≈6.7:1.

## Do / Don't

Do:

- Build floating layers on the shadow tiers (`shadow4`/`shadow8`), never on
  ad-hoc blur values.
- Keep the window/layer split: `bg` is the Mica-style base, `surface` is the
  layer above it; controls sit on `surface`.
- Use `stroke_gradient` for any WinUI-style elevation border; plain `stroke`
  and `stroke_gradient` cannot be combined.
- Let the accent roles ride the OS accent; brand-80 is only the fallback.

Don't:

- Don't put shadows on resting sections in dark mode (they flatten to 0);
  dark elevation is layer color, not shadow.
- Don't use `control-line` for dividers — it is the interactive-control
  stroke; `divider` is the static one.
- Don't round dialogs or drawers below 8px (`borderRadiusXLarge` is the
  published dialog radius in this adaptation).

## Deliberate deviations from published Fluent tokens

Recorded decisions, not drift:

1. **Window base colors.** `bg` follows WinUI's `SolidBackgroundFillColorBase`
   (`#202020` dark, `#f5f5f5` light-family) instead of v9
   `colorNeutralBackground1`; the layer above uses v9's background1. Fluent 2
   spans both worlds; this theme keeps the desktop one.
2. **Control strokes.** `control-line`/`stroke-hover`/`stroke-pressed` use
   WinUI `ControlStrokeColor*` values rather than v9
   `colorNeutralStrokeAccessible` (`#616161` light / `#adadad` dark).
3. **Dark brand derivation.** Dark `primary` mixes the accent 85% over black —
   one shade step down the brand ramp, the direction `webDarkTheme` itself
   takes (`colorBrandBackground` = brand-70; hover = the base accent). At the
   fallback seed the mix lands within ΔRGB ≤ 4 of brand-70 `#115ea3`.
4. **Dialog shadow geometry.** `0 0 8px` + `0 32px 64px` — the shadow28
   geometry — while the reference maps dialogs to `shadow16`.
5. **Scrollbar invariants.** Both modes reuse the light thumb greys
   (`#c6c6c6` / `#8f8f8f`) rather than a dark scrollbar palette.

## Sources

- `theme.json` — all token and recipe values on this page.
- `zroutery.plugin.json` — plugin identity and the `theme.contribute` grant.
- `ui/src/themes/compiler.ts::compileTheme` — validation, `stroke_gradient`
  and acrylic emission.
- `ui/src/themes/registry.ts::applyThemeRef` — font loading and activation.
- https://github.com/SwePalm/fluent2-design-system — Fluent 2 tokens compiled
  from `@fluentui/tokens`.
- https://github.com/fefogarcia/approved-skills — Fluent 2 token reference
  (shadow tier mapping, duration/curve catalog).
