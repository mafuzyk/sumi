//! Write a `.clip` that exercises the features the editor can produce: a
//! folder with a child layer, a layer mask, a clipping layer and an adjustment
//! layer. Used to check that CLIP STUDIO PAINT accepts what this writer emits.
//!
//! Usage: mixed_features <out.clip>

use clip_core::adjust;
use clip_core::write::ClipWriter;

/// Opaque rectangle on a transparent canvas. Structured shapes matter: a
/// full-canvas fill cannot show whether clipping, masks or corrections are
/// applied at all.
fn rect(width: u32, height: u32, x0: u32, y0: u32, x1: u32, y1: u32, colour: [u8; 4]) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in y0.min(height)..y1.min(height) {
        for x in x0.min(width)..x1.min(width) {
            let i = ((y * width + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&colour);
        }
    }
    pixels
}

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).expect("usage: mixed_features <out.clip> [all|folder|mask|clip|adjust]");
    let mode = std::env::args().nth(2).unwrap_or_else(|| "all".to_string());
    let with_folder = mode == "all" || mode == "folder";
    let with_mask = mode == "all" || mode == "mask";
    let with_clip = mode == "all" || mode == "clip";
    let with_adjust = mode == "all" || mode == "adjust" || mode == "levels";
    let with_levels = mode == "levels";
    let width = 800;
    let height = 600;
    let mut writer = ClipWriter::blank(width, height)?;
    let canvas = writer.canvas_root_folder()?;
    let paper = writer
        .db()
        .query_row("select MainId from Layer where LayerType=1584 limit 1", [], |row| {
            row.get::<_, i64>(0)
        })?;
    let paint = writer
        .db()
        .query_row(
            "select MainId from Layer where not (LayerFolder & 1) and LayerType not in (256, 1584) limit 1",
            [],
            |row| row.get::<_, i64>(0),
        )?;

    // Base rectangle on the paint layer; the rest of the canvas stays empty.
    writer.set_pixels(paint, &rect(width, height, 100, 100, 700, 500, [230, 90, 60, 255]), width, height)?;

    // A folder with one child layer inside it, at half opacity.
    let mut order = vec![paper, paint];
    if with_folder {
        let folder = writer.add_folder(paint, "Grupo")?;
        let inside = writer.add_raster_layer_in(folder, 0, "Dentro")?;
        writer.set_pixels(inside, &rect(width, height, 100, 100, 450, 500, [40, 90, 220, 255]), width, height)?;
        writer.set_layer_attr(inside, None, Some(128), None, None)?;
        order.push(folder);
    }
    // A clipping layer above its base: over the folder when there is one,
    // otherwise over the paint layer.
    let mut clip_top = paint;
    if with_clip {
        let base = if with_folder { order[2] } else { paint };
        let clipped = writer.add_raster_layer(base, "Clipado")?;
        // A band wider than the base: with clipping it only shows over the base
        // rectangle, without clipping it paints the whole band.
        writer.set_pixels(clipped, &rect(width, height, 0, 250, 800, 350, [250, 220, 40, 255]), width, height)?;
        writer.set_layer_clip(clipped, true)?;
        order.push(clipped);
        clip_top = clipped;
    }
    if with_adjust {
        // A complete FilterLayerInfo blob, not a bare parameter payload: the
        // blob carries the filter type and body size CSP and this crate parse.
        // `levels` mode writes a type-2 correction, the encoding CSP's own
        // corpus uses, so an HSV failure can be told apart from a filter
        // that CSP refuses in general.
        let kind = if with_levels {
            adjust::FilterKind::Levels(adjust::Levels {
                in_black: 0.2,
                in_white: 0.8,
                out_black: 0.0,
                out_white: 1.0,
            })
        } else {
            adjust::FilterKind::Hsv { hue: 40.0, saturation: -20.0, luminosity: 0.0 }
        };
        let filter = adjust::Filter { kind, raw: Vec::new() };
        let blob = filter.to_blob().expect("HSV filter encodes");
        let adjustment = writer.add_adjustment(clip_top, "Matiz", &blob)?;
        // `mark_adjustment` already sets LayerVisibility = 3 (visible with its mask).
        order.push(adjustment);
    }
    // A layer mask on the paint layer, half hidden with a soft edge.
    let mask_mip = if with_mask { writer.add_layer_mask(paint)? } else { 0 };
    let mut mask = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let coverage = ((u32::from(y.min(height - y)) * 255) / (height / 2).max(1)).min(255);
        for _ in 0..width {
            mask.extend_from_slice(&[255, 255, 255, coverage as u8]);
        }
    }
    if with_mask {
        let _ = mask_mip; // the mask surface was created by add_layer_mask
        writer.set_mask_pixels(paint, &mask, width, height)?;
    }

    // Keep the paper under everything.
    writer.set_sibling_order(canvas, &order)?;
    writer.refresh_preview(25.0)?;
    writer.save(&out)?;
    println!("wrote {out}");
    Ok(())
}
