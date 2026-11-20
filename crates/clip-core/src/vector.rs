//! Vector stroke payloads (`VectorObjectList.VectorData`).
//!
//! Vector layers (line art, frame borders) store no raster pixels in the
//! offscreen mip chain: their content lives in an external chunk referenced by
//! `VectorObjectList`. The payload is a flat sequence of objects:
//!
//! ```text
//! object:
//!   u32 header_size       88 for strokes, 96 for frame rectangles
//!   u32 72
//!   u32 point_size        88
//!   u32 88
//!   u32 point_count
//!   u32 flags
//!   i32 bbox[4]
//!   ... style bytes up to header_size
//!   point[point_count]:
//!     f64 x, f64 y
//!     i32 rect[4]
//!     f32 pressure, width_scale, ...
//!     f32 tangents / flags
//! ```
//!
//! Coordinates are page pixels. For ordinary strokes (`header_size == 88`) the
//! f32 at object offset 76 is the brush width in tenths of a millimetre
//! (2.865 -> 0.2865 mm); the u32 at offset 72 is a style/tone parameter. Frame
//! rectangles (`header_size == 96`) use a different layout.

use crate::model::Layer;
use crate::render::Image;

#[derive(Debug, Clone, PartialEq)]
pub struct VectorObject {
    pub points: Vec<[f64; 2]>,
    /// Stroke width in hundredths of a millimetre (0.2865 mm -> 28.65).
    pub width: f32,
    pub flags: u32,
    pub bbox: [i32; 4],
}

fn u32_at(b: &[u8], p: usize) -> Option<u32> {
    b.get(p..p + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn f64_at(b: &[u8], p: usize) -> Option<f64> {
    b.get(p..p + 8).map(|s| {
        f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

/// Parse one external vector payload into stroke objects.
pub fn parse(payload: &[u8]) -> Vec<VectorObject> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 40 <= payload.len() {
        let Some(header_size) = u32_at(payload, p) else { break };
        if header_size != 88 && header_size != 96 {
            break;
        }
        let Some(point_size) = u32_at(payload, p + 8) else { break };
        if point_size != 88 {
            break;
        }
        let Some(count) = u32_at(payload, p + 16) else { break };
        let Some(flags) = u32_at(payload, p + 20) else { break };
        if count > 1_000_000 {
            break;
        }
        let header_size = header_size as usize;
        let count = count as usize;
        let end = p + header_size + count * point_size as usize;
        if end > payload.len() {
            break;
        }
        let width = if header_size == 88 {
            u32_at(payload, p + 76)
                .map(|bits| f32::from_bits(bits) * 10.0)
                .unwrap_or(0.0)
        } else {
            u32_at(payload, p + 72).unwrap_or(0) as f32
        };
        let bbox = [
            u32_at(payload, p + 24).unwrap_or(0) as i32,
            u32_at(payload, p + 28).unwrap_or(0) as i32,
            u32_at(payload, p + 32).unwrap_or(0) as i32,
            u32_at(payload, p + 36).unwrap_or(0) as i32,
        ];
        let mut points = Vec::with_capacity(count);
        for i in 0..count {
            let q = p + header_size + i * point_size as usize;
            let (Some(x), Some(y)) = (f64_at(payload, q), f64_at(payload, q + 8)) else {
                break;
            };
            points.push([x, y]);
        }
        out.push(VectorObject { points, width, flags, bbox });
        p = end;
    }
    out
}

/// Opaque colour used to draw a vector layer.
///
/// Vector line-art layers are drawn with the layer draw colour when the file
/// carries one (tone layers store it in `Layer.DrawColorMain*`); everything
/// else is black.
pub fn layer_color(layer: &Layer) -> [u8; 4] {
    match layer.draw_color {
        Some(c) => [c[0], c[1], c[2], 255],
        None => [0, 0, 0, 255],
    }
}

/// Rasterise objects into a canvas-sized straight-RGBA image.
///
/// `scale` is the render scale (1.0 = 100 %) and `px_per_mm` the canvas
/// resolution. Strokes are drawn as round-capped polylines with one-pixel
/// antialiasing.
pub fn rasterize(
    objects: &[VectorObject],
    width: u32,
    height: u32,
    scale: f64,
    px_per_mm: f64,
    color: [u8; 4],
    alpha_scale: f64,
) -> Image {
    let mut img = Image::new(width, height);
    for obj in objects {
        let radius = (obj.width as f64 / 100.0 * px_per_mm * scale * 0.5).max(0.35);
        if obj.points.len() == 1 {
            let [x, y] = obj.points[0];
            draw_disc(&mut img, x * scale, y * scale, radius, color, alpha_scale);
            continue;
        }
        for pair in obj.points.windows(2) {
            let a = [pair[0][0] * scale, pair[0][1] * scale];
            let b = [pair[1][0] * scale, pair[1][1] * scale];
            draw_segment(&mut img, a, b, radius, color, alpha_scale);
        }
    }
    img
}

fn draw_disc(img: &mut Image, x: f64, y: f64, radius: f64, color: [u8; 4], alpha_scale: f64) {
    let r = radius + 0.5;
    let x0 = (x - r).floor().max(0.0) as u32;
    let y0 = (y - r).floor().max(0.0) as u32;
    let x1 = ((x + r).ceil() as i64).clamp(0, img.width as i64) as u32;
    let y1 = ((y + r).ceil() as i64).clamp(0, img.height as i64) as u32;
    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as f64 + 0.5 - x;
            let dy = py as f64 + 0.5 - y;
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (radius + 0.5 - d).clamp(0.0, 1.0);
            blend_pixel(img, px, py, cov * alpha_scale, color);
        }
    }
}

fn draw_segment(
    img: &mut Image,
    a: [f64; 2],
    b: [f64; 2],
    radius: f64,
    color: [u8; 4],
    alpha_scale: f64,
) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 {
        draw_disc(img, a[0], a[1], radius, color, alpha_scale);
        return;
    }
    // Capsule cross-sections: a stroke of thickness 2r is the convex hull of
    // two discs, so each scanline intersects it in one interval. Walking only
    // that interval avoids the quadratic cost of iterating a diagonal bbox.
    let nx = -dy / len * radius;
    let ny = dx / len * radius;
    let quad = [
        [a[0] + nx, a[1] + ny],
        [a[0] - nx, a[1] - ny],
        [b[0] - nx, b[1] - ny],
        [b[0] + nx, b[1] + ny],
    ];
    let y0 = (a[1].min(b[1]) - radius - 1.0).floor().max(0.0) as u32;
    let y1 = ((a[1].max(b[1]) + radius + 1.0).ceil() as i64).clamp(0, img.height as i64) as u32;
    let vx = dx;
    let vy = dy;
    let len2 = len * len;
    for py in y0..y1 {
        let y = py as f64 + 0.5;
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for c in [a, b] {
            let d = y - c[1];
            if d.abs() <= radius {
                let s = (radius * radius - d * d).sqrt();
                lo = lo.min(c[0] - s);
                hi = hi.max(c[0] + s);
            }
        }
        for i in 0..4 {
            let p = quad[i];
            let q = quad[(i + 1) % 4];
            if (p[1] - y) * (q[1] - y) <= 0.0 && (p[1] - q[1]).abs() > 1e-12 {
                let t = (y - p[1]) / (q[1] - p[1]);
                let x = p[0] + t * (q[0] - p[0]);
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
        if hi <= lo {
            continue;
        }
        let x0 = (lo - 1.0).floor().max(0.0) as u32;
        let x1 = ((hi + 1.0).ceil() as i64).clamp(0, img.width as i64) as u32;
        for px in x0..x1 {
            let x = px as f64 + 0.5;
            let t = (((x - a[0]) * vx + (y - a[1]) * vy) / len2).clamp(0.0, 1.0);
            let ex = x - (a[0] + t * vx);
            let ey = y - (a[1] + t * vy);
            let d = (ex * ex + ey * ey).sqrt();
            let cov = (radius + 0.5 - d).clamp(0.0, 1.0);
            blend_pixel(img, px, py, cov * alpha_scale, color);
        }
    }
}

fn blend_pixel(img: &mut Image, x: u32, y: u32, coverage: f64, color: [u8; 4]) {
    if coverage <= 0.0 {
        return;
    }
    let i = ((y * img.width + x) * 4) as usize;
    let src_a = coverage * (color[3] as f64 / 255.0);
    let dst_a = img.data[i + 3] as f64 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let src = color[c] as f64 / 255.0;
        let dst = img.data[i + c] as f64 / 255.0;
        img.data[i + c] = (((src * src_a + dst * dst_a * (1.0 - src_a)) / out_a) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    img.data[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}
