//! Small custom controls in the Nook style: spring toggle, segmented
//! control, slider, color swatches and round icon button.

use crate::anim;
use crate::geometry::Squircle;
use crate::paint;
use crate::spring::SpringParams;
use crate::theme::{SIZE_LABEL, Theme, mix, with_alpha};
use egui::{Align2, Color32, Response, Sense, Stroke, Ui, pos2, vec2};

/// Size of [`toggle`].
pub const TOGGLE_SIZE: egui::Vec2 = vec2(40.0, 24.0);

/// Material 3 switch: a `primary` track with a large `on_primary` knob when
/// on; a `surface_container_highest` track with an outline and a small
/// knob when off. The knob glides on a spring and grows while pressed.
pub fn toggle(ui: &mut Ui, on: &mut bool, theme: &Theme) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(TOGGLE_SIZE, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let ctx = ui.ctx();
    let t = anim::spring(
        ctx,
        resp.id.with("knob"),
        if *on { 1.0 } else { 0.0 },
        SpringParams::with_overshoot(500.0, 0.06),
    );
    let lit = t.clamp(0.0, 1.0);
    let press = anim::spring(
        ctx,
        resp.id.with("press"),
        if resp.is_pointer_button_down_on() {
            1.0
        } else {
            0.0
        },
        SpringParams::critical(900.0),
    );
    let roles = &theme.roles;
    let painter = ui.painter();
    let radius = rect.height() * 0.5;
    painter.rect_filled(
        rect,
        radius,
        mix(roles.surface_container_highest, roles.primary, lit),
    );
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(2.0, with_alpha(roles.outline, 1.0 - lit)),
        egui::StrokeKind::Inside,
    );
    // Knob: 12 px off, 18 px on, 2 px more while pressed.
    let d = 12.0 + 6.0 * lit + 2.0 * press;
    let travel = rect.width() - rect.height();
    let center = pos2(rect.min.x + radius + travel * t, rect.center().y);
    painter.circle_filled(center, d * 0.5, mix(roles.outline, roles.on_primary, lit));
    resp
}

/// Segmented control: a round track, with the selected option on a
/// `secondary_container` pill that slides between options on a spring.
pub fn segmented(ui: &mut Ui, options: &[&str], selected: &mut usize, theme: &Theme) -> Response {
    let font = Theme::font(SIZE_LABEL);
    let seg_w = options
        .iter()
        .map(|o| {
            ui.painter()
                .layout_no_wrap((*o).to_owned(), font.clone(), theme.text)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
        + 14.0;
    let size = vec2(seg_w * options.len() as f32 + 4.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    let roles = &theme.roles;
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, roles.surface_container_highest);

    if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let i = (((p.x - rect.min.x - 2.0) / seg_w).floor() as usize).min(options.len() - 1);
        if i != *selected {
            *selected = i;
            resp.mark_changed();
        }
    }
    let pos = anim::spring(
        ui.ctx(),
        resp.id.with("seg"),
        *selected as f32,
        SpringParams::critical(520.0),
    );
    let hl = egui::Rect::from_min_size(
        pos2(rect.min.x + 2.0 + pos * seg_w, rect.min.y + 2.0),
        vec2(seg_w, size.y - 4.0),
    );
    painter.rect_filled(hl, 10.0, roles.secondary_container);
    for (i, o) in options.iter().enumerate() {
        let c = pos2(rect.min.x + 2.0 + seg_w * (i as f32 + 0.5), rect.center().y);
        let near = 1.0 - (pos - i as f32).abs().min(1.0);
        let color = mix(roles.on_surface_variant, roles.on_secondary_container, near);
        painter.text(c, Align2::CENTER_CENTER, *o, font.clone(), color);
    }
    resp
}

/// Horizontal slider over `0..=1` in the Material 3 Expressive style: a
/// thick `primary` active track, a small handle bar and a
/// `secondary_container` inactive track. `steps > 0` snaps the value to that
/// many intervals.
pub fn slider(ui: &mut Ui, value: &mut f32, width: f32, steps: u32, theme: &Theme) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click_and_drag());
    let inset = 8.0;
    if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let mut t = ((p.x - rect.min.x - inset) / (rect.width() - 2.0 * inset)).clamp(0.0, 1.0);
        if steps > 0 {
            t = (t * steps as f32).round() / steps as f32;
        }
        if (t - *value).abs() > f32::EPSILON {
            *value = t;
            resp.mark_changed();
        }
    }
    let t = value.clamp(0.0, 1.0);
    let hover = anim::tween(
        ui.ctx(),
        resp.id.with("hover"),
        resp.hovered() || resp.dragged(),
        0.12,
    );
    let painter = ui.painter();
    let track = egui::Rect::from_center_size(rect.center(), vec2(rect.width() - 2.0 * inset, 10.0));
    let x = track.min.x + track.width() * t;
    // The handle bar leaves a gap on each side of it.
    let gap = 5.0;
    let (cap, round) = (5, 2);
    let part = |from: f32, to: f32, color: Color32, left: u8, right: u8| {
        if to - from > 0.5 {
            painter.rect_filled(
                egui::Rect::from_min_max(pos2(from, track.min.y), pos2(to, track.max.y)),
                egui::CornerRadius {
                    nw: left,
                    sw: left,
                    ne: right,
                    se: right,
                },
                color,
            );
        }
    };
    part(track.min.x, x - gap, theme.roles.primary, cap, round);
    part(
        x + gap,
        track.max.x,
        theme.roles.secondary_container,
        round,
        cap,
    );
    let bar = egui::Rect::from_center_size(pos2(x, rect.center().y), vec2(4.0 - hover, 20.0));
    painter.rect_filled(bar, 2.0, theme.roles.primary);
    resp
}

/// A row of round color swatches; `None` is drawn as a "no color" swatch.
/// `selected` is the index of the chosen swatch, if any.
pub fn swatches(
    ui: &mut Ui,
    colors: &[Option<Color32>],
    selected: &mut Option<usize>,
    theme: &Theme,
) -> Response {
    const D: f32 = 16.0;
    const GAP: f32 = 6.0;
    let n = colors.len() as f32;
    let size = vec2(n * D + (n - 1.0).max(0.0) * GAP, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp
        .hover_pos()
        .map(|p| (((p.x - rect.min.x) / (D + GAP)) as usize).min(colors.len() - 1));
    if resp.clicked()
        && let Some(i) = hovered
        && *selected != Some(i)
    {
        *selected = Some(i);
        resp.mark_changed();
    }
    let painter = ui.painter();
    let rim = theme.roles.outline_variant;
    for (i, color) in colors.iter().enumerate() {
        let c = pos2(rect.min.x + D * 0.5 + i as f32 * (D + GAP), rect.center().y);
        let grow = if hovered == Some(i) { 1.0 } else { 0.0 };
        let r = D * 0.5 + grow;
        match color {
            Some(color) => {
                painter.circle_filled(c, r, *color);
            }
            None => {
                // Neutral swatch with a diagonal slash.
                let [r8, g8, b8, _] =
                    mix(theme.glass_top, theme.glass_bottom, 0.5).to_srgba_unmultiplied();
                painter.circle_filled(c, r, Color32::from_rgb(r8, g8, b8));
                let d = vec2(r * 0.6, -r * 0.6);
                painter.line_segment([c - d, c + d], Stroke::new(1.5, theme.text_dim));
            }
        }
        painter.circle_stroke(c, r, Stroke::new(1.0, rim));
        if *selected == Some(i) {
            painter.circle_stroke(c, r + 2.5, Stroke::new(1.5, theme.text));
        }
    }
    resp
}

/// Round icon button with a hover wash and press scale.
pub fn icon_button(
    ui: &mut Ui,
    glyph: &'static str,
    size: f32,
    active: bool,
    theme: &Theme,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let hover = anim::spring(
        ui.ctx(),
        resp.id.with("hover"),
        if resp.hovered() { 1.0 } else { 0.0 },
        SpringParams::snappy(),
    );
    let press = anim::spring(
        ui.ctx(),
        resp.id.with("press"),
        if resp.is_pointer_button_down_on() {
            1.0
        } else {
            0.0
        },
        SpringParams::critical(900.0),
    );
    let painter = ui.painter();
    let r = rect.width() * 0.5 * (1.0 - 0.06 * press);
    // Round: a `primary` circle when on, else a `surface_container_highest`
    // one, with an 8% state layer on hover.
    let (fill, color) = if active {
        (theme.roles.primary, theme.roles.on_primary)
    } else {
        (
            theme.roles.surface_container_highest,
            theme.roles.on_surface,
        )
    };
    painter.circle_filled(rect.center(), r, fill);
    if hover > 0.01 {
        painter.circle_filled(rect.center(), r, with_alpha(color, 0.08 * hover));
    }
    paint::icon(
        painter,
        rect.center(),
        glyph,
        size * 0.55 * (1.0 - 0.06 * press),
        color,
        active,
    );
    resp
}

/// Clickable header of a collapsible card: icon, title, a dim summary and a
/// chevron that turns as `open` (the section's 0..1 reveal) moves. `rect` is
/// the whole header strip; returns its response (clicked = toggle).
pub fn disclosure_header(
    ui: &mut Ui,
    rect: egui::Rect,
    glyph: &'static str,
    title: &str,
    summary: &str,
    open: f32,
    theme: &Theme,
) -> Response {
    let resp = ui.interact(rect, ui.id().with(("disclosure", title)), Sense::click());
    let hover = anim::tween(ui.ctx(), resp.id.with("hover"), resp.hovered(), 0.12);
    let painter = ui.painter();
    if hover > 0.01 {
        let s = Squircle::new(rect.shrink(1.0), crate::theme::TILE_RADIUS - 1.0);
        painter.add(egui::epaint::PathShape::convex_polygon(
            s.outline(),
            with_alpha(theme.roles.on_surface, 0.08 * hover),
            Stroke::NONE,
        ));
    }
    let cy = rect.center().y;
    let x = rect.min.x + 14.0;
    paint::icon(
        painter,
        pos2(x + 10.0, cy),
        glyph,
        crate::theme::SIZE_ICON,
        mix(theme.text_dim, theme.accent, open.clamp(0.0, 1.0)),
        false,
    );
    let title_g = painter.layout_no_wrap(
        title.to_owned(),
        Theme::font(crate::theme::SIZE_BODY),
        theme.text,
    );
    let tx = x + 30.0;
    let title_w = title_g.size().x;
    painter.galley(pos2(tx, cy - title_g.size().y * 0.5), title_g, theme.text);
    // The summary fades out as the section opens: the controls say it then.
    painter.text(
        pos2(tx + title_w + 10.0, cy),
        Align2::LEFT_CENTER,
        summary,
        Theme::font(SIZE_LABEL),
        with_alpha(theme.text_faint, 1.0 - open.clamp(0.0, 1.0)),
    );
    // Chevron: a caret drawn as two strokes, rotating from right to down.
    let c = pos2(rect.max.x - 22.0, cy);
    let angle = (1.0 - open) * -std::f32::consts::FRAC_PI_2;
    let rot = egui::emath::Rot2::from_angle(angle);
    let arm = 4.0;
    let pts = [
        vec2(-arm, -arm * 0.5),
        vec2(0.0, arm * 0.5),
        vec2(arm, -arm * 0.5),
    ]
    .map(|p| c + rot * p);
    painter.add(egui::epaint::PathShape::line(
        pts.to_vec(),
        egui::epaint::PathStroke::new(1.5, mix(theme.text_dim, theme.text, hover)),
    ));
    resp
}

/// A thin row separator.
pub fn separator(ui: &mut Ui, theme: &Theme) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, theme.roles.outline_variant),
    );
}

/// A card (same flat look as a bento tile) behind arbitrary content.
pub fn card(painter: &egui::Painter, rect: egui::Rect, theme: &Theme) {
    painter.add(egui::epaint::PathShape::convex_polygon(
        Squircle::new(rect, crate::theme::TILE_RADIUS).outline(),
        theme.roles.surface_container,
        Stroke::NONE,
    ));
}
