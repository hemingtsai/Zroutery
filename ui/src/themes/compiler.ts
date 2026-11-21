import type { ColorExpr, ComponentRecipe, ComponentState, ShadowValue, ThemeV1 } from "./types.ts";
import { COLOR_ROLES, THEME_PARTS, type ThemePart } from "./parts.ts";

export const THEME_RENDERER_VERSION = 1;
const fail = (): never => { throw new Error("invalid_theme"); };
const obj = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return fail();
  return value as Record<string, unknown>;
};
function keys(value: unknown, allowed: readonly string[]): Record<string, unknown> {
  const v = obj(value);
  if (Object.keys(v).some((k) => !allowed.includes(k))) fail();
  return v;
}
function number(value: unknown, low: number, high: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < low || value > high) fail();
  return value as number;
}
function label(value: unknown): void {
  const localized = keys(value, ["en", "zh-CN", "zh-TW"]);
  for (const lang of ["en", "zh-CN", "zh-TW"]) {
    if (typeof localized[lang] !== "string" || !(localized[lang] as string).length || (localized[lang] as string).length > 512 || /[\u0000-\u001f]/.test(localized[lang] as string)) fail();
  }
}

export function color(expr: ColorExpr, roles: Record<string, ColorExpr>, seen = new Set<string>(), depth = 0): string {
  if (depth > 16) fail();
  const v = obj(expr);
  if ("hex" in v) {
    keys(v, ["hex"]);
    if (typeof v.hex !== "string" || !/^#[\da-f]{6}([\da-f]{2})?$/i.test(v.hex)) fail();
    return v.hex as string;
  }
  if ("role" in v) {
    keys(v, ["role"]);
    const role = v.role as string;
    if (!COLOR_ROLES.includes(role as (typeof COLOR_ROLES)[number]) || !roles[role] || seen.has(role)) fail();
    seen.add(role);
    color(roles[role], roles, seen, depth + 1);
    seen.delete(role);
    return `var(--${role})`;
  }
  if ("system" in v) {
    keys(v, ["system", "fallback"]);
    if (v.system !== "accent" && v.system !== "accent_foreground") fail();
    return `var(--system-${v.system === "accent" ? "accent" : "accent-fg"},${color(v.fallback as ColorExpr, roles, seen, depth + 1)})`;
  }
  if ("tonal" in v) {
    // MD3 dynamic color, declaratively: keep the OS accent's hue, replace the
    // tone. `lightness`/`chroma` are the baseline role's own OKLCH coordinates,
    // so the fallback seed reproduces the published baseline scheme; `min(c,…)`
    // lets low-chroma accents go quietly neutral. Emitted as a relative color,
    // which WebView2 (Chromium 119+) resolves at paint time.
    keys(v, ["tonal"]);
    const t = obj(v.tonal);
    keys(t, ["of", "lightness", "chroma", "fallback"]);
    if (t.of !== "accent" && t.of !== "accent_foreground") fail();
    const accent = t.of === "accent" ? "accent" : "accent-fg";
    return `oklch(from var(--system-${accent},${color(t.fallback as ColorExpr, roles, seen, depth + 1)}) ${number(t.lightness, 0, 1)} min(c,${number(t.chroma, 0, 0.4)}) h)`;
  }
  keys(v, ["mix", "first_fraction"]);
  if (!Array.isArray(v.mix) || v.mix.length !== 2) fail();
  const fraction = number(v.first_fraction, 0, 1);
  return `color-mix(in srgb,${color((v.mix as ColorExpr[])[0], roles, seen, depth + 1)} ${fraction * 100}%,${color((v.mix as ColorExpr[])[1], roles, seen, depth + 1)})`;
}
const px = (value: number): string => `${value}px`;
function shadows(values: readonly ShadowValue[], roles: Record<string, ColorExpr>): string {
  if (!Array.isArray(values) || values.length > 4) fail();
  return values.map((s) => {
    keys(s, ["x", "y", "blur", "spread", "color", "inset"]);
    return `${s.inset ? "inset " : ""}${px(number(s.x, -64, 64))} ${px(number(s.y, -64, 64))} ${px(number(s.blur, 0, 64))} ${px(number(s.spread, -8, 8))} ${color(s.color, roles)}`;
  }).join(",") || "none";
}
function predicate(part: ThemePart, state: ComponentState): string {
  keys(state, ["interaction", "focus_visible", "disabled", "selected", "checked", "readonly", "context"]);
  if (!["rest", "hover", "pressed"].includes(state.interaction)) fail();
  let suffix = state.interaction === "rest" ? "" : state.interaction === "hover" ? ":hover" : ":active";
  const states: [keyof ComponentState, string][] = [
    ["focus_visible", ":focus-visible"], ["disabled", ":disabled"], ["readonly", "[readonly]"],
    // Radix marks a checked select row on the row itself; a menu radio's
    // `aria-checked` lives on the item its dot decorates (see selector()).
    ["checked", part === "select.item" ? "[data-state=checked]" : part === "menu.radio" ? "[aria-checked=true]" : ":checked"],
    ["selected", part.startsWith("kpi.") ? "[aria-pressed=true]" : part.startsWith("segment.") ? "[data-active=true]" : ".active"],
  ];
  for (const [key, selector] of states) {
    if (state[key] !== undefined) {
      if (typeof state[key] !== "boolean") fail();
      suffix += state[key] ? selector : `:not(${selector})`;
    }
  }
  if (state.context !== undefined && !["page", "section", "nested_section", "overlay"].includes(state.context)) fail();
  return suffix;
}
function selector(part: ThemePart, recipe: ComponentRecipe): string {
  let s: string = THEME_PARTS[part];
  const state = predicate(part, recipe.when);
  // The icon slot has no state of its own: its selected/hover states live on
  // the owning nav item, so the predicate moves to the ancestor (same shape
  // as the table.cell rewrite below).
  if (part === "navigation.icon") {
    return state ? s.replace(" .nav-icon", "") + state + " .nav-icon" : s;
  }
  if (recipe.variant) {
    if (!["default", "primary", "ghost", "danger", "warning", "error"].includes(recipe.variant)) fail();
    if (part === "button") s += recipe.variant === "default" ? ":not(.btn-primary):not(.btn-ghost):not(.btn-danger)" : `.btn-${recipe.variant}`;
    else if (part === "banner") s += recipe.variant === "warning" ? ".banner-warn" : recipe.variant === "error" ? ".banner-danger" : "";
    else fail();
  }
  // Predicates go on the input/row, not on its generated pseudo-element/cell.
  if (part === "table.cell") return s.replace(" tr td", ` tr${state} td`);
  if (part === "kpi.label" || part === "kpi.value") return s.replace("[aria-pressed=true]", `[aria-pressed=true]${state}`);
  // The menu radio's dot is decorated, but `aria-checked` is the item's.
  if (part === "menu.radio" && state.includes("[aria-checked=true]")) {
    return s.replace(".menu-radio", `.menu-item[aria-checked=true] .menu-radio`);
  }
  const pseudo = s.indexOf("::");
  return pseudo >= 0 ? s.slice(0, pseudo) + state + s.slice(pseudo) : s + state;
}
function priority(r: ComponentRecipe): number {
  // Host-defined stable priority: combination states outrank their constituents.
  return (r.when.interaction === "rest" ? 0 : r.when.interaction === "hover" ? 4 : 8)
    + (r.when.selected || r.when.checked ? 16 : 0) + (r.when.focus_visible ? 32 : 0)
    + (r.when.disabled ? 64 : 0) + (r.variant ? 2 : 0) + (r.part === "section.flat" ? 1 : 0);
}
function declarations(r: ComponentRecipe, roles: Record<string, ColorExpr>, rail_morphology = false): string {
  keys(r, ["part", "variant", "when", "fill", "foreground", "stroke", "stroke_gradient", "radius", "shadow", "focus_rings", "decoration", "layout", "material", "metrics", "stroke_edge"]);
  const part = r.part as ThemePart;
  const out: string[] = [];
  const add = (key: string, value: string) => out.push(`${key}:${value}`);
  // A pack that styles the disabled state takes over its rendering entirely:
  // the industrial base fades disabled controls to 0.4 opacity, which would
  // stack with the pack's own disabled ink (M3 disables at 12%/38%, Fluent at
  // ~5%/30% fills, neither through opacity).
  if (r.when.disabled) add("opacity", "1");
  if (r.stroke && r.stroke_gradient) fail();
  if (r.fill && !r.stroke_gradient) add("background", color(r.fill, roles));
  if (r.foreground) add("color", color(r.foreground, roles));
  if (r.radius !== undefined) {
    const radius = px(number(r.radius, 0, 999));
    add("border-radius", part === "drawer" ? `${radius} 0 0 ${radius}` : radius);
  }
  if (r.stroke) {
    keys(r.stroke, ["width", "color"]);
    const edge = r.stroke_edge ?? "all";
    if (!["all", "start", "end", "bottom"].includes(edge)) fail();
    const stroke = `${px(number(r.stroke.width, 0, 4))} solid ${color(r.stroke.color, roles)}`;
    add(edge === "all" ? "border" : `border-${edge === "start" ? "left" : edge === "end" ? "right" : "bottom"}`, stroke);
  }
  if (r.stroke_gradient) {
    // WinUI ControlElevationBorderBrush as a typed recipe: the fill is
    // re-emitted as the padding-box layer of the composite, so the gradient
    // paints only the ring around the fill, over the page background — the
    // base rule's background-color is dropped, or it would bleed through a
    // semi-transparent ring. Without the fill layer the border-box gradient
    // would flood the whole element.
    // `?? fail()` rather than a guard: the never-returning call keeps the
    // ColorExpr type through the block without a narrowing step.
    const fillExpr = r.fill ?? fail();
    const g = r.stroke_gradient;
    keys(g, ["width", "from", "to", "angle"]);
    if ((r.stroke_edge ?? "all") !== "all") fail();
    const angle = g.angle === undefined ? 180 : number(g.angle, 0, 360);
    const fill = color(fillExpr, roles);
    add("background-color", "transparent");
    add("border", `${px(number(g.width, 0, 4))} solid transparent`);
    add("background-image", `linear-gradient(${fill},${fill}),linear-gradient(${angle}deg,${color(g.from, roles)},${color(g.to, roles)})`);
    add("background-origin", "padding-box,border-box");
    add("background-clip", "padding-box,border-box");
  }
  if (r.focus_rings) {
    if (!Array.isArray(r.focus_rings) || r.focus_rings.length > 3) fail();
    if (r.focus_rings.length === 0) { add("outline", "none"); add("box-shadow", "none"); }
    else {
      const [first, ...rest] = r.focus_rings;
      for (const ring of r.focus_rings) keys(ring, ["width", "offset", "color"]);
      add("outline", `${px(number(first.width, 0, 4))} solid ${color(first.color, roles)}`);
      add("outline-offset", px(number(first.offset, -4, 4)));
      if (rest.length) add("box-shadow", rest.map((ring) => `0 0 0 ${px(number(ring.width, 0, 4))} ${color(ring.color, roles)}`).join(","));
    }
  }
  if (r.shadow) add("box-shadow", shadows(r.shadow, roles));
  if (r.metrics) {
    const m = r.metrics;
    keys(m, ["width", "height", "padding_block", "padding_inline", "padding_top", "padding_bottom", "padding_start", "gap", "column_gap", "font_size", "font_weight", "thumb_x", "thumb_y"]);
    for (const [key, value] of Object.entries(m)) {
      if (value === undefined) continue;
      if (key === "font_weight") { add("font-weight", String(number(value, 100, 800))); continue; }
      if (key === "thumb_x" || key === "thumb_y") {
        if (part !== "switch.thumb") fail();
        add(key === "thumb_x" ? "left" : "top", px(number(value, 0, 40))); continue;
      }
      const property = { padding_block: "padding-block", padding_inline: "padding-inline", padding_top: "padding-top", padding_bottom: "padding-bottom", padding_start: "padding-left", font_size: "font-size", column_gap: "column-gap" }[key] ?? key;
      const max = key === "width" && part === "shell.sidebar" ? 280 : key === "width" || key === "height" ? (part.startsWith("shell.") ? 64 : 48) : key === "font_size" ? 28 : 32;
      add(property, px(number(value, key === "font_size" ? 10 : 0, max)));
    }
  }
  if (r.material) {
    keys(r.material, ["kind", "opacity", "blur", "saturation"]);
    number(r.material.opacity, 0, 1);
    if (r.material.kind !== "solid" && r.material.kind !== "acrylic") fail();
    if (r.material.kind === "acrylic") {
      if (part !== "shell.sidebar") fail();
      const material = `blur(${px(number(r.material.blur, 0, 24))}) saturate(${number(r.material.saturation ?? 1, 0, 1.5)})`;
      add("-webkit-backdrop-filter", material); add("backdrop-filter", material);
    }
  }
  if (r.layout === "flatten_nested") { add("padding", "0"); add("border-radius", "0"); add("background", "none"); }
  else if (r.layout === "card_stack") { if (part !== "tab.panel") fail(); add("display", "flex"); add("flex-direction", "column"); }
  else if (r.layout === "nav_rail") {
    // M3 navigation rail: the icon sits above its label inside the item, and
    // the active indicator pill wraps the icon slot (styled via
    // navigation.icon) rather than the whole row.
    if (part !== "navigation.item") fail();
    add("display", "inline-flex"); add("flex-direction", "column");
    add("align-items", "center"); add("text-align", "center"); add("gap", "6px");
  }
  else if (r.layout === "flat") {
    if (part === "segment") { add("border", "none"); add("overflow", "hidden"); }
  } else if (r.layout && r.layout !== "filled_section") fail();
  if (r.decoration === "inherit_ink") add("color", "inherit");
  else if (r.decoration === "circle") { if (part !== "menu.radio") fail(); add("border-radius", "50%"); }
  else if (r.decoration === "indicator_anchor") { if (part !== "navigation.item" && part !== "segment.item") fail(); add("position", "relative"); }
  else if (r.decoration === "nav_indicator_morph") { if (part !== "navigation.item") fail(); /* no paint of its own: the pack declares the shared-element morph, the host wires it (marker var + companion rule) */ }
  else if (r.decoration === "plain_title") { add("text-transform", "none"); add("letter-spacing", "0"); }
  else if (r.decoration === "leading_indicator" || r.decoration === "underline") {
    if (part === "navigation.indicator") {
      add("content", '""'); add("position", "absolute"); add("left", "0"); add("top", "50%"); add("transform", "translateY(-50%)");
    } else if (part === "segment.indicator") {
      add("content", '""'); add("position", "absolute"); add("left", "12px"); add("right", "12px"); add("bottom", "0"); add("width", "auto");
    } else if (part === "toast.marker") { add("height", "auto"); add("align-self", "stretch"); }
    else fail();
  } else if (r.decoration && !["none", "state_layer"].includes(r.decoration)) fail();
  if (part.startsWith("switch.")) add("transition", "background-color var(--t-fast) var(--ease),border-color var(--t-fast) var(--ease),left var(--t-fast) var(--ease),top var(--t-fast) var(--ease),width var(--t-fast) var(--ease),height var(--t-fast) var(--ease)");
  // Selection is a fill change on these parts; a skin that restyles them gets
  // the host's state transition so the change animates instead of snapping.
  // Industrial itself stays transition-free here: no pack, no motion.
  if (part === "navigation.item" || part === "segment.item" || part === "kpi.trigger") {
    add("transition", "background-color var(--t-fast) var(--ease),color var(--t-fast) var(--ease),border-color var(--t-fast) var(--ease),box-shadow var(--t-fast) var(--ease)");
  }
  // The rail indicator is the one morph a user watches for, so it rides the
  // container slot (--t-page) rather than the micro-interaction slot.
  if (part === "navigation.icon") {
    add("transition", "background-color var(--t-page) var(--ease)");
  }
  // The nav icon slot is display:none in the host (industrial is text-only);
  // a skin that styles it means to show it.
  if (part === "navigation.icon") {
    // The slot is a sized box around the svg: center it, and — under the rail
    // morphology — drop the host's horizontal label gap, which would shove
    // the indicator pill off the column's centerline.
    add("display", "inline-flex");
    add("justify-content", "center");
    add("align-items", "center");
    if (rail_morphology) add("margin", "0");
  }
  if (part === "scrollbar.thumb") add("background-clip", "content-box");
  return out.join(";");
}

/** Shared validator/compiler for bundled and third-party data. No ID branches. */
export function compileTheme(value: unknown, reference?: string): string {
  if (reference !== undefined && !/^[a-zA-Z0-9_.-]{1,128}$/.test(reference)) fail();
  const raw = keys(value, ["schema_version", "name", "modes", "typography", "metrics", "motion"]);
  if (raw.schema_version !== 1) fail();
  label(raw.name);
  const theme = raw as unknown as ThemeV1;
  const motionCss = motion(theme);
  keys(theme.modes, ["light", "dark"]);
  keys(theme.typography, ["family", "scale"]);
  const families = {
    "host:industrial": '"Noto Serif SC Variable","Noto Serif SC","PingFang SC","Microsoft YaHei",serif',
    "host:roboto": '"Roboto","Microsoft YaHei",sans-serif',
    "system:segoe_ui": '"Segoe UI Variable","Segoe UI","Microsoft YaHei",sans-serif',
  };
  if (!(theme.typography.family in families) || !["compact", "material_desktop", "fluent_desktop"].includes(theme.typography.scale)) fail();
  keys(theme.metrics, ["control_height", "segment_height", "switch_width", "switch_height"]);
  number(theme.metrics.control_height, 24, 40); number(theme.metrics.segment_height, 24, 40);
  number(theme.metrics.switch_width, 28, 48); number(theme.metrics.switch_height, 16, 24);
  let css = "";
  const common = new Map<string, { target: string; declarations: string; modes: string[] }>();
  const rules: { target: string; declarations: string; mode: string; key: string }[] = [];
  for (const mode of ["light", "dark"] as const) {
    const root = `html[data-plugin-theme${reference ? `="${reference}"` : ""}][data-theme=${mode}]`;
    const sheet = theme.modes[mode];
    keys(sheet, ["colors", "recipes"]);
    const morphs = sheet.recipes.some((r) => r.part === "navigation.item" && r.decoration === "nav_indicator_morph");
    keys(sheet.colors, COLOR_ROLES);
    for (const role of ["bg", "surface", "surface-muted", "line", "line-muted", "text", "text-2", "muted", "primary", "primary-hover", "on-primary"]) if (!sheet.colors[role]) fail();
    if (!Array.isArray(sheet.recipes) || sheet.recipes.length > 512) fail();
    css += `${root}{--font-sans:${families[theme.typography.family]};--control-h:${theme.metrics.control_height}px;${motionCss}${morphs ? "--nav-indicator-morph:1;" : ""}`;
    css += Object.entries(sheet.colors).map(([role, expr]) => {
      const tonal = expr as { tonal?: { fallback: ColorExpr } };
      // A tonal role rides a double declaration: the baseline hex first, so an
      // engine without relative-color syntax never loses the role.
      const literal = tonal.tonal ? `--${role}:${color(tonal.tonal.fallback, sheet.colors)};` : "";
      return `${literal}--${role}:${color(expr, sheet.colors)}`;
    }).join(";") + "}\n";
    const rail_morphology = sheet.recipes.some((r) => r.part === "navigation.item" && r.layout === "nav_rail");
    for (const recipe of [...sheet.recipes].sort((a, b) => priority(a) - priority(b))) {
      if (!(recipe.part in THEME_PARTS)) fail();
      const part = recipe.part as ThemePart;
      // Security roots never match an external recipe, including universal focus.
      let target = selector(part, recipe);
      const pseudo = target.indexOf("::");
      const excluded = ":not([data-host-security],[data-host-security] *)";
      target = pseudo >= 0 ? target.slice(0, pseudo) + excluded + target.slice(pseudo) : target + excluded;
      const body = declarations(recipe, sheet.colors, rail_morphology);
      const key = `${target}{${body}}`;
      const existing = common.get(key);
      if (existing) { if (!existing.modes.includes(mode)) existing.modes.push(mode); }
      else common.set(key, { target, declarations: body, modes: [mode] });
      rules.push({ target, declarations: body, mode, key });
      // Radix highlights menu rows with `data-highlighted` during keyboard
      // navigation, exactly where a pointer would hover; a menu hover recipe
      // covers both, so the rule is emitted for each in turn.
      if ((part === "menu.item" || part === "menu.radio") && recipe.when.interaction === "hover") {
        const keyboardTarget = target.replace(":hover", "[data-highlighted]");
        const keyboardKey = `${keyboardTarget}{${body}}`;
        const keyboardExisting = common.get(keyboardKey);
        if (keyboardExisting) { if (!keyboardExisting.modes.includes(mode)) keyboardExisting.modes.push(mode); }
        else common.set(keyboardKey, { target: keyboardTarget, declarations: body, modes: [mode] });
        rules.push({ target: keyboardTarget, declarations: body, mode, key: keyboardKey });
      }
      // The rail puts the count badge under the label, where the host's
      // `margin-left: auto` push would strand it at the column edge.
      if (recipe.layout === "nav_rail") {
        const countTarget = `${target} .nav-count`;
        const countKey = `${countTarget}{margin:0}`;
        const countExisting = common.get(countKey);
        if (countExisting) { if (!countExisting.modes.includes(mode)) countExisting.modes.push(mode); }
        else common.set(countKey, { target: countTarget, declarations: "margin:0", modes: [mode] });
        rules.push({ target: countTarget, declarations: "margin:0", mode, key: countKey });
      }
      // The declared morph names the active item's icon slot as the shared
      // element the host's view transition slides.
      if (recipe.decoration === "nav_indicator_morph") {
        const morphTarget = target.replace(".nav-item", ".nav-item.active") + " .nav-icon";
        const morphKey = `${morphTarget}{view-transition-name:nav-indicator}`;
        const morphExisting = common.get(morphKey);
        if (morphExisting) { if (!morphExisting.modes.includes(mode)) morphExisting.modes.push(mode); }
        else common.set(morphKey, { target: morphTarget, declarations: "view-transition-name:nav-indicator", modes: [mode] });
        rules.push({ target: morphTarget, declarations: "view-transition-name:nav-indicator", mode, key: morphKey });
      }
    }
  }
  const emitted = new Set<string>();
  // Common rules first, then per-mode differences. Otherwise a dark card's
  // extra shadow value could move its base AFTER the common flatten-nested rule.
  for (const rule of [...rules.filter((r) => common.get(r.key)!.modes.length === 2), ...rules.filter((r) => common.get(r.key)!.modes.length === 1)]) {
    if (emitted.has(rule.key)) continue;
    emitted.add(rule.key);
    const root = `html[data-plugin-theme${reference ? `="${reference}"` : ""}]${common.get(rule.key)!.modes.length === 1 ? `[data-theme=${rule.mode}]` : "[data-theme]"}`;
    css += `${root} ${rule.target}{${rule.declarations}}\n`;
  }
  if (css.length > 192 * 1024 || /url\(|@import|<\/style/i.test(css)) fail();
  return css;
}

/** Host-owned easing curves; a pack picks a name, never a bezier string. */
export const MOTION_EASINGS = {
  standard: "cubic-bezier(0.2,0,0,1)",
  emphasized_accelerate: "cubic-bezier(0.3,0,0.8,0.15)",
  emphasized_decelerate: "cubic-bezier(0.05,0.7,0.1,1)",
  easy_ease: "cubic-bezier(0.33,0,0.67,1)",
  decelerate_max: "cubic-bezier(0.1,0.9,0.2,1)",
  accelerate_max: "cubic-bezier(0.9,0.1,1,0.2)",
  expo_out: "cubic-bezier(0.16,1,0.3,1)",
} as const;

function motion(theme: { motion?: unknown }): string {
  const raw = theme.motion;
  if (raw === undefined) return "";
  const v = obj(raw);
  keys(v, ["fast_ms", "page_ms", "drawer_ms", "ease"]);
  const ease = v.ease;
  if (typeof ease !== "string" || !(ease in MOTION_EASINGS)) fail();
  const fast = number(v.fast_ms, 30, 600);
  const page = number(v.page_ms, 30, 600);
  const drawer = number(v.drawer_ms, 30, 600);
  return `--t-fast:${fast}ms;--t-page:${page}ms;--t-drawer:${drawer}ms;--ease:${MOTION_EASINGS[ease as keyof typeof MOTION_EASINGS]};`;
}
