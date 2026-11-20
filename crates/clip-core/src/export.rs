//! Export a composited document to raster or layered formats.
//!
//! `.clip` saving stays in `ClipWriter` because it edits an existing CSP
//! document. This module covers the formats that can be generated from the
//! straight-RGBA composite: PNG, JPEG, WebP, TIFF, flattened PSD/PSB and a
//! Krita `.kra` with paint layers.

use std::fs::File;
use std::io::{BufWriter, Cursor, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use image::ImageEncoder;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::kra::KraDocument;
use crate::render::Image;

/// Raster formats written directly from the composite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterFormat {
    Png,
    Jpeg,
    WebP,
    Tiff,
}

impl RasterFormat {
    /// Match a filename extension, case-insensitively.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" | "jpe" => Some(Self::Jpeg),
            "webp" => Some(Self::WebP),
            "tif" | "tiff" => Some(Self::Tiff),
            _ => None,
        }
    }
}

/// One paint layer handed to the Krita writer. Layers are listed top-first,
/// matching `maindoc.xml`, and `image` must already be composited into canvas
/// coordinates at the document size.
pub struct KraLayerData<'a> {
    pub name: &'a str,
    pub visible: bool,
    /// Straight opacity in 0..=1.
    pub opacity: f64,
    /// Canvas-sized straight RGBA.
    pub image: &'a Image,
}

/// Save a composite in one of the four raster formats.
///
/// JPEG has no alpha, so transparent pixels are composited over white. The
/// `quality` argument is used by JPEG (1..=100); the other formats are
/// lossless.
pub fn save_raster(image: &Image, path: &Path, format: RasterFormat, quality: u8) -> Result<()> {
    match format {
        RasterFormat::Png => {
            let mut data = Vec::new();
            {
                let mut encoder = png::Encoder::new(Cursor::new(&mut data), image.width, image.height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(&image.data)?;
            }
            std::fs::write(path, data).with_context(|| format!("write {}", path.display()))?;
        }
        RasterFormat::Jpeg => {
            let mut rgb = Vec::with_capacity((image.width * image.height * 3) as usize);
            for px in image.data.chunks_exact(4) {
                let a = px[3] as u32;
                for c in 0..3 {
                    let straight = px[c] as u32;
                    let white = 255u32;
                    let blended = (straight * a + white * (255 - a)) / 255;
                    rgb.push(blended as u8);
                }
            }
            let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
            let mut writer = BufWriter::new(file);
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut writer,
                quality.clamp(1, 100),
            );
            encoder
                .write_image(&rgb, image.width, image.height, image::ExtendedColorType::Rgb8)
                .with_context(|| format!("encode JPEG {}", path.display()))?;
            writer.flush()?;
        }
        RasterFormat::WebP => {
            let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
            let mut writer = BufWriter::new(file);
            let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut writer);
            encoder
                .write_image(
                    &image.data,
                    image.width,
                    image.height,
                    image::ExtendedColorType::Rgba8,
                )
                .with_context(|| format!("encode WebP {}", path.display()))?;
            writer.flush()?;
        }
        RasterFormat::Tiff => {
            let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
            let mut writer = BufWriter::new(file);
            let encoder = image::codecs::tiff::TiffEncoder::new(&mut writer);
            encoder
                .write_image(
                    &image.data,
                    image.width,
                    image.height,
                    image::ExtendedColorType::Rgba8,
                )
                .with_context(|| format!("encode TIFF {}", path.display()))?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Write a flattened RGB+alpha Photoshop document.
///
/// This is intentionally the baseline Photoshop file: header, no image
/// resources, no layer records, and raw planar channels. It opens everywhere
/// PSD is supported but does not preserve the editor's layer stack yet.
pub fn write_psd(image: &Image, path: &Path, psb: bool) -> Result<()> {
    let (width, height) = (image.width, image.height);
    if width == 0 || height == 0 {
        bail!("cannot export an empty image");
    }
    if psb {
        if width > 300_000 || height > 300_000 {
            bail!("PSB dimensions exceed the 300000 px format limit");
        }
    } else if width > 30_000 || height > 30_000 {
        bail!("PSD dimensions exceed the 30000 px limit; use .psb");
    }

    let mut out = Vec::with_capacity((width * height * 4) as usize + 64);
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&(if psb { 2u16 } else { 1u16 }).to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&4u16.to_be_bytes()); // RGBA channels
    // Both PSD and PSB store 32-bit height and width here; PSB only widens
    // the later layer/mask section length.
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes()); // 8 bits per channel
    out.extend_from_slice(&3u16.to_be_bytes()); // RGB colour mode
    out.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    out.extend_from_slice(&0u32.to_be_bytes()); // image resources
    if psb {
        out.extend_from_slice(&0u64.to_be_bytes()); // 64-bit layer/mask length
    } else {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // raw image data

    for channel in 0..4 {
        for px in image.data.chunks_exact(4) {
            out.push(px[channel]);
        }
    }
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Write a Krita document with one paint layer per entry.
///
/// Krita's native layer payload is a tiled RGBA stream, not a PNG. This writer
/// emits 64×64 uncompressed BGRA tiles; ZIP deflate still keeps the file
/// compact. `merged` is stored as `mergedimage.png` and `preview.png`.
pub fn write_kra(
    path: &Path,
    width: u32,
    height: u32,
    layers: &[KraLayerData<'_>],
    merged: &Image,
) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("cannot export an empty image");
    }
    if merged.width != width || merged.height != height {
        bail!("merged image does not match the canvas size");
    }
    for layer in layers {
        if layer.image.width != width || layer.image.height != height {
            bail!(
                "layer {:?} is {}x{}, expected the canvas size {width}x{height}",
                layer.name,
                layer.image.width,
                layer.image.height
            );
        }
    }
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/x-krita")?;

    let image_name = "sumi";
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<DOC xmlns=\"http://www.calligra.org/DTD/krita\" kritaVersion=\"5.2.0\" syntaxVersion=\"2.0\" editor=\"Krita\">\n");
    xml.push_str(&format!(
        " <IMAGE name=\"sumi\" width=\"{width}\" height=\"{height}\" colorspacename=\"RGBA\" mime=\"application/x-kra\" x-res=\"72\" y-res=\"72\">\n  <layers>\n"
    ));
    for (index, layer) in layers.iter().enumerate() {
        let filename = format!("layer{}", index + 1);
        xml.push_str(&format!(
            "   <layer name=\"{}\" nodetype=\"paintlayer\" visible=\"{}\" opacity=\"{}\" compositeop=\"normal\" x=\"0\" y=\"0\" filename=\"{filename}\" colorspacename=\"RGBA\"/>\n",
            xml_escape(layer.name),
            if layer.visible { 1 } else { 0 },
            (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u32,
        ));
    }
    xml.push_str("  </layers>\n </IMAGE>\n</DOC>\n");
    zip.start_file("maindoc.xml", deflated)?;
    zip.write_all(xml.as_bytes())?;
    zip.start_file("documentinfo.xml", deflated)?;
    zip.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE document-info PUBLIC '-//KDE//DTD document-info 1.1//EN' 'http://www.calligra.org/DTD/document-info-1.1.dtd'>\n<document-info xmlns=\"http://www.calligra.org/DTD/document-info\"><about><author><full-name/></author></about></document-info>\n")?;

    for (index, layer) in layers.iter().enumerate() {
        let data = encode_krita_tiles(&layer.image.data, width, height);
        zip.start_file(format!("{image_name}/layers/layer{}", index + 1), deflated)?;
        zip.write_all(&data)?;
    }

    let merged_png = encode_png(merged)?;
    zip.start_file("mergedimage.png", deflated)?;
    zip.write_all(&merged_png)?;
    zip.start_file("preview.png", deflated)?;
    zip.write_all(&merged_png)?;
    zip.finish()?;
    Ok(())
}

pub(crate) fn encode_png(image: &Image) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut data), image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&image.data)?;
    }
    Ok(data)
}

/// Krita's tiled layer stream: header, then 64×64 BGRA tiles prefixed with a
/// compression byte. All tiles are written raw so a reader can rebuild the
/// layer without needing `.defaultpixel` files.
pub(crate) fn encode_krita_tiles(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let tile_w = 64u32;
    let tile_h = 64u32;
    let count = width.div_ceil(tile_w) * height.div_ceil(tile_h);
    let mut out = Vec::with_capacity((width * height * 4) as usize + 128);
    out.extend_from_slice(b"VERSION 2\n");
    out.extend_from_slice(b"TILEWIDTH 64\n");
    out.extend_from_slice(b"TILEHEIGHT 64\n");
    out.extend_from_slice(b"PIXELSIZE 4\n");
    out.extend_from_slice(format!("DATA {count}\n").as_bytes());
    const TILE_BYTES: usize = 64 * 64 * 4;
    let mut tile = vec![0u8; TILE_BYTES];
    for tile_y in (0..height).step_by(tile_h as usize) {
        for tile_x in (0..width).step_by(tile_w as usize) {
            tile.fill(0);
            for row in 0..tile_h.min(height - tile_y) {
                let src_y = tile_y + row;
                let copy_w = tile_w.min(width - tile_x);
                for col in 0..copy_w {
                    let src_x = tile_x + col;
                    let src = ((src_y * width + src_x) * 4) as usize;
                    let dst = ((row * tile_w + col) * 4) as usize;
                    tile[dst] = data[src + 2];
                    tile[dst + 1] = data[src + 1];
                    tile[dst + 2] = data[src];
                    tile[dst + 3] = data[src + 3];
                }
            }
            out.extend_from_slice(format!("{tile_x},{tile_y},LZF,{}\n", TILE_BYTES + 1).as_bytes());
            out.push(0);
            out.extend_from_slice(&tile);
        }
    }
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Composite a `.kra` and export it through the same path. Useful for the CLI
/// and for tests; the editor uses its in-memory layers instead.
pub fn export_kra_merged(input: &Path, output: &Path, format: RasterFormat, quality: u8) -> Result<()> {
    let doc = KraDocument::open(input)?;
    let merged = doc.composite();
    save_raster(&merged, output, format, quality)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(width: u32, height: u32) -> Image {
        let mut image = Image::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                image.data[i] = (x * 255 / width.max(1)) as u8;
                image.data[i + 1] = (y * 255 / height.max(1)) as u8;
                image.data[i + 2] = 40;
                image.data[i + 3] = 255;
            }
        }
        image
    }

    #[test]
    fn raster_formats_round_trip_dimensions() {
        let dir = tempdir().unwrap();
        let image = sample(48, 32);
        for (name, format) in [
            ("a.png", RasterFormat::Png),
            ("a.jpg", RasterFormat::Jpeg),
            ("a.webp", RasterFormat::WebP),
            ("a.tiff", RasterFormat::Tiff),
        ] {
            let path = dir.path().join(name);
            save_raster(&image, &path, format, 92).unwrap();
            let decoded = image::open(&path).unwrap();
            assert_eq!(decoded.width(), 48, "{name}");
            assert_eq!(decoded.height(), 32, "{name}");
        }
    }

    #[test]
    fn kra_round_trips_layers_and_pixels() {
        let dir = tempdir().unwrap();
        let mut top = Image::new(70, 70);
        for px in top.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        let mut bottom = Image::new(70, 70);
        for px in bottom.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 255, 255]);
        }
        let merged = sample(70, 70);
        let path = dir.path().join("out.kra");
        write_kra(
            &path,
            70,
            70,
            &[
                KraLayerData { name: "top", visible: true, opacity: 0.5, image: &top },
                KraLayerData { name: "bottom", visible: true, opacity: 1.0, image: &bottom },
            ],
            &merged,
        )
        .unwrap();

        let doc = KraDocument::open(&path).unwrap();
        assert_eq!((doc.width, doc.height), (70, 70));
        assert_eq!(doc.layers.len(), 2);
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["top", "bottom"]);
        let i = (10 * 70 + 10) * 4;
        assert_eq!(&doc.layers[0].image.data[i..i + 4], &[255, 0, 0, 255]);
        assert!((doc.layers[0].opacity - 0.5).abs() < 0.01);
        assert_eq!(&doc.layers[1].image.data[i..i + 4], &[0, 0, 255, 255]);
        assert_eq!(doc.merged.unwrap().data.len(), 70 * 70 * 4);
    }

    #[test]
    fn psd_and_psb_have_valid_headers() {
        let dir = tempdir().unwrap();
        let image = sample(40, 30);
        for (name, psb, version) in [("a.psd", false, 1u16), ("a.psb", true, 2u16)] {
            let path = dir.path().join(name);
            write_psd(&image, &path, psb).unwrap();
            let data = std::fs::read(&path).unwrap();
            assert_eq!(&data[..4], b"8BPS");
            assert_eq!(u16::from_be_bytes([data[4], data[5]]), version);
            if psb {
                assert_eq!(
                    u32::from_be_bytes([data[14], data[15], data[16], data[17]]),
                    30
                );
            } else {
                assert_eq!(
                    u32::from_be_bytes([data[14], data[15], data[16], data[17]]),
                    30
                );
                assert_eq!(
                    u32::from_be_bytes([data[18], data[19], data[20], data[21]]),
                    40
                );
            }
        }
    }
}
