//! Selections: how much of each canvas pixel is selected (0 none, 255 all), made from shapes or by
//! colour, and combined, inverted and feathered.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use crate::geom::{IRect, Vec2};
use crate::par::cores;

/// A selection over a `width` × `height` canvas, one byte per pixel, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A polygon's coverage only where it reaches: `rect` in canvas pixels and `coverage` row by
/// row from `rect`'s top left. It keeps only the bbox the polygon actually reaches, so a bounded
/// shape does not allocate the whole canvas; a canvas-spanning shape naturally spans it.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskPatch {
    pub rect: IRect,
    pub coverage: Vec<u8>,
}

impl MaskPatch {
    /// The smallest nonzero rectangle in this patch, with its rows. The canvas is never touched;
    /// only the patch's own pixels are scanned. Invalid rectangles/coverage give `None`.
    pub fn trim(self) -> Option<MaskPatch> {
        let (x, y) = (i32::try_from(self.rect.x0).ok()?, i32::try_from(self.rect.y0).ok()?);
        if x < 0 || y < 0 {
            return None;
        }
        let (w, h) = (self.rect.width() as usize, self.rect.height() as usize);
        if w == 0 || h == 0 || self.coverage.len() != w * h {
            return None;
        }
        trim_patch(x as usize, y as usize, w, h, self.coverage)
    }
}

/// How a new shape joins the selection there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Add,
    Remove,
}

/// Rows of sub-samples per pixel when filling a polygon.
const POLYGON_ROWS: usize = 4;

impl Mask {
    /// Nothing selected.
    pub fn empty(width: u32, height: u32) -> Mask {
        Mask { width, height, data: vec![0; width as usize * height as usize] }
    }

    /// The smallest rectangle holding everything it takes (x, y, width, height); none when it
    /// takes nothing.
    pub fn bounds(&self) -> Option<[u32; 4]> {
        let w = self.width as usize;
        if w == 0 {
            return None;
        }
        let (mut top, mut bottom, mut left, mut right) = (None, 0, usize::MAX, 0);
        for (y, row) in self.data.chunks(w).enumerate() {
            let Some(first) = row.iter().position(|&v| v > 0) else { continue };
            let last = row.iter().rposition(|&v| v > 0).unwrap_or(first);
            top.get_or_insert(y);
            bottom = y;
            left = left.min(first);
            right = right.max(last);
        }
        let top = top?;
        let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        Some([n(left), n(top), n(right - left + 1), n(bottom - top + 1)])
    }

    pub fn is_empty(&self) -> bool {
        self.data.iter().all(|&v| v == 0)
    }

    /// The rectangle between corners `a` and `b` (canvas pixels), with partly covered edge pixels.
    pub fn rect(width: u32, height: u32, a: Vec2, b: Vec2) -> Mask {
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        let mut m = Mask::empty(width, height);
        let w = width as usize;
        let cover = |lo: f32, hi: f32, p: usize| (hi.min(p as f32 + 1.0) - lo.max(p as f32)).clamp(0.0, 1.0);
        let cols: Vec<f32> = (0..w).map(|x| cover(x0, x1, x)).collect();
        // a row wholly inside is the same each time
        let full: Vec<u8> = cols.iter().map(|&cx| to_byte(cx)).collect();
        for (y, row) in m.data.chunks_exact_mut(w.max(1)).enumerate() {
            let cy = cover(y0, y1, y);
            if cy <= 0.0 {
                continue;
            }
            if cy >= 1.0 {
                row.copy_from_slice(&full);
                continue;
            }
            for (v, cx) in row.iter_mut().zip(&cols) {
                *v = to_byte(cx * cy);
            }
        }
        m
    }

    /// The ellipse in the box between corners `a` and `b`, with soft edge pixels.
    pub fn ellipse(width: u32, height: u32, a: Vec2, b: Vec2) -> Mask {
        let c = a.lerp(b, 0.5);
        let (rx, ry) = (((b.x - a.x) / 2.0).abs(), ((b.y - a.y) / 2.0).abs());
        let mut m = Mask::empty(width, height);
        if rx < 1e-3 || ry < 1e-3 {
            return m;
        }
        let w = width as usize;
        let (y_lo, y_hi) =
            ((c.y - ry - 1.0).max(0.0) as usize, ((c.y + ry + 1.0).max(0.0) as usize).min(height as usize));
        let (x_lo, x_hi) = ((c.x - rx - 1.0).max(0.0) as usize, ((c.x + rx + 1.0).max(0.0) as usize).min(w));
        // the rows it reaches, in bands side by side
        let reached = m.data.get_mut(y_lo * w..y_hi * w).unwrap_or_default();
        by_bands(reached, w, |first, rows| {
            for (y, row) in (y_lo + first..).zip(rows.chunks_mut(w.max(1))) {
                for x in x_lo..x_hi {
                    let p = Vec2::new((x as f32 + 0.5 - c.x) / rx, (y as f32 + 0.5 - c.y) / ry);
                    let len = p.length();
                    // the distance from the edge in pixels: the implicit function over its gradient
                    let gradient = Vec2::new(p.x / rx, p.y / ry).length().max(1e-6);
                    let distance = (len - 1.0) * len / gradient;
                    if let Some(v) = row.get_mut(x) {
                        *v = to_byte(0.5 - distance);
                    }
                }
            }
        });
        m
    }

    /// The polygon through `points` (canvas pixels, closed back to the first), filled by the
    /// non-zero rule, with partly covered edge pixels.
    pub fn polygon(width: u32, height: u32, points: &[Vec2]) -> Mask {
        let mut m = Mask::empty(width, height);
        let pts: Vec<Vec2> = points.iter().copied().filter(|p| p.is_finite()).collect();
        if pts.len() < 3 {
            return m;
        }
        let (y_lo, y_hi) = polygon_rows(height, &pts);
        if y_hi <= y_lo {
            return m;
        }
        let edges = polygon_edges(&pts);
        let w = width as usize;
        let reached = m.data.get_mut(y_lo * w..y_hi * w).unwrap_or_default();
        polygon_raster(0, w, y_lo, y_hi, &edges, reached);
        m
    }

    /// The polygon through `points`, like [`Mask::polygon`], only over the pixels it reaches.
    /// Coordinates stay in canvas/world space: nothing is translated before the crossings are
    /// measured, so the coverage bytes are the same as the full mask's. The nonzero spans are
    /// walked once for their true bounds and again to raster them; memory stays O(edges + patch),
    /// and a bbox that truly spans the canvas can still be W×H.
    pub fn polygon_patch(width: u32, height: u32, points: &[Vec2]) -> Option<MaskPatch> {
        let pts: Vec<Vec2> = points.iter().copied().filter(|p| p.is_finite()).collect();
        if pts.len() < 3 || width == 0 || height == 0 {
            return None;
        }
        let (y_lo, y_hi) = polygon_rows(height, &pts);
        if y_hi <= y_lo {
            return None;
        }
        let edges = polygon_edges(&pts);
        let (x_lo, x_hi, y_lo, y_hi) = polygon_span_bounds(width as usize, y_lo, y_hi, &edges)?;
        let (w, h) = (x_hi - x_lo, y_hi - y_lo);
        let mut coverage = vec![0u8; w * h];
        polygon_raster(x_lo, x_hi, y_lo, y_hi, &edges, &mut coverage);
        trim_patch(x_lo, y_lo, w, h, coverage)
    }

    /// Joins `other` to this selection.
    pub fn combine(&mut self, other: &Mask, how: Combine) {
        for (a, &b) in self.data.iter_mut().zip(&other.data) {
            *a = match how {
                Combine::Add => (*a).max(b),
                Combine::Remove => (*a).min(255 - b),
            };
        }
    }

    pub fn invert(&mut self) {
        self.data.iter_mut().for_each(|v| *v = 255 - *v);
    }

    /// Expands (`amount > 0`) or contracts (`amount < 0`) the coverage by a square window of
    /// `|amount|` pixels each way: the largest or smallest alpha in the window, without a
    /// threshold. The window is clipped at the canvas edges, as the original GTK port did, so a
    /// selection touching an edge is not eroded past it. Partly covered pixels keep their exact
    /// coverage values; no anti-aliasing is invented. `0`, no pixels, or no selection is a no-op.
    pub fn expand_contract(&mut self, amount: i32) {
        let (w, h) = (self.width as usize, self.height as usize);
        if amount == 0 || w == 0 || h == 0 || self.data.len() != w * h {
            return;
        }
        // The square window cannot reach past the longest side; clamping keeps huge signed
        // amounts (including i32::MIN) safe without a radius-proportional loop.
        let radius = (amount.unsigned_abs() as usize).min(w.max(h));
        if radius == 0 {
            return;
        }
        let dilate = amount > 0;
        separable_extremum(&mut self.data, w, h, radius, dilate);
    }

    /// Softens the edges: three box blurs `radius` pixels wide each way (close to a Gaussian).
    pub fn feather(&mut self, radius: f32) {
        self.feather_in(radius, &mut Vec::new());
    }

    /// [`Mask::feather`] working in `room`, kept from one call to the next (a slider feathering
    /// again and again doesn't ask the system for the memory each time).
    pub fn feather_in(&mut self, radius: f32, room: &mut Vec<f32>) {
        let r = radius.max(0.0).round() as usize;
        if r == 0 {
            return;
        }
        // only around what is selected can change: past three blurs' reach of it all stays clear,
        // so the box that far around it is blurred alone (its edges clear, as the canvas's are
        // there), with the very same sums
        let Some([bx, by, bw, bh]) = self.bounds() else { return };
        let reach = 3 * r + 1;
        let (w, h) = (self.width as usize, self.height as usize);
        let (x0, y0) = ((bx as usize).saturating_sub(reach), (by as usize).saturating_sub(reach));
        let (x1, y1) = (((bx + bw) as usize + reach).min(w), ((by + bh) as usize + reach).min(h));
        if (x1 - x0) * (y1 - y0) < w * h * 3 / 4 {
            let mut part = Mask { width: (x1 - x0) as u32, height: (y1 - y0) as u32, data: Vec::new() };
            for row in self.data.chunks(w.max(1)).skip(y0).take(y1 - y0) {
                part.data.extend_from_slice(row.get(x0..x1).unwrap_or_default());
            }
            part.feather_all(r, room);
            for (row, from) in self.data.chunks_mut(w.max(1)).skip(y0).zip(part.data.chunks((x1 - x0).max(1))) {
                if let Some(to) = row.get_mut(x0..x1) {
                    to.copy_from_slice(from);
                }
            }
            return;
        }
        self.feather_all(r, room);
    }

    /// [`Mask::feather`] over the whole mask, `r` pixels a blur. Blurs across and down don't
    /// depend on each other's order, so each row takes its three across while it is at hand, then
    /// narrow strips of columns their three down: two passes over the picture, not six.
    fn feather_all(&mut self, r: usize, values: &mut Vec<f32>) {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 {
            return;
        }
        // every value is written before it is read
        if values.len() != w * h {
            values.clear();
            values.resize(w * h, 0.0);
        }
        let data = &self.data;
        by_bands(values, w, |first, rows| {
            let mut line = Vec::new();
            for (y, row) in (first..).zip(rows.chunks_mut(w)) {
                let from = data.get(y * w..).and_then(|d| d.get(..w)).unwrap_or(&[]);
                for (f, &v) in row.iter_mut().zip(from) {
                    *f = f32::from(v);
                }
                for _ in 0..3 {
                    blur_row(row, r, &mut line);
                }
            }
        });
        blur_columns_into(values, &mut self.data, w, h, r);
    }

    /// The pixels joined to `seed` whose colour is within `threshold` (0..1) of the seed's, in
    /// premultiplied RGBA8 `pixels` the mask's size: an automatic selection.
    pub fn by_colour(width: u32, height: u32, pixels: &[u8], seed: (u32, u32), threshold: f32) -> Mask {
        ColourFlood::new(width, height, pixels, seed).map_or_else(|| Mask::empty(width, height), |f| f.mask(threshold))
    }
}

/// How far apart two colours may be added up (the four channels' differences, 0..255 each) and
/// still be within a threshold: twice 255. Farther ones are kept as one more.
const APART_MOST: u16 = 510;

/// An automatic selection's measure of a layer from a seed, kept while its threshold is dragged:
/// each pixel's colour difference from the seed's (the four channels' differences added up), and,
/// once worked out on a thread of its own, each pixel's reach (the least difference at which the
/// selection gets to it, see [`crate::fill::reaches`]): with it a selection is one pass, until
/// then a flood.
pub struct ColourFlood {
    width: u32,
    height: u32,
    seed: (u32, u32),
    apart: Arc<Vec<u16>>,
    reach: Arc<OnceLock<Vec<u16>>>,
}

impl ColourFlood {
    /// `pixels` (premultiplied RGBA8, `width` x `height`) measured from the pixel at `seed`; None
    /// when the seed is off the picture.
    pub fn new(width: u32, height: u32, pixels: &[u8], seed: (u32, u32)) -> Option<ColourFlood> {
        let (w, h) = (width as usize, height as usize);
        let (sx, sy) = (seed.0 as usize, seed.1 as usize);
        let Some(needed) = w.checked_mul(h).and_then(|count| count.checked_mul(4)) else { return None };
        if sx >= w || sy >= h || pixels.len() < needed {
            return None;
        }
        let i = (sy * w + sx) * 4;
        let target: [u8; 4] = pixels.get(i..i + 4)?.try_into().ok()?;
        // each worked out once, in bands side by side
        let colours = pixels.as_chunks::<4>().0;
        let mut apart = vec![0u16; w * h];
        by_bands(&mut apart, w, |first, rows| {
            for (n, p) in rows.iter_mut().zip(colours.iter().skip(first * w)) {
                let sum: u16 = p.iter().zip(&target).map(|(a, b)| u16::from(a.abs_diff(*b))).sum();
                *n = sum.min(APART_MOST + 1);
            }
        });
        Some(ColourFlood { width, height, seed, apart: Arc::new(apart), reach: Arc::new(OnceLock::new()) })
    }

    /// The pixel it is measured from.
    pub fn seed(&self) -> (u32, u32) {
        self.seed
    }

    /// Starts working out each pixel's reach on a thread of its own (not for very large pictures,
    /// nor if a thread can't start: the selections stay floods). The thread stops if the
    /// measure is let go first.
    pub fn work_out_reach(&self) {
        // two bytes apart and two of reach a pixel, and lists of up to four
        if !crate::fill::reach_fits(u64::from(self.width) * u64::from(self.height), 8) {
            return;
        }
        let (apart, reach) = (self.apart.clone(), self.reach.clone());
        let (w, h, seed) = (self.width as usize, self.height as usize, (self.seed.0 as usize, self.seed.1 as usize));
        let _ = std::thread::Builder::new().name("selection reach".to_owned()).spawn(move || {
            let wanted = || Arc::strong_count(&reach) > 1;
            if let Some(found) = crate::fill::reaches(&apart, w, h, seed, APART_MOST + 1, &wanted) {
                let _ = reach.set(found);
            }
        });
    }

    /// The pixels joined to the seed whose colour is within `threshold` (0..1) of the seed's.
    pub fn mask(&self, threshold: f32) -> Mask {
        // the differences added up, at most this
        let cut = (threshold.clamp(0.0, 1.0) * 2.0 * 255.0).floor() as u16;
        match self.reach.get() {
            Some(reach) => {
                let mut m = Mask::empty(self.width, self.height);
                by_bands(&mut m.data, self.width as usize, |first, rows| {
                    let w = self.width as usize;
                    for (v, &r) in rows.iter_mut().zip(reach.iter().skip(first * w)) {
                        *v = if r <= cut { 255 } else { 0 };
                    }
                });
                m
            }
            None => self
                .by_scanlines(cut, crate::fill::flood_here_most(self.apart.len()))
                .unwrap_or_else(|| self.by_runs(cut)),
        }
    }

    /// [`ColourFlood::mask`] by a flood on this thread: scanlines from the seed, each span selected
    /// whole, then the runs above and below it seeded, each row looked along as a slice; none once
    /// it has selected more than `most` pixels (a large one is quicker on every core,
    /// [`ColourFlood::by_runs`]).
    fn by_scanlines(&self, cut: u16, most: usize) -> Option<Mask> {
        let mut m = Mask::empty(self.width, self.height);
        let (w, h) = (self.width as usize, self.height as usize);
        // the pixels near the seed's colour are marked open (1) until the fill reaches them (255)
        by_bands(&mut m.data, w, |first, rows| {
            for (n, &d) in rows.iter_mut().zip(self.apart.iter().skip(first * w)) {
                *n = u8::from(d <= cut);
            }
        });
        let mut stack = vec![(self.seed.0 as usize, self.seed.1 as usize)];
        let mut filled = 0;
        while let Some((x, y)) = stack.pop() {
            let Some(line) = m.data.get_mut(y * w..(y + 1) * w) else { continue };
            if line.get(x) != Some(&1) {
                continue;
            }
            let l = x - line.get(..x).unwrap_or_default().iter().rev().take_while(|&&v| v == 1).count();
            let r = x + line.get(x + 1..).unwrap_or_default().iter().take_while(|&&v| v == 1).count();
            filled += r + 1 - l;
            if filled > most {
                return None;
            }
            if let Some(span) = line.get_mut(l..=r) {
                span.fill(255);
            }
            for ny in [y.wrapping_sub(1), y + 1] {
                // off the top (wrapped round) or the bottom: the row's place isn't worked out
                if ny >= h {
                    continue;
                }
                let Some(next) = m.data.get(ny * w..(ny + 1) * w) else { continue };
                let Some(under) = next.get(l..=r) else { continue };
                let mut xi = 0;
                while let Some(start) = under.get(xi..).and_then(|rest| rest.iter().position(|&v| v == 1)) {
                    stack.push((l + xi + start, ny));
                    xi += start;
                    xi += under.get(xi..).unwrap_or_default().iter().take_while(|&&v| v == 1).count();
                }
            }
        }
        // near but not joined: not selected
        by_bands(&mut m.data, w, |_, rows| {
            for v in rows.iter_mut().filter(|v| **v == 1) {
                *v = 0;
            }
        });
        Some(m)
    }

    /// [`ColourFlood::mask`] by a flood, worked out on every core ([`crate::fill::joined`]).
    fn by_runs(&self, cut: u16) -> Mask {
        let (w, h) = (self.width as usize, self.height as usize);
        let seed = (self.seed.0 as usize, self.seed.1 as usize);
        let data = crate::fill::joined(&self.apart, w, h, seed, cut, |_| 255);
        Mask { width: self.width, height: self.height, data }
    }
}

impl Mask {
    /// How much is selected at canvas point `p`, between pixel centres.
    fn sample(&self, p: Vec2) -> f32 {
        let (x, y) = (p.x - 0.5, p.y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: f32, yi: f32| -> f32 {
            if xi < 0.0 || yi < 0.0 || xi >= self.width as f32 || yi >= self.height as f32 {
                return 0.0;
            }
            let i = yi as usize * self.width as usize + xi as usize;
            f32::from(self.data.get(i).copied().unwrap_or(0)) / 255.0
        };
        let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx;
        let bottom = at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// The selection moved as content is: each pixel takes what `to_source` (a pixel centre to
    /// where it came from) finds there.
    pub fn moved(&self, to_source: impl Fn(Vec2) -> Option<Vec2> + Sync) -> Mask {
        let mut out = Mask::empty(self.width, self.height);
        let Some([bx, by, bw, bh]) = self.bounds() else { return out };
        // a point finds nothing where both pixels it reads across (or down) are outside the
        // rectangle the mask takes
        let (left, right) = (bx as f32 - 1.0, (bx + bw) as f32);
        let (top, bottom) = (by as f32 - 1.0, (by + bh) as f32);
        let w = self.width as usize;
        by_bands(&mut out.data, w, |first, rows| {
            for (y, row) in (first..).zip(rows.chunks_exact_mut(w)) {
                for (x, v) in row.iter_mut().enumerate() {
                    let Some(s) = to_source(Vec2::new(x as f32 + 0.5, y as f32 + 0.5)) else { continue };
                    let (sx, sy) = (s.x - 0.5, s.y - 0.5);
                    if sx >= left && sx < right && sy >= top && sy < bottom {
                        *v = to_byte(self.sample(s));
                    }
                }
            }
        });
        out
    }

    /// The selection moved through a mesh of triangles, three corners each of (where it goes,
    /// where it came from) in pixels, as a warp moves content.
    pub fn warped(&self, triangles: &[[f32; 4]]) -> Mask {
        let mut out = Mask::empty(self.width, self.height);
        let w = self.width as usize;
        // each band of rows takes the parts of the triangles that fall on it
        by_bands(&mut out.data, w, |first, rows| {
            let last = first + rows.len() / w.max(1);
            for &[a, b, c] in triangles.as_chunks::<3>().0 {
                let (pa, pb, pc) = (Vec2::new(a[0], a[1]), Vec2::new(b[0], b[1]), Vec2::new(c[0], c[1]));
                let area = (pb.x - pa.x) * (pc.y - pa.y) - (pc.x - pa.x) * (pb.y - pa.y);
                if area.abs() < 1e-9 {
                    continue;
                }
                let (lo_x, hi_x) = (
                    pa.x.min(pb.x).min(pc.x).floor().max(0.0) as usize,
                    (pa.x.max(pb.x).max(pc.x).ceil().max(0.0) as usize).min(w),
                );
                let (lo_y, hi_y) = (
                    (pa.y.min(pb.y).min(pc.y).floor().max(0.0) as usize).max(first),
                    (pa.y.max(pb.y).max(pc.y).ceil().max(0.0) as usize).min(last),
                );
                for y in lo_y..hi_y {
                    for x in lo_x..hi_x {
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        // where p falls in the triangle, as weights of its corners
                        let wb = ((p.x - pa.x) * (pc.y - pa.y) - (pc.x - pa.x) * (p.y - pa.y)) / area;
                        let wc = ((pb.x - pa.x) * (p.y - pa.y) - (p.x - pa.x) * (pb.y - pa.y)) / area;
                        let wa = 1.0 - wb - wc;
                        if wa < -1e-4 || wb < -1e-4 || wc < -1e-4 {
                            continue;
                        }
                        let s = Vec2::new(a[2] * wa + b[2] * wb + c[2] * wc, a[3] * wa + b[3] * wb + c[3] * wc);
                        if let Some(v) = rows.get_mut((y - first) * w + x) {
                            *v = (*v).max(to_byte(self.sample(s)));
                        }
                    }
                }
            }
        });
        out
    }
}

/// The Gaussian's sigma (canvas pixels) for a blur `amount` (0..1), as feathering and the Gaussian
/// blur use it: it widens faster than the amount, from a fraction of a pixel to about 680.
pub fn feather_radius(amount: f32) -> f32 {
    let a = amount.clamp(0.0, 1.0);
    if a <= 0.0 {
        return 0.0;
    }
    let taps = ((a.sqrt() * 63.0).floor() + 1.0).min(64.0);
    let scale = (a * 32.0).floor().max(1.0);
    taps / 3.0 * scale
}

fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The closed edges of a polygon.
fn polygon_edges(points: &[Vec2]) -> Vec<(Vec2, Vec2)> {
    points.iter().zip(points.iter().cycle().skip(1)).map(|(a, b)| (*a, *b)).collect()
}

/// The rows a polygon reaches, as the full mask has always taken them.
fn polygon_rows(height: u32, points: &[Vec2]) -> (usize, usize) {
    let (lo, hi) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    (lo.max(0.0) as usize, (hi.max(0.0).ceil() as usize).min(height as usize))
}

/// Fills `out` (`x_hi - x_lo` per row) with the polygon's coverage, measuring every crossing in
/// world coordinates. `x_lo`/`x_hi` only choose which columns are kept; they never move a vertex.
fn polygon_raster(x_lo: usize, x_hi: usize, y_lo: usize, y_hi: usize, edges: &[(Vec2, Vec2)], out: &mut [u8]) {
    let width = x_hi - x_lo;
    if width == 0 || y_hi <= y_lo || out.len() != width * (y_hi - y_lo) {
        return;
    }
    by_bands(out, width, |first, rows| {
        let mut coverage = vec![0.0f32; width + 1];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        for (k, row) in rows.chunks_mut(width).enumerate() {
            let y = y_lo + first + k;
            coverage.iter_mut().for_each(|c| *c = 0.0);
            polygon_row_spans(y, edges, &mut crossings, |x0, x1| {
                add_span_between(&mut coverage, x0, x1, 1.0 / POLYGON_ROWS as f32, x_lo, x_hi);
            });
            for (v, c) in row.iter_mut().zip(&coverage) {
                *v = to_byte(*c);
            }
        }
    });
}

/// Walks the nonzero-winding spans of world row `y`, in crossing order, through `visit`.
/// Crossings are kept as the full raster keeps them, NaNs included and sorted by their bits'
/// order; only the spans themselves are judged. `crossings` is reused between rows.
fn polygon_row_spans(
    y: usize,
    edges: &[(Vec2, Vec2)],
    crossings: &mut Vec<(f32, i32)>,
    mut visit: impl FnMut(f32, f32),
) {
    for sub in 0..POLYGON_ROWS {
        let sy = y as f32 + (sub as f32 + 0.5) / POLYGON_ROWS as f32;
        crossings.clear();
        for &(a, b) in edges {
            if (a.y <= sy) != (b.y <= sy) {
                let x = a.x + (sy - a.y) / (b.y - a.y) * (b.x - a.x);
                crossings.push((x, if b.y > a.y { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
        let mut winding = 0;
        for pair in crossings.windows(2) {
            let [(x0, d), (x1, _)] = pair else { continue };
            winding += d;
            if winding != 0 {
                visit(*x0, *x1);
            }
        }
    }
}

/// The world columns and rows actually reached by nonzero-winding spans, clipped to the canvas.
/// Infinities clamp like the raster's spans do; NaN spans paint nothing and are skipped only after
/// the winding has counted them, exactly as [`polygon_raster`] does. The returned rows are the
/// actual painted rows, not merely the vertices' Y range.
fn polygon_span_bounds(
    width: usize,
    y_lo: usize,
    y_hi: usize,
    edges: &[(Vec2, Vec2)],
) -> Option<(usize, usize, usize, usize)> {
    let mut crossings = Vec::new();
    let (mut x_lo, mut x_hi) = (width, 0);
    let (mut actual_y_lo, mut actual_y_hi) = (y_hi, y_lo);
    for y in y_lo..y_hi {
        polygon_row_spans(y, edges, &mut crossings, |x0, x1| {
            let (a, b) = (x0.clamp(0.0, width as f32), x1.clamp(0.0, width as f32));
            if a.is_nan() || b.is_nan() || b <= a {
                return;
            }
            let first = (a.floor() as usize).min(width);
            let last = (b.ceil() as usize).min(width);
            if last > first {
                x_lo = x_lo.min(first);
                x_hi = x_hi.max(last);
                actual_y_lo = actual_y_lo.min(y);
                actual_y_hi = actual_y_hi.max(y + 1);
            }
        });
    }
    (x_hi > x_lo && actual_y_hi > actual_y_lo).then_some((x_lo, x_hi, actual_y_lo, actual_y_hi))
}

/// A span restricted to the world columns `[left, right)`, with the row starting at `left`.
fn add_span_between(row: &mut [f32], x0: f32, x1: f32, amount: f32, left: usize, right: usize) {
    let (lo, hi) = (left as f32, right as f32);
    let (x0, x1) = (x0.clamp(lo, hi), x1.clamp(lo, hi));
    if x0.is_nan() || x1.is_nan() || x1 <= x0 {
        return;
    }
    let (first, last) = (x0.floor() as usize, x1.floor() as usize);
    if first == last {
        if first >= left
            && first < right
            && let Some(pixel) = row.get_mut(first - left)
        {
            *pixel += (x1 - x0) * amount;
        }
        return;
    }
    if first >= left
        && first < right
        && let Some(pixel) = row.get_mut(first - left)
    {
        *pixel += (first as f32 + 1.0 - x0) * amount;
    }
    let begin = (first + 1).max(left);
    let end = last.min(right);
    if end > begin {
        for c in row.iter_mut().skip(begin - left).take(end - begin) {
            *c += amount;
        }
    }
    if last >= left
        && last < right
        && let Some(pixel) = row.get_mut(last - left)
    {
        *pixel += (x1 - last as f32) * amount;
    }
}

/// The smallest nonzero rectangle inside a rasterised patch and its rows.
fn trim_patch(x_lo: usize, y_lo: usize, width: usize, height: usize, coverage: Vec<u8>) -> Option<MaskPatch> {
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    for (y, row) in coverage.chunks(width.max(1)).enumerate().take(height) {
        let Some(first) = row.iter().position(|&v| v != 0) else { continue };
        let last = row.iter().rposition(|&v| v != 0).unwrap_or(first);
        x0 = x0.min(first);
        y0 = y0.min(y);
        x1 = x1.max(last + 1);
        y1 = y1.max(y + 1);
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut trimmed = vec![0u8; w * h];
    for (y, to) in trimmed.chunks_mut(w.max(1)).enumerate() {
        let from = (y0 + y) * width + x0;
        let Some(piece) = coverage.get(from..from + w) else { continue };
        to.copy_from_slice(piece);
    }
    Some(MaskPatch {
        rect: IRect::new(
            i32::try_from(x_lo + x0).ok()?,
            i32::try_from(y_lo + y0).ok()?,
            i32::try_from(x_lo + x1).ok()?,
            i32::try_from(y_lo + y1).ok()?,
        ),
        coverage: trimmed,
    })
}

/// How many columns [`blur_columns_into`] takes at a time: a strip the height of a big canvas
/// stays in a core's cache.
const STRIP: usize = 64;

/// Three box blurs `2r + 1` tall down `values`' columns (`w` by `h`; the top and bottom rows
/// repeated past them), rounded into `out`: the columns in bands, one to a core, each band a
/// strip at a time, so each blur works on what is in the cache.
fn blur_columns_into(values: &[f32], out: &mut [u8], w: usize, h: usize, r: usize) {
    let part = w.div_ceil(cores());
    // each band's columns: its piece of every row
    let mut bands: Vec<Vec<&mut [u8]>> = (0..w.div_ceil(part)).map(|_| Vec::with_capacity(h)).collect();
    for line in out.chunks_mut(w).take(h) {
        for (band, piece) in bands.iter_mut().zip(line.chunks_mut(part)) {
            band.push(piece);
        }
    }
    std::thread::scope(|scope| {
        for (k, mut pieces) in bands.into_iter().enumerate() {
            scope.spawn(move || {
                let (from, to) = (k * part, (k * part + part).min(w));
                let (mut strip, mut blurred, mut sums) = (Vec::new(), Vec::new(), Vec::new());
                for x0 in (from..to).step_by(STRIP) {
                    let x1 = (x0 + STRIP).min(to);
                    strip.clear();
                    for y in 0..h {
                        strip.extend_from_slice(values.get(y * w + x0..y * w + x1).unwrap_or_default());
                    }
                    for _ in 0..3 {
                        blur_strip(&strip, &mut blurred, &mut sums, x1 - x0, h, r);
                        std::mem::swap(&mut strip, &mut blurred);
                    }
                    for (piece, row) in pieces.iter_mut().zip(strip.chunks(x1 - x0)) {
                        let to = piece.get_mut(x0 - from..x1 - from).unwrap_or_default();
                        for (o, v) in to.iter_mut().zip(row) {
                            *o = v.round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            });
        }
    });
}

/// One box blur `2r + 1` tall down `src`, a strip `sw` wide and `h` tall (its rows one after
/// another), into `dst`: the columns' running sums side by side, kept in `sums`.
fn blur_strip(src: &[f32], dst: &mut Vec<f32>, sums: &mut Vec<f32>, sw: usize, h: usize, r: usize) {
    dst.clear();
    dst.resize(src.len(), 0.0);
    sums.clear();
    sums.resize(sw, 0.0);
    let row = |i: isize| src.get(i.clamp(0, h as isize - 1) as usize * sw..).and_then(|s| s.get(..sw)).unwrap_or(&[]);
    let span = (2 * r + 1) as f32;
    for d in -(r as isize)..=r as isize {
        for (s, v) in sums.iter_mut().zip(row(d)) {
            *s += v;
        }
    }
    for (i, out) in (0isize..).zip(dst.chunks_mut(sw.max(1))) {
        for (o, s) in out.iter_mut().zip(sums.iter()) {
            *o = s / span;
        }
        let (add, drop) = (row(i + r as isize + 1), row(i - r as isize));
        for ((s, a), d) in sums.iter_mut().zip(add).zip(drop) {
            *s += a - d;
        }
    }
}

/// `f(first row, its rows)` over bands of `data`'s rows (`width` values each), side by side.
fn by_bands<T: Send>(data: &mut [T], width: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let width = width.max(1);
    let band = (data.len() / width).div_ceil(cores()).max(1);
    std::thread::scope(|scope| {
        for (k, rows) in data.chunks_mut(band * width).enumerate() {
            let f = &f;
            scope.spawn(move || f(k * band, rows));
        }
    });
}

/// A box blur `2r + 1` wide along `row` (its ends repeated past them), with `line` to work in.
fn blur_row(row: &mut [f32], r: usize, line: &mut Vec<f32>) {
    // the row with its ends repeated `r` (and `r` + 1) further out, so the window never needs
    // bringing back inside it
    let (Some(&first), Some(&last)) = (row.first(), row.last()) else { return };
    line.clear();
    line.extend(std::iter::repeat_n(first, r));
    line.extend_from_slice(row);
    line.extend(std::iter::repeat_n(last, r + 1));
    let span = (2 * r + 1) as f32;
    let mut sum: f32 = line.get(..=2 * r).unwrap_or(&[]).iter().sum();
    let ahead = line.get(2 * r + 1..).unwrap_or(&[]);
    for ((v, a), d) in row.iter_mut().zip(ahead).zip(line.iter()) {
        *v = sum / span;
        sum += a - d;
    }
}

fn push_extremum(queue: &mut VecDeque<usize>, values: &[u8], index: usize, dilate: bool) {
    let Some(&value) = values.get(index) else { return };
    while let Some(&back) = queue.back() {
        let Some(&behind) = values.get(back) else { break };
        let replace = if dilate { behind <= value } else { behind >= value };
        if replace {
            queue.pop_back();
        } else {
            break;
        }
    }
    queue.push_back(index);
}

/// One separable max (dilate) or min (contract) pass, O(width * height): each line keeps a
/// monotonic deque of candidate indices, so a huge radius costs no more than a small one.
fn separable_extremum(data: &mut [u8], width: usize, height: usize, radius: usize, dilate: bool) {
    let mut horizontal = vec![0u8; width * height];
    let mut queue = VecDeque::with_capacity(width.max(height));
    for y in 0..height {
        let row = y * width;
        let Some(values) = data.get(row..row + width) else { break };
        let Some(out) = horizontal.get_mut(row..row + width) else { break };
        queue.clear();
        for x in 0..=radius.min(width - 1) {
            push_extremum(&mut queue, values, x, dilate);
        }
        for (x, cell) in out.iter_mut().enumerate() {
            while queue.front().is_some_and(|&index| index < x.saturating_sub(radius)) {
                queue.pop_front();
            }
            if x > 0 {
                let right = x + radius;
                if right < width {
                    push_extremum(&mut queue, values, right, dilate);
                }
            }
            let best = *queue.front().unwrap_or(&x);
            if let Some(&v) = values.get(best) {
                *cell = v;
            }
        }
    }
    let mut line = vec![0u8; height];
    for x in 0..width {
        for (y, value) in line.iter_mut().enumerate() {
            if let Some(&v) = horizontal.get(y * width + x) {
                *value = v;
            }
        }
        queue.clear();
        for y in 0..=radius.min(height - 1) {
            push_extremum(&mut queue, &line, y, dilate);
        }
        for y in 0..height {
            while queue.front().is_some_and(|&index| index < y.saturating_sub(radius)) {
                queue.pop_front();
            }
            if y > 0 {
                let bottom = y + radius;
                if bottom < height {
                    push_extremum(&mut queue, &line, bottom, dilate);
                }
            }
            let best = *queue.front().unwrap_or(&y);
            let Some(&v) = line.get(best) else { continue };
            if let Some(cell) = data.get_mut(y * width + x) {
                *cell = v;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// An automatic selection the plain way: the pixels joined to the seed through pixels whose
    /// colour, the four channels' differences added up, is within the threshold.
    fn selected(w: usize, h: usize, pixels: &[u8], seed: (usize, usize), threshold: f32) -> Vec<u8> {
        let limit = threshold.clamp(0.0, 1.0) * 2.0 * 255.0;
        let i = (seed.1 * w + seed.0) * 4;
        let target = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
        let near = |j: usize| {
            let p = &pixels[j * 4..j * 4 + 4];
            let apart: u32 = p.iter().zip(&target).map(|(a, b)| u32::from(a.abs_diff(*b))).sum();
            apart as f32 <= limit
        };
        let mut m = vec![0u8; w * h];
        let mut stack = vec![seed];
        while let Some((x, y)) = stack.pop() {
            let j = y * w + x;
            if m[j] != 0 || !near(j) {
                continue;
            }
            m[j] = 255;
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < w && ny < h {
                    stack.push((nx, ny));
                }
            }
        }
        m
    }

    /// What measuring a 4K picture from a seed costs, in bands side by side against one thread:
    /// run on a device with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn colour_flood_cost() {
        let (w, h) = (3840u32, 2160u32);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, (i % 7) as u8, 90, 255]).collect();
        for _ in 0..3 {
            let start = std::time::Instant::now();
            std::hint::black_box(ColourFlood::new(w, h, &px, (100, 100)));
            let bands = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            let target = [100u8, 2, 90, 255];
            let one: Vec<u16> = px
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p.iter().zip(&target).map(|(a, b)| u16::from(a.abs_diff(*b))).sum::<u16>().min(APART_MOST + 1))
                .collect();
            std::hint::black_box(one);
            println!("measured in bands {bands:.1} ms, on one thread {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    #[test]
    fn a_size_whose_byte_count_wraps_is_refused() {
        // the count and its byte size cannot be worked out: refuse instead of wrapping past them
        assert!(ColourFlood::new(u32::MAX, u32::MAX, &[], (0, 0)).is_none());
        let px = [0; 4];
        assert!(ColourFlood::new(1, 1, &px, (0, 0)).is_some());
    }

    #[test]
    fn an_automatic_selection_by_reach_or_flood_selects_what_a_plain_flood_does() {
        for seed in 0u32..40 {
            let mut state = seed;
            let mut next = |n: u32| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) % n.max(1)
            };
            let (w, h) = (8 + next(40), 8 + next(30));
            let colours: Vec<[u8; 4]> =
                (0..4).map(|_| [next(256) as u8, next(256) as u8, next(256) as u8, next(256) as u8]).collect();
            let mut px = vec![0u8; (w * h * 4) as usize];
            for p in px.chunks_mut(4) {
                let c = colours[next(4) as usize];
                p.copy_from_slice(&c.map(|v| v.saturating_add(next(24) as u8)));
            }
            let start = (next(w), next(h));
            let flood = ColourFlood::new(w, h, &px, start).unwrap();
            let top = APART_MOST + 1;
            let at = (start.0 as usize, start.1 as usize);
            let reach = crate::fill::reaches(&flood.apart, w as usize, h as usize, at, top, &|| true).unwrap();
            let _ = flood.reach.set(reach);
            for threshold in [0.0f32, 0.05, 0.1, 0.3, 0.5, 0.9, 1.0] {
                let plain = selected(w as usize, h as usize, &px, (start.0 as usize, start.1 as usize), threshold);
                let cut = (threshold * 2.0 * 255.0).floor() as u16;
                assert_eq!(flood.by_runs(cut).data, plain, "seed {seed} threshold {threshold}");
                let here = flood.by_scanlines(cut, usize::MAX).map(|m| m.data);
                assert_eq!(here, Some(plain.clone()), "seed {seed} threshold {threshold}");
                assert_eq!(flood.mask(threshold).data, plain, "seed {seed} threshold {threshold}");
            }
        }
    }

    /// How long feathering a 4K selection takes at a few amounts: run on a device with
    /// `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn feather_cost() {
        let (w, h) = (3840u32, 2160u32);
        let mut m = Mask::empty(w, h);
        for y in 200..1900 {
            for x in 300..3500 {
                m.data[(y * w + x) as usize] = 255;
            }
        }
        let mut room = Vec::new();
        for amount in [0.02f32, 0.1, 0.3, 0.6, 1.0] {
            let (mut each, mut kept) = (Vec::new(), Vec::new());
            for _ in 0..5 {
                let mut c = m.clone();
                let start = std::time::Instant::now();
                c.feather(feather_radius(amount));
                each.push(start.elapsed().as_secs_f64() * 1000.0);
                let mut c = m.clone();
                let start = std::time::Instant::now();
                c.feather_in(feather_radius(amount), &mut room);
                kept.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            each.sort_by(f64::total_cmp);
            kept.sort_by(f64::total_cmp);
            println!(
                "feather {amount} (radius {}): {:.1} ms, its room kept {:.1} ms",
                feather_radius(amount),
                each[2],
                kept[2]
            );
        }
    }

    /// How long an automatic selection over most of a 4K layer takes: run on a device with
    /// `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn by_colour_cost() {
        let (w, h) = (3840u32, 2160u32);
        // a soft gradient with a darker blob, so the threshold decides how far it spreads
        let mut px = vec![0u8; (w * h * 4) as usize];
        for (i, p) in px.chunks_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let v = (100 + x * 60 / w + y * 40 / h) as u8;
            let blob = (x as i32 - 1900).pow(2) + (y as i32 - 1000).pow(2) < 400 * 400;
            p.copy_from_slice(&if blob { [30, 30, 40, 255] } else { [v, v, v, 255] });
        }
        for threshold in [0.05f32, 0.2, 0.5] {
            let mut each = Vec::new();
            for _ in 0..5 {
                let start = std::time::Instant::now();
                let m = Mask::by_colour(w, h, &px, (100, 100), threshold);
                each.push(start.elapsed().as_secs_f64() * 1000.0);
                std::hint::black_box(m);
            }
            each.sort_by(f64::total_cmp);
            println!("by colour {threshold}: {:.1} ms (fastest {:.1})", each[2], each[0]);
        }
    }

    #[test]
    fn feathering_around_the_selection_alone_matches_feathering_it_all() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for (w, h, at, radius) in
            [(400u32, 300u32, (150u32, 120u32), 6.0f32), (500, 400, (2, 380), 11.0), (300, 300, (100, 100), 40.0)]
        {
            let mut m = Mask::empty(w, h);
            // a blotch of random coverage, and a few lone pixels near it
            for y in at.1..(at.1 + 40).min(h) {
                for x in at.0..(at.0 + 60).min(w) {
                    m.data[(y * w + x) as usize] = (next() % 256) as u8;
                }
            }
            let mut whole = m.clone();
            whole.feather_all(radius.round() as usize, &mut Vec::new());
            m.feather(radius);
            assert!(m.data == whole.data, "{w}x{h} at {at:?} radius {radius}");
        }
    }

    #[test]
    fn a_colour_selection_joins_sides_not_corners() {
        // W white, B black: the top left white touches the rest only at a corner
        let rows = ["WBWWW", "BWWBW", "WWBBW", "WWWWW"];
        let px: Vec<u8> = rows
            .iter()
            .flat_map(|r| r.bytes())
            .flat_map(|c| if c == b'W' { [255, 255, 255, 255] } else { [0, 0, 0, 255] })
            .collect();
        let count = |m: &Mask| m.data.iter().filter(|&&v| v == 255).count();
        assert_eq!(count(&Mask::by_colour(5, 4, &px, (0, 0), 0.0)), 1);
        // the rest of the white, round the black on both sides and below it
        let m = Mask::by_colour(5, 4, &px, (2, 0), 0.0);
        assert_eq!(count(&m), 14);
        assert_eq!(m.data.first(), Some(&0));
        assert_eq!(m.data.get(2 * 5), Some(&255));
    }

    #[test]
    fn bounds_hold_what_a_mask_takes() {
        let mut m = Mask::empty(10, 8);
        assert_eq!(m.bounds(), None);
        if let Some(v) = m.data.get_mut(3 * 10 + 2) {
            *v = 9;
        }
        if let Some(v) = m.data.get_mut(5 * 10 + 7) {
            *v = 255;
        }
        assert_eq!(m.bounds(), Some([2, 3, 6, 3]));
    }

    fn at(m: &Mask, x: u32, y: u32) -> u8 {
        m.data[(y * m.width + x) as usize]
    }

    #[test]
    fn a_rectangle_covers_its_pixels_and_part_of_its_edges() {
        let m = Mask::rect(10, 10, Vec2::new(7.5, 8.0), Vec2::new(2.0, 2.0));
        assert_eq!(at(&m, 2, 2), 255);
        assert_eq!(at(&m, 7, 5), 128);
        assert_eq!(at(&m, 8, 5), 0);
        assert_eq!(at(&m, 1, 1), 0);
        assert!(!m.is_empty());
        assert!(Mask::empty(4, 4).is_empty());
    }

    #[test]
    fn an_ellipse_is_round_with_soft_edges() {
        let m = Mask::ellipse(40, 40, Vec2::new(0.5, 0.5), Vec2::new(39.5, 19.5));
        assert_eq!(at(&m, 20, 10), 255);
        assert_eq!(at(&m, 1, 1), 0);
        assert_eq!(at(&m, 20, 25), 0);
        // the pixel the end of the long axis runs through is about half covered
        let edge = at(&m, 39, 9);
        assert!(edge > 40 && edge < 220, "{edge}");
    }

    #[test]
    fn a_polygon_fills_by_the_non_zero_rule() {
        let tri = [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(0.0, 10.0)];
        let m = Mask::polygon(12, 12, &tri);
        assert_eq!(at(&m, 1, 1), 255);
        assert_eq!(at(&m, 8, 8), 0);
        // a pixel the diagonal cuts in half
        let half = at(&m, 4, 5);
        assert!((100..=155).contains(&half), "{half}");
        // a square traced twice the same way stays filled
        let sq = [Vec2::new(2.0, 2.0), Vec2::new(8.0, 2.0), Vec2::new(8.0, 8.0), Vec2::new(2.0, 8.0)];
        let twice: Vec<Vec2> = sq.iter().chain(&sq).copied().collect();
        assert_eq!(at(&Mask::polygon(12, 12, &twice), 5, 5), 255);
        assert!(Mask::polygon(12, 12, &tri[..2]).is_empty());
    }

    #[test]
    fn expand_and_contract_keep_fractional_coverage_and_truncate_edges() {
        // A single half-covered pixel expands as 128, not as a thresholded 255.
        let mut soft = Mask::empty(5, 1);
        soft.data[2] = 128;
        soft.expand_contract(2);
        assert_eq!(soft.data, vec![128; 5]);
        soft.expand_contract(-3);
        assert_eq!(soft.data, vec![128; 5], "uniform partial coverage stays partial");

        // A selection touching the left edge stays there when contracted: the
        // window is clipped, not padded with zeros outside the canvas.
        let mut edge = Mask::empty(7, 7);
        for y in 1..6 {
            for x in 0..3 {
                edge.data[y * 7 + x] = 255;
            }
        }
        edge.expand_contract(-1);
        assert_eq!(at(&edge, 0, 2), 255, "the clipped edge is not eroded");
        assert_eq!(at(&edge, 2, 2), 0, "the interior edge is eroded");

        let mut grown = Mask::empty(7, 7);
        for y in 1..6 {
            for x in 0..3 {
                grown.data[y * 7 + x] = 255;
            }
        }
        grown.expand_contract(1);
        assert_eq!(at(&grown, 3, 2), 255, "one pixel further");
        assert_eq!(at(&grown, 4, 2), 0, "but only one");
    }

    fn brute_expand_contract(mask: &Mask, amount: i32) -> Mask {
        let mut out = mask.clone();
        if amount == 0 {
            return out;
        }
        // i64 makes i32::MIN safe; saturating bounds make huge radii full-canvas.
        let radius = i64::from(amount).unsigned_abs() as usize;
        let dilate = amount > 0;
        let (w, h) = (mask.width as usize, mask.height as usize);
        for y in 0..h {
            for x in 0..w {
                let top = y.saturating_sub(radius);
                let bottom = y.saturating_add(radius).min(h.saturating_sub(1));
                let left = x.saturating_sub(radius);
                let right = x.saturating_add(radius).min(w.saturating_sub(1));
                let mut value = if dilate { 0 } else { 255 };
                for sy in top..=bottom {
                    for sx in left..=right {
                        let candidate = mask.data[sy * w + sx];
                        value = if dilate { value.max(candidate) } else { value.min(candidate) };
                    }
                }
                out.data[y * w + x] = value;
            }
        }
        out
    }

    #[test]
    fn expand_contract_matches_a_bruteforce_square_window() {
        let coverage = [0u8, 1, 64, 128, 254, 255];
        let sizes = [(1usize, 1usize), (1, 7), (7, 1), (7, 5), (9, 4)];
        let amounts = [0, 1, -1, 2, -2, 5, -5, 7, -7, 100, -100, i32::MAX, i32::MIN];
        for (w, h) in sizes {
            let data: Vec<u8> = (0..w * h).map(|i| coverage[i % coverage.len()]).collect();
            let original = Mask { width: w as u32, height: h as u32, data };
            for amount in amounts {
                let mut got = original.clone();
                got.expand_contract(amount);
                let want = brute_expand_contract(&original, amount);
                assert_eq!(
                    got.data, want.data,
                    "mismatch at {w}x{h}, amount {amount}: got {:?}, want {:?}",
                    got.data, want.data
                );
            }
        }
    }

    #[test]
    fn expand_and_contract_noop_and_extremes_are_safe() {
        let mut m = Mask::rect(9, 9, Vec2::new(2.0, 2.0), Vec2::new(7.0, 7.0));
        let before = m.clone();
        m.expand_contract(0);
        assert_eq!(m, before);
        m.expand_contract(i32::MIN);
        assert!(m.is_empty(), "contracting by i32::MIN takes everything");
        m.expand_contract(i32::MAX);
        assert_eq!(m.data.iter().filter(|&&v| v != 0).count(), 0, "empty stays empty");
        let mut full = Mask::empty(4, 4);
        full.data.fill(200);
        full.expand_contract(i32::MAX);
        assert!(full.data.iter().all(|&v| v == 200), "full stays full");
    }

    #[test]
    fn adding_removing_inverting_and_feathering() {
        let mut m = Mask::rect(20, 20, Vec2::new(0.0, 0.0), Vec2::new(10.0, 20.0));
        m.combine(&Mask::rect(20, 20, Vec2::new(10.0, 0.0), Vec2::new(15.0, 20.0)), Combine::Add);
        assert_eq!(at(&m, 12, 5), 255);
        m.combine(&Mask::rect(20, 20, Vec2::new(0.0, 0.0), Vec2::new(20.0, 5.0)), Combine::Remove);
        assert_eq!(at(&m, 2, 2), 0);
        assert_eq!(at(&m, 2, 7), 255);
        m.invert();
        assert_eq!(at(&m, 2, 2), 255);
        assert_eq!(at(&m, 2, 7), 0);
        let mut f = Mask::rect(40, 40, Vec2::new(10.0, 10.0), Vec2::new(30.0, 30.0));
        f.feather(3.0);
        assert_eq!(at(&f, 20, 20), 255);
        let edge = at(&f, 10, 20);
        assert!(edge > 60 && edge < 200, "{edge}");
        assert!(at(&f, 6, 20) < at(&f, 10, 20));
    }

    #[test]
    fn a_selection_moves_with_its_content() {
        let m = Mask::rect(20, 20, Vec2::new(0.0, 0.0), Vec2::new(5.0, 5.0));
        // 10 right: each pixel looks 10 left
        let moved = m.moved(|p| Some(p - Vec2::new(10.0, 0.0)));
        assert_eq!(at(&moved, 12, 2), 255);
        assert_eq!(at(&moved, 2, 2), 0);
        // the same through two triangles
        let c = |x: f32, y: f32| [x + 10.0, y, x, y];
        let mesh = [c(0.0, 0.0), c(20.0, 0.0), c(20.0, 20.0), c(0.0, 0.0), c(20.0, 20.0), c(0.0, 20.0)];
        let warped = m.warped(&mesh);
        assert_eq!(at(&warped, 12, 2), 255);
        assert_eq!(at(&warped, 2, 2), 0);
    }

    #[test]
    fn the_feather_widens_with_its_amount() {
        assert_eq!(feather_radius(0.0), 0.0);
        assert!((feather_radius(0.52) - 245.3).abs() < 0.5, "{}", feather_radius(0.52));
        assert!(feather_radius(0.1) < feather_radius(0.2) && feather_radius(1.0) > 650.0);
    }

    #[test]
    fn colour_selection_floods_what_matches_and_is_joined() {
        // a 6 × 1 strip: red, red, blue, red, red, red
        let red = [255u8, 0, 0, 255];
        let blue = [0u8, 0, 255, 255];
        let px: Vec<u8> = [red, red, blue, red, red, red].concat();
        let m = Mask::by_colour(6, 1, &px, (0, 0), 0.1);
        assert_eq!(m.data, vec![255, 255, 0, 0, 0, 0]);
        let all = Mask::by_colour(6, 1, &px, (0, 0), 1.0);
        assert!(all.data.iter().all(|&v| v == 255));
        assert!(Mask::by_colour(6, 1, &px, (9, 0), 0.5).is_empty());
    }
    fn patch_matches_full(width: u32, height: u32, points: &[Vec2]) {
        let full = Mask::polygon(width, height, points);
        let patch = Mask::polygon_patch(width, height, points);
        match full.bounds() {
            None => assert!(patch.is_none(), "{points:?}: an empty polygon has no patch"),
            Some([x, y, w, h]) => {
                let patch = patch.expect("a reaching polygon has a patch");
                assert_eq!(patch.rect, IRect::new(x as i32, y as i32, (x + w) as i32, (y + h) as i32), "{points:?}");
                assert_eq!(patch.coverage.len(), (w * h) as usize, "{points:?}");
                for py in 0..h as usize {
                    for px in 0..w as usize {
                        assert_eq!(
                            patch.coverage[py * w as usize + px],
                            full.data[(y as usize + py) * width as usize + x as usize + px],
                            "{points:?} at {px},{py}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_polygon_patch_is_filled_at_its_world_position() {
        let triangle = [Vec2::new(2.3, 2.7), Vec2::new(9.8, 3.1), Vec2::new(4.2, 10.6)];
        let patch = Mask::polygon_patch(20, 20, &triangle).expect("the triangle reaches pixels");
        assert!(patch.rect.x0 >= 2 && patch.rect.y0 >= 2, "{:?}", patch.rect);
        assert!(!patch.coverage.is_empty());
        patch_matches_full(20, 20, &triangle);
    }

    #[test]
    fn a_polygon_patch_matches_the_full_mask_byte_for_byte() {
        let cases: [&[Vec2]; 6] = [
            &[Vec2::new(2.3, 2.7), Vec2::new(9.8, 3.1), Vec2::new(4.2, 10.6)],
            &[
                Vec2::new(10.0, 2.0),
                Vec2::new(17.0, 10.0),
                Vec2::new(3.0, 10.0),
                Vec2::new(16.0, 3.0),
                Vec2::new(4.0, 3.0),
            ],
            &[Vec2::new(-5.0, -5.0), Vec2::new(25.0, -2.0), Vec2::new(8.0, 30.0)],
            &[Vec2::new(2.1, 2.1), Vec2::new(2.9, 2.1), Vec2::new(2.1, 2.9)],
            &[Vec2::new(-1.0e30, -1.0e30), Vec2::new(1.0e30, 2.0), Vec2::new(2.0, 1.0e30)],
            &[Vec2::new(1.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 1.0)],
        ];
        for points in cases {
            patch_matches_full(20, 20, points);
        }
    }

    #[test]
    fn polygon_span_bounds_tracks_the_rows_the_polygon_actually_reaches() {
        // A full-width, two-row rectangle with a long retraced tail down x=0: only y=10..12 paints.
        let points = [
            Vec2::new(0.0, 10.0),
            Vec2::new(100.0, 10.0),
            Vec2::new(100.0, 12.0),
            Vec2::new(0.0, 12.0),
            Vec2::new(0.0, 100.0),
            Vec2::new(0.0, 12.0),
            Vec2::new(0.0, 10.0),
        ];
        let edges = polygon_edges(&points);
        let (_, _, y0, y1) = polygon_span_bounds(100, 10, 100, &edges).expect("the rectangle reaches pixels");
        assert_eq!((y0, y1), (10, 12), "the retraced tail must not enlarge the row allocation");
        patch_matches_full(100, 100, &points);
    }

    #[test]
    fn a_polygon_patch_keeps_a_crossing_beyond_its_vertex_x_bounds() {
        // The edge from 1e30 back to 100 cancels to x=0 in f32, though every vertex is at x>=100.
        let points = [Vec2::new(100.0, -1.0), Vec2::new(1.0e30, -1.0e30), Vec2::new(100.0, 2.0)];
        let full = Mask::polygon(20, 20, &points);
        assert_eq!(full.data[0], 255, "the full raster reaches the canvas's first pixel");
        patch_matches_full(20, 20, &points);
    }

    #[test]
    fn a_polygon_patch_keeps_a_valid_clamped_infinite_span() {
        // Finite vertices, an overflowing x difference, and a span whose ends clamp to the canvas.
        let points = [Vec2::new(-f32::MAX, 0.0), Vec2::new(f32::MAX, 2.0), Vec2::new(0.0, 2.0)];
        let full = Mask::polygon(20, 20, &points);
        assert_eq!(full.data[0], 255, "the clamped span reaches the first pixel");
        patch_matches_full(20, 20, &points);
    }

    #[test]
    fn a_polygon_patch_keeps_left_pixels_before_a_nonempty_vertex_x_bbox() {
        // Here the vertex bbox starts at 100 and is nonempty, but the raster still fills from x=0.
        let points = [Vec2::new(100.0, -1.0), Vec2::new(1.0e30, -1.0e30), Vec2::new(200.0, 2.0)];
        let full = Mask::polygon(300, 20, &points);
        assert_eq!(full.data[0], 255, "the full raster reaches x=0");
        assert_eq!(full.data[99], 255, "the lost left part reaches into the vertex bbox");
        patch_matches_full(300, 20, &points);
    }

    #[test]
    fn a_polygon_patch_leaves_the_canvas_unmaterialised() {
        let (width, height) = (4096u32, 4096u32);
        let triangle = [Vec2::new(100.0, 100.0), Vec2::new(110.0, 100.0), Vec2::new(100.0, 110.0)];
        let patch = Mask::polygon_patch(width, height, &triangle).expect("a patch");
        assert!(patch.rect.width() <= 12 && patch.rect.height() <= 12, "{:?}", patch.rect);
        assert!(patch.coverage.len() < 200, "only the patch, not {} canvas bytes", width * height);
    }

    const PRE_REFACTOR_GOLDENS: &[(u32, u32, &str, u64)] = &[
        (1, 1, "fractional", 0xaf63c04c8601bcf8),
        (1, 1, "outside", 0xaf64954c860326e7),
        (1, 1, "self_intersect", 0xaf639b4c86017e19),
        (1, 1, "repeated", 0xaf63d04c8601d828),
        (1, 1, "one_pixel", 0xaf64144c86024bb4),
        (1, 1, "huge_max", 0xaf64814c860304eb),
        (1, 1, "degenerate", 0xaf63bd4c8601b7df),
        (1, 1, "lcg", 0xaf63fd4c8602249f),
        (1, 1, "lcg", 0xaf63c64c8601c72a),
        (1, 1, "lcg", 0xaf64724c8602eb6e),
        (1, 1, "lcg", 0xaf64724c8602eb6e),
        (17, 13, "fractional", 0x83d30c1f5cc34a97),
        (17, 13, "outside", 0x02fb8d1aba544f10),
        (17, 13, "self_intersect", 0x9fb62dad4fb862b3),
        (17, 13, "repeated", 0x57a20abaebcd1d0b),
        (17, 13, "one_pixel", 0xe76c941877f9eb39),
        (17, 13, "huge_max", 0x7ee46d56c920d54b),
        (17, 13, "degenerate", 0xfc774b5bccc629ef),
        (17, 13, "lcg", 0x05046f86376dfc12),
        (17, 13, "lcg", 0x64258b740c25cf8c),
        (17, 13, "lcg", 0x64f175160f7e46b9),
        (17, 13, "lcg", 0xd9ddc52ab678b18e),
        (20, 20, "fractional", 0x0f17cba3fafd6d99),
        (20, 20, "outside", 0x31e971d43fad5ded),
        (20, 20, "self_intersect", 0x572cdee12cd1d346),
        (20, 20, "repeated", 0x9c9477ca372f4a60),
        (20, 20, "one_pixel", 0x779de2526d6fdcac),
        (20, 20, "huge_max", 0x5b90a087e23cbb25),
        (20, 20, "degenerate", 0x2c1b93daafb34265),
        (20, 20, "lcg", 0xfde7252d2c960454),
        (20, 20, "lcg", 0x949c84027c8f3a8b),
        (20, 20, "lcg", 0xf7760edce4e32a00),
        (20, 20, "lcg", 0x3fd2a330c46c8506),
        (100, 100, "fractional", 0xfe2b8c2b05eb42f1),
        (100, 100, "outside", 0xfef4dc70429eafbb),
        (100, 100, "self_intersect", 0xd870bb9329dee9b6),
        (100, 100, "repeated", 0x9db98ba9dcbf1e0c),
        (100, 100, "one_pixel", 0x746136fcdf9d008c),
        (100, 100, "huge_max", 0xd607988619fedb25),
        (100, 100, "degenerate", 0xa6e4f0723147f065),
        (100, 100, "lcg", 0x0f55f8ee981531d6),
        (100, 100, "lcg", 0x96534a8403940240),
        (100, 100, "lcg", 0x487a78fe97c717cd),
        (100, 100, "lcg", 0x19f2a32eb22d3729),
    ];

    fn fnv64(data: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for &byte in data {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    fn golden_cases(w: u32, h: u32) -> Vec<(&'static str, Vec<Vec2>)> {
        let s = w.max(h) as f32;
        let normalized =
            |points: &[(f32, f32)]| -> Vec<Vec2> { points.iter().map(|&(x, y)| Vec2::new(x * s, y * s)).collect() };
        let mut out = vec![
            ("fractional", normalized(&[(0.13, 0.14), (0.55, 0.16), (0.24, 0.58)])),
            ("outside", normalized(&[(-0.20, -0.10), (1.30, 0.05), (0.40, 1.40)])),
            ("self_intersect", normalized(&[(0.50, 0.10), (0.90, 0.50), (0.10, 0.50), (0.80, 0.15), (0.20, 0.15)])),
            (
                "repeated",
                normalized(&[
                    (0.15, 0.15),
                    (0.60, 0.15),
                    (0.60, 0.60),
                    (0.15, 0.60),
                    (0.15, 0.15),
                    (0.60, 0.15),
                    (0.60, 0.60),
                    (0.15, 0.60),
                ]),
            ),
            ("one_pixel", normalized(&[(0.30, 0.30), (0.30 + 0.80 / s, 0.30), (0.30, 0.30 + 0.80 / s)])),
            (
                "huge_max",
                vec![Vec2::new(-f32::MAX, -f32::MAX), Vec2::new(f32::MAX, 0.1 * s), Vec2::new(0.2 * s, f32::MAX)],
            ),
            ("degenerate", normalized(&[(0.50, 0.50), (0.50, 0.50), (0.50, 0.50)])),
        ];
        let mut lcg_state = 0x9e37_79b9_7f4a_7c15u64 ^ (u64::from(w) << 32) ^ u64::from(h);
        let mut lcg = || {
            lcg_state = lcg_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            lcg_state
        };
        for _ in 0..4 {
            let count = 5 + (lcg() % 4) as usize;
            let points = (0..count)
                .map(|_| {
                    let x = (lcg() % 10_000) as f32 / 10_000.0 * (w as f32 + 20.0) - 10.0;
                    let y = (lcg() % 10_000) as f32 / 10_000.0 * (h as f32 + 20.0) - 10.0;
                    Vec2::new(x, y)
                })
                .collect();
            out.push(("lcg", points));
        }
        out
    }

    #[test]
    fn full_and_patch_match_the_old_main_goldens() {
        let mut cases = Vec::new();
        for (w, h) in [(1u32, 1u32), (17, 13), (20, 20), (100, 100)] {
            for (name, points) in golden_cases(w, h) {
                cases.push((w, h, name, points));
            }
        }
        assert_eq!(cases.len(), PRE_REFACTOR_GOLDENS.len());
        for ((w, h, name, points), &(gw, gh, gname, want)) in cases.iter().zip(PRE_REFACTOR_GOLDENS) {
            assert_eq!((*w, *h, *name), (gw, gh, gname));
            let full = Mask::polygon(*w, *h, points);
            assert_eq!(fnv64(&full.data), want, "{w}x{h} {name}: new full mask");
            let patch = Mask::polygon_patch(*w, *h, points);
            let mut rebuilt = Mask::empty(*w, *h);
            if let Some(patch) = patch {
                let [x, y, pw, ph] = full.bounds().expect("the full mask has pixels");
                assert_eq!(patch.rect, IRect::new(x as i32, y as i32, (x + pw) as i32, (y + ph) as i32));
                for py in 0..ph as usize {
                    let from = py * pw as usize;
                    let to = (y as usize + py) * *w as usize + x as usize;
                    rebuilt.data[to..to + pw as usize].copy_from_slice(&patch.coverage[from..from + pw as usize]);
                }
            }
            assert_eq!(rebuilt.data, full.data, "{w}x{h} {name}: patch re-expanded");
            assert_eq!(fnv64(&rebuilt.data), want, "{w}x{h} {name}: patch golden");
        }
    }

    #[test]
    fn a_span_ending_in_nan_never_underflows_or_writes_the_row() {
        let mut row = [0.0f32; 4];
        add_span_between(&mut row, 5.0, f32::NAN, 1.0, 2, 4);
        assert_eq!(row, [0.0; 4], "an invalid crossing changes nothing");
    }

    #[test]
    fn a_huge_patch_trimmed_by_a_tiny_fractional_clip_matches_the_legacy_intersection() {
        let (w, h) = (512u32, 512u32);
        let full_shape =
            [Vec2::new(0.0, 0.0), Vec2::new(w as f32, 0.0), Vec2::new(w as f32, h as f32), Vec2::new(0.0, h as f32)];
        let patch = Mask::polygon_patch(w, h, &full_shape).expect("the full shape has a patch");
        assert_eq!(patch.coverage.len(), (w * h) as usize, "the huge patch starts whole");
        // Tiny clip, including 0 and 1 at its edges: zeros must be trimmed, 1 must survive.
        let mut clip = vec![0u8; (w * h) as usize];
        let values = [0u8, 1, 128, 255];
        for y in 250..253usize {
            for x in 240..245usize {
                clip[y * w as usize + x] = values[(x + y) % values.len()];
            }
        }
        let mut clipped = patch.coverage.clone();
        for (py, row) in clipped.chunks_mut(w as usize).enumerate() {
            for (px, value) in row.iter_mut().enumerate() {
                let at = py * w as usize + px;
                *value = ((u16::from(*value) * u16::from(clip[at]) + 127) / 255) as u8;
            }
        }
        let trimmed = MaskPatch { rect: patch.rect, coverage: clipped }.trim().expect("a nonzero intersection");
        // The legacy path clips the full mask and asks for its nonzero bounds + that part.
        let mut legacy = Mask::polygon(w, h, &full_shape);
        for (a, &b) in legacy.data.iter_mut().zip(&clip) {
            *a = ((u16::from(*a) * u16::from(b) + 127) / 255) as u8;
        }
        let [x, y, lw, lh] = legacy.bounds().expect("the tiny clip reaches pixels");
        let legacy_part: Vec<u8> = (y..y + lh)
            .flat_map(|row| legacy.data[(row * w + x) as usize..(row * w + x + lw) as usize].to_vec())
            .collect();
        assert_eq!(trimmed.rect, IRect::new(x as i32, y as i32, (x + lw) as i32, (y + lh) as i32));
        assert_eq!(trimmed.coverage, legacy_part);
        assert!(trimmed.coverage.len() < patch.coverage.len(), "the tiny ROI is kept");
    }
}
