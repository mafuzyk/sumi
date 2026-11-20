//! Native Krita (`.kra`) reader.
//!
//! A `.kra` is a ZIP archive: `maindoc.xml` holds the layer tree and each
//! paint layer is a tiled image file under `<image-name>/layers/`. Tiles are
//! 64x64, start with a one-byte compression flag (0 = raw, 1 = LZF), and store
//! interleaved BGRA bytes. Missing tiles use the layer's `.defaultpixel`.
//!
//! The reader flattens groups with inherited opacity/visibility, honours the
//! common blend modes, applies transparency and free-transform masks,
//! desaturate filter masks, rasterises vector shape layers from their embedded
//! SVG, resolves clone layers and composites into straight RGBA. Generator,
//! filter and file layers are not evaluated yet; the merged PNG in the archive
//! remains the reference for those documents.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{bail, Context, Result};
use roxmltree::Node;

use crate::blend;
use crate::render::Image;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KraNode {
    Layer(usize),
    Folder(usize),
}

#[derive(Debug, Clone)]
pub struct KraFolder {
    pub name: String,
    /// `filename` attribute in `maindoc.xml`, used for write-back.
    pub filename: String,
    pub depth: usize,
    /// Own opacity before the enclosing groups (0..1).
    pub opacity: f64,
    /// Own visibility before the enclosing groups.
    pub visible: bool,
    pub blend_mode: i64,
    /// Children bottom-up, matching `layers` order.
    pub children: Vec<KraNode>,
}

pub struct KraDocument {
    pub width: u32,
    pub height: u32,
    pub name: String,
    pub layers: Vec<KraLayer>,
    pub folders: Vec<KraFolder>,
    /// Bottom-up root children, matching the layer stacking order.
    pub root_children: Vec<KraNode>,
    pub merged: Option<Image>,
    /// Canvas resolution from `maindoc.xml` (`x-res`).
    pub ppi: f64,
}

pub struct KraLayer {
    pub name: String,
    pub visible: bool,
    pub opacity: f64,
    pub blend_mode: i64,
    pub image: Image,
    /// Transparency mask; the alpha channel holds the coverage (255 keeps the
    /// layer pixel, 0 hides it). Already in canvas coordinates.
    pub mask: Option<Image>,
    pub uuid: String,
    /// Layer origin in canvas pixels.
    pub offset: (i64, i64),
    /// UUID of the source layer for clone layers.
    pub clone_from: Option<String>,
    /// `filename` attribute in `maindoc.xml`; identifies the layer on write-back.
    pub filename: String,
    /// Tiled paint data entry. `None` for vector and clone layers.
    pub data_path: Option<String>,
    /// Transparency mask entry, when the layer has one.
    pub mask_path: Option<String>,
    /// Own opacity before the enclosing groups are folded in (0..1).
    pub own_opacity: f64,
    /// Opacity inherited from the enclosing groups (0..1).
    pub inherited_opacity: f64,
    /// Own visibility before the enclosing groups are folded in.
    pub own_visible: bool,
    /// Visibility inherited from the enclosing groups.
    pub inherited_visible: bool,
}

impl KraDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())
            .with_context(|| format!("open {}", path.as_ref().display()))?;
        let mut zip = zip::ZipArchive::new(file).context("read .kra ZIP")?;
        let xml = read_zip_string(&mut zip, "maindoc.xml")?;
        let doc = roxmltree::Document::parse_with_options(
            &xml,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            },
        )
        .context("parse maindoc.xml")?;
        let image = doc
            .descendants()
            .find(|n| n.has_tag_name("IMAGE"))
            .context("maindoc.xml has no IMAGE")?;
        let width = attr_num(&image, "width").unwrap_or(0);
        let height = attr_num(&image, "height").unwrap_or(0);
        if width == 0 || height == 0 {
            bail!("Krita document has zero canvas size");
        }
        let name = image.attribute("name").unwrap_or("Krita").to_string();
        let base = name.clone();
        let ppi = image
            .attribute("x-res")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(72.0);
        let mut layers = Vec::new();
        let mut folders = Vec::new();
        let mut root_children = Vec::new();
        if let Some(root) = image.children().find(|n| n.has_tag_name("layers")) {
            walk_layers(
                &mut zip,
                root,
                &base,
                width,
                height,
                ppi,
                1.0,
                true,
                0,
                &mut layers,
                &mut folders,
                &mut root_children,
            )?;
        }
        root_children.reverse();
        resolve_clones(&mut layers);
        let merged = read_zip_png(&mut zip, "mergedimage.png").ok();
        Ok(KraDocument {
            width,
            height,
            name,
            layers,
            folders,
            root_children,
            merged,
            ppi: ppi as f64,
        })
    }

    /// Composite all supported layers into a straight-RGBA image.
    pub fn composite(&self) -> Image {
        let mut out = Image::new(self.width, self.height);
        // Krita lists layers top-to-bottom in maindoc.xml; composite from the
        // bottom of the stack upwards.
        for layer in self.layers.iter().rev() {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            for (i, (dst, src)) in out
                .data
                .chunks_exact_mut(4)
                .zip(layer.image.data.chunks_exact(4))
                .enumerate()
            {
                let mut sa = src[3] as f32 / 255.0 * layer.opacity as f32;
                if let Some(mask) = &layer.mask {
                    sa *= mask.data.get(i * 4 + 3).copied().unwrap_or(0) as f32 / 255.0;
                }
                if sa <= 0.0 {
                    continue;
                }
                let ab = dst[3] as f32 / 255.0;
                let cb = [
                    dst[0] as f32 / 255.0,
                    dst[1] as f32 / 255.0,
                    dst[2] as f32 / 255.0,
                ];
                let cs = [
                    src[0] as f32 / 255.0,
                    src[1] as f32 / 255.0,
                    src[2] as f32 / 255.0,
                ];
                let (rgb, oa) = blend::over(layer.blend_mode, cb, ab, cs, sa);
                dst[0] = (rgb[0] * 255.0).round().clamp(0.0, 255.0) as u8;
                dst[1] = (rgb[1] * 255.0).round().clamp(0.0, 255.0) as u8;
                dst[2] = (rgb[2] * 255.0).round().clamp(0.0, 255.0) as u8;
                dst[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        out
    }
}

/// One layer whose pixels, mask and attributes are written back into an
/// existing `.kra` archive. Entries that are not listed here are copied
/// verbatim, so groups, vector layers, filter masks and blend settings the
/// reader does not edit survive the round-trip.
pub struct KraLayerEdit<'a> {
    /// `filename` attribute in `maindoc.xml`, e.g. `layer3`.
    pub filename: &'a str,
    /// New layer or group name.
    pub name: Option<&'a str>,
    /// ZIP entry holding the tiled paint data; `None` updates attributes only.
    pub data_path: Option<&'a str>,
    /// ZIP entry holding the transparency mask, when the layer has one.
    pub mask_path: Option<&'a str>,
    /// Canvas-sized layer pixels in straight RGBA.
    pub image: ImageRef<'a>,
    pub mask: Option<ImageRef<'a>>,
    /// Layer origin in canvas pixels. Offset layers are not written back yet.
    pub offset: (i64, i64),
    /// New own opacity (0..1), before group opacity is folded in.
    pub opacity: Option<f64>,
    /// New own visibility. `None` keeps the stored value (a layer inside a
    /// hidden group cannot express its own choice through the editor).
    pub visible: Option<bool>,
    /// Krita `compositeop` name.
    pub composite: Option<&'a str>,
}

/// Borrowed RGBA pixels for write-back, so saving does not clone every layer.
#[derive(Clone, Copy)]
pub struct ImageRef<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// Replace layer payloads and attributes in an existing `.kra` without
/// re-encoding its document tree. The archive is rewritten through a sibling
/// temporary file, so a failure leaves the original untouched.
#[derive(Clone, Copy)]
pub struct KraAddedLayer<'a> {
    pub name: &'a str,
    /// `filename` attribute for the new `maindoc.xml` layer (e.g. `layer13`).
    pub filename: &'a str,
    /// ZIP entry for the new tiled paint data.
    pub data_path: &'a str,
    /// Parent group `filename`, or `None` for the root.
    pub parent_filename: Option<&'a str>,
    /// Existing sibling to insert before in XML order (top-to-bottom), or
    /// `None` for the top of the parent group.
    pub above_filename: Option<&'a str>,
    pub image: ImageRef<'a>,
    pub opacity: f64,
    pub visible: bool,
    pub composite: &'a str,
}

pub struct KraRemovedLayer<'a> {
    pub filename: &'a str,
    pub data_path: Option<&'a str>,
    pub mask_path: Option<&'a str>,
}

#[derive(Clone, Copy)]
pub struct KraAddedGroup<'a> {
    pub name: &'a str,
    pub filename: &'a str,
    pub parent_filename: Option<&'a str>,
    pub above_filename: Option<&'a str>,
    pub opacity: f64,
    pub visible: bool,
    pub composite: &'a str,
}

pub struct KraRemovedGroup<'a> {
    pub filename: &'a str,
}

/// A new transparency mask attached to an existing or newly added layer.
#[derive(Clone, Copy)]
pub struct KraAddedMask<'a> {
    /// Parent layer `filename`.
    pub layer_filename: &'a str,
    /// New mask `filename` (e.g. `mask13`).
    pub mask_filename: &'a str,
    pub name: &'a str,
    /// ZIP entry for the tiled one-channel mask.
    pub data_path: &'a str,
    /// Canvas-sized RGBA whose alpha channel is the mask coverage.
    pub image: ImageRef<'a>,
}

pub struct KraRemovedMask<'a> {
    pub layer_filename: &'a str,
    pub mask_filename: &'a str,
    pub data_path: &'a str,
}

/// Replace layer payloads and attributes in an existing `.kra` without
/// re-encoding its document tree. The archive is rewritten through a sibling
/// temporary file, so a failure leaves the original untouched.
pub fn write_back(path: &Path, edits: &[KraLayerEdit<'_>], merged: &Image) -> Result<()> {
    write_back_tree(path, edits, &[], &[], &[], &[], &[], &[], merged, None)
}

/// Like [`write_back`], but also inserts and removes paint layers in
/// `maindoc.xml` and their tiled ZIP entries. Groups and other layer types are
/// left untouched.
#[allow(clippy::too_many_arguments)]
pub fn write_back_tree(
    path: &Path,
    edits: &[KraLayerEdit<'_>],
    added: &[KraAddedLayer<'_>],
    added_groups: &[KraAddedGroup<'_>],
    added_masks: &[KraAddedMask<'_>],
    removed: &[KraRemovedLayer<'_>],
    removed_groups: &[KraRemovedGroup<'_>],
    removed_masks: &[KraRemovedMask<'_>],
    merged: &Image,
    canvas_size: Option<(u32, u32)>,
) -> Result<()> {
    for edit in edits {
        if edit.offset != (0, 0) {
            bail!(
                "cannot write back offset layer {:?} at {},{}; only canvas-aligned layers are supported",
                edit.filename,
                edit.offset.0,
                edit.offset.1
            );
        }
    }
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("read .kra ZIP")?;
    struct Entry {
        name: String,
        method: zip::CompressionMethod,
        data: Vec<u8>,
    }
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        entries.push(Entry {
            name: entry.name().to_string(),
            method: entry.compression(),
            data,
        });
    }
    drop(archive);

    for edit in edits {
        let Some(data_path) = edit.data_path else {
            continue;
        };
        let mut found = false;
        for entry in &mut entries {
            if entry.name == data_path {
                entry.data = crate::export::encode_krita_tiles(
                    edit.image.data,
                    edit.image.width,
                    edit.image.height,
                );
                entry.method = zip::CompressionMethod::Deflated;
                found = true;
            } else if Some(entry.name.as_str()) == edit.mask_path {
                let mask = edit
                    .mask
                    .context("edit supplied a mask path without a mask")?;
                entry.data = encode_mask_tiles(mask.data, mask.width, mask.height);
                entry.method = zip::CompressionMethod::Deflated;
            }
        }
        if !found {
            bail!("Krita layer data {:?} is not in the archive", data_path);
        }
    }

    for remove in removed_masks {
        let default_pixel = format!("{}.defaultpixel", remove.data_path);
        entries.retain(|entry| entry.name != remove.data_path && entry.name != default_pixel);
    }
    for remove in removed {
        entries.retain(|entry| {
            Some(entry.name.as_str()) != remove.data_path
                && Some(entry.name.as_str()) != remove.mask_path
        });
    }
    for add in added {
        if entries.iter().any(|entry| entry.name == add.data_path) {
            bail!("Krita layer entry {:?} already exists", add.data_path);
        }
        entries.push(Entry {
            name: add.data_path.to_string(),
            method: zip::CompressionMethod::Deflated,
            data: crate::export::encode_krita_tiles(
                add.image.data,
                add.image.width,
                add.image.height,
            ),
        });
    }
    for add in added_masks {
        let default_pixel = format!("{}.defaultpixel", add.data_path);
        if entries
            .iter()
            .any(|entry| entry.name == add.data_path || entry.name == default_pixel)
        {
            bail!("Krita mask entry {:?} already exists", add.data_path);
        }
        entries.push(Entry {
            name: add.data_path.to_string(),
            method: zip::CompressionMethod::Deflated,
            data: encode_mask_tiles(add.image.data, add.image.width, add.image.height),
        });
        entries.push(Entry {
            name: default_pixel,
            method: zip::CompressionMethod::Deflated,
            // A transparency mask defaults to fully visible.
            data: vec![255],
        });
    }

    let merged_png = crate::export::encode_png(merged)?;
    let mut patched_xml = None;
    for entry in &mut entries {
        match entry.name.as_str() {
            "maindoc.xml" => {
                let mut xml = std::str::from_utf8(&entry.data)
                    .context("maindoc.xml is not UTF-8")?
                    .to_string();
                for remove in removed_masks {
                    xml = remove_mask_element(&xml, remove.mask_filename)?;
                }
                for remove in removed {
                    xml = remove_layer_element(&xml, remove.filename)?;
                }
                for remove in removed_groups {
                    xml = remove_layer_element(&xml, remove.filename)?;
                }
                for add in added_groups {
                    let template = group_template(&xml)?;
                    let tag = new_group_tag(&template, add);
                    xml = insert_group_element(&xml, add, &tag)?;
                }
                for add in added {
                    let template = paint_layer_template(&xml)?;
                    let tag = new_layer_tag(&template, add);
                    xml = insert_layer_element(&xml, add, &tag)?;
                }
                for add in added_masks {
                    xml = insert_mask_element(&xml, add)?;
                }
                for edit in edits {
                    xml = patch_layer_attrs(&xml, edit)?;
                }
                if let Some((width, height)) = canvas_size {
                    xml = patch_canvas_size(&xml, width, height)?;
                }
                entry.data = xml.into_bytes();
                entry.method = zip::CompressionMethod::Deflated;
                patched_xml = Some(());
            }
            "mergedimage.png" | "preview.png" => {
                entry.data = merged_png.clone();
                entry.method = zip::CompressionMethod::Deflated;
            }
            _ => {}
        }
    }
    if patched_xml.is_none() {
        bail!("archive has no maindoc.xml");
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Krita path has no file name")?;
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut writer = zip::ZipWriter::new(File::create(&tmp)?);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for entry in &entries {
        let options = if entry.name == "mimetype" {
            stored
        } else {
            deflated
        };
        writer.start_file(&entry.name, options)?;
        writer.write_all(&entry.data)?;
    }
    writer.finish()?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn parse_maindoc(xml: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .context("parse maindoc.xml")
}

fn paint_layer_template(xml: &str) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    for node in doc.descendants().filter(|node| {
        node.has_tag_name("layer") && node.attribute("nodetype") == Some("paintlayer")
    }) {
        let range = node.range();
        let slice = &xml[range.start..range.end];
        if let Some(open) = slice.find('>') {
            let mut tag = slice[..=open].to_string();
            if !tag.ends_with("/>") {
                tag.pop();
                tag.push_str("/>");
            }
            return Ok(tag);
        }
    }
    bail!("maindoc.xml has no paint layer to use as a template")
}

fn new_layer_tag(template: &str, add: &KraAddedLayer<'_>) -> String {
    let mut tag = template.to_string();
    tag = set_attr(&tag, "name", &xml_escape(add.name));
    tag = set_attr(&tag, "filename", add.filename);
    tag = set_attr(&tag, "nodetype", "paintlayer");
    tag = set_attr(&tag, "x", "0");
    tag = set_attr(&tag, "y", "0");
    tag = set_attr(&tag, "visible", if add.visible { "1" } else { "0" });
    tag = set_attr(&tag, "opacity", &opacity_byte(add.opacity).to_string());
    tag = set_attr(&tag, "compositeop", add.composite);
    tag = set_attr(&tag, "selected", "false");
    tag = set_attr(&tag, "uuid", &new_layer_uuid(add.filename));
    tag
}

fn new_layer_uuid(seed: &str) -> String {
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut hasher);
    let a = hasher.finish();
    let b = a.rotate_left(29) ^ 0x9e37_79b9_7f4a_7c15;
    format!(
        "{{{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}}}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0x0fff) as u16,
        (b >> 48) as u16,
        b & 0xffff_ffff_ffff
    )
}

fn group_template(xml: &str) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    for node in doc.descendants().filter(|node| {
        node.has_tag_name("layer") && node.attribute("nodetype") == Some("grouplayer")
    }) {
        let range = node.range();
        let slice = &xml[range.start..range.end];
        if let Some(open) = slice.find('>') {
            let mut tag = slice[..=open].to_string();
            if tag.ends_with("/>") {
                tag.truncate(tag.len() - 2);
                tag.push('>');
            }
            return Ok(tag);
        }
    }
    bail!("maindoc.xml has no group layer to use as a template")
}

fn new_group_tag(template: &str, add: &KraAddedGroup<'_>) -> String {
    let mut tag = template.to_string();
    tag = set_attr(&tag, "name", &xml_escape(add.name));
    tag = set_attr(&tag, "filename", add.filename);
    tag = set_attr(&tag, "nodetype", "grouplayer");
    tag = set_attr(&tag, "passthrough", "0");
    tag = set_attr(&tag, "visible", if add.visible { "1" } else { "0" });
    tag = set_attr(&tag, "opacity", &opacity_byte(add.opacity).to_string());
    tag = set_attr(&tag, "compositeop", add.composite);
    tag = set_attr(&tag, "selected", "false");
    tag = set_attr(&tag, "uuid", &new_layer_uuid(add.filename));
    format!("{tag}\n<layers>\n</layers>\n</layer>")
}

fn insert_group_element(xml: &str, add: &KraAddedGroup<'_>, tag: &str) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    let layers = if let Some(parent) = add.parent_filename {
        let parent_node = doc
            .descendants()
            .find(|node| node.has_tag_name("layer") && node.attribute("filename") == Some(parent));
        parent_node
            .and_then(|node| node.children().find(|child| child.has_tag_name("layers")))
            .with_context(|| format!("group {parent:?} has no <layers> element"))?
    } else {
        doc.descendants()
            .find(|node| node.has_tag_name("layers"))
            .context("maindoc.xml has no root <layers>")?
    };
    let offset = if let Some(above) = add.above_filename {
        doc.descendants()
            .find(|node| node.attribute("filename") == Some(above))
            .map(|node| node.range().start)
            .with_context(|| format!("sibling group {above:?} is not in maindoc.xml"))?
    } else {
        let start = layers.range().start;
        let open = xml[start..]
            .find('>')
            .context("<layers> has no closing bracket")?;
        start + open + 1
    };
    let mut out = String::with_capacity(xml.len() + tag.len() + 2);
    out.push_str(&xml[..offset]);
    out.push('\n');
    out.push_str(tag);
    out.push_str(&xml[offset..]);
    Ok(out)
}

fn new_mask_tag(add: &KraAddedMask<'_>) -> String {
    format!(
        r#"<mask nodetype="transparencymask" name="{}" locked="0" x="0" y="0" uuid="{}" visible="1" filename="{}"/>"#,
        xml_escape(add.name),
        new_layer_uuid(add.mask_filename),
        add.mask_filename
    )
}

fn insert_mask_element(xml: &str, add: &KraAddedMask<'_>) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    let layer = doc
        .descendants()
        .find(|node| {
            node.has_tag_name("layer") && node.attribute("filename") == Some(add.layer_filename)
        })
        .with_context(|| format!("layer {:?} is not in maindoc.xml", add.layer_filename))?;
    let tag = new_mask_tag(add);
    if let Some(masks) = layer.children().find(|node| node.has_tag_name("masks")) {
        let end = masks.range().end;
        let close = xml[..end]
            .rfind("</masks>")
            .context("<masks> has no closing tag")?;
        let mut out = String::with_capacity(xml.len() + tag.len() + 2);
        out.push_str(&xml[..close]);
        out.push_str(&tag);
        out.push('\n');
        out.push_str(&xml[close..]);
        return Ok(out);
    }
    let start = layer.range().start;
    let open_end = start
        + xml[start..]
            .find('>')
            .context("layer tag has no closing bracket")?;
    let open = &xml[start..=open_end];
    let mut out = String::with_capacity(xml.len() + tag.len() + 32);
    if open.ends_with("/>") {
        out.push_str(&xml[..start]);
        out.push_str(&open[..open.len() - 2]);
        out.push('>');
        out.push_str("\n<masks>\n");
        out.push_str(&tag);
        out.push_str("\n</masks>\n</layer>");
        out.push_str(&xml[open_end + 1..]);
    } else {
        // The layer is already open (for example after its previous mask was
        // removed); insert a new <masks> block before its existing closing
        // tag instead of appending a second </layer>.
        let layer_end = layer.range().end;
        let close = xml[..layer_end]
            .rfind("</layer>")
            .context("layer tag has no closing element")?;
        out.push_str(&xml[..close]);
        out.push_str("<masks>\n");
        out.push_str(&tag);
        out.push_str("\n</masks>\n");
        out.push_str(&xml[close..]);
    }
    Ok(out)
}

fn remove_mask_element(xml: &str, mask_filename: &str) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    let mask = doc
        .descendants()
        .find(|node| node.has_tag_name("mask") && node.attribute("filename") == Some(mask_filename))
        .with_context(|| format!("maindoc.xml has no mask {mask_filename:?}"))?;
    let range = mask.range();
    let mut out = String::with_capacity(xml.len());
    out.push_str(&xml[..range.start]);
    out.push_str(&xml[range.end..]);
    // Drop a <masks> block that no longer holds any masks.
    let doc = parse_maindoc(&out)?;
    if let Some(masks) = doc
        .descendants()
        .find(|node| node.has_tag_name("masks") && !node.children().any(|c| c.is_element()))
    {
        let range = masks.range();
        let mut cleaned = String::with_capacity(out.len());
        cleaned.push_str(&out[..range.start]);
        cleaned.push_str(&out[range.end..]);
        return Ok(cleaned);
    }
    Ok(out)
}

fn insert_layer_element(xml: &str, add: &KraAddedLayer<'_>, tag: &str) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    let layers = if let Some(parent) = add.parent_filename {
        let parent_node = doc
            .descendants()
            .find(|node| node.has_tag_name("layer") && node.attribute("filename") == Some(parent));
        parent_node
            .and_then(|node| node.children().find(|child| child.has_tag_name("layers")))
            .with_context(|| format!("group {parent:?} has no <layers> element"))?
    } else {
        doc.descendants()
            .find(|node| node.has_tag_name("layers"))
            .context("maindoc.xml has no root <layers>")?
    };
    let offset = if let Some(above) = add.above_filename {
        doc.descendants()
            .find(|node| node.attribute("filename") == Some(above))
            .map(|node| node.range().start)
            .with_context(|| format!("sibling layer {above:?} is not in maindoc.xml"))?
    } else {
        let start = layers.range().start;
        let open = xml[start..]
            .find('>')
            .context("<layers> has no closing bracket")?;
        start + open + 1
    };
    let mut out = String::with_capacity(xml.len() + tag.len() + 2);
    out.push_str(&xml[..offset]);
    out.push('\n');
    out.push_str(tag);
    out.push_str(&xml[offset..]);
    Ok(out)
}

fn remove_layer_element(xml: &str, filename: &str) -> Result<String> {
    let doc = parse_maindoc(xml)?;
    let node = doc
        .descendants()
        .find(|node| node.has_tag_name("layer") && node.attribute("filename") == Some(filename))
        .with_context(|| format!("maindoc.xml has no layer {filename:?}"))?;
    let range = node.range();
    let mut out = String::with_capacity(xml.len());
    out.push_str(&xml[..range.start]);
    out.push_str(&xml[range.end..]);
    Ok(out)
}

/// A mask is a one-channel tiled stream/// A mask is a one-channel tiled stream: `PIXELSIZE 1`, alpha bytes only.
fn encode_mask_tiles(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let tile_w = 64u32;
    let tile_h = 64u32;
    let count = width.div_ceil(tile_w) * height.div_ceil(tile_h);
    let mut out = Vec::with_capacity((width * height) as usize + 128);
    out.extend_from_slice(b"VERSION 2\n");
    out.extend_from_slice(b"TILEWIDTH 64\n");
    out.extend_from_slice(b"TILEHEIGHT 64\n");
    out.extend_from_slice(b"PIXELSIZE 1\n");
    out.extend_from_slice(format!("DATA {count}\n").as_bytes());
    let mut tile = vec![0u8; (tile_w * tile_h) as usize];
    for tile_y in (0..height).step_by(tile_h as usize) {
        for tile_x in (0..width).step_by(tile_w as usize) {
            tile.fill(0);
            for row in 0..tile_h.min(height.saturating_sub(tile_y)) {
                let src_y = tile_y + row;
                let copy_w = tile_w.min(width.saturating_sub(tile_x));
                for col in 0..copy_w {
                    let src = ((src_y * width + tile_x + col) * 4) as usize;
                    tile[(row * tile_w + col) as usize] = data[src + 3];
                }
            }
            out.extend_from_slice(format!("{tile_x},{tile_y},LZF,{}\n", tile.len() + 1).as_bytes());
            out.push(0);
            out.extend_from_slice(&tile);
        }
    }
    out
}

/// Update `opacity`, `visible` and `compositeop` in the start tag that carries
/// `filename="..."`. Everything outside that tag is copied byte for byte.
fn patch_canvas_size(xml: &str, width: u32, height: u32) -> Result<String> {
    let start = xml
        .find("<IMAGE")
        .context("maindoc.xml has no IMAGE element")?;
    let end = start
        + xml[start..]
            .find('>')
            .context("maindoc.xml IMAGE element is unterminated")?;
    let mut tag = xml[start..=end].to_string();
    tag = replace_xml_attr(&tag, "width", &width.to_string());
    tag = replace_xml_attr(&tag, "height", &height.to_string());
    Ok(format!("{}{}{}", &xml[..start], tag, &xml[end + 1..]))
}

fn replace_xml_attr(tag: &str, name: &str, value: &str) -> String {
    let needle = format!("{name}=\"");
    let Some(start) = tag.find(&needle) else {
        return tag.to_string();
    };
    let value_start = start + needle.len();
    let Some(end) = tag[value_start..].find('"') else {
        return tag.to_string();
    };
    format!(
        "{}{}{}",
        &tag[..value_start],
        value,
        &tag[value_start + end..]
    )
}

fn patch_layer_attrs(xml: &str, edit: &KraLayerEdit<'_>) -> Result<String> {
    let marker = format!(" filename=\"{}\"", edit.filename);
    let pos = xml
        .find(&marker)
        .with_context(|| format!("maindoc.xml has no layer filename {:?}", edit.filename))?;
    let start = xml[..pos].rfind('<').context("layer tag has no start")?;
    let end = pos + xml[pos..].find('>').context("layer tag has no end")?;
    let mut tag = xml[start..=end].to_string();
    if let Some(name) = edit.name {
        tag = set_attr(&tag, "name", &xml_escape(name));
    }
    if let Some(opacity) = edit.opacity {
        tag = set_attr(&tag, "opacity", &opacity_byte(opacity).to_string());
    }
    if let Some(visible) = edit.visible {
        tag = set_attr(&tag, "visible", if visible { "1" } else { "0" });
    }
    if let Some(composite) = edit.composite {
        tag = set_attr(&tag, "compositeop", composite);
    }
    let mut out = String::with_capacity(xml.len() + 24);
    out.push_str(&xml[..start]);
    out.push_str(&tag);
    out.push_str(&xml[end + 1..]);
    Ok(out)
}

fn opacity_byte(opacity: f64) -> u32 {
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u32
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn set_attr(tag: &str, name: &str, value: &str) -> String {
    let pattern = format!(" {name}=\"");
    if let Some(pos) = tag.find(&pattern) {
        let value_start = pos + pattern.len();
        let Some(len) = tag[value_start..].find('"') else {
            return tag.to_string();
        };
        let mut out = String::with_capacity(tag.len() + 8);
        out.push_str(&tag[..value_start]);
        out.push_str(value);
        out.push_str(&tag[value_start + len..]);
        out
    } else {
        let close = tag
            .rfind("/>")
            .or_else(|| tag.rfind('>'))
            .unwrap_or(tag.len());
        let mut out = String::with_capacity(tag.len() + value.len() + name.len() + 4);
        out.push_str(&tag[..close]);
        out.push_str(&format!(" {name}=\"{value}\""));
        out.push_str(&tag[close..]);
        out
    }
}

/// Krita `compositeop` name for a blend id used by the native compositor.
pub fn composite_name(mode: i64) -> &'static str {
    match mode {
        blend::MODE_DARKEN => "darken",
        blend::MODE_MULTIPLY => "multiply",
        blend::MODE_COLOR_BURN => "burn",
        blend::MODE_LINEAR_BURN => "linearburn",
        blend::MODE_SUBTRACT => "subtract",
        blend::MODE_DARKER_COLOR => "darkercolor",
        blend::MODE_LIGHTEN => "lighten",
        blend::MODE_SCREEN => "screen",
        blend::MODE_COLOR_DODGE => "dodge",
        blend::MODE_ADD => "add",
        blend::MODE_LIGHTER_COLOR => "lightercolor",
        blend::MODE_OVERLAY => "overlay",
        blend::MODE_SOFT_LIGHT => "softlight",
        blend::MODE_HARD_LIGHT => "hardlight",
        blend::MODE_VIVID_LIGHT => "vividlight",
        blend::MODE_LINEAR_LIGHT => "linearlight",
        blend::MODE_PIN_LIGHT => "pinlight",
        blend::MODE_HARD_MIX => "hardmix",
        blend::MODE_DIFFERENCE => "difference",
        blend::MODE_EXCLUSION => "exclusion",
        blend::MODE_HUE => "hue",
        blend::MODE_SATURATION => "saturation",
        blend::MODE_COLOR => "color",
        blend::MODE_LUMINOSITY => "luminosity",
        blend::MODE_DIVIDE => "divide",
        _ => "normal",
    }
}

/// Krita clone layers carry no pixels; they re-display another layer shifted by
/// the difference between the two layer origins.
fn resolve_clones(layers: &mut [KraLayer]) {
    let mut sources: HashMap<String, usize> = HashMap::new();
    for (i, layer) in layers.iter().enumerate() {
        if layer.clone_from.is_none() && !layer.uuid.is_empty() {
            sources.insert(layer.uuid.clone(), i);
        }
    }
    let mut resolved: Vec<bool> = layers.iter().map(|l| l.clone_from.is_none()).collect();
    loop {
        let mut progress = false;
        for i in 0..layers.len() {
            if resolved[i] {
                continue;
            }
            let Some(source_uuid) = layers[i].clone_from.as_ref() else {
                resolved[i] = true;
                continue;
            };
            let Some(&si) = sources.get(source_uuid) else {
                continue;
            };
            if !resolved[si] {
                continue;
            }
            let source = layers[si].image.clone();
            let dx = layers[i].offset.0 - layers[si].offset.0;
            let dy = layers[i].offset.1 - layers[si].offset.1;
            layers[i].image = translate_image(&source, dx, dy);
            resolved[i] = true;
            progress = true;
        }
        if !progress {
            break;
        }
    }
}

fn translate_image(src: &Image, dx: i64, dy: i64) -> Image {
    if dx == 0 && dy == 0 {
        return src.clone();
    }
    let mut out = Image::new(src.width, src.height);
    for y in 0..src.height as i64 {
        let sy = y - dy;
        if sy < 0 || sy >= src.height as i64 {
            continue;
        }
        for x in 0..src.width as i64 {
            let sx = x - dx;
            if sx < 0 || sx >= src.width as i64 {
                continue;
            }
            let s = ((sy as usize) * src.width as usize + sx as usize) * 4;
            let d = ((y as usize) * src.width as usize + x as usize) * 4;
            out.data[d..d + 4].copy_from_slice(&src.data[s..s + 4]);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_layers(
    zip: &mut zip::ZipArchive<File>,
    node: Node,
    base: &str,
    width: u32,
    height: u32,
    ppi: f32,
    inherited_opacity: f64,
    inherited_visible: bool,
    depth: usize,
    out: &mut Vec<KraLayer>,
    folders: &mut Vec<KraFolder>,
    rows: &mut Vec<KraNode>,
) -> Result<()> {
    for child in node.children().filter(|n| n.has_tag_name("layer")) {
        let kind = child.attribute("nodetype").unwrap_or("paintlayer");
        let before = out.len();
        let own_visible = child.attribute("visible") != Some("0");
        let visible = inherited_visible && own_visible;
        let own_opacity = child
            .attribute("opacity")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(255.0)
            / 255.0;
        let opacity = inherited_opacity * own_opacity;
        match kind {
            "grouplayer" => {
                let fi = folders.len();
                folders.push(KraFolder {
                    name: child.attribute("name").unwrap_or("Group").to_string(),
                    filename: child.attribute("filename").unwrap_or("group").to_string(),
                    depth,
                    opacity: own_opacity,
                    visible: own_visible,
                    blend_mode: blend_mode(child.attribute("compositeop").unwrap_or("normal")),
                    children: Vec::new(),
                });
                let mut children = Vec::new();
                if let Some(group) = child.children().find(|n| n.has_tag_name("layers")) {
                    walk_layers(
                        zip,
                        group,
                        base,
                        width,
                        height,
                        ppi,
                        opacity,
                        visible,
                        depth + 1,
                        out,
                        folders,
                        &mut children,
                    )?;
                }
                children.reverse();
                folders[fi].children = children;
                rows.push(KraNode::Folder(fi));
            }
            "paintlayer" => {
                let Some(filename) = child.attribute("filename") else {
                    continue;
                };
                let path = format!("{base}/layers/{filename}");
                let offset = (
                    child
                        .attribute("x")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                    child
                        .attribute("y")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                );
                let image = match read_layer(zip, &path, width, height, offset) {
                    Ok(image) => image,
                    Err(_) => continue,
                };
                let transform = read_transform_mask(zip, child, base);
                let image = apply_transform(image, transform);
                let image = apply_filter_mask(image, zip, child, base, width, height);
                let (mask, mask_path): (Option<Image>, Option<String>) =
                    match read_transparency_mask(zip, child, base, width, height).unwrap_or(None) {
                        Some((mask, path)) => (Some(mask), Some(path)),
                        None => (None, None),
                    };
                out.push(KraLayer {
                    name: child.attribute("name").unwrap_or(filename).to_string(),
                    visible,
                    opacity,
                    blend_mode: blend_mode(child.attribute("compositeop").unwrap_or("normal")),
                    image,
                    mask,
                    uuid: child.attribute("uuid").unwrap_or("").to_string(),
                    offset,
                    clone_from: None,
                    filename: filename.to_string(),
                    data_path: Some(path),
                    mask_path,
                    own_opacity,
                    inherited_opacity,
                    own_visible,
                    inherited_visible,
                });
            }
            "shapelayer" => {
                let Some(filename) = child.attribute("filename") else {
                    continue;
                };
                let path = format!("{base}/layers/{filename}.shapelayer/content.svg");
                let Ok(svg) = read_zip_string(zip, &path) else {
                    continue;
                };
                let Ok(image) = render_shape_layer(&svg, width, height, ppi) else {
                    continue;
                };
                let (mask, mask_path): (Option<Image>, Option<String>) =
                    match read_transparency_mask(zip, child, base, width, height).unwrap_or(None) {
                        Some((mask, path)) => (Some(mask), Some(path)),
                        None => (None, None),
                    };
                let offset = (
                    child
                        .attribute("x")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                    child
                        .attribute("y")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                );
                out.push(KraLayer {
                    name: child.attribute("name").unwrap_or(filename).to_string(),
                    visible,
                    opacity,
                    blend_mode: blend_mode(child.attribute("compositeop").unwrap_or("normal")),
                    image,
                    mask,
                    uuid: child.attribute("uuid").unwrap_or("").to_string(),
                    offset,
                    clone_from: None,
                    filename: filename.to_string(),
                    data_path: None,
                    mask_path,
                    own_opacity,
                    inherited_opacity,
                    own_visible,
                    inherited_visible,
                });
            }
            "clonelayer" => {
                let Some(source) = child.attribute("clonefromuuid") else {
                    continue;
                };
                let filename = child.attribute("filename").unwrap_or("clone");
                let (mask, mask_path): (Option<Image>, Option<String>) =
                    match read_transparency_mask(zip, child, base, width, height).unwrap_or(None) {
                        Some((mask, path)) => (Some(mask), Some(path)),
                        None => (None, None),
                    };
                let offset = (
                    child
                        .attribute("x")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                    child
                        .attribute("y")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                );
                out.push(KraLayer {
                    name: child.attribute("name").unwrap_or("Clone").to_string(),
                    visible,
                    opacity,
                    blend_mode: blend_mode(child.attribute("compositeop").unwrap_or("normal")),
                    image: Image::new(width, height),
                    mask,
                    uuid: child.attribute("uuid").unwrap_or("").to_string(),
                    offset,
                    clone_from: Some(source.to_string()),
                    filename: filename.to_string(),
                    data_path: None,
                    mask_path,
                    own_opacity,
                    inherited_opacity,
                    own_visible,
                    inherited_visible,
                });
            }
            // Generator, fill and file layers reference resources outside the
            // archive. Represent them as transparent non-writable layers so the
            // tree survives round-trip; their merged contribution is already
            // available in the archive preview.
            other => {
                let Some(filename) = child.attribute("filename") else {
                    continue;
                };
                let path = format!("{base}/layers/{filename}");
                let offset = (
                    child
                        .attribute("x")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                    child
                        .attribute("y")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(0),
                );
                let image = read_layer(zip, &path, width, height, offset)
                    .unwrap_or_else(|_| Image::new(width, height));
                let (mask, mask_path): (Option<Image>, Option<String>) =
                    match read_transparency_mask(zip, child, base, width, height).unwrap_or(None) {
                        Some((mask, path)) => (Some(mask), Some(path)),
                        None => (None, None),
                    };
                out.push(KraLayer {
                    name: child
                        .attribute("name")
                        .unwrap_or_else(|| child.attribute("generatorname").unwrap_or(other))
                        .to_string(),
                    visible,
                    opacity,
                    blend_mode: blend_mode(child.attribute("compositeop").unwrap_or("normal")),
                    image,
                    mask,
                    uuid: child.attribute("uuid").unwrap_or("").to_string(),
                    offset,
                    clone_from: None,
                    filename: filename.to_string(),
                    data_path: None,
                    mask_path,
                    own_opacity,
                    inherited_opacity,
                    own_visible,
                    inherited_visible,
                });
            }
        }
        if kind != "grouplayer" && out.len() > before {
            rows.push(KraNode::Layer(before));
        }
    }
    Ok(())
}

fn read_layer(
    zip: &mut zip::ZipArchive<File>,
    path: &str,
    width: u32,
    height: u32,
    offset: (i64, i64),
) -> Result<Image> {
    let data = read_zip_bytes(zip, path)?;
    let default = read_zip_bytes(zip, &format!("{path}.defaultpixel")).ok();
    let mut image = Image::new(width, height);
    if let Some(default) = default.as_deref().filter(|d| d.len() >= 4) {
        for p in image.data.chunks_exact_mut(4) {
            p.copy_from_slice(&[default[2], default[1], default[0], default[3]]);
        }
    }
    decode_tiles_at(&data, &mut image, 4, offset).with_context(|| format!("decode {path}"))?;
    Ok(image)
}

/// Render a Krita shape layer. Krita stores vector layers as a standalone SVG
/// whose width and height are in points; the document resolution converts
/// those points to canvas pixels (a 245.76 pt viewBox at 300 PPI is 1024 px).
fn render_shape_layer(svg: &str, width: u32, height: u32, ppi: f32) -> Result<Image> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb = font_database();
    options.dpi = ppi;
    // Krita names the font as "Arial", which fontconfig only substitutes but
    // fontdb does not. Fall back to an installed sans-serif family instead of
    // dropping the whole text element.
    options.font_resolver.select_font = Box::new(|font, db| {
        if let Some(id) = resvg::usvg::FontResolver::default_font_selector()(font, db) {
            return Some(id);
        }
        let style = match font.style() {
            resvg::usvg::FontStyle::Normal => resvg::usvg::fontdb::Style::Normal,
            resvg::usvg::FontStyle::Italic => resvg::usvg::fontdb::Style::Italic,
            resvg::usvg::FontStyle::Oblique => resvg::usvg::fontdb::Style::Oblique,
        };
        let names: Vec<resvg::usvg::fontdb::Family> = [
            "Liberation Sans",
            "DejaVu Sans",
            "Noto Sans",
            "Nimbus Sans",
            "Helvetica",
            "Arial",
        ]
        .iter()
        .map(|name| resvg::usvg::fontdb::Family::Name(name))
        .collect();
        let query = resvg::usvg::fontdb::Query {
            families: &names,
            weight: resvg::usvg::fontdb::Weight(font.weight()),
            stretch: resvg::usvg::fontdb::Stretch::Normal,
            style,
        };
        db.query(&query)
    });
    let tree = resvg::usvg::Tree::from_str(svg, &options).context("parse shape layer SVG")?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        bail!("shape layer has zero size");
    }
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(width, height).context("allocate shape layer pixmap")?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut image = Image::new(width, height);
    for (dst, src) in image
        .data
        .chunks_exact_mut(4)
        .zip(pixmap.data().chunks_exact(4))
    {
        let a = src[3];
        if a == 0 {
            continue;
        }
        // tiny-skia produces premultiplied RGBA; the compositor wants straight.
        for c in 0..3 {
            dst[c] = ((src[c] as f32 * 255.0 / a as f32).round()).clamp(0.0, 255.0) as u8;
        }
        dst[3] = a;
    }
    Ok(image)
}

fn font_database() -> Arc<resvg::usvg::fontdb::Database> {
    static DATABASE: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DATABASE
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

#[derive(Debug, Clone, Copy)]
struct FreeTransform {
    transformed_center: (f32, f32),
    original_center: (f32, f32),
    scale: (f32, f32),
    shear: (f32, f32),
    rotation_z: f32,
}

impl FreeTransform {
    /// Map an output pixel back to its source position.
    fn source_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let (sc, cc) = self.rotation_z.to_radians().sin_cos();
        let (sx, sy) = self.scale;
        let (shx, shy) = self.shear;
        // A = R * S * shear.
        let a11 = cc * sx;
        let a12 = cc * sx * shx - sc * sy;
        let a21 = sc * sx;
        let a22 = sc * sx * shx + cc * sy;
        let det = a11 * a22 - a12 * a21;
        if det.abs() < 1e-6 {
            return None;
        }
        let dx = x - self.transformed_center.0;
        let dy = y - self.transformed_center.1;
        let rx = (a22 * dx - a12 * dy) / det;
        let ry = (-a21 * dx + a11 * dy) / det;
        Some((
            self.original_center.0 + rx + shy * ry,
            self.original_center.1 + ry,
        ))
    }
}

/// Read the first visible free-transform mask of a layer.
fn read_transform_mask(
    zip: &mut zip::ZipArchive<File>,
    layer: Node,
    base: &str,
) -> Option<FreeTransform> {
    let masks = layer.children().find(|n| n.has_tag_name("masks"))?;
    let mask = masks
        .children()
        .filter(|n| n.has_tag_name("mask"))
        .find(|n| {
            n.attribute("nodetype") == Some("transformmask") && n.attribute("visible") != Some("0")
        })?;
    let filename = mask.attribute("filename")?;
    let path = format!("{base}/layers/{filename}.transformconfig");
    let xml = read_zip_string(zip, &path).ok()?;
    parse_transform_config(&xml)
}

fn parse_transform_config(xml: &str) -> Option<FreeTransform> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .ok()?;
    let main = doc.descendants().find(|n| n.has_tag_name("main"))?;
    if main.attribute("id") != Some("tooltransformparams") {
        return None;
    }
    let free = doc
        .descendants()
        .find(|n| n.has_tag_name("free_transform"))?;
    let point = |tag: &str| -> Option<(f32, f32)> {
        let node = free.children().find(|n| n.has_tag_name(tag))?;
        Some((
            node.attribute("x")?.parse().ok()?,
            node.attribute("y")?.parse().ok()?,
        ))
    };
    let value = |tag: &str, default: f32| -> f32 {
        free.children()
            .find(|n| n.has_tag_name(tag))
            .and_then(|n| n.attribute("value"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    // 3D rotations and perspective need a full projective transform.
    if value("aX", 0.0).abs() > 1e-4
        || value("aY", 0.0).abs() > 1e-4
        || value("shearX", 0.0).abs() > 1e-4
        || value("shearY", 0.0).abs() > 1e-4
    {
        return None;
    }
    if let Some(flat) = free
        .children()
        .find(|n| n.has_tag_name("flattenedPerspectiveTransform"))
    {
        for (name, expected) in [
            ("m11", 1.0),
            ("m22", 1.0),
            ("m33", 1.0),
            ("m12", 0.0),
            ("m13", 0.0),
            ("m21", 0.0),
            ("m23", 0.0),
            ("m31", 0.0),
            ("m32", 0.0),
        ] {
            let v: f32 = flat.attribute(name)?.parse().ok()?;
            if (v - expected).abs() > 1e-4 {
                return None;
            }
        }
    }
    Some(FreeTransform {
        transformed_center: point("transformedCenter")?,
        original_center: point("originalCenter")?,
        scale: (value("scaleX", 1.0), value("scaleY", 1.0)),
        shear: (0.0, 0.0),
        rotation_z: value("aZ", 0.0),
    })
}

fn apply_transform(image: Image, transform: Option<FreeTransform>) -> Image {
    let Some(transform) = transform else {
        return image;
    };
    if transform.scale.0.abs() < 1e-4 || transform.scale.1.abs() < 1e-4 {
        return image;
    }
    let mut out = Image::new(image.width, image.height);
    for y in 0..image.height {
        for x in 0..image.width {
            let Some((sx, sy)) = transform.source_point(x as f32 + 0.5, y as f32 + 0.5) else {
                continue;
            };
            let sx = sx - 0.5;
            let sy = sy - 0.5;
            if sx < -1.0 || sy < -1.0 || sx > image.width as f32 || sy > image.height as f32 {
                continue;
            }
            let (r, g, b, a) = sample_bilinear(&image, sx, sy);
            let o = ((y * image.width + x) * 4) as usize;
            out.data[o] = r;
            out.data[o + 1] = g;
            out.data[o + 2] = b;
            out.data[o + 3] = a;
        }
    }
    out
}

fn sample_bilinear(image: &Image, x: f32, y: f32) -> (u8, u8, u8, u8) {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let mut acc = [0f32; 4];
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let px = x0 as i64 + dx;
        let py = y0 as i64 + dy;
        if px < 0 || py < 0 || px >= image.width as i64 || py >= image.height as i64 {
            continue;
        }
        let i = ((py as usize) * image.width as usize + px as usize) * 4;
        let a = image.data[i + 3] as f32 / 255.0 * weight;
        acc[0] += image.data[i] as f32 / 255.0 * a;
        acc[1] += image.data[i + 1] as f32 / 255.0 * a;
        acc[2] += image.data[i + 2] as f32 / 255.0 * a;
        acc[3] += a;
    }
    if acc[3] <= 1e-6 {
        return (0, 0, 0, 0);
    }
    (
        (acc[0] / acc[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        (acc[1] / acc[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        (acc[2] / acc[3] * 255.0).round().clamp(0.0, 255.0) as u8,
        (acc[3] * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// Apply a visible Krita filter mask to a layer image. Only the Desaturate
/// filter is implemented; its `type=1` mode is Rec.709 luminosity, which was
/// checked against the gray rectangle in the Krita nodetypes fixture.
fn apply_filter_mask(
    mut image: Image,
    zip: &mut zip::ZipArchive<File>,
    layer: Node,
    base: &str,
    width: u32,
    height: u32,
) -> Image {
    let Some(masks) = layer.children().find(|n| n.has_tag_name("masks")) else {
        return image;
    };
    for mask in masks.children().filter(|n| n.has_tag_name("mask")) {
        if mask.attribute("nodetype") != Some("filtermask")
            || mask.attribute("visible") == Some("0")
        {
            continue;
        }
        if mask.attribute("filtername") != Some("desaturate") {
            continue;
        }
        let Some(filename) = mask.attribute("filename") else {
            continue;
        };
        let path = format!("{base}/layers/{filename}.filterconfig");
        let Ok(xml) = read_zip_string(zip, &path) else {
            continue;
        };
        let Ok(config) = roxmltree::Document::parse_with_options(
            &xml,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            },
        ) else {
            continue;
        };
        let mode = config
            .descendants()
            .find(|n| n.has_tag_name("param") && n.attribute("name") == Some("type"))
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<i64>().ok())
            .unwrap_or(1);
        if mode != 1 {
            continue;
        }
        let selection =
            read_mask_selection(zip, &format!("{base}/layers/{filename}"), width, height);
        for (i, px) in image.data.chunks_exact_mut(4).enumerate() {
            let t = selection
                .as_ref()
                .map(|s| s.data[i * 4 + 3] as f32 / 255.0)
                .unwrap_or(1.0);
            if t <= 0.0 {
                continue;
            }
            let gray = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
            for c in 0..3 {
                px[c] = (px[c] as f32 * (1.0 - t) + gray * t)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
    image
}

fn read_mask_selection(
    zip: &mut zip::ZipArchive<File>,
    base_path: &str,
    width: u32,
    height: u32,
) -> Option<Image> {
    let candidates = [
        format!("{base_path}.pixelselection"),
        format!("{base_path}.pixel_selection"),
    ];
    for path in candidates {
        if zip.by_name(&path).is_ok() {
            return read_mask(zip, &path, width, height).ok();
        }
    }
    None
}

/// Read the first visible transparency mask of a layer.
///
/// Krita stores masks as `<masks><mask nodetype="transparency" .../></masks>`
/// children of the layer. The raster lives next to the layer files as
/// `<filename>.pixel_selection`, a one-channel tiled image whose value is the
/// layer coverage.
fn read_transparency_mask(
    zip: &mut zip::ZipArchive<File>,
    layer: Node,
    base: &str,
    width: u32,
    height: u32,
) -> Result<Option<(Image, String)>> {
    let Some(masks) = layer.children().find(|n| n.has_tag_name("masks")) else {
        return Ok(None);
    };
    for mask in masks.children().filter(|n| n.has_tag_name("mask")) {
        // Krita writes `transparencymask`; older documents used
        // `transparency`. Both name the same non-destructive mask.
        if !matches!(
            mask.attribute("nodetype"),
            Some("transparencymask") | Some("transparency")
        ) {
            continue;
        }
        if mask.attribute("visible") == Some("0") {
            continue;
        }
        let Some(filename) = mask.attribute("filename") else {
            continue;
        };
        let candidates = [
            format!("{base}/layers/{filename}.pixelselection"),
            format!("{base}/layers/{filename}.pixel_selection"),
            format!("{base}/layers/{filename}"),
        ];
        let Some(path) = candidates.iter().find(|p| zip.by_name(p).is_ok()) else {
            continue;
        };
        let image = read_mask(zip, path, width, height)?;
        return Ok(Some((image, path.clone())));
    }
    Ok(None)
}

fn read_mask(
    zip: &mut zip::ZipArchive<File>,
    path: &str,
    width: u32,
    height: u32,
) -> Result<Image> {
    let data = read_zip_bytes(zip, path)?;
    let default = read_zip_bytes(zip, &format!("{path}.defaultpixel")).ok();
    let mut image = Image::new(width, height);
    if let Some(default) = default.as_deref().filter(|d| !d.is_empty()) {
        let alpha = default[0];
        for p in image.data.chunks_exact_mut(4) {
            p.copy_from_slice(&[0, 0, 0, alpha]);
        }
    }
    decode_tiles_at(&data, &mut image, 1, (0, 0)).with_context(|| format!("decode mask {path}"))?;
    Ok(image)
}

fn decode_tiles_at(
    data: &[u8],
    image: &mut Image,
    pixel_size: usize,
    offset: (i64, i64),
) -> Result<()> {
    let mut pos = 0usize;
    let version = read_line(data, &mut pos)?;
    if !version.starts_with("VERSION ") {
        bail!("not a Krita tiled layer (missing VERSION)");
    }
    let tile_w: usize = key_value(read_line(data, &mut pos)?, "TILEWIDTH")?;
    let tile_h: usize = key_value(read_line(data, &mut pos)?, "TILEHEIGHT")?;
    let stored_pixel_size: usize = key_value(read_line(data, &mut pos)?, "PIXELSIZE")?;
    let tile_count: usize = key_value(read_line(data, &mut pos)?, "DATA")?;
    if stored_pixel_size != pixel_size {
        bail!("expected {pixel_size}-byte Krita tiles, file stores {stored_pixel_size}");
    }
    if pixel_size != 4 && pixel_size != 1 {
        bail!("unsupported Krita tile pixel size {pixel_size}");
    }
    let tile_bytes = tile_w * tile_h * pixel_size;
    for _ in 0..tile_count {
        let line = read_line(data, &mut pos)?;
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != 4 {
            bail!("bad tile header {line:?}");
        }
        let tx: i64 = fields[0].trim().parse()?;
        let ty: i64 = fields[1].trim().parse()?;
        let compression = fields[2].trim();
        let size: usize = fields[3].trim().parse()?;
        if pos + size > data.len() {
            bail!("truncated tile at {tx},{ty}");
        }
        let payload = &data[pos..pos + size];
        pos += size;
        if payload.is_empty() {
            continue;
        }
        let compressed = compression == "LZF" && payload[0] == 1;
        let decoded = if compressed {
            lzf_decompress(&payload[1..], tile_bytes)?
        } else {
            payload[1..].to_vec()
        };
        if decoded.len() < tile_bytes {
            bail!("short tile at {tx},{ty}: {} bytes", decoded.len());
        }
        // LZF tiles store one plane per channel; raw tiles are already
        // interleaved. Both end up interleaved here.
        let raw: Vec<u8> = if compressed && pixel_size == 4 {
            let area = tile_w * tile_h;
            let mut interleaved = vec![0u8; area * 4];
            for i in 0..area {
                for c in 0..4 {
                    interleaved[i * 4 + c] = decoded[c * area + i];
                }
            }
            interleaved
        } else {
            decoded
        };
        for row in 0..tile_h as i64 {
            let y = offset.1 + ty + row;
            if y < 0 || y >= image.height as i64 {
                continue;
            }
            for col in 0..tile_w as i64 {
                let x = offset.0 + tx + col;
                if x < 0 || x >= image.width as i64 {
                    continue;
                }
                let src = ((row as usize) * tile_w + col as usize) * pixel_size;
                let dst = ((y as usize) * image.width as usize + x as usize) * 4;
                if pixel_size == 1 {
                    image.data[dst] = 0;
                    image.data[dst + 1] = 0;
                    image.data[dst + 2] = 0;
                    image.data[dst + 3] = raw[src];
                } else {
                    image.data[dst] = raw[src + 2];
                    image.data[dst + 1] = raw[src + 1];
                    image.data[dst + 2] = raw[src];
                    image.data[dst + 3] = raw[src + 3];
                }
            }
        }
    }
    Ok(())
}

/// Krita's LZF variant. The first control byte is incremented before use and
/// a back-reference points `offset + 1` bytes behind the output cursor.
fn lzf_decompress(src: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut ip = 0usize;
    while ip + 1 < src.len() {
        let control = src[ip] as usize + 1;
        let offset = ((src[ip] as usize) & 31) << 8;
        let mut length = (src[ip] as usize) >> 5;
        ip += 1;
        if control < 33 {
            if ip + control > src.len() {
                bail!("LZF literal overruns input");
            }
            out.extend_from_slice(&src[ip..ip + control]);
            ip += control;
        } else {
            length -= 1;
            if length == 6 {
                length += src[ip] as usize;
                ip += 1;
            }
            let mut reference = out
                .len()
                .checked_sub(offset + 1 + src[ip] as usize)
                .context("LZF back-reference before output")?;
            ip += 1;
            for _ in 0..length + 3 {
                let byte = *out.get(reference).context("LZF copy past output")?;
                out.push(byte);
                reference += 1;
            }
        }
        if out.len() > expected {
            break;
        }
    }
    out.truncate(expected);
    if out.len() < expected {
        bail!("LZF produced {} of {expected} bytes", out.len());
    }
    Ok(out)
}

fn read_line<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a str> {
    let start = *pos;
    let end = data[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map(|i| start + i)
        .unwrap_or(data.len());
    *pos = (end + 1).min(data.len());
    std::str::from_utf8(&data[start..end]).context("non-UTF-8 tile header")
}

fn key_value(line: &str, key: &str) -> Result<usize> {
    let mut parts = line.split_whitespace();
    let name = parts.next().unwrap_or("");
    if name != key {
        bail!("expected {key}, got {line:?}");
    }
    parts
        .next()
        .context("missing value")?
        .parse()
        .context("bad number")
}

fn read_zip_string(zip: &mut zip::ZipArchive<File>, name: &str) -> Result<String> {
    let mut s = String::new();
    zip.by_name(name)?.read_to_string(&mut s)?;
    Ok(s)
}

fn read_zip_bytes(zip: &mut zip::ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    zip.by_name(name)?.read_to_end(&mut data)?;
    Ok(data)
}

fn read_zip_png(zip: &mut zip::ZipArchive<File>, name: &str) -> Result<Image> {
    let data = read_zip_bytes(zip, name)?;
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let mut image = Image::new(info.width, info.height);
    let src = &buf[..info.buffer_size()];
    match info.color_type {
        png::ColorType::Rgba => image.data.copy_from_slice(src),
        png::ColorType::Rgb => {
            for (i, px) in src.chunks_exact(3).enumerate() {
                image.data[i * 4..i * 4 + 3].copy_from_slice(px);
                image.data[i * 4 + 3] = 255;
            }
        }
        other => bail!("unsupported merged PNG colour type {other:?}"),
    }
    Ok(image)
}

fn attr_num(node: &Node, name: &str) -> Option<u32> {
    node.attribute(name)?.parse().ok()
}

fn blend_mode(name: &str) -> i64 {
    match name {
        "normal" => blend::MODE_NORMAL,
        "darken" => blend::MODE_DARKEN,
        "multiply" => blend::MODE_MULTIPLY,
        "burn" | "colorburn" => blend::MODE_COLOR_BURN,
        "linearburn" => blend::MODE_LINEAR_BURN,
        "subtract" => blend::MODE_SUBTRACT,
        "darkercolor" => blend::MODE_DARKER_COLOR,
        "lighten" => blend::MODE_LIGHTEN,
        "screen" => blend::MODE_SCREEN,
        "dodge" | "colordodge" => blend::MODE_COLOR_DODGE,
        "add" => blend::MODE_ADD,
        "lightercolor" => blend::MODE_LIGHTER_COLOR,
        "overlay" => blend::MODE_OVERLAY,
        "softlight" => blend::MODE_SOFT_LIGHT,
        "hardlight" => blend::MODE_HARD_LIGHT,
        "vividlight" => blend::MODE_VIVID_LIGHT,
        "linearlight" => blend::MODE_LINEAR_LIGHT,
        "pinlight" => blend::MODE_PIN_LIGHT,
        "hardmix" => blend::MODE_HARD_MIX,
        "difference" => blend::MODE_DIFFERENCE,
        "exclusion" => blend::MODE_EXCLUSION,
        "hue" => blend::MODE_HUE,
        "saturation" => blend::MODE_SATURATION,
        "color" => blend::MODE_COLOR,
        "luminosity" => blend::MODE_LUMINOSITY,
        "divide" => blend::MODE_DIVIDE,
        _ => blend::MODE_NORMAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> String {
        format!("{}/../../../samples/kra/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn reads_and_composites_a_krita_document() {
        let doc = KraDocument::open(sample("example_uncompressed.kra")).unwrap();
        assert_eq!((doc.width, doc.height), (64, 64));
        assert_eq!(doc.layers.len(), 1);
        let image = doc.composite();
        let merged = doc.merged.expect("mergedimage.png");
        let mut diff = 0f64;
        for (a, b) in image.data.iter().zip(merged.data.iter()) {
            diff += (*a as f64 - *b as f64).abs();
        }
        let mean = diff / image.data.len() as f64;
        assert!(mean < 1.0, "mean channel difference {mean}");
    }

    #[test]
    fn folders_expose_the_krita_group_tree() {
        let doc = KraDocument::open(sample("example_RGBA.kra")).unwrap();
        assert_eq!(doc.folders.len(), 1, "one group in the fixture");
        assert_eq!(doc.folders[0].name, "flower");
        assert_eq!(doc.folders[0].children.len(), 5);
        assert!(matches!(doc.root_children.last(), Some(KraNode::Folder(0))));
        let pistil = doc.layers.iter().position(|l| l.name == "pistil").unwrap();
        assert!(
            doc.folders[0].children.contains(&KraNode::Layer(pistil)),
            "pistil must be a child of the flower group"
        );
    }

    /// Build a small `.kra` in memory: one opaque paint layer and a
    /// transparency mask that hides its right half.
    fn masked_kra(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;

        let path = dir.join("masked.kra");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/x-krita").unwrap();
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="8" x-res="72" y-res="72" name="masked" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer"
          filename="layer0" name="Paint" x="0" y="0">
    <masks>
     <mask visible="1" nodetype="transparencymask" filename="mask0" name="Mask" x="0" y="0"/>
    </masks>
   </layer>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.start_file("maindoc.xml", deflated).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();

        // A 64x64 raw BGRA tile, fully opaque white.
        let mut layer = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n".to_vec();
        let payload_len = 1 + 64 * 64 * 4;
        layer.extend_from_slice(format!("0,0,RAW,{payload_len}\n").as_bytes());
        layer.push(0);
        for _ in 0..64 * 64 {
            layer.extend_from_slice(&[255, 255, 255, 255]);
        }
        zip.start_file("masked/layers/layer0", deflated).unwrap();
        zip.write_all(&layer).unwrap();

        // A one-channel pixel selection: left half selected, right half not.
        let mut alpha = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 1\nDATA 1\n".to_vec();
        let payload_len = 1 + 64 * 64;
        alpha.extend_from_slice(format!("0,0,RAW,{payload_len}\n").as_bytes());
        alpha.push(0);
        for _y in 0..64 {
            for x in 0..64 {
                alpha.push(if x < 4 { 255 } else { 0 });
            }
        }
        zip.start_file("masked/layers/mask0.pixelselection", deflated)
            .unwrap();
        zip.write_all(&alpha).unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn write_back_updates_pixels_and_masks_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = masked_kra(dir.path());
        let doc = KraDocument::open(&path).unwrap();
        let mut image = doc.layers[0].image.clone();
        image.data[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let mut mask = doc.layers[0].mask.clone().expect("mask");
        for (i, px) in mask.data.chunks_exact_mut(4).enumerate() {
            px[3] = if i % 8 >= 4 { 255 } else { 0 };
        }
        let merged = doc.composite();
        write_back(
            &path,
            &[KraLayerEdit {
                filename: "layer0",
                name: None,
                data_path: Some("masked/layers/layer0"),
                mask_path: Some("masked/layers/mask0.pixelselection"),
                image: ImageRef {
                    width: image.width,
                    height: image.height,
                    data: &image.data,
                },
                mask: Some(ImageRef {
                    width: mask.width,
                    height: mask.height,
                    data: &mask.data,
                }),
                offset: (0, 0),
                opacity: Some(1.0),
                visible: Some(true),
                composite: Some("normal"),
            }],
            &merged,
        )
        .unwrap();
        let reopened = KraDocument::open(&path).unwrap();
        assert_eq!(&reopened.layers[0].image.data[0..4], &[255, 0, 0, 255]);
        let mask = reopened.layers[0].mask.as_ref().expect("mask survives");
        assert_eq!(
            &mask.data[0..4],
            &[0, 0, 0, 0],
            "left half must stay hidden"
        );
        assert_eq!(
            &mask.data[7 * 4..7 * 4 + 4],
            &[0, 0, 0, 255],
            "right half must be revealed"
        );
    }

    #[test]
    fn write_back_preserves_groups_and_updates_layer_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edited.kra");
        std::fs::copy(sample("example_RGBA.kra"), &path).unwrap();
        let doc = KraDocument::open(&path).unwrap();
        let pistil = doc.layers.iter().position(|l| l.name == "pistil").unwrap();
        let mut image = doc.layers[pistil].image.clone();
        image.data[0..4].copy_from_slice(&[255, 0, 255, 255]);
        let merged = doc.composite();
        write_back(
            &path,
            &[KraLayerEdit {
                filename: "layer3",
                name: None,
                data_path: Some("Sample/layers/layer3"),
                mask_path: None,
                image: ImageRef {
                    width: image.width,
                    height: image.height,
                    data: &image.data,
                },
                mask: None,
                offset: (0, 0),
                opacity: Some(0.5),
                visible: Some(true),
                composite: Some("multiply"),
            }],
            &merged,
        )
        .unwrap();
        let reopened = KraDocument::open(&path).unwrap();
        let pistil = reopened.layers.iter().find(|l| l.name == "pistil").unwrap();
        assert_eq!(&pistil.image.data[0..4], &[255, 0, 255, 255]);
        assert!(
            (pistil.opacity - 0.5).abs() < 0.01,
            "opacity {}",
            pistil.opacity
        );
        assert_eq!(pistil.blend_mode, blend::MODE_MULTIPLY);
        let file = File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let xml = read_zip_string(&mut zip, "maindoc.xml").unwrap();
        assert!(xml.contains("grouplayer"), "group node must survive");
        assert!(
            zip.by_name("Sample/layers/layer4").is_ok(),
            "sibling layer data must survive"
        );
        assert!(zip.by_name("mergedimage.png").is_ok());
    }

    /// A paint layer whose Krita `x`/`y` offsets move its tile data.
    fn offset_kra(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;

        let path = dir.join("offset.kra");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/x-krita").unwrap();
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="8" x-res="72" y-res="72" name="offset" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer"
          filename="layer0" name="Paint" x="4" y="1"/>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.start_file("maindoc.xml", stored).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        let mut layer = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n".to_vec();
        let payload_len = 1 + 64 * 64 * 4;
        layer.extend_from_slice(format!("0,0,RAW,{payload_len}\n").as_bytes());
        layer.push(0);
        for _ in 0..64 * 64 {
            layer.extend_from_slice(&[255, 255, 255, 255]);
        }
        zip.start_file("offset/layers/layer0", stored).unwrap();
        zip.write_all(&layer).unwrap();
        zip.finish().unwrap();
        path
    }

    /// A clone layer that points at a paint layer by UUID.
    fn clone_kra(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;

        let path = dir.join("clone.kra");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/x-krita").unwrap();
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="8" x-res="72" y-res="72" name="clone" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="clonelayer"
          filename="layer1" name="Clone" x="4" y="4" uuid="{clone}"
          clonefromuuid="{source}"/>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer"
          filename="layer0" name="Source" x="1" y="1" uuid="{source}"/>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.start_file("maindoc.xml", stored).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        let mut layer = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n".to_vec();
        let payload_len = 1 + 64 * 64 * 4;
        layer.extend_from_slice(format!("0,0,RAW,{payload_len}\n").as_bytes());
        layer.push(0);
        for i in 0..64 * 64 {
            // One opaque source pixel at the layer origin.
            let px = if i == 0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            layer.extend_from_slice(&px);
        }
        zip.start_file("clone/layers/layer0", stored).unwrap();
        zip.write_all(&layer).unwrap();
        zip.finish().unwrap();
        path
    }

    /// A paint layer with a vertical flip transform mask.
    fn transform_kra(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;

        let path = dir.join("transform.kra");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/x-krita").unwrap();
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="8" x-res="72" y-res="72" name="transform" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer"
          filename="layer0" name="Paint" x="0" y="0" uuid="{paint}">
    <masks>
     <mask visible="1" nodetype="transformmask" filename="mask0" name="Flip" x="0" y="0"/>
    </masks>
   </layer>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.start_file("maindoc.xml", stored).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        let mut layer = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n".to_vec();
        let payload_len = 1 + 64 * 64 * 4;
        layer.extend_from_slice(format!("0,0,RAW,{payload_len}\n").as_bytes());
        layer.push(0);
        for y in 0..64 {
            for _ in 0..64 {
                let px = if y < 4 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 0]
                };
                layer.extend_from_slice(&px);
            }
        }
        zip.start_file("transform/layers/layer0", stored).unwrap();
        zip.write_all(&layer).unwrap();
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
        zip.start_file("transform/layers/mask0.transformconfig", stored)
            .unwrap();
        zip.write_all(config.as_bytes()).unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn transform_mask_flips_the_layer() {
        let dir = tempfile::tempdir().unwrap();
        let doc = KraDocument::open(transform_kra(dir.path())).unwrap();
        let image = doc.composite();
        let alpha = |x: usize, y: usize| image.data[(y * 8 + x) * 4 + 3];
        assert_eq!(alpha(2, 1), 0, "the source top half must move down");
        assert!(
            alpha(2, 6) > 200,
            "the flipped pixels must land in the bottom half"
        );
    }

    /// A red paint layer with a Desaturate filter mask.
    fn desaturate_kra(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;

        let path = dir.join("desaturate.kra");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/x-krita").unwrap();
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DOC xmlns="http://www.calligra.org/DTD/krita" syntaxVersion="2.0">
 <IMAGE width="8" height="8" x-res="72" y-res="72" name="desaturate" colorspacename="RGBA">
  <layers>
   <layer visible="1" opacity="255" compositeop="normal" nodetype="paintlayer"
          filename="layer0" name="Paint" x="0" y="0">
    <masks>
     <mask visible="1" nodetype="filtermask" filtername="desaturate"
           filename="mask0" name="Desaturate" x="0" y="0"/>
    </masks>
   </layer>
  </layers>
 </IMAGE>
</DOC>"#;
        zip.start_file("maindoc.xml", stored).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        let mut layer = b"VERSION 2\nTILEWIDTH 64\nTILEHEIGHT 64\nPIXELSIZE 4\nDATA 1\n".to_vec();
        let payload_len = 1 + 64 * 64 * 4;
        layer.extend_from_slice(format!("0,0,RAW,{payload_len}\n").as_bytes());
        layer.push(0);
        for _ in 0..64 * 64 {
            layer.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
        }
        zip.start_file("desaturate/layers/layer0", stored).unwrap();
        zip.write_all(&layer).unwrap();
        let config = r#"<!DOCTYPE params>
<params version="0">
 <param type="string" name="type"><![CDATA[1]]></param>
</params>"#;
        zip.start_file("desaturate/layers/mask0.filterconfig", stored)
            .unwrap();
        zip.write_all(config.as_bytes()).unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn desaturate_filter_mask_turns_a_red_layer_gray() {
        let dir = tempfile::tempdir().unwrap();
        let doc = KraDocument::open(desaturate_kra(dir.path())).unwrap();
        let image = doc.composite();
        let px = &image.data[(2 * 8 + 2) * 4..(2 * 8 + 2) * 4 + 4];
        assert_eq!((px[0], px[1], px[2]), (54, 54, 54));
        assert_eq!(px[3], 255);
    }

    #[test]
    fn clone_layers_repeat_the_source_at_their_offset() {
        let dir = tempfile::tempdir().unwrap();
        let doc = KraDocument::open(clone_kra(dir.path())).unwrap();
        assert_eq!(doc.layers.len(), 2);
        let image = doc.composite();
        let alpha = |x: usize, y: usize| image.data[(y * 8 + x) * 4 + 3];
        assert_eq!(alpha(1, 1), 255, "source pixel");
        assert_eq!(alpha(4, 4), 255, "clone pixel");
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(3, 3), 0);
    }

    #[test]
    fn paint_layer_offsets_move_the_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let doc = KraDocument::open(offset_kra(dir.path())).unwrap();
        let image = doc.composite();
        let alpha = |x: usize, y: usize| image.data[(y * 8 + x) * 4 + 3];
        assert_eq!(alpha(4, 1), 255, "the offset origin must be opaque");
        assert_eq!(alpha(0, 0), 0, "canvas outside the layer must stay empty");
    }

    #[test]
    fn transparency_mask_hides_layer_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let doc = KraDocument::open(masked_kra(dir.path())).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert!(doc.layers[0].mask.is_some(), "mask was not decoded");
        let image = doc.composite();
        let left = image.data[(2 * 8 + 1) as usize * 4 + 3];
        let right = image.data[(2 * 8 + 6) as usize * 4 + 3];
        assert_eq!(left, 255, "left half should stay visible");
        assert_eq!(right, 0, "right half should be masked out");
    }

    #[test]
    fn reads_a_grouped_krita_document() {
        let doc = KraDocument::open(sample("example_RGBA.kra")).unwrap();
        assert_eq!((doc.width, doc.height), (256, 128));
        assert!(
            doc.layers.len() >= 3,
            "expected paint layers, got {}",
            doc.layers.len()
        );
        let image = doc.composite();
        let merged = doc.merged.expect("mergedimage.png");
        let mut diff = 0f64;
        for (a, b) in image.data.iter().zip(merged.data.iter()) {
            diff += (*a as f64 - *b as f64).abs();
        }
        let mean = diff / image.data.len() as f64;
        assert!(
            mean < 1.0,
            "grouped composite differs from merged by {mean}"
        );
    }

    #[test]
    fn renders_a_vector_shape_layer() {
        let doc = KraDocument::open(sample("example_vector.kra")).unwrap();
        assert_eq!((doc.width, doc.height), (1024, 1024));
        let vector = doc
            .layers
            .iter()
            .find(|l| l.name == "Vector Layer 1")
            .expect("vector layer is missing");
        assert!(
            vector.image.data.chunks_exact(4).any(|p| p[3] > 0),
            "vector layer rendered empty"
        );
        let image = doc.composite();
        let merged = doc.merged.expect("mergedimage.png");
        let mut diff = 0f64;
        for (a, b) in image.data.iter().zip(merged.data.iter()) {
            diff += (*a as f64 - *b as f64).abs();
        }
        let mean = diff / image.data.len() as f64;
        assert!(mean < 1.0, "vector composite differs from merged by {mean}");
    }
}
