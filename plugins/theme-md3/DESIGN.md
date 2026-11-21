# DESIGN.md — Material 3 theme plugin

> Design companion for [`theme.json`](theme.json) in this directory. Format
> follows the SKILL.md/DESIGN.md pair convention of
> [bergside/awesome-design-skills](https://github.com/bergside/awesome-design-skills)
> and the DESIGN.md distill format of
> [VoltAgent/awesome-design-md](https://github.com/VoltAgent/awesome-design-md).
> Every value below is measured from `theme.json`, not guessed; the spec it
> follows is Google's Material Design 3 as distilled in
> [hamen/material-3-skill](https://github.com/hamen/material-3-skill)
> (`references/color-system.md`, `references/typography-and-shape.md`,
> `references/component-catalog.md`).

## Brand & mission

Bring MD3's tonal color system and component semantics to a dense desktop
routing tool, in both light and dark modes, while staying adaptive: the accent
roles follow the OS accent color when one is exposed, with the MD3 baseline
purple (`#6750A4`) as fallback. The theme is a desktop adaptation — MD3's
52×32dp switches and 48dp touch targets are scaled to a pointer-first 36px
control height — but color pairs, shape tokens and elevation tiers follow the
baseline scheme exactly where the baseline publishes values.

## Style foundations

### Color tokens

Colors are declared per mode as host color roles and compiled to CSS custom
properties by `ui/src/themes/compiler.ts::compileTheme`. Roles map to MD3
baseline roles as follows.

| Host role | Light | Dark | MD3 baseline role |
|---|---|---|---|
| `bg` | `#fef7ff` | `#141218` | `surface` |
| `surface` | `#f7f2fa` | `#1d1b20` | `surface-container-low` |
| `surface-muted` | `#f3edf7` | `#211f26` | `surface-container` |
| `surface-high` | `#ece6f0` | `#2b2930` | `surface-container-high` |
| `container-highest` | `#e6e0e9` | `#36343b` | `surface-container-highest` |
| `line` | `#79747e` | `#938f99` | `outline` |
| `line-muted` | `#cac4d0` | `#49454f` | `outline-variant` |
| `text` | `#1d1b20` | `#e6e0e9` | `on-surface` |
| `text-2`, `muted` | `#49454f` | `#cac4d0` | `on-surface-variant` |
| `primary` | accent (fallback `#6750a4`) | accent mix | `primary` |
| `on-primary` | accent-fg (fallback `#ffffff`) | accent mix | `on-primary` |
| `primary-hover` | 8% on-primary over primary | same | state layer on `primary` |
| `primary-container` | accent-hue tone (fallback `#eaddff`) | accent-hue tone (fallback `#4f378b`) | `primary-container` |
| `on-primary-container` | accent-hue tone (fallback `#21005d`) | accent-hue tone (fallback `#eaddff`) | `on-primary-container` |
| `secondary-container` | accent-hue tone (fallback `#e8def8`) | accent-hue tone (fallback `#4a4458`) | `secondary-container` |
| `on-secondary-container` | accent-hue tone (fallback `#1d192b`) | accent-hue tone (fallback `#e8def8`) | `on-secondary-container` |
| `error-container` | `#f9dedc` | `#8c1d18` | `error-container` |
| `on-error-container` | `#410e0b` | `#f9dedc` | `on-error-container` |
| `danger` | `#b3261e` | `#f2b8b5` | `error` |
| `inverse-surface` | `#322f35` | `#e6e0e9` | `inverse-surface` |
| `inverse-on-surface` | `#f5eff7` | `#322f35` | `inverse-on-surface` |
| `inverse-primary` | `#d0bcff` | `#6750a4` | `inverse-primary` |
| `ok` / `ok-bg` | `#00722a` / `#daecdb` | `#93d69c` / `#1b3820` | extension (MD3 has no success role) |
| `warn` / `warn-bg` | `#835100` / `#f5e3cd` | `#eab26b` / `#432b07` | extension (MD3 has no warning role) |
| `hover` | 8% `text` over transparent | same | state layer, hover 8% |
| `selected` | 12% `text` over transparent | same | state layer, selected 12% |

All light-mode values are byte-identical to the MD3 baseline scheme for the
roles the baseline publishes.

### Dynamic color

Material You's promise is a scheme generated from one source color, and the
source here is the OS accent. The four container roles are declared as
`tonal` expressions: they keep the accent's hue at runtime and take the
baseline role's own OKLCH lightness and chroma ceiling, so the fallback seed
(`#6750a4`-family) reproduces the published baseline scheme byte-for-byte
while any other accent recolors the whole tonal layer — nav pills, tonal
buttons, selections, KPI cells — in the accent's hue. Emission is a CSS
relative color (`oklch(from var(--system-accent,seed) L min(c,cap) h)`) over
a baseline literal, so engines without relative-color syntax keep the exact
baseline scheme.

Deliberately static: `error`/`on-error-container` (MD3 keeps the error
palette out of dynamic color), the ok/warn extension pairs, and `primary`
itself, which rides the raw OS accent rather than tone 40 so the user's
picked color stays visible. The neutral surface ladder and the outline pair
are dynamic too: M3's neutral palettes take the seed's hue, so every surface
carries a whisper of the accent at the baseline's own chroma (0.010–0.018)
instead of the baseline purple's — the fallback seed reproduces the published
scheme exactly, any other accent de-purples the canvas.

### Typography

- Family: `host:roboto` → Roboto (MD3 plain typeface), loaded by
  `ui/src/themes/registry.ts::applyThemeRef`.
- Scale: `material_desktop` (host-defined in `ui/src/styles.css`).
- Weights: 400 for titles, 500 for labels — matching MD3 `Title Large` (22sp
  w400) and `Label Large` (14sp w500) usage.

### Shape scale

| Token | Radius | Parts |
|---|---|---|
| full | 999px | buttons, segment, segment items, navigation items, switch track/thumb, menu radio |
| extra-large | 28px | dialog, banner |
| large | 16px | drawer |
| medium | 12px | section (card), kpi cell |
| small (adapted to 4px) | 4px | input, select trigger/menu, popover, toast, chart card |
| full (badge/chip at 8px) | 8px | badge, chip |
| component token 0px | 0px | banner (`md-comp-banner` publishes container-shape 0px, overriding the sys extra-large) |
| extra-small | 2px | checkbox |

### Elevation

MD3 uses tonal surfaces first; shadows only for floating layers. Shadow CSS
values match the baseline's published levels exactly:

| Level | Value | Parts |
|---|---|---|
| 1 | `0 1px 2px 30% black, 0 1px 3px 1px 15% black` | drawer, section (the elevated-card mapping, light) |
| 2 | `0 1px 2px 30% black, 0 2px 6px 2px 15% black` | popover, select menu, chart card (light) |
| 3 | `0 1px 3px 30% black, 0 4px 8px 3px 15% black` | dialog (light) |

Dark mode drops elevation shadows to 0 and tints floating surfaces toward
`primary` (8–11% mix), following MD3's tonal-elevation guidance.

## Component families

| Part | Recipe (rest) | MD3 component |
|---|---|---|
| button default | secondary-container fill, full pill, w500 | filled tonal |
| button primary | primary fill, on-primary ink | filled |
| button ghost | transparent, primary ink, 8% hover | text button |
| button danger | error-container fill, on-error-container ink | error tonal |
| button pressed | 12% state layer over the container ink (all four variants) | `pressed-state-layer-opacity` 0.12 |
| button disabled | `text` 12% fill / 38% ink, all variants | `disabled-container-opacity` 0.12, `disabled-label-text-opacity` 0.38 |
| checkbox | unchecked: 2px `line` ring; checked: `primary` fill | outlined checkbox |
| segment | full pill, 1px inset outline ring, selected = secondary-container | segmented button |
| segment.item hover/pressed | 8% / 12% `text` state layers | state layers |
| navigation.item | full pill; selected = secondary-container; 14px w500 label | nav rail active indicator, `label-large` |
| navigation.item hover/pressed | 8% / 12% `text` state layers | state layers |
| select.item, menu.item hover | 8% `hover` fill (menu doubles onto Radix `data-highlighted`) | list/menu item state layer |
| switch | off: container-highest track + 2px outline stroke; on: primary track | switch |
| toast | inverse-surface fill, 4px radius | snackbar |
| section | `surface` fill (container-low tier) + level-1 shadow, 12px radius | **elevated card** — full-page panels read as slabs if the filled card's `container-highest` is scaled up to them; the elevated variant lifts instead of darkens |
| kpi.cell | `container-highest` fill, 12px radius | filled card (compact widget, the scale the filled card was designed for) |
| table rows | 48px row height | data table (52dp, clamped) |
| list.row, timeline.item | 1px bottom `line-muted` | divider uses `outline-variant` (never `outline`) |
| input, select.trigger | transparent fill, 2px primary inset focus ring | outlined text field |

## Accessibility

- Color pairing follows the MD3 pairing table: `primary`→`on-primary`,
  `*-container`→`on-*-container`, `inverse-surface`→`inverse-on-surface`.
  Nothing is paired across rows.
- Focus: 2px `primary` ring at offset −2 on inputs, select triggers and menu
  items — MD3's 2dp active indicator.
- Dark `primary`/`on-primary` are derived from the OS accent by `color-mix`;
  at the fallback seed the pair measures ≈7.5:1 (baseline pair ≈7.7:1).
- Dark `on-error-container` (`#f9dedc`) on `error-container` (`#8c1d18`)
  measures ≈7.2:1 — the baseline pair.

## Do / Don't

Do:

- Use `line-muted` (`outline-variant`) for every divider; keep `line`
  (`outline`) for 3:1 boundaries such as input strokes.
- Reach for `secondary-container` as the "tonal selection" fill — it is the
  theme's one selection ink across buttons, segments, nav and selects.
- Keep all four button variants in the Filled → Tonal → Text → Error emphasis
  order when adding variants.
- Scale touch metrics through `metrics.control_height` (36px), never per-rule.

Don't:

- Don't introduce shadows on resting cards or sections; elevation on the page
  plane is tonal (`surface-container-*`), not shadowed.
- Don't reuse `error-container` for warnings; `warn`/`warn-bg` is the
  extension pair for that.
- Don't hardcode the baseline purple; accent roles must stay accent-driven so
  the OS accent keeps working.

## Deliberate deviations from the MD3 baseline

Recorded decisions, not drift:

1. **Dark accent derivation.** Dark `primary` mixes the OS accent 45% toward
   white and `on-primary` 15% toward the dark surface, approximating "tone 80 /
   tone 20" for any seed. The static baseline pair (`#D0BCFF` / `#381E72`)
   applies only when no OS accent exists and only as the fallback mix input.
2. **Badge radius 8px.** MD3 badges are full-radius count indicators; this
   app's `.badge` is a status word label, so it takes the chip's 8px instead.
3. **Desktop control metrics.** Switch 36×20 (thumb 14/16), controls 36px —
   pointer-first desktop scale rather than 52×32dp touch targets.
4. **4px text-field radius.** Inputs use 4px all corners (outlined-field
   density) rather than the 8px `small` corner mapping.
5. **No tertiary roles.** The host role list has no tertiary slot; container
   roles carry the accent structure instead.
6. **48px table rows.** `md-comp-data-table` publishes a 52dp row height; the
   compiler's non-shell metric bound is 48, so rows land at 48px.
7. **Disabled is ink, not opacity.** The pack disables at the published
   12%/38% on-surface mixes; the compiler drops the host's 0.4 opacity fade
   for any pack that styles the disabled state, and the pair intentionally
   reads below the 4.5:1 body floor (disabled is WCAG-exempt).

## Sources

- `theme.json` — all token and recipe values on this page.
- `zroutery.plugin.json` — plugin identity and the `theme.contribute` grant.
- `ui/src/themes/compiler.ts::compileTheme` — validation and CSS emission.
- `ui/src/themes/parts.ts::THEME_PARTS` — host selectors the recipes bind to.
- `ui/src/themes/registry.ts::applyThemeRef` — font loading and activation.
- https://github.com/hamen/material-3-skill — MD3 reference (tokens, shapes,
  elevation, component catalog).
