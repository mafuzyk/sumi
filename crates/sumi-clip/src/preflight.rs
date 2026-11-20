//! A conservative preflight for Krita (`.kra`) documents.
//!
//! `clip-core`'s reader (`KraDocument::open`) is read-only here, and it skips what it cannot read: a
//! paint layer whose tiles fail to decode, a mask with no data, a colour model, blend name or node
//! type it does not know all disappear or come back as `normal`. This module runs before the reader
//! and inspects the same bytes again after it, so the adapter can refuse a document instead of
//! importing a quiet subset of it:
//!
//! * `maindoc.xml` is parsed on its own: every colour space, node type, blend name, filename, UUID,
//!   clone reference, mask and tiled layer is checked against what the reader provably keeps;
//! * only the header and the tile table of a tiled layer are read (`VERSION` through `DATA`), and
//!   no payload is kept;
//! * the rows of a tile table are bounded by the entry's own size, never by the canvas: Krita
//!   paints on an unbounded canvas, stores tiles outside it, and the reader clips them;
//! * the parsed tree is reconciled with the reader's by identity (filename, kind, parent), so a
//!   layer the reader dropped without saying so is an error rather than an absence.
//!
//! Nothing here trusts a size the archive or its `maindoc.xml` declares. The private copy, the
//! number of layers that become a canvas-sized image, every image entry and every tile entry and
//! payload are bounded before the reader runs (see [`MAX_DOCUMENT_BYTES`] and
//! [`MAX_TILE_SIDE`]), so a hostile document is refused with an error naming what it claimed
//! instead of making the reader allocate it.
//!
//! Group pass-through is not exposed by the reader's `KraFolder`, so the decision is fixed here.
//! `passthrough="1"` is a pass-through group, which this engine reproduces exactly only when the
//! group is Normal and fully opaque; any other blend or opacity on one is refused. `passthrough="0"`
//! or a missing attribute is an isolated group (Krita's own default for a group that never opted
//! in). An isolated Normal group is what the engine itself renders as pass-through, so when a
//! direct child would show the difference it is imported with
//! [`ImportWarning::GroupIsolationLost`], mirroring the `.clip` adapter. Masks are refused unless
//! they are ordinary, visible transparency masks: the reader applies only the first transform mask
//! it can prove, only a Desaturate filter mask, and ignores masks on groups entirely. A mask the
//! file itself switched off (`visible="0"`, see the reader's own visibility filter) is inert to the
//! reader as well: a switched-off transform or filter mask changes no pixel and is left out, and a
//! switched-off transparency mask is the one mask the engine could have held, so leaving it out is
//! reported with [`ImportWarning::MaskHiddenNotRead`]. The embedded ICC profile is not read by the
//! reader at all: a profile on the `IMAGE` or on a layer that is not one of the names this importer
//! treats as sRGB is reported with [`ImportWarning::ProfileNotApplied`], because the pixels are
//! taken as sRGB whatever the profile says.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use clip_core::kra::{KraDocument, KraLayer, KraNode};
use roxmltree::{Document, Node};
use sumi_engine::document::{canvas_edge_most, layer_cap_for};

use crate::{ImportError, ImportWarning};

/// The only colour model whose 8-bit tiles the reader decodes as straight RGBA.
const SUPPORTED_COLORSPACE: &str = "RGBA";

/// The largest document this import copies into its private snapshot: 2 GiB, twice the largest
/// canvas the engine holds (16384 × 16384 premultiplied RGBA is 1 GiB) and as much again for the
/// tiles of its layers, so no artwork this app can have written is refused while a hostile file
/// cannot fill the small disk the temporary directory sits on.
const MAX_DOCUMENT_BYTES: u64 = 2 << 30;

/// The profile names this importer treats as sRGB, so pixels carrying one need no transform. It is
/// a short allowlist, not every name Krita may write for sRGB: any other name — including a linear
/// gamma sibling, a foreign working space, an empty value, or a real sRGB file this list does not
/// know — names a transform this import does not apply, so it is reported and the pixels are read
/// as sRGB. Nothing is refused on account of a profile.
const SRGB_PROFILES: [&str; 3] = ["sRGB built-in", "sRGB-elle-V2-srgbtrc.icc", "sRGB-elle-V4-srgbtrc.icc"];

/// Four bytes per pixel for paint data, one for masks: the reader's `decode_tiles_at` contract.
const PAINT_PIXELS: usize = 4;
const MASK_PIXELS: usize = 1;

/// `VERSION`, `TILEWIDTH`, `TILEHEIGHT`, `PIXELSIZE`, `DATA`: the rows the reader reads first.
const HEADER_ROWS: usize = 5;

/// More than any five-row header needs, and far less than one tile.
const HEADER_BYTES: u64 = 512;

/// The largest tile side this import reads. Krita writes 64 × 64 tiles, and the reader allocates
/// `TILEWIDTH × TILEHEIGHT × PIXELSIZE` bytes for every tile it decodes, so a header claiming a
/// million-pixel side is an allocation the size of the file many times over. 1024 is a
/// power-of-two side no Krita writer emits today, and the largest one whose tile still fits
/// comfortably: 1024 × 1024 × 4 bytes is 4 MiB.
const MAX_TILE_SIDE: u64 = 1024;

/// A bound on `maindoc.xml`, which grows with the layer count and not with the canvas.
const MAX_MAINDOC_BYTES: u64 = 32 << 20;

/// Bytes per pixel of an image entry, the widest the reader decodes (`mergedimage.png` and
/// `preview.png` are RGBA, and a `.defaultpixel` is one such pixel).
const IMAGE_PIXELS: u64 = 4;

/// What one `mergedimage.png` or `preview.png` may declare uncompressed beyond its own pixels: a
/// PNG filter byte per row plus room for its chunks. The reader reads the whole entry into memory
/// and decodes it, so the bound is on the declared size, not on the bytes the file spends.
const MAX_IMAGE_SLACK: u64 = 1 << 16;

/// The most a `.defaultpixel` entry may hold: the one pixel the reader copies out of it, four
/// bytes in Krita's BGRA, with room for a writer that stores a wider sample.
const MAX_DEFAULT_PIXEL_BYTES: u64 = 16;

/// The longest tile header line (`x,y,LZF,size`, the size as many as twenty digits): a longer line
/// is not a tile table, and reading it whole would be the allocation this check prevents.
const MAX_TILE_LINE_BYTES: u64 = 64;

/// The fewest bytes one row of a tile table can take: `x,y,s,n` with every part one character, plus
/// its newline. No row is shorter, so an entry's own size caps how many rows it can list; the
/// canvas caps nothing, because Krita stores tiles outside it.
const MIN_TILE_ROW_BYTES: u64 = 8;

/// The most a zip-deflated stream can expand to against its own compressed size: DEFLATE's ceiling
/// is 1032:1. A tile entry whose central directory claims more than this is a stream no deflater
/// wrote, and the reader decompresses such an entry whole.
const ZIP_DEFLATE_MOST: u64 = 1032;

/// The exact `compositeop` names the reader maps to a known mode, aliases included. Anything else
/// reaches the adapter as `normal`, which would hide `erase`, `behind` and future Krita modes.
const BLEND_NAMES: [&str; 28] = [
    "normal",
    "darken",
    "multiply",
    "burn",
    "colorburn",
    "linearburn",
    "subtract",
    "darkercolor",
    "lighten",
    "screen",
    "dodge",
    "colordodge",
    "add",
    "lightercolor",
    "overlay",
    "softlight",
    "hardlight",
    "vividlight",
    "linearlight",
    "pinlight",
    "hardmix",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
    "divide",
];

/// A private copy of the document, so the preflight and the reader consume the same bytes.
///
/// `KraDocument::open` takes a path and reopens it; reading the original twice would let a file
/// swapped in between the passes describe different documents to each. The copy is removed when the
/// snapshot is dropped, and the original is only ever read. `tempfile` makes the directory private
/// (0700 where the platform has modes) and exclusive, so another process cannot plant an entry in
/// it between the copy and either pass.
///
/// The copy is bounded by [`MAX_DOCUMENT_BYTES`]: an oversized document is refused before it lands
/// in the temporary directory, because the reader would then allocate from it.
pub(crate) struct Snapshot {
    /// Kept alive for the snapshot's lifetime: dropping it removes the directory.
    _directory: tempfile::TempDir,
    path: PathBuf,
    original: PathBuf,
}

impl Snapshot {
    pub(crate) fn of(path: &Path) -> Result<Self, ImportError> {
        // A FIFO, socket or device would block or never end inside a copy; only a regular file is a
        // document to import.
        let metadata =
            fs::metadata(path).map_err(|error| unreadable(path, format!("opening the document: {error}")))?;
        if !metadata.is_file() {
            return Err(unreadable(path, "the document is not a regular file"));
        }
        if metadata.len() > MAX_DOCUMENT_BYTES {
            return Err(oversized(path, metadata.len()));
        }
        let directory = tempfile::Builder::new()
            .prefix("sumi-clip-kra-")
            .tempdir()
            .map_err(|error| unreadable(path, format!("preparing a private copy: {error}")))?;
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("document.kra");
        let copy = directory.path().join(name);
        copy_document(path, &copy)?;
        Ok(Snapshot { _directory: directory, path: copy, original: path.to_owned() })
    }

    /// The copy both passes read.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The file the person chose, for messages.
    pub(crate) fn original(&self) -> &Path {
        &self.original
    }
}

/// Copies `path` to `copy`, refusing to write more than [`MAX_DOCUMENT_BYTES`].
///
/// The metadata check in [`Snapshot::of`] bounds a file that stays the size it was; `take` bounds
/// one that grows while it is copied, so neither check on its own lets a hostile document fill the
/// disk the copy is written to.
fn copy_document(path: &Path, copy: &Path) -> Result<(), ImportError> {
    let source = File::open(path).map_err(|error| unreadable(path, format!("opening the document: {error}")))?;
    let mut target =
        File::create(copy).map_err(|error| unreadable(path, format!("preparing a private copy: {error}")))?;
    let written = std::io::copy(&mut source.take(MAX_DOCUMENT_BYTES.saturating_add(1)), &mut target)
        .map_err(|error| unreadable(path, format!("copying the document: {error}")))?;
    if written > MAX_DOCUMENT_BYTES {
        return Err(oversized(path, written));
    }
    Ok(())
}

/// The refusal of a document larger than this import will copy or read.
fn oversized(path: &Path, bytes: u64) -> ImportError {
    unreadable(
        path,
        format!(
            "the document is {bytes} bytes or more, larger than the {MAX_DOCUMENT_BYTES} bytes a \
             document may be here"
        ),
    )
}

/// What `maindoc.xml` says the document holds, in the reader's own bottom-up order.
pub(crate) struct Expected {
    root: Vec<ExpectedNode>,
    /// What the reader cannot keep, reported rather than dropped in silence.
    pub(crate) warnings: Vec<ImportWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedNode {
    Layer { identity: Identity, mask: bool },
    Group { identity: Identity, children: Vec<ExpectedNode> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    name: String,
    filename: String,
}

impl Identity {
    /// The layer as a person knows it, with the filename that identifies it in the archive.
    fn describe(&self) -> String {
        format!("{:?} ({})", self.name, self.filename)
    }
}

/// The kind of node in the stack: what the engine maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Layer,
    Group,
}

/// What the archive declares a node to be, including the reader's own paint/shape/clone split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeenKind {
    Paint,
    Shape,
    Clone,
    Group,
}

/// One `<layer>` as clone resolution sees it: identity plus what its `clonefromuuid` points at.
struct SeenNode {
    identity: Identity,
    kind: SeenKind,
    clone_from: Option<String>,
}

/// Everything the walk has claimed so far, for duplicate and clone checks after the tree is read.
#[derive(Default)]
struct Collected {
    filenames: HashSet<String>,
    nodes: HashMap<String, SeenNode>,
    /// UUIDs in document order, so a broken clone is always reported for the same layer.
    order: Vec<String>,
}

/// Reads `maindoc.xml` and each layer's tile header, refusing what the reader would lose.
pub(crate) fn inspect(snapshot: &Snapshot) -> Result<Expected, ImportError> {
    let original = snapshot.original();
    let file =
        File::open(snapshot.path()).map_err(|error| unreadable(original, format!("opening the archive: {error}")))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| unreadable(original, format!("the archive is not a readable ZIP: {error}")))?;
    let xml = read_maindoc(&mut archive, original)?;
    let parsed =
        Document::parse_with_options(&xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
            .map_err(|error| unreadable(original, format!("maindoc.xml is not readable XML: {error}")))?;
    let Some(image) = parsed.descendants().find(|node| node.has_tag_name("IMAGE")) else {
        return Err(unreadable(original, "maindoc.xml has no IMAGE element"));
    };
    check_image_colorspace(&image)?;
    let (width, height) = check_canvas_size(&image, original)?;
    let base = image.attribute("name").unwrap_or("Krita").to_owned();
    let mut warnings = Vec::new();
    check_profile(&image, &mut warnings);
    // The reader decodes the merged preview whatever the import does with it, so its size is
    // bounded before the reader runs.
    check_image_entries(&mut archive, original, width, height)?;
    let mut walk = Walk {
        archive: &mut archive,
        path: original,
        base,
        collected: Collected::default(),
        warnings,
        cap: layer_cap_for(width, height),
        painted: 0,
    };
    let root = match image.children().find(|node| node.has_tag_name("layers")) {
        Some(layers) => walk.layers(layers)?,
        None => Vec::new(),
    };
    walk.check_clones()?;
    Ok(Expected { root, warnings: walk.warnings })
}

/// The five leading rows of a tiled stream: enough to know whether the reader can decode it.
struct Header {
    pixels: usize,
    tile_width: u64,
    tile_height: u64,
    /// The number of tiles the stream lists.
    data: u64,
}

impl Header {
    /// Reads no tile: a payload that decodes to fewer bytes than a tile only shows up when the
    /// reader decodes it, and that is what [`reconcile`] catches. The table itself is walked
    /// separately, by [`check_tile_table`].
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut rows = bytes.split(|byte| *byte == b'\n');
        let mut head: Vec<&[u8]> = Vec::with_capacity(HEADER_ROWS);
        for _ in 0..HEADER_ROWS {
            let Some(row) = rows.next() else {
                return Err("ends before its DATA row".to_owned());
            };
            head.push(row);
        }
        let [version, tile_width, tile_height, pixel_size, data] =
            <[&[u8]; HEADER_ROWS]>::try_from(head).map_err(|_| "has a malformed header".to_owned())?;
        if !text(version)?.starts_with("VERSION ") {
            return Err("is not a Krita tiled layer (no VERSION row)".to_owned());
        }
        let tile_width = number(text(tile_width)?, "TILEWIDTH")? as u64;
        let tile_height = number(text(tile_height)?, "TILEHEIGHT")? as u64;
        let pixels = number(text(pixel_size)?, "PIXELSIZE")?;
        let data = number(text(data)?, "DATA")? as u64;
        // The reader allocates `TILEWIDTH × TILEHEIGHT × PIXELSIZE` bytes per tile it decodes
        // (and `lzf_decompress` reserves that much up front), so both sides are bounded before
        // any tile is touched.
        for (name, side) in [("TILEWIDTH", tile_width), ("TILEHEIGHT", tile_height)] {
            if !(1..=MAX_TILE_SIDE).contains(&side) {
                return Err(format!(
                    "declares {name} {side}; this import reads tiles from 1 to {MAX_TILE_SIDE} pixels a side"
                ));
            }
        }
        Ok(Header { pixels, tile_width, tile_height, data })
    }

    /// The bytes one tile of this stream decodes to: the reader's `TILEWIDTH × TILEHEIGHT ×
    /// PIXELSIZE`, both sides already bounded to [`MAX_TILE_SIDE`].
    fn tile_bytes(&self) -> u64 {
        let pixels = u64::try_from(self.pixels).unwrap_or(u64::MAX);
        self.tile_width.saturating_mul(self.tile_height).saturating_mul(pixels)
    }
}

/// A `maindoc.xml` row as text.
fn text(row: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(row).map_err(|_| "has a non-UTF-8 header row".to_owned())
}

/// The number after `key` in a tile header row.
fn number(row: &str, key: &str) -> Result<usize, String> {
    let mut parts = row.split_whitespace();
    if parts.next() != Some(key) {
        return Err(format!("expected {key}, got {row:?}"));
    }
    let Some(value) = parts.next() else {
        return Err(format!("{key} has no value"));
    };
    value.parse().map_err(|_| format!("{key} is not a number: {value:?}"))
}

/// The walk over `<layers>`: every node is checked against what the reader keeps.
struct Walk<'a> {
    archive: &'a mut zip::ZipArchive<File>,
    path: &'a Path,
    base: String,
    collected: Collected,
    warnings: Vec<ImportWarning>,
    /// The canvas's layer cap: the reader builds one canvas-sized image per layer and mask, so the
    /// declared count is checked here, before `KraDocument::open` allocates any of them.
    cap: usize,
    /// Layers and masks so far: what [`Walk::room`] counts against `cap`.
    painted: usize,
}

impl Walk<'_> {
    /// Whether one more canvas-sized image fits the canvas's layer cap, counted as the import
    /// counts its pixel buffers. A document declaring more layers than this is refused while only
    /// its `maindoc.xml` has been read.
    fn room(&mut self) -> Result<(), ImportError> {
        let count = self.painted.saturating_add(1);
        if count > self.cap {
            return Err(ImportError::TooManyLayers { count, most: self.cap });
        }
        self.painted = count;
        Ok(())
    }

    /// One `<layers>` block, bottom-up as the reader reports it.
    fn layers(&mut self, layers: Node<'_, '_>) -> Result<Vec<ExpectedNode>, ImportError> {
        let mut nodes = Vec::new();
        for child in layers.children().filter(|node| node.has_tag_name("layer")) {
            nodes.push(self.layer(child)?);
        }
        nodes.reverse();
        Ok(nodes)
    }

    /// One `<layer>`: its identity, what it needs in the archive, and its children.
    fn layer(&mut self, node: Node<'_, '_>) -> Result<ExpectedNode, ImportError> {
        let path = self.path;
        check_colorspace(&node, &format!("the layer {}", describe(&node)))?;
        check_profile(&node, &mut self.warnings);
        let Some(nodetype) = node.attribute("nodetype") else {
            return Err(ImportError::Unsupported(format!(
                "the layer {} declares no node type; the reader would guess `paintlayer` for it",
                describe(&node)
            )));
        };
        if !matches!(nodetype, "paintlayer" | "grouplayer" | "shapelayer" | "clonelayer") {
            return Err(ImportError::Unsupported(format!(
                "the layer {} has the node type {nodetype:?}, which this import does not read",
                describe(&node)
            )));
        }
        let Some(filename) = node.attribute("filename").filter(|name| !name.trim().is_empty()) else {
            return Err(unreadable(
                path,
                format!("the layer {} has no filename, so the reader would skip it", describe(&node)),
            ));
        };
        let identity =
            Identity { name: node.attribute("name").unwrap_or(filename).to_owned(), filename: filename.to_owned() };
        self.claim_filename(filename, &identity)?;
        let Some(uuid) = node.attribute("uuid").filter(|uuid| !uuid.trim().is_empty()) else {
            return Err(unreadable(
                path,
                format!("the layer {} has no UUID, so clones could not be followed", identity.describe()),
            ));
        };
        if let Some(blend) = node.attribute("compositeop") {
            if !BLEND_NAMES.contains(&blend) {
                return Err(ImportError::Unsupported(format!(
                    "the blend mode {blend:?} of the layer {} is not one the reader keeps; it would \
                     become Normal",
                    identity.describe()
                )));
            }
        }
        // Krita's inherit-alpha is a clip against what lies below; the reader never reads the flag,
        // so importing the layer would blend it with everything.
        if let Some(inherit) = node.attributes().find(|attribute| attribute.name().eq_ignore_ascii_case("inheritalpha"))
        {
            let value = inherit.value();
            if value == "1" || value.eq_ignore_ascii_case("true") {
                return Err(ImportError::Unsupported(format!(
                    "the layer {} inherits alpha from the layer below (clipping), which this import \
                     does not keep",
                    identity.describe()
                )));
            }
        }
        // Krita's `locked` and `collapsed` attributes are not exposed by the reader at all: they
        // are read here so they are said rather than dropped. Both are states this app has a field
        // for (`Layer::locked`, `Layer::collapsed`), so mapping them would be the better fix, but
        // that needs the reader (or a side channel beside it) to carry them; until then the honest
        // import is the one that names the loss.
        for (attribute, what) in [("locked", "lock"), ("collapsed", "collapsed state")] {
            if node.attribute(attribute).is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true")) {
                self.warnings.push(ImportWarning::StateNotRead { layer: identity.name.clone(), what });
            }
        }
        match nodetype {
            "grouplayer" => self.group(node, identity, uuid),
            "paintlayer" => {
                self.room()?;
                let mask = self.masks(node, &identity)?;
                self.layer_data(&identity)?;
                self.remember(uuid, &identity, SeenKind::Paint, None)?;
                Ok(ExpectedNode::Layer { identity, mask })
            }
            "shapelayer" => {
                self.room()?;
                let mask = self.masks(node, &identity)?;
                self.shape_data(&identity)?;
                self.remember(uuid, &identity, SeenKind::Shape, None)?;
                Ok(ExpectedNode::Layer { identity, mask })
            }
            "clonelayer" => {
                self.room()?;
                let mask = self.masks(node, &identity)?;
                let Some(target) = node.attribute("clonefromuuid").filter(|target| !target.is_empty()) else {
                    return Err(unreadable(
                        path,
                        format!(
                            "the clone layer {} names no source UUID, so the reader would import it \
                             transparent",
                            identity.describe()
                        ),
                    ));
                };
                self.remember(uuid, &identity, SeenKind::Clone, Some(target.to_owned()))?;
                Ok(ExpectedNode::Layer { identity, mask })
            }
            other => Err(ImportError::Unsupported(format!(
                "the layer {} has the node type {other:?}, which this import does not read",
                identity.describe()
            ))),
        }
    }

    /// A group: isolation is decided here, because `KraFolder` does not expose it.
    fn group(&mut self, node: Node<'_, '_>, identity: Identity, uuid: &str) -> Result<ExpectedNode, ImportError> {
        let path = self.path;
        self.reject_group_masks(node, &identity)?;
        let blend = node.attribute("compositeop").unwrap_or("normal");
        let passthrough = node.attribute("passthrough");
        let opacity = node.attribute("opacity").and_then(|value| value.parse::<f64>().ok()).unwrap_or(255.0) / 255.0;
        let mut nested = node.children().filter(|child| child.has_tag_name("layers"));
        let block = nested.next();
        if nested.next().is_some() {
            return Err(unreadable(
                path,
                format!("the group {} has more than one <layers> block", identity.describe()),
            ));
        }
        let shows_isolation = block.is_some_and(|layers| {
            layers
                .children()
                .filter(|child| child.has_tag_name("layer"))
                .any(|child| child.attribute("compositeop").unwrap_or("normal") != "normal")
        });
        let children = match block {
            Some(layers) => self.layers(layers)?,
            None => Vec::new(),
        };
        match passthrough {
            Some("1") => {
                // A pass-through group has no blend or opacity of its own. The reader would still
                // hand both to the engine, so only the neutral values are provable.
                if blend != "normal" {
                    return Err(ImportError::Unsupported(format!(
                        "the pass-through group {} also declares the blend mode {blend:?}, which the \
                         reader would apply to the group as a whole",
                        identity.describe()
                    )));
                }
                if opacity < 1.0 {
                    return Err(ImportError::Unsupported(format!(
                        "the pass-through group {} is at opacity {opacity:.2}; the engine can only pass \
                         through at full opacity",
                        identity.describe()
                    )));
                }
            }
            // `0` is an isolated group, and so is a missing attribute: Krita only writes the
            // attribute for a group that opted in, and a group is isolated unless it did.
            None | Some("0") => {
                if blend == "normal" && shows_isolation {
                    self.warnings.push(ImportWarning::GroupIsolationLost { layer: identity.name.clone() });
                }
            }
            Some(other) => {
                return Err(ImportError::Unsupported(format!(
                    "the group {} declares passthrough={other:?}, which this import does not know",
                    identity.describe()
                )));
            }
        }
        self.remember(uuid, &identity, SeenKind::Group, None)?;
        Ok(ExpectedNode::Group { identity, children })
    }

    /// Masks of a paint, shape or clone layer: only ordinary visible transparency masks stay.
    ///
    /// Returns whether the reader must keep a transparency mask for this layer.
    fn masks(&mut self, layer: Node<'_, '_>, identity: &Identity) -> Result<bool, ImportError> {
        let mut transparency = 0_usize;
        for masks in layer.children().filter(|child| child.has_tag_name("masks")) {
            for mask in masks.children().filter(|child| child.has_tag_name("mask")) {
                // A mask the file itself switched off (`visible="0"`) attenuates nothing, and the
                // reader skips it exactly like a missing mask. A transparency mask is the only kind
                // this import could have kept, so its absence is said; a transform or filter mask
                // is never kept in either state and changes no pixel when hidden.
                if mask.attribute("visible") == Some("0") {
                    if matches!(mask.attribute("nodetype"), Some("transparencymask") | Some("transparency")) {
                        self.warnings.push(ImportWarning::MaskHiddenNotRead { layer: identity.name.clone() });
                    }
                    continue;
                }
                let nodetype = mask.attribute("nodetype").unwrap_or("unknown");
                match nodetype {
                    "transparencymask" | "transparency" => {
                        transparency += 1;
                        if transparency > 1 {
                            return Err(ImportError::Unsupported(format!(
                                "the layer {} has more than one visible transparency mask; the reader \
                                 keeps only the first",
                                identity.describe()
                            )));
                        }
                        self.mask_data(&mask, identity)?;
                        self.room()?;
                    }
                    "transformmask" => {
                        return Err(ImportError::Unsupported(format!(
                            "the layer {} has a visible transform mask, which this import does not keep",
                            identity.describe()
                        )));
                    }
                    "filtermask" => {
                        let filter = mask.attribute("filtername").unwrap_or("unknown");
                        return Err(ImportError::Unsupported(format!(
                            "the layer {} has a visible filter mask ({filter}), which this import does \
                             not keep",
                            identity.describe()
                        )));
                    }
                    other => {
                        return Err(ImportError::Unsupported(format!(
                            "the layer {} has a visible mask of type {other:?}, which this import does \
                             not read",
                            identity.describe()
                        )));
                    }
                }
            }
        }
        Ok(transparency > 0)
    }

    /// A group's masks: the reader applies none of them, so any visible one is a loss.
    fn reject_group_masks(&self, node: Node<'_, '_>, identity: &Identity) -> Result<(), ImportError> {
        for masks in node.children().filter(|child| child.has_tag_name("masks")) {
            for mask in masks.children().filter(|child| child.has_tag_name("mask")) {
                if mask.attribute("visible") == Some("0") {
                    continue;
                }
                return Err(ImportError::Unsupported(format!(
                    "the group {} has a visible {} mask; the reader never applies a mask to a group",
                    identity.describe(),
                    mask.attribute("nodetype").unwrap_or("unknown")
                )));
            }
        }
        Ok(())
    }

    /// A paint layer's tiled data: it must exist and store four bytes per pixel.
    fn layer_data(&mut self, identity: &Identity) -> Result<(), ImportError> {
        let entry = format!("{}/layers/{}", self.base, identity.filename);
        let header = self.tile_header(&entry)?;
        check_default_pixel(self.archive, self.path, &entry)?;
        if header.pixels != PAINT_PIXELS {
            return Err(ImportError::Unsupported(format!(
                "the paint layer {} stores {} bytes per pixel; only {PAINT_PIXELS}-byte RGBA tiles are \
                 read",
                identity.describe(),
                header.pixels
            )));
        }
        Ok(())
    }

    /// A shape layer's SVG: the reader skips the layer when the entry is absent.
    fn shape_data(&mut self, identity: &Identity) -> Result<(), ImportError> {
        let path = self.path;
        let entry = format!("{}/layers/{}.shapelayer/content.svg", self.base, identity.filename);
        self.archive.by_name(&entry).map_err(|error| {
            unreadable(
                path,
                format!("the shape layer {} has no vector data at {entry:?}: {error}", identity.describe()),
            )
        })?;
        Ok(())
    }

    /// A transparency mask's tiled data, looked up the way the reader looks it up.
    fn mask_data(&mut self, mask: &Node<'_, '_>, identity: &Identity) -> Result<(), ImportError> {
        let path = self.path;
        let Some(filename) = mask.attribute("filename").filter(|name| !name.trim().is_empty()) else {
            return Err(unreadable(
                path,
                format!(
                    "a transparency mask of the layer {} has no filename, so the reader would import \
                     the layer unmasked",
                    identity.describe()
                ),
            ));
        };
        self.claim_filename(
            filename,
            &Identity { name: format!("mask of {}", identity.describe()), filename: filename.to_owned() },
        )?;
        // The reader decodes a mask at the origin, whatever `x`/`y` say: a displaced mask would be
        // applied where it is not.
        let coordinate = |name: &str| mask.attribute(name).and_then(|value| value.parse::<i64>().ok()).unwrap_or(0);
        let (x, y) = (coordinate("x"), coordinate("y"));
        if x != 0 || y != 0 {
            return Err(ImportError::Unsupported(format!(
                "the transparency mask {filename:?} of the layer {} is placed at {x},{y}; the reader \
                 applies masks at the origin",
                identity.describe()
            )));
        }
        let base = format!("{}/layers/{filename}", self.base);
        let candidates = [format!("{base}.pixelselection"), format!("{base}.pixel_selection"), base];
        let mut entry = None;
        for candidate in candidates {
            if self.archive.by_name(&candidate).is_ok() {
                entry = Some(candidate);
                break;
            }
        }
        let Some(entry) = entry else {
            return Err(unreadable(
                path,
                format!(
                    "the transparency mask {filename:?} of the layer {} has no tiled data, so the reader \
                     would import the layer unmasked",
                    identity.describe()
                ),
            ));
        };
        let header = self.tile_header(&entry)?;
        check_default_pixel(self.archive, self.path, &entry)?;
        if header.pixels != MASK_PIXELS {
            return Err(ImportError::Unsupported(format!(
                "the transparency mask {filename:?} of the layer {} stores {} bytes per pixel; only \
                 {MASK_PIXELS}-byte coverage is read",
                identity.describe(),
                header.pixels
            )));
        }
        Ok(())
    }

    /// The header and the tile table of `entry`, bounded before either is trusted.
    fn tile_header(&mut self, entry: &str) -> Result<Header, ImportError> {
        tile_header(self.archive, self.path, entry)
    }

    /// The first claim of a filename; a second one would map two elements to one archive entry.
    fn claim_filename(&mut self, filename: &str, identity: &Identity) -> Result<(), ImportError> {
        if !self.collected.filenames.insert(filename.to_owned()) {
            return Err(unreadable(
                self.path,
                format!(
                    "maindoc.xml gives the filename {filename:?} to more than one element; the second \
                     is {}",
                    identity.describe()
                ),
            ));
        }
        Ok(())
    }

    /// Records a layer by UUID, refusing a duplicate, which would make clone targets ambiguous.
    fn remember(
        &mut self,
        uuid: &str,
        identity: &Identity,
        kind: SeenKind,
        clone_from: Option<String>,
    ) -> Result<(), ImportError> {
        if self.collected.nodes.contains_key(uuid) {
            return Err(unreadable(
                self.path,
                format!("two layers share the UUID {uuid:?}; the second is {}", identity.describe()),
            ));
        }
        self.collected.order.push(uuid.to_owned());
        self.collected.nodes.insert(uuid.to_owned(), SeenNode { identity: identity.clone(), kind, clone_from });
        Ok(())
    }

    /// Every clone must resolve, in one step, to a layer that has pixels: the reader never follows
    /// a chain and leaves a clone of a clone transparent.
    fn check_clones(&self) -> Result<(), ImportError> {
        for uuid in &self.collected.order {
            let Some(node) = self.collected.nodes.get(uuid) else { continue };
            let Some(target) = node.clone_from.as_deref() else { continue };
            let current = target.to_owned();
            // The loop this replaced could only resolve one step: a clone of a clone is refused
            // below, so the only cycle reachable here is a clone that names itself.
            if current == uuid.as_str() {
                return Err(ImportError::Unsupported(format!(
                    "the clone layer {} is part of a cycle of clone references",
                    node.identity.describe()
                )));
            }
            let Some(source) = self.collected.nodes.get(&current) else {
                return Err(unreadable(
                    self.path,
                    format!(
                        "the clone layer {} follows the UUID {current:?}, which maindoc.xml does \
                         not hold",
                        node.identity.describe()
                    ),
                ));
            };
            match source.kind {
                SeenKind::Clone => {
                    return Err(ImportError::Unsupported(format!(
                        "the clone layer {} follows another clone, {}, which the reader never \
                         resolves",
                        node.identity.describe(),
                        source.identity.describe()
                    )));
                }
                SeenKind::Group => {
                    return Err(unreadable(
                        self.path,
                        format!(
                            "the clone layer {} follows the group {}, which has no pixels",
                            node.identity.describe(),
                            source.identity.describe()
                        ),
                    ));
                }
                SeenKind::Paint | SeenKind::Shape => {}
            }
        }
        Ok(())
    }
}

/// The facts of one tiled entry, refused when the entry claims more than its own bytes allow.
///
/// Only the header and the tile table are read: the reader's decoder allocates a tile per row it
/// decodes, and every number here decides how much it would allocate.
fn tile_header(archive: &mut zip::ZipArchive<File>, path: &Path, entry: &str) -> Result<Header, ImportError> {
    let (declared, compressed, header) = {
        let mut file = archive
            .by_name(entry)
            .map_err(|error| unreadable(path, format!("the tiled data {entry:?} is not in the archive: {error}")))?;
        // The central directory's sizes: known before a byte of it is decompressed.
        let (declared, compressed) = (file.size(), file.compressed_size());
        let mut head = Vec::new();
        (&mut file)
            .take(HEADER_BYTES)
            .read_to_end(&mut head)
            .map_err(|error| unreadable(path, format!("reading the header of {entry:?}: {error}")))?;
        let header = Header::parse(&head).map_err(|why| unreadable(path, format!("the tiled data {entry:?} {why}")))?;
        (declared, compressed, header)
    };
    // The reader decompresses this entry whole, so its declared size is bounded by the smaller of
    // two ceilings: what a DEFLATE stream of `compressed` bytes can expand to (1032:1), and the
    // snapshot limit `MAX_DOCUMENT_BYTES`. A stored entry, whose two sizes are equal, and every
    // stream a real deflater wrote stay under both.
    let most = compressed.saturating_mul(ZIP_DEFLATE_MOST).min(MAX_DOCUMENT_BYTES);
    if declared > most {
        return Err(unreadable(
            path,
            format!(
                "the tiled data {entry:?} declares {declared} bytes uncompressed, more than the {most} \
                 its {compressed} compressed bytes can expand to"
            ),
        ));
    }
    // Krita's canvas has no edge: the writer stores tiles wherever paint landed, including outside
    // the canvas, and the reader clips those tiles (`x < 0 || x >= width`). The canvas grid is
    // therefore no bound on this table. The entry's own size is: every row costs at least
    // `MIN_TILE_ROW_BYTES`, so `declared / MIN_TILE_ROW_BYTES` rows at most. The five header rows
    // more than cover the one byte a last row without its newline saves.
    let rows_most = declared / MIN_TILE_ROW_BYTES;
    if header.data > rows_most {
        return Err(unreadable(
            path,
            format!(
                "the tiled data {entry:?} lists {} tiles, more than the {rows_most} rows an entry of \
                 {declared} bytes holds",
                header.data
            ),
        ));
    }
    check_tile_table(archive, path, entry, &header)?;
    Ok(header)
}

/// Walks the tile table of `entry`, refusing a payload that cannot fit the entry.
///
/// The reader's own guard is `pos + size > data.len()`, which wraps when a hostile `size` is close
/// to `usize::MAX`: the range it then slices runs backwards and panics on the entry. Reading the
/// table here costs one more pass over the entry — the reader decompresses that entry anyway — and
/// every payload is skipped through a fixed buffer, so this allocates nothing the header has not
/// already bounded.
fn check_tile_table(
    archive: &mut zip::ZipArchive<File>,
    path: &Path,
    entry: &str,
    header: &Header,
) -> Result<(), ImportError> {
    let file = archive
        .by_name(entry)
        .map_err(|error| unreadable(path, format!("the tiled data {entry:?} is not in the archive: {error}")))?;
    let mut reader = BufReader::new(file);
    // The header rows parsed above, skipped again to reach the table.
    for _ in 0..HEADER_ROWS {
        tile_line(&mut reader, HEADER_BYTES).map_err(|reason| tile_refusal(path, entry, reason))?;
    }
    let tile_bytes = header.tile_bytes();
    let payload_most = tile_payload_most(tile_bytes);
    for _ in 0..header.data {
        let line = tile_line(&mut reader, MAX_TILE_LINE_BYTES).map_err(|reason| tile_refusal(path, entry, reason))?;
        let (x, y, size) = tile_entry(&line).map_err(|reason| tile_refusal(path, entry, reason))?;
        if size > payload_most {
            return Err(unreadable(
                path,
                format!(
                    "the tiled data {entry:?} declares a {size}-byte tile at {x},{y}; a {tile_bytes}-byte \
                     tile and its storage flag take at most {payload_most}"
                ),
            ));
        }
        let mut sink = std::io::sink();
        let skipped = std::io::copy(&mut (&mut reader).take(size), &mut sink)
            .map_err(|error| unreadable(path, format!("reading the tile at {x},{y} of {entry:?}: {error}")))?;
        if skipped != size {
            return Err(unreadable(path, format!("the tiled data {entry:?} ends inside the tile at {x},{y}")));
        }
    }
    Ok(())
}

/// A refusal about one tiled entry, with the reason the table or a payload gave.
fn tile_refusal(path: &Path, entry: &str, reason: impl std::fmt::Display) -> ImportError {
    unreadable(path, format!("the tiled data {entry:?} {reason}"))
}

/// One row of a tile stream, refused when it is longer than `most` bytes: a hostile row must not be
/// read into memory without limit. A last row without its newline is accepted, as the reader
/// accepts it.
fn tile_line(reader: &mut impl BufRead, most: u64) -> Result<String, String> {
    let mut line = Vec::new();
    // One byte more than the row and one for its newline: a row over the bound is refused below.
    (&mut *reader).take(most.saturating_add(2)).read_until(b'\n', &mut line).map_err(|error| error.to_string())?;
    if line.is_empty() {
        return Err("ends before its tile table".to_owned());
    }
    let row = line.as_slice().strip_suffix(b"\n".as_slice()).unwrap_or(line.as_slice());
    if u64::try_from(row.len()).unwrap_or(u64::MAX) > most {
        return Err(format!("has a row longer than the {most} bytes a tile row may take"));
    }
    match std::str::from_utf8(row) {
        Ok(row) => Ok(row.to_owned()),
        Err(_) => Err("has a non-UTF-8 tile row".to_owned()),
    }
}

/// The `x,y,LZF,size` parts of one tile row: where its tile sits and the bytes it stores.
fn tile_entry(line: &str) -> Result<(i64, i64, u64), String> {
    let malformed = || format!("has the malformed tile row {line:?}");
    let mut fields = line.split(',');
    let (Some(x), Some(y), Some(_storage), Some(size), None) =
        (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(malformed());
    };
    let x = x.trim().parse::<i64>().map_err(|_| malformed())?;
    let y = y.trim().parse::<i64>().map_err(|_| malformed())?;
    let size = size.trim().parse::<u64>().map_err(|_| malformed())?;
    Ok((x, y, size))
}

/// The most one tile's payload may take: the tile it decodes to, its storage flag, and the small
/// expansion an LZF writer can add. Krita stores an incompressible tile raw, so a larger claim is a
/// size the reader would slice past the entry's end.
fn tile_payload_most(tile_bytes: u64) -> u64 {
    tile_bytes.saturating_add(tile_bytes / 8).saturating_add(1)
}

/// The most a canvas-sized image entry may declare uncompressed: four bytes a pixel, the PNG filter
/// byte a row, and its own chunks.
fn image_bound(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(IMAGE_PIXELS.saturating_add(1))
        .saturating_add(MAX_IMAGE_SLACK)
}

/// `mergedimage.png` and `preview.png`: the reader reads the first whole, whatever the import does
/// with it, and decodes the PNG it holds into images of its own declared size. The uncompressed
/// size the central directory declares is refused before that allocation.
fn check_image_entries(
    archive: &mut zip::ZipArchive<File>,
    path: &Path,
    width: u32,
    height: u32,
) -> Result<(), ImportError> {
    let most = image_bound(width, height);
    for entry in ["mergedimage.png", "preview.png"] {
        // An entry the archive cannot even open is one the reader skips as absent (`.ok()`), so it
        // allocates nothing from it; only an entry it can open is worth sizing.
        let Ok(file) = archive.by_name(entry) else { continue };
        let declared = file.size();
        if declared > most {
            return Err(unreadable(
                path,
                format!(
                    "the image {entry:?} declares {declared} bytes uncompressed, more than the {most} a \
                     {width} by {height} canvas and its chunks hold"
                ),
            ));
        }
    }
    Ok(())
}

/// The `.defaultpixel` beside a tiled entry: the reader reads it whole into memory for one pixel.
fn check_default_pixel(archive: &mut zip::ZipArchive<File>, path: &Path, entry: &str) -> Result<(), ImportError> {
    let name = format!("{entry}.defaultpixel");
    // The reader reads this with `.ok()`, so an entry it cannot open allocates nothing.
    let Ok(file) = archive.by_name(&name) else { return Ok(()) };
    let declared = file.size();
    if declared > MAX_DEFAULT_PIXEL_BYTES {
        return Err(unreadable(
            path,
            format!(
                "the default pixel of {entry:?} declares {declared} bytes uncompressed, more than the \
                 {MAX_DEFAULT_PIXEL_BYTES} the one pixel it holds may take"
            ),
        ));
    }
    Ok(())
}

/// Reconciles the declared tree with the one the reader built, by identity rather than by count.
///
/// A layer the reader dropped (its tiles failed to decode, its mask fell through) is an error here,
/// not a silently shorter stack. Parents are compared too, so a layer the reader reparented is
/// refused as well.
pub(crate) fn reconcile(snapshot: &Snapshot, expected: &Expected, document: &KraDocument) -> Result<(), ImportError> {
    let path = snapshot.original();
    let want = expected.flatten();
    let got = flatten_reader(document)?;
    let mut by_filename: HashMap<&str, usize> = HashMap::new();
    for (index, node) in got.iter().enumerate() {
        if by_filename.insert(node.filename, index).is_some() {
            return Err(unreadable(path, format!("the reader returned the layer {:?} twice", node.filename)));
        }
    }
    for node in &want {
        let Some(index) = by_filename.get(node.filename).copied() else {
            return Err(unreadable(
                path,
                format!(
                    "the reader dropped the {} {}, which maindoc.xml lists{}",
                    kind_name(node.kind),
                    identity(node.name, node.filename),
                    under(node.parent)
                ),
            ));
        };
        let Some(found) = got.get(index) else { continue };
        if found.kind != node.kind {
            return Err(unreadable(
                path,
                format!(
                    "the reader turned the {} {} into a {}",
                    kind_name(node.kind),
                    identity(node.name, node.filename),
                    kind_name(found.kind)
                ),
            ));
        }
        if found.parent != node.parent {
            return Err(unreadable(
                path,
                format!(
                    "the reader placed the {} {} under {}, not under {}",
                    kind_name(node.kind),
                    identity(node.name, node.filename),
                    found.parent.unwrap_or("the root"),
                    node.parent.unwrap_or("the root")
                ),
            ));
        }
    }
    if want.len() != got.len() {
        let extra = got.iter().find(|node| !want.iter().any(|want| want.filename == node.filename));
        if let Some(extra) = extra {
            return Err(unreadable(
                path,
                format!(
                    "the reader returned the {} {}, which maindoc.xml does not list",
                    kind_name(extra.kind),
                    identity(extra.name, extra.filename)
                ),
            ));
        }
    }
    check_masks(path, expected, document)
}

/// The reader must have kept every mask the file declares visible: it drops a failing mask
/// without a word (`unwrap_or(None)`), so an expected mask that is absent is a loss.
fn check_masks(path: &Path, expected: &Expected, document: &KraDocument) -> Result<(), ImportError> {
    let mut kept: HashMap<&str, &KraLayer> = HashMap::new();
    for layer in &document.layers {
        kept.insert(layer.filename.as_str(), layer);
    }
    for identity in expected.masked() {
        match kept.get(identity.filename.as_str()) {
            Some(layer) if layer.mask.is_some() => {}
            Some(_) => {
                return Err(unreadable(
                    path,
                    format!(
                        "the reader dropped the transparency mask of the layer {}, which maindoc.xml \
                         declares visible",
                        identity.describe()
                    ),
                ));
            }
            None => {}
        }
    }
    Ok(())
}

/// A node as reconciliation compares it: identity, kind and parent, never a position.
struct Flat<'a> {
    kind: Kind,
    name: &'a str,
    filename: &'a str,
    parent: Option<&'a str>,
}

impl Expected {
    /// The tree, flattened: order does not matter once identity and parent are compared.
    fn flatten(&self) -> Vec<Flat<'_>> {
        fn walk<'a>(nodes: &'a [ExpectedNode], parent: Option<&'a str>, out: &mut Vec<Flat<'a>>) {
            for node in nodes {
                match node {
                    ExpectedNode::Layer { identity, .. } => {
                        out.push(Flat { kind: Kind::Layer, name: &identity.name, filename: &identity.filename, parent })
                    }
                    ExpectedNode::Group { identity, children } => {
                        out.push(Flat {
                            kind: Kind::Group,
                            name: &identity.name,
                            filename: &identity.filename,
                            parent,
                        });
                        walk(children, Some(&identity.filename), out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, None, &mut out);
        out
    }

    /// The layers whose transparency mask the reader must have kept.
    fn masked(&self) -> Vec<&Identity> {
        fn walk<'a>(nodes: &'a [ExpectedNode], out: &mut Vec<&'a Identity>) {
            for node in nodes {
                match node {
                    ExpectedNode::Layer { identity, mask: true } => out.push(identity),
                    ExpectedNode::Layer { .. } => {}
                    ExpectedNode::Group { children, .. } => walk(children, out),
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

/// The reader's tree, flattened the same way, from `root_children` and the folders it names.
fn flatten_reader(document: &KraDocument) -> Result<Vec<Flat<'_>>, ImportError> {
    fn walk<'a>(
        nodes: &'a [KraNode],
        parent: Option<&'a str>,
        document: &'a KraDocument,
        out: &mut Vec<Flat<'a>>,
    ) -> Result<(), ImportError> {
        for node in nodes {
            match node {
                KraNode::Layer(index) => {
                    let Some(layer) = document.layers.get(*index) else {
                        return Err(ImportError::Unreadable(format!(
                            "the reader's layer tree names layer {index}, which it does not hold"
                        )));
                    };
                    out.push(Flat { kind: Kind::Layer, name: &layer.name, filename: &layer.filename, parent });
                }
                KraNode::Folder(index) => {
                    let Some(folder) = document.folders.get(*index) else {
                        return Err(ImportError::Unreadable(format!(
                            "the reader's layer tree names group {index}, which it does not hold"
                        )));
                    };
                    out.push(Flat { kind: Kind::Group, name: &folder.name, filename: &folder.filename, parent });
                    walk(&folder.children, Some(&folder.filename), document, out)?;
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(&document.root_children, None, document, &mut out)?;
    Ok(out)
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Layer => "layer",
        Kind::Group => "group",
    }
}

fn identity(name: &str, filename: &str) -> String {
    format!("{name:?} ({filename})")
}

fn under(parent: Option<&str>) -> String {
    match parent {
        Some(parent) => format!(" in the group ({parent})"),
        None => String::new(),
    }
}

/// The canvas the archive declares, refused before the reader allocates anything from it.
///
/// The reader builds a canvas-sized image per layer from these two numbers, so a document claiming
/// 200000 × 200000 must fail here, before that allocation, and not after `KraDocument::open`.
fn check_canvas_size(image: &Node<'_, '_>, path: &Path) -> Result<(u32, u32), ImportError> {
    let dimension = |name: &str| -> Result<u64, ImportError> {
        let Some(value) = image.attribute(name) else {
            return Err(unreadable(path, format!("maindoc.xml's IMAGE declares no {name}")));
        };
        value.trim().parse::<u64>().map_err(|_| {
            unreadable(path, format!("maindoc.xml's IMAGE declares {name}={value:?}, which is not a number"))
        })
    };
    let (width, height) = (dimension("width")?, dimension("height")?);
    if width == 0 || height == 0 {
        return Err(unreadable(path, format!("maindoc.xml declares a {width} by {height} pixel canvas")));
    }
    let most = canvas_edge_most();
    if width > u64::from(most) || height > u64::from(most) {
        let clamp = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
        return Err(ImportError::TooLarge { width: clamp(width), height: clamp(height), most });
    }
    let (width, height) = (u32::try_from(width).unwrap_or(most), u32::try_from(height).unwrap_or(most));
    Ok((width, height))
}

/// The `IMAGE` element's colour model: only the one whose tiles the reader decodes.
fn check_image_colorspace(image: &Node<'_, '_>) -> Result<(), ImportError> {
    check_colorspace(image, "the document")?;
    match image.attribute("colorspacename") {
        Some(SUPPORTED_COLORSPACE) => Ok(()),
        Some(other) => Err(ImportError::Unsupported(format!(
            "the document declares the colour space {other:?}; only {SUPPORTED_COLORSPACE} is read"
        ))),
        None => Err(ImportError::Unsupported(format!(
            "the document declares no colour space; only {SUPPORTED_COLORSPACE} is read"
        ))),
    }
}

/// A colour profile an `IMAGE` or a layer declares: the reader never reads it, so every pixel it
/// decodes is taken as sRGB. A name this importer treats as sRGB is silent; any other name — a
/// linear-gamma sibling, a foreign working space or an empty value — is reported, never converted.
fn check_profile(node: &Node<'_, '_>, warnings: &mut Vec<ImportWarning>) {
    let Some(profile) = node.attribute("profile") else { return };
    if SRGB_PROFILES.contains(&profile) {
        return;
    }
    warnings.push(ImportWarning::ProfileNotApplied { profile: profile.to_owned() });
}

/// A layer-level colour declaration, when the file carries one, must be the same model.
fn check_colorspace(node: &Node<'_, '_>, what: &str) -> Result<(), ImportError> {
    for attribute in ["colorspacename", "colorspace"] {
        let Some(value) = node.attribute(attribute) else { continue };
        if value != SUPPORTED_COLORSPACE {
            return Err(ImportError::Unsupported(format!(
                "{what} declares the colour space {value:?}; only {SUPPORTED_COLORSPACE} is read"
            )));
        }
    }
    Ok(())
}

/// How a `<layer>` is named in an error before its filename is trusted.
fn describe(node: &Node<'_, '_>) -> String {
    let name = node.attribute("name").unwrap_or("<unnamed>");
    match node.attribute("filename") {
        Some(filename) => format!("{name:?} ({filename})"),
        None => format!("{name:?} (no filename)"),
    }
}

/// `maindoc.xml`, bounded so a hostile archive cannot make the preflight allocate without limit.
fn read_maindoc(archive: &mut zip::ZipArchive<File>, path: &Path) -> Result<String, ImportError> {
    let file = archive
        .by_name("maindoc.xml")
        .map_err(|error| unreadable(path, format!("the archive has no readable maindoc.xml: {error}")))?;
    let mut xml = String::new();
    file.take(MAX_MAINDOC_BYTES + 1)
        .read_to_string(&mut xml)
        .map_err(|error| unreadable(path, format!("reading maindoc.xml: {error}")))?;
    if xml.len() as u64 > MAX_MAINDOC_BYTES {
        return Err(unreadable(path, "maindoc.xml is implausibly large"));
    }
    Ok(xml)
}

fn unreadable(path: &Path, why: impl std::fmt::Display) -> ImportError {
    ImportError::Unreadable(format!("reading {}: {why}", path.display()))
}
