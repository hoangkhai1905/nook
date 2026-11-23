//! Media: now playing from any app that reports to Windows (Spotify,
//! browsers, the Media Player...) through SMTC, with album art, progress and
//! previous / play-pause / next. While something plays the pill shows
//! its title and art (`Badge::Media`).
//!
//! The tile picks a layout from its size in cells ([`layout_for`]).
//!
//! The SMTC work runs on `nook_platform::media`'s worker thread; this module
//! only drains its snapshots in `on_tick` and sends commands back.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, ColorImage, Pos2, Rect, Response, Sense, TextureHandle, TextureOptions, Ui,
    UiBuilder, pos2, vec2,
};
use nook_platform::media::{self, MediaCommand, MediaWatcher, NowPlaying};
use nook_ui::module::{Badge, Module, TileCtx, TileSize};
use nook_ui::theme::{SIZE_BODY, SIZE_LABEL, TILE_PADDING, Theme, with_alpha};
use nook_ui::{icons, paint, widgets};
use std::sync::Arc;
use std::time::Duration;

const CELL: f32 = 80.0;
const CELL_GAP: f32 = 8.0;

/// How the tile arranges itself, by its size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// 1 wide: art with a play button over it.
    Cover,
    /// 2x1: art, title, play.
    Row,
    /// 2 wide, taller: art above the title, progress, transport.
    Stack,
    /// 3+ wide, 1 tall: art, title and progress, transport.
    Strip,
    /// 3+ wide, 2+ tall: the reference card, scaled to the tile.
    Card,
}

fn layout_for(cols: usize, rows: usize) -> Layout {
    match (cols, rows) {
        (1, _) => Layout::Cover,
        (2, 1) => Layout::Row,
        (_, 1) => Layout::Strip,
        (2, _) => Layout::Stack,
        _ => Layout::Card,
    }
}

/// Cells a tile's content rect spans (the rect is the tile minus its padding).
fn cells(size: egui::Vec2) -> (usize, usize) {
    let n = |len: f32| {
        (((len + 2.0 * TILE_PADDING + CELL_GAP) / (CELL + CELL_GAP)).round() as usize).max(1)
    };
    (n(size.x), n(size.y))
}

/// Time left as "-m:ss", rounded up so it only reads "-0:00" at the end.
fn remaining_label(pos: f64, dur: f64) -> String {
    format!("-{}", mmss((dur - pos).max(0.0).ceil()))
}

const BAR_COUNT: usize = 4;
const BAR_W: f32 = 2.5;
const BAR_GAP: f32 = 2.0;
const BAR_MIN: f32 = 3.0;
const BAR_MAX: f32 = 14.0;

/// Little equaliser, right edge at `right`, centred on `cy`. Moves only while
/// `playing`; the caller schedules the repaints.
fn paint_bars(ui: &Ui, right: f32, cy: f32, t: f64, playing: bool, s: f32, color: Color32) {
    let total = (BAR_COUNT as f32 * BAR_W + (BAR_COUNT - 1) as f32 * BAR_GAP) * s;
    for i in 0..BAR_COUNT {
        let k = i as f64;
        let wave = if playing {
            ((t * (5.0 + 1.7 * k) + k * 1.9).sin() * 0.5 + 0.5) as f32
        } else {
            0.3
        };
        let h = (BAR_MIN + (BAR_MAX - BAR_MIN) * wave) * s;
        let x = right - total + i as f32 * (BAR_W + BAR_GAP) * s;
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(x, cy - h / 2.0), vec2(BAR_W * s, h)),
            BAR_W * s / 2.0,
            color,
        );
    }
}

#[derive(Default)]
pub struct Media {
    watcher: Option<MediaWatcher>,
    now: Option<NowPlaying>,
    /// Texture for `now.art`, keyed by the art's allocation.
    art: Option<(usize, TextureHandle)>,
    ctx: Option<egui::Context>,
    /// Set after bringing the app forward; the app then closes the panel.
    close: bool,
}

impl Media {
    fn send(&self, cmd: MediaCommand) {
        if let Some(w) = &self.watcher {
            w.send(cmd);
        }
    }

    fn art_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        let art = self.now.as_ref()?.art.as_ref()?;
        let key = Arc::as_ptr(art) as usize;
        if self.art.as_ref().map(|(k, _)| *k) != Some(key) {
            let image =
                ColorImage::from_rgba_unmultiplied([art.width, art.height], art.rgba.as_slice());
            let tex = ctx.load_texture("nook-media-art", image, TextureOptions::LINEAR);
            self.art = Some((key, tex));
        }
        self.art.as_ref().map(|(_, t)| t.id())
    }

    /// Clicking `area` brings the playing app's window forward.
    fn open_zone(&mut self, ui: &Ui, area: Rect, now: &NowPlaying) {
        let open = ui
            .interact(area, ui.id().with("media-open"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if open.clicked() {
            match media::focus_app(&now.app_id, &[&now.title]) {
                Ok(true) => self.close = true,
                Ok(false) => tracing::debug!("media: no window for {}", now.app_id),
                Err(e) => tracing::debug!("media: focusing {}: {e}", now.app_id),
            }
        }
    }
}

/// Single-line text, elided with "…" to `width`.
fn text_line(ui: &Ui, pos: Pos2, text: &str, width: f32, font: egui::FontId, color: Color32) {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(pos, galley, color);
}

fn button_at(
    ui: &mut Ui,
    center: Pos2,
    size: f32,
    glyph: &'static str,
    active: bool,
    theme: &Theme,
) -> Response {
    let rect = Rect::from_center_size(center, vec2(size, size));
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(glyph));
    widgets::icon_button(&mut child, glyph, size, active, theme)
}

/// "m:ss", or "h:mm:ss" from an hour up (hours keep counting past a day).
fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s / 3600 {
        0 => format!("{}:{:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}", s / 60 % 60, s % 60),
    }
}

/// Rounded image: a squircle-ish clip is overkill at this size, a rounded
/// rect mesh reads the same.
fn rounded_image(ui: &Ui, rect: Rect, tex: egui::TextureId, radius: f32) {
    ui.painter().add(
        egui::epaint::RectShape::filled(rect, radius, Color32::WHITE)
            .with_texture(tex, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))),
    );
}

/// Album art, or a placeholder with a note.
fn draw_art(ui: &Ui, rect: Rect, tex: Option<egui::TextureId>, radius: f32, theme: &Theme) {
    match tex {
        Some(tex) => rounded_image(ui, rect, tex, radius),
        None => {
            ui.painter()
                .rect_filled(rect, radius, theme.roles.surface_container_highest);
            paint::icon(
                ui.painter(),
                rect.center(),
                icons::MUSIC_NOTES,
                (rect.width() * 0.45).clamp(14.0, 40.0),
                theme.text_faint,
                false,
            );
        }
    }
}

/// A thin rounded progress bar centred on `y`.
fn progress_bar(ui: &Ui, x: (f32, f32), y: f32, h: f32, time: (f64, f64), theme: &Theme) {
    let bar = Rect::from_min_max(pos2(x.0, y - h / 2.0), pos2(x.1, y + h / 2.0));
    ui.painter()
        .rect_filled(bar, h / 2.0, with_alpha(theme.text_faint, 0.3));
    let mut fill = bar;
    fill.set_width(bar.width() * (time.0 / time.1.max(1.0)).clamp(0.0, 1.0) as f32);
    ui.painter().rect_filled(fill, h / 2.0, theme.accent_fg());
}

/// Prev / play-pause / next as plain icons centred on `center`, play the
/// larger one. Returns what was clicked.
fn transport(
    ui: &mut Ui,
    center: Pos2,
    s: f32,
    now: &NowPlaying,
    theme: &Theme,
) -> Option<MediaCommand> {
    let step = 38.0 * s;
    let (play, tip) = if now.playing {
        (icons::PAUSE, "Pause")
    } else {
        (icons::PLAY, "Play")
    };
    let buttons = [
        (
            -step,
            icons::SKIP_BACK,
            20.0 * s,
            "Previous",
            now.can_previous,
            MediaCommand::Previous,
        ),
        (
            0.0,
            play,
            30.0 * s,
            tip,
            now.can_toggle,
            MediaCommand::PlayPause,
        ),
        (
            step,
            icons::SKIP_FORWARD,
            20.0 * s,
            "Next",
            now.can_next,
            MediaCommand::Next,
        ),
    ];
    let mut cmd = None;
    for (dx, glyph, px, tip, enabled, command) in buttons {
        if !enabled {
            continue;
        }
        let at = pos2(center.x + dx, center.y);
        let hit = Rect::from_center_size(at, vec2(px + 12.0, px + 12.0));
        let resp = ui
            .interact(hit, ui.id().with(("media-btn", glyph)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(tip);
        let color = if resp.hovered() {
            theme.text
        } else {
            theme.text_dim
        };
        paint::icon(ui.painter(), at, glyph, px, color, true);
        if resp.clicked() {
            cmd = Some(command);
        }
    }
    cmd
}

impl Module for Media {
    fn id(&self) -> &'static str {
        "media"
    }

    fn name(&self) -> &str {
        "Media"
    }

    fn icon(&self) -> &'static str {
        icons::MUSIC_NOTES
    }

    /// Every size draws its own layout, so there is no compact face.
    fn size_hint(&self) -> TileSize {
        TileSize::Small
    }

    fn default_size(&self) -> TileSize {
        TileSize::new(3, 2)
    }

    fn is_active(&self) -> bool {
        self.now.as_ref().is_some_and(|n| n.playing)
    }

    fn collapsed_badge(&self) -> Option<Badge> {
        let n = self.now.as_ref().filter(|n| n.playing)?;
        Some(Badge::Media {
            title: if n.title.is_empty() {
                "Unknown title".to_owned()
            } else {
                n.title.clone()
            },
            art: self.art.as_ref().map(|(_, t)| t.id()),
        })
    }

    fn attach(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        let ctx = ctx.clone();
        match MediaWatcher::spawn(move || ctx.request_repaint()) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => tracing::warn!("media: {e}"),
        }
    }

    fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close)
    }

    fn on_tick(&mut self) {
        if let Some(snapshot) = self.watcher.as_ref().and_then(MediaWatcher::poll) {
            self.now = snapshot;
            if let (Some(ctx), Some(n)) = (&self.ctx, &self.now) {
                nook_ui::theme::ensure_cjk(ctx, &n.title);
                nook_ui::theme::ensure_cjk(ctx, &n.artist);
            }
            if self.now.as_ref().and_then(|n| n.art.as_ref()).is_none() {
                self.art = None;
            } else if let Some(ctx) = self.ctx.clone() {
                // The pill reads the texture without a context.
                self.art_texture(&ctx);
            }
        }
    }

    fn next_repaint(&self, tile_visible: bool) -> Option<Duration> {
        // The progress bar and time advance once a second while visible.
        let n = self.now.as_ref()?;
        (tile_visible && n.playing && n.timeline.is_some()).then(|| Duration::from_secs(1))
    }

    fn ui_tile(&mut self, ui: &mut Ui, ctx: &mut TileCtx<'_>) {
        let theme = ctx.theme;
        let rect = ui.max_rect();
        let (cols, rows) = cells(rect.size());
        let layout = layout_for(cols, rows);

        let Some(now) = self.now.clone() else {
            let c = rect.center();
            let lone = layout == Layout::Cover;
            paint::icon(
                ui.painter(),
                c - vec2(0.0, if lone { 0.0 } else { 8.0 }),
                icons::MUSIC_NOTES,
                24.0,
                theme.text_faint,
                false,
            );
            if !lone {
                ui.painter().text(
                    c + vec2(0.0, 18.0),
                    Align2::CENTER_CENTER,
                    "Nothing playing",
                    Theme::font(SIZE_LABEL),
                    theme.text_dim,
                );
            }
            return;
        };
        let tex = self.art_texture(ui.ctx());
        let title = if now.title.is_empty() {
            "Unknown title"
        } else {
            &now.title
        };
        let pos = now.position();
        let (play_glyph, play_tip) = if now.playing {
            (icons::PAUSE, "Pause")
        } else {
            (icons::PLAY, "Play")
        };
        let semibold = Theme::font_semibold(SIZE_LABEL + 1.0);
        let small = Theme::font(SIZE_LABEL);
        let mut cmd = None;

        // Each layout first registers the zone that brings the app's window
        // forward (art, title, artist), then draws; the transport comes after,
        // so it wins its clicks.
        match layout {
            Layout::Cover => {
                let side = rect.width().min(rect.height());
                let art = Rect::from_center_size(rect.center(), vec2(side, side));
                self.open_zone(ui, rect, &now);
                draw_art(ui, art, tex, 12.0, theme);
                if now.can_toggle
                    && button_at(ui, art.center(), 30.0, play_glyph, true, theme)
                        .on_hover_text(play_tip)
                        .clicked()
                {
                    cmd = Some(MediaCommand::PlayPause);
                }
            }
            Layout::Row => {
                let art = Rect::from_min_size(rect.min, vec2(rect.height(), rect.height()));
                self.open_zone(ui, rect, &now);
                draw_art(ui, art, tex, 12.0, theme);
                let btn = 28.0;
                let x = art.max.x + 8.0;
                let w = (rect.max.x - btn - 6.0 - x).max(0.0);
                let cy = rect.center().y;
                text_line(ui, pos2(x, cy - 15.0), title, w, semibold, theme.text);
                text_line(ui, pos2(x, cy + 1.0), &now.artist, w, small, theme.text_dim);
                if now.can_toggle
                    && button_at(
                        ui,
                        pos2(rect.max.x - btn / 2.0, cy),
                        btn,
                        play_glyph,
                        true,
                        theme,
                    )
                    .on_hover_text(play_tip)
                    .clicked()
                {
                    cmd = Some(MediaCommand::PlayPause);
                }
            }
            Layout::Stack => {
                let controls_y = rect.max.y - 14.0;
                let bar_y = controls_y - 22.0;
                let side = (bar_y - 44.0 - rect.min.y).clamp(32.0, rect.width());
                let art = Rect::from_min_size(rect.min, vec2(side, side));
                let text_y = art.max.y + 6.0;
                let zone = Rect::from_min_max(rect.min, pos2(rect.max.x, text_y + 30.0));
                self.open_zone(ui, zone, &now);
                draw_art(ui, art, tex, 12.0, theme);
                let w = rect.width();
                text_line(ui, pos2(rect.min.x, text_y), title, w, semibold, theme.text);
                text_line(
                    ui,
                    pos2(rect.min.x, text_y + 15.0),
                    &now.artist,
                    w,
                    small,
                    theme.text_dim,
                );
                if let Some(time) = pos {
                    progress_bar(ui, (rect.min.x, rect.max.x), bar_y, 3.0, time, theme);
                }
                cmd = transport(ui, pos2(rect.center().x, controls_y), 1.0, &now, theme);
            }
            Layout::Strip => {
                let art = Rect::from_min_size(rect.min, vec2(rect.height(), rect.height()));
                let controls_w = 116.0;
                let zone = Rect::from_min_max(rect.min, pos2(rect.max.x - controls_w, rect.max.y));
                self.open_zone(ui, zone, &now);
                draw_art(ui, art, tex, 12.0, theme);
                let x = art.max.x + 10.0;
                let w = (rect.max.x - controls_w - x).max(0.0);
                text_line(
                    ui,
                    pos2(x, rect.min.y + 6.0),
                    title,
                    w,
                    semibold,
                    theme.text,
                );
                text_line(
                    ui,
                    pos2(x, rect.min.y + 22.0),
                    &now.artist,
                    w,
                    small,
                    theme.text_dim,
                );
                if let Some(time) = pos {
                    progress_bar(ui, (x, x + w), rect.max.y - 10.0, 3.0, time, theme);
                }
                let c = pos2(rect.max.x - controls_w / 2.0, rect.center().y);
                cmd = transport(ui, c, 1.0, &now, theme);
            }
            Layout::Card => {
                // Designed at 3x2 (240 x 152 inside the padding); bigger
                // tiles scale it.
                let s = (rect.width() / 240.0).min(rect.height() / 152.0).max(0.8);
                let art = Rect::from_min_size(rect.min, vec2(48.0 * s, 48.0 * s));
                let zone = Rect::from_min_max(rect.min, pos2(rect.max.x, art.max.y));
                self.open_zone(ui, zone, &now);
                draw_art(ui, art, tex, 10.0 * s, theme);
                let cy = art.center().y;
                paint_bars(
                    ui,
                    rect.max.x,
                    cy,
                    ctx.now,
                    now.playing,
                    s,
                    theme.roles.primary,
                );
                let bars_w = (BAR_COUNT as f32 * BAR_W + (BAR_COUNT - 1) as f32 * BAR_GAP) * s;
                let x = art.max.x + 10.0 * s;
                let w = (rect.max.x - bars_w - 10.0 * s - x).max(0.0);
                text_line(
                    ui,
                    pos2(x, cy - 16.0 * s),
                    title,
                    w,
                    Theme::font_semibold(SIZE_BODY * s),
                    theme.text,
                );
                text_line(
                    ui,
                    pos2(x, cy + 2.0 * s),
                    &now.artist,
                    w,
                    Theme::font(SIZE_LABEL * s),
                    theme.text_dim,
                );
                let bar_y = art.max.y + 14.0 * s;
                let time_y = bar_y + 7.0 * s;
                if let Some((p, d)) = pos {
                    progress_bar(ui, (rect.min.x, rect.max.x), bar_y, 4.0 * s, (p, d), theme);
                    let font = Theme::font(SIZE_LABEL * s);
                    paint::digit_slots(
                        ui.painter(),
                        &mmss(p),
                        font.clone(),
                        theme.text_dim,
                        pos2(rect.min.x, time_y),
                        Align2::LEFT_TOP,
                    );
                    paint::digit_slots(
                        ui.painter(),
                        &remaining_label(p, d),
                        font,
                        theme.text_dim,
                        pos2(rect.max.x, time_y),
                        Align2::RIGHT_TOP,
                    );
                }
                let below = time_y + SIZE_LABEL * s * 1.3;
                let c = pos2(rect.center().x, (below + rect.max.y) / 2.0);
                cmd = transport(ui, c, s * 1.15, &now, theme);
                if now.playing {
                    // The equaliser moves while playing, and only then.
                    ui.ctx().request_repaint_after(Duration::from_millis(66));
                }
            }
        }

        if let Some(cmd) = cmd {
            self.send(cmd);
            if cmd == MediaCommand::PlayPause {
                // Flip right away; the next snapshot confirms it.
                if let Some(n) = &mut self.now {
                    if let Some((pos, dur)) = n.position() {
                        n.timeline = Some((pos, dur));
                        n.captured = std::time::Instant::now();
                    }
                    n.playing = !n.playing;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layout of a `c` x `r` tile, from the content rect the shell gives it.
    fn layout_of(c: usize, r: usize) -> Layout {
        let side = |n: usize| n as f32 * CELL + (n - 1) as f32 * CELL_GAP - 2.0 * TILE_PADDING;
        let (cols, rows) = cells(vec2(side(c), side(r)));
        assert_eq!((cols, rows), (c, r));
        layout_for(cols, rows)
    }

    #[test]
    fn layout_follows_the_size_in_cells() {
        assert_eq!(layout_of(1, 1), Layout::Cover);
        assert_eq!(layout_of(2, 1), Layout::Row);
        assert_eq!(layout_of(2, 2), Layout::Stack);
        assert_eq!(layout_of(3, 2), Layout::Card);
        assert_eq!(layout_of(4, 1), Layout::Strip);
        assert_eq!(layout_of(6, 4), Layout::Card);
    }

    #[test]
    fn remaining_time_counts_down_as_minutes_seconds() {
        assert_eq!(remaining_label(10.0, 141.0), "-2:11");
        assert_eq!(remaining_label(10.4, 141.0), "-2:11");
        assert_eq!(remaining_label(141.0, 141.0), "-0:00");
        assert_eq!(remaining_label(200.0, 141.0), "-0:00");
    }

    #[test]
    fn long_media_shows_hours() {
        assert_eq!(mmss(59.0 * 60.0 + 59.0), "59:59");
        assert_eq!(mmss(3600.0), "1:00:00");
        assert_eq!(mmss(4.0 * 3600.0 + 3.0 * 60.0 + 54.0), "4:03:54");
        // Two days of audio.
        assert_eq!(mmss(48.0 * 3600.0), "48:00:00");
    }
}
