# DESIGN.md — Industrial Desktop (host theme)

> Design companion for [`src/industrial.css`](src/industrial.css), the host's
> built-in appearance. Format follows the SKILL.md/DESIGN.md pair convention of
> [bergside/awesome-design-skills](https://github.com/bergside/awesome-design-skills)
> and the DESIGN.md distill format of
> [VoltAgent/awesome-design-md](https://github.com/VoltAgent/awesome-design-md).
> Every value below is measured from `src/industrial.css` and
> `src/styles.css`; the audit lens is the monodesign skill
> ([monoes/monomind `monodesign`](https://github.com/monoes/monomind)) —
> contrast floors, motion discipline, and the absolute bans.

## Brand & mission

A quiet desktop utility, not an AI product: strict monochrome (black, white,
grey steps), a black primary action, native fonts, hairline dividers instead of
cards, no shadows on the page plane, no pills, no icons in navigation. The
references are Finder, Proxyman, Fork, JetBrains Toolbox, 1Password — software
that looks like software. The test for every element: does it serve the
information structure? If it only makes the interface look designed, it goes.

## Style foundations

### Color tokens

| Role | Light | Dark | Use |
|---|---|---|---|
| `bg` / `surface` | `#ffffff` | `#111111` | page and panel plane |
| `surface-muted` / `hover` | `#f5f5f5` | `#1a1a1a` | the one step down: inputs' readonly fill, code blocks, hovers |
| `selected` | `#ebebeb` | `#242424` | selection: flat tint, never a height or weight change |
| `line` | `#000000` | `#333333` | structural rules: sidebar edge, table head rule |
| `line-muted` | `#d4d4d4` | `#2a2a2a` | the quiet line: row dividers, timestamps' rules |
| `text` | `#000000` | `#e0e0e0` | primary ink |
| `text-2` | `#555555` | `#888888` | secondary ink |
| `muted` | `#767676` | `#8a8a8a` | hints, timestamps, placeholders — 4.54:1 / 5.47:1, body-text floor |
| `primary` / `primary-hover` / `on-primary` | `#000000` / `#333333` / `#ffffff` | `#ffffff` / `#cccccc` / `#000000` | the one loud element |
| `ok` / `warn` / `danger` | `#1f6b3a` / `#8a5a00` / `#b42318` | `#57b377` / `#d19a3d` / `#e0655a` | status ink + matching `*-bg` tints |
| `scrollbar-thumb(-hover)` | `#c6c6c6` / `#8f8f8f` | `#454545` / `#6a6a6a` | the one token that cannot be a line color |

The palette has no accent hue. Selection, focus and the pressed underline are
text ink; color appears only as status dots and tinted status badges.

### Typography

- UI face: `"Noto Serif SC Variable"` (serif, literary voice) — the token is
  `--font-serif`, with `--font-sans` kept as a compatibility alias.
- Mono: IBM Plex Mono stack with `Microsoft YaHei` before the generic monospace
  so CJK inside code runs borrows the UI face instead of falling to SimSun.
- Scale (documented in the file header): 20 display / 18 h1 / 15 h2 / 14 body /
  12 caption / 11 micro. Weights 400–600; never above 600.
- Statistics are prose: figures that describe behavior (`--kpi-value`,
  `.stats strong`) use the UI face; identifiers, paths and hashes use mono.

### Spacing system

8pt scale throughout: gaps of 8 / 16 / 24 / 32; page padding 28px 32px 40px;
content max-width 1080px; control height 30px; field labels 14px high. The
drawer body's 24px gap was squared to the scale deliberately (an in-file
comment records the 26px it replaced).

### Shape

`--radius: 0`. Every corner is square — thumbs, badges, banners, dialogs. The
scrollbar thumb documents the decision explicitly: its corners match every
other corner in the app.

### Elevation

No shadows on the page plane. One floating-layer shadow (`--shadow-layer`:
`0 8px 24px rgba(0,0,0,0.08)` light / 0.4 dark) is reserved for the four
floating surfaces: select menu, popover, chart hover card, toasts. Detail
surfaces (drawer) get a border and a surface step instead.

### Motion

- `--ease: cubic-bezier(0.25, 0.1, 0.25, 1)`; durations 80ms (fast) /
  140ms (page) / 180ms (drawer).
- Motion is entrance-only and 1–4px: `page-in` fade+2px rise, drawer slide,
  toast rise. Nothing loops except the 2px `busybar` progress line.
- The disclosure animates `grid-template-rows` (0fr→1fr) — the documented
  exception, chosen because it needs no height measurement.
- `prefers-reduced-motion: reduce` collapses every animation and transition to
  0.01ms in one block at the end of the file, so later motion is covered by
  default.
- Theme switches transition background/color over 120ms so a mode change never
  reads as a flash.

### Z-index scale

30 chart hover card · 40 drawer veil · 41 drawer · 60 popovers/menus · 70
toast host — a semantic ladder, commented at the chart card, with no 999s.

## Component families

| Family | Grammar |
|---|---|
| buttons | 1px `line` border, radius 0, black primary fill; ghost = text-only with hover tint; hover deepens the border to `text-2` |
| inputs | 1px `line` border, radius 0; numeric fields in mono at 12px; readonly = muted surface |
| segment | bottom-ruled strip; selection is a 2px inset underline, constant 500 weight so selection never reflows the strip |
| select / menus | trigger matches an input; menu is a floating layer (the shadow exception); highlighted row gets an inset focus ring |
| list rows | hairline bottom dividers, no containing box; hover = flat tint; selection = tint + 2px inset left edge so the two greys never need squinting |
| tables | 12px muted uppercase-free head ink; row rules; clickable rows reuse the list grammar exactly |
| drawer | fixed right sheet: border + surface step, no shadow; save-status foot pinned under the body |
| toasts | bottom-right buttons with a 7px status dot; info is neutral ink, not a third color |
| badges | tinted only for status; otherwise monochrome surface + hairline |
| charts | ink-only SVG: bars/lines/dots in text ink, 5-level tint ramp from 6% to 100%, hover card instead of native tooltips |
| KPI row | hairline-separated cells; the pressed measure carries the same 2px underline as segments — no cards |

## Accessibility

- Contrast floors are fixed and documented in-file: `--muted` went from #999999
  (2.85:1) to #767676 (4.54:1) light and #555555→#8a8a8a dark (5.47:1); badge
  status pairs were raised past 4.5:1 the same way.
- Every focusable element shares one ring: `outline: 2px solid var(--text)`,
  drawn inside where clipping demands it (tab panels, select items).
- State that is shown only as color carries a `.visually-hidden` text twin
  (status dots are `aria-hidden`; the state is stated next to them).
- Tables carry visually-hidden captions; the disclosure is a real `<button>`
  with `aria-expanded`; the tab panel takes `tabindex=0` so keyboard arrival
  lands somewhere named.
- The theme switch never leaves `--muted` under 4.5:1 on either plane.

## Do / Don't

Do:

- Add a new quiet ink by stepping between the greys that exist; do not add a
  hue.
- Put any new floating layer on `--shadow-layer` and a new page element on
  nothing.
- Keep selection flat: tint + the 2px edge, never a size or weight change.
- Route CJK through the UI face inside mono runs (the mono stack already
  carries the fallback).

Don't:

- Don't round a corner: `--radius: 0` is load-bearing, not an omission.
- Don't introduce a second status color set; `ok`/`warn`/`danger` and their
  dots are the whole status vocabulary.
- Don't animate layout outside the documented 0fr disclosure exception.
- Don't decorate: no gradient text, no glass, no icon furniture in nav, no
  illustration in empty states — a line, a hint, one action.

## Deliberate choices

1. **Serif UI.** A literary voice for a routing tool is the identity; Roboto
   and Segoe remain available as the plugin themes' faces.
2. **2px side marks.** The selected row's inset 2px edge and the inference
   pool's 2px left rule are monochrome ink carrying selection/nesting state —
   side stripes as *state*, not decoration.
3. **1080px page width.** Wider than a prose measure on purpose: this register
   is tables, ledgers and charts, and the table scrolls inside its own section.
4. **Stale header sentence.** The file header still says "one restrained blue
   for links and focus"; the palette has been strictly monochrome since the
   quiet-line work. The tokens rule; the header sentence is history.

## Sources

- `src/industrial.css` — every value and quoted contrast figure on this page.
- `src/styles.css` — import order and the `[data-host-security]` privileged
  prompt palette that pins the monochrome baseline for security surfaces.
- `src/theme.ts::applyTheme` — light/dark resolution via `data-theme`.
- `src/themes/preferences.ts::loadThemeRef` — `industrial` as the default ref.
- `src/themes/registry.ts::applyThemeRef` — safe-appearance fallback to this
  theme and plugin-theme deactivation.
- https://github.com/monoes/monomind — monodesign skill (contrast, motion,
  anti-slop rules) used as the audit lens.
