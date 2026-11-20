// Integration tests run beside the app, not in it: the workspace's no-panic lints are for shipped code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
use std::path::PathBuf;

use clip_core::Image;
use clip_core::convert::{ConvertLayer, write_flat_clip};
use clip_core::write::ClipWriter;
use sumi_clip::import;
use sumi_engine::document::{BlendMode, LayerKind};

/// A small `.clip` made for the test: a full bottom layer and a hidden top layer carrying one
/// pixel, moved to (2, 1), at half opacity and multiply.
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sumi-clip-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.clip"));
    let bottom = Image::filled(8, 6, [10, 20, 30, 255]);
    let mut top = Image::new(8, 6);
    top.data[..4].copy_from_slice(&[200, 100, 50, 255]);
    let layers = [
        ConvertLayer { name: "Bottom", image: &bottom, opacity: 1.0, visible: true, composite: 0 },
        ConvertLayer { name: "Top", image: &top, opacity: 0.5, visible: false, composite: 2 },
    ];
    write_flat_clip(&path, 8, 6, &layers, false).unwrap();
    let mut writer = ClipWriter::load(&path).unwrap();
    let top_id: i64 =
        writer.db().query_row("select MainId from Layer where LayerName='Top'", [], |r| r.get(0)).unwrap();
    writer.set_layer_surface_offsets(top_id, 2, 1).unwrap();
    writer.save(&path).unwrap();
    path
}

#[test]
fn a_clip_layer_stack_keeps_order_visibility_offsets_and_pixels() {
    let path = fixture("stack");
    let imported = import(&path).unwrap();
    assert_eq!((imported.document.width(), imported.document.height()), (8, 6));
    assert_eq!(imported.name, "stack");
    assert!(imported.warnings.is_empty());

    let paint: Vec<_> = imported.document.layers().iter().filter(|l| l.kind == LayerKind::Paint).collect();
    assert_eq!(paint.len(), 2, "the paper layer is the canvas's background, not a paint layer");
    assert_eq!((paint[0].name.as_str(), paint[1].name.as_str()), ("Bottom", "Top"));
    assert!(paint[0].visible);
    assert!(!paint[1].visible, "the hidden layer stays hidden");
    assert!((paint[1].opacity - 0.5).abs() < 1e-6, "half opacity, from CSP's 0..=256 scale");
    assert_eq!(paint[1].blend, BlendMode::Multiply);
    assert!(imported.document.background_visible, "the paper layer is shown");

    let pixels_of = |id| imported.pixels.iter().find(|(layer, _)| *layer == id).map(|(_, pixels)| pixels.as_slice());
    let bottom = pixels_of(paint[0].id).expect("the bottom layer's pixels");
    assert_eq!(bottom, &[10, 20, 30, 255].repeat(8 * 6)[..]);
    let top = pixels_of(paint[1].id).expect("the top layer's pixels");
    assert_eq!(top.len(), 8 * 6 * 4);
    let at = |x: usize, y: usize| {
        let i = (y * 8 + x) * 4;
        [top[i], top[i + 1], top[i + 2], top[i + 3]]
    };
    assert_eq!(at(2, 1), [200, 100, 50, 255], "the layer's one pixel, moved by its offset");
    let painted = (0..6).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| at(x, y) != [0; 4]).count();
    assert_eq!(painted, 1, "nothing else is painted");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_mask_the_file_enables_but_holds_no_pixels_for_is_reported() {
    use sumi_clip::ImportWarning;

    let dir = std::env::temp_dir().join(format!("sumi-clip-maskless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("maskless.clip");

    let mut writer = ClipWriter::blank(8, 6).unwrap();
    let (paint, name): (i64, String) = writer
        .db()
        .query_row(
            "select MainId, LayerName from Layer where LayerType=1 and LayerFolder&1=0 \
             and LayerRenderMipmap is not null and LayerRenderMipmap<>0 limit 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    writer.add_layer_mask(paint).unwrap();
    // The file declares a mask and enables it (bit 2), but its mip chain names no Mipmap row, so
    // it carries no payload at all: `layer_mask` has nothing to read.
    writer.db().execute("update Layer set LayerLayerMaskMipmap=999999 where MainId=?1", [paint]).unwrap();
    writer.save(&path).unwrap();

    let imported = import(&path).unwrap();
    assert!(
        imported.warnings.contains(&ImportWarning::MaskNotRead { layer: name.clone() }),
        "the unreadable mask is named, not dropped in silence: {:?}",
        imported.warnings
    );
    assert!(
        !imported.document.layers().iter().any(|layer| layer.kind == LayerKind::Mask),
        "the layer comes back unmasked"
    );

    let _ = std::fs::remove_dir_all(dir);
}
