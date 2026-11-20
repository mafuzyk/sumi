//! End-to-end tests against real CLIP STUDIO PAINT documents.
//!
//! The fixtures in `samples/` were produced by CSP itself (or by the
//! `clipparse` reference package for `emptyimage.clip`), so they are the
//! ground truth for the reader, the codec, the compositor and the writer.

use std::path::{Path, PathBuf};

use clip_core::chunk;
use clip_core::model::ClipFile;
use clip_core::render::{render, RenderOptions};
use clip_core::write::ClipWriter;

fn samples() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../samples");
    // These fixtures are required by the default suite but are not tracked in
    // Git yet (REPO-11), so a clean checkout must say so instead of failing on
    // an opaque unwrap inside a test.
    assert!(
        dir.is_dir(),
        "UNAVAILABLE: the required samples/ directory {} is missing; the default \
         suite is not self-contained (see docs/FIDELITY_GATES.md)",
        dir.display()
    );
    dir
}

/// The default suite's inputs must be part of the checkout, not a local
/// accident: this test fails on a tree that lost them (REPO-11).
#[test]
fn required_default_fixtures_are_present() {
    let dir = samples();
    for name in [
        "csp_simple.clip",
        "emptyimage.clip",
        "kra/example_RGBA.kra",
        "kra/example_uncompressed.kra",
        "kra/example_vector.kra",
    ] {
        let path = dir.join(name);
        assert!(
            path.is_file(),
            "required fixture {} is missing from the checkout",
            path.display()
        );
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(size > 0, "required fixture {} is empty", path.display());
    }
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/blank.clip");
    assert!(
        template.is_file(),
        "the embedded blank template {} must be in the checkout",
        template.display()
    );
}

/// Corpus fixtures (CSP sample art under `samples/extracted/`) are not tracked
/// in the repository. Those tests only exist under the `corpus` feature, and
/// with the feature on an absent fixture must fail as unavailable instead of
/// passing as if the contract had been exercised.
#[cfg(feature = "corpus")]
#[test]
#[should_panic(expected = "UNAVAILABLE")]
fn missing_corpus_fixture_fails_as_unavailable() {
    // The gate itself: an absent corpus file must fail loudly, never pass.
    corpus(PathBuf::from("/nonexistent/sumi-corpus-fixture.clip"));
}

#[cfg(feature = "corpus")]
fn corpus(path: PathBuf) -> PathBuf {
    assert!(
        path.exists(),
        "UNAVAILABLE: corpus fixture {} is not installed",
        path.display()
    );
    path
}

fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let f = std::fs::File::open(path).expect("open png");
    let dec = png::Decoder::new(std::io::BufReader::new(f));
    let mut reader = dec.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    let bytes = &buf[..info.buffer_size()];
    let mut rgba = vec![0u8; (info.width * info.height * 4) as usize];
    match info.color_type {
        png::ColorType::Rgba => rgba.copy_from_slice(bytes),
        png::ColorType::Rgb => {
            for i in 0..(info.width * info.height) as usize {
                rgba[i * 4..i * 4 + 3].copy_from_slice(&bytes[i * 3..i * 3 + 3]);
                rgba[i * 4 + 3] = 255;
            }
        }
        other => panic!("unsupported preview colour type {other:?}"),
    }
    (info.width, info.height, rgba)
}

#[test]
fn container_invariants_hold_on_every_sample() {
    for name in ["csp_simple.clip", "emptyimage.clip"] {
        let path = samples().join(name);
        let data = std::fs::read(&path).unwrap();
        let c = chunk::parse(&data).unwrap();
        assert_eq!(c.head.version, 256, "{name}");
        assert!(!c.exta.is_empty(), "{name} has no external chunks");
        assert!(!c.sqlite.is_empty(), "{name} has no SQLite chunk");
        assert_eq!(
            c.head.sqlite_offset as usize,
            c.sqlite.start - 16,
            "{name}: header points at the CHNKSQLi tag"
        );
        assert!(data.len() >= c.sqlite.end, "{name}");
    }
}

#[test]
fn roundtrip_is_byte_identical() {
    for name in ["csp_simple.clip", "emptyimage.clip"] {
        let src = samples().join(name);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("rt.clip");
        let mut w = ClipWriter::load(&src).unwrap();
        w.save(&out).unwrap();
        let a = std::fs::read(&src).unwrap();
        let b = std::fs::read(&out).unwrap();
        assert_eq!(a, b, "{name} is not byte-identical after round-trip");
    }
}

#[test]
fn render_matches_the_preview_csp_stored() {
    let src = samples().join("csp_simple.clip");
    let file = ClipFile::open(&src).unwrap();
    let preview = file.preview_png().expect("CanvasPreview");
    let dir = tempfile::tempdir().unwrap();
    let preview_path = dir.path().join("preview.png");
    std::fs::write(&preview_path, preview).unwrap();
    let (pw, ph, want) = read_png(&preview_path);

    let img = render(
        &file,
        &RenderOptions { scale: 100.0, background: Some([255, 255, 255, 255]), include_hidden: false },
    )
    .unwrap();
    assert_eq!((img.width, img.height), (pw, ph));

    let mut mean = 0f64;
    let mut max = 0u8;
    let mut changed = 0usize;
    for (got, exp) in img.data.chunks_exact(4).zip(want.chunks_exact(4)) {
        for c in 0..3 {
            let d = got[c].abs_diff(exp[c]);
            if d > 0 {
                changed += 1;
            }
            mean += d as f64;
            max = max.max(d);
        }
    }
    mean /= (pw * ph * 3) as f64;
    assert_eq!(max, 0, "render differs from CanvasPreview (mean {mean}, {changed} channels)");
}

/// A save that cannot publish must leave the previous document byte-identical.
#[test]
fn failed_save_preserves_existing_document() {
    use std::os::unix::fs::PermissionsExt;
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("locked.clip");
    std::fs::copy(&src, &target).unwrap();
    let original = std::fs::read(&target).unwrap();

    let mut w = ClipWriter::load(&target).unwrap();
    let (lw, lh) = w.layer_surface_size(6).unwrap();
    let pixels = vec![9u8; (lw * lh * 4) as usize];
    w.set_pixels(6, &pixels, lw, lh).unwrap();

    // A read-only directory makes the temporary replacement fail before the
    // destination could be touched.
    let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
    perms.set_mode(0o500);
    std::fs::set_permissions(dir.path(), perms).unwrap();
    let result = w.save(&target);
    let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(dir.path(), perms).unwrap();

    assert!(result.is_err(), "a save into a read-only directory must fail");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        original,
        "the original document was modified by a failed save"
    );
}

/// Editing, saving, editing again and saving twice must publish the latest
/// pixels; a later no-op save must not restore the pre-save bytes.
#[test]
fn second_save_keeps_latest_edits() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("twice.clip");
    std::fs::copy(&src, &target).unwrap();

    let mut w = ClipWriter::load(&target).unwrap();
    let (lw, lh) = w.layer_surface_size(6).unwrap();
    let mut first = vec![0u8; (lw * lh * 4) as usize];
    for px in first.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 255, 255]);
    }
    w.set_pixels(6, &first, lw, lh).unwrap();
    w.save(&target).unwrap();

    let mut second = first.clone();
    for px in second.chunks_exact_mut(4) {
        px.copy_from_slice(&[255, 0, 0, 255]);
    }
    w.set_pixels(6, &second, lw, lh).unwrap();
    w.save(&target).unwrap();
    // A third save without changes must keep the second edit.
    w.save(&target).unwrap();

    let reopened = ClipFile::open(&target).unwrap();
    let layer = reopened.layers.get(&6).unwrap();
    let chain = reopened.mip_chain(layer.render_mipmap).unwrap();
    let full = chain.iter().find(|m| (m.scale - 100.0).abs() < 1e-6).unwrap();
    let off = reopened.offscreen(full.offscreen).unwrap().unwrap();
    let decoded = reopened.decode_surface(&off).unwrap();
    let red = decoded
        .chunks_exact(4)
        .filter(|px| px[0] > 200 && px[1] < 40 && px[2] < 40)
        .count();
    assert!(
        red > decoded.len() / 4 / 2,
        "the second edit was not published: {red} red pixels"
    );
    drop(w);
}

/// The published file must reopen with the pixels that were just saved and
/// stay valid.
#[test]
fn atomic_save_reopens_with_latest_pixels() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("atomic.clip");
    std::fs::copy(&src, &target).unwrap();
    let mut w = ClipWriter::load(&target).unwrap();
    let (lw, lh) = w.layer_surface_size(6).unwrap();
    let mut pixels = vec![0u8; (lw * lh * 4) as usize];
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 255, 0, 255]);
    }
    w.set_pixels(6, &pixels, lw, lh).unwrap();
    w.save(&target).unwrap();
    drop(w);
    let reopened = ClipFile::open(&target).unwrap();
    assert!(reopened.validate().is_empty(), "the published file is invalid");
    let layer = reopened.layers.get(&6).unwrap();
    let chain = reopened.mip_chain(layer.render_mipmap).unwrap();
    let full = chain.iter().find(|m| (m.scale - 100.0).abs() < 1e-6).unwrap();
    let off = reopened.offscreen(full.offscreen).unwrap().unwrap();
    let decoded = reopened.decode_surface(&off).unwrap();
    assert!(
        decoded
            .chunks_exact(4)
            .filter(|px| px[1] > 200 && px[0] < 40 && px[2] < 40)
            .count()
            > decoded.len() / 4 / 2,
        "reopening the atomically saved file lost the edited pixels"
    );
}

#[test]
fn layer_pixels_survive_a_save_and_reopen() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pixels.clip");
    let file = ClipFile::open(&src).unwrap();
    let layer = 6;
    let mut w = ClipWriter::load(&src).unwrap();
    let (lw, lh) = w.layer_surface_size(layer).unwrap();
    let mut pixels = vec![0u8; (lw * lh * 4) as usize];
    for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
        if i % 17 == 0 {
            px.copy_from_slice(&[0, 200, 40, 255]);
        }
    }
    w.set_pixels(layer, &pixels, lw, lh).unwrap();
    w.save(&out).unwrap();
    let reopened = ClipFile::open(&out).unwrap();
    let layer_ref = reopened.layers.get(&layer).unwrap();
    let chain = reopened.mip_chain(layer_ref.render_mipmap).unwrap();
    let full = chain.iter().find(|m| (m.scale - 100.0).abs() < 1e-6).unwrap();
    let off = reopened.offscreen(full.offscreen).unwrap().unwrap();
    let decoded = reopened.decode_surface(&off).unwrap();
    assert_eq!(decoded.len(), pixels.len());
    // Straight RGBA on disk; the writer must be pixel-exact for content.
    for (i, (a, b)) in decoded.chunks_exact(4).zip(pixels.chunks_exact(4)).enumerate() {
        if a[3] != 0 || b[3] != 0 {
            assert_eq!(a, b, "pixel {i} changed during save");
        }
    }
    assert!(reopened.validate().is_empty());
    let _ = file;
}

#[test]
fn added_layer_is_linked_and_removable() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let with_layer = dir.path().join("added.clip");
    let mut w = ClipWriter::load(&src).unwrap();
    let before = ClipFile::open(&src).unwrap().layers.len();
    let id = w.add_raster_layer(3, "Test layer").unwrap();
    w.refresh_preview(25.0).unwrap();
    w.save(&with_layer).unwrap();

    let f = ClipFile::open(&with_layer).unwrap();
    assert_eq!(f.layers.len(), before + 1);
    let added = f.layers.get(&id).expect("new layer row");
    assert_eq!(added.name, "Test layer");
    assert!(added.is_raster());
    assert!(!f.mip_chain(added.render_mipmap).unwrap().is_empty());
    assert!(f.validate().is_empty());
    // The new layer must be the sibling immediately above layer 3.
    assert_eq!(f.layers.get(&3).unwrap().next, id);
    // Its pixels are explicit zeros, not the template's init fill colour.
    let full = f
        .mip_chain(added.render_mipmap)
        .unwrap()
        .into_iter()
        .find(|m| (m.scale - 100.0).abs() < 1e-6)
        .unwrap();
    let off = f.offscreen(full.offscreen).unwrap().unwrap();
    assert!(!off.attribute.init.has_color);
    let pixels = f.decode_surface(&off).unwrap();
    assert!(pixels.iter().all(|&b| b == 0), "new layer must start transparent");

    let without = dir.path().join("removed.clip");
    let mut w = ClipWriter::load(&with_layer).unwrap();
    w.delete_layer(id).unwrap();
    w.save(&without).unwrap();
    let f2 = ClipFile::open(&without).unwrap();
    assert_eq!(f2.layers.len(), before);
    assert!(f2.layers.get(&id).is_none());
    assert!(f2.validate().is_empty());
}

#[test]
fn preview_refresh_changes_canvas_preview_only_via_rendering() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("refreshed.clip");
    let mut w = ClipWriter::load(&src).unwrap();
    w.refresh_preview(25.0).unwrap();
    w.save(&out).unwrap();
    let f = ClipFile::open(&out).unwrap();
    let png = f.preview_png().expect("preview exists");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let img = render(&f, &RenderOptions { scale: 25.0, background: Some([255; 4]), include_hidden: false }).unwrap();
    // The stored preview is the same composite at the same scale.
    let preview_path = dir.path().join("p.png");
    std::fs::write(&preview_path, png).unwrap();
    let (pw, ph, want) = read_png(&preview_path);
    assert_eq!((img.width, img.height), (pw, ph));
    let mut max = 0u8;
    for (g, e) in img.data.chunks_exact(4).zip(want.chunks_exact(4)) {
        for c in 0..3 {
            max = max.max(g[c].abs_diff(e[c]));
        }
    }
    assert_eq!(max, 0, "refreshed preview is not the rendered composite");
}

#[test]
fn layer_mask_hides_pixels_and_survives_reopen() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let masked = dir.path().join("masked.clip");
    let mut w = ClipWriter::load(&src).unwrap();
    let layer = 6; // top pink fill over the green layer
    w.add_layer_mask(layer).unwrap();
    let (mw, mh) = w.mask_surface_size(layer).unwrap().unwrap();
    let mut mask = vec![255u8; (mw * mh * 4) as usize];
    for y in 0..mh {
        for x in 0..mw / 2 {
            let i = ((y * mw + x) * 4 + 3) as usize;
            mask[i] = 0;
        }
    }
    w.set_mask_pixels(layer, &mask, mw, mh).unwrap();
    w.refresh_preview(100.0).unwrap();
    w.save(&masked).unwrap();

    let f = ClipFile::open(&masked).unwrap();
    let img = render(&f, &RenderOptions { scale: 100.0, background: Some([255; 4]), include_hidden: false }).unwrap();
    let left = img.data[((600 * img.width + 400) * 4) as usize..][..3].to_vec();
    let right = img.data[((600 * img.width + 1200) * 4) as usize..][..3].to_vec();
    assert_eq!(left, vec![135, 225, 105], "masked half must reveal the green layer");
    assert_eq!(right, vec![255, 190, 250], "unmasked half must keep the pink layer");

    // The mask mip must round-trip as an alpha-only surface.
    let layer_ref = f.layers.get(&layer).unwrap();
    let chain = f.mip_chain(layer_ref.mask_mipmap).unwrap();
    let full = chain.iter().find(|m| (m.scale - 100.0).abs() < 1e-6).unwrap();
    let off = f.offscreen(full.offscreen).unwrap().unwrap();
    assert_eq!(off.attribute.channels, 0);
    let decoded = f.decode_surface(&off).unwrap();
    let a_left = decoded[((600 * mw + 100) * 4 + 3) as usize];
    let a_right = decoded[((600 * mw + 1400) * 4 + 3) as usize];
    assert_eq!(a_left, 0);
    assert_eq!(a_right, 255);

    // Removing the mask restores the original composite.
    let removed = dir.path().join("removed.clip");
    let mut w = ClipWriter::load(&masked).unwrap();
    w.remove_layer_mask(layer).unwrap();
    w.save(&removed).unwrap();
    let f2 = ClipFile::open(&removed).unwrap();
    let img2 = render(&f2, &RenderOptions { scale: 100.0, background: Some([255; 4]), include_hidden: false }).unwrap();
    assert_eq!(&img2.data[((600 * img2.width + 400) * 4) as usize..][..3], &[255, 190, 250]);
    assert!(f2.validate().is_empty());
}

#[test]
fn clipping_uses_the_masked_base_alpha() {
    let src = samples().join("csp_simple.clip");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("clip.clip");
    let mut w = ClipWriter::load(&src).unwrap();
    // Mask the green layer (5): left half hidden.
    w.add_layer_mask(5).unwrap();
    let (mw, mh) = w.mask_surface_size(5).unwrap().unwrap();
    let mut mask = vec![255u8; (mw * mh * 4) as usize];
    for y in 0..mh {
        for x in 0..mw / 2 {
            mask[((y * mw + x) * 4 + 3) as usize] = 0;
        }
    }
    w.set_mask_pixels(5, &mask, mw, mh).unwrap();
    // Clip the pink layer (6) to the green base.
    w.set_layer_clip(6, true).unwrap();
    w.refresh_preview(100.0).unwrap();
    w.save(&out).unwrap();

    let f = ClipFile::open(&out).unwrap();
    let img = render(&f, &RenderOptions { scale: 100.0, background: Some([255; 4]), include_hidden: false }).unwrap();
    let px = |x: usize, y: usize| img.data[((y * img.width as usize + x) * 4)..][..3].to_vec();
    // Base mask hides the green layer on the left, so the clipped pink layer
    // is invisible there and the black layer below shows through.
    assert_eq!(px(400, 600), vec![8, 8, 8]);
    assert_eq!(px(1200, 600), vec![255, 190, 250]);
    assert!(f.validate().is_empty());
}

/// Native CSP text layers carry their source string; Sumi must read it and be
/// able to rewrite it without disturbing the rest of the document.
#[cfg(feature = "corpus")]
#[test]
fn native_text_layers_can_be_listed_and_rewritten() {
    let path = corpus(samples().join("extracted/lolita-en_PRO/lolita-en_01.clip"));
    let file = ClipFile::open(&path).expect("the corpus document must open");
    let texts: Vec<(i64, String)> = file
        .layers
        .values()
        .filter_map(|layer| {
            file.text_layer(layer.main_id)
                .ok()
                .flatten()
                .map(|text| (layer.main_id, text.text))
        })
        .collect();
    assert!(
        texts.len() >= 3,
        "the corpus document should carry several text layers, found {}",
        texts.len()
    );
    assert!(
        texts.iter().any(|(_, text)| !text.trim().is_empty()),
        "a text layer must expose its source string"
    );

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("edited.clip");
    let mut writer = ClipWriter::load(&path).expect("the corpus document must load");
    let (edited_id, _) = texts[0].clone();
    writer
        .set_text_string(edited_id, "Sumi editou este texto")
        .expect("the writer must accept a new source string");
    writer.save(&out).expect("the edited document must save");

    let reopened = ClipFile::open(&out).expect("the edited document must reopen");
    let edited = reopened
        .text_layer(edited_id)
        .expect("the text column must still be readable")
        .expect("the edited layer must still be a text layer");
    assert_eq!(
        edited.text, "Sumi editou este texto",
        "the rewritten string must survive the save"
    );
    for (other_id, original) in &texts[1..] {
        let other = reopened
            .text_layer(*other_id)
            .expect("text layers stay readable")
            .expect("text layers stay text");
        assert_eq!(
            &other.text, original,
            "rewriting one layer must not touch another ({other_id})"
        );
    }
}

#[cfg(feature = "corpus")]
#[test]
fn adjustment_layers_expose_their_filter_settings() {
    use clip_core::adjust::{Filter, FilterKind};
    let tsuaii = corpus(samples().join("extracted/tsuaii/tsuaii.clip"));
    let f = ClipFile::open(&tsuaii).unwrap();
    let layer = f
        .layers
        .values()
        .find(|l| l.is_adjustment())
        .expect("tsuaii has a color-balance layer");
    let filter = Filter::parse(layer.filter_layer_info.as_deref().unwrap()).unwrap();
    match filter.kind {
        FilterKind::ColorBalance(cb) => {
            assert!(cb.preserve_luminosity);
            assert_eq!(cb.shadows, [-25.0, 2.0, 7.0]);
            assert_eq!(cb.midtones, [10.0, 2.0, 9.0]);
            assert_eq!(cb.highlights, [9.0, -1.0, -7.0]);
        }
        other => panic!("expected color balance, got {other:?}"),
    }

    let lolita = corpus(samples().join("extracted/lolita-en_PRO/lolita-en_01.clip"));
    let f = ClipFile::open(&lolita).unwrap();
    let layer = f
        .layers
        .values()
        .find(|l| l.is_adjustment())
        .expect("lolita has an HSV layer");
    let filter = Filter::parse(layer.filter_layer_info.as_deref().unwrap()).unwrap();
    assert_eq!(
        filter.kind,
        FilterKind::Hsv { hue: 0.0, saturation: -40.0, luminosity: 33.0 }
    );
}

#[cfg(feature = "corpus")]
#[test]
fn vector_layers_expose_stroke_geometry() {
    let src = corpus(samples().join("extracted/lolita-en_PRO/lolita-en_01.clip"));
    let file = ClipFile::open(&src).unwrap();
    let objects = file.vector_objects(61).unwrap();
    assert!(objects.len() > 500, "expected hundreds of strokes, got {}", objects.len());
    let first = &objects[0];
    assert_eq!(first.points.len(), 6);
    let [x, y] = first.points[0];
    let [x0, y0, x1, y1] = first.bbox;
    assert!(x >= x0 as f64 && x <= x1 as f64, "{x} outside bbox x {x0}..{x1}");
    assert!(y >= y0 as f64 && y <= y1 as f64, "{y} outside bbox y {y0}..{y1}");
    // Tone layers are not vector strokes; frame borders have their own layout.
    assert!(file.vector_objects(182).unwrap().len() > 3000);
}

#[cfg(feature = "corpus")]
#[test]
fn one_bit_masks_reveal_panel_windows() {
    let src = corpus(samples().join("extracted/lolita-en_PRO/lolita-en_01.clip"));
    let file = ClipFile::open(&src).unwrap();
    let layer = file.layers.get(&382).expect("Folder 7");
    let mask = clip_core::render::layer_mask(&file, layer, 25.0)
        .unwrap()
        .expect("panel mask");
    let covered = mask.data.chunks_exact(4).filter(|p| p[3] > 0).count();
    let frac = covered as f64 / (mask.width * mask.height) as f64;
    // A panel window, not a full reveal and not an empty strip.
    assert!(frac > 0.05 && frac < 0.6, "panel mask covers {frac}");
}

#[cfg(feature = "corpus")]
#[test]
fn cropped_surfaces_sum_layer_and_render_offsets() {
    let src = corpus(samples().join("extracted/lolita-en_PRO/lolita-en_01.clip"));
    let file = ClipFile::open(&src).unwrap();
    // Layer 440 stores a canvas-wide surface whose crop is recorded in the
    // render-offscreen offsets: (0,352) + (0,-352) = (0,0). Using only the
    // layer offset shifted every clipped colour layer on this page.
    let layer = file.layers.get(&440).expect("Layer 14");
    assert_eq!((layer.offset_x, layer.offset_y), (0, 352));
    assert_eq!((layer.render_offset_x, layer.render_offset_y), (0, -352));
    let surface = clip_core::render::layer_surface(&file, layer, 25.0)
        .unwrap()
        .expect("surface");
    assert_eq!(surface.offset, (0, 0));

    // Partial vector surfaces combine the same way, and their mask follows
    // the crop too.
    let vector = file.layers.get(&90).expect("Layer 8");
    let surface = clip_core::render::layer_surface(&file, vector, 25.0)
        .unwrap()
        .expect("vector surface");
    assert_eq!(surface.offset, (0, 0));
}

#[cfg(feature = "corpus")]
#[test]
fn editable_text_layers_decode_style_and_extra_runs() {
    let src = corpus(samples().join("extracted/lolita-en_PRO/lolita-en_01.clip"));
    let file = ClipFile::open(&src).unwrap();
    let main = file.text_layer(9).unwrap().expect("layer 9 has source text");
    assert!(main.text.starts_with("Are you done \r\nyet?"));
    let style = main.style.as_ref().expect("style");
    assert_eq!(style.font_family, "CCJoeKubert");
    assert_eq!(style.font_size, 9.0);
    assert_eq!(style.fill, [0, 0, 0]);
    // 9.00 pt at 600 dpi = 75 px per em.
    assert!((style.pixels_per_point - 600.0 / 72.0).abs() < 0.01);
    assert_eq!(main.extra_runs.len(), 1);
    assert_eq!(main.extra_runs[0].text, "I’m \r\nhungry!");

    // The same style block stores white for the older, hidden text run.
    let white = file.text_layer(15).unwrap().expect("layer 15 has source text");
    assert_eq!(white.style.as_ref().unwrap().fill, [255, 255, 255]);
    assert_eq!(white.style.as_ref().unwrap().font_size, 11.0);

    {
        let jose = corpus(samples().join("extracted/jose-en_PRO/jose-en_02.clip"));
        let file = ClipFile::open(&jose).unwrap();
        let smile = file.text_layer(267).unwrap().expect("SMILE layer");
        assert_eq!(smile.text, "SMILE!");
        assert_eq!(smile.style.as_ref().unwrap().font_size, 13.0);
        assert_eq!(smile.style.as_ref().unwrap().fill, [0, 0, 0]);
    }
}

#[cfg(feature = "corpus")]
#[test]
fn text_string_survives_a_writer_roundtrip() {
    let src = corpus(samples().join("extracted/jose-en_PRO/jose-en_02.clip"));
    let out = std::env::temp_dir().join(format!("clip-text-{}.clip", std::process::id()));
    {
        let mut writer = ClipWriter::load(&src).unwrap();
        writer.set_text_string(267, "HELLO\nFROM RUST").unwrap();
        writer.save(&out).unwrap();
    }
    let reopened = ClipFile::open(&out).unwrap();
    let text = reopened.text_layer(267).unwrap().expect("text layer");
    assert_eq!(text.text, "HELLO\r\nFROM RUST");
    std::fs::remove_file(&out).ok();
}
