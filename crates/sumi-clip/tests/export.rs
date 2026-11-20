//! Export round trips: a native document written as `.clip` or `.kra` is read back and compared.
// Integration tests run beside the app, not in it: the workspace's no-panic lints are for shipped code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::io::Read;
use std::path::PathBuf;

use clip_core::kra::KraDocument;
use clip_core::{ClipFile, layer_mask};
use sumi_clip::{ExportError, ExportFormat, ExportWarning, ImportWarning, export, import};
use sumi_engine::document::{BlendMode, Document, Layer, LayerId, LayerKind, LayerText};

/// A layer as the tests build one: shown, opaque, normal unless set afterwards.
fn layer(id: u64, name: &str, kind: LayerKind, parent: Option<LayerId>) -> Layer {
    Layer {
        id: LayerId(id),
        name: name.to_owned(),
        visible: true,
        opacity: 1.0,
        blend: BlendMode::Normal,
        alpha_lock: false,
        clipped: false,
        locked: false,
        reference: false,
        kind,
        parent,
        collapsed: false,
        text: None,
        private: false,
        assisted: false,
        plugin: None,
    }
}

/// Canvas-sized premultiplied pixels from a straight source, with the engine's rounding.
fn premultiplied(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut out = vec![0_u8; (width * height) as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, a] = pixel(x, y);
            let at = ((y * width + x) * 4) as usize;
            let over = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
            out[at..at + 4].copy_from_slice(&[over(r), over(g), over(b), a]);
        }
    }
    out
}

/// A mask layer's pixels: opaque grey, light showing the layer.
fn mask_pixels(width: u32, height: u32, grey: impl Fn(u32, u32) -> u8) -> Vec<u8> {
    let mut out = vec![255_u8; (width * height) as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let at = ((y * width + x) * 4) as usize;
            let value = grey(x, y);
            out[at..at + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    out
}

fn directory(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sumi-export-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

fn at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    pixels[at..at + 4].try_into().expect("four channels")
}

fn near(got: [u8; 4], want: [u8; 4]) {
    for channel in 0..4 {
        let difference = i16::from(got[channel]) - i16::from(want[channel]);
        assert!(difference.abs() <= 1, "channel {channel}: got {got:?}, expected {want:?} within one step");
    }
}

/// An 8 × 6 document: a bottom paint layer with a gradient mask, a clipped half-opaque screen
/// layer with one pixel moved off the origin, and a half-opaque multiply group holding a hidden
/// half-opaque screen layer.
fn artwork() -> (Document, Vec<(LayerId, Vec<u8>)>) {
    let (width, height) = (8_u32, 6_u32);
    let bottom = LayerId(1);
    let mask = LayerId(2);
    let clipped = LayerId(3);
    let inside = LayerId(4);
    let group = LayerId(5);

    let bottom_layer = layer(1, "Bottom", LayerKind::Paint, None);
    let mask_layer = layer(2, "Mask", LayerKind::Mask, Some(bottom));
    let mut clipped_layer = layer(3, "Clipped", LayerKind::Paint, None);
    clipped_layer.blend = BlendMode::Screen;
    clipped_layer.opacity = 0.5;
    clipped_layer.clipped = true;
    let mut inside_layer = layer(4, "Inside", LayerKind::Paint, Some(group));
    inside_layer.blend = BlendMode::Screen;
    inside_layer.opacity = 0.5;
    inside_layer.visible = false;
    let mut group_layer = layer(5, "Group", LayerKind::Group, None);
    group_layer.blend = BlendMode::Multiply;
    group_layer.opacity = 0.5;

    let layers = vec![bottom_layer, mask_layer, clipped_layer, inside_layer, group_layer];
    let mut document = Document::from_layers(width, height, layers, clipped);
    document.dpi = 300.0;
    document.background = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0];
    document.background_visible = true;

    let mut pixels = Vec::new();
    pixels.push((bottom, premultiplied(width, height, |_, _| [10, 20, 30, 255])));
    pixels.push((
        mask,
        mask_pixels(width, height, |x, _| match x {
            0 => 64,
            1 => 128,
            2 => 192,
            3 => 255,
            _ => 0,
        }),
    ));
    pixels.push((
        clipped,
        premultiplied(width, height, |x, y| if (x, y) == (2, 1) { [200, 100, 50, 255] } else { [0; 4] }),
    ));
    pixels.push((
        inside,
        premultiplied(width, height, |x, y| if (x, y) == (5, 4) { [200, 100, 50, 128] } else { [0; 4] }),
    ));
    (document, pixels)
}

fn pixels_of(imported: &sumi_clip::ImportedArtwork, id: LayerId) -> Vec<u8> {
    imported
        .pixels
        .iter()
        .find(|(layer, _)| *layer == id)
        .map(|(_, pixels)| pixels.clone())
        .expect("the layer's pixels")
}

#[test]
fn a_clip_export_round_trips_through_import() {
    let (document, pixels) = artwork();
    let dir = directory("clip");
    let path = dir.join("stack.clip");
    let warnings = export(&path, ExportFormat::Clip, &document, &pixels, "stack").expect("a .clip");
    assert!(warnings.is_empty(), "{warnings:?}");
    // The produced file is one the reader accepts on its own.
    ClipFile::open(&path).expect("a readable .clip");

    let imported = import(&path).expect("the .clip again");
    assert_eq!((imported.document.width(), imported.document.height()), (8, 6));
    assert_eq!(imported.name, "stack");
    assert!((imported.document.dpi - 300.0).abs() < 0.01, "the canvas's resolution");
    assert!(imported.document.background_visible);
    for (got, want) in imported.document.background.iter().zip([51.0, 102.0, 153.0]) {
        assert!((got * 255.0 - want).abs() <= 1.0, "background {got} is not {want}/255");
    }

    let order: Vec<(&str, LayerKind)> =
        imported.document.layers().iter().map(|layer| (layer.name.as_str(), layer.kind)).collect();
    assert_eq!(
        order,
        [
            ("Bottom", LayerKind::Paint),
            ("Mask", LayerKind::Mask),
            ("Clipped", LayerKind::Paint),
            ("Inside", LayerKind::Paint),
            ("Group", LayerKind::Group),
        ],
        "bottom first, the mask above its layer, the group above what it holds"
    );

    let by_name = |name: &str| imported.document.layers().iter().find(|layer| layer.name == name).expect("the layer");
    let bottom = by_name("Bottom");
    let mask = by_name("Mask");
    let clipped = by_name("Clipped");
    let inside = by_name("Inside");
    let group = by_name("Group");

    assert_eq!(bottom.parent, None);
    assert_eq!(bottom.blend, BlendMode::Normal);
    assert!((bottom.opacity - 1.0).abs() < 1e-6);

    assert_eq!(mask.parent, Some(bottom.id), "the mask belongs to the bottom layer");

    assert_eq!(clipped.parent, None);
    assert!(clipped.clipped, "the file's clip is kept");
    assert_eq!(clipped.blend, BlendMode::Screen);
    assert!((clipped.opacity - 0.5).abs() < 1e-6);
    assert_eq!(imported.document.current(), clipped.id, "the file opens on the same layer");

    assert_eq!(inside.parent, Some(group.id));
    assert!(!inside.visible, "the hidden layer stays hidden");
    assert!((inside.opacity - 0.5).abs() < 1e-6);
    assert_eq!(inside.blend, BlendMode::Screen);

    assert_eq!(group.blend, BlendMode::Multiply);
    assert!((group.opacity - 0.5).abs() < 1e-6);

    let bottom_pixels = pixels_of(&imported, bottom.id);
    assert_eq!(at(&bottom_pixels, 8, 0, 0), [10, 20, 30, 255]);
    assert_eq!(at(&bottom_pixels, 8, 7, 5), [10, 20, 30, 255]);

    let clipped_pixels = pixels_of(&imported, clipped.id);
    assert_eq!(at(&clipped_pixels, 8, 2, 1), [200, 100, 50, 255], "the pixel after its offset");
    let painted = (0..6)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .filter(|&(x, y)| at(&clipped_pixels, 8, x, y) != [0; 4])
        .count();
    assert_eq!(painted, 1, "nothing else is painted");

    let inside_pixels = pixels_of(&imported, inside.id);
    near(at(&inside_pixels, 8, 5, 4), [100, 50, 25, 128]);

    let mask_pixels = pixels_of(&imported, mask.id);
    assert_eq!(at(&mask_pixels, 8, 0, 0), [64, 64, 64, 255]);
    assert_eq!(at(&mask_pixels, 8, 1, 5), [128, 128, 128, 255]);
    assert_eq!(at(&mask_pixels, 8, 2, 0), [192, 192, 192, 255]);
    assert_eq!(at(&mask_pixels, 8, 3, 5), [255, 255, 255, 255]);
    assert_eq!(at(&mask_pixels, 8, 4, 0), [0, 0, 0, 255], "uncovered, still opaque");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_kra_export_round_trips_through_import() {
    let (document, pixels) = artwork();
    // Krita has no clipping flag: the exported document is the same without it, and it opens on
    // the layer the import brings back as current, the topmost paint layer.
    let current = LayerId(4);
    let layers: Vec<Layer> = document
        .layers()
        .iter()
        .map(|layer| {
            let mut layer = layer.clone();
            if layer.clipped {
                layer.clipped = false;
            }
            layer
        })
        .collect();
    let mut document = Document::from_layers(document.width(), document.height(), layers, current);
    document.dpi = 300.0;
    document.background = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0];
    document.background_visible = true;

    let dir = directory("kra");
    let path = dir.join("stack.kra");
    let warnings = export(&path, ExportFormat::Kra, &document, &pixels, "stack").expect("a .kra");
    assert_eq!(warnings, vec![ExportWarning::BackgroundNotStored], "Krita has no paper to hold the background");

    let kra = KraDocument::open(&path).expect("a readable .kra");
    assert_eq!((kra.width, kra.height), (8, 6));
    assert!((kra.ppi - 300.0).abs() < 0.5, "the canvas's resolution");
    assert_eq!(kra.folders.len(), 1, "one group");
    assert_eq!(kra.root_children.len(), 3, "two root paint layers and the group");
    assert_eq!(kra.layers.len(), 3, "three paint layers, nothing for the group");

    let bottom = kra.layers.iter().find(|layer| layer.name == "Bottom").expect("Bottom");
    assert!((bottom.own_opacity - 1.0).abs() < 1e-6);
    let mask = bottom.mask.as_ref().expect("the bottom layer's mask");
    assert_eq!(at(&mask.data, 8, 0, 0), [0, 0, 0, 64], "the mask's coverage channel");
    assert_eq!(at(&mask.data, 8, 4, 0), [0, 0, 0, 0]);
    assert_eq!(at(&bottom.image.data, 8, 0, 0), [10, 20, 30, 255], "straight RGBA");

    let inside = kra.layers.iter().find(|layer| layer.name == "Inside").expect("Inside");
    assert!(!inside.own_visible);
    assert!((inside.own_opacity - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(inside.blend_mode, clip_core::blend::MODE_SCREEN);
    near(at(&inside.image.data, 8, 5, 4), [200, 100, 50, 128]);

    let file = std::fs::File::open(&path).expect("the written file");
    let mut zip = zip::ZipArchive::new(file).expect("a ZIP archive");
    let mut xml = String::new();
    zip.by_name("maindoc.xml").expect("maindoc.xml").read_to_string(&mut xml).expect("UTF-8");
    assert_eq!(xml.matches("selected=\"true\"").count(), 1, "one current layer");
    assert!(xml.contains("passthrough=\"0\""), "an isolated group");
    assert!(xml.contains("nodetype=\"transparencymask\""), "a transparency mask");
    assert!(xml.contains("profile=\"sRGB built-in\""), "the exported pixels are sRGB, and the file says so: {xml}");

    let imported = import(&path).expect("the .kra again");
    assert_eq!((imported.document.width(), imported.document.height()), (8, 6));
    assert!(!imported.document.background_visible, "a .kra has no paper layer");
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    assert_eq!(
        imported.document.layers().iter().map(|layer| (layer.name.as_str(), layer.kind)).collect::<Vec<_>>(),
        [
            ("Bottom", LayerKind::Paint),
            ("Mask", LayerKind::Mask),
            ("Clipped", LayerKind::Paint),
            ("Inside", LayerKind::Paint),
            ("Group", LayerKind::Group),
        ]
    );
    let by_name = |name: &str| imported.document.layers().iter().find(|layer| layer.name == name).expect("the layer");
    assert_eq!(imported.document.current(), by_name("Inside").id, "the topmost paint layer");
    let group = by_name("Group");
    assert_eq!(group.blend, BlendMode::Multiply);
    assert!((group.opacity - 128.0 / 255.0).abs() < 1e-6, "the group's own opacity");
    assert!(!by_name("Inside").visible);

    let inside_pixels = pixels_of(&imported, by_name("Inside").id);
    near(at(&inside_pixels, 8, 5, 4), [100, 50, 25, 128]);
    let mask_pixels = pixels_of(&imported, by_name("Mask").id);
    assert_eq!(at(&mask_pixels, 8, 0, 0), [64, 64, 64, 255]);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_pass_through_group_is_written_as_one_in_both_formats() {
    let paint = LayerId(1);
    let group = LayerId(2);
    let width = 4;
    let height = 3;
    // Normal and fully opaque: the engine renders it pass-through.
    let group_layer = layer(2, "Group", LayerKind::Group, None);
    let mut paint_layer = layer(1, "Paint", LayerKind::Paint, Some(group));
    paint_layer.visible = true;
    let mut document = Document::from_layers(width, height, vec![paint_layer, group_layer], paint);
    document.background_visible = false;
    let pixels = vec![(paint, premultiplied(width, height, |_, _| [90, 40, 10, 255]))];
    let dir = directory("pass-through");

    let clip = dir.join("flat.clip");
    export(&clip, ExportFormat::Clip, &document, &pixels, "flat").expect("a .clip");
    let imported = import(&clip).expect("the .clip again");
    let imported_group = imported.document.layers().iter().find(|layer| layer.name == "Group").expect("the group");
    assert!(imported_group.passes_through(), "mode 30 at full opacity is pass-through");

    let kra = dir.join("flat.kra");
    export(&kra, ExportFormat::Kra, &document, &pixels, "flat").expect("a .kra");
    let file = std::fs::File::open(&kra).expect("the written file");
    let mut zip = zip::ZipArchive::new(file).expect("a ZIP archive");
    let mut xml = String::new();
    zip.by_name("maindoc.xml").expect("maindoc.xml").read_to_string(&mut xml).expect("UTF-8");
    assert!(xml.contains("passthrough=\"1\""), "{xml}");
    let imported = import(&kra).expect("the .kra again");
    let imported_group = imported.document.layers().iter().find(|layer| layer.name == "Group").expect("the group");
    assert!(imported_group.passes_through());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_kra_export_refuses_a_clipped_layer_by_name() {
    let (document, pixels) = artwork();
    let dir = directory("kra-clip");
    let path = dir.join("stack.kra");
    match export(&path, ExportFormat::Kra, &document, &pixels, "stack") {
        Err(ExportError::Unrepresentable { layer, what, .. }) => {
            assert_eq!(layer, "Clipped");
            assert!(what.contains("clipping"), "{what}");
        }
        Err(other) => panic!("expected an unrepresentable clipping, got {other}"),
        Ok(_) => panic!("Krita has no clipping flag: the export must be refused"),
    }
    assert!(!path.exists(), "a refused export writes no file");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_shade_mode_is_refused_by_name_in_both_formats() {
    let (document, pixels) = artwork();
    let layers: Vec<Layer> = document
        .layers()
        .iter()
        .map(|layer| {
            let mut layer = layer.clone();
            if layer.name == "Bottom" {
                layer.blend = BlendMode::Shade;
            }
            layer
        })
        .collect();
    let document = Document::from_layers(document.width(), document.height(), layers, document.current());
    let dir = directory("shade");
    for format in [ExportFormat::Clip, ExportFormat::Kra] {
        let path = dir.join(match format {
            ExportFormat::Clip => "shade.clip",
            ExportFormat::Kra => "shade.kra",
        });
        match export(&path, format, &document, &pixels, "shade") {
            Err(ExportError::Unrepresentable { layer, what, .. }) => {
                assert_eq!(layer, "Bottom");
                assert!(what.contains("Shade"), "{what}");
            }
            Err(other) => panic!("expected an unrepresentable blend mode, got {other}"),
            Ok(_) => panic!("Shade has no exact counterpart: the export must be refused"),
        }
        assert!(!path.exists(), "a refused export writes no file");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_text_layer_is_written_as_pixels_with_a_warning() {
    let (document, pixels) = artwork();
    let bottom = LayerId(1);
    let layers: Vec<Layer> = document
        .layers()
        .iter()
        .map(|layer| {
            let mut layer = layer.clone();
            if layer.id == bottom {
                layer.text = Some(LayerText::default());
            }
            layer
        })
        .collect();
    let document = Document::from_layers(document.width(), document.height(), layers, document.current());

    let dir = directory("text");
    let path = dir.join("text.clip");
    let warnings = export(&path, ExportFormat::Clip, &document, &pixels, "text").expect("a .clip");
    assert_eq!(
        warnings,
        vec![ExportWarning::RasterizedText { layer: "Bottom".to_owned() }],
        "the text is no longer editable, and that is said"
    );
    let imported = import(&path).expect("the .clip again");
    let bottom = imported.document.layers().iter().find(|layer| layer.name == "Bottom").expect("the bottom layer");
    assert_eq!(at(&pixels_of(&imported, bottom.id), 8, 0, 0), [10, 20, 30, 255]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A one paint layer document, shown at full opacity on a transparent canvas.
fn flat(width: u32, height: u32, name: &str) -> (Document, Vec<(LayerId, Vec<u8>)>) {
    let paint = LayerId(1);
    let layer = layer(1, name, LayerKind::Paint, None);
    let mut document = Document::from_layers(width, height, vec![layer], paint);
    document.background_visible = false;
    let pixels = vec![(paint, premultiplied(width, height, |_, _| [10, 20, 30, 255]))];
    (document, pixels)
}

#[test]
fn a_kra_export_publishes_atomically_and_follows_a_symlink() {
    let (width, height) = (4_u32, 3_u32);
    let (document, pixels) = flat(width, height, "Paint");
    let dir = directory("kra-publish");
    let path = dir.join("art.kra");

    // A refused export leaves the document that was already there exactly as it was.
    std::fs::write(&path, b"the old document").expect("an old destination");
    let mut truncated = pixels.clone();
    truncated[0].1.truncate(8);
    match export(&path, ExportFormat::Kra, &document, &truncated, "art") {
        Err(ExportError::PixelSize { .. }) => {}
        other => panic!("expected the pixel size to be refused, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&path).expect("the old destination"),
        b"the old document",
        "a failed export does not touch the destination"
    );

    // A successful export replaces it and leaves nothing beside it.
    export(&path, ExportFormat::Kra, &document, &pixels, "art").expect("a .kra");
    assert_eq!(&std::fs::read(&path).expect("the archive")[..2], b"PK", "the archive is published");
    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .expect("the directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "art.kra")
        .collect();
    assert!(leftovers.is_empty(), "no temporary is left behind: {leftovers:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // The destination's permissions survive the publish, where the filesystem has Unix modes
        // (the build's temporary directory is an NTFS volume, which reports 0755 for everything
        // and ignores `chmod`).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("a mode on the destination");
        let honours_modes =
            std::fs::metadata(&path).map(|metadata| metadata.permissions().mode() & 0o777 == 0o640).unwrap_or(false);
        export(&path, ExportFormat::Kra, &document, &pixels, "art").expect("a second .kra");
        if honours_modes {
            let mode = std::fs::metadata(&path).expect("the archive").permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "the destination keeps its mode");
        }

        // A symlink destination is followed: the link stays and the file it names is replaced.
        let real = dir.join("real.kra");
        std::fs::write(&real, b"stale").expect("a stale file");
        let link = dir.join("link.kra");
        std::os::unix::fs::symlink(&real, &link).expect("a symlink");
        export(&link, ExportFormat::Kra, &document, &pixels, "art").expect("a .kra through a link");
        assert!(
            std::fs::symlink_metadata(&link).expect("the link").file_type().is_symlink(),
            "the link itself is kept, not replaced by a file"
        );
        assert_eq!(
            &std::fs::read(&real).expect("the linked file")[..2],
            b"PK",
            "the file the link names is the new archive"
        );
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_control_character_in_a_layer_name_is_replaced_and_the_file_reopens() {
    let (width, height) = (4_u32, 3_u32);
    let (document, pixels) = flat(width, height, "bad\u{1}name");
    let dir = directory("kra-control");
    let path = dir.join("control.kra");
    export(&path, ExportFormat::Kra, &document, &pixels, "control").expect("a .kra");

    // The reader parses the XML strictly: a character XML 1.0 forbids would corrupt the file.
    let kra = KraDocument::open(&path).expect("a readable .kra");
    let name = &kra.layers.first().expect("one layer").name;
    assert_eq!(name, "bad\u{FFFD}name", "the control character became the replacement");
    assert!(!name.contains('\u{1}'), "no invalid character reaches the file");

    let imported = import(&path).expect("the .kra again");
    assert!(
        imported.document.layers().iter().any(|layer| layer.name == "bad\u{FFFD}name"),
        "the preflight and the reader accept the written name"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A paint layer with a mask layer above it, both canvas-sized; the mask hidden when `visible` is
/// false, and covering only the left half when it is shown.
fn masked(visible: bool) -> (Document, Vec<(LayerId, Vec<u8>)>) {
    let (width, height) = (8_u32, 4_u32);
    let paint = LayerId(1);
    let mask = LayerId(2);
    let mut mask_layer = layer(2, "Mask", LayerKind::Mask, Some(paint));
    mask_layer.visible = visible;
    let mut document =
        Document::from_layers(width, height, vec![layer(1, "Paint", LayerKind::Paint, None), mask_layer], paint);
    document.background_visible = false;
    let pixels = vec![
        (paint, premultiplied(width, height, |_, _| [10, 20, 30, 255])),
        (mask, mask_pixels(width, height, |x, _| if x < 4 { 255 } else { 0 })),
    ];
    (document, pixels)
}

#[test]
fn a_hidden_mask_is_written_switched_off_in_both_formats() {
    let (document, pixels) = masked(false);
    let dir = directory("hidden-mask");
    assert!(!document.background_visible);

    // CLIP STUDIO PAINT: attaching a mask enables it, so the bit is cleared again, and the layer
    // imports without the mask attenuating it.
    let clip = dir.join("hidden.clip");
    export(&clip, ExportFormat::Clip, &document, &pixels, "hidden").expect("a .clip");
    let file = ClipFile::open(&clip).expect("a readable .clip");
    let paint = file.layers.values().find(|layer| layer.name == "Paint").expect("the paint layer");
    assert_eq!(paint.visibility & 2, 0, "the hidden mask bit is clear");
    assert_eq!(paint.visibility & 1, 1, "the layer itself stays shown");
    let imported = import(&clip).expect("the .clip again");
    assert!(
        !imported.document.layers().iter().any(|layer| layer.kind == LayerKind::Mask),
        "a mask the file switched off does not attenuate the imported layer"
    );

    // Krita: the mask row carries its own visibility, and it is the hidden one.
    let kra = dir.join("hidden.kra");
    let kra_warnings = export(&kra, ExportFormat::Kra, &document, &pixels, "hidden").expect("a .kra");
    assert!(kra_warnings.is_empty(), "a hidden mask is stored, not lost: {kra_warnings:?}");
    let file = std::fs::File::open(&kra).expect("the written file");
    let mut zip = zip::ZipArchive::new(file).expect("a ZIP archive");
    let mut xml = String::new();
    zip.by_name("maindoc.xml").expect("maindoc.xml").read_to_string(&mut xml).expect("UTF-8");
    let mask = xml.lines().find(|line| line.contains("nodetype=\"transparencymask\"")).expect("a mask element");
    assert!(mask.contains("visible=\"0\""), "the hidden mask stays hidden: {mask}");

    // The merged preview is what Krita shows for the file: a mask the engine hides attenuates
    // nothing in the stack, so it must attenuate nothing there either.
    let written = KraDocument::open(&kra).expect("a readable .kra");
    let merged = written.merged.as_ref().expect("the merged preview");
    assert_eq!(at(&merged.data, 8, 0, 0), [10, 20, 30, 255], "the left half is painted");
    assert_eq!(
        at(&merged.data, 8, 7, 0),
        [10, 20, 30, 255],
        "the hidden mask does not erase the right half of the preview"
    );

    // The reader honours `visible="0"`, so the hidden mask does not come back attenuating anything:
    // the written file shows exactly what the artwork shows, and the unread mask is announced.
    let imported = import(&kra).expect("the .kra again");
    assert!(
        !imported.document.layers().iter().any(|layer| layer.kind == LayerKind::Mask),
        "a mask the file switched off does not attenuate the imported layer"
    );
    assert_eq!(
        imported.warnings,
        [ImportWarning::MaskHiddenNotRead { layer: "Paint".to_owned() }],
        "the mask is not read back, and the loss is said"
    );
    let paint = imported.document.layers().iter().find(|layer| layer.name == "Paint").expect("the paint layer");
    let painted = pixels_of(&imported, paint.id);
    assert_eq!(at(&painted, 8, 0, 0), [10, 20, 30, 255], "the left half keeps its pixels");
    assert_eq!(at(&painted, 8, 7, 0), [10, 20, 30, 255], "the right half is not erased");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_half_covering_mask_keeps_its_coverage_after_a_clip_round_trip() {
    let (document, pixels) = masked(true);
    let dir = directory("mask-channels");
    let path = dir.join("coverage.clip");
    export(&path, ExportFormat::Clip, &document, &pixels, "coverage").expect("a .clip");

    // The writer takes the coverage from the alpha channel; the import must read the same
    // coverage back, half the canvas covered and half not.
    let file = ClipFile::open(&path).expect("a readable .clip");
    let paint = file.layers.values().find(|layer| layer.name == "Paint").expect("the paint layer");
    let mask = layer_mask(&file, paint, 100.0).expect("the mask's surface").expect("the layer has a mask");
    assert_eq!((mask.width, mask.height), (8, 4), "the mask surface is the canvas");

    let imported = import(&path).expect("the .clip again");
    let mask =
        imported.document.layers().iter().find(|layer| layer.kind == LayerKind::Mask).expect("the imported mask");
    let mask = pixels_of(&imported, mask.id);
    assert_eq!(at(&mask, 8, 0, 0), [255, 255, 255, 255], "the covered half");
    assert_eq!(at(&mask, 8, 3, 3), [255, 255, 255, 255], "the covered half at its far corner");
    assert_eq!(at(&mask, 8, 5, 0), [0, 0, 0, 255], "uncovered, still opaque");
    assert_eq!(at(&mask, 8, 7, 3), [0, 0, 0, 255], "uncovered at the far corner");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_multi_tile_kra_round_trips_past_the_first_tile() {
    let (width, height) = (70_u32, 66_u32);
    let (document, _) = flat(width, height, "Paint");
    // Four tiles at 64 x 64: paint a pixel in each, the far ones on partial tiles.
    let painted = [(0_u32, 0_u32), (63, 0), (64, 1), (0, 64), (69, 65)];
    let pixels = vec![(
        LayerId(1),
        premultiplied(width, height, |x, y| if painted.contains(&(x, y)) { [200, 100, 50, 255] } else { [0; 4] }),
    )];
    let dir = directory("multi-tile");
    let path = dir.join("tiles.kra");
    export(&path, ExportFormat::Kra, &document, &pixels, "tiles").expect("a .kra");

    let kra = KraDocument::open(&path).expect("a readable .kra");
    let layer = kra.layers.first().expect("one layer");
    for (x, y) in painted {
        assert_eq!(at(&layer.image.data, width, x, y), [200, 100, 50, 255], "the reader keeps the pixel at {x},{y}");
    }

    let imported = import(&path).expect("the .kra again");
    let layer = imported.document.layers().first().expect("one layer");
    let pixels = pixels_of(&imported, layer.id);
    for (x, y) in painted {
        assert_eq!(at(&pixels, width, x, y), [200, 100, 50, 255], "pixel {x},{y} after the import");
    }
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_dot_only_artwork_name_writes_no_parent_path_component() {
    let (document, pixels) = flat(4, 3, "Paint");
    let dir = directory("dot-name");
    let path = dir.join("dots.kra");
    export(&path, ExportFormat::Kra, &document, &pixels, "..").expect("a .kra");

    let file = std::fs::File::open(&path).expect("the written file");
    let mut zip = zip::ZipArchive::new(file).expect("a ZIP archive");
    let names: Vec<String> =
        (0..zip.len()).filter_map(|index| zip.by_index(index).ok().map(|entry| entry.name().to_owned())).collect();
    assert!(
        names.iter().any(|name| name.starts_with("Untitled/layers/")),
        "the layer entry sits in the fallback folder: {names:?}"
    );
    for name in &names {
        for component in name.split('/') {
            assert!(
                component != ".." && component != "." && !component.is_empty(),
                "the entry {name:?} does not step out of the archive"
            );
        }
    }
    let imported = import(&path).expect("the .kra again");
    assert_eq!(imported.document.layers().len(), 1, "the document still imports");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn state_a_format_cannot_hold_is_named_in_a_warning() {
    let (width, height) = (4_u32, 3_u32);
    let dir = directory("state-warnings");
    let pixels = vec![(LayerId(1), premultiplied(width, height, |_, _| [10, 20, 30, 255]))];

    // CLIP STUDIO PAINT has neither collapsed nor private layers.
    let mut paint = layer(1, "Paint", LayerKind::Paint, None);
    paint.collapsed = true;
    paint.private = true;
    let document = Document::from_layers(width, height, vec![paint], LayerId(1));
    let clip = dir.join("state.clip");
    let warnings = export(&clip, ExportFormat::Clip, &document, &pixels, "state").expect("a .clip");
    for what in ["collapsed state", "private flag"] {
        assert!(
            warnings.contains(&ExportWarning::StateNotStored { layer: "Paint".to_owned(), what }),
            "{what} is named: {warnings:?}"
        );
    }

    // Krita keeps locked and collapsed, but not an alpha lock or a reference flag.
    let mut paint = layer(1, "Paint", LayerKind::Paint, None);
    paint.alpha_lock = true;
    paint.reference = true;
    paint.private = true;
    let document = Document::from_layers(width, height, vec![paint], LayerId(1));
    let kra = dir.join("state.kra");
    let warnings = export(&kra, ExportFormat::Kra, &document, &pixels, "state").expect("a .kra");
    for what in ["alpha lock", "reference layer flag", "private flag"] {
        assert!(
            warnings.contains(&ExportWarning::StateNotStored { layer: "Paint".to_owned(), what }),
            "{what} is named: {warnings:?}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
