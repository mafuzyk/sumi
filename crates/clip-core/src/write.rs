//! Writer: layer attributes, pixels, preview refresh and container rebuild.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::attr::Attr;
use crate::chunk::{self, Container};
use crate::codec;
use crate::model::ClipFile;
use crate::{Error, Result};

/// Minimal CSP document used as the schema template for new files.
const BLANK_TEMPLATE: &[u8] = include_bytes!("../assets/blank.clip");

/// SQL literal used when cloning a row with overridden columns.
enum Lit {
    Int(i64),
    Text(String),
    Null,
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

pub struct ClipWriter {
    pub source: Vec<u8>,
    pub container: Container,
    pub db: Connection,
    pub payloads: HashMap<String, Vec<u8>>,
    pub order: Vec<String>,
    pub identifier: Vec<u8>,
    pub dirty: bool,
    _tmp: tempfile::TempDir,
}

impl ClipWriter {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let source = std::fs::read(path)?;
        let container = chunk::parse(&source)?;
        let tmp = tempfile::Builder::new().prefix("clipwr-").tempdir()?;
        let db_path = tmp.path().join("meta.sqlite");
        std::fs::write(&db_path, &source[container.sqlite.clone()])?;
        let db = Connection::open(&db_path)?;
        db.pragma_update(None, "journal_mode", "DELETE")?;
        let mut payloads = HashMap::new();
        for c in &container.exta {
            payloads.insert(c.id.clone(), source[c.data.clone()].to_vec());
        }
        let order = container.exta.iter().map(|c| c.id.clone()).collect();
        let identifier = container.head.identifier.clone();
        Ok(ClipWriter {
            source,
            container,
            db,
            payloads,
            order,
            identifier,
            dirty: false,
            _tmp: tmp,
        })
    }

    /// Build a writer for a brand-new document from the bundled minimal
    /// `.clip` template.
    pub fn blank(width: u32, height: u32) -> Result<Self> {
        Self::from_template(BLANK_TEMPLATE, width, height)
    }

    /// Build a writer for a brand-new document from a minimal `.clip`
    /// template (one paper layer and one raster layer), resizing the canvas
    /// and every mip level to `width` x `height`.
    ///
    /// A native new-document path is still out of reach: CSP's SQLite schema
    /// has hundreds of interdependent rows. Cloning a blank CSP document and
    /// rewriting its canvas geometry keeps every table consistent while the
    /// editor supplies the real layer pixels and names.
    pub fn from_template(template: &[u8], width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::format("canvas size must be positive"));
        }
        let tmp = tempfile::Builder::new().prefix("cliptpl-").tempdir()?;
        let path = tmp.path().join("blank.clip");
        std::fs::write(&path, template)?;
        let mut writer = Self::load(&path)?;
        writer.resize_blank(width, height)?;
        Ok(writer)
    }

    fn resize_blank(&mut self, width: u32, height: u32) -> Result<()> {
        let layers: Vec<i64> = {
            let mut st = self.db.prepare(
                "select MainId from Layer where coalesce(LayerRenderMipmap,0)<>0",
            )?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<i64>>>()?
        };
        for layer in layers {
            for (scale, offscreen) in self.mip_offscreens(layer)? {
                // MipmapInfo.ThisScale is a percentage (100, 50, 25, ...).
                let fraction = scale / 100.0;
                let w = ((width as f64 * fraction).round() as u32).max(1);
                let h = ((height as f64 * fraction).round() as u32).max(1);
                self.resize_surface(offscreen, w, h)?;
            }
        }
        self.db.execute(
            "update Canvas set CanvasWidth=?1, CanvasHeight=?2",
            rusqlite::params![width as f64, height as f64],
        )?;
        self.db.execute(
            "update LayerThumbnail set ThumbnailCanvasWidth=?1, ThumbnailCanvasHeight=?2, \
             ThumbnailSmallerNeedRefresh=1, ThumbnailSmallNeedRefresh=1, \
             ThumbnailMiddleNeedRefresh=1, ThumbnailLargeNeedRefresh=1, \
             ThumbnailLargerNeedRefresh=1, ThumbnailMiddle2xNeedRefresh=1, \
             ThumbnailLarger2xNeedRefresh=1, ThumbnailSmallerNeedRefresh1=1, \
             ThumbnailSmallNeedRefresh1=1, ThumbnailMiddleNeedRefresh1=1, \
             ThumbnailLargeNeedRefresh1=1, ThumbnailLargerNeedRefresh1=1, \
             ThumbnailMiddle2xNeedRefresh1=1, ThumbnailLarger2xNeedRefresh1=1",
            rusqlite::params![width, height],
        )?;
        self.dirty = true;
        Ok(())
    }

    fn resize_surface(&mut self, offscreen: i64, width: u32, height: u32) -> Result<()> {
        let blob: Vec<u8> = self.db.query_row(
            "select Attribute from Offscreen where MainId=?1",
            [offscreen],
            |r| r.get(0),
        )?;
        let mut attr = Attr::decode(&blob)?;
        attr.width = width;
        attr.height = height;
        attr.cols = width.div_ceil(attr.block_w);
        attr.rows = height.div_ceil(attr.block_h);
        // write_surface re-encodes the block table; this keeps the row
        // decodable until that happens.
        attr.block_sizes = vec![0; (attr.cols * attr.rows) as usize];
        self.db.execute(
            "update Offscreen set Attribute=?1 where MainId=?2",
            rusqlite::params![attr.encode(), offscreen],
        )?;
        let blank = vec![0u8; width as usize * height as usize * 4];
        self.write_surface(offscreen, &blank, width, height)
    }

    pub fn set_layer_attr(
        &mut self,
        main_id: i64,
        name: Option<&str>,
        opacity: Option<i64>,
        composite: Option<i64>,
        visible: Option<bool>,
    ) -> Result<()> {
        if let Some(n) = name {
            self.db.execute("update Layer set LayerName=?1 where MainId=?2", rusqlite::params![n, main_id])?;
        }
        if let Some(o) = opacity {
            self.db
                .execute("update Layer set LayerOpacity=?1 where MainId=?2", rusqlite::params![o.clamp(0, 256), main_id])?;
        }
        if let Some(c) = composite {
            self.db
                .execute("update Layer set LayerComposite=?1 where MainId=?2", rusqlite::params![c, main_id])?;
        }
        if let Some(v) = visible {
            self.db.execute(
                "update Layer set LayerVisibility=?1 where MainId=?2",
                rusqlite::params![v as i64, main_id],
            )?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Replace every mip level of a layer with straight RGBA8 pixels.
    pub fn set_pixels(&mut self, main_id: i64, rgba: &[u8], width: u32, height: u32) -> Result<()> {
        let levels = self.mip_offscreens(main_id)?;
        if levels.is_empty() {
            return Err(Error::format("layer has no render mipmap"));
        }
        for (scale, off_main) in levels {
            let factor = (scale / 100.0).max(0.0001);
            let tw = ((width as f64 * factor).round().max(1.0)) as u32;
            let th = ((height as f64 * factor).round().max(1.0)) as u32;
            self.resize_render_offscreen(off_main, tw, th)?;
            let pixels = if tw == width && th == height {
                rgba.to_vec()
            } else {
                resize_rgba(rgba, width, height, tw, th)
            };
            self.write_surface(off_main, &pixels, tw, th)?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Retarget an offscreen surface to a new resolution before writing it.
    /// Partial layer surfaces and cropped canvas edits both need this; simply
    /// resampling into the old attribute dimensions silently stretches the
    /// layer when it is reopened.
    fn resize_render_offscreen(
        &mut self,
        off_main: i64,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let attr_blob: Vec<u8> = self.db.query_row(
            "select Attribute from Offscreen where MainId=?1",
            [off_main],
            |r| r.get(0),
        )?;
        let mut attr = Attr::decode(&attr_blob)?;
        if attr.width == width && attr.height == height {
            return Ok(());
        }
        attr.width = width.max(1);
        attr.height = height.max(1);
        attr.block_stride = attr.block_w;
        attr.cols = (attr.width + attr.block_w - 1) / attr.block_w;
        attr.rows = (attr.height + attr.block_h - 1) / attr.block_h;
        attr.block_sizes = vec![0u32; (attr.cols * attr.rows) as usize];
        attr.init.has_color = false;
        attr.init.rgba = 0;
        attr.init.extra.clear();
        self.db.execute(
            "update Offscreen set Attribute=?1 where MainId=?2",
            rusqlite::params![attr.encode(), off_main],
        )?;
        self.dirty = true;
        Ok(())
    }

    fn write_surface(&mut self, off_main: i64, rgba: &[u8], width: u32, height: u32) -> Result<()> {
        let (attr_blob, eid): (Vec<u8>, Option<String>) = self.db.query_row(
            "select Attribute, cast(BlockData as text) from Offscreen where MainId=?1",
            [off_main],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut attr = Attr::decode(&attr_blob)?;
        if attr.width != width || attr.height != height {
            return Err(Error::format(format!(
                "surface {off_main} is {}x{}, not {width}x{height}",
                attr.width, attr.height
            )));
        }
        // The pixels we are about to write replace any "fill the whole
        // surface with the init colour" shortcut copied from a template.
        attr.init.has_color = false;
        attr.init.rgba = 0;
        attr.init.extra.clear();
        let tiles = split_tiles(rgba, width, height, attr.block_w, attr.block_h)?;
        let payload = codec::encode_offscreen(&tiles, &mut attr, flate2::Compression::default())?;
        let attr_blob = attr.encode();
        let id = match eid {
            Some(id) if !id.is_empty() && self.payloads.contains_key(&id) => id,
            _ => {
                let id = self.new_external_id();
                self.order.push(id.clone());
                self.db.execute(
                    "insert into ExternalChunk(ExternalID, Offset) values(?1, 0)",
                    [&id],
                )?;
                id
            }
        };
        self.payloads.insert(id.clone(), payload);
        self.db.execute(
            "update Offscreen set Attribute=?1, BlockData=?2 where MainId=?3",
            rusqlite::params![attr_blob, id.as_bytes(), off_main],
        )?;
        Ok(())
    }

    /// Native pixel size of a layer's 100% surface.
    pub fn layer_surface_size(&self, layer_main: i64) -> Result<(u32, u32)> {
        let off = self.offscreen_at(layer_main, 100.0)?;
        let blob: Vec<u8> = self.db.query_row(
            "select Attribute from Offscreen where MainId=?1",
            [off],
            |r| r.get(0),
        )?;
        let attr = Attr::decode(&blob)?;
        Ok((attr.width, attr.height))
    }

    pub fn mip_offscreens(&self, layer_main: i64) -> Result<Vec<(f64, i64)>> {
        let rmip: i64 = self.db.query_row(
            "select coalesce(LayerRenderMipmap,0) from Layer where MainId=?1",
            [layer_main],
            |r| r.get(0),
        )?;
        self.mip_chain_offscreens(rmip)
    }

    pub fn mip_chain_offscreens(&self, rmip: i64) -> Result<Vec<(f64, i64)>> {
        if rmip == 0 {
            return Ok(Vec::new());
        }
        let (count, mut cur): (i64, i64) = self.db.query_row(
            "select MipmapCount, BaseMipmapInfo from Mipmap where MainId=?1",
            [rmip],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut out = Vec::new();
        for _ in 0..count.max(0) {
            let (s, off, next): (f64, i64, i64) = self.db.query_row(
                "select ThisScale, Offscreen, NextIndex from MipmapInfo where MainId=?1",
                [cur],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            out.push((s, off));
            if next == 0 {
                break;
            }
            cur = next;
        }
        Ok(out)
    }

    pub fn offscreen_at(&self, layer_main: i64, scale: f64) -> Result<i64> {
        let rmip: i64 = self.db.query_row(
            "select coalesce(LayerRenderMipmap,0) from Layer where MainId=?1",
            [layer_main],
            |r| r.get(0),
        )?;
        if rmip == 0 {
            return Err(Error::format("layer has no render mipmap"));
        }
        let base: i64 = self.db.query_row(
            "select BaseMipmapInfo from Mipmap where MainId=?1",
            [rmip],
            |r| r.get(0),
        )?;
        let mut cur = base;
        loop {
            let (s, off, next): (f64, i64, i64) = self.db.query_row(
                "select ThisScale, Offscreen, NextIndex from MipmapInfo where MainId=?1",
                [cur],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if (s - scale).abs() < 1e-6 {
                return Ok(off);
            }
            if next == 0 {
                return Ok(off);
            }
            cur = next;
        }
    }

    fn new_external_id(&self) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let mut seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
        seed ^= std::process::id() as u64;
        let mut s = String::with_capacity(40);
        s.push_str("extrnlid");
        let mut x = seed;
        for _ in 0..4 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            s.push_str(&format!("{:08X}", (x >> 32) as u32));
        }
        s
    }

    /// Write the file. When nothing changed, output is byte-identical.
    /// Serialize the document to its final byte image.
    fn build_bytes(&mut self) -> Result<Vec<u8>> {
        if !self.dirty {
            return Ok(self.source.clone());
        }
        let mut db_bytes = self.serialize_db()?;
        for round in 0..6 {
            let (bytes, offsets) =
                chunk::rebuild(&self.identifier, &db_bytes, &self.order, &self.payloads);
            let changed = self.update_offsets(&offsets)?;
            if !changed && round > 0 {
                return Ok(bytes);
            }
            db_bytes = self.serialize_db()?;
        }
        let (bytes, _) =
            chunk::rebuild(&self.identifier, &db_bytes, &self.order, &self.payloads);
        Ok(bytes)
    }

    /// Publish the document at `path`.
    ///
    /// The replacement is written to a temporary file in the destination
    /// directory, flushed and synced, then renamed over the destination, so a
    /// reader (or a crash) sees either the complete old file or the complete
    /// new one. The writer records the published bytes only after the rename
    /// succeeded: a failed save keeps the document dirty and the previous
    /// bytes intact for a later attempt.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let target = resolve_link(path);
        let bytes = self.build_bytes()?;
        publish_atomically(&target, &bytes)?;
        self.source = bytes;
        self.dirty = false;
        Ok(())
    }

    fn update_offsets(&self, offsets: &HashMap<String, u64>) -> Result<bool> {
        let mut st = self.db.prepare("select ExternalID, Offset from ExternalChunk")?;
        let rows: Vec<(String, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        let mut changed = false;
        for (id, old) in rows {
            if let Some(&new) = offsets.get(&id) {
                if old as u64 != new {
                    self.db.execute(
                        "update ExternalChunk set Offset=?1 where ExternalID=?2",
                        rusqlite::params![new as i64, id],
                    )?;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    fn serialize_db(&self) -> Result<Vec<u8>> {
        let out = self._tmp.path().join("vacuum.sqlite");
        let _ = std::fs::remove_file(&out);
        // VACUUM INTO writes a clean, compact copy with the current contents.
        self.db.execute("VACUUM INTO ?1", [out.to_string_lossy().to_string()])?;
        Ok(std::fs::read(out)?)
    }

    /// Re-render the document and store a fresh CanvasPreview PNG.
    pub fn refresh_preview(&mut self, preview_scale: f64) -> Result<()> {
        // Materialize the current state to a temp file, open it through the
        // normal reader and render.
        let tmp = self._tmp.path().join("preview-src.clip");
        let saved_dirty = self.dirty;
        self.dirty = true;
        self.save(&tmp)?;
        self.dirty = saved_dirty;
        let file = ClipFile::open(&tmp)?;
        let (pw, ph) = file.canvas.pixel_size();
        let opts = crate::RenderOptions {
            scale: preview_scale,
            background: Some([255, 255, 255, 255]),
            include_hidden: false,
        };
        let img = crate::render::render(&file, &opts)?;
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, img.width, img.height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header()?;
            w.write_image_data(&img.data)?;
            w.finish()?;
        }
        let _ = (pw, ph);
        self.set_canvas_preview(&png, img.width, img.height)?;
        self.dirty = true;
        Ok(())
    }

    pub fn set_canvas_preview(&mut self, png: &[u8], w: u32, h: u32) -> Result<()> {
        let exists: i64 =
            self.db.query_row("select count(*) from CanvasPreview", [], |r| r.get(0))?;
        if exists == 0 {
            self.db.execute(
                "insert into CanvasPreview(MainId, CanvasId, ImageType, ImageWidth, ImageHeight, ImageData) values(1,1,1,?1,?2,?3)",
                rusqlite::params![w as i64, h as i64, png],
            )?;
        } else {
            self.db.execute(
                "update CanvasPreview set ImageWidth=?1, ImageHeight=?2, ImageData=?3",
                rusqlite::params![w as i64, h as i64, png],
            )?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Layers that carry a mask while their visibility does not enable it.
    ///
    /// CLIP STUDIO PAINT paints a mask only when `LayerVisibility` has bit 2;
    /// files written before that bit was understood show the mask thumbnail and
    /// paint the layer unmasked. Reported as a note so old documents stay
    /// identifiable.
    pub fn layers_with_a_disabled_mask(&self) -> Vec<i64> {
        let mut out = Vec::new();
        let Ok(mut st) = self.db.prepare(
            "select MainId from Layer where coalesce(LayerLayerMaskMipmap,0) <> 0 \
             and (coalesce(LayerVisibility,1) & 2) = 0 order by MainId",
        ) else {
            return out;
        };
        if let Ok(rows) = st.query_map([], |r| r.get::<_, i64>(0)) {
            out.extend(rows.filter_map(std::result::Result::ok));
        }
        out
    }

    /// Layer rows whose thumbnail columns point at `LayerThumbnail` rows that
    /// do not exist.
    ///
    /// A dangling reference opens in CLIP STUDIO PAINT but the layer is not
    /// usable as a mask source or correction, so the condition is worth
    /// reporting even though it is not structural damage.
    pub fn layers_with_missing_thumbnail_rows(&self) -> Vec<(i64, &'static str, i64)> {
        let mut out = Vec::new();
        let Ok(mut st) = self.db.prepare(
            "select l.MainId, coalesce(l.LayerRenderThumbnail,0), \
                    coalesce(l.LayerLayerMaskThumbnail,0) from Layer l",
        ) else {
            return out;
        };
        let Ok(rows) = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) else {
            return out;
        };
        let rows: Vec<(i64, i64, i64)> = rows.filter_map(std::result::Result::ok).collect();
        for (layer, render, mask) in rows {
            for (column, id) in [
                ("LayerRenderThumbnail", render),
                ("LayerLayerMaskThumbnail", mask),
            ] {
                if id == 0 {
                    continue;
                }
                let exists: i64 = self
                    .db
                    .query_row(
                        "select count(*) from LayerThumbnail where MainId=?1",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if exists == 0 {
                    out.push((layer, column, id));
                }
            }
        }
        out
    }

    /// Simple structural validation of what would be written.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let referenced: HashSet<&str> = self.payloads.keys().map(|s| s.as_str()).collect();
        for id in &self.order {
            if !referenced.contains(id.as_str()) {
                problems.push(format!("chunk {id} has no payload"));
            }
        }
        for id in &self.order {
            if !id.starts_with("extrnlid") || id.len() != 40 {
                problems.push(format!("non-standard external id {id}"));
            }
        }
        if self.identifier.is_empty() {
            problems.push("empty file identifier".into());
        }
        problems
    }


    // ---- layer tree editing ------------------------------------------------

    fn max_main_id(&self, table: &str) -> Result<i64> {
        let n: i64 = self.db.query_row(
            &format!("select coalesce(max(MainId),0) from {table}"),
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    fn clone_row_with(
        &mut self,
        table: &str,
        source_main: i64,
        overrides: &[(&str, Lit)],
    ) -> Result<()> {
        let cols = column_set(&self.db, table)?;
        let mut names = Vec::new();
        let mut exprs = Vec::new();
        for c in &cols {
            if c == "_PW_ID" {
                continue;
            }
            names.push(c.as_str());
            match overrides.iter().find(|(k, _)| *k == c.as_str()) {
                Some((_, Lit::Int(v))) => exprs.push(v.to_string()),
                Some((_, Lit::Text(v))) => exprs.push(format!("'{}'", v.replace('\'', "''"))),
                Some((_, Lit::Null)) => exprs.push("NULL".to_string()),
                None => exprs.push(c.clone()),
            }
        }
        let sql = format!(
            "insert into {table} (_PW_ID, {}) select NULL, {} from {table} where MainId=?1",
            names.join(", "),
            exprs.join(", ")
        );
        self.db.execute(&sql, [source_main])?;
        Ok(())
    }

    /// Slot `(previous, next)` for a row inserted above `above` inside
    /// `parent`; `above == 0` appends above the parent's top child.
    fn sibling_slot(&self, parent: i64, above: i64) -> Result<(i64, i64)> {
        if above != 0 {
            let next: i64 = self.db.query_row(
                "select coalesce(LayerNextIndex,0) from Layer where MainId=?1",
                [above],
                |r| r.get(0),
            )?;
            return Ok((above, next));
        }
        let first: i64 = self.db.query_row(
            "select coalesce(LayerFirstChildIndex,0) from Layer where MainId=?1",
            [parent],
            |r| r.get(0),
        )?;
        if first == 0 {
            return Ok((parent, 0));
        }
        let mut cur = first;
        loop {
            let n: i64 = self.db.query_row(
                "select coalesce(LayerNextIndex,0) from Layer where MainId=?1",
                [cur],
                |r| r.get(0),
            )?;
            if n == 0 {
                break;
            }
            cur = n;
        }
        Ok((cur, 0))
    }

    fn link_slot(&mut self, parent: i64, prev: i64, new_layer: i64, next: i64) -> Result<()> {
        self.db.execute(
            "update Layer set LayerNextIndex=?1 where MainId=?2",
            rusqlite::params![next, new_layer],
        )?;
        if prev == parent {
            self.db.execute(
                "update Layer set LayerFirstChildIndex=?1 where MainId=?2",
                rusqlite::params![new_layer, parent],
            )?;
        } else {
            self.db.execute(
                "update Layer set LayerNextIndex=?1 where MainId=?2",
                rusqlite::params![new_layer, prev],
            )?;
        }
        Ok(())
    }

    /// Insert a new empty raster layer directly above `above` (or at the top of
    /// the root folder when `above` is zero). Returns the new `Layer.MainId`.
    pub fn add_raster_layer(&mut self, above: i64, name: &str) -> Result<i64> {
        let root: i64 = self.db.query_row("select CanvasRootFolder from Canvas limit 1", [], |r| r.get(0))?;
        self.add_raster_layer_in(root, above, name)
    }

    /// Insert a raster layer inside `parent`, above `above` (zero means the
    /// top child of `parent`). Returns the new `Layer.MainId`.
    /// Copy the mip chain shape of `template` for `layer`, with fresh
    /// offscreen surfaces, and return the new `Mipmap` row id.
    fn create_render_mip_chain(&mut self, canvas_id: i64, layer: i64, template: i64) -> Result<i64> {
        let mip = self.max_main_id("Mipmap")? + 1;
        let levels = self.mip_offscreens(template)?;
        if levels.is_empty() {
            return Err(Error::format("template layer has no mip chain"));
        }

        // Fresh offscreen + external chunk for every mip level.
        let mut offscreens = Vec::new();
        for (_scale, off) in &levels {
            let (attr_blob, _) = self.db.query_row(
                "select Attribute, BlockData from Offscreen where MainId=?1",
                [off],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
            )?;
            let attr = Attr::decode(&attr_blob)?;
            let new_off = self.max_main_id("Offscreen")? + 1;
            let ext = self.new_external_id();
            self.order.push(ext.clone());
            self.payloads.insert(ext.clone(), Vec::new());
            self.db.execute(
                "insert into ExternalChunk(ExternalID, Offset) values(?1, 0)",
                [&ext],
            )?;
            self.db.execute(
                "insert into Offscreen(MainId, CanvasId, LayerId, Attribute, BlockData) \
                 values(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![new_off, canvas_id, layer, attr_blob, ext.as_bytes()],
            )?;
            let empty = vec![0u8; attr.width as usize * attr.height as usize * 4];
            self.write_surface(new_off, &empty, attr.width, attr.height)?;
            offscreens.push(new_off);
        }

        // MipmapInfo chain.
        let mut info_ids = Vec::with_capacity(offscreens.len());
        for _ in 0..offscreens.len() {
            info_ids.push(self.max_main_id("MipmapInfo")? + 1 + info_ids.len() as i64);
        }
        for (i, (scale, _)) in levels.iter().enumerate() {
            let next = info_ids.get(i + 1).copied().unwrap_or(0);
            self.db.execute(
                "insert into MipmapInfo(MainId, CanvasId, LayerId, ThisScale, Offscreen, NextIndex) \
                 values(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![info_ids[i], canvas_id, layer, scale, offscreens[i], next],
            )?;
        }
        self.db.execute(
            "insert into Mipmap(MainId, CanvasId, LayerId, MipmapCount, BaseMipmapInfo) \
             values(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![mip, canvas_id, layer, levels.len() as i64, info_ids[0]],
        )?;

        Ok(mip)
    }

    pub fn add_raster_layer_in(&mut self, parent: i64, above: i64, name: &str) -> Result<i64> {
        let canvas_id: i64 = self.db.query_row("select MainId from Canvas limit 1", [], |r| r.get(0))?;
        let template: Option<i64> = self
            .db
            .query_row(
                "select MainId from Layer where LayerType=1 and LayerFolder&1=0 \
                 and LayerRenderMipmap is not null and LayerRenderMipmap<>0 limit 1",
                [],
                |r| r.get(0),
            )
            .ok();
        let template = template.ok_or_else(|| Error::format("document has no raster layer to copy"))?;

        let new_layer = self.max_main_id("Layer")? + 1;
        let new_mip = self.create_render_mip_chain(canvas_id, new_layer, template)?;

        // Where does the new row go in the sibling list?
        let (prev, next) = self.sibling_slot(parent, above)?;

        self.clone_row_with(
            "Layer",
            template,
            &[
                ("MainId", Lit::Int(new_layer)),
                ("CanvasId", Lit::Int(canvas_id)),
                ("LayerName", Lit::Text(name.to_string())),
                ("LayerType", Lit::Int(1)),
                ("LayerLock", Lit::Int(0)),
                ("LayerClip", Lit::Int(0)),
                ("LayerMasking", Lit::Int(0)),
                ("LayerOffsetX", Lit::Int(0)),
                ("LayerOffsetY", Lit::Int(0)),
                ("LayerRenderOffscrOffsetX", Lit::Int(0)),
                ("LayerRenderOffscrOffsetY", Lit::Int(0)),
                ("LayerMaskOffsetX", Lit::Int(0)),
                ("LayerMaskOffsetY", Lit::Int(0)),
                ("LayerOpacity", Lit::Int(256)),
                ("LayerComposite", Lit::Int(0)),
                ("LayerFolder", Lit::Int(0)),
                ("LayerVisibility", Lit::Int(1)),
                ("LayerSelect", Lit::Int(0)),
                ("LayerNextIndex", Lit::Int(next)),
                ("LayerFirstChildIndex", Lit::Int(0)),
                ("LayerRenderMipmap", Lit::Int(new_mip)),
                ("LayerLayerMaskMipmap", Lit::Int(0)),
                ("DrawToRenderOffscreenType", Lit::Null),
                ("LayerUuid", Lit::Text(layer_uuid(new_layer, canvas_id))),
            ],
        )?;

        self.link_slot(parent, prev, new_layer, next)?;
        self.db.execute("update Canvas set CanvasCurrentLayer=?1", [new_layer])?;
        self.dirty = true;
        Ok(new_layer)
    }

    /// Insert an empty layer folder above `above` in the root, or at the top of
    /// the root when `above` is zero.
    pub fn add_folder(&mut self, above: i64, name: &str) -> Result<i64> {
        let root: i64 = self.db.query_row("select CanvasRootFolder from Canvas limit 1", [], |r| r.get(0))?;
        self.add_folder_in(root, above, name)
    }

    /// Insert an empty layer folder inside `parent`, above `above` (zero means
    /// the top child of `parent`). Returns the new `Layer.MainId`.
    pub fn add_folder_in(&mut self, parent: i64, above: i64, name: &str) -> Result<i64> {
        let canvas_id: i64 = self.db.query_row("select MainId from Canvas limit 1", [], |r| r.get(0))?;
        let root: i64 = self.db.query_row("select CanvasRootFolder from Canvas limit 1", [], |r| r.get(0))?;
        let parent = if parent == 0 { root } else { parent };
        let (prev, next) = self.sibling_slot(parent, above)?;
        let new_layer = self.max_main_id("Layer")? + 1;
        // CSP keeps a render mipmap and thumbnail for every group it writes,
        // and documents without them crash it on open. Copy the shape of an
        // existing raster layer so the group starts with a blank surface.
        let mip_template: Option<i64> = self
            .db
            .query_row(
                "select MainId from Layer where LayerType=1 and LayerFolder&1=0 \
                 and LayerRenderMipmap is not null and LayerRenderMipmap<>0 limit 1",
                [],
                |r| r.get(0),
            )
            .ok();
        let new_mip = match mip_template {
            Some(template) => self.create_render_mip_chain(canvas_id, new_layer, template)?,
            None => 0,
        };
        // The canvas root row is a folder row with every implementation-specific
        // column present; override the fields that describe the new child.
        self.clone_row_with(
            "Layer",
            root,
            &[
                ("MainId", Lit::Int(new_layer)),
                ("CanvasId", Lit::Int(canvas_id)),
                ("LayerName", Lit::Text(name.to_string())),
                ("LayerType", Lit::Int(0)),
                ("LayerLock", Lit::Int(0)),
                ("LayerClip", Lit::Int(0)),
                ("LayerMasking", Lit::Int(0)),
                ("LayerOffsetX", Lit::Int(0)),
                ("LayerOffsetY", Lit::Int(0)),
                ("LayerRenderOffscrOffsetX", Lit::Int(0)),
                ("LayerRenderOffscrOffsetY", Lit::Int(0)),
                ("LayerMaskOffsetX", Lit::Int(0)),
                ("LayerMaskOffsetY", Lit::Int(0)),
                ("LayerOpacity", Lit::Int(256)),
                ("LayerComposite", Lit::Int(30)),
                ("LayerFolder", Lit::Int(17)),
                ("LayerVisibility", Lit::Int(1)),
                ("LayerSelect", Lit::Int(0)),
                ("LayerNextIndex", Lit::Int(next)),
                ("LayerFirstChildIndex", Lit::Int(0)),
                ("LayerRenderMipmap", Lit::Int(new_mip)),
                ("LayerLayerMaskMipmap", Lit::Int(0)),
                ("LayerRenderThumbnail", Lit::Int(0)),
                ("DrawToRenderOffscreenType", Lit::Null),
                ("LayerUuid", Lit::Text(layer_uuid(new_layer, canvas_id))),
            ],
        )?;
        self.link_slot(parent, prev, new_layer, next)?;
        self.db.execute("update Canvas set CanvasCurrentLayer=?1", [new_layer])?;
        self.dirty = true;
        Ok(new_layer)
    }

    /// Repoint a layer's render and/or layer-mask mip chains.
    pub fn set_layer_mips(
        &mut self,
        main_id: i64,
        render: Option<i64>,
        mask: Option<i64>,
    ) -> Result<()> {
        if let Some(r) = render {
            self.db.execute(
                "update Layer set LayerRenderMipmap=?1 where MainId=?2",
                rusqlite::params![r, main_id],
            )?;
        }
        if let Some(m) = mask {
            self.db.execute(
                "update Layer set LayerLayerMaskMipmap=?1 where MainId=?2",
                rusqlite::params![m, main_id],
            )?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Repoint the layer-thumbnail offscreens (the small previews CSP keeps in
    /// `LayerThumbnail`) for a layer's render and/or mask thumbnail.
    pub fn set_layer_thumbnail_offscreen(
        &mut self,
        main_id: i64,
        render: Option<i64>,
        mask: Option<i64>,
    ) -> Result<()> {
        let row = self
            .db
            .query_row(
                "select coalesce(LayerRenderThumbnail,0), coalesce(LayerLayerMaskThumbnail,0) \
                 from Layer where MainId=?1",
                [main_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok();
        let (rthumb, mthumb) = row.unwrap_or((0, 0));
        if let Some(off) = render {
            if rthumb != 0 {
                self.db.execute(
                    "update LayerThumbnail set ThumbnailOffscreen=?1 where MainId=?2",
                    rusqlite::params![off, rthumb],
                )?;
            }
        }
        if let Some(off) = mask {
            if mthumb != 0 {
                self.db.execute(
                    "update LayerThumbnail set ThumbnailOffscreen=?1 where MainId=?2",
                    rusqlite::params![off, mthumb],
                )?;
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Replace a layer's UUID.
    pub fn set_layer_uuid(&mut self, main_id: i64, uuid: &str) -> Result<()> {
        self.db.execute(
            "update Layer set LayerUuid=?1 where MainId=?2",
            rusqlite::params![uuid, main_id],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Set the opaque `LayerSelect` flags CSP stores for a layer.
    pub fn set_layer_select(&mut self, main_id: i64, value: i64) -> Result<()> {
        self.db.execute(
            "update Layer set LayerSelect=?1 where MainId=?2",
            rusqlite::params![value, main_id],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Make sure the optional `FilterLayerInfo` column exists.
    fn ensure_filter_column(&mut self) -> Result<()> {
        let cols: Vec<String> = {
            let mut st = self.db.prepare("pragma table_info(Layer)")?;
            let rows = st.query_map([], |r| r.get::<_, String>(1))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        if !cols.iter().any(|c| c == "FilterLayerInfo") {
            self.db.execute("alter table Layer add column FilterLayerInfo BLOB", [])?;
        }
        Ok(())
    }

    /// Store a raw `PWVFilterLayerInfo` blob on a layer.
    pub fn set_filter_info(&mut self, main_id: i64, info: &[u8]) -> Result<()> {
        self.ensure_filter_column()?;
        self.db.execute(
            "update Layer set FilterLayerInfo=?1 where MainId=?2",
            rusqlite::params![info, main_id],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Insert an adjustment layer above `above`.
    ///
    /// The row starts as a copy of a raster layer (so every implementation
    /// specific column keeps a sane default), then switches to
    /// `LayerType = 4098` with the given `FilterLayerInfo` blob and an
    /// all-white mask, which is how CSP stores a full-canvas adjustment.
    pub fn add_adjustment(
        &mut self,
        above: i64,
        name: &str,
        filter: &[u8],
    ) -> Result<i64> {
        let id = self.add_raster_layer(above, name)?;
        self.mark_adjustment(id, filter)?;
        Ok(id)
    }

    /// Turn an existing raster row into a correction layer: the same flags
    /// `add_adjustment` writes, for a row that was inserted into a specific
    /// sibling position instead of above one.
    pub fn mark_adjustment(&mut self, id: i64, filter: &[u8]) -> Result<()> {
        self.set_filter_info(id, filter)?;
        let mask_mip = self.add_layer_mask(id)?;
        let render_mip: i64 = self.db.query_row(
            "select coalesce(LayerRenderMipmap,0) from Layer where MainId=?1",
            [id],
            |r| r.get(0),
        )?;
        // Mirror the flags CSP writes for a correction layer: visible with a
        // render thumbnail, `SpecialRenderType = 13`, mask generated from the
        // stored mask mip.
        // `LayerRenderThumbnail` and `LayerLayerMaskThumbnail` keep the ids the
        // row inherited from its raster template: those point at rows that
        // exist, while mipmap ids are not `LayerThumbnail` rows at all.
        self.db.execute(
            "update Layer set LayerType=4098, LayerVisibility=3, LayerSelect=0, \
             LayerOpacity=256, LayerComposite=0, LayerClip=0, LayerMasking=33, \
             SpecialRenderType=13, SetRenderThumbnailInfoType=20, \
             DrawRenderThumbnailType=20 where MainId=?1",
            [id],
        )?;
        // The raster template carries colour-type and light-table columns that
        // CSP leaves NULL on a correction layer. A non-NULL value there makes
        // CSP treat the row as a colour-type layer and skip the filter.
        let cols = column_set(&self.db, "Layer")?;
        for name in [
            "LayerColorTypeIndex",
            "LayerColorTypeBlackChecked",
            "LayerColorTypeWhiteChecked",
            "LightTableInfo",
        ] {
            if cols.iter().any(|c| c == name) {
                self.db.execute(
                    &format!("update Layer set {name}=NULL where MainId=?1"),
                    [id],
                )?;
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Remove a raster layer from the tree and drop its surfaces.
    pub fn delete_layer(&mut self, main_id: i64) -> Result<()> {
        // Unlink from the sibling chain.
        let mut st = self.db.prepare("select MainId, coalesce(LayerNextIndex,0), coalesce(LayerFirstChildIndex,0) from Layer")?;
        let rows: Vec<(i64, i64, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        for (id, next, first) in rows {
            if next == main_id {
                let new_next: i64 = self.db.query_row(
                    "select coalesce(LayerNextIndex,0) from Layer where MainId=?1",
                    [main_id],
                    |r| r.get(0),
                )?;
                self.db.execute(
                    "update Layer set LayerNextIndex=?1 where MainId=?2",
                    rusqlite::params![new_next, id],
                )?;
            }
            if first == main_id {
                let new_first: i64 = self.db.query_row(
                    "select coalesce(LayerNextIndex,0) from Layer where MainId=?1",
                    [main_id],
                    |r| r.get(0),
                )?;
                self.db.execute(
                    "update Layer set LayerFirstChildIndex=?1 where MainId=?2",
                    rusqlite::params![new_first, id],
                )?;
            }
        }
        // Drop mip chain surfaces: the render chain and, when the row still
        // owns one, the mask chain. A mask whose owner row is gone would
        // otherwise stay in the file as an unreferenced surface.
        let render_mip: i64 = self.db.query_row(
            "select coalesce(LayerRenderMipmap,0) from Layer where MainId=?1",
            [main_id],
            |r| r.get(0),
        )?;
        self.drop_mip_chain(render_mip)?;
        let mask_mip: i64 = self.db.query_row(
            "select coalesce(LayerLayerMaskMipmap,0) from Layer where MainId=?1",
            [main_id],
            |r| r.get(0),
        )?;
        self.drop_mip_chain(mask_mip)?;
        self.db.execute("delete from Layer where MainId=?1", [main_id])?;
        self.dirty = true;
        Ok(())
    }

    /// Delete one mip chain: its offscreen surfaces, their external payloads,
    /// every `MipmapInfo` row and the `Mipmap` row itself.
    fn drop_mip_chain(&mut self, mip: i64) -> Result<()> {
        if mip == 0 {
            return Ok(());
        }
        for (_scale, off) in self.mip_chain_offscreens(mip)? {
            let eid: Option<Vec<u8>> = self
                .db
                .query_row("select BlockData from Offscreen where MainId=?1", [off], |r| r.get(0))
                .ok()
                .flatten();
            if let Some(bytes) = eid {
                let id = String::from_utf8_lossy(&bytes).into_owned();
                self.payloads.remove(&id);
                self.order.retain(|x| x != &id);
                self.db.execute("delete from ExternalChunk where ExternalID=?1", [&id])?;
            }
            self.db.execute("delete from Offscreen where MainId=?1", [off])?;
        }
        let mut cur: i64 = self
            .db
            .query_row("select BaseMipmapInfo from Mipmap where MainId=?1", [mip], |r| r.get(0))?;
        loop {
            let next: i64 = self
                .db
                .query_row("select coalesce(NextIndex,0) from MipmapInfo where MainId=?1", [cur], |r| r.get(0))
                .unwrap_or(0);
            self.db.execute("delete from MipmapInfo where MainId=?1", [cur])?;
            if next == 0 {
                break;
            }
            cur = next;
        }
        self.db.execute("delete from Mipmap where MainId=?1", [mip])?;
        self.dirty = true;
        Ok(())
    }

    /// Hide every non-folder layer except the given MainIds.
    ///
    /// Used to isolate one painted surface (plus adjustments and the paper)
    /// when measuring how CSP renders a document.
    pub fn hide_all_except(&mut self, keep: &[i64]) -> Result<usize> {
        let mut st = self.db.prepare(
            "select MainId from Layer where not (LayerFolder & 1) and LayerType not in (256)",
        )?;
        let ids: Vec<i64> = st
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        let mut hidden = 0;
        for id in ids {
            if keep.contains(&id) {
                continue;
            }
            self.db.execute(
                "update Layer set LayerVisibility=0 where MainId=?1",
                [id],
            )?;
            hidden += 1;
        }
        self.dirty = true;
        Ok(hidden)
    }

    /// Drop the document colour profiles (keeps everything else intact).
    pub fn clear_color_profiles(&mut self) -> Result<()> {
        for col in [
            "CanvasSrcProfileName",
            "CanvasSrcProfile",
            "CanvasDstProfileName",
            "CanvasDstProfile",
            "CanvasSimulateSrcProfileName",
            "CanvasSimulateSrcProfile",
            "CanvasSimulateDstProfileName",
            "CanvasSimulateDstProfile",
        ] {
            let exists: i64 = self.db.query_row(
                "select count(*) from pragma_table_info('Canvas') where name=?1",
                [col],
                |r| r.get(0),
            )?;
            if exists > 0 {
                self.db.execute(&format!("update Canvas set {col}=NULL"), [])?;
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Enable or disable clipping for one layer.
    /// Replace the source text of an editable text layer.
    ///
    /// CSP re-renders text layers from `TextLayerString`; the stored mipmap
    /// is only a cache. Line breaks must be CRLF, as in CSP's own writer.
    pub fn set_text_string(&mut self, layer_id: i64, text: &str) -> Result<()> {
        let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
        let changed = self.db.execute(
            "update Layer set TextLayerString=?1 where MainId=?2",
            rusqlite::params![normalized.as_bytes(), layer_id],
        )?;
        if changed == 0 {
            return Err(crate::Error::format(format!("no layer {layer_id}")));
        }
        self.dirty = true;
        Ok(())
    }

    pub fn set_layer_clip(&mut self, main_id: i64, clip: bool) -> Result<()> {
        self.db.execute(
            "update Layer set LayerClip=?1 where MainId=?2",
            rusqlite::params![clip as i64, main_id],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Persist the editor's lock flags. Bits 0, 1 and 4 are the observed
    /// alpha, position and reference flags; unknown bits are preserved.
    pub fn set_layer_locks(
        &mut self,
        main_id: i64,
        alpha: bool,
        position: bool,
        reference: bool,
        draft: bool,
    ) -> Result<()> {
        let current: i64 = self.db.query_row(
            "select coalesce(LayerLock,0) from Layer where MainId=?1",
            [main_id],
            |r| r.get(0),
        )?;
        let mut flags = current & !(1 | 2 | 16);
        if alpha {
            flags |= 1;
        }
        if position {
            flags |= 2;
        }
        if reference {
            flags |= 16;
        }
        self.db.execute(
            "update Layer set LayerLock=?1 where MainId=?2",
            rusqlite::params![flags, main_id],
        )?;
        let has_draft: bool = self.db.query_row(
            "select count(*) from pragma_table_info('Layer') where name='DraftLayer'",
            [],
            |r| r.get::<_, i64>(0).map(|count| count > 0),
        )?;
        if has_draft {
            self.db.execute(
                "update Layer set DraftLayer=?1 where MainId=?2",
                rusqlite::params![draft as i64, main_id],
            )?;
        }
        self.dirty = true;
        Ok(())
    }

    pub fn canvas_root_folder(&self) -> Result<i64> {
        Ok(self.db.query_row("select CanvasRootFolder from Canvas limit 1", [], |r| r.get(0))?)
    }

    pub fn canvas_size(&self) -> Result<(u32, u32)> {
        let (width, height): (f64, f64) = self.db.query_row(
            "select CanvasWidth, CanvasHeight from Canvas limit 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((width.round().max(1.0) as u32, height.round().max(1.0) as u32))
    }

    /// Change the canvas geometry and resize every raster surface. Callers
    /// must replace the layer pixels afterwards with `set_pixels`; the blank
    /// intermediate keeps all offscreen block tables decodable at the new
    /// dimensions.
    pub fn resize_canvas(&mut self, width: u32, height: u32) -> Result<()> {
        self.resize_blank(width, height)
    }

    /// Rewrite one sibling chain in the editor's bottom-to-top order. A
    /// folder's child list is represented by `LayerFirstChildIndex` plus each
    /// row's `LayerNextIndex`; updating the complete chain makes reorder and
    /// merge operations survive a CSP save.
    pub fn set_sibling_order(&mut self, parent_id: i64, top_down: &[i64]) -> Result<()> {
        let first = top_down.first().copied().unwrap_or(0);
        self.db.execute(
            "update Layer set LayerFirstChildIndex=?1 where MainId=?2",
            rusqlite::params![first, parent_id],
        )?;
        for (index, id) in top_down.iter().enumerate() {
            let next = top_down.get(index + 1).copied().unwrap_or(0);
            self.db.execute(
                "update Layer set LayerNextIndex=?1 where MainId=?2",
                rusqlite::params![next, id],
            )?;
        }
        self.dirty = true;
        Ok(())
    }

    /// Create an all-white layer mask that mirrors the layer's render mips.
    /// Returns the new `Mipmap.MainId`.
    pub fn add_layer_mask(&mut self, layer_main: i64) -> Result<i64> {
        self.add_mask_from_render(layer_main, layer_main)
    }

    /// Create a folder mask with an explicit native surface size. Folder rows
    /// often have no render mip chain of their own, so copy only the block
    /// geometry from an existing offscreen and keep the requested mask extent.
    pub fn add_folder_mask(
        &mut self,
        folder_main: i64,
        width: u32,
        height: u32,
    ) -> Result<i64> {
        self.add_mask_surface(folder_main, width, height)
    }

    /// Create a layer mask whose native surface dimensions match an
    /// independently placed mask instead of stretching it to the layer's
    /// render surface.
    pub fn add_layer_mask_sized(
        &mut self,
        layer_main: i64,
        width: u32,
        height: u32,
    ) -> Result<i64> {
        self.add_mask_surface(layer_main, width, height)
    }

    fn add_mask_surface(
        &mut self,
        owner_main: i64,
        width: u32,
        height: u32,
    ) -> Result<i64> {
        let existing: i64 = self.db.query_row(
            "select coalesce(LayerLayerMaskMipmap,0) from Layer where MainId=?1",
            [owner_main],
            |r| r.get(0),
        )?;
        if existing != 0 {
            return Err(Error::format("layer already has a mask"));
        }
        let canvas_id: i64 = self.db.query_row("select MainId from Canvas limit 1", [], |r| r.get(0))?;
        let attr_blob: Vec<u8> = self.db.query_row(
            "select Attribute from Offscreen limit 1",
            [],
            |r| r.get(0),
        )?;
        let mut attr = Attr::decode(&attr_blob)?;
        attr.width = width.max(1);
        attr.height = height.max(1);
        attr.channels = 0;
        attr.color_mode = 1;
        attr.alpha_flag = 1;
        attr.bit_depth = 1;
        attr.block_stride = attr.block_w;
        attr.cols = (attr.width + attr.block_w - 1) / attr.block_w;
        attr.rows = (attr.height + attr.block_h - 1) / attr.block_h;
        attr.block_sizes = vec![0u32; (attr.cols * attr.rows) as usize];
        attr.init.has_color = false;
        attr.init.rgba = 0;
        attr.init.extra.clear();
        let new_off = self.max_main_id("Offscreen")? + 1;
        let ext = self.new_external_id();
        self.order.push(ext.clone());
        self.payloads.insert(ext.clone(), Vec::new());
        self.db.execute(
            "insert into ExternalChunk(ExternalID, Offset) values(?1, 0)",
            [&ext],
        )?;
        self.db.execute(
            "insert into Offscreen(MainId, CanvasId, LayerId, Attribute, BlockData)              values(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![new_off, canvas_id, owner_main, attr.encode(), ext.as_bytes()],
        )?;
        let white = vec![255u8; width as usize * height as usize * 4];
        self.write_surface(new_off, &white, width, height)?;
        let info_id = self.max_main_id("MipmapInfo")? + 1;
        self.db.execute(
            "insert into MipmapInfo(MainId, CanvasId, LayerId, ThisScale, Offscreen, NextIndex)              values(?1, ?2, ?3, 100, ?4, 0)",
            rusqlite::params![info_id, canvas_id, owner_main, new_off],
        )?;
        let new_mip = self.max_main_id("Mipmap")? + 1;
        self.db.execute(
            "insert into Mipmap(MainId, CanvasId, LayerId, MipmapCount, BaseMipmapInfo)              values(?1, ?2, ?3, 1, ?4)",
            rusqlite::params![new_mip, canvas_id, owner_main, info_id],
        )?;
        self.db.execute(
            "update Layer set LayerLayerMaskMipmap=?1, LayerMaskOffsetX=0, LayerMaskOffsetY=0,              LayerVisibility=(coalesce(LayerVisibility,1) | 2) where MainId=?2",
            rusqlite::params![new_mip, owner_main],
        )?;
        self.dirty = true;
        Ok(new_mip)
    }

    fn add_mask_from_render(
        &mut self,
        mask_owner: i64,
        render_source: i64,
    ) -> Result<i64> {
        let existing: i64 = self.db.query_row(
            "select coalesce(LayerLayerMaskMipmap,0) from Layer where MainId=?1",
            [mask_owner],
            |r| r.get(0),
        )?;
        if existing != 0 {
            return Err(Error::format("layer already has a mask"));
        }
        let canvas_id: i64 = self.db.query_row("select MainId from Canvas limit 1", [], |r| r.get(0))?;
        let levels = self.mip_offscreens(render_source)?;
        if levels.is_empty() {
            return Err(Error::format("layer has no render mip chain"));
        }
        let new_mip = self.max_main_id("Mipmap")? + 1;
        let mut offscreens = Vec::new();
        for (_scale, off) in &levels {
            let (attr_blob, _) = self.db.query_row(
                "select Attribute, BlockData from Offscreen where MainId=?1",
                [off],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
            )?;
            let mut attr = Attr::decode(&attr_blob)?;
            attr.channels = 0;
            attr.color_mode = 1;
            attr.alpha_flag = 1;
            attr.bit_depth = 1;
            attr.block_stride = attr.block_w;
            attr.init.has_color = false;
            attr.init.rgba = 0;
            attr.init.extra.clear();
            attr.block_sizes = vec![0u32; (attr.cols * attr.rows) as usize];
            let new_off = self.max_main_id("Offscreen")? + 1;
            let ext = self.new_external_id();
            self.order.push(ext.clone());
            self.payloads.insert(ext.clone(), Vec::new());
            self.db.execute(
                "insert into ExternalChunk(ExternalID, Offset) values(?1, 0)",
                [&ext],
            )?;
            self.db.execute(
                "insert into Offscreen(MainId, CanvasId, LayerId, Attribute, BlockData) \
                 values(?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![new_off, canvas_id, mask_owner, attr.encode(), ext.as_bytes()],
            )?;
            // White reveals the whole layer.
            let white = vec![255u8; attr.width as usize * attr.height as usize * 4];
            self.write_surface(new_off, &white, attr.width, attr.height)?;
            offscreens.push(new_off);
        }
        let mut info_ids = Vec::with_capacity(offscreens.len());
        for _ in 0..offscreens.len() {
            info_ids.push(self.max_main_id("MipmapInfo")? + 1 + info_ids.len() as i64);
        }
        for (i, (scale, _)) in levels.iter().enumerate() {
            let next = info_ids.get(i + 1).copied().unwrap_or(0);
            self.db.execute(
                "insert into MipmapInfo(MainId, CanvasId, LayerId, ThisScale, Offscreen, NextIndex) \
                 values(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![info_ids[i], canvas_id, mask_owner, scale, offscreens[i], next],
            )?;
        }
        self.db.execute(
            "insert into Mipmap(MainId, CanvasId, LayerId, MipmapCount, BaseMipmapInfo) \
             values(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![new_mip, canvas_id, mask_owner, levels.len() as i64, info_ids[0]],
        )?;
        self.db.execute(
            "update Layer set LayerLayerMaskMipmap=?1, LayerMaskOffsetX=0, LayerMaskOffsetY=0, \
             LayerVisibility=(coalesce(LayerVisibility,1) | 2) where MainId=?2",
            rusqlite::params![new_mip, mask_owner],
        )?;
        self.dirty = true;
        Ok(new_mip)
    }

    pub fn remove_layer_mask(&mut self, layer_main: i64) -> Result<()> {
        let mip: i64 = self.db.query_row(
            "select coalesce(LayerLayerMaskMipmap,0) from Layer where MainId=?1",
            [layer_main],
            |r| r.get(0),
        )?;
        if mip == 0 {
            return Ok(());
        }
        self.drop_mip_chain(mip)?;
        self.db.execute(
            "update Layer set LayerLayerMaskMipmap=0, \
             LayerVisibility=(coalesce(LayerVisibility,1) & ~2) where MainId=?1",
            [layer_main],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Synchronize the native surface origin with the editor's layer offset.
    /// The editor folds `LayerOffsetX/Y + LayerRenderOffscrOffsetX/Y` into one
    /// local offset, so write it through the render-offset columns and leave
    /// the other offset columns at zero.
    pub fn set_layer_surface_offsets(
        &mut self,
        layer_main: i64,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.db.execute(
            "update Layer set \
                LayerOffsetX=0, LayerOffsetY=0, \
                LayerRenderOffscrOffsetX=?1, LayerRenderOffscrOffsetY=?2 \
             where MainId=?3",
            rusqlite::params![x, y, layer_main],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Native document-space origins of a layer and its mask:
    /// `(render offset x, render offset y, mask offset x, mask offset y)`.
    pub fn layer_offsets(&self, layer_main: i64) -> Result<(i32, i32, i32, i32)> {
        self.db
            .query_row(
                "select coalesce(LayerRenderOffscrOffsetX,0), \
                        coalesce(LayerRenderOffscrOffsetY,0), \
                        coalesce(LayerMaskOffsetX,0), \
                        coalesce(LayerMaskOffsetY,0) \
                 from Layer where MainId=?1",
                [layer_main],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
    }

    /// Set a layer or folder mask's native document-space origin.
    pub fn set_layer_mask_offset(
        &mut self,
        layer_main: i64,
        x: i32,
        y: i32,
    ) -> Result<()> {
        self.db.execute(
            "update Layer set LayerMaskOffsetX=?1, LayerMaskOffsetY=?2 where MainId=?3",
            rusqlite::params![x, y, layer_main],
        )?;
        self.dirty = true;
        Ok(())
    }

    /// Replace every mask mip with the alpha channel of `rgba`.
    pub fn set_mask_pixels(
        &mut self,
        layer_main: i64,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<()> {
        let mip: i64 = self.db.query_row(
            "select coalesce(LayerLayerMaskMipmap,0) from Layer where MainId=?1",
            [layer_main],
            |r| r.get(0),
        )?;
        if mip == 0 {
            return Err(Error::format("layer has no mask"));
        }
        for (scale, off) in self.mip_chain_offscreens(mip)? {
            let factor = (scale / 100.0).max(0.0001);
            let tw = ((width as f64 * factor).round().max(1.0)) as u32;
            let th = ((height as f64 * factor).round().max(1.0)) as u32;
            self.resize_render_offscreen(off, tw, th)?;
            let pixels = if tw == width && th == height {
                rgba.to_vec()
            } else {
                resize_rgba(rgba, width, height, tw, th)
            };
            self.write_surface(off, &pixels, tw, th)?;
        }
        self.dirty = true;
        Ok(())
    }

    pub fn mask_surface_size(&self, layer_main: i64) -> Result<Option<(u32, u32)>> {
        let mip: i64 = self.db.query_row(
            "select coalesce(LayerLayerMaskMipmap,0) from Layer where MainId=?1",
            [layer_main],
            |r| r.get(0),
        )?;
        let levels = self.mip_chain_offscreens(mip)?;
        let Some((_s, off)) = levels.first() else { return Ok(None) };
        let blob: Vec<u8> = self.db.query_row(
            "select Attribute from Offscreen where MainId=?1",
            [*off],
            |r| r.get(0),
        )?;
        let attr = Attr::decode(&blob)?;
        Ok(Some((attr.width, attr.height)))
    }

    #[allow(dead_code)]
    pub fn db(&self) -> &Connection {
        &self.db
    }
}

/// CSP-style layer UUID: 32 hex digits grouped 10-4-4-4-10.
///
/// The id must be unique per layer; CSP merges layers whose UUIDs collide when
/// it re-saves a document.
fn layer_uuid(main_id: i64, canvas_id: i64) -> String {
    // SplitMix64 over the ids gives a stable, unique-looking value without a
    // random number generator dependency.
    let mut z = (main_id as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(canvas_id as u64);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let hi = format!("{z:016x}");
    let lo = format!("{:016x}", z.rotate_left(17) ^ (main_id as u64));
    format!(
        "{}-{}-{}-{}-{}",
        &hi[0..10],
        &hi[10..14],
        &hi[14..16],
        &lo[0..2],
        &lo[2..12]
    )
}

fn split_tiles(rgba: &[u8], width: u32, height: u32, bw: u32, bh: u32) -> Result<Vec<Vec<u8>>> {
    if rgba.len() != (width * height * 4) as usize {
        return Err(Error::format("pixel buffer size mismatch"));
    }
    let cols = width.div_ceil(bw);
    let rows = height.div_ceil(bh);
    let mut tiles = Vec::with_capacity((cols * rows) as usize);
    for by in 0..rows {
        for bx in 0..cols {
            let mut tile = vec![0u8; (bw * bh * 4) as usize];
            for y in 0..bh {
                let sy = by * bh + y;
                if sy >= height {
                    break;
                }
                for x in 0..bw {
                    let sx = bx * bw + x;
                    if sx >= width {
                        break;
                    }
                    let si = ((sy * width + sx) * 4) as usize;
                    let di = ((y * bw + x) * 4) as usize;
                    tile[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
                }
            }
            tiles.push(tile);
        }
    }
    Ok(tiles)
}

fn resize_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let fx = sw as f64 / dw as f64;
    let fy = sh as f64 / dh as f64;
    for y in 0..dh {
        let y0 = (y as f64 * fy).floor() as u32;
        let y1 = (((y + 1) as f64 * fy).ceil() as u32).min(sh).max(y0 + 1);
        for x in 0..dw {
            let x0 = (x as f64 * fx).floor() as u32;
            let x1 = (((x + 1) as f64 * fx).ceil() as u32).min(sw).max(x0 + 1);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = ((yy * sw + xx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + n / 2) / n).min(255) as u8;
            }
        }
    }
    out
}

/// Follow a symlink so publishing replaces the linked file instead of the link.
fn resolve_link(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

/// Write `bytes` next to `target` and rename them over it.
///
/// The temporary lives in the destination directory so the rename stays on one
/// filesystem and is therefore atomic. Existing permissions are preserved, the
/// file is fsynced before the rename and the directory after it, so a crash
/// cannot publish a truncated document.
pub fn publish_atomically(target: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let directory = directory_of(target);
    let mut temporary = tempfile::Builder::new()
        .prefix(".clip-save-")
        .suffix(".tmp")
        .tempfile_in(&directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().flush()?;
    publish_file(temporary.path(), target)?;
    Ok(())
}

/// Publish a fully written temporary file as `target`.
///
/// Used by every save path (normal save, Save As and the KRA archive writer):
/// the temporary is fsynced, keeps the destination's permissions, is renamed
/// over the destination and the parent directory is fsynced, so a crash leaves
/// either the old or the new document — never a truncated one.
pub fn publish_file(temporary: &Path, target: &Path) -> Result<()> {
    let target = resolve_link(target);
    let directory = directory_of(&target);
    let file = std::fs::File::open(temporary)?;
    file.sync_all()?;
    if let Ok(meta) = std::fs::metadata(&target) {
        let _ = file.set_permissions(meta.permissions());
    }
    drop(file);
    std::fs::rename(temporary, &target)?;
    if let Ok(handle) = std::fs::File::open(&directory) {
        let _ = handle.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_whose_layer_does_not_enable_it_is_reported() -> anyhow::Result<()> {
        let mut writer = ClipWriter::blank(8, 8)?;
        let paint: i64 = writer.db().query_row(
            "select MainId from Layer where LayerType=1 limit 1",
            [],
            |r| r.get(0),
        )?;
        writer.add_layer_mask(paint)?;
        assert!(
            writer.layers_with_a_disabled_mask().is_empty(),
            "adding a mask through the writer enables it"
        );
        // A document written before the fix: the mask exists, the bit is clear.
        writer.db.execute(
            "update Layer set LayerVisibility = coalesce(LayerVisibility,1) & ~2 where MainId=?1",
            [paint],
        )?;
        assert_eq!(
            writer.layers_with_a_disabled_mask(),
            vec![paint],
            "a mask CSP would ignore is reported"
        );
        Ok(())
    }

    #[test]
    fn a_correction_does_not_dangle_its_thumbnail_reference() -> anyhow::Result<()> {
        let mut writer = ClipWriter::blank(8, 8)?;
        assert!(
            writer.layers_with_missing_thumbnail_rows().is_empty(),
            "the template references only rows it has"
        );
        let paint: i64 = writer.db().query_row(
            "select MainId from Layer where LayerType=1 limit 1",
            [],
            |r| r.get(0),
        )?;
        let filter = crate::adjust::Filter {
            kind: crate::adjust::FilterKind::Hsv { hue: 30.0, saturation: 0.0, luminosity: 0.0 },
            raw: Vec::new(),
        };
        let blob = filter.to_blob().expect("encodes");
        let correction = writer.add_adjustment(paint, "Hue", &blob)?;
        let dangling = writer.layers_with_missing_thumbnail_rows();
        assert!(
            !dangling.iter().any(|(layer, _, _)| *layer == correction),
            "a correction must keep a thumbnail row that exists: {dangling:?}"
        );

        // The detector still reports a real dangling reference.
        writer.db.execute(
            "update Layer set LayerRenderThumbnail=9999 where MainId=?1",
            [correction],
        )?;
        assert!(
            writer
                .layers_with_missing_thumbnail_rows()
                .iter()
                .any(|(layer, column, id)| *layer == correction
                    && *column == "LayerRenderThumbnail"
                    && *id == 9999),
            "an id that no row carries is still reported"
        );
        Ok(())
    }

    #[test]
    fn a_masked_layer_enables_the_mask_visibility_bit() -> anyhow::Result<()> {
        let mut writer = ClipWriter::blank(8, 8)?;
        let paint: i64 = writer.db().query_row(
            "select MainId from Layer where LayerType=1 limit 1",
            [],
            |r| r.get(0),
        )?;
        let before: i64 = writer.db().query_row(
            "select coalesce(LayerVisibility,1) from Layer where MainId=?1",
            [paint],
            |r| r.get(0),
        )?;
        writer.add_layer_mask(paint)?;
        let masked: i64 = writer.db().query_row(
            "select coalesce(LayerVisibility,1) from Layer where MainId=?1",
            [paint],
            |r| r.get(0),
        )?;
        assert_eq!(
            masked & 2,
            2,
            "CSP only paints a mask whose layer has visibility bit 2 set: {masked}"
        );
        assert_eq!(masked & 1, before & 1, "the layer keeps its own visibility");
        writer.remove_layer_mask(paint)?;
        let cleared: i64 = writer.db().query_row(
            "select coalesce(LayerVisibility,1) from Layer where MainId=?1",
            [paint],
            |r| r.get(0),
        )?;
        assert_eq!(cleared, before, "removing the mask restores the row");
        Ok(())
    }

    #[test]
    fn a_group_created_here_carries_its_own_render_mipmap() -> anyhow::Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grouped.clip");
        let mut writer = ClipWriter::blank(8, 8)?;
        let folder = writer.add_folder(0, "Grupo")?;
        writer.save(&path)?;

        let file = ClipFile::open(&path)?;
        assert!(
            file.groups_without_render_mipmap().is_empty(),
            "a group created here must carry a render mipmap"
        );

        let writer = ClipWriter::load(&path)?;
        let (mip_layer, mip_count): (i64, i64) = writer.db().query_row(
            "select m.MainId is not null, m.MipmapCount from Layer l \
             join Mipmap m on m.MainId=l.LayerRenderMipmap where l.MainId=?1",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(mip_layer, 1, "the group points at its own mipmap row");
        assert!(mip_count >= 1, "the mipmap lists at least one level");

        let info_rows: i64 = writer.db().query_row(
            "select count(*) from MipmapInfo where LayerId=?1",
            [folder],
            |r| r.get(0),
        )?;
        assert_eq!(info_rows, mip_count, "every level has a MipmapInfo row");
        let surface_rows: i64 = writer.db().query_row(
            "select count(*) from Offscreen where LayerId=?1",
            [folder],
            |r| r.get(0),
        )?;
        assert_eq!(surface_rows, mip_count, "every level has a surface row");
        Ok(())
    }

    #[test]
    fn a_group_whose_mipmap_was_dropped_is_still_reported() -> anyhow::Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grouped.clip");
        let mut writer = ClipWriter::blank(8, 8)?;
        let folder = writer.add_folder(0, "Grupo")?;
        writer.set_layer_mips(folder, Some(0), None)?;
        writer.save(&path)?;

        let file = ClipFile::open(&path)?;
        let bare = file.groups_without_render_mipmap();
        assert_eq!(bare.len(), 1, "a group row without a mipmap is reported");
        assert_eq!(bare[0].main_id, folder);
        Ok(())
    }

    /// The build embeds this file, so it has to be tracked: without it a clean
    /// checkout cannot compile the crate at all.
    #[test]
    fn the_embedded_blank_template_is_a_readable_container() {
        assert!(!BLANK_TEMPLATE.is_empty(), "the template must be embedded");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.clip");
        std::fs::write(&path, BLANK_TEMPLATE).unwrap();
        let file = ClipFile::open(&path).unwrap();
        let (width, height) = file.canvas.pixel_size();
        assert!(width > 0 && height > 0, "the template canvas is empty");
        assert!(
            !file.layers.is_empty(),
            "the template must carry the paper and paint layers a blank document needs"
        );
    }
}

fn directory_of(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
