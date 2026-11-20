//! Minimal PDF writer for page export.
//!
//! Sumi exports one raster page per PDF page: the composite is embedded as a
//! Flate-compressed DeviceRGB stream, alpha is composited over white, and the
//! page box derives from the document DPI. No fonts, vector objects or colour
//! profiles are written, so the result is a print-ready bitmap book, not an
//! editable vector document.

use crate::render::Image;
use anyhow::{bail, Context, Result};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;
use std::path::Path;

/// One exported page: a composite image plus its resolution.
pub struct PdfPage<'a> {
    pub image: &'a Image,
    /// Pixels per inch used to convert the pixel size into PDF points.
    pub dpi: f32,
}

impl<'a> PdfPage<'a> {
    pub fn new(image: &'a Image, dpi: f32) -> Self {
        PdfPage { image, dpi: if dpi > 1.0 { dpi } else { 72.0 } }
    }

    /// Page size in PDF points (1/72 inch).
    pub fn points(&self) -> (f32, f32) {
        let scale = 72.0 / self.dpi;
        (self.image.width as f32 * scale, self.image.height as f32 * scale)
    }
}

fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}

/// Composite the straight-alpha image over an opaque white page.
fn rgb_over_white(image: &Image) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(image.data.len() / 4 * 3);
    for pixel in image.data.chunks_exact(4) {
        let alpha = pixel[3] as f32 / 255.0;
        for channel in 0..3 {
            let value = pixel[channel] as f32 * alpha + 255.0 * (1.0 - alpha);
            rgb.push(value.round().clamp(0.0, 255.0) as u8);
        }
    }
    rgb
}

/// Write `pages` to `path` as a PDF with one raster page per image.
pub fn write_pdf(path: &Path, pages: &[PdfPage<'_>]) -> Result<()> {
    if pages.is_empty() {
        bail!("a PDF needs at least one page");
    }
    // Object layout: 1 catalog, 2 page tree, then per page: page object,
    // image XObject and content stream.
    let mut objects: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut kids = String::new();
    let mut next_id = 3u32;
    let mut per_page = Vec::new();
    for page in pages {
        let page_id = next_id;
        let image_id = next_id + 1;
        let content_id = next_id + 2;
        next_id += 3;
        kids.push_str(&format!("{page_id} 0 R "));
        per_page.push((page, page_id, image_id, content_id));
    }

    objects.push((
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
    ));
    objects.push((
        2,
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.trim_end(), pages.len())
            .into_bytes(),
    ));

    for (page, page_id, image_id, content_id) in per_page {
        let (width, height) = page.points();
        objects.push((
            page_id,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width:.3} {height:.3}] \
                 /Resources << /XObject << /Im0 {image_id} 0 R >> >> /Contents {content_id} 0 R >>"
            )
            .into_bytes(),
        ));
        let rgb = rgb_over_white(page.image);
        let compressed = compress(&rgb).context("compress page pixels")?;
        let mut image_object = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            page.image.width,
            page.image.height,
            compressed.len()
        )
        .into_bytes();
        image_object.extend_from_slice(&compressed);
        image_object.extend_from_slice(b"\nendstream");
        objects.push((image_id, image_object));

        let content = format!("q\n{width:.3} 0 0 {height:.3} 0 0 cm\n/Im0 Do\nQ\n");
        objects.push((
            content_id,
            format!("<< /Length {} >>\nstream\n{}endstream", content.len(), content)
                .into_bytes(),
        ));
    }

    objects.sort_by_key(|(id, _)| *id);
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (id, body) in &objects {
        offsets.push(out.len());
        out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_start
        )
        .as_bytes(),
    );
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn page(width: u32, height: u32, color: [u8; 4]) -> Image {
        Image::filled(width, height, color)
    }

    fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
        haystack[from..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|index| index + from)
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).to_string()
    }

    #[test]
    fn pdf_page_uses_the_document_dpi() {
        // 300 pixels at 300 dpi is one inch, i.e. 72 points.
        let image = page(300, 300, [255, 255, 255, 255]);
        let pdf_page = PdfPage::new(&image, 300.0);
        assert_eq!(pdf_page.points(), (72.0, 72.0));
        // Mixed DPI pages must keep their own physical size.
        let small = page(100, 100, [0, 0, 0, 255]);
        assert!((PdfPage::new(&small, 72.0).points().0 - 100.0).abs() < 0.01);
    }

    #[test]
    fn every_page_becomes_a_pdf_page_with_the_right_box() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("story.pdf");
        let first = page(100, 50, [255, 0, 0, 255]);
        let second = page(100, 100, [0, 0, 255, 255]);
        let pages = [PdfPage::new(&first, 72.0), PdfPage::new(&second, 72.0)];
        write_pdf(&target, &pages).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        let body = text(&bytes);
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(body.trim_end().ends_with("%%EOF"), "the file must end with EOF");
        assert!(body.contains("/Count 2"), "two pages in the page tree");
        assert_eq!(body.matches("/Type /Page ").count(), 2);
        assert!(body.contains("/MediaBox [0 0 100.000 50.000]"));
        assert!(body.contains("/MediaBox [0 0 100.000 100.000]"));
    }

    #[test]
    fn poppler_reports_the_exported_page_size_and_content() {
        // An independent reader (Poppler) states the physical size and paints
        // the page, so this is external acceptance rather than a self-check.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("poppler.pdf");
        let first = page(300, 300, [255, 0, 0, 255]);
        let second = page(600, 400, [0, 0, 255, 255]);
        let pages = [PdfPage::new(&first, 300.0), PdfPage::new(&second, 150.0)];
        write_pdf(&target, &pages).unwrap();

        let info = std::process::Command::new("pdfinfo")
            .args(["-f", "1", "-l", "2"])
            .arg(&target)
            .output()
            .expect("pdfinfo is required for the PDF physical-size gate");
        assert!(
            info.status.success(),
            "pdfinfo failed: {}",
            String::from_utf8_lossy(&info.stderr)
        );
        // Poppler right-aligns the numbers, so compare after squeezing runs of
        // spaces instead of hard-coding its column width.
        let report = String::from_utf8_lossy(&info.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            report.contains("Page 1 size: 72 x 72 pts"),
            "300 px at 300 dpi is one inch: {report}"
        );
        assert!(
            report.contains("Page 2 size: 288 x 192 pts"),
            "600x400 px at 150 dpi is 4x2.67 inch: {report}"
        );

        let prefix = dir.path().join("render");
        let render = std::process::Command::new("pdftoppm")
            .args(["-f", "1", "-l", "1", "-r", "72"])
            .arg(&target)
            .arg(&prefix)
            .output()
            .expect("pdftoppm is required for the PDF content gate");
        assert!(
            render.status.success(),
            "pdftoppm failed: {}",
            String::from_utf8_lossy(&render.stderr)
        );
        let ppm = std::fs::read(dir.path().join("render-1.ppm")).expect("rendered PPM");
        let mut parts = ppm.splitn(4, |byte| *byte == b'\n');
        assert_eq!(parts.next(), Some(&b"P6"[..]), "Poppler writes a P6 PPM");
        let size = String::from_utf8_lossy(parts.next().expect("PPM size line"));
        assert_eq!(size.trim(), "72 72", "one inch at 72 dpi is 72 pixels");
        let maxval = String::from_utf8_lossy(parts.next().expect("PPM max value line"));
        assert_eq!(maxval.trim(), "255");
        let pixels = parts.next().expect("PPM pixel data");
        assert_eq!(
            &pixels[0..3],
            &[255, 0, 0],
            "the exported page must paint the document colour"
        );
    }

    #[test]
    fn the_page_box_follows_the_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dpi.pdf");
        let image = page(144, 144, [255, 255, 255, 255]);
        write_pdf(&target, &[PdfPage::new(&image, 144.0)]).unwrap();
        let body = text(&std::fs::read(&target).unwrap());
        assert!(body.contains("/MediaBox [0 0 72.000 72.000]"), "{body}");
    }

    #[test]
    fn transparent_pixels_are_composited_over_white() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("alpha.pdf");
        let image = page(2, 2, [0, 0, 0, 0]);
        write_pdf(&target, &[PdfPage::new(&image, 72.0)]).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        // Locate the image stream by its /Subtype /Image dictionary.
        let dictionary = find(&bytes, b"/Subtype /Image", 0).expect("image dictionary");
        let length_start = find(&bytes, b"/Length ", dictionary).unwrap() + b"/Length ".len();
        let length: usize = text(&bytes[length_start..])
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let stream = find(&bytes, b"stream\n", dictionary).unwrap() + b"stream\n".len();
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[stream..stream + length]);
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, vec![255u8; 2 * 2 * 3], "alpha composites onto white");
    }

    #[test]
    fn the_xref_offsets_point_at_their_objects() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("xref.pdf");
        let image = page(8, 8, [10, 20, 30, 255]);
        write_pdf(&target, &[PdfPage::new(&image, 72.0)]).unwrap();
        let bytes = std::fs::read(&target).unwrap();
        let body = text(&bytes);
        let xref_start: usize = body
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(bytes[xref_start..].starts_with(b"xref"), "startxref points at the table");
        let table = String::from_utf8_lossy(&bytes[xref_start..]).to_string();
        let mut id = 1;
        for line in table.lines().skip(3) {
            if line.len() < 18 || !line.ends_with(' ') {
                break;
            }
            let offset: usize = line[..10].parse().unwrap();
            let expected = format!("{id} 0 obj");
            assert!(
                bytes[offset..].starts_with(expected.as_bytes()),
                "entry {id} points at {:?}",
                &bytes[offset..offset + 16]
            );
            id += 1;
        }
        assert!(id > 3, "the table lists every object");
    }
}
