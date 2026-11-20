//! Reading CLIP STUDIO PAINT (`.clip`) and Krita (`.kra`) documents into a native artwork, and
//! writing a native artwork back out to either of them.
//!
//! The parsers live in `clip-core`; this crate maps their layer trees onto the engine's
//! [`Document`] and pixel buffers, refusing what it would otherwise lose silently. Importing
//! reads the original file and never writes it; [`export`] writes a new file at a destination of
//! its own, and never reads the document an import came from.

use std::collections::HashMap;
use std::path::Path;

use clip_core::blend;
use clip_core::kra::{KraDocument, KraLayer, KraNode};
use clip_core::model::CanvasUnit;
use clip_core::{ClipFile, Image, Layer as ClipLayer, layer_mask, layer_surface};
use sumi_engine::document::{BlendMode, Document, Layer, LayerId, LayerKind, canvas_edge_most, layer_cap_for};

pub mod export;

pub use export::{ExportError, ExportFormat, ExportWarning, export, preflight_export};

mod preflight;

/// CLIP STUDIO PAINT's `LayerLock` bits: 1 is the alpha lock, 2 the position lock and 16 the
/// reference-layer flag. The first and last name states the engine keeps; a layer's other bits name
/// locks it has no field for.
const LOCK_ALPHA: i64 = 1;
const LOCK_POSITION: i64 = 2;
const LOCK_REFERENCE: i64 = 16;

/// A document read from a foreign file, ready to be saved as a native artwork.
pub struct ImportedArtwork {
    pub document: Document,
    /// The name to give the artwork: the file's stem, as the user knows it.
    pub name: String,
    /// Paint layers' pixels (premultiplied RGBA, the canvas's size) and masks', by id.
    pub pixels: Vec<(LayerId, Vec<u8>)>,
    /// What the file held that the native document cannot: said, never dropped in silence.
    pub warnings: Vec<ImportWarning>,
}

/// A part of a foreign document that its native copy cannot keep as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportWarning {
    /// A text layer was imported as its painted pixels: the text is no longer editable.
    RasterizedText { layer: String },
    /// A vector layer was imported as its painted pixels: the strokes are no longer editable.
    RasterizedVector { layer: String },
    /// A clone layer was imported as its painted pixels: it no longer follows its source.
    RasterizedClone { layer: String },
    /// The file clips a layer where the native document cannot: it was imported unclipped.
    Unclipped { layer: String },
    /// The file's group is isolated, which the native document cannot keep: its rendering is
    /// pass-through, where the file's is not.
    GroupIsolationLost { layer: String },
    /// The layer's blend mode has no exact counterpart here: the closest one was used.
    ApproximatedBlend { layer: String },
    /// The paper layer held more than one colour: the canvas's background is the paper's colour
    /// where it is flat, and white otherwise.
    ApproximatedPaper,
    /// The file says the layer has a mask, but it holds no pixels to read: the layer was imported
    /// unmasked, which is how CSP itself shows a mask without content.
    MaskNotRead { layer: String },
    /// The layer's mask is switched off in the file, and the reader skips a mask it is told not to
    /// show: the layer was imported without it.
    MaskHiddenNotRead { layer: String },
    /// A colour profile the reader never applies: the pixels it decodes were taken as sRGB.
    ProfileNotApplied { profile: String },
    /// The file keeps a layer state this import has no field for: it was left out.
    StateNotRead { layer: String, what: &'static str },
}

impl ImportWarning {
    /// The warning as one line for a person.
    pub fn message(&self) -> String {
        match self {
            ImportWarning::RasterizedText { layer } => {
                format!("the text layer {layer:?} was imported as pixels and is no longer editable")
            }
            ImportWarning::RasterizedVector { layer } => {
                format!("the vector layer {layer:?} was imported as pixels and is no longer editable")
            }
            ImportWarning::RasterizedClone { layer } => {
                format!("the clone layer {layer:?} was imported as pixels and no longer follows its source")
            }
            ImportWarning::Unclipped { layer } => {
                format!("the layer {layer:?} is clipped in the file but cannot be clipped here; imported unclipped")
            }
            ImportWarning::GroupIsolationLost { layer } => {
                format!("the group {layer:?} is isolated in the file; it was imported as a pass-through group")
            }
            ImportWarning::ApproximatedBlend { layer } => {
                format!("the blend mode of the layer {layer:?} has no exact counterpart here: the closest one was used")
            }
            ImportWarning::ApproximatedPaper => {
                "the paper layer was not one colour; it was imported on white".to_owned()
            }
            ImportWarning::MaskNotRead { layer } => {
                format!("the mask of the layer {layer:?} holds no pixels; the layer was imported unmasked")
            }
            ImportWarning::MaskHiddenNotRead { layer } => {
                format!(
                    "the mask of the layer {layer:?} is switched off in the file; it was not read, so \
                     the layer was imported without it"
                )
            }
            ImportWarning::ProfileNotApplied { profile } => {
                format!("the colour profile {profile:?} was not applied; every pixel was read as sRGB")
            }
            ImportWarning::StateNotRead { layer, what } => {
                format!("the {what} of the layer {layer:?} is in the file and was not imported")
            }
        }
    }
}

/// Why a foreign document could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("this is not a .clip or .kra document")]
    UnknownKind,
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Unreadable(String),
    #[error("the document is {width} by {height} pixels, larger than the {most} a canvas may be here")]
    TooLarge { width: u32, height: u32, most: u32 },
    #[error("the document has {count} painting layers, more than the {most} a canvas this size holds")]
    TooManyLayers { count: usize, most: usize },
}

/// Reads the `.clip` or `.kra` document at `path` into a native document.
pub fn import(path: &Path) -> Result<ImportedArtwork, ImportError> {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("clip") => import_clip(path),
        Some("kra") => import_kra(path),
        _ => Err(ImportError::UnknownKind),
    }
}

/// A `.clip` document's layers, bottom first, as the engine's own.
fn import_clip(path: &Path) -> Result<ImportedArtwork, ImportError> {
    let file = ClipFile::open(path).map_err(|e| unreadable(path, &e))?;
    if let CanvasUnit::Unknown(unit) = file.canvas.unit {
        return Err(ImportError::Unsupported(format!("the canvas's unit ({unit}) is one this app does not know")));
    }
    let (width, height) = file.canvas.pixel_size();
    let most = canvas_edge_most();
    if width > most || height > most {
        return Err(ImportError::TooLarge { width, height, most });
    }

    let mut building = ClipBuilding {
        layers: Vec::with_capacity(file.layers.len()),
        pixels: Vec::with_capacity(file.layers.len()),
        warnings: Vec::new(),
        next_id: 1,
        painted: 0,
        cap: layer_cap_for(width, height),
        ids: HashMap::new(),
        paper: None,
        clipped: Vec::new(),
    };
    clip_nodes(&file, &file.children(file.canvas.root_folder), None, width, height, path, &mut building)?;

    let ClipBuilding { layers, mut pixels, mut warnings, ids, paper, clipped, .. } = building;
    let current = clip_current(&layers, file.canvas.current_layer, &ids);
    let mut document = Document::from_layers(width, height, layers, current);
    document.dpi = file.canvas.resolution.clamp(1.0, 9999.0) as f32;
    let paper = paper.and_then(|id| file.layers.get(&id));
    document.background_visible = paper.is_some_and(|paper| paper.is_visible());
    if let Some(paper) = paper {
        document.background = paper_color(&file, paper, path, &mut warnings)?;
    }
    // the engine keeps a clip only where a paint layer sits directly below it in the same group;
    // it drops the rest while building the document, so the loss is reported here and the
    // honest unclipped rendering is left as it is
    for (id, name) in clipped {
        if !document.layer(id).is_some_and(|layer| layer.clipped) {
            warnings.push(ImportWarning::Unclipped { layer: name });
        }
    }
    // a canvas whose document gained a layer (an empty stack gets one) starts it empty
    for layer in document.layers() {
        if layer.kind != LayerKind::Group && !pixels.iter().any(|(id, _)| *id == layer.id) {
            pixels.push((layer.id, vec![0; width as usize * height as usize * 4]));
        }
    }

    Ok(ImportedArtwork { document, name: artwork_name(path), pixels, warnings })
}

/// What mapping a `.clip` layer tree has gathered so far.
struct ClipBuilding {
    layers: Vec<Layer>,
    pixels: Vec<(LayerId, Vec<u8>)>,
    warnings: Vec<ImportWarning>,
    next_id: u64,
    /// Paint layers and masks: what [`layer_cap_for`] counts.
    painted: usize,
    /// The canvas's layer cap, fixed before the walk and never exceeded: a layer's pixel buffer is
    /// only allocated once [`ClipBuilding::pixel_room`] has said it fits.
    cap: usize,
    /// Every pushed layer's CSP `MainId` as the engine's, for the current layer.
    ids: HashMap<i64, LayerId>,
    /// The paper layer's `MainId`: it becomes the canvas's background.
    paper: Option<i64>,
    /// Paint layers the file clips, with their names: the built document is checked for each,
    /// since it drops the clip it cannot honour.
    clipped: Vec<(LayerId, String)>,
}

impl ClipBuilding {
    fn id(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Whether one more pixel buffer (a paint layer's or a mask's) fits the canvas's cap: checked
    /// before that buffer is allocated, so a file claiming many layers cannot exhaust memory.
    fn pixel_room(&self) -> Result<(), ImportError> {
        let count = self.painted + 1;
        if count > self.cap {
            return Err(ImportError::TooManyLayers { count, most: self.cap });
        }
        Ok(())
    }
}

/// `main_ids`, bottom first, and everything a group holds added to `building`: a group's contents
/// sit just below it, a mask just above its layer, as the engine keeps them.
fn clip_nodes(
    file: &ClipFile,
    main_ids: &[i64],
    parent: Option<LayerId>,
    width: u32,
    height: u32,
    path: &Path,
    building: &mut ClipBuilding,
) -> Result<(), ImportError> {
    for main_id in main_ids {
        let Some(source) = file.layers.get(main_id) else {
            // A child the file's own index does not hold: nothing of it can be read, so it is
            // refused by id rather than skipped as if the stack were shorter.
            return Err(ImportError::Unreadable(format!(
                "the document's layer tree names layer {main_id}, which the document does not hold"
            )));
        };
        if source.is_root() {
            continue;
        }
        if source.is_paper() {
            if building.paper.replace(source.main_id).is_some() {
                return Err(ImportError::Unsupported("the document has more than one paper layer".to_owned()));
            }
            continue;
        }
        if source.is_folder() {
            // The engine has no group mask: attenuating everything a group holds cannot be kept.
            // A mask CSP does not paint (its visibility bit clear) attenuates nothing.
            if source.mask_mipmap != 0 && source.visibility & 2 != 0 {
                return Err(ImportError::Unsupported(format!(
                    "the mask of the group {:?} is not read yet",
                    source.name
                )));
            }
            // The engine can only clip a paint layer: a clipped group's clip is lost, said here.
            if source.clip != 0 {
                building.warnings.push(ImportWarning::Unclipped { layer: source.name.clone() });
            }
            // The engine renders every Normal, fully opaque group as pass-through; an isolated
            // group differs from that only when what it holds can show it.
            if source.composite == blend::MODE_NORMAL && source.alpha() >= 1.0 && clip_isolation_matters(file, source) {
                building.warnings.push(ImportWarning::GroupIsolationLost { layer: source.name.clone() });
            }
            let id = building.id();
            building.ids.insert(source.main_id, id);
            clip_nodes(file, &file.children(source.main_id), Some(id), width, height, path, building)?;
            building.layers.push(clip_group(source, id, parent, &mut building.warnings)?);
            continue;
        }
        if source.is_adjustment() {
            return Err(ImportError::Unsupported(format!("the adjustment layer {:?} is not read yet", source.name)));
        }
        if source.special_render_type == 14 {
            return Err(ImportError::Unsupported(format!("the frame border layer {:?} is not read yet", source.name)));
        }
        // A `.clip` text layer's surface is only its bubble shape: the glyphs stay in
        // `TextLayerString`, so importing the pixels would lose the text.
        if file.has_text_layer(source.main_id) {
            return Err(ImportError::Unsupported(format!("the text layer {:?} is not read yet", source.name)));
        }
        if !file.vector_objects(source.main_id).map_err(|e| unreadable(path, &e))?.is_empty() {
            building.warnings.push(ImportWarning::RasterizedVector { layer: source.name.clone() });
        }

        let id = building.id();
        building.ids.insert(source.main_id, id);
        if source.clip != 0 {
            building.clipped.push((id, source.name.clone()));
        }
        building.layers.push(native_layer(id, source, parent, &mut building.warnings)?);
        building.pixel_room()?;
        building.pixels.push((id, clip_pixels(file, source, width, height, path)?));
        building.painted += 1;

        // CSP paints a mask only when its own visibility bit is set; a mask with no pixels at all
        // cannot attenuate anything, so it is absent rather than lost.
        if source.mask_mipmap != 0 && source.visibility & 2 != 0 {
            building.pixel_room()?;
            let mask = layer_mask(file, source, 100.0).map_err(|e| unreadable(path, &e))?;
            if let Some(image) = mask {
                let mask_id = building.id();
                building.layers.push(plain_layer(mask_id, "Mask".to_owned(), LayerKind::Mask, Some(id)));
                building
                    .pixels
                    .push((mask_id, mask_pixels(&image, source.mask_offset_x, source.mask_offset_y, width, height)));
                building.painted += 1;
            } else {
                // The file enables a mask whose mip chain carries no payload: the layer comes back
                // without the attenuation the file declares, and that is said, not dropped.
                building.warnings.push(ImportWarning::MaskNotRead { layer: source.name.clone() });
            }
        } else if source.mask_mipmap != 0 && clip_mask_painted(file, source) {
            // The file holds a mask with painted coverage and switched it off. The stack keeps no
            // attenuation either way, but the mask is real state the imported artwork loses, so it
            // is said exactly as the `.kra` preflight says a mask the file switched off.
            building.warnings.push(ImportWarning::MaskHiddenNotRead { layer: source.name.clone() });
        }
    }
    Ok(())
}

/// The layer the file has current when it is a paint layer; the first paint layer otherwise.
fn clip_current(layers: &[Layer], csp_current: i64, ids: &HashMap<i64, LayerId>) -> LayerId {
    if let Some(&id) = ids.get(&csp_current) {
        if layers.iter().any(|layer| layer.id == id && layer.kind == LayerKind::Paint) {
            return id;
        }
    }
    layers.iter().find(|layer| layer.kind == LayerKind::Paint).map_or(LayerId(1), |layer| layer.id)
}

/// A `.kra` document's layers, bottom first, as the engine's own.
fn import_kra(path: &Path) -> Result<ImportedArtwork, ImportError> {
    // The reader takes a path and reopens it, so both passes read one private copy: a file swapped
    // between them cannot make the preflight describe bytes the reader never saw.
    let snapshot = preflight::Snapshot::of(path)?;
    let expected = preflight::inspect(&snapshot)?;
    let file = KraDocument::open(snapshot.path()).map_err(|error| reader_error(path, &error))?;
    // By identity, not by count: a layer the reader dropped or reparented is refused here, before
    // any pixel is mapped.
    preflight::reconcile(&snapshot, &expected, &file)?;
    let (width, height) = (file.width, file.height);
    let most = canvas_edge_most();
    if width > most || height > most {
        return Err(ImportError::TooLarge { width, height, most });
    }

    let mut building = Building {
        layers: Vec::new(),
        pixels: Vec::new(),
        warnings: expected.warnings,
        next_id: 1,
        painted: 0,
        cap: layer_cap_for(width, height),
    };
    kra_nodes(&file, &file.root_children, None, &mut building)?;

    let Building { layers, mut pixels, warnings, .. } = building;
    // the topmost paint layer is the one a file opens on
    let current = layers.iter().rev().find(|layer| layer.kind == LayerKind::Paint).map_or(LayerId(1), |layer| layer.id);
    let mut document = Document::from_layers(width, height, layers, current);
    document.dpi = file.ppi.clamp(1.0, 9999.0) as f32;
    // a `.kra` has no paper layer: nothing is painted behind the layers
    document.background_visible = false;
    // a canvas whose document gained a layer (an empty stack gets one) starts it empty
    for layer in document.layers() {
        if layer.kind != LayerKind::Group && !pixels.iter().any(|(id, _)| *id == layer.id) {
            pixels.push((layer.id, vec![0; width as usize * height as usize * 4]));
        }
    }

    Ok(ImportedArtwork { document, name: artwork_name(path), pixels, warnings })
}

/// What building the native stack has gathered so far.
struct Building {
    layers: Vec<Layer>,
    pixels: Vec<(LayerId, Vec<u8>)>,
    warnings: Vec<ImportWarning>,
    next_id: u64,
    /// Paint layers and masks: what [`layer_cap_for`] counts.
    painted: usize,
    /// The canvas's layer cap, fixed before the walk: see [`Building::pixel_room`].
    cap: usize,
}

impl Building {
    fn id(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Whether one more pixel buffer (a paint layer's or a mask's) fits the canvas's cap: checked
    /// before that buffer is allocated, so a file claiming many layers cannot exhaust memory.
    fn pixel_room(&self) -> Result<(), ImportError> {
        let count = self.painted + 1;
        if count > self.cap {
            return Err(ImportError::TooManyLayers { count, most: self.cap });
        }
        Ok(())
    }
}

/// `nodes`, bottom first, and everything a group holds added to `building`: a group's contents sit
/// just below it, a mask just above its layer, as the engine keeps them.
fn kra_nodes(
    file: &KraDocument,
    nodes: &[KraNode],
    parent: Option<LayerId>,
    building: &mut Building,
) -> Result<(), ImportError> {
    for node in nodes {
        match node {
            KraNode::Folder(index) => {
                let Some(folder) = file.folders.get(*index) else {
                    return Err(ImportError::Unreadable(format!(
                        "the document's layer tree names group {index}, which it does not hold"
                    )));
                };
                let id = building.id();
                kra_nodes(file, &folder.children, Some(id), building)?;
                let mut layer = plain_layer(id, folder.name.clone(), LayerKind::Group, parent);
                layer.visible = folder.visible;
                layer.opacity = folder.opacity as f32;
                layer.blend = blend_mode(folder.blend_mode, &folder.name, &mut building.warnings)?;
                building.layers.push(layer);
            }
            KraNode::Layer(index) => {
                let Some(source) = file.layers.get(*index) else {
                    return Err(ImportError::Unreadable(format!(
                        "the document's layer tree names layer {index}, which it does not hold"
                    )));
                };
                kra_layer(file, source, parent, building)?;
            }
        }
    }
    Ok(())
}

/// One paint layer (clone and vector layers included, as their resolved pixels) and its mask.
fn kra_layer(
    file: &KraDocument,
    source: &KraLayer,
    parent: Option<LayerId>,
    building: &mut Building,
) -> Result<(), ImportError> {
    let painted = source.data_path.is_some();
    if !painted && source.clone_from.is_none() && !has_coverage(&source.image) {
        return Err(ImportError::Unsupported(format!(
            "the layer {:?} has no paint data and no pixels: generator, filter and file layers are not read",
            source.name
        )));
    }
    if !painted {
        // the reader resolved a vector or clone layer into pixels: nothing of it stays editable
        let warning = if source.clone_from.is_some() {
            ImportWarning::RasterizedClone { layer: source.name.clone() }
        } else {
            ImportWarning::RasterizedVector { layer: source.name.clone() }
        };
        building.warnings.push(warning);
    }

    let mut layer = plain_layer(building.id(), source.name.clone(), LayerKind::Paint, parent);
    layer.visible = source.own_visible;
    layer.opacity = source.own_opacity as f32;
    layer.blend = blend_mode(source.blend_mode, &source.name, &mut building.warnings)?;
    let id = layer.id;
    building.layers.push(layer);
    building.pixel_room()?;
    building.pixels.push((id, place(&source.image, (0, 0), file.width, file.height)));
    building.painted += 1;

    if let Some(mask) = &source.mask {
        // the decoded mask is held already; only its canvas-sized coverage is allocated here
        building.pixel_room()?;
        let mask_id = building.id();
        building.layers.push(plain_layer(mask_id, "Mask".to_owned(), LayerKind::Mask, Some(id)));
        building.pixels.push((mask_id, coverage(mask, file.width, file.height)));
        building.painted += 1;
    }
    Ok(())
}

/// A layer as the engine keeps one before its pixels: shown, opaque, normal.
fn plain_layer(id: LayerId, name: String, kind: LayerKind, parent: Option<LayerId>) -> Layer {
    Layer {
        id,
        name,
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

/// Whether the image paints anything at all: some pixel is not fully transparent.
fn has_coverage(image: &Image) -> bool {
    image.data.chunks_exact(4).any(|px| px.get(3).is_some_and(|alpha| *alpha != 0))
}

/// A transparency mask's coverage as an opaque grey layer: the coverage in every channel.
fn coverage(image: &Image, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0_u8; width as usize * height as usize * 4];
    for (index, px) in image.data.chunks_exact(4).enumerate() {
        let Some(alpha) = px.get(3).copied() else { continue };
        let at = index * 4;
        let Some(dst) = out.get_mut(at..at + 4) else { break };
        dst.copy_from_slice(&[alpha, alpha, alpha, 255]);
    }
    out
}

/// A mask's coverage as an opaque grey layer on the canvas: every channel the coverage, alpha 255,
/// placed at the mask's own offset.
fn mask_pixels(image: &Image, offset_x: i64, offset_y: i64, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0_u8; width as usize * height as usize * 4];
    if image.width == 0 || image.height == 0 {
        return out;
    }
    for (index, px) in image.data.chunks_exact(4).enumerate() {
        let Some(alpha) = px.get(3).copied() else { continue };
        let x = offset_x + (index % image.width as usize) as i64;
        let y = offset_y + (index / image.width as usize) as i64;
        if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
            continue;
        }
        let at = ((y as u32 * width + x as u32) * 4) as usize;
        let Some(dst) = out.get_mut(at..at + 4) else { continue };
        dst.copy_from_slice(&[alpha, alpha, alpha, 255]);
    }
    out
}

/// The layer as the engine keeps it, bar its pixels.
fn native_layer(
    id: LayerId,
    source: &ClipLayer,
    parent: Option<LayerId>,
    warnings: &mut Vec<ImportWarning>,
) -> Result<Layer, ImportError> {
    let mut layer = plain_layer(id, source.name.clone(), LayerKind::Paint, parent);
    layer.visible = source.is_visible();
    layer.opacity = source.alpha();
    layer.blend = blend_mode(source.composite, &source.name, warnings)?;
    layer.clipped = source.clip != 0;
    keep_lock_flags(&mut layer, source, warnings);
    Ok(layer)
}

/// CSP's `LayerLock` bit field as the engine's own layer state.
///
/// The alpha lock and the reference-layer flag name states this app keeps, so they are mapped; the
/// position lock and any bit beyond these three have no field here, and are said rather than dropped.
fn keep_lock_flags(layer: &mut Layer, source: &ClipLayer, warnings: &mut Vec<ImportWarning>) {
    layer.alpha_lock = (source.lock_flags & LOCK_ALPHA) != 0;
    layer.reference = (source.lock_flags & LOCK_REFERENCE) != 0;
    if (source.lock_flags & LOCK_POSITION) != 0 {
        warnings.push(ImportWarning::StateNotRead { layer: source.name.clone(), what: "position lock" });
    }
    if (source.lock_flags & !(LOCK_ALPHA | LOCK_POSITION | LOCK_REFERENCE)) != 0 {
        warnings.push(ImportWarning::StateNotRead { layer: source.name.clone(), what: "unrecognised lock flag" });
    }
}

/// Whether a `.clip` layer's mask holds painted coverage, whatever the file's own visibility bit
/// says: the reader decodes a mask it is not told to show, so the pixels decide.
///
/// A mask whose own data will not decode carries nothing this import could have kept, so it is not
/// a loss to report — and a broken mask must not fail an import that used to succeed.
fn clip_mask_painted(file: &ClipFile, source: &ClipLayer) -> bool {
    layer_mask(file, source, 100.0).is_ok_and(|mask| mask.is_some_and(|image| has_coverage(&image)))
}

/// Whether an isolated group's rendering can differ from the pass-through one the engine gives it:
/// when a direct child blends other than normally, is clipped, or is an isolated group of its own.
fn clip_isolation_matters(file: &ClipFile, group: &ClipLayer) -> bool {
    file.children(group.main_id).iter().any(|main_id| {
        let Some(child) = file.layers.get(main_id) else { return false };
        let normal = if child.is_folder() {
            child.composite == blend::MODE_PASS_THROUGH
        } else {
            child.composite == blend::MODE_NORMAL
        };
        child.clip != 0 || !normal
    })
}

/// A CSP group as the engine's: pass-through composites as normal only when the group is fully
/// opaque, which is the case the engine's pass-through reproduces exactly.
fn clip_group(
    source: &ClipLayer,
    id: LayerId,
    parent: Option<LayerId>,
    warnings: &mut Vec<ImportWarning>,
) -> Result<Layer, ImportError> {
    let blend = if source.composite == blend::MODE_PASS_THROUGH {
        if source.alpha() >= 1.0 {
            BlendMode::Normal
        } else {
            return Err(ImportError::Unsupported(format!(
                "the group {:?} passes through at opacity {}, which has no counterpart here",
                source.name,
                source.alpha()
            )));
        }
    } else {
        blend_mode(source.composite, &source.name, warnings)?
    };
    let mut layer = plain_layer(id, source.name.clone(), LayerKind::Group, parent);
    layer.visible = source.is_visible();
    layer.opacity = source.alpha();
    layer.blend = blend;
    keep_lock_flags(&mut layer, source, warnings);
    Ok(layer)
}

/// A layer's composite mode (CSP's or Krita's) as the engine's, refusing the ones it has no exact
/// counterpart for; a glow mode is approximated by the plain one, and said so.
fn blend_mode(composite: i64, layer: &str, warnings: &mut Vec<ImportWarning>) -> Result<BlendMode, ImportError> {
    Ok(match composite {
        blend::MODE_NORMAL => BlendMode::Normal,
        blend::MODE_MULTIPLY => BlendMode::Multiply,
        blend::MODE_DARKEN => BlendMode::Darken,
        blend::MODE_COLOR_BURN => BlendMode::ColorBurn,
        blend::MODE_LINEAR_BURN => BlendMode::LinearBurn,
        blend::MODE_SUBTRACT => BlendMode::Subtract,
        blend::MODE_DARKER_COLOR => BlendMode::DarkerColor,
        blend::MODE_LIGHTEN => BlendMode::Lighten,
        blend::MODE_SCREEN => BlendMode::Screen,
        blend::MODE_COLOR_DODGE => BlendMode::ColorDodge,
        blend::MODE_COLOR_DODGE_GLOW => {
            warnings.push(ImportWarning::ApproximatedBlend { layer: layer.to_owned() });
            BlendMode::ColorDodge
        }
        blend::MODE_ADD => BlendMode::Add,
        blend::MODE_ADD_GLOW => {
            warnings.push(ImportWarning::ApproximatedBlend { layer: layer.to_owned() });
            BlendMode::Add
        }
        blend::MODE_LIGHTER_COLOR => BlendMode::LighterColor,
        blend::MODE_OVERLAY => BlendMode::Overlay,
        blend::MODE_SOFT_LIGHT => BlendMode::SoftLight,
        blend::MODE_HARD_LIGHT => BlendMode::HardLight,
        blend::MODE_VIVID_LIGHT => BlendMode::VividLight,
        blend::MODE_LINEAR_LIGHT => BlendMode::LinearLight,
        blend::MODE_PIN_LIGHT => BlendMode::PinLight,
        blend::MODE_HARD_MIX => BlendMode::HardMix,
        blend::MODE_DIFFERENCE => BlendMode::Difference,
        blend::MODE_EXCLUSION => BlendMode::Exclusion,
        blend::MODE_HUE => BlendMode::Hue,
        blend::MODE_SATURATION => BlendMode::Saturation,
        blend::MODE_COLOR => BlendMode::Color,
        blend::MODE_LUMINOSITY => BlendMode::Luminosity,
        blend::MODE_DIVIDE => BlendMode::Divide,
        other => {
            return Err(ImportError::Unsupported(format!(
                "the blend mode {other} of the layer {layer:?} has no exact counterpart here"
            )));
        }
    })
}

/// A layer's pixels on the canvas, premultiplied as the engine keeps them.
fn clip_pixels(
    file: &ClipFile,
    layer: &ClipLayer,
    width: u32,
    height: u32,
    path: &Path,
) -> Result<Vec<u8>, ImportError> {
    let Some(surface) = layer_surface(file, layer, 100.0).map_err(|e| unreadable(path, &e))? else {
        return Ok(vec![0; width as usize * height as usize * 4]);
    };
    Ok(place(&surface.image, surface.offset, width, height))
}

/// `image` placed at `offset` on a `width` × `height` canvas, clipped to it, premultiplied.
fn place(image: &Image, offset: (i32, i32), width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0_u8; width as usize * height as usize * 4];
    if image.width == 0 || image.height == 0 {
        return out;
    }
    for (index, px) in image.data.chunks_exact(4).enumerate() {
        let Ok([r, g, b, a]) = <[u8; 4]>::try_from(px) else { continue };
        let x = i64::from(offset.0) + (index % image.width as usize) as i64;
        let y = i64::from(offset.1) + (index / image.width as usize) as i64;
        if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
            continue;
        }
        let at = ((y as u32 * width + x as u32) * 4) as usize;
        let Some(dst) = out.get_mut(at..at + 4) else { continue };
        let alpha = u16::from(a);
        let over = |c: u8| ((u16::from(c) * alpha + 127) / 255) as u8;
        dst.copy_from_slice(&[over(r), over(g), over(b), a]);
    }
    out
}

/// The paper's colour where it is flat, or white.
fn paper_color(
    file: &ClipFile,
    paper: &ClipLayer,
    path: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<[f32; 3], ImportError> {
    let white = [1.0; 3];
    let Some(surface) = layer_surface(file, paper, 100.0).map_err(|e| unreadable(path, &e))? else {
        return Ok(white);
    };
    let mut found: Option<[u8; 3]> = None;
    for px in surface.image.data.chunks_exact(4) {
        let Ok([r, g, b, a]) = <[u8; 4]>::try_from(px) else { continue };
        if a == 0 {
            continue;
        }
        match found {
            Some(colour) if colour != [r, g, b] => {
                warnings.push(ImportWarning::ApproximatedPaper);
                return Ok(white);
            }
            Some(_) => {}
            None => found = Some([r, g, b]),
        }
    }
    Ok(found.map(|[r, g, b]| [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0]).unwrap_or(white))
}

/// The artwork's name: the file's, as the user knows it.
fn artwork_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or_else(|| "Untitled Artwork".to_owned())
}

fn unreadable(path: &Path, error: &clip_core::Error) -> ImportError {
    match error {
        // a file this app can read but this part of it does not understand: say what, not "unreadable"
        clip_core::Error::Unsupported(why) => ImportError::Unsupported(why.to_owned()),
        _ => ImportError::Unreadable(format!("reading {}: {error}", path.display())),
    }
}

/// A reader error as a message about the file the person chose.
///
/// The reader works on a private snapshot, so its outer context names a temporary path instead of
/// theirs; only the root of its chain says what is actually wrong with the document.
fn reader_error(path: &Path, error: &anyhow::Error) -> ImportError {
    let cause = error.chain().last().map_or_else(|| "an unknown error".to_owned(), ToString::to_string);
    ImportError::Unreadable(format!("reading {}: {cause}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_error_names_the_chosen_file_not_the_private_snapshot() {
        let error = anyhow::anyhow!(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or directory"))
            .context("open /tmp/sumi-clip-kra-4242/art.kra");
        let message = reader_error(Path::new("/home/artist/art.kra"), &error).to_string();
        assert!(message.contains("/home/artist/art.kra"), "the chosen file is named: {message}");
        assert!(!message.contains("sumi-clip-kra-"), "the snapshot is not: {message}");
        assert!(message.contains("no such file"), "the root cause is kept: {message}");
    }
}
