//! Shell geometry: the continuous-corner "squircle" outline, hit-testing and
//! the bento grid packer. Pure math, no egui state, so it is unit-tested.

use egui::{Pos2, Rect, Vec2, pos2, vec2};

/// Exponent of the superellipse corners. 2.0 is a circular arc; higher values
/// give the smoother "continuous" corner where the curve eases into the edge.
pub const CORNER_EXPONENT: f32 = 4.0;

/// Points sampled per corner when building the outline polygon.
pub const CORNER_SEGMENTS: usize = 14;

/// A rounded rectangle whose corners are superellipse quadrants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Squircle {
    pub rect: Rect,
    pub radius: f32,
    /// Corner superellipse exponent (2 = circular arc).
    pub exponent: f32,
}

impl Squircle {
    pub fn new(rect: Rect, radius: f32) -> Self {
        let max_r = 0.5 * rect.width().min(rect.height());
        Self {
            rect,
            radius: radius.clamp(0.0, max_r.max(0.0)),
            exponent: CORNER_EXPONENT,
        }
    }

    /// Overrides the corner exponent (clamped to >= 2: below that the corner
    /// would pinch inward past a circular arc).
    pub fn with_exponent(mut self, exponent: f32) -> Self {
        self.exponent = exponent.max(2.0);
        self
    }

    /// Is `p` inside (or on) the outline?
    pub fn contains(&self, p: Pos2) -> bool {
        let r = self.radius;
        let half = self.rect.size() * 0.5;
        let d = (p - self.rect.center()).abs();
        if d.x > half.x || d.y > half.y {
            return false;
        }
        if r <= 0.0 {
            return true;
        }
        // Distance into the corner zone, normalized by the radius.
        let qx = ((d.x - (half.x - r)) / r).max(0.0);
        let qy = ((d.y - (half.y - r)) / r).max(0.0);
        qx.powf(self.exponent) + qy.powf(self.exponent) <= 1.0 + 1e-4
    }

    /// The outline as a convex polygon, clockwise in screen space starting at
    /// the top-right corner.
    pub fn outline(&self) -> Vec<Pos2> {
        let mut pts = Vec::with_capacity(4 * (CORNER_SEGMENTS + 1));
        self.outline_into(&mut pts);
        pts
    }

    pub fn outline_into(&self, pts: &mut Vec<Pos2>) {
        pts.clear();
        let r = self.radius;
        let Rect { min, max } = self.rect;
        // Corner centers and the quadrant each corner sweeps.
        // Corner centers in clockwise order (screen space, y down), each with
        // the start angle of its quarter sweep.
        let corners = [
            (pos2(max.x - r, min.y + r), -90.0_f32), // top-right
            (pos2(max.x - r, max.y - r), 0.0),       // bottom-right
            (pos2(min.x + r, max.y - r), 90.0),      // bottom-left
            (pos2(min.x + r, min.y + r), 180.0),     // top-left
        ];
        let e = 2.0 / self.exponent;
        let shape = |v: f32| v.signum() * v.abs().powf(e);
        for (c, start) in corners {
            for s in 0..=CORNER_SEGMENTS {
                let deg = start + 90.0 * s as f32 / CORNER_SEGMENTS as f32;
                let (sin, cos) = deg.to_radians().sin_cos();
                pts.push(c + vec2(shape(cos), shape(sin)) * r);
            }
        }
    }

    pub fn translate(self, delta: Vec2) -> Self {
        Self {
            rect: self.rect.translate(delta),
            ..self
        }
    }
}

/// The shell's outline: a body hanging from the top edge of the screen with
/// round bottom corners and a concave flare where each side meets the edge.
/// `rect` is the body between the flares; the flares reach `flare` points
/// beyond it on each side (0 = none, for a notch against a screen side).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Notch {
    pub rect: Rect,
    /// Bottom corner radius.
    pub radius: f32,
    /// Flare radius on the `[left, right]` side.
    pub flare: [f32; 2],
}

impl Notch {
    pub fn new(rect: Rect, radius: f32, flare: [f32; 2]) -> Self {
        let r = radius.clamp(0.0, 0.5 * rect.width().min(rect.height()).max(0.0));
        // A flare must fit on the straight part of the side.
        let f = |f: f32| f.clamp(0.0, (rect.height() - r).max(0.0));
        Self {
            rect,
            radius: r,
            flare: [f(flare[0]), f(flare[1])],
        }
    }

    /// The same shape grown by `d` points all round (slack for hit-testing
    /// the anti-aliased edge).
    pub fn expand(self, d: f32) -> Self {
        Self {
            rect: self.rect.expand(d),
            radius: self.radius + d,
            ..self
        }
    }

    /// Is `p` inside (or on) the outline?
    pub fn contains(&self, p: Pos2) -> bool {
        let Rect { min, max } = self.rect;
        if p.y < min.y || p.y > max.y {
            return false;
        }
        let [fl, fr] = self.flare;
        if p.x < min.x {
            // Left flare: the corner square minus the circle that is cut out.
            return fl > 0.0 && p.x >= min.x - fl && p.y <= min.y + fl && {
                let c = pos2(min.x - fl, min.y + fl);
                (p - c).length_sq() >= fl * fl - 1e-3
            };
        }
        if p.x > max.x {
            return fr > 0.0 && p.x <= max.x + fr && p.y <= min.y + fr && {
                let c = pos2(max.x + fr, min.y + fr);
                (p - c).length_sq() >= fr * fr - 1e-3
            };
        }
        // Body: only the bottom corners are cut.
        let r = self.radius;
        let dx = (min.x + r - p.x).max(p.x - (max.x - r)).max(0.0);
        let dy = (p.y - (max.y - r)).max(0.0);
        dx * dx + dy * dy <= r * r + 1e-3
    }

    /// The outline, clockwise in screen space from the top-right end of the
    /// top edge. Not convex: the flares curve inward.
    pub fn outline(&self) -> Vec<Pos2> {
        let mut pts = Vec::with_capacity(4 * (CORNER_SEGMENTS + 1));
        self.outline_into(&mut pts);
        pts
    }

    pub fn outline_into(&self, pts: &mut Vec<Pos2>) {
        pts.clear();
        let Rect { min, max } = self.rect;
        let [fl, fr] = self.flare;
        let r = self.radius;
        // (center, radius, start angle in degrees, sweep in degrees)
        let arcs = [
            (pos2(max.x + fr, min.y + fr), fr, -90.0_f32, -90.0_f32), // right flare
            (pos2(max.x - r, max.y - r), r, 0.0, 90.0),               // bottom-right
            (pos2(min.x + r, max.y - r), r, 90.0, 90.0),              // bottom-left
            (pos2(min.x - fl, min.y + fl), fl, 0.0, -90.0),           // left flare
        ];
        for (c, radius, start, sweep) in arcs {
            // A zero-radius corner (no flare against a screen side) is one point.
            let n = if radius < 0.01 { 0 } else { CORNER_SEGMENTS };
            for s in 0..=n {
                let deg = start + sweep * s as f32 / CORNER_SEGMENTS as f32;
                let (sin, cos) = deg.to_radians().sin_cos();
                pts.push(c + vec2(cos, sin) * radius);
            }
        }
    }
}

/// Linear interpolation helper.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

// ---------------------------------------------------------------------------
// Bento grid

/// Tile footprint in grid cells (columns x rows), 1x1 up to 6x4. Written in
/// config as `"3x2"`; the old `small`/`wide`/`large` names read as 1x1, 2x1
/// and 2x2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileSize {
    cols: u8,
    rows: u8,
}

#[allow(non_upper_case_globals)]
impl TileSize {
    pub const MAX_COLS: usize = 6;
    pub const MAX_ROWS: usize = 4;
    /// 1x1
    pub const Small: TileSize = TileSize { cols: 1, rows: 1 };
    /// 2x1
    pub const Wide: TileSize = TileSize { cols: 2, rows: 1 };
    /// 2x2
    pub const Large: TileSize = TileSize { cols: 2, rows: 2 };

    /// A size of `cols` x `rows`, clamped to 1x1..=6x4.
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols: cols.clamp(1, Self::MAX_COLS) as u8,
            rows: rows.clamp(1, Self::MAX_ROWS) as u8,
        }
    }

    pub fn cells(self) -> (usize, usize) {
        (self.cols as usize, self.rows as usize)
    }

    /// Is this smaller than `needed` in either direction?
    pub fn is_smaller_than(self, needed: TileSize) -> bool {
        self.cols < needed.cols || self.rows < needed.rows
    }
}

impl std::fmt::Display for TileSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

impl std::str::FromStr for TileSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "small" => return Ok(Self::Small),
            "wide" => return Ok(Self::Wide),
            "large" => return Ok(Self::Large),
            _ => {}
        }
        let bad = || format!("tile size `{s}` is not 1x1 to 6x4");
        let (c, r) = s.split_once('x').ok_or_else(bad)?;
        let c = c.parse::<usize>().map_err(|_| bad())?;
        let r = r.parse::<usize>().map_err(|_| bad())?;
        if (1..=Self::MAX_COLS).contains(&c) && (1..=Self::MAX_ROWS).contains(&r) {
            Ok(Self::new(c, r))
        } else {
            Err(bad())
        }
    }
}

impl serde::Serialize for TileSize {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for TileSize {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Grid metrics for the bento layout.
#[derive(Clone, Copy, Debug)]
pub struct GridSpec {
    pub columns: usize,
    pub rows: usize,
    pub cell: f32,
    pub gap: f32,
}

impl GridSpec {
    pub fn size(&self) -> Vec2 {
        vec2(
            self.columns as f32 * self.cell + (self.columns.saturating_sub(1)) as f32 * self.gap,
            self.rows as f32 * self.cell + (self.rows.saturating_sub(1)) as f32 * self.gap,
        )
    }

    pub fn cell_rect(&self, origin: Pos2, col: usize, row: usize, size: TileSize) -> Rect {
        let (w, h) = size.cells();
        let step = self.cell + self.gap;
        let min = origin + vec2(col as f32 * step, row as f32 * step);
        let span = vec2(
            w as f32 * self.cell + (w - 1) as f32 * self.gap,
            h as f32 * self.cell + (h - 1) as f32 * self.gap,
        );
        Rect::from_min_size(min, span)
    }
}

/// Where a tile landed in the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub index: usize,
    pub col: usize,
    pub row: usize,
}

/// Dense first-fit packing in row-major order (fill left-to-right, then the
/// next row), so a tab uses as few rows as it can. Tiles that do not fit are
/// left out of the result.
pub fn pack(spec: &GridSpec, sizes: &[TileSize]) -> Vec<Placement> {
    let mut used = vec![false; spec.columns * spec.rows];
    let mut out = Vec::with_capacity(sizes.len());
    for (index, size) in sizes.iter().enumerate() {
        let (w, h) = size.cells();
        'search: for row in 0..spec.rows {
            for col in 0..spec.columns {
                if col + w > spec.columns || row + h > spec.rows {
                    continue;
                }
                let free =
                    (col..col + w).all(|c| (row..row + h).all(|r| !used[r * spec.columns + c]));
                if free {
                    for c in col..col + w {
                        for r in row..row + h {
                            used[r * spec.columns + c] = true;
                        }
                    }
                    out.push(Placement { index, col, row });
                    break 'search;
                }
            }
        }
    }
    out
}

/// Packs `sizes` onto as many grids as it takes: each page takes what
/// [`pack`] fits of the tiles still left, in order. Indices are into
/// `sizes`. Always returns at least one (maybe empty) page.
pub fn pack_pages(spec: &GridSpec, sizes: &[TileSize]) -> Vec<Vec<Placement>> {
    let mut left: Vec<usize> = (0..sizes.len()).collect();
    let mut pages = Vec::new();
    while !left.is_empty() {
        let page_sizes: Vec<TileSize> = left.iter().map(|&i| sizes[i]).collect();
        let placed: Vec<Placement> = pack(spec, &page_sizes)
            .into_iter()
            .map(|p| Placement {
                index: left[p.index],
                ..p
            })
            .collect();
        if placed.is_empty() {
            // A tile larger than the grid; drop the rest.
            break;
        }
        left.retain(|i| !placed.iter().any(|p| p.index == *i));
        pages.push(placed);
    }
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    pages
}

/// Rows the tallest of `pages` reaches (at least 1).
pub fn rows_used(pages: &[Vec<Placement>], sizes: &[TileSize]) -> usize {
    pages
        .iter()
        .flatten()
        .map(|p| p.row + sizes[p.index].cells().1)
        .max()
        .unwrap_or(0)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pill() -> Squircle {
        Squircle::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(140.0, 34.0)), 17.0)
    }

    #[test]
    fn radius_is_clamped_to_half_the_short_side() {
        let s = Squircle::new(Rect::from_min_size(Pos2::ZERO, vec2(100.0, 20.0)), 50.0);
        assert_eq!(s.radius, 10.0);
    }

    #[test]
    fn contains_center_and_edges() {
        let s = pill();
        assert!(s.contains(pos2(70.0, 17.0)));
        assert!(s.contains(pos2(70.0, 0.5)));
        assert!(s.contains(pos2(0.5, 17.0)));
        assert!(!s.contains(pos2(-0.5, 17.0)));
        assert!(!s.contains(pos2(70.0, 34.5)));
    }

    #[test]
    fn corners_are_cut() {
        let s = pill();
        // The bounding box corner is outside a rounded shape.
        assert!(!s.contains(pos2(0.5, 0.5)));
        assert!(!s.contains(pos2(139.5, 33.5)));
        // A point just inside the curve is inside.
        assert!(s.contains(pos2(4.0, 6.0)));
    }

    #[test]
    fn circular_exponent_makes_a_true_capsule() {
        let s = pill().with_exponent(2.0);
        // On the semicircle end at 45 degrees.
        let c = pos2(17.0, 17.0);
        let on = c + vec2(-1.0, -1.0).normalized() * 16.9;
        let off = c + vec2(-1.0, -1.0).normalized() * 17.2;
        assert!(s.contains(on));
        assert!(!s.contains(off));
        // The default squircle bulges further into the corner than a circle.
        assert!(pill().contains(off));
    }

    #[test]
    fn square_rect_with_zero_radius_contains_corner() {
        let s = Squircle::new(Rect::from_min_size(Pos2::ZERO, vec2(10.0, 10.0)), 0.0);
        assert!(s.contains(pos2(0.0, 0.0)));
        assert!(s.contains(pos2(10.0, 10.0)));
    }

    #[test]
    fn outline_points_lie_on_the_boundary() {
        let s = Squircle::new(
            Rect::from_min_size(pos2(10.0, 5.0), vec2(200.0, 80.0)),
            24.0,
        );
        let pts = s.outline();
        assert_eq!(pts.len(), 4 * (CORNER_SEGMENTS + 1));
        for p in &pts {
            assert!(s.contains(*p), "{p:?} should be on/inside");
            // Nudge outward from the center: must leave the shape.
            let out = *p + (*p - s.rect.center()).normalized() * 0.75;
            assert!(!s.contains(out), "{out:?} should be outside");
        }
    }

    #[test]
    fn outline_is_clockwise_and_convex() {
        let pts = pill().outline();
        let n = pts.len();
        let mut area2 = 0.0;
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            area2 += a.x * b.y - b.x * a.y;
        }
        // Screen space (y down): clockwise has positive signed area.
        assert!(area2 > 0.0);
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            let c = pts[(i + 2) % n];
            let cross = (b - a).x * (c - b).y - (b - a).y * (c - b).x;
            assert!(cross >= -1e-3, "concave at {i}");
        }
    }

    fn notch() -> Notch {
        // Body x 100..240 (a 140 x 34 pill), flares 10 on both sides.
        Notch::new(
            Rect::from_min_size(pos2(100.0, 0.0), vec2(140.0, 34.0)),
            17.0,
            [10.0, 10.0],
        )
    }

    #[test]
    fn notch_contains_the_flares() {
        let n = notch();
        // Body and the top edge across the flares.
        assert!(n.contains(pos2(170.0, 17.0)));
        assert!(n.contains(pos2(95.0, 0.5)));
        assert!(n.contains(pos2(245.0, 0.5)));
        // Just outside the concave arc, next to the body (inside the flare).
        assert!(n.contains(pos2(99.0, 2.0)));
        assert!(n.contains(pos2(241.0, 2.0)));
        // The scooped-out corner: inside the flare square, inside the circle.
        assert!(!n.contains(pos2(92.0, 8.0)));
        assert!(!n.contains(pos2(248.0, 8.0)));
        // Below the flare, and beyond its reach.
        assert!(!n.contains(pos2(99.0, 12.0)));
        assert!(!n.contains(pos2(89.0, 0.5)));
        assert!(!n.contains(pos2(171.0, -0.5)));
    }

    #[test]
    fn notch_bottom_corners_are_round() {
        let n = notch();
        assert!(!n.contains(pos2(100.5, 33.5)));
        assert!(!n.contains(pos2(239.5, 33.5)));
        assert!(!n.contains(pos2(105.0, 33.5)));
        assert!(!n.contains(pos2(170.0, 34.5)));
        assert!(n.contains(pos2(117.0, 33.5)));
        // No flare against a screen side.
        let left = Notch::new(n.rect, 17.0, [0.0, 10.0]);
        assert!(!left.contains(pos2(95.0, 0.5)));
        assert!(left.contains(pos2(245.0, 0.5)));
    }

    #[test]
    fn notch_outline_matches_contains() {
        for n in [notch(), Notch::new(notch().rect, 17.0, [0.0, 10.0])] {
            let pts = n.outline();
            // Clockwise (positive area in screen space) and simple enough
            // that every vertex is on the boundary of `contains`.
            let mut area2 = 0.0;
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                area2 += a.x * b.y - b.x * a.y;
            }
            assert!(area2 > 0.0);
            for p in &pts {
                assert!(n.contains(*p), "{p:?} should be on/inside");
            }
            // The polygon area agrees with a grid count of `contains`.
            let mut hits = 0.0;
            for y in 0..340 {
                for x in 850..2550 {
                    if n.contains(pos2(x as f32 / 10.0 + 0.05, y as f32 / 10.0 + 0.05)) {
                        hits += 0.01;
                    }
                }
            }
            assert!(
                (hits - area2 / 2.0).abs() < 20.0,
                "{hits} vs {}",
                area2 / 2.0
            );
        }
    }

    fn grid() -> GridSpec {
        GridSpec {
            columns: 6,
            rows: 4,
            cell: 80.0,
            gap: 8.0,
        }
    }

    fn at(index: usize, col: usize, row: usize) -> Placement {
        Placement { index, col, row }
    }

    #[test]
    fn packer_fills_rows_left_to_right() {
        // Media 3x2 with four 1x1s: they fill beside it, then under it.
        let media = TileSize::new(3, 2);
        let sizes = [
            media,
            TileSize::Small,
            TileSize::Small,
            TileSize::Small,
            TileSize::Small,
        ];
        let placed = pack(&grid(), &sizes);
        assert_eq!(
            placed,
            [
                at(0, 0, 0),
                at(1, 3, 0),
                at(2, 4, 0),
                at(3, 5, 0),
                at(4, 3, 1)
            ]
        );
        assert_eq!(rows_used(&[placed], &sizes), 2);
    }

    #[test]
    fn a_full_grid_overflows_to_a_second_page() {
        let sizes = [TileSize::new(6, 4), TileSize::Small, TileSize::new(2, 2)];
        let pages = pack_pages(&grid(), &sizes);
        assert_eq!(pages, [vec![at(0, 0, 0)], vec![at(1, 0, 0), at(2, 1, 0)]]);
        assert_eq!(rows_used(&pages, &sizes), 4);
        // Six wide tiles of 3x2 do not fit in one page: four do.
        let sizes = [TileSize::new(3, 2); 5];
        let pages = pack_pages(&grid(), &sizes);
        assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [4, 1]);
    }

    #[test]
    fn tile_sizes_parse_and_migrate() {
        assert_eq!("small".parse(), Ok(TileSize::new(1, 1)));
        assert_eq!("wide".parse(), Ok(TileSize::new(2, 1)));
        assert_eq!("large".parse(), Ok(TileSize::new(2, 2)));
        assert_eq!("6x4".parse(), Ok(TileSize::new(6, 4)));
        for bad in ["7x1", "1x5", "0x2", "huge??", "2", "2x"] {
            assert!(bad.parse::<TileSize>().is_err(), "{bad}");
        }
        assert_eq!(TileSize::new(3, 2).to_string(), "3x2");
        assert!(TileSize::new(1, 4).is_smaller_than(TileSize::Large));
        assert!(!TileSize::new(3, 2).is_smaller_than(TileSize::Large));
    }

    #[test]
    fn packer_skips_tiles_that_do_not_fit() {
        let spec = GridSpec {
            columns: 2,
            rows: 1,
            cell: 10.0,
            gap: 2.0,
        };
        let placed = pack(&spec, &[TileSize::Large, TileSize::Wide]);
        assert_eq!(
            placed,
            vec![Placement {
                index: 1,
                col: 0,
                row: 0
            }]
        );
    }

    #[test]
    fn cell_rect_spans_gaps() {
        let spec = grid();
        assert_eq!(spec.size(), vec2(520.0, 344.0));
        let r = spec.cell_rect(Pos2::ZERO, 1, 1, TileSize::Large);
        assert_eq!(r, Rect::from_min_size(pos2(88.0, 88.0), vec2(168.0, 168.0)));
    }

    #[test]
    fn overflow_goes_to_the_next_page() {
        let spec = GridSpec {
            columns: 2,
            rows: 1,
            cell: 60.0,
            gap: 8.0,
        };
        let sizes = [TileSize::Small, TileSize::Wide, TileSize::Small];
        let pages = pack_pages(&spec, &sizes);
        let idx: Vec<Vec<usize>> = pages
            .iter()
            .map(|p| p.iter().map(|pl| pl.index).collect())
            .collect();
        assert_eq!(idx, [vec![0, 2], vec![1]]);
        assert_eq!(pack_pages(&spec, &[]).len(), 1);
    }
}
