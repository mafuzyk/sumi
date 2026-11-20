//! Metadata model over the embedded SQLite database.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use rusqlite::Connection;

use crate::attr::Attr;
use crate::chunk::{self, Container};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasUnit {
    Px,
    Cm,
    Mm,
    Inch,
    Unknown(i64),
}

impl CanvasUnit {
    fn from_i64(v: i64) -> Self {
        match v {
            0 => CanvasUnit::Px,
            1 => CanvasUnit::Cm,
            2 => CanvasUnit::Mm,
            3 => CanvasUnit::Inch,
            n => CanvasUnit::Unknown(n),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Canvas {
    pub id: i64,
    pub unit: CanvasUnit,
    pub width: f64,
    pub height: f64,
    pub resolution: f64,
    pub root_folder: i64,
    pub current_layer: i64,
    pub channel_bytes: i64,
    pub channel_order: i64,
}

impl Canvas {
    pub fn pixel_size(&self) -> (u32, u32) {
        let (w, h) = match self.unit {
            CanvasUnit::Px => (self.width, self.height),
            CanvasUnit::Mm => (self.width * self.resolution / 25.4, self.height * self.resolution / 25.4),
            CanvasUnit::Cm => (self.width * self.resolution / 2.54, self.height * self.resolution / 2.54),
            CanvasUnit::Inch => (self.width * self.resolution, self.height * self.resolution),
            CanvasUnit::Unknown(_) => (self.width, self.height),
        };
        (w.round().max(1.0) as u32, h.round().max(1.0) as u32)
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub main_id: i64,
    pub name: String,
    pub kind: i64,
    pub opacity: i64,
    pub composite: i64,
    pub visibility: i64,
    pub clip: i64,
    /// Raw `LayerLock` bit field.
    pub lock_flags: i64,
    /// `DraftLayer` column when present.
    pub draft_layer: i64,
    pub folder: i64,
    pub next: i64,
    pub first_child: i64,
    pub offset_x: i64,
    pub offset_y: i64,
    pub render_offset_x: i64,
    pub render_offset_y: i64,
    pub mask_offset_x: i64,
    pub mask_offset_y: i64,
    pub render_mipmap: i64,
    pub mask_mipmap: i64,
    pub draw_to_render_type: Option<i64>,
    /// Raw `FilterLayerInfo` BLOB of an adjustment layer.
    pub filter_layer_info: Option<Vec<u8>>,
    /// `Layer.SpecialRenderType` (14 = comic frame border).
    pub special_render_type: i64,
    /// `Layer.DrawColorMain*` when the layer carries a draw colour.
    pub draw_color: Option<[u8; 3]>,
}

impl Layer {
    pub fn is_folder(&self) -> bool {
        self.folder & 1 != 0
    }
    pub fn is_visible(&self) -> bool {
        self.visibility & 1 != 0
    }
    pub fn is_paper(&self) -> bool {
        self.kind == 1584
    }
    pub fn is_root(&self) -> bool {
        self.kind == 256
    }
    pub fn is_adjustment(&self) -> bool {
        self.kind & 4096 != 0
    }
    pub fn is_raster(&self) -> bool {
        !self.is_folder() && !self.is_adjustment() && !self.is_paper() && !self.is_root()
    }
    /// CSP stores opacity 0..=256, not 0..=255.
    pub fn alpha(&self) -> f32 {
        (self.opacity.clamp(0, 256) as f32) / 256.0
    }
}

/// CSP packs `DrawColorMain*` as a byte replicated into all four positions of
/// the u32 (0x3E3E3E3E is channel 62); plain 0..=255 values also occur.
fn draw_color(r: i64, g: i64, b: i64) -> Option<[u8; 3]> {
    if r == 0 && g == 0 && b == 0 {
        return None;
    }
    let ch = |v: i64| -> u8 {
        let v = v as u64;
        if v > 255 {
            (v & 0xFF) as u8
        } else {
            v as u8
        }
    };
    Some([ch(r), ch(g), ch(b)])
}

#[derive(Debug, Clone)]
pub struct MipLevel {
    pub main_id: i64,
    pub scale: f64,
    pub offscreen: i64,
}

#[derive(Debug, Clone)]
pub struct Offscreen {
    pub main_id: i64,
    pub layer_id: i64,
    pub attribute: Attr,
    pub external_id: Option<String>,
}

pub struct ClipFile {
    pub path: PathBuf,
    pub data: Mmap,
    pub container: Container,
    pub db: Connection,
    pub canvas: Canvas,
    pub layers: HashMap<i64, Layer>,
    _tmp: tempfile::TempDir,
    pub db_path: PathBuf,
}

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    let n: i64 = db.query_row(
        "select count(*) from sqlite_master where type='table' and name=?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn column_set(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut st = db.prepare(&format!("pragma table_info({table})"))?;
    let rows = st.query_map([], |r| r.get::<_, String>(1))?;
    let mut v = Vec::new();
    for r in rows {
        v.push(r?);
    }
    Ok(v)
}

impl ClipFile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let f = std::fs::File::open(&path)?;
        let data = unsafe { Mmap::map(&f)? };
        let container = chunk::parse(&data)?;
        let sqlite = &data[container.sqlite.clone()];
        let tmp = tempfile::Builder::new().prefix("clipdb-").tempdir()?;
        let db_path = tmp.path().join("meta.sqlite");
        {
            let mut out = std::fs::File::create(&db_path)?;
            out.write_all(sqlite)?;
            out.flush()?;
        }
        let db = Connection::open(&db_path)?;
        // Read-only semantics for the modelling layer.
        db.pragma_update(None, "query_only", true)?;

        let (id, unit, width, height, resolution, root_folder, current_layer, channel_bytes, channel_order) = db.query_row(
            "select MainId, CanvasUnit, CanvasWidth, CanvasHeight, CanvasResolution, CanvasRootFolder, CanvasCurrentLayer, CanvasChannelBytes, CanvasDefaultChannelOrder from Canvas limit 1",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(7)?.unwrap_or(1),
                    r.get::<_, Option<i64>>(8)?.unwrap_or(33),
                ))
            },
        )?;
        let canvas = Canvas {
            id,
            unit: CanvasUnit::from_i64(unit),
            width,
            height,
            resolution,
            root_folder,
            current_layer,
            channel_bytes,
            channel_order,
        };

        let mut layers = HashMap::new();
        if has_table(&db, "Layer")? {
            let cols = column_set(&db, "Layer")?;
            let want = [
                ("MainId", "main"),
                ("LayerName", "name"),
                ("LayerType", "kind"),
                ("LayerOpacity", "opacity"),
                ("LayerComposite", "composite"),
                ("LayerVisibility", "visibility"),
                ("LayerClip", "clip"),
                ("LayerLock", "lock"),
                ("DraftLayer", "draft"),
                ("LayerFolder", "folder"),
                ("LayerNextIndex", "next"),
                ("LayerFirstChildIndex", "first"),
                ("LayerOffsetX", "ox"),
                ("LayerOffsetY", "oy"),
                ("LayerRenderOffscrOffsetX", "rox"),
                ("LayerRenderOffscrOffsetY", "roy"),
                ("LayerMaskOffsetX", "mox"),
                ("LayerMaskOffsetY", "moy"),
                ("LayerRenderMipmap", "rmip"),
                ("LayerLayerMaskMipmap", "mmip"),
                ("DrawToRenderOffscreenType", "drawtype"),
                ("FilterLayerInfo", "filter"),
                ("DrawColorMainRed", "dcr"),
                ("DrawColorMainGreen", "dcg"),
                ("DrawColorMainBlue", "dcb"),
                ("SpecialRenderType", "srt"),
            ];
            let mut sel = Vec::new();
            for (c, _) in &want {
                if cols.iter().any(|x| x == c) {
                    sel.push(*c);
                } else {
                    sel.push("NULL");
                }
            }
            let sql = format!("select {} from Layer", sel.join(","));
            let mut st = db.prepare(&sql)?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let main: i64 = r.get(0)?;
                let name: Option<String> = r.get(1)?;
                layers.insert(
                    main,
                    Layer {
                        main_id: main,
                        name: name.unwrap_or_default(),
                        kind: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        opacity: r.get::<_, Option<i64>>(3)?.unwrap_or(256),
                        composite: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        visibility: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                        clip: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                        lock_flags: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                        draft_layer: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                        folder: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                        next: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                        first_child: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
                        offset_x: r.get::<_, Option<i64>>(12)?.unwrap_or(0),
                        offset_y: r.get::<_, Option<i64>>(13)?.unwrap_or(0),
                        render_offset_x: r.get::<_, Option<i64>>(14)?.unwrap_or(0),
                        render_offset_y: r.get::<_, Option<i64>>(15)?.unwrap_or(0),
                        mask_offset_x: r.get::<_, Option<i64>>(16)?.unwrap_or(0),
                        mask_offset_y: r.get::<_, Option<i64>>(17)?.unwrap_or(0),
                        render_mipmap: r.get::<_, Option<i64>>(18)?.unwrap_or(0),
                        mask_mipmap: r.get::<_, Option<i64>>(19)?.unwrap_or(0),
                        draw_to_render_type: r.get(20)?,
                        filter_layer_info: r.get(21)?,
                        special_render_type: r.get::<_, Option<i64>>(25)?.unwrap_or(0),
                        draw_color: draw_color(
                            r.get::<_, Option<i64>>(22)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(23)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(24)?.unwrap_or(0),
                        ),
                    },
                );
            }
        }

        Ok(ClipFile {
            path,
            data,
            container,
            db,
            canvas,
            layers,
            _tmp: tmp,
            db_path,
        })
    }

    pub fn project_version(&self) -> Result<String> {
        Ok(self
            .db
            .query_row("select ProjectInternalVersion from Project limit 1", [], |r| {
                r.get::<_, Option<String>>(0)
            })?
            .unwrap_or_default())
    }

    pub fn project_name(&self) -> String {
        self.db
            .query_row("select ProjectName from Project limit 1", [], |r| r.get::<_, Option<String>>(0))
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Number of layers that can own a paint surface (the editor's memory
    /// estimate uses this).
    pub fn paint_layer_count(&self) -> Result<usize> {
        Ok(self
            .layers
            .values()
            .filter(|l| {
                !l.is_folder() && !l.is_adjustment() && !l.is_paper() && !l.is_root()
                    && l.render_mipmap != 0
            })
            .count())
    }

    /// Children of a folder, bottom-most first.
    pub fn children(&self, folder: i64) -> Vec<i64> {
        let mut out = Vec::new();
        let Some(l) = self.layers.get(&folder) else { return out };
        let mut cur = l.first_child;
        let mut guard = 0;
        while cur != 0 && guard < 100_000 {
            let Some(child) = self.layers.get(&cur) else { break };
            out.push(cur);
            cur = child.next;
            guard += 1;
        }
        out
    }

    pub fn mip_chain(&self, mipmap_main: i64) -> Result<Vec<MipLevel>> {
        if mipmap_main == 0 || !has_table(&self.db, "Mipmap")? {
            return Ok(Vec::new());
        }
        let row: Option<(i64, i64)> = self
            .db
            .query_row(
                "select MipmapCount, BaseMipmapInfo from Mipmap where MainId=?1",
                [mipmap_main],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((count, mut cur)) = row else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for _ in 0..count.max(0) {
            let r: Option<(f64, i64, i64)> = self
                .db
                .query_row(
                    "select ThisScale, Offscreen, NextIndex from MipmapInfo where MainId=?1",
                    [cur],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let Some((scale, offscreen, next)) = r else { break };
            out.push(MipLevel { main_id: cur, scale, offscreen });
            if next == 0 {
                break;
            }
            cur = next;
        }
        Ok(out)
    }

    /// Best mip level for a requested scale (percent, 100 = full size).
    pub fn mip_at<'a>(&self, chain: &'a [MipLevel], scale: f64) -> Option<&'a MipLevel> {
        chain
            .iter()
            .filter(|m| m.scale >= scale - 1e-6)
            .min_by(|a, b| a.scale.partial_cmp(&b.scale).unwrap())
            .or_else(|| chain.iter().max_by(|a, b| a.scale.partial_cmp(&b.scale).unwrap()))
    }

    pub fn offscreen(&self, main_id: i64) -> Result<Option<Offscreen>> {
        if !has_table(&self.db, "Offscreen")? {
            return Ok(None);
        }
        let cols = column_set(&self.db, "Offscreen")?;
        let block_expr = if cols.iter().any(|c| c == "BlockData") { "cast(BlockData as text)" } else { "NULL" };
        let sql = format!(
            "select MainId, LayerId, Attribute, {block_expr} from Offscreen where MainId=?1"
        );
        let row: Option<(i64, i64, Vec<u8>, Option<String>)> = self
            .db
            .query_row(&sql, [main_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        let Some((main, layer, blob, eid)) = row else { return Ok(None) };
        let attribute = Attr::decode(&blob)?;
        Ok(Some(Offscreen { main_id: main, layer_id: layer, attribute, external_id: eid }))
    }

    pub fn external_payload(&self, id: &str) -> Option<&[u8]> {
        let idx = *self.container.by_id.get(id)?;
        let range = self.container.exta[idx].data.clone();
        Some(&self.data[range])
    }

    /// True when the layer carries text content (`Layer.TextLayerString`).
    pub fn has_text_layer(&self, layer_id: i64) -> bool {
        if column_set(&self.db, "Layer")
            .map(|c| !c.iter().any(|x| x == "TextLayerString"))
            .unwrap_or(true)
        {
            return false;
        }
        self.db
            .query_row(
                "select length(coalesce(TextLayerString, x'')) from Layer where MainId=?1",
                [layer_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// Decode an editable text layer, if `layer_id` carries text source.
    pub fn text_layer(&self, layer_id: i64) -> Result<Option<crate::text::TextLayer>> {
        let columns = column_set(&self.db, "Layer")?;
        if !columns.iter().any(|c| c == "TextLayerString") {
            return Ok(None);
        }
        let row = self
            .db
            .query_row(
                "select cast(TextLayerString as text), TextLayerAttributes, \
                 TextLayerStringArray, TextLayerAttributesArray \
                 from Layer where MainId=?1",
                [layer_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<Vec<u8>>>(1)?,
                        r.get::<_, Option<Vec<u8>>>(2)?,
                        r.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .ok();
        let Some((text, attributes, strings, styles)) = row else {
            return Ok(None);
        };
        let text = text.unwrap_or_default();
        if text.is_empty() && strings.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
            return Ok(None);
        }
        Ok(Some(crate::text::parse_layer(
            &text,
            attributes.as_deref(),
            strings.as_deref(),
            styles.as_deref(),
        )))
    }

    /// Parse a layer's vector strokes, if it has any.
    pub fn vector_objects(&self, layer_id: i64) -> Result<Vec<crate::vector::VectorObject>> {
        if !has_table(&self.db, "VectorObjectList")? {
            return Ok(Vec::new());
        }
        let row: Option<String> = self
            .db
            .query_row(
                "select cast(VectorData as text) from VectorObjectList where LayerId=?1 limit 1",
                [layer_id],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let Some(id) = row else { return Ok(Vec::new()) };
        let Some(payload) = self.external_payload(&id) else { return Ok(Vec::new()) };
        Ok(crate::vector::parse(payload))
    }

    /// Decode a surface at its native size to straight RGBA8.
    pub fn decode_surface(&self, off: &Offscreen) -> Result<Vec<u8>> {
        let Some(id) = &off.external_id else {
            return Ok(crate::codec::decode_offscreen(&[], &off.attribute).unwrap_or_else(|_| {
                let mut v = vec![0u8; off.attribute.width as usize * off.attribute.height as usize * 4];
                if off.attribute.init.has_color {
                    let c = off.attribute.init.rgba;
                    let rgba = [(c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8];
                    for px in v.chunks_exact_mut(4) {
                        px.copy_from_slice(&rgba);
                    }
                }
                v
            }));
        };
        let Some(payload) = self.external_payload(id) else {
            let mut v = vec![0u8; off.attribute.width as usize * off.attribute.height as usize * 4];
            if off.attribute.init.has_color {
                let c = off.attribute.init.rgba;
                let rgba = [(c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8];
                for px in v.chunks_exact_mut(4) {
                    px.copy_from_slice(&rgba);
                }
            }
            return Ok(v);
        };
        crate::codec::decode_offscreen(payload, &off.attribute)
    }

    pub fn preview_png(&self) -> Option<Vec<u8>> {
        self.db
            .query_row(
                "select ImageData from CanvasPreview where ImageData is not null limit 1",
                [],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok()
    }

    pub fn external_columns(&self) -> Result<Vec<(String, String)>> {
        if !has_table(&self.db, "ExternalTableAndColumnName")? {
            return Ok(Vec::new());
        }
        let mut st = self
            .db
            .prepare("select TableName, ColumnName from ExternalTableAndColumnName")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn external_table(&self) -> Result<Vec<(String, u64)>> {
        if !has_table(&self.db, "ExternalChunk")? {
            return Ok(Vec::new());
        }
        let mut st = self.db.prepare("select ExternalID, Offset from ExternalChunk")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Bytes of the source file, for byte-identical round-trips.
    pub fn source(&self) -> &[u8] {
        &self.data
    }

    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut ids: HashMap<&str, usize> = HashMap::new();
        for c in &self.container.exta {
            *ids.entry(c.id.as_str()).or_default() += 1;
            let tag = &self.data[c.offset..c.offset + 8];
            if tag != chunk::EXTA_TAG {
                problems.push(format!("{} does not point at CHNKExta", c.id));
            }
        }
        for (id, n) in ids {
            if n > 1 {
                problems.push(format!("duplicate external id {id}"));
            }
        }
        match self.external_table() {
            Ok(rows) => {
                for (id, off) in rows {
                    if off as usize + 8 > self.data.len() || &self.data[off as usize..off as usize + 8] != chunk::EXTA_TAG {
                        problems.push(format!("ExternalChunk {id} -> {off} is not a chunk"));
                    }
                    if !self.container.by_id.contains_key(&id) {
                        problems.push(format!("ExternalChunk {id} has no CHNKExta"));
                    }
                }
            }
            Err(e) => problems.push(format!("ExternalChunk: {e}")),
        }
        if !self.container.sqlite.start < self.data.len() {
            problems.push("SQLite chunk outside file".into());
        }
        problems
    }

    /// Group rows that carry no render mipmap.
    ///
    /// CLIP STUDIO PAINT writes a render mipmap (and a thumbnail) for every
    /// group it creates; documents where this application created the group
    /// have neither, and CSP has crashed opening one of those. Reported as a
    /// note, not a validation failure, until the cause is confirmed.
    pub fn groups_without_render_mipmap(&self) -> Vec<&Layer> {
        self.layers
            .values()
            .filter(|layer| layer.kind == 0 && layer.render_mipmap == 0)
            .collect()
    }

    /// Number of CHNKExta chunks that are not referenced by ExternalChunk.
    pub fn unreferenced_chunks(&self) -> Vec<String> {
        let mut referenced = std::collections::HashSet::new();
        if let Ok(rows) = self.external_table() {
            for (id, _) in rows {
                referenced.insert(id);
            }
        }
        self.container
            .exta
            .iter()
            .filter(|c| !referenced.contains(&c.id))
            .map(|c| c.id.clone())
            .collect()
    }
}
