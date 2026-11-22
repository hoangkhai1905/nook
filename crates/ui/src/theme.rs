//! Design tokens (Material 3 tonal palette), plus font setup.

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use material_colors::color::Rgb;
use material_colors::hct::Hct;
use material_colors::scheme::Scheme;
use material_colors::scheme::variant::SchemeTonalSpot;
use serde::{Deserialize, Serialize};

/// Fallback accent when no wallpaper color is available: electric violet.
pub const FALLBACK_ACCENT: Color32 = Color32::from_rgb(0x7C, 0x5C, 0xFF);

/// 8 px spacing grid.
pub const SPACE: f32 = 8.0;
pub const TILE_RADIUS: f32 = 16.0;
pub const TILE_PADDING: f32 = 12.0;

pub const SIZE_LABEL: f32 = 11.0;
pub const SIZE_BODY: f32 = 13.0;
pub const SIZE_BIG: f32 = 28.0;
pub const SIZE_ICON: f32 = 18.0;

pub const FAMILY_SEMIBOLD: &str = "nook-semibold";
pub const FAMILY_ICON_FILL: &str = "phosphor-fill";
pub const FAMILY_ICON_REGULAR: &str = "phosphor";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Dark,
    Light,
    #[default]
    Auto,
}

/// User styling on top of the built-in palette (see the settings page).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Opacity of the surface over the blurred backdrop, 0..1.
    pub glass_opacity: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            glass_opacity: 0.85,
        }
    }
}

/// The Material 3 color roles Nook uses, from a tonal-spot scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Roles {
    pub primary: Color32,
    pub on_primary: Color32,
    pub primary_container: Color32,
    pub on_primary_container: Color32,
    pub secondary_container: Color32,
    pub on_secondary_container: Color32,
    pub surface: Color32,
    pub surface_container_low: Color32,
    pub surface_container: Color32,
    pub surface_container_high: Color32,
    pub surface_container_highest: Color32,
    pub on_surface: Color32,
    pub on_surface_variant: Color32,
    pub outline: Color32,
    pub outline_variant: Color32,
    pub error: Color32,
}

impl Roles {
    /// Tonal-spot scheme (light or dark) generated from `seed`.
    pub fn from_seed(seed: Color32, dark: bool) -> Self {
        let hct: Hct = Rgb::new(seed.r(), seed.g(), seed.b()).into();
        let s = Scheme::from(SchemeTonalSpot::new(hct, dark, None).scheme);
        let c = |c: Rgb| Color32::from_rgb(c.red, c.green, c.blue);
        Self {
            primary: c(s.primary),
            on_primary: c(s.on_primary),
            primary_container: c(s.primary_container),
            on_primary_container: c(s.on_primary_container),
            secondary_container: c(s.secondary_container),
            on_secondary_container: c(s.on_secondary_container),
            surface: c(s.surface),
            surface_container_low: c(s.surface_container_low),
            surface_container: c(s.surface_container),
            surface_container_high: c(s.surface_container_high),
            surface_container_highest: c(s.surface_container_highest),
            on_surface: c(s.on_surface),
            on_surface_variant: c(s.on_surface_variant),
            outline: c(s.outline),
            outline_variant: c(s.outline_variant),
            error: c(s.error),
        }
    }

    /// Role-by-role [`mix`]; `t = 0` is `a`.
    pub fn mix(a: &Self, b: &Self, t: f32) -> Self {
        let m = |x: Color32, y: Color32| mix(x, y, t);
        Self {
            primary: m(a.primary, b.primary),
            on_primary: m(a.on_primary, b.on_primary),
            primary_container: m(a.primary_container, b.primary_container),
            on_primary_container: m(a.on_primary_container, b.on_primary_container),
            secondary_container: m(a.secondary_container, b.secondary_container),
            on_secondary_container: m(a.on_secondary_container, b.on_secondary_container),
            surface: m(a.surface, b.surface),
            surface_container_low: m(a.surface_container_low, b.surface_container_low),
            surface_container: m(a.surface_container, b.surface_container),
            surface_container_high: m(a.surface_container_high, b.surface_container_high),
            surface_container_highest: m(a.surface_container_highest, b.surface_container_highest),
            on_surface: m(a.on_surface, b.on_surface),
            on_surface_variant: m(a.on_surface_variant, b.on_surface_variant),
            outline: m(a.outline, b.outline),
            outline_variant: m(a.outline_variant, b.outline_variant),
            error: m(a.error, b.error),
        }
    }
}

/// How long a palette change takes to cross-fade.
const FADE_SECS: f64 = 0.3;

/// Cross-fades [`Roles`] when the seed or the light/dark mode changes.
/// Asks for repaints only while a fade runs, so an idle shell stays idle.
pub struct Fade {
    key: Option<(Color32, bool)>,
    from: Roles,
    to: Roles,
    start: f64,
}

impl Default for Fade {
    fn default() -> Self {
        let r = Roles::from_seed(FALLBACK_ACCENT, true);
        Self {
            key: None,
            from: r,
            to: r,
            start: f64::NEG_INFINITY,
        }
    }
}

impl Fade {
    /// The roles to draw with at `now` (seconds).
    pub fn roles(&mut self, ctx: &egui::Context, seed: Color32, dark: bool, now: f64) -> Roles {
        if self.key != Some((seed, dark)) {
            let first = self.key.is_none();
            let shown = self.at(now);
            self.key = Some((seed, dark));
            self.to = Roles::from_seed(seed, dark);
            self.from = if first { self.to } else { shown };
            self.start = now;
        }
        if now - self.start < FADE_SECS {
            ctx.request_repaint();
        }
        self.at(now)
    }

    fn at(&self, now: f64) -> Roles {
        let t = ((now - self.start) / FADE_SECS).clamp(0.0, 1.0) as f32;
        Roles::mix(&self.from, &self.to, t)
    }
}

/// Resolved colors for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub roles: Roles,
    /// Primary.
    pub accent: Color32,
    /// Surface fill of the shell (top and bottom match: it is flat).
    pub glass_top: Color32,
    pub glass_bottom: Color32,
    pub tile_top: Color32,
    pub tile_bottom: Color32,
    pub tile_rim: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    /// The collapsed pill is always "ink" (a hardware-notch black), whatever
    /// the theme; it blends into the surface as the shell opens.
    pub pill_top: Color32,
    pub pill_bottom: Color32,
    pub pill_text: Color32,
    pub pill_text_dim: Color32,
}

impl Theme {
    /// `translucent` is true when a blurred backdrop (acrylic) sits behind
    /// the window; otherwise the surface is drawn opaque.
    pub fn new(dark: bool, seed: Color32, translucent: bool) -> Self {
        Self::styled(dark, seed, translucent, &Look::default())
    }

    /// [`Theme::new`] with the user's [`Look`] applied.
    pub fn styled(dark: bool, seed: Color32, translucent: bool, look: &Look) -> Self {
        Self::from_roles(dark, Roles::from_seed(seed, dark), translucent, look)
    }

    /// Builds the theme from already generated (or mid-fade) roles.
    pub fn from_roles(dark: bool, roles: Roles, translucent: bool, look: &Look) -> Self {
        let alpha = if translucent {
            look.glass_opacity.clamp(0.2, 1.0)
        } else {
            1.0
        };
        let surface = with_alpha(roles.surface, alpha);
        let ink_alpha = if translucent { 0.90 } else { 1.0 };
        Self {
            dark,
            roles,
            accent: roles.primary,
            glass_top: surface,
            glass_bottom: surface,
            tile_top: roles.surface_container,
            tile_bottom: roles.surface_container,
            tile_rim: Color32::TRANSPARENT,
            text: roles.on_surface,
            text_dim: roles.on_surface_variant,
            text_faint: roles.outline,
            pill_top: with_alpha(Color32::from_rgb(24, 24, 28), ink_alpha),
            pill_bottom: with_alpha(Color32::from_rgb(6, 6, 8), ink_alpha),
            pill_text: Color32::from_rgb(244, 244, 246),
            pill_text_dim: Color32::from_rgb(150, 150, 160),
        }
    }

    /// Surface fill (top, bottom) at shell `progress`: ink pill at 0, the
    /// surface from ~70% open.
    pub fn glass_at(&self, progress: f32) -> (Color32, Color32) {
        let t = blend(progress);
        (
            mix(self.pill_top, self.glass_top, t),
            mix(self.pill_bottom, self.glass_bottom, t),
        )
    }

    /// Fill for active tiles: the primary container.
    pub fn active_tile(&self) -> (Color32, Color32) {
        (self.roles.primary_container, self.roles.primary_container)
    }

    /// Accent readable as text/icon color on the surface.
    pub fn accent_fg(&self) -> Color32 {
        self.roles.primary
    }

    pub fn font(size: f32) -> FontId {
        FontId::proportional(size)
    }

    pub fn font_semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(FAMILY_SEMIBOLD.into()))
    }
}

/// Smoothstep of `progress / 0.7`: how far the pill look has turned into
/// the panel look.
fn blend(progress: f32) -> f32 {
    let t = (progress / 0.7).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Multiplies a color's alpha (unmultiplied semantics).
pub fn with_alpha(c: Color32, alpha: f32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let a = (a as f32 * alpha.clamp(0.0, 1.0)).round() as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Mixes two colors in gamma space; `t = 0` is `a`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let [ar, ag, ab, aa] = a.to_srgba_unmultiplied();
    let [br, bg, bb, ba] = b.to_srgba_unmultiplied();
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(ar, br), l(ag, bg), l(ab, bb), l(aa, ba))
}

/// Parses `#RRGGBB`.
pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// Installs the text and icon fonts.
///
/// Text uses Google Sans Flex (SIL OFL 1.1), a variable font bundled in the
/// binary. The same bytes are registered twice with different `wght`
/// coordinates: 400 for regular, 500 for [`FAMILY_SEMIBOLD`]. Its digits are
/// proportional and egui cannot switch on `tnum`, so ticking text goes
/// through `paint::digit_slots`. egui's bundled font stays as a fallback.
pub fn install_fonts(ctx: &egui::Context) {
    const GSF: &[u8] = include_bytes!("../assets/fonts/GoogleSansFlex.ttf");
    let mut fonts = FontDefinitions::default();
    let weight = |w: f32| {
        let mut data = FontData::from_static(GSF);
        data.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", w)]);
        data.into()
    };
    fonts.font_data.insert("gsf".into(), weight(400.0));
    fonts.font_data.insert("gsf-medium".into(), weight(500.0));
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "gsf".into());
    let mut medium = proportional.clone();
    medium[0] = "gsf-medium".into();
    fonts
        .families
        .insert(FontFamily::Name(FAMILY_SEMIBOLD.into()), medium);

    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    egui_phosphor::add_font_bytes_as_family(
        &mut fonts,
        FAMILY_ICON_FILL,
        egui_phosphor::Variant::Fill.font_bytes(),
    );
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex("#7C5CFF"), Some(FALLBACK_ACCENT));
        assert_eq!(to_hex(FALLBACK_ACCENT), "#7C5CFF");
        assert_eq!(parse_hex("nope"), None);
    }

    #[test]
    fn alpha_and_mix() {
        assert_eq!(with_alpha(Color32::WHITE, 0.5).a(), 128);
        assert_eq!(mix(Color32::BLACK, Color32::WHITE, 1.0), Color32::WHITE);
    }

    fn luminance(c: Color32) -> f64 {
        let l = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * l(c.r()) + 0.7152 * l(c.g()) + 0.0722 * l(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn seed_gives_distinct_light_and_dark_schemes() {
        let dark = Roles::from_seed(FALLBACK_ACCENT, true);
        let light = Roles::from_seed(FALLBACK_ACCENT, false);
        assert_ne!(dark, light);
        assert!(luminance(dark.surface) < luminance(light.surface));
    }

    #[test]
    fn on_surface_is_legible_on_surface() {
        for seed in [
            FALLBACK_ACCENT,
            Color32::from_rgb(230, 190, 40),
            Color32::WHITE,
        ] {
            for dark in [true, false] {
                let r = Roles::from_seed(seed, dark);
                assert!(contrast(r.on_surface, r.surface) >= 4.5);
            }
        }
    }

    #[test]
    fn look_controls_opacity() {
        let clear = Look { glass_opacity: 0.5 };
        let t = Theme::styled(true, FALLBACK_ACCENT, true, &clear);
        assert_eq!(t.glass_top.to_srgba_unmultiplied()[3], 128);
        assert_eq!(t.glass_top, t.glass_bottom);
        // The solid material ignores the opacity setting.
        let t = Theme::styled(true, FALLBACK_ACCENT, false, &clear);
        assert_eq!(t.glass_top.to_srgba_unmultiplied()[3], 255);
        assert_eq!(Look::default().glass_opacity, 0.85);
    }

    #[test]
    fn fade_runs_300ms_then_settles() {
        let ctx = egui::Context::default();
        let mut f = Fade::default();
        let first = f.roles(&ctx, FALLBACK_ACCENT, true, 10.0);
        assert_eq!(first, Roles::from_seed(FALLBACK_ACCENT, true));
        let target = Roles::from_seed(FALLBACK_ACCENT, false);
        assert_eq!(f.roles(&ctx, FALLBACK_ACCENT, false, 10.0), first);
        let mid = f.roles(&ctx, FALLBACK_ACCENT, false, 10.15);
        assert_ne!(mid, first);
        assert_ne!(mid, target);
        assert_eq!(f.roles(&ctx, FALLBACK_ACCENT, false, 10.31), target);
    }
}
