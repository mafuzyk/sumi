//! The export pre-flight says, before anything is written, what the writer says: the same
//! warnings, in the same order, and the same refusal naming the same layer.
// Integration tests run beside the app, not in it: the workspace's no-panic lints are for shipped code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};

use sumi_clip::{ExportError, ExportFormat, ExportWarning, export, preflight_export};
use sumi_engine::document::{BlendMode, Document, Layer, LayerId, LayerKind, LayerText, PluginLayer};

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

/// A temporary folder for one test, removed with its files when the test ends.
fn directory(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sumi-preflight-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    dir
}

/// Canvas-sized pixels for every paint layer and mask of `document`: transparent paint and a
/// mask fully covered, which the writer takes and the pre-flight never reads.
fn pixels_of(document: &Document) -> Vec<(LayerId, Vec<u8>)> {
    let bytes = (document.width() * document.height()) as usize * 4;
    document
        .layers()
        .iter()
        .filter(|layer| layer.kind != LayerKind::Group)
        .map(|layer| {
            let channel = if layer.kind == LayerKind::Mask { 255 } else { 0 };
            (layer.id, vec![channel; bytes])
        })
        .collect()
}

/// The writer's answer and the pre-flight's for `document` as `format`: the file is written and
/// its warnings compared, or both must refuse with the same words. Returns the pre-flight's own
/// answer.
fn same_as_writer(dir: &Path, document: &Document, format: ExportFormat) -> Result<Vec<ExportWarning>, ExportError> {
    let extension = match format {
        ExportFormat::Clip => "clip",
        ExportFormat::Kra => "kra",
    };
    let path = dir.join(format!("check.{extension}"));
    let written = export(&path, format, document, &pixels_of(document), "check");
    let checked = preflight_export(document, format);
    match (&written, &checked) {
        (Ok(warnings), Ok(preflighted)) => {
            assert_eq!(preflighted, warnings, "the pre-flight says what the writer said");
        }
        (Err(writer_error), Err(preflight_error)) => {
            assert_eq!(preflight_error.to_string(), writer_error.to_string(), "both refuse for the same reason");
        }
        _ => panic!("the writer answered {written:?} and the pre-flight {checked:?}"),
    }
    checked
}

#[test]
fn a_document_the_format_holds_whole_has_nothing_to_say() {
    let (width, height) = (4_u32, 3_u32);
    let bottom = layer(1, "Bottom", LayerKind::Paint, None);
    let mask = layer(2, "Mask", LayerKind::Mask, Some(LayerId(1)));
    let mut document = Document::from_layers(width, height, vec![bottom, mask], LayerId(1));
    // Krita has no paper layer: a document that shows none has nothing it cannot store
    document.background_visible = false;
    for format in [ExportFormat::Clip, ExportFormat::Kra] {
        let dir = directory(&format!("clean-{}", format.label()));
        let warnings = same_as_writer(&dir, &document, format).expect("the format holds it");
        assert!(warnings.is_empty(), "nothing of it is lost: {warnings:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_text_or_plugin_layer_is_the_warning_the_writer_gives() {
    let (width, height) = (4_u32, 3_u32);
    let mut text = layer(1, "Title", LayerKind::Paint, None);
    text.text = Some(LayerText::default());
    let mut plugin = layer(2, "Shapes", LayerKind::Paint, None);
    plugin.plugin = Some(PluginLayer { kind: "someone.vector".to_owned(), data: None });
    let mut document = Document::from_layers(width, height, vec![text, plugin], LayerId(1));
    document.background_visible = false;
    for format in [ExportFormat::Clip, ExportFormat::Kra] {
        let dir = directory(&format!("rasterized-{}", format.label()));
        let warnings = same_as_writer(&dir, &document, format).expect("both become pixels");
        for wanted in [
            ExportWarning::RasterizedText { layer: "Title".to_owned() },
            ExportWarning::RasterizedPlugin { layer: "Shapes".to_owned() },
        ] {
            assert!(warnings.contains(&wanted), "{wanted:?} is said: {warnings:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_layer_state_a_format_cannot_hold_is_said_as_the_writer_says_it() {
    let (width, height) = (4_u32, 3_u32);
    let mut group = layer(4, "Group", LayerKind::Group, None);
    group.collapsed = true;
    group.private = true;
    group.alpha_lock = true;
    group.reference = true;
    let mut inside = layer(1, "Inside", LayerKind::Paint, Some(LayerId(4)));
    inside.assisted = true;
    inside.locked = true;
    inside.collapsed = true;
    inside.private = true;
    inside.alpha_lock = true;
    inside.reference = true;
    let mut document = Document::from_layers(width, height, vec![inside, group], LayerId(1));
    document.background_visible = false;
    for format in [ExportFormat::Clip, ExportFormat::Kra] {
        let dir = directory(&format!("state-{}", format.label()));
        let warnings = same_as_writer(&dir, &document, format).expect("the layer state is said");
        assert!(
            warnings.contains(&ExportWarning::StateNotStored { layer: "Group".to_owned(), what: "alpha lock" }),
            "{format:?} names the group's state: {warnings:?}"
        );
        assert!(
            warnings.contains(&ExportWarning::StateNotStored { layer: "Inside".to_owned(), what: "drawing assist" }),
            "{format:?} names the layer's assist: {warnings:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn krita_refuses_a_clipped_layer_by_name_and_the_preflight_does_too() {
    let (width, height) = (4_u32, 3_u32);
    let base = layer(1, "Base", LayerKind::Paint, None);
    let mut ink = layer(2, "Ink", LayerKind::Paint, None);
    ink.clipped = true;
    let mut document = Document::from_layers(width, height, vec![base, ink], LayerId(1));
    document.background_visible = false;
    let dir = directory("kra-clipped");
    let error = same_as_writer(&dir, &document, ExportFormat::Kra).expect_err("Krita has no clipping flag");
    let said = error.to_string();
    assert!(said.contains("Ink"), "the layer is named: {said}");
    assert!(said.contains("clipping"), "the lost property is named: {said}");
    // CLIP STUDIO PAINT keeps the flag, so the same document is written there with nothing to say
    let warnings = same_as_writer(&dir, &document, ExportFormat::Clip).expect("a .clip holds it");
    assert!(warnings.is_empty(), "{warnings:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn krita_says_the_canvas_background_is_not_stored_where_clip_keeps_it() {
    let (width, height) = (4_u32, 3_u32);
    let paint = layer(1, "Paint", LayerKind::Paint, None);
    let mut document = Document::from_layers(width, height, vec![paint], LayerId(1));
    document.background_visible = true;
    let dir = directory("background");
    let warnings = same_as_writer(&dir, &document, ExportFormat::Kra).expect("a .kra");
    assert_eq!(warnings, [ExportWarning::BackgroundNotStored]);
    let warnings = same_as_writer(&dir, &document, ExportFormat::Clip).expect("a .clip");
    assert!(warnings.is_empty(), "the paper row keeps it: {warnings:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_shade_layer_is_refused_by_name_before_any_pixel_is_read() {
    let (width, height) = (4_u32, 3_u32);
    let mut paint = layer(1, "Shade", LayerKind::Paint, None);
    paint.blend = BlendMode::Shade;
    let mut document = Document::from_layers(width, height, vec![paint], LayerId(1));
    document.background_visible = false;
    for format in [ExportFormat::Clip, ExportFormat::Kra] {
        let error = preflight_export(&document, format).expect_err("no counterpart blend mode");
        let said = error.to_string();
        assert!(said.contains("Shade blend mode"), "the blend mode is named: {said}");
        assert!(said.contains("Shade"), "the layer is named: {said}");
    }
}

#[test]
fn a_layer_with_no_pixels_is_found_only_when_writing() {
    let (width, height) = (4_u32, 3_u32);
    let paint = layer(1, "Paint", LayerKind::Paint, None);
    let mut document = Document::from_layers(width, height, vec![paint], LayerId(1));
    document.background_visible = false;
    // the pre-flight reads the layer state alone: it cannot see an export buffer
    assert!(preflight_export(&document, ExportFormat::Clip).is_ok(), "the pre-flight has no pixels to look for");
    let dir = directory("missing-pixels");
    let path = dir.join("missing.clip");
    match export(&path, ExportFormat::Clip, &document, &[], "missing") {
        Err(ExportError::MissingPixels { layer }) => assert_eq!(layer, "Paint"),
        other => panic!("the writer refuses a layer it has no pixels for: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// CLIP STUDIO PAINT's writer makes a folder with no lock row: a group's lock would otherwise
/// vanish without a word. Krita's `maindoc.xml` has the attribute, so it stores it.
#[test]
fn a_locked_group_is_said_where_the_format_has_no_row_and_stored_where_it_has() {
    let (width, height) = (4_u32, 3_u32);
    let inside = layer(2, "Inside", LayerKind::Paint, Some(LayerId(1)));
    let mut group = layer(1, "Group", LayerKind::Group, None);
    group.locked = true;
    let mut document = Document::from_layers(width, height, vec![inside, group], LayerId(2));
    document.background_visible = false;
    let dir = directory("group-lock");
    let warnings = same_as_writer(&dir, &document, ExportFormat::Clip).expect("a .clip");
    assert!(
        warnings.contains(&ExportWarning::StateNotStored { layer: "Group".to_owned(), what: "lock" }),
        "the group's lock is said: {warnings:?}"
    );
    let warnings = same_as_writer(&dir, &document, ExportFormat::Kra).expect("a .kra");
    assert!(warnings.is_empty(), "Krita keeps the lock, so there is nothing to say: {warnings:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Neither format has a row for the drawing guide: a document that set one up loses it, once, and
/// a document that never did says nothing.
#[test]
fn a_drawing_guide_is_said_once_by_both_formats() {
    let (width, height) = (4_u32, 3_u32);
    let paint = layer(1, "Paint", LayerKind::Paint, None);
    let mut document = Document::from_layers(width, height, vec![paint], LayerId(1));
    document.background_visible = false;
    for format in [ExportFormat::Clip, ExportFormat::Kra] {
        let dir = directory(&format!("guide-{}", format.label()));
        let plain = same_as_writer(&dir, &document, format).expect("no guide is set up");
        assert!(plain.is_empty(), "a document with no guide loses none: {plain:?}");
        document.guide.set_up = true;
        let warnings = same_as_writer(&dir, &document, format).expect("a guide is lost");
        assert_eq!(warnings, [ExportWarning::GuideNotStored], "{format:?} says it once");
        document.guide.set_up = false;
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Two pixel buffers for one layer would let the first one be written in silence: the caller's
/// duplicate is refused by the layer's name before anything is written.
#[test]
fn two_pixel_buffers_for_one_layer_are_refused_by_name() {
    let (width, height) = (4_u32, 3_u32);
    let paint = layer(1, "Paint", LayerKind::Paint, None);
    let mut document = Document::from_layers(width, height, vec![paint], LayerId(1));
    document.background_visible = false;
    let bytes = vec![0_u8; width as usize * height as usize * 4];
    let pixels = vec![(LayerId(1), bytes.clone()), (LayerId(1), bytes)];
    let dir = directory("duplicate-pixels");
    let path = dir.join("duplicate.clip");
    match export(&path, ExportFormat::Clip, &document, &pixels, "duplicate") {
        Err(ExportError::DuplicatePixels { layer }) => assert_eq!(layer, "Paint"),
        other => panic!("the duplicate is refused by the layer's name: {other:?}"),
    }
    assert!(!path.exists(), "nothing is written for a refused export");
    let _ = std::fs::remove_dir_all(dir);
}
