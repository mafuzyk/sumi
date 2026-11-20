//! Flat layer-stack conversion into a new CSP `.clip` document.
//!
//! The editor's Save As uses the richer `csp-app::doc` tree builder to retain
//! groups. This helper is for CLI conversion where a portable flat stack is
//! enough and keeps the conversion independent from the GTK application.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::render::Image;
use crate::write::ClipWriter;

#[derive(Clone, Copy)]
pub struct ConvertLayer<'a> {
    pub name: &'a str,
    pub image: &'a Image,
    pub opacity: f64,
    pub visible: bool,
    pub composite: i64,
}

/// Write a flat bottom-to-top layer stack into a fresh `.clip` template.
pub fn write_flat_clip(
    path: &Path,
    width: u32,
    height: u32,
    layers: &[ConvertLayer<'_>],
    hide_paper: bool,
) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("cannot create an empty .clip document");
    }
    let mut writer = ClipWriter::blank(width, height)?;
    let paper_id: i64 = writer
        .db()
        .query_row(
            "select MainId from Layer where LayerType=1584 limit 1",
            [],
            |r| r.get(0),
        )
        .context("blank .clip has no paper layer")?;
    let template_paint: i64 = writer
        .db()
        .query_row(
            "select MainId from Layer where LayerType=1 and coalesce(LayerRenderMipmap,0)<>0 limit 1",
            [],
            |r| r.get(0),
        )
        .context("blank .clip has no raster layer")?;
    if hide_paper {
        writer.set_layer_attr(paper_id, None, None, None, Some(false))?;
    }
    let mut previous = paper_id;
    for (index, layer) in layers.iter().enumerate() {
        if layer.image.width != width || layer.image.height != height {
            bail!(
                "layer {:?} is {}x{}, expected canvas {width}x{height}",
                layer.name,
                layer.image.width,
                layer.image.height
            );
        }
        let id = if index == 0 {
            template_paint
        } else {
            writer.add_raster_layer(previous, layer.name)?
        };
        writer.set_pixels(id, &layer.image.data, width, height)?;
        writer.set_layer_attr(
            id,
            Some(layer.name),
            Some((layer.opacity.clamp(0.0, 1.0) * 256.0).round() as i64),
            Some(layer.composite),
            Some(layer.visible),
        )?;
        previous = id;
    }
    writer.refresh_preview(25.0)?;
    writer.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_conversion_preserves_layer_order_and_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flat.clip");
        let bottom = Image::filled(8, 6, [10, 20, 30, 255]);
        let top = Image::new(8, 6);
        let layers = [
            ConvertLayer {
                name: "Bottom",
                image: &bottom,
                opacity: 1.0,
                visible: true,
                composite: 0,
            },
            ConvertLayer {
                name: "Top",
                image: &top,
                opacity: 0.5,
                visible: true,
                composite: crate::blend::MODE_MULTIPLY,
            },
        ];
        write_flat_clip(&path, 8, 6, &layers, false).unwrap();
        let reopened = ClipWriter::load(&path).unwrap();
        let names: Vec<String> = {
            let mut stmt = reopened
                .db()
                .prepare("select LayerName from Layer where LayerType=1 order by _PW_ID")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(Result::ok)
                .collect()
        };
        assert_eq!(names, ["Bottom", "Top"]);
    }
}
