// Integration tests run beside the app, not in it: the workspace's no-panic lints are for shipped code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
use std::path::PathBuf;

use clip_core::Image;
use clip_core::blend;
use clip_core::write::ClipWriter;
use sumi_clip::{ImportError, ImportWarning, import};
use sumi_engine::document::{BlendMode, LayerKind};

/// A small `.clip` made for the test: a bottom layer with a mask whose coverage sits in its first
/// four columns, and a hidden half-opaque screen layer at (2, 1) inside a half-opaque multiply
/// group above it.
fn stack(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.clip"));

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let bottom: i64 = writer
        .db()
        .query_row("select MainId from Layer where LayerType=1 and LayerFolder&1=0 limit 1", [], |row| row.get(0))
        .unwrap();
    writer.set_layer_attr(bottom, Some("Bottom"), Some(256), Some(blend::MODE_NORMAL), Some(true)).unwrap();
    let solid = Image::filled(8, 6, [10, 20, 30, 255]);
    writer.set_pixels(bottom, &solid.data, 8, 6).unwrap();

    let group = writer.add_folder(0, "Group").unwrap();
    writer.set_layer_attr(group, None, Some(128), Some(blend::MODE_MULTIPLY), Some(true)).unwrap();

    let inside = writer.add_raster_layer_in(group, 0, "Inside").unwrap();
    // hidden, half opaque, screen; its one pixel is moved to (2, 1) below, after the mask exists
    writer.set_layer_attr(inside, None, Some(128), Some(blend::MODE_SCREEN), Some(false)).unwrap();
    let mut one = Image::new(8, 6);
    one.data[..4].copy_from_slice(&[200, 100, 50, 128]);
    writer.set_pixels(inside, &one.data, 8, 6).unwrap();
    writer.set_layer_surface_offsets(inside, 2, 1).unwrap();

    // The mask belongs to the bottom layer: a mask shows its own layer as much as it is light.
    writer.add_layer_mask(bottom).unwrap();
    let mut coverage = vec![0_u8; 8 * 6 * 4];
    for y in 0..6 {
        for (x, alpha) in [64_u8, 128, 192, 255].into_iter().enumerate() {
            let at = (y * 8 + x) * 4;
            coverage[at + 3] = alpha;
        }
    }
    writer.set_mask_pixels(bottom, &coverage, 8, 6).unwrap();
    writer.set_layer_mask_offset(bottom, 2, 1).unwrap();

    writer.save(&path).unwrap();
    path
}

#[test]
fn a_clip_layer_tree_keeps_groups_masks_modes_and_pixels() {
    let path = stack("stack");
    let imported = import(&path).unwrap();
    assert_eq!((imported.document.width(), imported.document.height()), (8, 6));
    assert_eq!(imported.name, "stack");
    assert!(imported.warnings.is_empty(), "nothing was rasterized");
    assert!(imported.document.background_visible, "the paper layer is shown");

    let layers = imported.document.layers();
    let order: Vec<(&str, LayerKind)> = layers.iter().map(|l| (l.name.as_str(), l.kind)).collect();
    assert_eq!(
        order,
        [
            ("Bottom", LayerKind::Paint),
            ("Mask", LayerKind::Mask),
            ("Inside", LayerKind::Paint),
            ("Group", LayerKind::Group),
        ],
        "bottom first, the mask just above its layer, the group above what it holds"
    );
    let [bottom, mask, inside, group] = layers else { panic!("four layers") };
    assert_eq!(bottom.parent, None);
    assert_eq!(mask.parent, Some(bottom.id), "the mask belongs to the bottom layer");
    assert_eq!(inside.parent, Some(group.id), "the inside layer is in the group");
    assert_eq!(group.parent, None);
    assert_eq!(imported.document.current(), inside.id, "the file's current paint layer");

    assert!(bottom.visible);
    assert!((bottom.opacity - 1.0).abs() < 1e-6);
    assert_eq!(bottom.blend, BlendMode::Normal);
    assert!(mask.visible);
    assert!((mask.opacity - 1.0).abs() < 1e-6);
    assert_eq!(mask.blend, BlendMode::Normal);
    assert!(!inside.visible, "the hidden layer stays hidden");
    assert!((inside.opacity - 0.5).abs() < 1e-6, "half opacity, from CSP's 0..=256 scale");
    assert_eq!(inside.blend, BlendMode::Screen);
    assert!(group.visible);
    assert!((group.opacity - 0.5).abs() < 1e-6, "the group's own opacity");
    assert_eq!(group.blend, BlendMode::Multiply);
    assert_eq!(imported.pixels.len(), 3, "two paint layers and one mask, nothing for the group");

    let pixels_of = |id| imported.pixels.iter().find(|(layer, _)| *layer == id).map(|(_, pixels)| pixels.as_slice());
    let at = |pixels: &[u8], x: usize, y: usize| {
        let i = (y * 8 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    let bottom_pixels = pixels_of(bottom.id).expect("the bottom layer's pixels");
    assert_eq!(bottom_pixels.len(), 8 * 6 * 4);
    assert_eq!(at(bottom_pixels, 0, 0), [10, 20, 30, 255]);
    assert_eq!(at(bottom_pixels, 7, 5), [10, 20, 30, 255]);

    let mask_pixels = pixels_of(mask.id).expect("the mask's pixels");
    assert_eq!(mask_pixels.len(), 8 * 6 * 4);
    assert_eq!(at(mask_pixels, 2, 1), [64, 64, 64, 255], "coverage, every channel, opaque");
    assert_eq!(at(mask_pixels, 3, 1), [128, 128, 128, 255]);
    assert_eq!(at(mask_pixels, 4, 1), [192, 192, 192, 255]);
    assert_eq!(at(mask_pixels, 5, 1), [255, 255, 255, 255]);
    assert_eq!(at(mask_pixels, 2, 5), [64, 64, 64, 255], "the mask's last row, moved down by one");
    assert_eq!(at(mask_pixels, 0, 0), [0; 4], "left of and above the mask's origin");
    assert_eq!(at(mask_pixels, 1, 1), [0; 4]);
    assert_eq!(at(mask_pixels, 2, 0), [0; 4]);
    assert_eq!(at(mask_pixels, 6, 1), [0, 0, 0, 255], "inside the mask's surface, uncovered");
    assert_eq!(at(mask_pixels, 7, 5), [0, 0, 0, 255]);

    let inside_pixels = pixels_of(inside.id).expect("the inside layer's pixels");
    // straight 200,100,50 at half alpha, premultiplied for the engine
    assert_eq!(at(inside_pixels, 2, 1), [100, 50, 25, 128], "the layer's pixel after its offset");
    let painted =
        (0..6).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| at(inside_pixels, x, y) != [0; 4]).count();
    assert_eq!(painted, 1, "nothing else is painted");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_pass_through_group_with_opacity_is_refused_by_name() {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-pass", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pass.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let group = writer.add_folder(0, "Overlay").unwrap();
    // `add_folder` writes CSP's pass-through composite; it cannot be kept at half opacity.
    writer.set_layer_attr(group, None, Some(128), None, Some(true)).unwrap();
    writer.save(&path).unwrap();

    match import(&path) {
        Err(ImportError::Unsupported(message)) => {
            assert!(message.contains("Overlay"), "the group is named: {message}");
        }
        Err(other) => panic!("expected Unsupported, got {other}"),
        Ok(_) => panic!("a half-opaque pass-through group must not be approximated"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_layer_clipped_to_a_group_is_reported_as_unclipped() {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-clipgroup", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clipgroup.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let bottom: i64 = writer
        .db()
        .query_row("select MainId from Layer where LayerType=1 and LayerFolder&1=0 limit 1", [], |row| row.get(0))
        .unwrap();
    writer.set_layer_attr(bottom, Some("Bottom"), Some(256), Some(blend::MODE_NORMAL), Some(true)).unwrap();
    writer.set_pixels(bottom, &Image::filled(8, 6, [10, 20, 30, 255]).data, 8, 6).unwrap();

    let group = writer.add_folder(0, "Group").unwrap();
    let inside = writer.add_raster_layer_in(group, 0, "Inside").unwrap();
    writer.set_pixels(inside, &Image::filled(8, 6, [10, 20, 30, 255]).data, 8, 6).unwrap();

    // the engine can only clip a paint layer to a paint layer: this one sits on a group
    let clipped = writer.add_raster_layer(group, "Clipped").unwrap();
    writer.set_layer_clip(clipped, true).unwrap();
    writer.save(&path).unwrap();

    let imported = import(&path).unwrap();
    let clipped = imported.document.layers().iter().find(|layer| layer.name == "Clipped").expect("the clipped layer");
    assert!(!clipped.clipped, "the engine cannot honour a clip onto a group");
    assert_eq!(imported.warnings, [ImportWarning::Unclipped { layer: "Clipped".to_owned() }]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_glow_blend_mode_is_approximated_and_named() {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-glow", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("glow.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let bottom: i64 = writer
        .db()
        .query_row("select MainId from Layer where LayerType=1 and LayerFolder&1=0 limit 1", [], |row| row.get(0))
        .unwrap();
    writer.set_layer_attr(bottom, Some("Bottom"), Some(256), Some(blend::MODE_NORMAL), Some(true)).unwrap();
    writer.set_pixels(bottom, &Image::filled(8, 6, [10, 20, 30, 255]).data, 8, 6).unwrap();
    let glow = writer.add_raster_layer(bottom, "Glow").unwrap();
    writer.set_layer_attr(glow, None, Some(256), Some(blend::MODE_ADD_GLOW), None).unwrap();
    writer.set_pixels(glow, &Image::filled(8, 6, [200, 100, 50, 255]).data, 8, 6).unwrap();
    writer.save(&path).unwrap();

    let imported = import(&path).unwrap();
    let glow_layer = imported.document.layers().iter().find(|layer| layer.name == "Glow").expect("the glow layer");
    assert_eq!(glow_layer.blend, BlendMode::Add, "the glow mode is approximated by the plain one");
    assert_eq!(imported.warnings, [ImportWarning::ApproximatedBlend { layer: "Glow".to_owned() }]);

    // a mode with no counterpart at all still refuses the document
    let unknown_path = dir.join("unknown.clip");
    let unknown = writer.add_raster_layer(glow, "Unknown").unwrap();
    writer.set_layer_attr(unknown, None, None, Some(99), None).unwrap();
    writer.save(&unknown_path).unwrap();
    match import(&unknown_path) {
        Err(ImportError::Unsupported(message)) => {
            assert!(message.contains("Unknown"), "the layer is named: {message}");
        }
        Err(other) => panic!("expected Unsupported, got {other}"),
        Ok(_) => panic!("an unknown blend mode must not be approximated"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_isolated_group_is_reported_as_pass_through() {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-isolated", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("isolated.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    // an isolated group: normal, fully opaque, holding a multiply layer the engine would let
    // through to what is below it
    let isolated = writer.add_folder(0, "Isolated").unwrap();
    writer.set_layer_attr(isolated, None, Some(256), Some(blend::MODE_NORMAL), Some(true)).unwrap();
    let blend_child = writer.add_raster_layer_in(isolated, 0, "Multiply child").unwrap();
    writer.set_layer_attr(blend_child, None, Some(256), Some(blend::MODE_MULTIPLY), Some(true)).unwrap();

    // a pass-through group whose children blend normally: the engine reproduces it exactly
    let pass = writer.add_folder(0, "Pass").unwrap();
    let plain_child = writer.add_raster_layer_in(pass, 0, "Plain child").unwrap();
    writer.set_layer_attr(plain_child, None, Some(256), Some(blend::MODE_NORMAL), Some(true)).unwrap();
    writer.save(&path).unwrap();

    let imported = import(&path).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::GroupIsolationLost { layer: "Isolated".to_owned() }],
        "only the isolated group whose contents can show the difference is named"
    );
    let isolated =
        imported.document.layers().iter().find(|layer| layer.name == "Isolated").expect("the isolated group");
    assert_eq!(isolated.blend, BlendMode::Normal, "the group keeps its mapped blend");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_group_mask_that_is_not_painted_is_not_an_error() {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-groupmask", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("groupmask.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let group = writer.add_folder(0, "Masked").unwrap();
    writer.add_folder_mask(group, 8, 6).unwrap();
    // `add_folder_mask` sets the visibility bit CSP paints a mask by; clearing it disables the mask
    writer
        .db()
        .execute("update Layer set LayerVisibility=coalesce(LayerVisibility,1) & ~2 where MainId=?1", [group])
        .unwrap();
    writer.save(&path).unwrap();

    let imported = import(&path).unwrap();
    assert!(imported.warnings.is_empty(), "a mask CSP does not paint is absent, not lost");
    let layers = imported.document.layers();
    assert!(
        layers.iter().any(|layer| layer.name == "Masked" && layer.kind == LayerKind::Group),
        "the group itself is kept"
    );
    assert!(
        !layers.iter().any(|layer| layer.kind == LayerKind::Mask),
        "the disabled mask attenuates nothing and is not imported"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_text_layer_is_refused_by_name() {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-text", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("text.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let raster: i64 = writer
        .db()
        .query_row("select MainId from Layer where LayerType=1 and LayerFolder&1=0 limit 1", [], |row| row.get(0))
        .unwrap();
    writer.set_layer_attr(raster, Some("Caption"), None, None, None).unwrap();
    // The blank template has no text column; add the one CSP stores the glyphs in.
    writer.db().execute("alter table Layer add column TextLayerString BLOB", []).unwrap();
    writer.set_text_string(raster, "Caption").unwrap();
    writer.save(&path).unwrap();

    match import(&path) {
        Err(ImportError::Unsupported(message)) => {
            assert!(message.contains("Caption"), "the text layer is named: {message}");
        }
        Err(other) => panic!("expected Unsupported, got {other}"),
        Ok(_) => panic!("a .clip text layer's bubble shape must not pass for its text"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.clip` holding one named paint layer with a full-coverage mask, the layer's lock flags and the
/// mask's visibility bit as the test asks for them.
fn masked(name: &str, lock_flags: Option<(bool, bool, bool)>, mask_painted: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sumi-clip-tree-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.clip"));

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let paint: i64 = writer
        .db()
        .query_row(
            "select MainId from Layer where LayerType=1 and LayerFolder&1=0 \
             and LayerRenderMipmap is not null and LayerRenderMipmap<>0 limit 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    writer.set_layer_attr(paint, Some("Inked"), Some(256), Some(blend::MODE_NORMAL), Some(true)).unwrap();
    writer.set_pixels(paint, &Image::filled(8, 6, [10, 20, 30, 255]).data, 8, 6).unwrap();
    if let Some((alpha, position, reference)) = lock_flags {
        writer.set_layer_locks(paint, alpha, position, reference, false).unwrap();
    }
    writer.add_layer_mask(paint).unwrap();
    let coverage = if mask_painted { 255 } else { 0 };
    let mut mask = vec![0_u8; 8 * 6 * 4];
    for px in mask.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, coverage]);
    }
    writer.set_mask_pixels(paint, &mask, 8, 6).unwrap();
    // `add_layer_mask` sets the visibility bit CSP paints a mask by; clearing it disables the mask.
    writer
        .db()
        .execute("update Layer set LayerVisibility=coalesce(LayerVisibility,1) & ~2 where MainId=?1", [paint])
        .unwrap();
    writer.save(&path).unwrap();
    path
}

/// A mask the file holds pixels for and switched off: the stack keeps no attenuation, but the mask
/// is state the artwork loses, so it is said instead of being dropped in silence.
#[test]
fn a_hidden_clip_mask_with_painted_coverage_is_said() {
    let path = masked("hidden-mask-painted", None, true);
    let imported = import(&path).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::MaskHiddenNotRead { layer: "Inked".to_owned() }],
        "the switched-off mask is named"
    );
    assert!(
        !imported.document.layers().iter().any(|layer| layer.kind == LayerKind::Mask),
        "the mask attenuates nothing, so no mask layer is made"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A switched-off mask with no coverage at all is absent rather than lost: there is nothing to say.
#[test]
fn a_hidden_clip_mask_with_no_coverage_says_nothing() {
    let path = masked("hidden-mask-empty", None, false);
    let imported = import(&path).unwrap();
    assert!(imported.warnings.is_empty(), "an empty mask is not a loss: {:?}", imported.warnings);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// CSP's `LayerLock` bits: the alpha lock and the reference flag are states the engine keeps, the
/// position lock is not, so it is said rather than dropped.
#[test]
fn a_clip_lock_flag_is_kept_where_the_engine_has_it_and_said_where_it_has_not() {
    let path = masked("locks", Some((true, true, true)), true);
    let imported = import(&path).unwrap();
    let layer = imported.document.layers().iter().find(|layer| layer.name == "Inked").expect("the paint layer");
    assert!(layer.alpha_lock, "the alpha lock is kept");
    assert!(layer.reference, "the reference-layer flag is kept");
    assert!(
        imported.warnings.contains(&ImportWarning::StateNotRead { layer: "Inked".to_owned(), what: "position lock" }),
        "the position lock has no field here, so it is said: {:?}",
        imported.warnings
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
