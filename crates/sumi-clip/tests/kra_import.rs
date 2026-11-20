// Integration tests run beside the app, not in it: the workspace's no-panic lints are for shipped code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
use std::io::Write;
use std::path::PathBuf;

use sumi_clip::{ImportError, import};
use sumi_engine::document::{BlendMode, LayerKind};

/// Writes a `.kra` holding `maindoc.xml` and the named ZIP entries, made here, never a sample file.
fn fixture(name: &str, xml: &str, entries: &[(&str, Vec<u8>)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sumi-kra-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.kra"));

    let file = std::fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("mimetype", stored).unwrap();
    writer.write_all(b"application/x-krita").unwrap();
    writer.start_file("maindoc.xml", stored).unwrap();
    writer.write_all(xml.as_bytes()).unwrap();
    for (entry, data) in entries {
        writer.start_file(*entry, stored).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    path
}

/// One uncompressed 64x64 Krita tile: `RAW`, the first byte the compression flag (0), the rest
/// either interleaved BGRA or one coverage byte per pixel for a mask.
fn raster_tile(pixel_size: usize, pixel: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
    let side = 64;
    let mut out = format!(
        "VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE {pixel_size}\nDATA 1\n0,0,RAW,{}\n",
        1 + side * side * pixel_size
    )
    .into_bytes();
    out.push(0);
    for y in 0..side {
        for x in 0..side {
            let [r, g, b, a] = pixel(x, y);
            if pixel_size == 1 {
                out.push(a);
            } else {
                out.extend_from_slice(&[b, g, r, a]);
            }
        }
    }
    out
}

/// A group whose one hidden, offset layer is at (2, 1), above a semi-transparent bottom layer with
/// a transparency mask covering its left half.
fn stack() -> PathBuf {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="6" x-res="300" y-res="300" name="fixture" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="128" compositeop="multiply" nodetype="grouplayer" name="Group"
          filename="group0" uuid="{3a7e0001-0000-4000-8000-000000000001}">
    <layers>
     <layer visible="0" opacity="128" compositeop="screen" nodetype="paintlayer" name="Inside"
            filename="layer0" uuid="{3a7e0001-0000-4000-8000-000000000002}" x="2" y="1"/>
    </layers>
   </layer>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Bottom"
          filename="layer1" uuid="{3a7e0001-0000-4000-8000-000000000003}" x="0" y="0">
    <masks>
     <mask visible="1" nodetype="transparencymask" name="Mask" filename="mask0" x="0" y="0"/>
    </masks>
   </layer>
  </layers>
 </IMAGE>
</DOC>"#;
    let inside = raster_tile(4, |x, y| if (x, y) == (0, 0) { [200, 100, 50, 255] } else { [0; 4] });
    let bottom = raster_tile(4, |_, _| [200, 100, 50, 128]);
    let mask = raster_tile(1, |x, _| [0, 0, 0, if x < 4 { 255 } else { 0 }]);
    fixture(
        "stack",
        xml,
        &[
            ("fixture/layers/layer0", inside),
            ("fixture/layers/layer1", bottom),
            ("fixture/layers/mask0.pixelselection", mask),
        ],
    )
}

#[test]
fn a_kra_layer_tree_keeps_groups_masks_modes_and_pixels() {
    let path = stack();
    let imported = import(&path).unwrap();
    assert_eq!((imported.document.width(), imported.document.height()), (8, 6));
    assert_eq!(imported.name, "stack");
    assert!((imported.document.dpi - 300.0).abs() < 0.01, "the canvas's resolution");
    assert!(!imported.document.background_visible, "a .kra has no paper layer");
    assert!(imported.warnings.is_empty(), "no layer needed rasterizing");

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
    assert_eq!(bottom.blend, BlendMode::Normal);
    assert!((bottom.opacity - 1.0).abs() < 1e-6);
    assert!(bottom.visible);
    assert!(!inside.visible, "the hidden layer stays hidden");
    assert!((inside.opacity - 128.0 / 255.0).abs() < 1e-4);
    assert_eq!(inside.blend, BlendMode::Screen);
    assert!(group.visible);
    assert!((group.opacity - 128.0 / 255.0).abs() < 1e-4, "the group's own opacity");
    assert_eq!(group.blend, BlendMode::Multiply);
    assert_eq!(imported.pixels.len(), 3, "two paint layers and one mask, nothing for the group");

    let pixels_of = |id| imported.pixels.iter().find(|(layer, _)| *layer == id).map(|(_, pixels)| pixels.as_slice());
    let at = |pixels: &[u8], x: usize, y: usize| {
        let i = (y * 8 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    let bottom_pixels = pixels_of(bottom.id).expect("the bottom layer's pixels");
    assert_eq!(bottom_pixels.len(), 8 * 6 * 4);
    // straight 200,100,50 at half alpha, premultiplied for the engine
    assert_eq!(at(bottom_pixels, 0, 0), [100, 50, 25, 128]);
    assert_eq!(at(bottom_pixels, 7, 5), [100, 50, 25, 128]);

    let mask_pixels = pixels_of(mask.id).expect("the mask's pixels");
    assert_eq!(at(mask_pixels, 0, 0), [255, 255, 255, 255], "covered: grey, opaque");
    assert_eq!(at(mask_pixels, 3, 5), [255, 255, 255, 255]);
    assert_eq!(at(mask_pixels, 4, 0), [0, 0, 0, 255], "uncovered: black, still opaque");
    assert_eq!(at(mask_pixels, 7, 5), [0, 0, 0, 255]);

    let inside_pixels = pixels_of(inside.id).expect("the inside layer's pixels");
    assert_eq!(at(inside_pixels, 2, 1), [200, 100, 50, 255], "the layer's pixel after its offset");
    let painted =
        (0..6).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| at(inside_pixels, x, y) != [0; 4]).count();
    assert_eq!(painted, 1, "nothing else is painted");

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_file_layer_is_refused_by_name() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="6" x-res="72" y-res="72" name="file" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="filelayer" name="Linked" filename="layer0" x="0" y="0"/>
  </layers>
 </IMAGE>
</DOC>"#;
    let path = fixture("linked", xml, &[]);
    match import(&path) {
        Err(ImportError::Unsupported(message)) => {
            assert!(message.contains("Linked"), "the layer is named: {message}");
        }
        Err(other) => panic!("expected Unsupported, got {other}"),
        Ok(_) => panic!("a file layer must not be imported"),
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
