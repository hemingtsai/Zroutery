//! Data-only theme contract. Published parts/properties compile to host-owned CSS.
use crate::{strict, Localized, PluginError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PARTS: &[&str] = &[
    "button",
    "input",
    "select.trigger",
    "select.menu",
    "select.item",
    "checkbox",
    "focus.target",
    "segment",
    "segment.item",
    "segment.indicator",
    "navigation.item",
    "navigation.icon",
    "navigation.indicator",
    "drawer",
    "banner",
    "dialog",
    "popover",
    "menu.item",
    "badge",
    "chip",
    "kpi.row",
    "kpi.cell",
    "kpi.first",
    "kpi.trigger",
    "kpi.label",
    "kpi.value",
    "table.cell",
    "list.row",
    "timeline.item",
    "toast",
    "toast.marker",
    "chart.card",
    "menu.radio",
    "switch.track",
    "switch.thumb",
    "section",
    "section.flat",
    "tab.panel",
    "field.label",
    "section.title",
    "shell.bar",
    "shell.sidebar",
    "shell.title",
    "scrollbar",
    "scrollbar.thumb",
    "chart",
];
pub const COLOR_ROLES: &[&str] = &[
    "bg",
    "surface",
    "surface-muted",
    "surface-high",
    "container-highest",
    "hover",
    "selected",
    "line",
    "line-muted",
    "line-strong",
    "control-line",
    "border-primary",
    "border-secondary",
    "text",
    "text-2",
    "muted",
    "primary",
    "primary-hover",
    "on-primary",
    "primary-container",
    "on-primary-container",
    "secondary-container",
    "on-secondary-container",
    "error-container",
    "on-error-container",
    "inverse-surface",
    "inverse-on-surface",
    "inverse-primary",
    "ok",
    "ok-bg",
    "warn",
    "warn-bg",
    "danger",
    "danger-bg",
    "scrollbar-thumb",
    "scrollbar-thumb-hover",
    "card-fill",
    "card-stroke",
    "subtle-secondary",
    "control-alt",
    "divider",
    "stroke-hover",
    "stroke-pressed",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorExpr {
    Hex(HexColor),
    Role(RoleColor),
    System(SystemColor),
    Tonal(TonalColor),
    Mix(MixColor),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HexColor {
    pub hex: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleColor {
    pub role: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemColor {
    pub system: AccentRole,
    pub fallback: Box<ColorExpr>,
}
/// MD3 dynamic color: hue from the OS accent, lightness and chroma ceiling
/// from the baseline role's own OKLCH coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TonalColor {
    pub tonal: TonalSpec,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TonalSpec {
    pub of: AccentRole,
    pub lightness: f64,
    pub chroma: f64,
    pub fallback: Box<ColorExpr>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentRole {
    Accent,
    AccentForeground,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixColor {
    pub mix: [Box<ColorExpr>; 2],
    pub first_fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowValue {
    pub x: f64,
    pub y: f64,
    pub blur: f64,
    pub spread: f64,
    pub color: ColorExpr,
    #[serde(default)]
    pub inset: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusRing {
    pub width: f64,
    pub offset: f64,
    pub color: ColorExpr,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentState {
    #[serde(default)]
    pub interaction: Interaction,
    #[serde(default)]
    pub focus_visible: Option<bool>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub selected: Option<bool>,
    #[serde(default)]
    pub checked: Option<bool>,
    #[serde(default)]
    pub readonly: Option<bool>,
    #[serde(default)]
    pub context: Option<ComponentContext>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interaction {
    #[default]
    Rest,
    Hover,
    Pressed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentContext {
    Page,
    Section,
    NestedSection,
    Overlay,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeMetrics {
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    #[serde(default)]
    pub padding_block: Option<f64>,
    #[serde(default)]
    pub padding_inline: Option<f64>,
    #[serde(default)]
    pub padding_top: Option<f64>,
    #[serde(default)]
    pub padding_bottom: Option<f64>,
    #[serde(default)]
    pub padding_start: Option<f64>,
    #[serde(default)]
    pub gap: Option<f64>,
    #[serde(default)]
    pub column_gap: Option<f64>,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub font_weight: Option<u16>,
    // Thumb position and indicator geometry are published part-local bounds;
    // these fields cannot position an arbitrary component or overlay.
    #[serde(default)]
    pub thumb_x: Option<f64>,
    #[serde(default)]
    pub thumb_y: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Material {
    pub kind: MaterialKind,
    pub opacity: f64,
    pub blur: f64,
    #[serde(default = "one")]
    pub saturation: f64,
}
fn one() -> f64 {
    1.0
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialKind {
    Solid,
    Acrylic,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decoration {
    None,
    Underline,
    LeadingIndicator,
    StateLayer,
    InheritInk,
    PlainTitle,
    Circle,
    IndicatorAnchor,
    NavIndicatorMorph,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipeLayout {
    Flat,
    FilledSection,
    FlattenNested,
    CardStack,
    NavRail,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub width: f64,
    pub color: ColorExpr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeGradient {
    pub width: f64,
    pub from: ColorExpr,
    pub to: ColorExpr,
    #[serde(default)]
    pub angle: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRecipe {
    pub part: String,
    #[serde(default)]
    pub variant: Option<String>,
    pub when: ComponentState,
    #[serde(default)]
    pub fill: Option<ColorExpr>,
    #[serde(default)]
    pub foreground: Option<ColorExpr>,
    #[serde(default)]
    pub stroke: Option<Stroke>,
    #[serde(default)]
    pub stroke_gradient: Option<StrokeGradient>,
    #[serde(default)]
    pub radius: Option<f64>,
    #[serde(default)]
    pub shadow: Option<Vec<ShadowValue>>,
    #[serde(default)]
    pub focus_rings: Option<Vec<FocusRing>>,
    #[serde(default)]
    pub decoration: Option<Decoration>,
    #[serde(default)]
    pub layout: Option<RecipeLayout>,
    #[serde(default)]
    pub material: Option<Material>,
    #[serde(default)]
    pub metrics: Option<RecipeMetrics>,
    #[serde(default)]
    pub stroke_edge: Option<StrokeEdge>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrokeEdge {
    All,
    Start,
    End,
    Bottom,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMode {
    pub colors: BTreeMap<String, ColorExpr>,
    pub recipes: Vec<ComponentRecipe>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeModes {
    pub light: ThemeMode,
    pub dark: ThemeMode,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Typography {
    pub family: FontRole,
    pub scale: TypeScale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontRole {
    #[serde(rename = "host:industrial")]
    Industrial,
    #[serde(rename = "host:roboto")]
    Roboto,
    #[serde(rename = "system:segoe_ui")]
    SegoeUi,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeScale {
    Compact,
    MaterialDesktop,
    FluentDesktop,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMetrics {
    pub control_height: f64,
    pub segment_height: f64,
    pub switch_width: f64,
    pub switch_height: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeV1 {
    pub schema_version: u16,
    pub name: Localized,
    pub modes: ThemeModes,
    pub typography: Typography,
    pub metrics: ThemeMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<Motion>,
}

/// Host motion tokens a skin may retune: the three duration slots every host
/// transition reads and the shared easing curve, chosen from the host's
/// closed curve set. Absent keeps the industrial defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Motion {
    pub fast_ms: f64,
    pub page_ms: f64,
    pub drawer_ms: f64,
    pub ease: MotionEase,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionEase {
    Standard,
    EmphasizedAccelerate,
    EmphasizedDecelerate,
    EasyEase,
    DecelerateMax,
    AccelerateMax,
    ExpoOut,
}

fn finite(value: f64, max: f64) -> bool {
    value.is_finite() && (0.0..=max).contains(&value)
}
fn color_valid(
    value: &ColorExpr,
    colors: &BTreeMap<String, ColorExpr>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> bool {
    if depth > 16 {
        return false;
    }
    match value {
        ColorExpr::Hex(v) => {
            matches!(v.hex.len(), 7 | 9)
                && v.hex.starts_with('#')
                && v.hex[1..].bytes().all(|c| c.is_ascii_hexdigit())
        }
        ColorExpr::Role(v) => {
            if !COLOR_ROLES.contains(&v.role.as_str()) || !seen.insert(v.role.clone()) {
                return false;
            }
            let valid = colors
                .get(&v.role)
                .is_some_and(|next| color_valid(next, colors, seen, depth + 1));
            seen.remove(&v.role);
            valid
        }
        ColorExpr::System(v) => color_valid(&v.fallback, colors, seen, depth + 1),
        ColorExpr::Tonal(v) => {
            finite(v.tonal.lightness, 1.0)
                && finite(v.tonal.chroma, 0.4)
                && color_valid(&v.tonal.fallback, colors, seen, depth + 1)
        }
        ColorExpr::Mix(v) => {
            finite(v.first_fraction, 1.0)
                && v.mix
                    .iter()
                    .all(|c| color_valid(c, colors, seen, depth + 1))
        }
    }
}
impl ThemeV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || !self.name.valid()
            || !(24.0..=40.0).contains(&self.metrics.control_height)
            || !(24.0..=40.0).contains(&self.metrics.segment_height)
            || !(28.0..=48.0).contains(&self.metrics.switch_width)
            || !(16.0..=24.0).contains(&self.metrics.switch_height)
        {
            return Err(PluginError::InvalidManifest);
        }
        if let Some(motion) = &self.motion {
            let durations_ok = [motion.fast_ms, motion.page_ms, motion.drawer_ms]
                .iter()
                .all(|d| (30.0..=600.0).contains(d));
            if !durations_ok {
                return Err(PluginError::InvalidManifest);
            }
        }
        for mode in [&self.modes.light, &self.modes.dark] {
            if mode.colors.len() > COLOR_ROLES.len()
                || mode.recipes.len() > 512
                || mode
                    .colors
                    .keys()
                    .any(|key| !COLOR_ROLES.contains(&key.as_str()))
            {
                return Err(PluginError::InvalidManifest);
            }
            for role in [
                "bg",
                "surface",
                "surface-muted",
                "line",
                "line-muted",
                "text",
                "text-2",
                "muted",
                "primary",
                "primary-hover",
                "on-primary",
            ] {
                if !mode.colors.contains_key(role) {
                    return Err(PluginError::InvalidManifest);
                }
            }
            let check_color = |c: &ColorExpr| color_valid(c, &mode.colors, &mut BTreeSet::new(), 0);
            if !mode.colors.values().all(check_color) {
                return Err(PluginError::InvalidManifest);
            }
            for r in &mode.recipes {
                if !PARTS.contains(&r.part.as_str())
                    || r.variant.as_deref().is_some_and(|v| {
                        !matches!(
                            v,
                            "default" | "primary" | "ghost" | "danger" | "warning" | "error"
                        )
                    })
                    || r.radius.is_some_and(|v| !finite(v, 999.0))
                    || r.fill.as_ref().is_some_and(|v| !check_color(v))
                    || r.foreground.as_ref().is_some_and(|v| !check_color(v))
                    || r.stroke
                        .as_ref()
                        .is_some_and(|v| !finite(v.width, 4.0) || !check_color(&v.color))
                {
                    return Err(PluginError::InvalidManifest);
                }
                if let Some(gradient) = &r.stroke_gradient {
                    if r.stroke.is_some()
                        || r.fill.is_none()
                        || r.stroke_edge.is_some_and(|edge| edge != StrokeEdge::All)
                        || !finite(gradient.width, 4.0)
                        || gradient.angle.is_some_and(|angle| !finite(angle, 360.0))
                        || !check_color(&gradient.from)
                        || !check_color(&gradient.to)
                    {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                if let Some(shadows) = &r.shadow {
                    if shadows.len() > 4
                        || shadows.iter().any(|s| {
                            !s.x.is_finite()
                                || !s.y.is_finite()
                                || s.x.abs() > 64.0
                                || s.y.abs() > 64.0
                                || !finite(s.blur, 64.0)
                                || !s.spread.is_finite()
                                || s.spread.abs() > 8.0
                                || !check_color(&s.color)
                        })
                    {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                if let Some(rings) = &r.focus_rings {
                    if rings.len() > 3
                        || rings.iter().any(|s| {
                            !finite(s.width, 4.0)
                                || !s.offset.is_finite()
                                || s.offset.abs() > 4.0
                                || !check_color(&s.color)
                        })
                    {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                if let Some(m) = &r.material {
                    if !finite(m.opacity, 1.0)
                        || !finite(m.blur, 24.0)
                        || !finite(m.saturation, 1.5)
                    {
                        return Err(PluginError::InvalidManifest);
                    }
                }
                if let Some(m) = &r.metrics {
                    if [m.width, m.height].into_iter().flatten().any(|v| {
                        // The sidebar width is the one shell dimension a rail
                        // theme must own; the compiler keeps the exact caps.
                        !finite(
                            v,
                            if r.part == "shell.sidebar" {
                                280.0
                            } else if r.part.starts_with("shell.") {
                                64.0
                            } else {
                                48.0
                            },
                        )
                    }) || [
                        m.padding_block,
                        m.padding_inline,
                        m.padding_top,
                        m.padding_bottom,
                        m.padding_start,
                        m.gap,
                        m.column_gap,
                    ]
                    .into_iter()
                    .flatten()
                    .any(|v| !finite(v, 32.0))
                        || m.font_size.is_some_and(|v| !(10.0..=28.0).contains(&v))
                        || m.font_weight.is_some_and(|v| !(100..=800).contains(&v))
                        || [m.thumb_x, m.thumb_y]
                            .into_iter()
                            .flatten()
                            .any(|v| !finite(v, 40.0))
                        || ((m.thumb_x.is_some() || m.thumb_y.is_some())
                            && r.part != "switch.thumb")
                    {
                        return Err(PluginError::InvalidManifest);
                    }
                }
            }
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let theme: Self = strict::decode(bytes, strict::MAX_FRAME_BYTES)?;
        theme.validate()?;
        Ok(theme)
    }
}

#[cfg(test)]
mod gradient_tests {
    use super::*;

    fn theme() -> serde_json::Value {
        serde_json::from_slice(include_bytes!("../../../plugins/theme-fluent/theme.json")).unwrap()
    }

    #[test]
    fn bundled_themes_validate_in_the_native_host() {
        for bytes in [
            include_bytes!("../../../plugins/theme-fluent/theme.json").as_slice(),
            include_bytes!("../../../plugins/theme-md3/theme.json").as_slice(),
        ] {
            ThemeV1::decode(bytes).unwrap();
        }
    }

    #[test]
    fn tonal_binds_the_accent_hue_to_a_bounded_baseline_profile() {
        let value: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../plugins/theme-md3/theme.json"))
                .unwrap();
        assert!(
            value["modes"]["light"]["colors"]["primary-container"]
                .get("tonal")
                .is_some(),
            "md3 baseline ships tonal containers"
        );
        // A different official seed rides the same profile.
        let mut reseeded = value.clone();
        reseeded["modes"]["light"]["colors"]["primary-container"]["tonal"]["fallback"] =
            serde_json::json!({"hex": "#0078d4"});
        ThemeV1::decode(&serde_json::to_vec(&reseeded).unwrap()).unwrap();
        for (field, bad) in [
            ("of", serde_json::json!("hue")),
            ("lightness", serde_json::json!(1.2)),
            ("lightness", serde_json::json!(-0.1)),
            ("chroma", serde_json::json!(0.5)),
        ] {
            let mut broken = value.clone();
            broken["modes"]["light"]["colors"]["primary-container"]["tonal"][field] = bad.clone();
            let bytes = serde_json::to_vec(&broken).unwrap();
            assert!(
                ThemeV1::decode(&bytes).is_err(),
                "reject tonal {field} = {bad}"
            );
        }
        let mut unknown = value.clone();
        unknown["modes"]["light"]["colors"]["primary-container"]["tonal"]["tone"] =
            serde_json::json!(90);
        let bytes = serde_json::to_vec(&unknown).unwrap();
        assert!(
            ThemeV1::decode(&bytes).is_err(),
            "deny unknown tonal fields"
        );
    }

    #[test]
    fn motion_retunes_only_the_published_slots() {
        let value: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../plugins/theme-md3/theme.json"))
                .unwrap();
        assert!(value.get("motion").is_some(), "md3 ships motion");
        ThemeV1::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
        for (field, bad) in [
            ("fast_ms", serde_json::json!(10)),
            ("page_ms", serde_json::json!(700)),
            ("ease", serde_json::json!("spring")),
        ] {
            let mut broken = value.clone();
            broken["motion"][field] = bad.clone();
            let bytes = serde_json::to_vec(&broken).unwrap();
            assert!(
                ThemeV1::decode(&bytes).is_err(),
                "reject motion {field} = {bad}"
            );
        }
        let mut unknown = value.clone();
        unknown["motion"]["curve"] = serde_json::json!("x");
        let bytes = serde_json::to_vec(&unknown).unwrap();
        assert!(
            ThemeV1::decode(&bytes).is_err(),
            "deny unknown motion fields"
        );
    }

    #[test]
    fn gradient_requires_fill_excludes_stroke_and_bounds_all_values() {
        for mutation in [
            "missing_fill",
            "stroke",
            "edge",
            "width",
            "angle",
            "color",
            "unknown",
        ] {
            let mut value = theme();
            let recipe = value["modes"]["light"]["recipes"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|recipe| recipe.get("stroke_gradient").is_some())
                .unwrap();
            match mutation {
                "missing_fill" => {
                    recipe.as_object_mut().unwrap().remove("fill");
                }
                "stroke" => {
                    recipe["stroke"] = serde_json::json!({"width":1,"color":{"hex":"#000000"}});
                }
                "edge" => {
                    recipe["stroke_edge"] = serde_json::json!("bottom");
                }
                "width" => {
                    recipe["stroke_gradient"]["width"] = serde_json::json!(5);
                }
                "angle" => {
                    recipe["stroke_gradient"]["angle"] = serde_json::json!(361);
                }
                "color" => {
                    recipe["stroke_gradient"]["from"] = serde_json::json!({"role":"not-a-role"});
                }
                "unknown" => {
                    recipe["stroke_gradient"]["css"] = serde_json::json!("arbitrary");
                }
                _ => unreachable!(),
            }
            assert!(
                ThemeV1::decode(&serde_json::to_vec(&value).unwrap()).is_err(),
                "{mutation}"
            );
        }
    }
}
