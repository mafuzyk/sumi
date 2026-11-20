//! Writing a native artwork back to CLIP STUDIO PAINT (`.clip`) or Krita (`.kra`).
//!
//! The layers and their canvases live in the engine; this module maps them onto the two
//! formats' own layer models and writes a new file at the destination the caller chose. It
//! never reads or changes the document an import came from.
//!
//! What a format cannot hold is named rather than changed in silence: [`ExportError`] refuses
//! an export whose result would differ from the artwork, and [`ExportWarning`] lists what the
//! user has to know about the copy that was written.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Seek, Write};
use std::path::Path;

use clip_core::blend;
use clip_core::kra::composite_name;
use clip_core::write::{ClipWriter, publish_file};
use sumi_engine::document::{BlendMode, Document, Layer, LayerId, LayerKind, mask_grey};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// The layer stack of the document as this module walks it: each parent's children bottom first,
/// as [`Document::layers`] keeps them, with a paint layer's mask beside it.
enum Node<'a> {
    /// A paint layer and, when it has one, the mask layer just above it.
    Paint { layer: &'a Layer, mask: Option<&'a Layer> },
    /// A group and everything it holds.
    Group { layer: &'a Layer, children: Vec<Node<'a>> },
}

/// The format [`export`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    /// CLIP STUDIO PAINT's `.clip`.
    Clip,
    /// Krita's `.kra`.
    Kra,
}

impl ExportFormat {
    /// How an error or warning names the format.
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Clip => "CLIP STUDIO PAINT",
            ExportFormat::Kra => "Krita",
        }
    }
}

/// A part of the native artwork that a written file keeps differently, or does not keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportWarning {
    /// A text layer was written as its painted pixels: the text is no longer editable.
    RasterizedText { layer: String },
    /// A plugin layer was written as its painted pixels: the plugin's data is gone.
    RasterizedPlugin { layer: String },
    /// Krita has no paper layer: the canvas's background colour was not stored.
    BackgroundNotStored,
    /// A layer state that only a native artwork holds was left out of the file.
    StateNotStored { layer: String, what: &'static str },
    /// The drawing guide over the canvas has no row in either format, so it was not stored.
    GuideNotStored,
}

impl ExportWarning {
    /// The warning as one line for a person.
    pub fn message(&self) -> String {
        match self {
            ExportWarning::RasterizedText { layer } => {
                format!("the text layer {layer:?} was written as pixels; its text is no longer editable")
            }
            ExportWarning::RasterizedPlugin { layer } => {
                format!("the plugin layer {layer:?} was written as pixels; the plugin's data is gone")
            }
            ExportWarning::BackgroundNotStored => {
                "Krita has no paper layer: the canvas's background colour was not stored".to_owned()
            }
            ExportWarning::StateNotStored { layer, what } => {
                format!("the {what} of the layer {layer:?} is not stored in this format")
            }
            ExportWarning::GuideNotStored => {
                "the drawing guide over the canvas is not stored in this format".to_owned()
            }
        }
    }
}

/// Why an artwork could not be written.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("{format} cannot represent {what} the layer {layer:?} holds")]
    Unrepresentable { format: &'static str, layer: String, what: &'static str },
    #[error("{format} cannot hold a {width} by {height} pixel canvas")]
    Canvas { format: &'static str, width: u32, height: u32 },
    #[error("the layer {layer:?} has no pixels in the export buffer")]
    MissingPixels { layer: String },
    #[error("the pixels of the layer {layer:?} are {got} bytes; a {width} by {height} canvas needs {expected}")]
    PixelSize { layer: String, got: usize, expected: usize, width: u32, height: u32 },
    #[error("the mask {layer:?} does not sit above a paint layer, so no format can hold it")]
    OrphanMask { layer: String },
    #[error("the layer {layer:?} is not reachable below any group of the document")]
    UnreachableLayer { layer: String },
    #[error("the export buffer holds the pixels of the layer {layer:?} twice")]
    DuplicatePixels { layer: String },
    #[error("{format} export failed: {message}")]
    Write { format: &'static str, message: String },
}

/// Write `document` and its pixels to `path` as `format`.
///
/// `pixels` holds every paint layer's and mask's pixels by [`LayerId`], once each: canvas-sized
/// premultiplied RGBA, with a mask's coverage in the channels [`mask_grey`] reads. `name` is the
/// artwork's name; it names the image inside a `.kra` and is ignored by `.clip`, which finds it in
/// the file name.
///
/// The file at `path` is created or replaced; nothing is read from an imported source.
pub fn export(
    path: &Path,
    format: ExportFormat,
    document: &Document,
    pixels: &[(LayerId, Vec<u8>)],
    name: &str,
) -> Result<Vec<ExportWarning>, ExportError> {
    check_canvas(document, format)?;
    let nodes = build_tree(document)?;
    let source = PixelSource::new(document, pixels)?;
    let mut warnings = Vec::new();
    warn_guides(document, &mut warnings);
    match format {
        ExportFormat::Clip => export_clip(path, document, &nodes, &source, &mut warnings)?,
        ExportFormat::Kra => export_kra(path, document, &nodes, &source, name, &mut warnings)?,
    }
    Ok(warnings)
}

/// What writing `document` as `format` would keep differently, and whether it would be refused.
///
/// It runs the writer's own tree building and per-layer rules without pixels: the layer state is
/// all it reads, so it touches neither the canvas nor the file system and can answer while a
/// format's row is shown. Two refusals need the export buffer and are found only when writing:
/// [`ExportError::MissingPixels`] and [`ExportError::PixelSize`]. So are the writers' own file
/// failures ([`ExportError::Write`]), which only a write can meet.
pub fn preflight_export(document: &Document, format: ExportFormat) -> Result<Vec<ExportWarning>, ExportError> {
    check_canvas(document, format)?;
    let nodes = build_tree(document)?;
    let mut warnings = Vec::new();
    warn_guides(document, &mut warnings);
    warn_kra_canvas(document, format, &mut warnings);
    check_tree(&nodes, format, &mut warnings)?;
    Ok(warnings)
}

/// Neither format has a row for the drawing guide over the canvas: a document that has set one up
/// loses it, said once for the file rather than once for each layer.
fn warn_guides(document: &Document, warnings: &mut Vec<ExportWarning>) {
    let guide = &document.guide;
    if guide.set_up || guide.on || !guide.vanishing.is_empty() {
        warnings.push(ExportWarning::GuideNotStored);
    }
}

/// A canvas no format can hold: a side of zero, which the writer refuses before anything else.
fn check_canvas(document: &Document, format: ExportFormat) -> Result<(), ExportError> {
    let (width, height) = (document.width(), document.height());
    if width == 0 || height == 0 {
        return Err(ExportError::Canvas { format: format.label(), width, height });
    }
    Ok(())
}

/// Krita has no paper layer: whether the canvas's background colour shows has nowhere to go, and
/// is said before any layer.
fn warn_kra_canvas(document: &Document, format: ExportFormat, warnings: &mut Vec<ExportWarning>) {
    if format == ExportFormat::Kra && document.background_visible {
        warnings.push(ExportWarning::BackgroundNotStored);
    }
}

/// The per-layer checks of both writers over `nodes`, in the order each writer meets them.
fn check_tree(nodes: &[Node<'_>], format: ExportFormat, warnings: &mut Vec<ExportWarning>) -> Result<(), ExportError> {
    match format {
        // CLIP STUDIO PAINT's writer makes every folder before any paint layer: the two passes
        // are its order, so the lines come out as the file writes them.
        ExportFormat::Clip => {
            check_clip_groups(nodes, warnings)?;
            check_clip_paints(nodes, warnings)
        }
        ExportFormat::Kra => check_kra_nodes(nodes, warnings),
    }
}

/// Every group's own state, parents before the groups they hold, as CLIP STUDIO PAINT's writer
/// adds its folders.
fn check_clip_groups(nodes: &[Node<'_>], warnings: &mut Vec<ExportWarning>) -> Result<(), ExportError> {
    for node in nodes {
        if let Node::Group { layer, children } = node {
            check_clip_group(layer, warnings)?;
            // The writer asks for the folder's blend mode next, and a pass-through folder is
            // Normal, which it always has.
            if !layer.passes_through() {
                clip_blend(layer, ExportFormat::Clip.label())?;
            }
            check_clip_groups(children, warnings)?;
        }
    }
    Ok(())
}

/// Every paint layer's state, in the order CLIP STUDIO PAINT's writer adds its raster layers.
fn check_clip_paints(nodes: &[Node<'_>], warnings: &mut Vec<ExportWarning>) -> Result<(), ExportError> {
    for node in nodes {
        match node {
            Node::Group { children, .. } => check_clip_paints(children, warnings)?,
            Node::Paint { layer, .. } => {
                warn_paint_state(layer, ExportFormat::Clip, warnings);
                clip_blend(layer, ExportFormat::Clip.label())?;
            }
        }
    }
    Ok(())
}

/// Krita's writer builds its tree depth first: a group's own state is said after what it holds.
fn check_kra_nodes(nodes: &[Node<'_>], warnings: &mut Vec<ExportWarning>) -> Result<(), ExportError> {
    let format = ExportFormat::Kra;
    for node in nodes {
        match node {
            Node::Group { layer, children } => {
                check_kra_unclipped(layer, "the clipping of a group")?;
                // Krita's writer refuses the blend mode before it walks the children.
                clip_blend(layer, format.label())?;
                check_kra_nodes(children, warnings)?;
                warn_kra_state(layer, warnings);
            }
            Node::Paint { layer, .. } => {
                check_kra_unclipped(layer, "the clipping")?;
                warn_paint_state(layer, format, warnings);
                clip_blend(layer, format.label())?;
            }
        }
    }
    Ok(())
}

/// What a paint layer holds that `format` cannot keep, said rather than dropped in silence: its
/// text or a plugin's data becomes pixels, and the layer state the format has no row for.
///
/// Both writers call this, so a check cannot be added to one and missed by the other.
fn warn_paint_state(layer: &Layer, format: ExportFormat, warnings: &mut Vec<ExportWarning>) {
    if layer.text.is_some() {
        warnings.push(ExportWarning::RasterizedText { layer: layer.name.clone() });
    }
    if layer.plugin.is_some() {
        warnings.push(ExportWarning::RasterizedPlugin { layer: layer.name.clone() });
    }
    match format {
        // A raster row carries alpha and reference locks; the rest has nowhere to go.
        ExportFormat::Clip => warn_state(
            layer,
            &[
                (layer.locked, "lock"),
                (layer.assisted, "drawing assist"),
                (layer.collapsed, "collapsed state"),
                (layer.private, "private flag"),
            ],
            warnings,
        ),
        // `maindoc.xml` keeps the lock and the collapsed state; these it has no row for.
        ExportFormat::Kra => {
            warn_state(layer, &[(layer.assisted, "drawing assist")], warnings);
            warn_kra_state(layer, warnings);
        }
    }
}

/// What a group holds that CLIP STUDIO PAINT cannot keep, or its clipping, which no folder holds.
fn check_clip_group(layer: &Layer, warnings: &mut Vec<ExportWarning>) -> Result<(), ExportError> {
    if layer.clipped {
        return Err(ExportError::Unrepresentable {
            format: ExportFormat::Clip.label(),
            layer: layer.name.clone(),
            what: "the clipping of a group",
        });
    }
    // A folder carries no lock, alpha-lock or reference row of its own in the writer: those states
    // of a group would otherwise vanish without a word.
    warn_state(
        layer,
        &[
            (layer.locked, "lock"),
            (layer.collapsed, "collapsed state"),
            (layer.private, "private flag"),
            (layer.alpha_lock, "alpha lock"),
            (layer.reference, "reference layer flag"),
        ],
        warnings,
    );
    Ok(())
}

/// Krita's own document has no clipping flag: writing it as "normal" would change the picture.
fn check_kra_unclipped(layer: &Layer, what: &'static str) -> Result<(), ExportError> {
    if layer.clipped {
        return Err(ExportError::Unrepresentable {
            format: ExportFormat::Kra.label(),
            layer: layer.name.clone(),
            what,
        });
    }
    Ok(())
}

/// Every state whose flag is held, as the warning naming `layer` and what was left out.
fn warn_state(layer: &Layer, held: &[(bool, &'static str)], warnings: &mut Vec<ExportWarning>) {
    for &(is_held, what) in held {
        if is_held {
            warnings.push(ExportWarning::StateNotStored { layer: layer.name.clone(), what });
        }
    }
}

/// The engine's pixels for a layer, by id, with the checks every format needs.
struct PixelSource<'a> {
    by_id: HashMap<LayerId, &'a [u8]>,
}

impl<'a> PixelSource<'a> {
    /// The buffer's pixels by layer, refusing two buffers for one layer.
    ///
    /// Two buffers for one id would let the first one win in silence, so the caller's duplicate is
    /// an error naming the layer rather than a file whose layer is the wrong pixels.
    fn new(document: &Document, pixels: &'a [(LayerId, Vec<u8>)]) -> Result<Self, ExportError> {
        let mut by_id = HashMap::with_capacity(pixels.len());
        for (id, data) in pixels {
            if by_id.insert(*id, data.as_slice()).is_some() {
                let layer = document.layer(*id).map_or_else(|| format!("layer #{}", id.0), |layer| layer.name.clone());
                return Err(ExportError::DuplicatePixels { layer });
            }
        }
        Ok(PixelSource { by_id })
    }

    fn get(&self, layer: &Layer, width: u32, height: u32) -> Result<&'a [u8], ExportError> {
        self.get_by_id(layer.id, &layer.name, width, height)
    }

    fn get_by_id(&self, id: LayerId, layer: &str, width: u32, height: u32) -> Result<&'a [u8], ExportError> {
        let Some(data) = self.by_id.get(&id).copied() else {
            return Err(ExportError::MissingPixels { layer: layer.to_owned() });
        };
        let expected = usize::try_from(u64::from(width) * u64::from(height) * 4).unwrap_or(usize::MAX);
        if data.len() != expected {
            return Err(ExportError::PixelSize { layer: layer.to_owned(), got: data.len(), expected, width, height });
        }
        Ok(data)
    }
}

/// The document's layer stack as [`Node`]s, refusing a layer the stack does not reach.
fn build_tree(document: &Document) -> Result<Vec<Node<'_>>, ExportError> {
    let mut seen = HashSet::new();
    let mut attached = HashSet::new();
    // A stack is a tree: the bound turns a document that somehow loops its parents into an error
    // instead of an endless walk.
    let depth_most = document.layers().len().saturating_add(1);
    let nodes = children_of(document, None, &mut seen, &mut attached, 0, depth_most)?;
    for layer in document.layers() {
        if layer.kind == LayerKind::Mask {
            if !attached.contains(&layer.id) {
                return Err(ExportError::OrphanMask { layer: layer.name.clone() });
            }
        } else if !seen.contains(&layer.id) {
            return Err(ExportError::UnreachableLayer { layer: layer.name.clone() });
        }
    }
    Ok(nodes)
}

/// `parent`'s children bottom first, each group's own children built the same way.
fn children_of<'a>(
    document: &'a Document,
    parent: Option<LayerId>,
    seen: &mut HashSet<LayerId>,
    attached: &mut HashSet<LayerId>,
    depth: usize,
    depth_most: usize,
) -> Result<Vec<Node<'a>>, ExportError> {
    if depth > depth_most {
        let layer =
            parent.and_then(|id| document.layer(id)).map_or("<the root>".to_owned(), |layer| layer.name.clone());
        return Err(ExportError::UnreachableLayer { layer });
    }
    let mut out = Vec::new();
    for layer in document.layers() {
        if layer.kind == LayerKind::Mask || layer.parent != parent {
            continue;
        }
        seen.insert(layer.id);
        if layer.kind == LayerKind::Group {
            out.push(Node::Group {
                layer,
                children: children_of(document, Some(layer.id), seen, attached, depth + 1, depth_most)?,
            });
            continue;
        }
        let mask = document.mask_of(layer.id).and_then(|id| document.layer(id));
        if let Some(mask) = mask {
            attached.insert(mask.id);
        }
        out.push(Node::Paint { layer, mask });
    }
    Ok(out)
}

/// A layer's straight RGBA from the engine's premultiplied pixels, a = 0 giving rgb 0.
fn straight(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0_u8; data.len()];
    for (dst, px) in out.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
        let Ok([r, g, b, a]) = <[u8; 4]>::try_from(px) else { continue };
        let alpha = u32::from(a);
        let unpremultiply = |c: u8| -> u8 {
            if alpha == 0 {
                return 0;
            }
            let value = (u32::from(c) * 255 + alpha / 2) / alpha;
            u8::try_from(value.min(255)).unwrap_or(u8::MAX)
        };
        dst.copy_from_slice(&[unpremultiply(r), unpremultiply(g), unpremultiply(b), a]);
    }
    out
}

/// One coverage byte per pixel from a mask layer's opaque grey pixels (light shows the layer).
fn coverage_values(data: &[u8]) -> Vec<u8> {
    data.chunks_exact(4)
        .map(|px| match <[u8; 4]>::try_from(px) {
            Ok(px) => mask_grey(px),
            Err(_) => 0,
        })
        .collect()
}

/// A mask's coverage as RGBA, which is the shape the CLIP STUDIO PAINT writer reads it in.
///
/// The coverage goes in every channel as well as alpha: the writer keeps the alpha, while CLIP
/// STUDIO PAINT may read the luminance of a mask, and both then carry the same value.
fn coverage_rgba(values: &[u8]) -> Vec<u8> {
    let mut out = vec![0_u8; values.len().saturating_mul(4)];
    for (dst, value) in out.chunks_exact_mut(4).zip(values.iter().copied()) {
        dst.copy_from_slice(&[value, value, value, value]);
    }
    out
}

/// CLIP STUDIO PAINT stores opacity as 0..=256, Krita as 0..=255.
fn opacity_256(opacity: f32) -> i64 {
    let clamped = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 1.0 };
    (f64::from(clamped) * 256.0).round() as i64
}

fn opacity_255(opacity: f32) -> u32 {
    let clamped = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 1.0 };
    (f64::from(clamped) * 255.0).round().clamp(0.0, 255.0) as u32
}

/// The engine's blend mode as CLIP STUDIO PAINT's, or the error naming the one it has none for.
fn clip_blend(layer: &Layer, format: &'static str) -> Result<i64, ExportError> {
    Ok(match layer.blend {
        BlendMode::Normal => blend::MODE_NORMAL,
        BlendMode::Multiply => blend::MODE_MULTIPLY,
        BlendMode::Darken => blend::MODE_DARKEN,
        BlendMode::ColorBurn => blend::MODE_COLOR_BURN,
        BlendMode::LinearBurn => blend::MODE_LINEAR_BURN,
        BlendMode::DarkerColor => blend::MODE_DARKER_COLOR,
        BlendMode::Lighten => blend::MODE_LIGHTEN,
        BlendMode::Screen => blend::MODE_SCREEN,
        BlendMode::ColorDodge => blend::MODE_COLOR_DODGE,
        BlendMode::Add => blend::MODE_ADD,
        BlendMode::LighterColor => blend::MODE_LIGHTER_COLOR,
        BlendMode::Overlay => blend::MODE_OVERLAY,
        BlendMode::SoftLight => blend::MODE_SOFT_LIGHT,
        BlendMode::HardLight => blend::MODE_HARD_LIGHT,
        BlendMode::VividLight => blend::MODE_VIVID_LIGHT,
        BlendMode::LinearLight => blend::MODE_LINEAR_LIGHT,
        BlendMode::PinLight => blend::MODE_PIN_LIGHT,
        BlendMode::HardMix => blend::MODE_HARD_MIX,
        BlendMode::Difference => blend::MODE_DIFFERENCE,
        BlendMode::Exclusion => blend::MODE_EXCLUSION,
        BlendMode::Subtract => blend::MODE_SUBTRACT,
        BlendMode::Divide => blend::MODE_DIVIDE,
        BlendMode::Hue => blend::MODE_HUE,
        BlendMode::Saturation => blend::MODE_SATURATION,
        BlendMode::Color => blend::MODE_COLOR,
        BlendMode::Luminosity => blend::MODE_LUMINOSITY,
        // The engine's Shade has no CLIP STUDIO PAINT counterpart, and Normal would change it.
        BlendMode::Shade => {
            return Err(ExportError::Unrepresentable {
                format,
                layer: layer.name.clone(),
                what: "the Shade blend mode",
            });
        }
    })
}

// ---------------------------------------------------------------------------------------------
// CLIP STUDIO PAINT
// ---------------------------------------------------------------------------------------------

/// Write a `.clip` from the bundled CSP template, replacing its layer stack with the document's.
fn export_clip(
    path: &Path,
    document: &Document,
    nodes: &[Node<'_>],
    source: &PixelSource<'_>,
    warnings: &mut Vec<ExportWarning>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    let (width, height) = (document.width(), document.height());
    let mut writer =
        ClipWriter::blank(width, height).map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    let root =
        writer.canvas_root_folder().map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    let paper = clip_paper_layer(&writer)?;
    let mut template = Some(clip_template_layer(&writer)?);
    let mut ids = HashMap::new();

    // Folders first: a child needs its group's row to hang from, whichever order the stack lists.
    create_clip_folders(&mut writer, nodes, root, &mut ids, warnings)?;

    let mut first_paint = None;
    create_clip_paints(
        &mut writer,
        nodes,
        root,
        &mut template,
        &mut ids,
        &mut first_paint,
        source,
        width,
        height,
        warnings,
    )?;
    // A template layer nobody reused would sit in the exported file as a blank layer.
    if let Some(template) = template {
        writer.delete_layer(template).map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    }

    order_clip(&mut writer, nodes, root, paper, &ids)?;
    write_paper(&mut writer, document, paper, width, height)?;
    set_canvas(&writer, document, &ids, first_paint)?;

    writer.refresh_preview(25.0).map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    writer.save(path).map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    Ok(())
}

/// The template's paper row: the canvas's background colour and visibility live on it.
fn clip_paper_layer(writer: &ClipWriter) -> Result<i64, ExportError> {
    writer
        .db
        .query_row("select MainId from Layer where LayerType=1584 limit 1", [], |row| row.get::<_, i64>(0))
        .map_err(|error| ExportError::Write {
            format: ExportFormat::Clip.label(),
            message: format!("the blank CLIP STUDIO PAINT template has no paper layer: {error}"),
        })
}

/// The template's raster row, reused for the first paint layer and copied for the others.
fn clip_template_layer(writer: &ClipWriter) -> Result<i64, ExportError> {
    writer
        .db
        .query_row(
            "select MainId from Layer where LayerType=1 and LayerFolder&1=0 \
             and LayerRenderMipmap is not null and LayerRenderMipmap<>0 limit 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| ExportError::Write {
            format: ExportFormat::Clip.label(),
            message: format!("the blank CLIP STUDIO PAINT template has no raster layer: {error}"),
        })
}

/// One CLIP STUDIO PAINT folder per group, parents before the groups they hold.
fn create_clip_folders(
    writer: &mut ClipWriter,
    nodes: &[Node<'_>],
    parent: i64,
    ids: &mut HashMap<LayerId, i64>,
    warnings: &mut Vec<ExportWarning>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    for node in nodes {
        let Node::Group { layer, children } = node else { continue };
        check_clip_group(layer, warnings)?;
        let (opacity, composite) = if layer.passes_through() {
            (256, blend::MODE_PASS_THROUGH)
        } else {
            let composite = clip_blend(layer, format)?;
            let opacity = opacity_256(layer.opacity);
            // A Normal group under full opacity must stay isolated when it is read back: 256
            // would make the engine treat it as pass-through again.
            let opacity = if composite == blend::MODE_NORMAL { opacity.min(255) } else { opacity };
            (opacity, composite)
        };
        let id = writer
            .add_folder_in(parent, 0, layer.name.as_str())
            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
        writer
            .set_layer_attr(id, None, Some(opacity), Some(composite), Some(layer.visible))
            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
        ids.insert(layer.id, id);
        create_clip_folders(writer, children, id, ids, warnings)?;
    }
    Ok(())
}

/// One CLIP STUDIO PAINT raster layer per paint layer, with its mask.
#[allow(clippy::too_many_arguments)]
fn create_clip_paints(
    writer: &mut ClipWriter,
    nodes: &[Node<'_>],
    parent: i64,
    template: &mut Option<i64>,
    ids: &mut HashMap<LayerId, i64>,
    first_paint: &mut Option<i64>,
    source: &PixelSource<'_>,
    width: u32,
    height: u32,
    warnings: &mut Vec<ExportWarning>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    for node in nodes {
        match node {
            Node::Group { layer, children } => {
                let Some(&id) = ids.get(&layer.id) else {
                    return Err(ExportError::UnreachableLayer { layer: layer.name.clone() });
                };
                create_clip_paints(writer, children, id, template, ids, first_paint, source, width, height, warnings)?;
            }
            Node::Paint { layer, mask } => {
                warn_paint_state(layer, ExportFormat::Clip, warnings);
                let composite = clip_blend(layer, format)?;
                let id = match template.take() {
                    Some(id) => {
                        writer
                            .set_layer_attr(
                                id,
                                Some(layer.name.as_str()),
                                Some(opacity_256(layer.opacity)),
                                Some(composite),
                                Some(layer.visible),
                            )
                            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                        id
                    }
                    None => {
                        let id = writer
                            .add_raster_layer_in(parent, 0, layer.name.as_str())
                            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                        writer
                            .set_layer_attr(
                                id,
                                None,
                                Some(opacity_256(layer.opacity)),
                                Some(composite),
                                Some(layer.visible),
                            )
                            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                        id
                    }
                };
                if first_paint.is_none() {
                    *first_paint = Some(id);
                }
                ids.insert(layer.id, id);
                writer
                    .set_layer_clip(id, layer.clipped)
                    .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                writer
                    .set_layer_locks(id, layer.alpha_lock, false, layer.reference, false)
                    .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                // The engine's pixels are canvas-sized: the native surface starts at the origin.
                writer
                    .set_layer_surface_offsets(id, 0, 0)
                    .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                let pixels = straight(source.get(layer, width, height)?);
                writer
                    .set_pixels(id, &pixels, width, height)
                    .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                if let Some(mask) = mask {
                    let coverage = coverage_values(source.get(mask, width, height)?);
                    writer
                        .add_layer_mask(id)
                        .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                    writer
                        .set_mask_pixels(id, &coverage_rgba(&coverage), width, height)
                        .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                    // Attaching a mask enables it; a hidden mask must not come back attenuating
                    // the layer, so its visibility bit is cleared again where the engine hid it.
                    if !mask.visible {
                        writer
                            .db
                            .execute(
                                "update Layer set LayerVisibility=(coalesce(LayerVisibility,1) & ~2) \
                                 where MainId=?1",
                                [id],
                            )
                            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rewrite every sibling chain in the document's order: paper first in the root, as in a CSP file.
fn order_clip(
    writer: &mut ClipWriter,
    nodes: &[Node<'_>],
    parent: i64,
    paper: i64,
    ids: &HashMap<LayerId, i64>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    let mut top = Vec::with_capacity(nodes.len() + 1);
    top.push(paper);
    for node in nodes {
        let layer = match node {
            Node::Paint { layer, .. } | Node::Group { layer, .. } => layer,
        };
        let Some(&id) = ids.get(&layer.id) else {
            return Err(ExportError::UnreachableLayer { layer: layer.name.clone() });
        };
        top.push(id);
    }
    writer
        .set_sibling_order(parent, &top)
        .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    order_clip_groups(writer, nodes, ids)
}

/// The child chains of every group under `nodes`, each in the document's order.
fn order_clip_groups(
    writer: &mut ClipWriter,
    nodes: &[Node<'_>],
    ids: &HashMap<LayerId, i64>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    for node in nodes {
        let Node::Group { layer, children } = node else { continue };
        let Some(&id) = ids.get(&layer.id) else {
            return Err(ExportError::UnreachableLayer { layer: layer.name.clone() });
        };
        let mut list = Vec::with_capacity(children.len());
        for child in children {
            let child = match child {
                Node::Paint { layer, .. } | Node::Group { layer, .. } => layer,
            };
            let Some(&child_id) = ids.get(&child.id) else {
                return Err(ExportError::UnreachableLayer { layer: child.name.clone() });
            };
            list.push(child_id);
        }
        writer
            .set_sibling_order(id, &list)
            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
        order_clip_groups(writer, children, ids)?;
    }
    Ok(())
}

/// The paper row carries the canvas's background colour and whether it is shown.
fn write_paper(
    writer: &mut ClipWriter,
    document: &Document,
    paper: i64,
    width: u32,
    height: u32,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    let [r, g, b] =
        document.background.map(|channel| (f64::from(channel.clamp(0.0, 1.0)) * 255.0).round().clamp(0.0, 255.0) as u8);
    let mut flat = vec![0_u8; width as usize * height as usize * 4];
    for px in flat.chunks_exact_mut(4) {
        px.copy_from_slice(&[r, g, b, 255]);
    }
    writer
        .set_layer_attr(paper, Some("Paper"), Some(256), Some(blend::MODE_NORMAL), Some(document.background_visible))
        .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    writer
        .set_pixels(paper, &flat, width, height)
        .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    Ok(())
}

/// The canvas's resolution and the layer a CSP file opens on are not reachable through the writer's
/// setters, so they are written into the template's rows directly.
fn set_canvas(
    writer: &ClipWriter,
    document: &Document,
    ids: &HashMap<LayerId, i64>,
    first_paint: Option<i64>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Clip.label();
    let resolution = if document.dpi.is_finite() { f64::from(document.dpi.clamp(1.0, 9999.0)) } else { 72.0 };
    let current = ids.get(&document.current()).copied().or(first_paint);
    writer
        .db
        .execute("update Canvas set CanvasResolution=?1", [resolution])
        .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    if let Some(current) = current {
        writer
            .db
            .execute("update Canvas set CanvasCurrentLayer=?1, CanvasUnit=0", [current])
            .map_err(|error| ExportError::Write { format, message: error.to_string() })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Krita
// ---------------------------------------------------------------------------------------------

/// One layer of the `.kra` about to be written: its `maindoc.xml` attributes and its payloads.
struct KraNode {
    group: bool,
    name: String,
    filename: String,
    uuid: String,
    visible: bool,
    opacity: u32,
    mode: i64,
    locked: bool,
    collapsed: bool,
    selected: bool,
    passthrough: bool,
    pixels: LayerId,
    mask: Option<KraMask>,
    children: Vec<KraNode>,
}

struct KraMask {
    name: String,
    filename: String,
    uuid: String,
    pixels: LayerId,
    /// Whether the mask attenuates the layer at all, as the engine holds it.
    visible: bool,
}

/// Write a `.kra`: `mimetype`, `maindoc.xml`, one tiled entry per paint layer and mask, and the
/// merged preview Krita expects.
fn export_kra(
    path: &Path,
    document: &Document,
    nodes: &[Node<'_>],
    source: &PixelSource<'_>,
    name: &str,
    warnings: &mut Vec<ExportWarning>,
) -> Result<(), ExportError> {
    let format = ExportFormat::Kra.label();
    let (width, height) = (document.width(), document.height());
    let base = kra_name(name);
    warn_kra_canvas(document, ExportFormat::Kra, warnings);
    let mut counter = 0_u32;
    let tree = build_kra(nodes, document.current(), &mut counter, warnings, format)?;
    let merged = kra_composite(&tree, source, width, height)?;
    let preview = encode_png(&merged, width, height)?;

    let xml = kra_xml(&base, document, &tree);
    let directory = match path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => directory,
        _ => Path::new("."),
    };
    // A temporary of this directory: the rename that publishes it stays on one filesystem, and
    // `publish_file` fsyncs it, keeps the destination's permissions, follows a symlink and
    // fsyncs the directory, so a crash leaves either the old document or the new one.
    let temporary =
        tempfile::Builder::new().prefix(".sumi-kra-").suffix(".tmp").tempfile_in(directory).map_err(|error| {
            ExportError::Write {
                format,
                message: format!("preparing a temporary file for {}: {error}", path.display()),
            }
        })?;
    write_kra_archive(temporary.path(), &xml, &base, &tree, source, width, height, &preview, format)?;
    publish_file(temporary.path(), path)
        .map_err(|error| ExportError::Write { format, message: format!("publishing {}: {error}", path.display()) })?;
    Ok(())
}

/// The document's layers as the `.kra` tree, refusing what the format cannot hold.
fn build_kra(
    nodes: &[Node<'_>],
    current: LayerId,
    counter: &mut u32,
    warnings: &mut Vec<ExportWarning>,
    format: &'static str,
) -> Result<Vec<KraNode>, ExportError> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            Node::Group { layer, children } => {
                check_kra_unclipped(layer, "the clipping of a group")?;
                let mode = clip_blend(layer, format)?;
                let children = build_kra(children, current, counter, warnings, format)?;
                warn_kra_state(layer, warnings);
                let filename = next_name(counter, "layer");
                let opacity = if layer.passes_through() {
                    255
                } else {
                    let opacity = opacity_255(layer.opacity);
                    // The reader here ignores `passthrough`: a Normal group at full opacity has to
                    // stay under it so it is not read back as pass-through.
                    if mode == blend::MODE_NORMAL { opacity.min(254) } else { opacity }
                };
                out.push(KraNode {
                    group: true,
                    name: layer.name.clone(),
                    uuid: layer_uuid(counter, layer.id),
                    filename,
                    visible: layer.visible,
                    opacity,
                    mode,
                    locked: layer.locked,
                    collapsed: layer.collapsed,
                    // The engine only ever makes a paint layer current, so this names the group
                    // only for a document whose selection was moved there by other code.
                    selected: layer.id == current,
                    passthrough: layer.passes_through(),
                    pixels: layer.id,
                    mask: None,
                    children,
                });
            }
            Node::Paint { layer, mask } => {
                // Krita has no clipping flag: its transparency masks and groups are a different
                // model, and writing the flag as "normal" would change what is shown.
                check_kra_unclipped(layer, "the clipping")?;
                warn_paint_state(layer, ExportFormat::Kra, warnings);
                let mode = clip_blend(layer, format)?;
                let filename = next_name(counter, "layer");
                let mask = *mask;
                let mask = mask.map(|mask| KraMask {
                    name: mask.name.clone(),
                    filename: next_name(counter, "mask"),
                    uuid: layer_uuid(counter, mask.id),
                    pixels: mask.id,
                    visible: mask.visible,
                });
                out.push(KraNode {
                    group: false,
                    name: layer.name.clone(),
                    uuid: layer_uuid(counter, layer.id),
                    filename,
                    visible: layer.visible,
                    opacity: opacity_255(layer.opacity),
                    mode,
                    locked: layer.locked,
                    collapsed: layer.collapsed,
                    selected: layer.id == current,
                    passthrough: false,
                    pixels: layer.id,
                    mask,
                    children: Vec::new(),
                });
            }
        }
    }
    Ok(out)
}

/// The layer state `maindoc.xml` has no row for, said rather than dropped in silence.
fn warn_kra_state(layer: &Layer, warnings: &mut Vec<ExportWarning>) {
    warn_state(
        layer,
        &[(layer.alpha_lock, "alpha lock"), (layer.reference, "reference layer flag"), (layer.private, "private flag")],
        warnings,
    );
}

/// The artwork's name, safe both as the `IMAGE` name and as the archive's folder name.
fn kra_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    let trimmed = cleaned.trim();
    // `.` and `..` are not folder names: they would make every entry a path step out of the
    // archive's own root, so a name that is only dots becomes the plain fallback.
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') { "Untitled".to_owned() } else { trimmed.to_owned() }
}

fn next_name(counter: &mut u32, prefix: &str) -> String {
    *counter = counter.saturating_add(1);
    format!("{prefix}{}", *counter)
}

/// A unique-looking UUID for a layer; Krita's clone layers refer to layers by it.
fn layer_uuid(counter: &mut u32, id: LayerId) -> String {
    let a = mix(counter.wrapping_mul(0x9E37_79B9) as u64 ^ id.0);
    let b = mix(a ^ 0x9E37_79B9_7F4A_7C15);
    format!(
        "{{{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}}}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0x0fff) as u16,
        (b >> 48) as u16,
        b & 0xffff_ffff_ffff
    )
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The colour profile an exported document declares: the engine's pixels are straight sRGB, and
/// `maindoc.xml`'s `profile` is where Krita expects that to be said.
const SRGB_PROFILE: &str = "sRGB built-in";

/// The `.kra`'s `maindoc.xml`, layers top first as Krita lists them.
fn kra_xml(base: &str, document: &Document, nodes: &[KraNode]) -> String {
    let (width, height) = (document.width(), document.height());
    let dpi = if document.dpi.is_finite() { document.dpi.clamp(1.0, 9999.0).round() as u32 } else { 72 };
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(
        "<!DOCTYPE DOC PUBLIC '-//KDE//DTD krita 2.0//EN' \
         'http://www.calligra.org/DTD/krita-2.0.dtd'>\n",
    );
    xml.push_str(
        "<DOC xmlns=\"http://www.calligra.org/DTD/krita\" kritaVersion=\"5.2.0\" \
         syntaxVersion=\"2.0\" editor=\"Krita\">\n",
    );
    xml.push_str(&format!(
        " <IMAGE name=\"{}\" width=\"{width}\" height=\"{height}\" colorspacename=\"RGBA\" \
         profile=\"{profile}\" mime=\"application/x-kra\" x-res=\"{dpi}\" y-res=\"{dpi}\">\n",
        xml_escape(base),
        profile = SRGB_PROFILE,
    ));
    xml.push_str("  <layers>\n");
    write_kra_xml(&mut xml, nodes, 3);
    xml.push_str("  </layers>\n </IMAGE>\n</DOC>\n");
    xml
}

fn write_kra_xml(xml: &mut String, nodes: &[KraNode], depth: usize) {
    let pad = " ".repeat(depth);
    // The document keeps its children bottom first; Krita's XML lists them top first.
    for node in nodes.iter().rev() {
        xml.push_str(&pad);
        xml.push_str("<layer");
        attr(xml, "name", &xml_escape(&node.name));
        attr(xml, "nodetype", if node.group { "grouplayer" } else { "paintlayer" });
        attr(xml, "filename", &node.filename);
        attr(xml, "x", "0");
        attr(xml, "y", "0");
        attr(xml, "visible", if node.visible { "1" } else { "0" });
        attr(xml, "opacity", &node.opacity.to_string());
        attr(xml, "compositeop", composite_name(node.mode));
        attr(xml, "locked", if node.locked { "1" } else { "0" });
        attr(xml, "collapsed", if node.collapsed { "1" } else { "0" });
        attr(xml, "uuid", &node.uuid);
        attr(xml, "colorspacename", "RGBA");
        if node.group {
            attr(xml, "passthrough", if node.passthrough { "1" } else { "0" });
        }
        if node.selected {
            attr(xml, "selected", "true");
        }
        if node.group {
            xml.push_str(">\n");
            xml.push_str(&format!("{pad} <layers>\n"));
            write_kra_xml(xml, &node.children, depth + 2);
            xml.push_str(&format!("{pad} </layers>\n"));
            xml.push_str(&format!("{pad}</layer>\n"));
        } else if let Some(mask) = &node.mask {
            xml.push_str(">\n");
            xml.push_str(&format!("{pad} <masks>\n"));
            xml.push_str(&format!("{pad}  <mask"));
            attr(xml, "name", &xml_escape(&mask.name));
            attr(xml, "nodetype", "transparencymask");
            attr(xml, "filename", &mask.filename);
            attr(xml, "uuid", &mask.uuid);
            attr(xml, "visible", if mask.visible { "1" } else { "0" });
            attr(xml, "x", "0");
            attr(xml, "y", "0");
            xml.push_str("/>\n");
            xml.push_str(&format!("{pad} </masks>\n"));
            xml.push_str(&format!("{pad}</layer>\n"));
        } else {
            xml.push_str("/>\n");
        }
    }
}

fn attr(xml: &mut String, name: &str, value: &str) {
    xml.push(' ');
    xml.push_str(name);
    xml.push_str("=\"");
    xml.push_str(value);
    xml.push('"');
}

/// `text` escaped for an XML attribute or text node, with the characters XML 1.0 cannot carry
/// replaced by U+FFFD: an escaped-but-invalid layer name would otherwise corrupt the document the
/// export reports as written.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 allows tab, line feed, carriage return and everything from U+0020 up, save
            // the two noncharacters at the end of the BMP.
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) >= 0x20 && c != '\u{FFFE}' && c != '\u{FFFF}' => out.push(c),
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

/// Write the archive through a sibling temporary file, so a failure leaves no half a document.
#[allow(clippy::too_many_arguments)]
fn write_kra_archive(
    path: &Path,
    xml: &str,
    base: &str,
    nodes: &[KraNode],
    source: &PixelSource<'_>,
    width: u32,
    height: u32,
    preview: &[u8],
    format: &'static str,
) -> Result<(), ExportError> {
    write_kra_zip(path, xml, base, nodes, source, width, height, preview)
        .map_err(|error| ExportError::Write { format, message: error })
}

fn write_kra_zip(
    path: &Path,
    xml: &str,
    base: &str,
    nodes: &[KraNode],
    source: &PixelSource<'_>,
    width: u32,
    height: u32,
    preview: &[u8],
) -> Result<(), String> {
    const DOCUMENT_INFO: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE document-info PUBLIC '-//KDE//DTD document-info 1.1//EN' \
'http://www.calligra.org/DTD/document-info-1.1.dtd'>\n\
<document-info xmlns=\"http://www.calligra.org/DTD/document-info\">\
<about><author><full-name/></author></about></document-info>\n";
    let file = File::create(path).map_err(|error| format!("creating {}: {error}", path.display()))?;
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("mimetype", stored).map_err(|error| error.to_string())?;
    zip.write_all(b"application/x-krita").map_err(|error| error.to_string())?;
    zip.start_file("maindoc.xml", deflated).map_err(|error| error.to_string())?;
    zip.write_all(xml.as_bytes()).map_err(|error| error.to_string())?;
    zip.start_file("documentinfo.xml", deflated).map_err(|error| error.to_string())?;
    zip.write_all(DOCUMENT_INFO.as_bytes()).map_err(|error| error.to_string())?;
    write_kra_entries(&mut zip, nodes, source, width, height, base)?;
    zip.start_file("mergedimage.png", deflated).map_err(|error| error.to_string())?;
    zip.write_all(preview).map_err(|error| error.to_string())?;
    zip.start_file("preview.png", deflated).map_err(|error| error.to_string())?;
    zip.write_all(preview).map_err(|error| error.to_string())?;
    zip.finish().map_err(|error| error.to_string())?;
    Ok(())
}

fn write_kra_entries<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    nodes: &[KraNode],
    source: &PixelSource<'_>,
    width: u32,
    height: u32,
    base: &str,
) -> Result<(), String> {
    for node in nodes {
        if node.group {
            write_kra_entries(zip, &node.children, source, width, height, base)?;
            continue;
        }
        let pixels = source.get_by_id(node.pixels, &node.name, width, height).map_err(|error| error.to_string())?;
        let tiles = encode_tiles(&straight(pixels), width, height, 4);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file(format!("{base}/layers/{}", node.filename), deflated).map_err(|error| error.to_string())?;
        zip.write_all(&tiles).map_err(|error| error.to_string())?;
        if let Some(mask) = &node.mask {
            let mask_pixels =
                source.get_by_id(mask.pixels, &mask.name, width, height).map_err(|error| error.to_string())?;
            let coverage = coverage_values(mask_pixels);
            let tiles = encode_tiles(&coverage, width, height, 1);
            let path = format!("{base}/layers/{}.pixelselection", mask.filename);
            zip.start_file(path.as_str(), deflated).map_err(|error| error.to_string())?;
            zip.write_all(&tiles).map_err(|error| error.to_string())?;
            // Krita fills the areas its tiles leave out from this; visible reads 255.
            zip.start_file(format!("{path}.defaultpixel"), deflated).map_err(|error| error.to_string())?;
            zip.write_all(&[255]).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// A Krita tiled image stream: a header, then one 64 × 64 tile per row and column.
///
/// Tiles are written raw (a zero storage flag) and interleaved: BGRA for a paint layer, one
/// coverage byte per pixel for a mask. That is the shape Krita's own version-2 writer accepts.
fn encode_tiles(data: &[u8], width: u32, height: u32, pixel_size: usize) -> Vec<u8> {
    let tile_w = 64_u32;
    let tile_h = 64_u32;
    let columns = width.div_ceil(tile_w);
    let rows = height.div_ceil(tile_h);
    let count = columns.saturating_mul(rows);
    let mut out = Vec::new();
    out.extend_from_slice(b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\n");
    out.extend_from_slice(format!("PIXELSIZE {pixel_size}\nDATA {count}\n").as_bytes());
    let tile_bytes = (tile_w * tile_h) as usize * pixel_size;
    let mut tile = vec![0_u8; tile_bytes];
    for tile_y in 0..rows {
        for tile_x in 0..columns {
            tile.fill(0);
            let x0 = tile_x.saturating_mul(tile_w);
            let y0 = tile_y.saturating_mul(tile_h);
            let tile_columns = tile_w.min(width.saturating_sub(x0));
            let tile_rows = tile_h.min(height.saturating_sub(y0));
            for row in 0..tile_rows {
                for col in 0..tile_columns {
                    let from = (((y0 + row) as usize) * width as usize + (x0 + col) as usize) * pixel_size;
                    let to = ((row as usize) * tile_w as usize + col as usize) * pixel_size;
                    if let (Some(src), Some(dst)) =
                        (data.get(from..from + pixel_size), tile.get_mut(to..to + pixel_size))
                    {
                        if pixel_size == 4 {
                            // The reader's raw tiles are BGRA.
                            if let Ok([r, g, b, a]) = <[u8; 4]>::try_from(src) {
                                dst.copy_from_slice(&[b, g, r, a]);
                            }
                        } else {
                            dst.copy_from_slice(src);
                        }
                    }
                }
            }
            out.extend_from_slice(format!("{x0},{y0},LZF,{}\n", tile_bytes.saturating_add(1)).as_bytes());
            out.push(0);
            out.extend_from_slice(&tile);
        }
    }
    out
}

/// A straight-RGBA composite of the tree, the reference image Krita stores beside the layers.
///
/// The engine's paper is not part of it: Krita has no background layer. A mask the engine hides is
/// left out of it, exactly as it leaves the layer unattenuated. An isolated group is composited on
/// its own and then blended; a pass-through group's children blend with what is
/// below it, as they would ungrouped.
fn kra_composite(tree: &[KraNode], source: &PixelSource<'_>, width: u32, height: u32) -> Result<Vec<u8>, ExportError> {
    let mut out = vec![0_u8; width as usize * height as usize * 4];
    compose_kra(tree, &mut out, source, width, height)?;
    Ok(out)
}

fn compose_kra(
    nodes: &[KraNode],
    dst: &mut [u8],
    source: &PixelSource<'_>,
    width: u32,
    height: u32,
) -> Result<(), ExportError> {
    for node in nodes {
        if !node.visible {
            continue;
        }
        if node.group {
            if node.passthrough {
                compose_kra(&node.children, dst, source, width, height)?;
            } else {
                let mut inside = vec![0_u8; dst.len()];
                compose_kra(&node.children, &mut inside, source, width, height)?;
                blend_into(dst, &inside, None, node.opacity, node.mode);
            }
            continue;
        }
        let pixels = source.get_by_id(node.pixels, &node.name, width, height)?;
        let pixels = straight(pixels);
        // A mask the engine hides attenuates nothing in the stack, so it must attenuate nothing in
        // the preview either: the written file and its merged image have to agree.
        let coverage = match &node.mask {
            Some(mask) if mask.visible => {
                Some(coverage_values(source.get_by_id(mask.pixels, &mask.name, width, height)?))
            }
            _ => None,
        };
        blend_into(dst, &pixels, coverage.as_deref(), node.opacity, node.mode);
    }
    Ok(())
}

/// Blend one straight-RGBA image over `dst`, scaled by `opacity` (0..=255) and a mask's coverage.
fn blend_into(dst: &mut [u8], src: &[u8], coverage: Option<&[u8]>, opacity: u32, mode: i64) {
    let scale = opacity.min(255) as f32 / 255.0;
    for (index, px) in src.chunks_exact(4).enumerate() {
        let Ok([r, g, b, a]) = <[u8; 4]>::try_from(px) else { continue };
        if a == 0 {
            continue;
        }
        let mut alpha = f32::from(a) / 255.0 * scale;
        if let Some(coverage) = coverage {
            let coverage = coverage.get(index).copied().unwrap_or(0);
            alpha *= f32::from(coverage) / 255.0;
        }
        if alpha <= 0.0 {
            continue;
        }
        let at = index.saturating_mul(4);
        let Some(pixel) = dst.get_mut(at..at + 4) else { continue };
        let Ok([dr, dg, db, da]) = <[u8; 4]>::try_from(&*pixel) else { continue };
        let (rgb, out_alpha) = blend::over(
            mode,
            [f32::from(dr) / 255.0, f32::from(dg) / 255.0, f32::from(db) / 255.0],
            f32::from(da) / 255.0,
            [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0],
            alpha,
        );
        let [red, green, blue] = rgb;
        pixel.copy_from_slice(&[to_byte(red), to_byte(green), to_byte(blue), to_byte(out_alpha)]);
    }
}

fn to_byte(value: f32) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

fn encode_png(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, ExportError> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| ExportError::Write { format: ExportFormat::Kra.label(), message: error.to_string() })?;
        writer
            .write_image_data(data)
            .map_err(|error| ExportError::Write { format: ExportFormat::Kra.label(), message: error.to_string() })?;
        writer
            .finish()
            .map_err(|error| ExportError::Write { format: ExportFormat::Kra.label(), message: error.to_string() })?;
    }
    Ok(png)
}
