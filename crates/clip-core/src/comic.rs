//! Comic primitives: speech balloons, frame borders, panel division and
//! screentones.
//!
//! These are plain raster operations on an [`Image`] so they work on any paint
//! layer, in `.clip` and `.kra` documents alike. CLIP STUDIO PAINT's own balloon
//! and frame layers keep an editable vector description plus a cached render;
//! Sumi draws the pixels and keeps the shape parameters in the undoable draw
//! call, which is what the editor needs to offer the tools.

use crate::render::Image;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform};

/// An axis-aligned rectangle in document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w: w.max(0.0), h: h.max(0.0) }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect::new(
            self.x + amount,
            self.y + amount,
            (self.w - amount * 2.0).max(0.0),
            (self.h - amount * 2.0).max(0.0),
        )
    }
}

/// Balloon outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalloonShape {
    Ellipse,
    /// Speech balloon with a rounded body, as CSP's default.
    Rounded,
}

/// A speech balloon with an optional triangular tail.
#[derive(Clone, Debug)]
pub struct Balloon {
    pub shape: BalloonShape,
    pub bounds: Rect,
    /// Tail tip; the tail is a triangle from the balloon rim to this point.
    pub tail: Option<(f32, f32)>,
    pub fill: [u8; 4],
    /// Outline colour and width in pixels; zero width disables the outline.
    pub stroke: Option<([u8; 4], f32)>,
}

impl Balloon {
    pub fn new(bounds: Rect, fill: [u8; 4]) -> Self {
        Balloon { shape: BalloonShape::Rounded, bounds, tail: None, fill, stroke: None }
    }
}

fn paint_of(color: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint.anti_alias = true;
    paint.shader = paint.shader.clone();
    Paint {
        shader: Shader::SolidColor(tiny_skia::Color::from_rgba8(
            color[0], color[1], color[2], color[3],
        )),
        anti_alias: true,
        ..paint
    }
}

/// Composite a straight-alpha source image over the target at `(x, y)`.
pub fn composite(target: &mut Image, source: &Image, x: i32, y: i32) {
    for sy in 0..source.height as i32 {
        let ty = y + sy;
        if ty < 0 || ty >= target.height as i32 {
            continue;
        }
        for sx in 0..source.width as i32 {
            let tx = x + sx;
            if tx < 0 || tx >= target.width as i32 {
                continue;
            }
            let source_index = ((sy as u32 * source.width + sx as u32) * 4) as usize;
            let target_index = ((ty as u32 * target.width + tx as u32) * 4) as usize;
            let alpha = source.data[source_index + 3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            for channel in 0..4 {
                let top = source.data[source_index + channel] as f32;
                let bottom = target.data[target_index + channel] as f32;
                let value = top * alpha + bottom * (1.0 - alpha);
                target.data[target_index + channel] = value.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Draw a balloon (body plus tail) into `target` at document coordinates.
pub fn draw_balloon(target: &mut Image, balloon: &Balloon) {
    let bounds = balloon.bounds;
    // The tail tip may sit well outside the body, so the working surface has to
    // cover both or the tail is clipped at the body edge.
    let mut min_x = bounds.x;
    let mut min_y = bounds.y;
    let mut max_x = bounds.x + bounds.w;
    let mut max_y = bounds.y + bounds.h;
    if let Some((tip_x, tip_y)) = balloon.tail {
        min_x = min_x.min(tip_x);
        min_y = min_y.min(tip_y);
        max_x = max_x.max(tip_x);
        max_y = max_y.max(tip_y);
    }
    let stroke_pad = balloon.stroke.map_or(0.0, |(_, width)| width.ceil());
    let origin_x = (min_x - stroke_pad).floor();
    let origin_y = (min_y - stroke_pad).floor();
    let width = ((max_x + stroke_pad).ceil() - origin_x + 1.0).max(1.0) as u32;
    let height = ((max_y + stroke_pad).ceil() - origin_y + 1.0).max(1.0) as u32;
    let Some(mut pixmap) = Pixmap::new(width, height) else {
        return;
    };
    let left = bounds.x - origin_x;
    let top = bounds.y - origin_y;
    let mut builder = PathBuilder::new();
    match balloon.shape {
        BalloonShape::Ellipse => {
            if let Some(oval) =
                tiny_skia::Rect::from_xywh(left, top, bounds.w.max(1.0), bounds.h.max(1.0))
            {
                builder.push_oval(oval);
            }
        }
        BalloonShape::Rounded => {
            let radius = Radius::of(balloon.shape, bounds).0;
            let radius = radius.min(bounds.w * 0.5).min(bounds.h * 0.5);
            push_round_rect(&mut builder, left, top, bounds.w, bounds.h, radius);
        }
    }
    let body = builder.finish();
    if let Some((tip_x, tip_y)) = balloon.tail {
        let mut tail = PathBuilder::new();
        // A tail from two points on the lower rim to the tip.
        let rim_y = top + bounds.h * 0.85;
        tail.move_to(left + bounds.w * 0.30, rim_y);
        tail.line_to(tip_x - origin_x, tip_y - origin_y);
        tail.line_to(left + bounds.w * 0.62, rim_y);
        tail.close();
        if let Some(tail_path) = tail.finish() {
            paint_path(&mut pixmap, &tail_path, balloon);
        }
    }
    if let Some(body) = body {
        paint_path(&mut pixmap, &body, balloon);
    }
    let surface = pixmap_to_image(&pixmap);
    composite(target, &surface, origin_x as i32, origin_y as i32);
}

fn paint_path(pixmap: &mut Pixmap, path: &tiny_skia::Path, balloon: &Balloon) {
    pixmap.fill_path(
        path,
        &paint_of(balloon.fill),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    if let Some((color, width)) = balloon.stroke {
        if width > 0.0 {
            pixmap.stroke_path(
                path,
                &paint_of(color),
                &Stroke { width, ..Stroke::default() },
                Transform::identity(),
                None,
            );
        }
    }
}

struct Radius(f32);

impl Radius {
    fn of(shape: BalloonShape, bounds: Rect) -> Radius {
        match shape {
            BalloonShape::Ellipse => Radius(0.0),
            BalloonShape::Rounded => Radius(bounds.w.min(bounds.h) * 0.18),
        }
    }
}

fn push_round_rect(builder: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.max(0.0).min(w * 0.5).min(h * 0.5);
    builder.move_to(x + r, y);
    builder.line_to(x + w - r, y);
    builder.quad_to(x + w, y, x + w, y + r);
    builder.line_to(x + w, y + h - r);
    builder.quad_to(x + w, y + h, x + w - r, y + h);
    builder.line_to(x + r, y + h);
    builder.quad_to(x, y + h, x, y + h - r);
    builder.line_to(x, y + r);
    builder.quad_to(x, y, x + r, y);
    builder.close();
}

/// Draw a rectangular frame border of `thickness` pixels around `bounds`.
pub fn draw_frame(target: &mut Image, bounds: Rect, thickness: f32, color: [u8; 4]) {
    let thickness = thickness.max(0.5);
    let outer = bounds;
    let inner = bounds.inset(thickness);
    let (x0, y0) = (outer.x.floor() as i64, outer.y.floor() as i64);
    let (x1, y1) = (
        (outer.x + outer.w).ceil() as i64,
        (outer.y + outer.h).ceil() as i64,
    );
    for y in y0..y1 {
        for x in x0..x1 {
            if x < 0 || y < 0 || x >= target.width as i64 || y >= target.height as i64 {
                continue;
            }
            let inside_inner = inner.contains(x as f32 + 0.5, y as f32 + 0.5);
            if inside_inner {
                continue;
            }
            let index = ((y as u32 * target.width + x as u32) * 4) as usize;
            target.data[index..index + 4].copy_from_slice(&color);
        }
    }
}

/// Split `bounds` into a `rows` by `cols` grid separated by `gutter` pixels.
pub fn divide_panels(bounds: Rect, rows: u32, cols: u32, gutter: f32) -> Vec<Rect> {
    let rows = rows.max(1);
    let cols = cols.max(1);
    let gutter = gutter.max(0.0);
    let cell_w = (bounds.w - gutter * (cols - 1) as f32) / cols as f32;
    let cell_h = (bounds.h - gutter * (rows - 1) as f32) / rows as f32;
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return Vec::new();
    }
    let mut panels = Vec::with_capacity((rows * cols) as usize);
    for row in 0..rows {
        for col in 0..cols {
            panels.push(Rect::new(
                bounds.x + col as f32 * (cell_w + gutter),
                bounds.y + row as f32 * (cell_h + gutter),
                cell_w,
                cell_h,
            ));
        }
    }
    panels
}

/// Draw a screentone inside `bounds`. `angle_deg` rotates the dot lattice,
/// `spacing` is the lattice pitch in pixels, `dot_ratio` the dot diameter as a
/// fraction of the pitch (0..=1) and `tone` scales the ink coverage.
pub fn draw_screentone(
    target: &mut Image,
    bounds: Rect,
    angle_deg: f32,
    spacing: f32,
    dot_ratio: f32,
    tone: u8,
    color: [u8; 4],
) {
    let spacing = spacing.max(2.0);
    let ratio = dot_ratio.clamp(0.05, 1.0);
    let coverage = (tone as f32 / 255.0).clamp(0.0, 1.0);
    if coverage <= 0.0 {
        return;
    }
    // Screen angle: the lattice is a square grid rotated about the region centre.
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let cx = bounds.x + bounds.w * 0.5;
    let cy = bounds.y + bounds.h * 0.5;
    let radius = spacing * ratio * 0.5 * coverage.sqrt();
    let x0 = bounds.x.floor() as i64;
    let y0 = bounds.y.floor() as i64;
    let x1 = (bounds.x + bounds.w).ceil() as i64;
    let y1 = (bounds.y + bounds.h).ceil() as i64;
    let alpha = color[3] as f32 / 255.0;
    for y in y0..y1 {
        for x in x0..x1 {
            if x < 0 || y < 0 || x >= target.width as i64 || y >= target.height as i64 {
                continue;
            }
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if !bounds.contains(px, py) {
                continue;
            }
            // Rotate into lattice space and test the nearest dot centre.
            let dx = px - cx;
            let dy = py - cy;
            let lattice_x = dx * cos + dy * sin;
            let lattice_y = -dx * sin + dy * cos;
            let nearest_x = (lattice_x / spacing).round() * spacing;
            let nearest_y = (lattice_y / spacing).round() * spacing;
            let distance = ((lattice_x - nearest_x).powi(2) + (lattice_y - nearest_y).powi(2)).sqrt();
            if distance > radius {
                continue;
            }
            let index = ((y as u32 * target.width + x as u32) * 4) as usize;
            for channel in 0..3 {
                let top = color[channel] as f32;
                let bottom = target.data[index + channel] as f32;
                target.data[index + channel] = (top * alpha + bottom * (1.0 - alpha))
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            let existing = target.data[index + 3] as f32 / 255.0;
            target.data[index + 3] =
                ((alpha + existing * (1.0 - alpha)) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn pixmap_to_image(pixmap: &Pixmap) -> Image {
    let mut image = Image::new(pixmap.width(), pixmap.height());
    for (index, pixel) in pixmap.pixels().iter().enumerate() {
        let alpha = pixel.alpha();
        if alpha == 0 {
            continue;
        }
        let straight = |channel: u8| {
            ((channel as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8
        };
        image.data[index * 4] = straight(pixel.red());
        image.data[index * 4 + 1] = straight(pixel.green());
        image.data[index * 4 + 2] = straight(pixel.blue());
        image.data[index * 4 + 3] = alpha;
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(image: &Image, region: Rect) -> usize {
        let mut count = 0;
        for y in 0..image.height {
            for x in 0..image.width {
                if !region.contains(x as f32 + 0.5, y as f32 + 0.5) {
                    continue;
                }
                let index = ((y * image.width + x) * 4) as usize;
                if image.data[index + 3] > 8 {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn a_balloon_fills_its_body_and_leaves_the_corners_clear() {
        let mut image = Image::new(200, 160);
        let bounds = Rect::new(20.0, 20.0, 160.0, 110.0);
        draw_balloon(&mut image, &Balloon::new(bounds, [255, 255, 255, 255]));
        let centre = Rect::new(90.0, 70.0, 20.0, 20.0);
        assert_eq!(ink(&image, centre), 400, "the body must be opaque");
        let corner = Rect::new(20.5, 20.5, 2.0, 2.0);
        assert_eq!(ink(&image, corner), 0, "the rounded corner stays clear");
    }

    #[test]
    fn a_tail_reaches_past_the_body() {
        let mut image = Image::new(200, 220);
        let bounds = Rect::new(20.0, 20.0, 160.0, 100.0);
        let mut balloon = Balloon::new(bounds, [255, 255, 255, 255]);
        balloon.tail = Some((100.0, 200.0));
        draw_balloon(&mut image, &balloon);
        let below = Rect::new(88.0, 150.0, 24.0, 24.0);
        assert!(ink(&image, below) > 40, "the tail must carry ink towards the tip");
    }

    #[test]
    fn the_tail_interior_is_filled_not_only_outlined() {
        let mut image = Image::new(200, 260);
        let bounds = Rect::new(20.0, 20.0, 160.0, 100.0);
        let mut balloon = Balloon::new(bounds, [255, 255, 255, 255]);
        balloon.tail = Some((100.0, 240.0));
        draw_balloon(&mut image, &balloon);
        // A point inside the tail, well below the body and away from the two
        // outline edges, must be opaque.
        let inside = Rect::new(96.0, 190.0, 8.0, 8.0);
        assert_eq!(ink(&image, inside), 64, "the tail interior must be filled");
    }

    #[test]
    fn a_frame_paints_a_border_band_and_keeps_the_inside_clean() {
        let mut image = Image::new(120, 120);
        let bounds = Rect::new(10.0, 10.0, 100.0, 100.0);
        draw_frame(&mut image, bounds, 4.0, [0, 0, 0, 255]);
        let inside = Rect::new(20.0, 20.0, 80.0, 80.0);
        assert_eq!(ink(&image, inside), 0, "the interior is untouched");
        let left_band = Rect::new(10.0, 40.0, 3.0, 40.0);
        assert_eq!(ink(&image, left_band), 120, "the border is solid");
    }

    #[test]
    fn panel_division_tiles_the_area_and_keeps_a_gutter() {
        let panels = divide_panels(Rect::new(0.0, 0.0, 300.0, 200.0), 2, 3, 10.0);
        assert_eq!(panels.len(), 6);
        let first = panels[0];
        let second = panels[1];
        assert!((second.x - (first.x + first.w) - 10.0).abs() < 0.01);
        let bottom = panels[3];
        assert!((bottom.y - (first.y + first.h) - 10.0).abs() < 0.01);
        assert!((panels[5].x + panels[5].w - 300.0).abs() < 0.01);
    }

    #[test]
    fn screentone_coverage_follows_the_tone_value() {
        let mut light = Image::new(80, 80);
        draw_screentone(
            &mut light,
            Rect::new(0.0, 0.0, 80.0, 80.0),
            0.0,
            8.0,
            1.0,
            40,
            [0, 0, 0, 255],
        );
        let mut dark = Image::new(80, 80);
        draw_screentone(
            &mut dark,
            Rect::new(0.0, 0.0, 80.0, 80.0),
            0.0,
            8.0,
            1.0,
            220,
            [0, 0, 0, 255],
        );
        let light_ink = ink(&light, Rect::new(0.0, 0.0, 80.0, 80.0));
        let dark_ink = ink(&dark, Rect::new(0.0, 0.0, 80.0, 80.0));
        assert!(light_ink > 0, "a light tone still prints dots");
        assert!(
            dark_ink > light_ink * 2,
            "a darker tone must cover more: {dark_ink} vs {light_ink}"
        );
    }

    #[test]
    fn a_screentone_stays_inside_its_bounds() {
        let mut image = Image::new(100, 100);
        draw_screentone(
            &mut image,
            Rect::new(20.0, 20.0, 40.0, 40.0),
            30.0,
            6.0,
            1.0,
            255,
            [0, 0, 0, 255],
        );
        assert_eq!(ink(&image, Rect::new(0.0, 0.0, 19.0, 100.0)), 0);
        assert!(ink(&image, Rect::new(20.0, 20.0, 40.0, 40.0)) > 200);
    }
}
