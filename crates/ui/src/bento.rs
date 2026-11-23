//! The expanded panel's bento grid: a tab bar on top, and under it the
//! active tab's tiles on a 6x4 grid, drawn as glass tiles with hover lift,
//! press scale and accent state, each hosting its module's `ui_tile`.
//!
//! Three extras live here. **Tabs**: Home, Tools, Capture and System, with an
//! indicator that springs between them; tiles slide sideways when the tab
//! changes. **Edit mode** (the pencil in the bar, a "Done" button while it
//! lasts) turns the grid into a live preview of its own layout: drag a tile
//! and the others spring out of its way as the grid repacks, drop it on a tab
//! name to move it there, drag the handle in its corner to resize it, and
//! the tick under its icon shows or hides it. **Expanding**: a tile made
//! smaller than its content needs draws a compact face, and a click opens
//! it: the grid repacks with it at full size, so it grows in place while its
//! neighbours glide out of the way. A click anywhere else (or Esc) folds it
//! back.

use crate::anim;
use crate::geometry::{GridSpec, Squircle, TileSize, pack_pages, rows_used};
use crate::icons;
use crate::module::{Badge, EdgeLine, GridTile, Module, ModuleStatus, Tab, TileCtx};
use crate::paint;
use crate::shell::ContentCtx;
use crate::spring::SpringParams;
use crate::theme::{self, SIZE_ICON, SIZE_LABEL, TILE_PADDING, TILE_RADIUS, Theme, with_alpha};
use egui::emath::TSTransform;
use egui::{Align2, Area, Color32, Id, Order, Pos2, Rect, Sense, Ui, UiBuilder, Vec2, pos2, vec2};

pub const GRID: GridSpec = GridSpec {
    columns: 6,
    rows: 4,
    cell: 80.0,
    gap: 8.0,
};

/// Inset of the panel's content (top/left/right) on the other pages.
pub const PANEL_PADDING: f32 = 16.0;

/// Above the tab bar, the bar itself, and the gap between it and the grid.
const BAR_TOP: f32 = 8.0;
const BAR_H: f32 = 32.0;
const BAR_GAP: f32 = 4.0;
/// Under the grid: the caption (and Update chip) line.
const GUTTER: f32 = 24.0;

/// Panel height for a grid showing `rows` rows. Two rows is 236, the panel's
/// minimum height.
pub fn panel_height(rows: usize) -> f32 {
    let rows = rows.clamp(1, GRID.rows) as f32;
    BAR_TOP + BAR_H + BAR_GAP + rows * GRID.cell + (rows - 1.0) * GRID.gap + GUTTER
}

/// Panel height for a tab holding tiles of `sizes`: it fits the rows they
/// use. Edit mode keeps one spare row so a tile can be dragged bigger.
pub fn tab_height(sizes: &[TileSize], editing: bool) -> f32 {
    let used = rows_used(&pack_pages(&GRID, sizes), sizes);
    panel_height(if editing { used + 1 } else { used })
}

/// Cells a drag of `d` points from a tile's corner spans, at least one.
fn snap_cells(d: f32) -> usize {
    ((d + GRID.gap) / (GRID.cell + GRID.gap)).round().max(1.0) as usize
}

/// A layout change made in edit mode.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// New order of every tile shown in edit mode.
    Reorder(Vec<&'static str>),
    /// Show a hidden tile or hide a shown one.
    ToggleShown(&'static str),
    Resize(&'static str, TileSize),
    /// Move a tile to another tab (it goes last there).
    MoveTab(&'static str, Tab),
}

#[derive(Default)]
pub struct BentoOutput {
    /// Id of the tile that was clicked this frame, if any.
    pub clicked: Option<&'static str>,
    /// Layout changes made in edit mode this frame.
    pub edits: Vec<Edit>,
    /// The pencil (or Done) was clicked, or Esc left edit mode.
    pub toggle_edit: bool,
    /// The gear was clicked.
    pub open_settings: bool,
    /// The tab changed this frame.
    pub tab_changed: bool,
    /// Rows the tab shown needs as laid out now (an open tile or a resize
    /// preview included, edit mode's spare row too), for the panel height.
    pub rows: usize,
}

/// Grid origin within a panel rect.
pub fn grid_rect(panel: Rect) -> Rect {
    let size = GRID.size();
    let top = panel.min.y + BAR_TOP + BAR_H + BAR_GAP;
    Rect::from_min_size(pos2(panel.center().x - size.x * 0.5, top), size)
}

/// Height of the caption line under the grid.
pub fn gutter_y(panel: Rect) -> f32 {
    panel.max.y - GUTTER * 0.5
}

/// Page of the tab shown, kept in egui memory.
#[derive(Clone, Copy, Default)]
struct PageState {
    page: usize,
    /// Wheel travel (in lines) not yet turned into a page flip.
    wheel: f32,
    /// egui time of the last wheel event and of the last flip.
    last_wheel: f64,
    last_flip: f64,
}

/// A tile held in edit mode, kept in egui memory.
#[derive(Clone)]
struct Drag {
    id: &'static str,
    /// Pointer offset from the tile's corner when it was picked up.
    grab: Vec2,
    /// The order being previewed, held tile included.
    order: Vec<&'static str>,
    /// When the held tile last changed places (debounces swaps).
    last_move: f64,
}

/// A tile being resized by its corner handle, kept in egui memory.
#[derive(Clone, Copy)]
struct Resize {
    id: &'static str,
    /// Where the tile's corner was when the drag began.
    origin: Pos2,
    /// The size being previewed.
    size: TileSize,
}

/// The compact tile opened to full size, kept in egui memory.
#[derive(Clone, Copy, Default)]
struct Expanded {
    id: Option<&'static str>,
    /// egui time it opened, so the opening click does not close it.
    since: f64,
    /// Show the page it lands on (it may move when it grows).
    reveal: bool,
}

/// Wheel travel, in lines, that flips a page.
const FLIP_AT: f32 = 1.0;
/// Pause after a flip so one fling does not run through every page.
const FLIP_COOLDOWN: f64 = 0.35;

fn wheel_lines(ctx: &egui::Context) -> f32 {
    ctx.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                egui::Event::MouseWheel { unit, delta, .. } => match unit {
                    egui::MouseWheelUnit::Line => delta.y,
                    egui::MouseWheelUnit::Point => delta.y / 40.0,
                    egui::MouseWheelUnit::Page => delta.y * 3.0,
                },
                _ => 0.0,
            })
            .sum()
    })
}

/// A tile smaller than its content needs gets the compact face.
fn is_compact(tile: &GridTile<'_>) -> bool {
    tile.module.status() == ModuleStatus::Ready
        && tile.size.is_smaller_than(tile.module.size_hint())
}

/// Tile background: a flat fill (primary container when active), with an
/// 8% state layer of the content color on hover.
fn tile_glass(painter: &egui::Painter, rect: Rect, theme: &Theme, active: bool, lift: f32) {
    let outline = Squircle::new(rect, TILE_RADIUS).outline();
    let (fill, on) = if active {
        (
            theme.roles.primary_container,
            theme.roles.on_primary_container,
        )
    } else {
        (theme.roles.surface_container, theme.roles.on_surface)
    };
    painter.add(egui::Shape::convex_polygon(
        outline.clone(),
        fill,
        egui::Stroke::NONE,
    ));
    if lift > 0.01 {
        painter.add(egui::Shape::convex_polygon(
            outline,
            with_alpha(on, 0.08 * lift.min(1.0)),
            egui::Stroke::NONE,
        ));
    }
}

/// Inset of the timer's edge line from the tile's edge (it is 3 px wide).
const EDGE_INSET: f32 = 4.0;

/// Draws a module's countdown line around the tile: a track, and the
/// primary line over it, which glides between its once-a-second steps.
fn edge_line(
    ctx: &egui::Context,
    painter: &egui::Painter,
    rect: Rect,
    id: Id,
    theme: &Theme,
    line: EdgeLine,
    now: f64,
) {
    let secs = if anim::motion(ctx).reduced { 0.0 } else { 0.3 };
    let progress = ctx.animate_value_with_time(id.with("edge"), line.progress, secs);
    let alpha = if line.pulse {
        0.4 + 0.6 * ((now * 3.0).sin() * 0.5 + 0.5) as f32
    } else if line.dim {
        0.4
    } else {
        1.0
    };
    let rect = rect.shrink(EDGE_INSET);
    let radius = TILE_RADIUS - EDGE_INSET;
    paint::edge_line(painter, rect, radius, 3.0, 1.0, theme.roles.outline_variant);
    paint::edge_line(
        painter,
        rect,
        radius,
        3.0,
        progress,
        with_alpha(theme.roles.primary, alpha),
    );
}

fn tile_inner(rect: Rect) -> Rect {
    rect.shrink(TILE_PADDING)
}

fn tab_icon(tab: Tab) -> &'static str {
    match tab {
        Tab::Home => icons::HOUSE,
        Tab::Tools => icons::WRENCH,
        Tab::Capture => icons::CAMERA,
        Tab::System => icons::CPU,
    }
}

/// The tab buttons and the indicator under the active one. Returns the tab
/// clicked and where each button sits (to drop a held tile on).
fn tab_bar(
    ui: &mut Ui,
    cctx: &ContentCtx,
    theme: &Theme,
    grid: Rect,
    active: Tab,
    held: Option<Pos2>,
) -> (Option<Tab>, Vec<(Tab, Rect)>) {
    let ctx = ui.ctx().clone();
    let roles = &theme.roles;
    let y = cctx.panel.min.y + BAR_TOP;
    let font = Theme::font_semibold(SIZE_LABEL + 2.0);
    let mut x = grid.min.x;
    let mut clicked = None;
    let mut rects = Vec::new();
    let mut under = None;
    for tab in Tab::ALL {
        let w = ui
            .painter()
            .layout_no_wrap(tab.name().to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x;
        let rect = Rect::from_min_size(pos2(x, y), vec2(12.0 + 16.0 + 6.0 + w + 12.0, BAR_H - 4.0));
        x = rect.max.x + 4.0;
        let resp = ui.interact(rect, Id::new(("nook-tab", tab)), Sense::click());
        let on = tab == active;
        let drop = held.is_some_and(|p| rect.contains(p));
        let hover = anim::tween(&ctx, resp.id.with("hover"), resp.hovered() || drop, 0.12);
        let painter = ui.painter();
        if hover > 0.0 {
            let fill = if drop {
                roles.primary_container
            } else {
                roles.surface_container_high
            };
            painter.rect_filled(rect, 14.0, with_alpha(fill, hover * cctx.alpha));
        }
        let color = if on {
            roles.primary
        } else {
            theme::mix(theme.text_dim, theme.text, hover)
        };
        let color = with_alpha(color, cctx.alpha);
        let cy = rect.center().y;
        paint::icon(
            painter,
            pos2(rect.min.x + 20.0, cy),
            tab_icon(tab),
            16.0,
            color,
            on,
        );
        painter.text(
            pos2(rect.min.x + 34.0, cy),
            Align2::LEFT_CENTER,
            tab.name(),
            font.clone(),
            color,
        );
        if on {
            under = Some(rect);
        }
        if resp.clicked() {
            clicked = Some(tab);
        }
        rects.push((tab, rect));
    }
    if let Some(r) = under {
        // Both edges spring on their own, so the indicator stretches as it
        // travels.
        let id = Id::new("nook-tab-indicator");
        let x0 = anim::spring(&ctx, id.with(0), r.min.x + 12.0, SpringParams::snappy());
        let x1 = anim::spring(&ctx, id.with(1), r.max.x - 12.0, SpringParams::snappy());
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(x0, y + BAR_H - 3.0), pos2(x1.max(x0), y + BAR_H)),
            1.5,
            with_alpha(roles.primary, cctx.alpha),
        );
    }
    (clicked, rects)
}

/// A round icon button in the bar; true when clicked.
fn bar_icon(
    ui: &mut Ui,
    cctx: &ContentCtx,
    theme: &Theme,
    c: Pos2,
    glyph: &str,
    tip: &str,
) -> bool {
    let id = Id::new(("nook-bar-icon", glyph));
    let resp = ui.interact(
        Rect::from_center_size(c, vec2(28.0, 28.0)),
        id,
        Sense::click(),
    );
    let hover = anim::tween(ui.ctx(), id.with("hover"), resp.hovered(), 0.12);
    let color = theme::mix(theme.text_dim, theme.text, hover);
    paint::icon(
        ui.painter(),
        c,
        glyph,
        16.0,
        with_alpha(color, cctx.alpha),
        false,
    );
    resp.on_hover_text(tip).clicked()
}

pub fn show(
    ui: &mut Ui,
    cctx: &ContentCtx,
    theme: &Theme,
    tiles: &mut [GridTile<'_>],
    editing: bool,
    tab: &mut Tab,
) -> BentoOutput {
    let ctx = ui.ctx().clone();
    let mut out = BentoOutput::default();
    let grid = grid_rect(cctx.panel);
    let now = ctx.input(|i| i.time);
    let mut caption: Option<(String, bool)> = None;
    let ids: Vec<&'static str> = tiles.iter().map(|t| t.module.id()).collect();

    let drag_id = Id::new("nook-bento-drag");
    let expand_id = Id::new("nook-bento-expand");
    let resize_id = Id::new("nook-bento-resize");
    let mut drag: Option<Drag> = if editing {
        ctx.data(|d| d.get_temp(drag_id))
    } else {
        None
    };
    let mut resize: Option<Resize> = if editing {
        ctx.data(|d| d.get_temp(resize_id))
    } else {
        None
    };
    let mut expanded: Expanded = if editing {
        Expanded::default()
    } else {
        ctx.data(|d| d.get_temp(expand_id)).unwrap_or_default()
    };
    if cctx.closing {
        expanded.id = None;
    }
    // The open tile is packed at its full size, the one being resized at
    // the size previewed.
    let open_id = expanded.id;
    let preview = resize.map(|r| (r.id, r.size));
    let size_of = move |t: &GridTile<'_>| {
        let id = t.module.id();
        if open_id == Some(id) {
            t.module.size_hint()
        } else if let Some((rid, size)) = preview
            && rid == id
        {
            size
        } else {
            t.size
        }
    };

    // Display order: the previewed order while a tile is held.
    let order: Vec<usize> = match &drag {
        Some(d) => {
            let mut o: Vec<usize> = d
                .order
                .iter()
                .filter_map(|id| ids.iter().position(|x| x == id))
                .collect();
            let missing: Vec<usize> = (0..ids.len()).filter(|i| !o.contains(i)).collect();
            o.extend(missing);
            o
        }
        None => (0..ids.len()).collect(),
    };
    // Every tab's tiles (indices into `tiles`, in display order) and pages.
    let by_tab: Vec<Vec<usize>> = Tab::ALL
        .iter()
        .map(|&t| {
            order
                .iter()
                .copied()
                .filter(|&i| tiles[i].tab == t)
                .collect()
        })
        .collect();
    let all_pages: Vec<_> = by_tab
        .iter()
        .map(|v| {
            let sizes: Vec<TileSize> = v.iter().map(|&i| size_of(&tiles[i])).collect();
            pack_pages(&GRID, &sizes)
        })
        .collect();

    // Pointer in content space (the content scales while the shell opens).
    let pointer = ctx
        .input(|i| i.pointer.interact_pos())
        .map(|p| cctx.transform.inverse() * p);

    // Paging: the wheel over the panel steps through the tab's pages, then
    // on to the next tab.
    let state_id = Id::new("nook-bento-page");
    let mut st: PageState = ctx.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let pages = &all_pages[tab.index()];
    st.page = st.page.min(pages.len() - 1);
    if expanded.reveal {
        expanded.reveal = false;
        if let Some(page) = pages.iter().position(|pl| {
            pl.iter()
                .any(|p| Some(ids[by_tab[tab.index()][p.index]]) == expanded.id)
        }) {
            st.page = page;
        }
    }
    let over_panel = pointer.is_some_and(|p| cctx.panel.contains(p));
    let wheel_taken = pointer.is_some_and(|p| {
        pages[st.page].iter().any(|pl| {
            let t = &tiles[by_tab[tab.index()][pl.index]];
            GRID.cell_rect(grid.min, pl.col, pl.row, size_of(t))
                .contains(p)
                && !editing
                && (!is_compact(t) || expanded.id == Some(t.module.id()))
                && t.module.captures_wheel()
        })
    });
    let mut goto: Option<(Tab, usize)> = None;
    let lines = wheel_lines(&ctx);
    if now - st.last_wheel > 0.25 {
        st.wheel = 0.0;
    }
    if lines != 0.0 && over_panel && !wheel_taken && cctx.alpha > 0.5 {
        st.wheel += lines;
        st.last_wheel = now;
        if st.wheel.abs() >= FLIP_AT && now - st.last_flip > FLIP_COOLDOWN {
            // Wheel down (negative y) goes on: the next page, else the next tab.
            let ti = tab.index();
            goto = if st.wheel < 0.0 {
                if st.page + 1 < pages.len() {
                    Some((*tab, st.page + 1))
                } else {
                    Tab::ALL.get(ti + 1).map(|&t| (t, 0))
                }
            } else if st.page > 0 {
                Some((*tab, st.page - 1))
            } else {
                ti.checked_sub(1).map(|i| (Tab::ALL[i], 0))
            };
            st.wheel = 0.0;
            st.last_flip = now;
        }
    }

    // The tab bar, with the page count, the edit button and the gear at its
    // right end.
    let held_ptr = drag.as_ref().and(pointer);
    let (clicked_tab, tab_rects) =
        tab_bar(ui, cctx, theme, grid, goto.map_or(*tab, |g| g.0), held_ptr);
    if let Some(t) = clicked_tab {
        goto = Some((t, 0));
    }
    {
        let cy = cctx.panel.min.y + BAR_TOP + (BAR_H - 4.0) * 0.5;
        if bar_icon(
            ui,
            cctx,
            theme,
            pos2(grid.max.x - 14.0, cy),
            icons::GEAR_SIX,
            "Settings",
        ) {
            out.open_settings = true;
        }
        let edit_id = Id::new("nook-bento-edit");
        let pages_right;
        if editing {
            pages_right = grid.max.x - 96.0;
            let rect =
                Align2::RIGHT_CENTER.anchor_size(pos2(grid.max.x - 32.0, cy), vec2(56.0, 26.0));
            let resp = ui.interact(rect, edit_id, Sense::click());
            let hover = anim::tween(&ctx, edit_id.with("hover"), resp.hovered(), 0.12);
            let fill = theme::mix(theme.roles.primary, theme.roles.on_primary, 0.12 * hover);
            ui.painter()
                .rect_filled(rect, 13.0, with_alpha(fill, cctx.alpha));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Done",
                Theme::font_semibold(SIZE_LABEL + 1.0),
                with_alpha(theme.roles.on_primary, cctx.alpha),
            );
            out.toggle_edit |= resp.clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape));
        } else {
            pages_right = grid.max.x - 66.0;
            if bar_icon(
                ui,
                cctx,
                theme,
                pos2(grid.max.x - 46.0, cy),
                icons::PENCIL_SIMPLE,
                "Edit the layout",
            ) {
                out.toggle_edit = true;
            }
        }
        if pages.len() > 1 {
            ui.painter().text(
                pos2(pages_right, cy),
                Align2::RIGHT_CENTER,
                format!("{}/{}", st.page + 1, pages.len()),
                Theme::font(SIZE_LABEL),
                with_alpha(theme.text_dim, cctx.alpha),
            );
        }
    }
    if let Some((t, page)) = goto {
        out.tab_changed = t != *tab;
        *tab = t;
        st.page = page;
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(state_id, st));

    // From here on, the tab shown.
    let ti = tab.index();
    let pages = &all_pages[ti];
    let tab_tiles = &by_tab[ti];
    st.page = st.page.min(pages.len() - 1);
    let shown_sizes: Vec<TileSize> = tab_tiles.iter().map(|&i| size_of(&tiles[i])).collect();
    out.rows = rows_used(pages, &shown_sizes) + usize::from(editing);

    // A held tile takes the place of the tile under the pointer; past the
    // last tile of the last page, it goes to the end.
    if let (Some(d), Some(p)) = (drag.as_mut(), pointer)
        && now - d.last_move > 0.12
        && grid.contains(p)
    {
        let pos_of = |tile: usize| order.iter().position(|&i| i == tile);
        let held_pos = order.iter().position(|&i| ids[i] == d.id);
        let mut target = None;
        for pl in &pages[st.page] {
            let i = tab_tiles[pl.index];
            if ids[i] == d.id {
                continue;
            }
            let r = GRID.cell_rect(grid.min, pl.col, pl.row, tiles[i].size);
            if r.shrink2(r.size() * 0.2).contains(p) {
                target = pos_of(i);
            }
        }
        if target.is_none() && st.page + 1 == pages.len() {
            let right = pages[st.page]
                .iter()
                .filter(|pl| ids[tab_tiles[pl.index]] != d.id)
                .map(|pl| {
                    GRID.cell_rect(grid.min, pl.col, pl.row, tiles[tab_tiles[pl.index]].size)
                        .max
                        .x
                })
                .fold(f32::MIN, f32::max);
            if p.x > right + GRID.gap * 0.5 {
                target = Some(order.len() - 1);
            }
        }
        if let (Some(to), Some(from)) = (target, held_pos)
            && to != from
        {
            let held = d.order.remove(from);
            d.order.insert(to.min(d.order.len()), held);
            d.last_move = now;
        }
    }

    let scroll = anim::spring(
        &ctx,
        state_id.with("scroll"),
        st.page as f32,
        SpringParams::snappy(),
    );
    let tab_scroll = anim::spring(
        &ctx,
        state_id.with("tabs"),
        ti as f32,
        SpringParams::snappy(),
    );
    let step = vec2(
        grid.width() + PANEL_PADDING * 2.0,
        GRID.size().y + PANEL_PADDING * 2.0,
    );
    // Tiles are cut off at the sides, the caption line and the glass.
    let clip = Rect::from_min_max(
        pos2(cctx.panel.min.x + 8.0, grid.min.y - 4.0),
        pos2(cctx.panel.max.x - 8.0, cctx.panel.max.y - GUTTER + 4.0),
    )
    .intersect(cctx.shape);
    let files_hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    // Where the open tile is drawn, to tell clicks outside it.
    let mut open_rect: Option<Rect> = None;
    let mut new_drag: Option<Drag> = None;

    for (tab_idx, all) in all_pages.iter().enumerate() {
        let dx = tab_idx as f32 - tab_scroll;
        if dx.abs() >= 1.0 {
            continue;
        }
        for (page_idx, placements) in all.iter().enumerate() {
            let distance = if tab_idx == ti {
                page_idx as f32 - scroll
            } else {
                page_idx as f32
            };
            if distance.abs() >= 1.0 {
                continue;
            }
            let shift = vec2(dx * step.x, distance * step.y);
            for (slot, p) in placements.iter().enumerate() {
                let tile_idx = by_tab[tab_idx][p.index];
                let size = size_of(&tiles[tile_idx]);
                let tile = &mut tiles[tile_idx];
                let id = Id::new(("nook-tile", tile.module.id()));
                let held = drag.as_ref().is_some_and(|d| d.id == tile.module.id());
                let target = GRID.cell_rect(grid.min, p.col, p.row, size);
                let is_open = expanded.id == Some(tile.module.id());
                // Tiles glide to new places and sizes when the layout changes.
                let (rect, page_alpha) = match (held, pointer, &drag) {
                    (true, Some(ptr), Some(d)) => {
                        let r = Rect::from_min_size(ptr - d.grab, target.size());
                        anim::spring_vec2(&ctx, id.with("pos"), r.min - grid.min - shift, true);
                        anim::spring_vec2(&ctx, id.with("size"), r.size(), true);
                        (r, 1.0)
                    }
                    _ => {
                        let pos =
                            anim::spring_vec2(&ctx, id.with("pos"), target.min - grid.min, false);
                        let sz = anim::spring_vec2(&ctx, id.with("size"), target.size(), false);
                        (
                            Rect::from_min_size(grid.min + pos, sz).translate(shift),
                            (1.0 - distance.abs()) * (1.0 - dx.abs()),
                        )
                    }
                };
                if is_open {
                    open_rect = Some(rect);
                }
                let ready = tile.module.status() == ModuleStatus::Ready;
                // An opening tile keeps its compact face until it has nearly
                // reached full size, then shows its full layout.
                let grown = rect.width() >= target.width() * 0.92
                    && rect.height() >= target.height() * 0.92;
                let compact = is_compact(tile) && !(is_open && grown);
                let stagger = cctx.stagger(slot);
                let mut alpha = cctx.alpha * stagger * page_alpha;
                if tile.hidden {
                    alpha *= 0.5;
                }
                if alpha <= 0.002 {
                    continue;
                }
                let interactive = (tab_idx == ti && page_idx == st.page) || held;

                let area = Area::new(id)
                    .order(if held {
                        Order::Foreground
                    } else {
                        Order::Middle
                    })
                    .fixed_pos(rect.min)
                    .movable(false)
                    .constrain(false)
                    .sense(Sense::hover())
                    .show(&ctx, |ui| {
                        if !held {
                            ui.set_clip_rect(clip);
                        }
                        ui.multiply_opacity(alpha);
                        let sense = match (interactive, editing) {
                            (false, _) => Sense::hover(),
                            (true, true) => Sense::click_and_drag(),
                            (true, false) => Sense::click(),
                        };
                        // The area must cover the tile, or egui never finds its
                        // layer under the pointer and nothing in it hovers.
                        ui.expand_to_include_rect(rect);
                        let resp = ui.interact(rect, id.with("tile"), sense);
                        let hovered = interactive && ui.rect_contains_pointer(rect);
                        let pressed = hovered && ctx.input(|i| i.pointer.primary_down());
                        if hovered && drag.is_none() {
                            caption = Some((tile.module.name().to_owned(), ready || editing));
                        }
                        let lift = anim::spring(
                            &ctx,
                            id.with("lift"),
                            if (hovered && (ready || editing)) || held {
                                1.0
                            } else {
                                0.0
                            },
                            SpringParams::snappy(),
                        );
                        let press = anim::spring(
                            &ctx,
                            id.with("press"),
                            if pressed && ready && !editing {
                                1.0
                            } else {
                                0.0
                            },
                            SpringParams::critical(900.0),
                        );

                        let painter = ui.painter().clone();
                        if held {
                            painter.rect_filled(
                                rect.translate(vec2(0.0, 5.0)).expand(2.0),
                                TILE_RADIUS + 2.0,
                                Color32::from_black_alpha(70),
                            );
                        }
                        let active = ready && !editing && tile.module.is_active();
                        tile_glass(&painter, rect, theme, active, lift);
                        if ready
                            && !editing
                            && let Some(line) = tile.module.edge_line()
                        {
                            edge_line(&ctx, &painter, rect, id, theme, line, now);
                        }
                        // Content on an active tile reads on-primary-container.
                        let on_tile;
                        let theme = if active {
                            on_tile = theme.on_active_tile();
                            &on_tile
                        } else {
                            theme
                        };
                        if is_open {
                            painter.rect_stroke(
                                rect.shrink(0.5),
                                TILE_RADIUS,
                                egui::Stroke::new(1.0, with_alpha(theme.accent, 0.6)),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if editing {
                            painter.rect_stroke(
                                rect.shrink(0.5),
                                TILE_RADIUS,
                                egui::Stroke::new(1.0, with_alpha(theme.accent, 0.45)),
                                egui::StrokeKind::Inside,
                            );
                        }

                        let inner = tile_inner(rect);
                        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).id_salt(id));
                        if !held {
                            child.set_clip_rect(rect.intersect(clip));
                        }
                        if !interactive {
                            child.disable();
                        }
                        if editing {
                            if let Some(e) =
                                edit_face(&mut child, rect, tile, theme, pointer, &mut resize)
                            {
                                out.edits.push(e);
                            }
                        } else if compact {
                            compact_tile(&mut child, inner, tile.module.as_ref(), theme, hovered);
                        } else if ready {
                            let mut tctx = TileCtx {
                                theme,
                                now,
                                hovered,
                                files_hovering,
                            };
                            tile.module.ui_tile(&mut child, &mut tctx);
                        } else {
                            coming_soon(&mut child, inner, tile.module.icon(), theme, hovered, id);
                        }

                        if editing {
                            if resp.drag_started()
                                && resize.is_none()
                                && let Some(ptr) = pointer
                            {
                                new_drag = Some(Drag {
                                    id: tile.module.id(),
                                    grab: ptr - rect.min,
                                    order: order.iter().map(|&i| ids[i]).collect(),
                                    last_move: now,
                                });
                            }
                        } else if resp.clicked() && ready {
                            if compact && !is_open {
                                expanded = Expanded {
                                    id: Some(tile.module.id()),
                                    since: now,
                                    reveal: true,
                                };
                            } else {
                                out.clicked = Some(tile.module.id());
                            }
                        }
                        (lift, press)
                    });

                let (lift, press) = area.inner;
                let scale = 1.0 - 0.03 * press + 0.05 * if held { lift } else { 0.0 };
                let c = rect.center().to_vec2();
                let slide = 8.0 * (1.0 - stagger) - 2.0 * lift;
                let tile_t = TSTransform::new(c * (1.0 - scale) + vec2(0.0, slide), scale);
                ctx.set_transform_layer(area.response.layer_id, cctx.transform * tile_t);
            }
        }
    }

    // Picking up, and putting down: the new order is the one previewed, or
    // the tab under the pointer when it was dropped on one.
    if let Some(d) = new_drag {
        drag = Some(d);
    }
    if let Some(d) = &drag
        && !ctx.input(|i| i.pointer.any_down())
    {
        let on_tab = pointer.and_then(|p| tab_rects.iter().find(|(_, r)| r.contains(p)));
        match on_tab {
            Some(&(t, _)) if t != *tab => out.edits.push(Edit::MoveTab(d.id, t)),
            _ if d.order != ids => out.edits.push(Edit::Reorder(d.order.clone())),
            _ => {}
        }
        drag = None;
    }
    // Letting go of a handle commits the size previewed, wherever the
    // pointer is (the tile may have left the screen meanwhile).
    if let Some(r) = resize
        && !ctx.input(|i| i.pointer.any_down())
    {
        if tiles
            .iter()
            .any(|t| t.module.id() == r.id && t.size != r.size)
        {
            out.edits.push(Edit::Resize(r.id, r.size));
        }
        resize = None;
    }
    if drag.is_some() || resize.is_some() {
        ctx.request_repaint();
    }
    ctx.data_mut(|data| {
        match drag {
            Some(d) => {
                data.insert_temp(drag_id, d);
            }
            None => {
                data.remove::<Drag>(drag_id);
            }
        }
        match resize {
            Some(r) => {
                data.insert_temp(resize_id, r);
            }
            None => {
                data.remove::<Resize>(resize_id);
            }
        }
    });

    // A click anywhere but the open tile (another tile included, which
    // still gets its click) folds it back, and so does Esc.
    if let Some(id) = expanded.id {
        let clicked_outside = ctx.input(|i| i.pointer.primary_clicked())
            && pointer.is_some_and(|p| open_rect.is_none_or(|r| !r.contains(p)));
        if (now - expanded.since > 0.1 && clicked_outside)
            || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            expanded.id = None;
        } else if caption.is_none()
            && let Some(t) = tiles.iter().find(|t| t.module.id() == id)
        {
            caption = Some((
                format!("{}  ·  Click outside to fold", t.module.name()),
                true,
            ));
        }
    }
    ctx.data_mut(|d| d.insert_temp(expand_id, expanded));

    // Caption line in the bottom gutter: hovered tile name, or the edit hint.
    let caption = caption.or_else(|| {
        editing.then(|| {
            (
                "Drag to move  ·  drop on a tab  ·  drag the corner to resize".to_owned(),
                true,
            )
        })
    });
    let gutter = gutter_y(cctx.panel);
    // Not while the glass is still growing down to it.
    if let Some((name, ready)) = caption
        && gutter < cctx.shape.max.y - 4.0
    {
        let text = if ready {
            name
        } else {
            format!("{name}  ·  Coming soon")
        };
        ui.painter().text(
            pos2(cctx.panel.center().x, gutter),
            Align2::CENTER_CENTER,
            text,
            Theme::font(SIZE_LABEL),
            with_alpha(theme.text_dim, cctx.alpha),
        );
    }
    out
}

/// The face of a tile in edit mode: its icon, the show tick under it and the
/// resize handle in its corner. Returns the edit made, if any.
fn edit_face(
    ui: &mut Ui,
    rect: Rect,
    tile: &GridTile<'_>,
    theme: &Theme,
    pointer: Option<Pos2>,
    resize: &mut Option<Resize>,
) -> Option<Edit> {
    let ctx = ui.ctx().clone();
    let id = Id::new(("nook-edit", tile.module.id()));
    let mut edit = None;
    let shown = !tile.hidden;
    let on = anim::tween(&ctx, id.with("on"), shown, 0.15);
    let c = rect.center();
    let painter = ui.painter().clone();
    paint::icon(
        &painter,
        c - vec2(0.0, 10.0),
        tile.module.icon(),
        SIZE_ICON + 2.0,
        with_alpha(theme.text, 0.5 + 0.5 * on),
        false,
    );

    // The tick: filled accent when shown, an empty ring when hidden.
    let tick_c = c + vec2(0.0, 12.0);
    let tick = ui.interact(
        Rect::from_center_size(tick_c, vec2(20.0, 16.0)),
        id.with("tick"),
        Sense::click(),
    );
    let hover = anim::tween(&ctx, id.with("tick-hover"), tick.hovered(), 0.1);
    let r = 6.0 + hover;
    painter.circle_stroke(
        tick_c,
        r,
        egui::Stroke::new(1.2, with_alpha(theme.text_dim, 1.0 - on)),
    );
    if on > 0.0 {
        let pop = 0.6 + 0.4 * anim::ease_out_cubic(on);
        painter.circle_filled(tick_c, r * pop, with_alpha(theme.accent, on));
        paint::icon(
            &painter,
            tick_c,
            icons::CHECK,
            8.5 * pop,
            with_alpha(Color32::WHITE, on),
            true,
        );
    }
    let tip = if shown {
        "Shown. Click to hide"
    } else {
        "Hidden. Click to show"
    };
    if tick.on_hover_text(tip).clicked() {
        edit = Some(Edit::ToggleShown(tile.module.id()));
    }

    // The resize handle: drag it and the size snaps to whole cells.
    let handle = ui.interact(
        Rect::from_min_size(rect.max - vec2(24.0, 24.0), vec2(24.0, 24.0)),
        id.with("resize"),
        Sense::drag(),
    );
    if handle.drag_started() {
        *resize = Some(Resize {
            id: tile.module.id(),
            origin: rect.min,
            size: tile.size,
        });
    }
    let mine = resize.is_some_and(|r| r.id == tile.module.id());
    if mine && let (Some(r), Some(p)) = (resize.as_mut(), pointer) {
        r.size = TileSize::new(snap_cells(p.x - r.origin.x), snap_cells(p.y - r.origin.y));
    }
    let color = if mine || handle.hovered() {
        theme.accent
    } else {
        theme.text_faint
    };
    let corner = rect.max - vec2(8.0, 8.0);
    for k in [4.0, 8.0] {
        painter.line_segment(
            [corner + vec2(0.0, -k), corner + vec2(-k, 0.0)],
            egui::Stroke::new(1.5, color),
        );
    }
    handle.on_hover_text("Drag to resize");
    edit
}

/// A module shown smaller than its layout needs: its icon (filled while
/// active), its name when there is room, and the live text of its badge
/// (e.g. the timer's time).
fn compact_tile(ui: &mut Ui, inner: Rect, module: &dyn Module, theme: &Theme, hovered: bool) {
    let active = module.is_active();
    let color = if active {
        theme.accent_fg()
    } else if hovered {
        theme.text
    } else {
        theme.text_dim
    };
    let label = match module.collapsed_badge() {
        Some(Badge::Ring { label, .. }) => Some(label),
        _ => None,
    };
    let painter = ui.painter();
    if inner.width() > inner.height() * 1.5 {
        // Wide: icon on the left, name and badge text beside it.
        let c = pos2(inner.min.x + 10.0, inner.center().y);
        paint::icon(painter, c, module.icon(), SIZE_ICON, color, active);
        let x = inner.min.x + 26.0;
        let (name_y, sub) = match &label {
            Some(l) => (inner.center().y - 7.0, Some(l.as_str())),
            None => (inner.center().y, None),
        };
        painter.text(
            pos2(x, name_y),
            Align2::LEFT_CENTER,
            module.name(),
            Theme::font_semibold(SIZE_LABEL + 1.0),
            theme.text,
        );
        if let Some(sub) = sub {
            painter.text(
                pos2(x, inner.center().y + 8.0),
                Align2::LEFT_CENTER,
                sub,
                Theme::font(SIZE_LABEL - 1.0),
                theme.text_dim,
            );
        }
    } else {
        let c = inner.center() - vec2(0.0, if label.is_some() { 6.0 } else { 0.0 });
        paint::icon(painter, c, module.icon(), SIZE_ICON + 2.0, color, active);
        if let Some(l) = label {
            painter.text(
                inner.center() + vec2(0.0, 13.0),
                Align2::CENTER_CENTER,
                l,
                Theme::font(SIZE_LABEL - 2.0),
                theme.text_dim,
            );
        }
    }
}

/// Disabled tile body: dim icon that crossfades to a "Soon" tag on hover.
fn coming_soon(ui: &mut Ui, inner: Rect, icon: &'static str, theme: &Theme, hovered: bool, id: Id) {
    let ctx = ui.ctx().clone();
    let t = anim::tween(&ctx, id.with("soon"), hovered, 0.12);
    let painter = ui.painter();
    let c = inner.center();
    paint::icon(
        painter,
        c,
        icon,
        SIZE_ICON + 2.0,
        with_alpha(theme.text_faint, 1.0 - t),
        false,
    );
    if t > 0.0 {
        painter.text(
            c,
            Align2::CENTER_CENTER,
            "Soon",
            Theme::font_semibold(SIZE_LABEL),
            with_alpha(theme.text_dim, t),
        );
    }
}

/// Standard tile header for modules: icon (regular/fill by state) and name.
pub fn tile_header(ui: &mut Ui, icon: &'static str, name: &str, active: bool, theme: &Theme) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    let color = if active {
        theme.accent_fg()
    } else {
        theme.text_dim
    };
    let icon_center = pos2(rect.min.x + 9.0, rect.min.y + 9.0);
    let (prev, t) = anim::icon_crossfade(ui.ctx(), ui.id().with("hdr"), icon);
    if let Some(prev) = prev {
        paint::icon(
            painter,
            icon_center,
            prev,
            SIZE_ICON,
            with_alpha(color, 1.0 - t),
            !active,
        );
    }
    paint::icon(
        painter,
        icon_center,
        icon,
        SIZE_ICON,
        with_alpha(color, t),
        active,
    );
    painter.text(
        pos2(rect.min.x + 24.0, rect.min.y + 9.0),
        Align2::LEFT_CENTER,
        name,
        Theme::font_semibold(SIZE_LABEL),
        theme.text_dim,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_height_fits_the_rows_a_tab_uses() {
        let small = TileSize::Small;
        // Home: Media 3x2 beside four 1x1s uses two rows, today's height.
        let home = [TileSize::new(3, 2), small, small, small, small];
        assert_eq!(tab_height(&home, false), 236.0);
        // A short tab is no taller than the minimum panel.
        assert_eq!(panel_height(1), 148.0);
        // Nine 1x1s and a 2x2 reach into a third row.
        let tools = [vec![TileSize::Large], vec![small; 9]].concat();
        assert_eq!(tab_height(&tools, false), panel_height(3));
        assert_eq!(panel_height(3), 324.0);
        // Four rows is the most; edit mode asks for one spare row.
        assert_eq!(tab_height(&[TileSize::new(6, 4)], true), panel_height(4));
        assert_eq!(tab_height(&home, true), panel_height(3));
        // Overflow to a second page keeps the full height.
        assert_eq!(
            tab_height(&[TileSize::new(6, 4), small], false),
            panel_height(4)
        );
    }

    #[test]
    fn drag_snaps_to_whole_cells() {
        // Pointer at the far edge of a 3x2 span from the tile's corner.
        assert_eq!(snap_cells(3.0 * GRID.cell + 2.0 * GRID.gap), 3);
        assert_eq!(snap_cells(0.0), 1);
        assert_eq!(snap_cells(1000.0), 11);
        assert_eq!(TileSize::new(snap_cells(1000.0), 1).cells(), (6, 1));
    }
}
