// Integration tests run beside the app, not in it: the workspace's no-panic lints are for shipped code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
use std::io::Write;
use std::path::{Path, PathBuf};

use sumi_clip::{ImportError, ImportWarning, import};
use sumi_engine::document::{BlendMode, LayerKind, layer_cap_for};

/// A `.kra` written for one test, removed with its directory when the test ends.
struct Temp {
    directory: PathBuf,
    path: PathBuf,
}

impl Temp {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Writes `maindoc.xml` and the named ZIP entries, made here, never a sample file.
fn fixture(name: &str, xml: &str, entries: &[(&str, Vec<u8>)]) -> Temp {
    write_fixture(name, xml, entries, zip::CompressionMethod::Stored)
}

/// The same fixture with every entry deflated, as a `.kra` writes its tiles.
fn fixture_deflated(name: &str, xml: &str, entries: &[(&str, Vec<u8>)]) -> Temp {
    write_fixture(name, xml, entries, zip::CompressionMethod::Deflated)
}

/// Writes one `.kra` whose entries carry `method`.
fn write_fixture(name: &str, xml: &str, entries: &[(&str, Vec<u8>)], method: zip::CompressionMethod) -> Temp {
    let directory = std::env::temp_dir().join(format!("sumi-kra-preflight-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}.kra"));

    let file = std::fs::File::create(&path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    writer.start_file("mimetype", options).unwrap();
    writer.write_all(b"application/x-krita").unwrap();
    writer.start_file("maindoc.xml", options).unwrap();
    writer.write_all(xml.as_bytes()).unwrap();
    for (entry, data) in entries {
        writer.start_file(*entry, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    Temp { directory, path }
}

/// A document whose `<layers>` block is `layers`, on an 8x6 canvas named `canvas`.
fn document(colorspace: &str, layers: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="6" x-res="120" y-res="120" name="canvas" colorspacename="{colorspace}">
  <layers>
{layers}
  </layers>
 </IMAGE>
</DOC>"#
    )
}

/// The same document, its `IMAGE` declaring an embedded colour profile.
fn profiled(profile: &str, layers: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="6" x-res="120" y-res="120" name="canvas" colorspacename="RGBA"
        profile="{profile}">
  <layers>
{layers}
  </layers>
 </IMAGE>
</DOC>"#
    )
}

/// One uncompressed 64x64 Krita tile: `RAW`, the first byte the compression flag (0), the rest
/// either interleaved BGRA, two bytes for an intentionally unsupported depth, or one coverage byte
/// per pixel for a mask. The body always matches the `DATA` size the header declares.
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
            } else if pixel_size == 2 {
                out.extend_from_slice(&[b, g]);
            } else {
                out.extend_from_slice(&[b, g, r, a]);
            }
        }
    }
    out
}

/// One uncompressed 64x64 RGBA tile at `at`, as the row of a tile table plus its payload. The
/// colour is straight RGBA, stored as the reader's BGRA like `raster_tile`.
fn raster_row(at: (i64, i64), pixel: [u8; 4]) -> Vec<u8> {
    let side = 64;
    let [r, g, b, a] = pixel;
    let mut out = format!("{},{},RAW,{}\n", at.0, at.1, 1 + side * side * 4).into_bytes();
    out.push(0);
    for _ in 0..side * side {
        out.extend_from_slice(&[b, g, r, a]);
    }
    out
}

/// A valid header over one complete but short tile: its payload is present (so the preflight's
/// table check passes) while it decodes to fewer bytes than a tile, which makes the reader's
/// decoder fail and drop the whole layer without a word.
fn truncated_tile() -> Vec<u8> {
    let mut out = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n0,0,RAW,100\n".to_vec();
    out.extend_from_slice(&[0; 100]);
    out
}

/// A pass-through group holding one Screen paint layer, above a masked bottom layer, every identity
/// the reader needs present.
fn grouped() -> Temp {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="grouplayer" passthrough="1"
          name="Pass" filename="group0" uuid="{6f1d0001-0000-4000-8000-000000000001}">
    <layers>
     <layer visible="1" opacity="255" compositeop="screen" nodetype="paintlayer" name="Inside"
            filename="layer0" uuid="{6f1d0001-0000-4000-8000-000000000002}" x="1" y="1"/>
    </layers>
   </layer>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Bottom"
          filename="layer1" uuid="{6f1d0001-0000-4000-8000-000000000003}" x="0" y="0">
    <masks>
     <mask visible="1" nodetype="transparencymask" name="Mask" filename="mask0" x="0" y="0"/>
    </masks>
   </layer>"#,
    );
    fixture(
        "grouped",
        &xml,
        &[
            (
                "canvas/layers/layer0",
                raster_tile(4, |x, y| if (x, y) == (0, 0) { [200, 100, 50, 255] } else { [0; 4] }),
            ),
            ("canvas/layers/layer1", raster_tile(4, |_, _| [200, 100, 50, 128])),
            ("canvas/layers/mask0.pixelselection", raster_tile(1, |x, _| [0, 0, 0, if x < 4 { 255 } else { 0 }])),
        ],
    )
}

/// The message of the refusal, which must never be a successful import.
fn refusal(name: &str, xml: &str, entries: &[(&str, Vec<u8>)]) -> String {
    let file = fixture(name, xml, entries);
    match import(file.path()) {
        Ok(_) => panic!("{name} must not import"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_known_grouped_masked_kra_still_imports_with_exact_layers_and_pixels() {
    let file = grouped();
    let imported = import(file.path()).unwrap();
    assert_eq!((imported.document.width(), imported.document.height()), (8, 6));
    assert_eq!(imported.name, "grouped");
    assert!((imported.document.dpi - 120.0).abs() < 0.01);
    assert!(!imported.document.background_visible, "a .kra has no paper layer");
    assert!(imported.warnings.is_empty(), "nothing of this file is lost");

    let layers = imported.document.layers();
    let order: Vec<(&str, LayerKind)> = layers.iter().map(|l| (l.name.as_str(), l.kind)).collect();
    assert_eq!(
        order,
        [
            ("Bottom", LayerKind::Paint),
            ("Mask", LayerKind::Mask),
            ("Inside", LayerKind::Paint),
            ("Pass", LayerKind::Group),
        ],
        "bottom first, the mask above its layer, the group above what it holds"
    );
    let [bottom, mask, inside, group] = layers else { panic!("four layers") };
    assert_eq!(mask.parent, Some(bottom.id));
    assert_eq!(inside.parent, Some(group.id));
    assert_eq!(group.parent, None);
    assert_eq!(inside.blend, BlendMode::Screen);
    assert_eq!(group.blend, BlendMode::Normal, "a pass-through group is Normal here");
    assert_eq!(imported.pixels.len(), 3, "two paint layers and one mask, nothing for the group");

    let pixels_of = |id| imported.pixels.iter().find(|(layer, _)| *layer == id).map(|(_, pixels)| pixels.as_slice());
    let at = |pixels: &[u8], x: usize, y: usize| {
        let i = (y * 8 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let bottom_pixels = pixels_of(bottom.id).unwrap();
    assert_eq!(at(bottom_pixels, 0, 0), [100, 50, 25, 128], "premultiplied at half alpha");
    let mask_pixels = pixels_of(mask.id).unwrap();
    assert_eq!(at(mask_pixels, 0, 0), [255, 255, 255, 255], "covered");
    assert_eq!(at(mask_pixels, 7, 5), [0, 0, 0, 255], "uncovered");
    let inside_pixels = pixels_of(inside.id).unwrap();
    assert_eq!(at(inside_pixels, 1, 1), [200, 100, 50, 255], "the layer's pixel after its offset");
}

#[test]
fn a_group_without_a_node_type_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" name="Ungrouped" filename="group0"
          uuid="{6f1d0002-0000-4000-8000-000000000001}">
    <layers>
     <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Child"
            filename="layer0" uuid="{6f1d0002-0000-4000-8000-000000000002}"/>
    </layers>
   </layer>"#,
    );
    let message = refusal("no-nodetype", &xml, &[]);
    assert!(message.contains("Ungrouped"), "the group is named: {message}");
    assert!(message.contains("node type"), "the missing declaration is named: {message}");
}

#[test]
fn a_layer_without_a_filename_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Nameless"
          uuid="{6f1d0003-0000-4000-8000-000000000001}"/>"#,
    );
    let message = refusal("no-filename", &xml, &[]);
    assert!(message.contains("Nameless"), "the layer is named: {message}");
    assert!(message.contains("filename"), "the missing attribute is named: {message}");
}

#[test]
fn two_layers_sharing_a_filename_are_refused() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="First"
          filename="layer0" uuid="{6f1d0004-0000-4000-8000-000000000001}"/>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Second"
          filename="layer0" uuid="{6f1d0004-0000-4000-8000-000000000002}"/>"#,
    );
    let message = refusal("duplicate-filename", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [0; 4]))]);
    assert!(message.contains("layer0"), "the shared filename is named: {message}");
}

#[test]
fn a_clone_of_a_missing_uuid_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="clonelayer" name="Copy"
          filename="clone0" uuid="{6f1d0005-0000-4000-8000-000000000001}"
          clonefromuuid="{6f1d0005-0000-4000-8000-0000000000ff}"/>"#,
    );
    let message = refusal("clone-missing", &xml, &[]);
    assert!(message.contains("Copy"), "the clone is named: {message}");
    assert!(message.contains("does not hold"), "the dangling reference is named: {message}");
}

#[test]
fn a_clone_cycle_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="clonelayer" name="Loop"
          filename="clone0" uuid="{6f1d0006-0000-4000-8000-000000000001}"
          clonefromuuid="{6f1d0006-0000-4000-8000-000000000001}"/>"#,
    );
    let message = refusal("clone-cycle", &xml, &[]);
    assert!(message.contains("Loop"), "the clone is named: {message}");
    assert!(message.contains("cycle"), "the cycle is named: {message}");
}

#[test]
fn a_cmyk_document_is_refused_by_name() {
    let xml = document(
        "CMYK",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Print"
          filename="layer0" uuid="{6f1d0007-0000-4000-8000-000000000001}"/>"#,
    );
    let message = refusal("cmyk", &xml, &[]);
    assert!(message.contains("CMYK"), "the colour model is named: {message}");
}

#[test]
fn an_erase_blend_mode_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="erase" nodetype="paintlayer" name="Eraser"
          filename="layer0" uuid="{6f1d0008-0000-4000-8000-000000000001}"/>"#,
    );
    let message = refusal("erase", &xml, &[]);
    assert!(message.contains("erase"), "the blend mode is named: {message}");
    assert!(message.contains("Eraser"), "the layer is named: {message}");
}

#[test]
fn a_two_byte_paint_tile_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Paint"
          filename="layer0" uuid="{6f1d0009-0000-4000-8000-000000000001}"/>"#,
    );
    // The entry's body now matches the two bytes per pixel its header declares, so it passes the
    // archive and table bounds and reaches the preflight's byte-per-pixel refusal.
    let message = refusal("pixelsize2", &xml, &[("canvas/layers/layer0", raster_tile(2, |_, _| [0; 4]))]);
    assert!(message.contains("Paint"), "the layer is named: {message}");
    assert!(message.contains("2 bytes per pixel"), "the stored depth is named: {message}");
}

#[test]
fn a_transform_mask_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Moved"
          filename="layer0" uuid="{6f1d000a-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="1" nodetype="transformmask" name="Transform" filename="mask0"/>
    </masks>
   </layer>"#,
    );
    let message = refusal("transformmask", &xml, &[]);
    assert!(message.contains("Moved"), "the layer is named: {message}");
    assert!(message.contains("transform mask"), "the unsupported mask is named: {message}");
}

#[test]
fn a_filter_mask_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Filtered"
          filename="layer0" uuid="{6f1d000b-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="1" nodetype="filtermask" filtername="blur" name="Blur" filename="mask0"/>
    </masks>
   </layer>"#,
    );
    let message = refusal("filtermask", &xml, &[]);
    assert!(message.contains("Filtered"), "the layer is named: {message}");
    assert!(message.contains("blur"), "the filter is named: {message}");
}

#[test]
fn a_mask_the_file_switched_off_is_not_a_loss() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Plain"
          filename="layer0" uuid="{6f1d000c-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="0" nodetype="transformmask" name="Transform" filename="mask0"/>
    </masks>
   </layer>"#,
    );
    let file = fixture("hidden-mask", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).unwrap();
    assert!(imported.warnings.is_empty(), "an inert mask is absent, not lost");
    assert_eq!(imported.document.layers().len(), 1, "only the paint layer");
}

#[test]
fn a_hidden_transparency_mask_is_inert_on_import_but_said() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Plain"
          filename="layer0" uuid="{6f1d0015-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="0" nodetype="transparencymask" name="Mask" filename="mask0" x="0" y="0"/>
    </masks>
   </layer>"#,
    );
    let file = fixture(
        "hidden-transparency",
        &xml,
        &[
            ("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255])),
            // Fully transparent coverage: applied, this mask would erase the whole layer.
            ("canvas/layers/mask0.pixelselection", raster_tile(1, |_, _| [0, 0, 0, 0])),
        ],
    );
    let imported = import(file.path()).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::MaskHiddenNotRead { layer: "Plain".to_owned() }],
        "the reader skips a mask the file switched off, and its absence is announced"
    );
    let layers = imported.document.layers();
    assert_eq!(layers.len(), 1, "the switched-off mask is not part of the stack");
    let painted = imported
        .pixels
        .iter()
        .find(|(id, _)| *id == layers[0].id)
        .map(|(_, pixels)| pixels.as_slice())
        .expect("the paint layer's pixels");
    assert_eq!(&painted[0..4], [10, 20, 30, 255], "the layer is imported without attenuation");
}

/// A hidden transform mask whose own parameters flip the layer: if the reader ignored `visible`,
/// the top half would land in the bottom half.
#[test]
fn a_hidden_transform_mask_moves_no_pixel() {
    let xml = document_sized(
        8,
        8,
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Paint"
          filename="layer0" uuid="{6f1d001a-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="0" nodetype="transformmask" name="Flip" filename="mask0" x="0" y="0"/>
    </masks>
   </layer>"#,
    );
    let config = r#"<!DOCTYPE transform_params>
<transform_params>
 <main id="tooltransformparams"/>
 <data mode="0">
  <free_transform>
   <transformedCenter type="pointf" x="4" y="4"/>
   <originalCenter type="pointf" x="4" y="4"/>
   <rotationCenterOffset type="pointf" x="0" y="0"/>
   <transformAroundRotationCenter type="value" value="0"/>
   <aX type="value" value="0"/>
   <aY type="value" value="0"/>
   <aZ type="value" value="0"/>
   <scaleX type="value" value="1"/>
   <scaleY type="value" value="-1"/>
   <shearX type="value" value="0"/>
   <shearY type="value" value="0"/>
   <keepAspectRatio type="value" value="0"/>
   <flattenedPerspectiveTransform m33="1" type="transform" m31="0" m12="0" m32="0" m21="0" m11="1" m13="0" m22="1" m23="0"/>
   <filterId type="value" value="Bicubic"/>
  </free_transform>
 </data>
</transform_params>"#;
    let file = fixture(
        "hidden-transform",
        &xml,
        &[
            ("canvas/layers/layer0", raster_tile(4, |_, y| if y < 4 { [255, 255, 255, 255] } else { [0; 4] })),
            ("canvas/layers/mask0.transformconfig", config.as_bytes().to_vec()),
        ],
    );
    let imported = import(file.path()).unwrap();
    assert!(
        imported.warnings.is_empty(),
        "a switched-off transform mask changes no pixel, so nothing is lost: {:?}",
        imported.warnings
    );
    let layer = imported.document.layers().first().expect("one layer");
    let pixels = imported
        .pixels
        .iter()
        .find(|(id, _)| *id == layer.id)
        .map(|(_, pixels)| pixels.as_slice())
        .expect("the layer's pixels");
    let alpha = |x: usize, y: usize| pixels[(y * 8 + x) * 4 + 3];
    assert_eq!(alpha(2, 1), 255, "the painted top half stays where it was");
    assert_eq!(alpha(2, 6), 0, "nothing was flipped into the bottom half");
}

/// A hidden Desaturate filter mask with full coverage: if the reader ignored `visible`, the red
/// layer would come back grey.
#[test]
fn a_hidden_filter_mask_changes_no_pixel() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Red"
          filename="layer0" uuid="{6f1d001b-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="0" nodetype="filtermask" filtername="desaturate" name="Grey" filename="mask0" x="0" y="0"/>
    </masks>
   </layer>"#,
    );
    let config = r#"<!DOCTYPE params>
<params version="0">
 <param type="string" name="type"><![CDATA[1]]></param>
</params>"#;
    let file = fixture(
        "hidden-filter",
        &xml,
        &[
            // Straight RGBA red: the tile writer swaps it to the reader's BGRA.
            ("canvas/layers/layer0", raster_tile(4, |_, _| [255, 0, 0, 255])),
            ("canvas/layers/mask0.filterconfig", config.as_bytes().to_vec()),
            ("canvas/layers/mask0.pixelselection", raster_tile(1, |_, _| [0, 0, 0, 255])),
        ],
    );
    let imported = import(file.path()).unwrap();
    assert!(
        imported.warnings.is_empty(),
        "a switched-off filter mask changes no pixel, so nothing is lost: {:?}",
        imported.warnings
    );
    let layer = imported.document.layers().first().expect("one layer");
    let pixels = imported
        .pixels
        .iter()
        .find(|(id, _)| *id == layer.id)
        .map(|(_, pixels)| pixels.as_slice())
        .expect("the layer's pixels");
    assert_eq!(&pixels[0..4], [255, 0, 0, 255], "red stays red, not the grey the filter would give");
}

#[test]
fn a_layer_colour_profile_is_reported_like_the_document_one() {
    let layer = r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Paint"
          filename="layer0" uuid="{6f1d001c-0000-4000-8000-000000000001}" profile="Adobe RGB (1998)"/>"#;
    let xml = document("RGBA", layer);
    let file =
        fixture("profile-layer-foreign", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::ProfileNotApplied { profile: "Adobe RGB (1998)".to_owned() }],
        "a layer's own profile is not applied either, and that is said"
    );

    let layer = r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Paint"
          filename="layer0" uuid="{6f1d001d-0000-4000-8000-000000000001}" profile="sRGB built-in"/>"#;
    let xml = document("RGBA", layer);
    let file =
        fixture("profile-layer-srgb", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).unwrap();
    assert!(imported.warnings.is_empty(), "a layer that is sRGB needs no transform: {:?}", imported.warnings);
}

#[test]
fn a_foreign_colour_profile_is_imported_as_srgb_with_a_warning() {
    let xml = profiled("Adobe RGB (1998)", &paint_layer("Paint", "layer0", "6f1d0016-0000-4000-8000-000000000001"));
    let file = fixture("profile-foreign", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::ProfileNotApplied { profile: "Adobe RGB (1998)".to_owned() }],
        "the reader never applies the profile: the pixels are taken as sRGB, and that is said"
    );
}

#[test]
fn known_srgb_profiles_are_silent_and_a_linear_gamma_one_is_not() {
    for (index, profile) in
        ["sRGB built-in", "sRGB-elle-V2-srgbtrc.icc", "sRGB-elle-V4-srgbtrc.icc"].into_iter().enumerate()
    {
        let xml = profiled(profile, &paint_layer("Paint", "layer0", "6f1d0017-0000-4000-8000-000000000001"));
        let file = fixture(
            &format!("profile-srgb-{index}"),
            &xml,
            &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))],
        );
        let imported = import(file.path()).unwrap();
        assert!(imported.warnings.is_empty(), "{profile} is the sRGB the reader assumes: {:?}", imported.warnings);
    }

    let linear = "sRGB-elle-V2-g10.icc";
    let xml = profiled(linear, &paint_layer("Paint", "layer0", "6f1d0018-0000-4000-8000-000000000001"));
    let file = fixture("profile-linear", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::ProfileNotApplied { profile: linear.to_owned() }],
        "a linear-gamma profile is not the transfer the import assumes"
    );
}

#[test]
fn a_document_without_a_profile_attribute_is_not_reported() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0019-0000-4000-8000-000000000001"));
    let file = fixture("profile-absent", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).unwrap();
    assert!(imported.warnings.is_empty(), "no profile is declared, so none is contradicted: {:?}", imported.warnings);
}

#[test]
fn a_layer_the_reader_drops_is_refused_not_skipped() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Lost"
          filename="layer0" uuid="{6f1d000d-0000-4000-8000-000000000001}"/>"#,
    );
    let message = refusal("dropped", &xml, &[("canvas/layers/layer0", truncated_tile())]);
    assert!(message.contains("Lost"), "the dropped layer is named: {message}");
    assert!(message.contains("dropped"), "the loss is named: {message}");
}

#[test]
fn an_isolated_group_whose_children_show_it_is_imported_with_a_warning() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="grouplayer" passthrough="0"
          name="Isolated" filename="group0" uuid="{6f1d000e-0000-4000-8000-000000000001}">
    <layers>
     <layer visible="1" opacity="255" compositeop="screen" nodetype="paintlayer" name="Inside"
            filename="layer0" uuid="{6f1d000e-0000-4000-8000-000000000002}"/>
    </layers>
   </layer>"#,
    );
    let file = fixture("isolated", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [1, 2, 3, 255]))]);
    let imported = import(file.path()).unwrap();
    assert_eq!(
        imported.warnings,
        [ImportWarning::GroupIsolationLost { layer: "Isolated".to_owned() }],
        "the engine renders this group pass-through, which the file is not"
    );
}

#[test]
fn a_pass_through_group_at_partial_opacity_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="128" compositeop="normal" nodetype="grouplayer" passthrough="1"
          name="Translucent" filename="group0" uuid="{6f1d000f-0000-4000-8000-000000000001}">
    <layers>
     <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Inside"
            filename="layer0" uuid="{6f1d000f-0000-4000-8000-000000000002}"/>
    </layers>
   </layer>"#,
    );
    let message = refusal("pass-through-opacity", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [0; 4]))]);
    assert!(message.contains("Translucent"), "the group is named: {message}");
    assert!(message.contains("opacity"), "the lost property is named: {message}");
}

#[test]
fn an_inherit_alpha_layer_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Clipped"
          filename="layer0" uuid="{6f1d0010-0000-4000-8000-000000000001}" inheritAlpha="1"/>"#,
    );
    let message = refusal("inherit-alpha", &xml, &[]);
    assert!(message.contains("Clipped"), "the layer is named: {message}");
    assert!(message.contains("alpha"), "the lost clipping is named: {message}");
}

#[test]
fn a_mask_placed_away_from_the_origin_is_refused_by_name() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Displaced"
          filename="layer0" uuid="{6f1d0011-0000-4000-8000-000000000001}">
    <masks>
     <mask visible="1" nodetype="transparencymask" name="Mask" filename="mask0" x="3" y="2"/>
    </masks>
   </layer>"#,
    );
    let message = refusal("mask-offset", &xml, &[]);
    assert!(message.contains("Displaced"), "the layer is named: {message}");
    assert!(message.contains("3,2"), "the mask's own position is named: {message}");
}

#[test]
fn a_malformed_maindoc_is_unreadable_not_a_success() {
    let file = fixture("not-zip", "this is not XML", &[]);
    match import(file.path()) {
        Err(ImportError::Unreadable(message)) => {
            assert!(message.contains("maindoc.xml"), "the reason is named: {message}");
        }
        Err(other) => panic!("expected Unreadable, got {other}"),
        Ok(_) => panic!("a malformed maindoc.xml must not import"),
    }
}

/// A document of `width` by `height` whose `<layers>` block is `layers`.
fn document_sized(width: u64, height: u64, colorspace: &str, layers: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="{width}" height="{height}" x-res="120" y-res="120" name="canvas" colorspacename="{colorspace}">
  <layers>
{layers}
  </layers>
 </IMAGE>
</DOC>"#
    )
}

/// A tiled stream whose first five rows are literally `header`, followed by `body`.
fn tiled(header: &str, body: &[u8]) -> Vec<u8> {
    let mut out = header.as_bytes().to_vec();
    out.extend_from_slice(body);
    out
}

/// One paint layer's `<layer>` element, with an identity the reader can keep.
fn paint_layer(name: &str, filename: &str, uuid: &str) -> String {
    format!(
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="{name}"
          filename="{filename}" uuid="{{{uuid}}}"/>"#
    )
}

#[test]
fn a_giant_canvas_is_refused_before_the_reader_allocates() {
    let xml = document_sized(200_000, 200_000, "RGBA", "");
    let message = refusal("giant", &xml, &[]);
    assert!(message.contains("200000"), "the declared size is named: {message}");
    assert!(message.contains("larger than"), "the refusal is about size: {message}");
}

#[test]
fn an_empty_canvas_is_refused_by_its_size() {
    let xml = document_sized(0, 0, "RGBA", "");
    let message = refusal("empty-canvas", &xml, &[]);
    assert!(message.contains("0 by 0"), "the declared size is named: {message}");
}

#[test]
fn an_absurd_tile_side_is_refused_before_the_reader_decodes_a_tile() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0012-0000-4000-8000-000000000001"));
    let header = "VERSION 2\nTILEWIDTH 65535\nTILEHEIGHT 65535\nPIXELSIZE 4\nDATA 1\n0,0,LZF,70000\n";
    let message = refusal("tile-side", &xml, &[("canvas/layers/layer0", tiled(header, &[0, 1, 2]))]);
    assert!(message.contains("TILEWIDTH"), "the offending row is named: {message}");
    assert!(message.contains("65535"), "the offending value is named: {message}");
    assert!(message.contains("1024"), "the bound is named: {message}");
}

/// Krita's canvas has no edge: a brush stroke past it is stored as tiles the canvas does not
/// cover, and the reader clips every tile it places (`x < 0 || x >= width`). An 8 by 6 canvas
/// holds one 64 by 64 tile, so `DATA 2` already names more tiles than its grid; one tile paints
/// the canvas and the other sits entirely beside it. Both must import, and the clipping must leave
/// only the tile that overlaps the canvas.
#[test]
fn tiles_outside_the_canvas_are_imported_and_clipped() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0013-0000-4000-8000-000000000001"));
    let mut stream = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 2\n".to_vec();
    stream.extend(raster_row((0, 0), [255, 0, 0, 255]));
    stream.extend(raster_row((64, 0), [0, 0, 255, 255]));
    let file = fixture("off-canvas", &xml, &[("canvas/layers/layer0", stream)]);
    let imported = import(file.path()).expect("a tile outside the canvas is clipped, not refused");
    let layer = imported.document.layers().first().expect("one layer");
    let pixels = imported
        .pixels
        .iter()
        .find(|(id, _)| *id == layer.id)
        .map(|(_, pixels)| pixels.as_slice())
        .expect("the layer's pixels");
    let at = |x: usize, y: usize| {
        let i = (y * 8 + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    assert_eq!(at(0, 0), [255, 0, 0, 255], "the tile on the canvas stays where it was");
    assert_eq!(at(7, 5), [255, 0, 0, 255], "and covers what the canvas shows of it");
    let painted = (0..6).flat_map(|y| (0..8).map(move |x| (x, y))).filter(|&(x, y)| at(x, y) != [0; 4]).count();
    assert_eq!(painted, 8 * 6, "the tile beside the canvas painted nothing inside it");
}

/// A stream that lists more tiles than its own bytes can hold rows: every row takes at least eight
/// bytes (`x,y,s,n` and its newline), so a sixty-byte entry cannot name a thousand tiles. The
/// entry's own size is the bound here; the canvas bounds nothing.
#[test]
fn a_tile_stream_listing_more_rows_than_its_entry_holds_is_refused() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0018-0000-4000-8000-000000000001"));
    let header = "VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1000\n";
    let message = refusal("tile-rows", &xml, &[("canvas/layers/layer0", tiled(header, &[]))]);
    assert!(message.contains("1000"), "the count it claims is named: {message}");
    assert!(message.contains("rows"), "the bound it breaks is named: {message}");
}

#[test]
fn no_import_error_names_the_private_snapshot() {
    let giant = document_sized(200_000, 200_000, "RGBA", "");
    let empty = document_sized(0, 0, "RGBA", "");
    let absurd = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0014-0000-4000-8000-000000000001"));
    let header = "VERSION 2\nTILEWIDTH 65535\nTILEHEIGHT 65535\nPIXELSIZE 4\nDATA 1\n0,0,LZF,70000\n";
    let cases = [
        ("snapshot-giant", giant, Vec::new()),
        ("snapshot-empty", empty, Vec::new()),
        ("snapshot-tile", absurd, vec![("canvas/layers/layer0", tiled(header, &[0, 1, 2]))]),
    ];
    for (name, xml, entries) in cases {
        let borrowed: Vec<(&str, Vec<u8>)> = entries;
        let file = fixture(name, &xml, &borrowed);
        let message = match import(file.path()) {
            Ok(_) => panic!("{name} must not import"),
            Err(error) => error.to_string(),
        };
        assert!(!message.contains("sumi-clip-kra-"), "the private snapshot path does not reach the person: {message}");
    }
}

/// The private snapshot directories this process may have under the temporary directory.
fn snapshot_directories() -> std::collections::HashSet<PathBuf> {
    match std::fs::read_dir(std::env::temp_dir()) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("sumi-clip-kra-"))
            })
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    }
}

#[test]
fn a_snapshot_directory_is_removed_after_a_successful_and_a_failed_import() {
    let before = snapshot_directories();
    let good = grouped();
    import(good.path()).expect("a known good file");
    drop(good);

    let giant = document_sized(200_000, 200_000, "RGBA", "");
    let bad = fixture("snapshot-cleanup", &giant, &[]);
    assert!(import(bad.path()).is_err(), "a giant canvas is refused");
    drop(bad);

    let leaked: Vec<PathBuf> = snapshot_directories().difference(&before).cloned().collect();
    assert!(leaked.is_empty(), "no snapshot directory survives the import: {leaked:?}");
}

#[cfg(unix)]
#[test]
fn a_socket_is_not_a_document_to_import() {
    let directory = std::env::temp_dir().join(format!("sumi-kra-socket-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("socket.kra");
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    match import(&path) {
        Err(ImportError::Unreadable(message)) => {
            assert!(message.contains("regular file"), "the kind of file is named: {message}");
        }
        Err(other) => panic!("expected Unreadable, got {other}"),
        Ok(_) => panic!("a socket is not a document"),
    }
    let _ = std::fs::remove_dir_all(directory);
}

// ---------------------------------------------------------------------------------------------
// Bounds: what the archive declares is refused before the reader allocates it
// ---------------------------------------------------------------------------------------------

/// A `.kra` of `bytes` length that never reaches the disk: the bound reads the length the metadata
/// declares, so a sparse file is a document exactly as large as it claims.
fn sparse(name: &str, bytes: u64) -> Temp {
    let directory = std::env::temp_dir().join(format!("sumi-kra-sparse-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}.kra"));
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(bytes).unwrap();
    drop(file);
    Temp { directory, path }
}

#[test]
fn a_document_larger_than_the_snapshot_limit_is_refused_before_it_is_copied() {
    let huge = sparse("huge", (2_u64 << 30) + 1);
    match import(huge.path()) {
        Err(ImportError::Unreadable(message)) => {
            assert!(message.contains("larger than"), "the limit is named: {message}");
            assert!(message.contains(&(2_u64 << 30).to_string()), "the declared size is named: {message}");
        }
        Err(other) => panic!("expected Unreadable, got {other}"),
        Ok(_) => panic!("a document of two GiB and a byte must not import"),
    }
}

/// The merged preview the reader decodes whatever the import does with it: bigger than the canvas it
/// belongs to, so the image it decodes would be bigger than the canvas too.
#[test]
fn a_merged_image_bigger_than_the_canvas_is_refused() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0020-0000-4000-8000-000000000001"));
    let message = refusal(
        "huge-merged",
        &xml,
        &[("canvas/layers/layer0", raster_tile(4, |_, _| [0; 4])), ("mergedimage.png", vec![0_u8; 80_000])],
    );
    assert!(message.contains("mergedimage.png"), "the entry is named: {message}");
    assert!(message.contains("uncompressed"), "the size it declares is named: {message}");
}

/// A real deflated tile entry is accepted. Compressible paint compresses far below the 1032:1 a
/// DEFLATE stream can reach, so the entry's declared size stays under its own compressed size
/// times that ceiling; a guard that confused the two would refuse what Krita writes.
#[test]
fn a_deflated_tile_entry_of_compressible_paint_is_accepted() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0019-0000-4000-8000-000000000001"));
    let file =
        fixture_deflated("deflated-tile", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [10, 20, 30, 255]))]);
    let imported = import(file.path()).expect("a deflated tile entry is what Krita writes");
    assert_eq!(imported.document.layers().len(), 1, "the layer survives");
}

/// A tile whose size the reader's `pos + size` would wrap around: it is refused here, where the
/// reader would slice backwards and panic on the entry.
#[test]
fn a_tile_whose_size_cannot_fit_the_entry_is_refused() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0023-0000-4000-8000-000000000001"));
    let entry =
        tiled("VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n0,0,RAW,18446744073709551615\n", &[]);
    let message = refusal("tile-wrap", &xml, &[("canvas/layers/layer0", entry)]);
    assert!(message.contains("18446744073709551615"), "the size it claims is named: {message}");
    assert!(message.contains("0,0"), "the tile it names is given: {message}");
}

/// A tile table that ends before the tiles its own header lists: the reader would drop the layer in
/// silence, so it is refused instead. `DATA 2` fits the entry's own bytes, so both declared rows are
/// plausible and only the missing second one is at fault.
#[test]
fn a_tile_table_that_ends_before_its_tiles_is_refused() {
    let xml = document("RGBA", &paint_layer("Paint", "layer0", "6f1d0024-0000-4000-8000-000000000001"));
    let entry = tiled("VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 4\nPIXELSIZE 4\nDATA 2\n0,0,RAW,10\n", &[0; 10]);
    let message = refusal("tile-table-short", &xml, &[("canvas/layers/layer0", entry)]);
    assert!(message.contains("ends before its tile table"), "the missing row is named: {message}");
}

/// More layers than a canvas this size may hold: refused while only `maindoc.xml` is read, so the
/// reader never builds one canvas-sized image per declared layer. Clones carry no tiles of their
/// own, so only the one source needs an archive entry; the count guard is the one under test.
#[test]
fn more_layers_than_the_canvas_holds_is_refused_before_the_reader_runs() {
    let cap = layer_cap_for(8, 6);
    let source_uuid = "6f1d0025-0000-4000-8000-000000000000";
    let mut layers = paint_layer("Source", "source0", source_uuid);
    for index in 0..cap {
        let uuid = format!("6f1d0026-0000-4000-8000-{:012x}", index + 1);
        layers.push_str(&format!(
            r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="clonelayer"
          name="Copy{index}" filename="layer{index}" uuid="{{{uuid}}}"
          clonefromuuid="{{{source_uuid}}}"/>"#
        ));
    }
    let entries = [("canvas/layers/source0", raster_tile(4, |_, _| [0; 4]))];
    let message = refusal("too-many-layers", &document("RGBA", &layers), &entries);
    assert!(message.contains("painting layers"), "the count is named: {message}");
    assert!(message.contains(&cap.saturating_add(1).to_string()), "the layer over the cap is named: {message}");
}

/// The reader exposes neither `locked` nor `collapsed`, so both are read from `maindoc.xml` and
/// said: dropping them in silence is the failure this guards.
#[test]
fn a_locked_or_collapsed_layer_is_said_rather_than_dropped() {
    let xml = document(
        "RGBA",
        r#"   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer" name="Kept"
          filename="layer0" uuid="{6f1d0026-0000-4000-8000-000000000001}" locked="1" collapsed="1"/>"#,
    );
    let file = fixture("locked", &xml, &[("canvas/layers/layer0", raster_tile(4, |_, _| [1, 2, 3, 255]))]);
    let imported = import(file.path()).unwrap();
    for what in ["lock", "collapsed state"] {
        let wanted = ImportWarning::StateNotRead { layer: "Kept".to_owned(), what };
        assert!(imported.warnings.contains(&wanted), "{wanted:?} is said: {:?}", imported.warnings);
    }
}
