//! Low-level painting: squircle fills with vertical gradients, the glass
//! surface (a flat or gradient fill), glows, icons and rings.

use crate::geometry::Squircle;
use crate::theme::{FAMILY_ICON_FILL, FAMILY_ICON_REGULAR, Theme, with_alpha};
use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{
    Align2, Color32, Context, FontFamily, FontId, Painter, Pos2, Shape, Stroke, pos2, vec2,
};

/// Advance of each char of `text`, with every digit widened to the widest
/// digit 0-9 so a ticking number never shifts. Google Sans Flex has
/// proportional digits and egui cannot enable `tnum`.
fn slot_advances(text: &str, mut glyph_w: impl FnMut(char) -> f32) -> Vec<f32> {
    let slot = ('0'..='9').map(&mut glyph_w).fold(0.0, f32::max);
    text.chars()
        .map(|c| if c.is_ascii_digit() { slot } else { glyph_w(c) })
        .collect()
}

fn glyph_widths(ctx: &Context, text: &str, font: &FontId) -> (Vec<f32>, f32) {
    ctx.fonts_mut(|f| {
        (
            slot_advances(text, |c| f.glyph_width(font, c)),
            f.row_height(font),
        )
    })
}

/// Width of `text` as [`digit_slots`] paints it.
pub fn digit_slots_width(ctx: &Context, text: &str, font: &FontId) -> f32 {
    glyph_widths(ctx, text, font).0.iter().sum()
}

/// Paints `text` with each digit centered in a fixed-width slot, anchored
/// like [`Painter::text`]. Returns the line box.
pub fn digit_slots(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    anchor: Pos2,
    align: Align2,
) -> egui::Rect {
    let (advances, height) = glyph_widths(painter.ctx(), text, &font);
    let rect = align.anchor_size(anchor, vec2(advances.iter().sum(), height));
    let mut x = rect.min.x;
    for (c, adv) in text.chars().zip(advances) {
        let w = painter.ctx().fonts_mut(|f| f.glyph_width(&font, c));
        painter.text(
            pos2(x + (adv - w) / 2.0, rect.min.y),
            Align2::LEFT_TOP,
            c,
            font.clone(),
            color,
        );
        x += adv;
    }
    rect
}

/// Vertical-gradient fill of a convex outline (fan triangulation from the
/// centroid; colors are linear in y, so the interpolation is exact).
pub fn fill_gradient(painter: &Painter, outline: &[Pos2], top: Color32, bottom: Color32) {
    if outline.len() < 3 {
        return;
    }
    let (min_y, max_y) = outline.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
        (lo.min(p.y), hi.max(p.y))
    });
    let span = (max_y - min_y).max(1.0);
    let color_at = |y: f32| lerp_color(top, bottom, (y - min_y) / span);
    let mut mesh = Mesh::default();
    let center = centroid(outline);
    mesh.colored_vertex(center, color_at(center.y));
    for p in outline {
        mesh.colored_vertex(*p, color_at(p.y));
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(Shape::mesh(mesh));
}

/// The shell's surface at shell `progress` (0 = pill, 1 = panel). With
/// `fill = false` the compositor draws it (see `nook_platform::backdrop`).
pub fn glass(
    painter: &Painter,
    outline: &[Pos2],
    theme: &Theme,
    opacity: f32,
    progress: f32,
    fill: bool,
) {
    if fill {
        let (top, bottom) = theme.glass_at(progress);
        fill_gradient(
            painter,
            outline,
            with_alpha(top, opacity),
            with_alpha(bottom, opacity),
        );
    }
}

/// Soft glow: a few expanding, fading squircles under a shape.
pub fn glow(painter: &Painter, shape: &Squircle, color: Color32, strength: f32) {
    if strength <= 0.01 {
        return;
    }
    for (grow, a) in [(6.0, 0.10), (3.5, 0.16), (1.5, 0.24)] {
        let s = Squircle::new(shape.rect.expand(grow), shape.radius + grow)
            .with_exponent(shape.exponent);
        painter.add(Shape::convex_polygon(
            s.outline(),
            with_alpha(color, a * strength),
            Stroke::NONE,
        ));
    }
}

/// A Phosphor glyph. `fill` selects the filled variant (active state).
pub fn icon(painter: &Painter, center: Pos2, glyph: &str, size: f32, color: Color32, fill: bool) {
    let family = if fill {
        FAMILY_ICON_FILL
    } else {
        FAMILY_ICON_REGULAR
    };
    painter.text(
        center,
        Align2::CENTER_CENTER,
        glyph,
        FontId::new(size, FontFamily::Name(family.into())),
        color,
    );
}

/// Progress ring; `progress` in 0..1 runs clockwise from 12 o'clock.
pub fn ring(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    width: f32,
    progress: f32,
    track: Color32,
    color: Color32,
) {
    painter.circle_stroke(center, radius, Stroke::new(width, track));
    let progress = progress.clamp(0.0, 1.0);
    if progress <= 0.0 {
        return;
    }
    let segments = ((64.0 * progress).ceil() as usize).max(2);
    let start = -std::f32::consts::FRAC_PI_2;
    let points: Vec<Pos2> = (0..=segments)
        .map(|i| {
            let a = start + std::f32::consts::TAU * progress * i as f32 / segments as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    painter.add(PathShape::line(points, PathStroke::new(width, color)));
}

fn centroid(points: &[Pos2]) -> Pos2 {
    let sum = points
        .iter()
        .fold(vec2(0.0, 0.0), |acc, p| acc + p.to_vec2());
    (sum / points.len() as f32).to_pos2()
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    // Premultiplied components interpolate correctly for alpha blending.
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_slots_keep_width_with_the_real_font() {
        let ctx = Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        let font = FontId::proportional(14.0);
        let w = |s| digit_slots_width(&ctx, s, &font);
        assert!(w("00:00") > 0.0);
        assert_eq!(w("11:11"), w("00:00"));
        assert_eq!(w("8:59"), w("1:11"));
    }

    #[test]
    fn slots_only_widen_digits() {
        let adv = slot_advances("1:0", |c| match c {
            '1' => 3.0,
            ':' => 2.0,
            _ => 5.0,
        });
        assert_eq!(adv, vec![5.0, 2.0, 5.0]);
    }
}
