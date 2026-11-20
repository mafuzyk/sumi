//! `Offscreen.Attribute` — the self-describing header of one raster surface.
//!
//! Measured layout (big-endian), from real files and cross-checked against the
//! published clipparse/clip-tools notes:
//!
//! ```text
//! u32[4] section sizes: [16, 102, 42+4*extra, 34+4*blocks]
//! u32 9, "Parameter" (UTF-16BE, 18 bytes)
//!   u32 width, height, cols, rows
//!   u32 color_mode, alpha_flag, channels, bit_depth
//!   65536, 4, 1024, 1
//!   block_w, 65536, block_h, block_stride
//!   8, 8, 0, 0
//! u32 9, "InitColor" (18 bytes)
//!   u32 20, has_color, packed_rgba, extra_count, channels
//!   u32[extra_count]
//! u32 9, "BlockSize" (18 bytes)
//!   u32 12, block_count, channels
//!   u32[block_count]
//! ```

use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct InitColor {
    pub has_color: bool,
    /// RGBA packed big-endian (0xFF0000FF is opaque red).
    pub rgba: u32,
    pub extra: Vec<u32>,
    pub channels: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Attr {
    pub width: u32,
    pub height: u32,
    pub cols: u32,
    pub rows: u32,
    pub color_mode: u32,
    pub alpha_flag: u32,
    pub channels: u32,
    pub bit_depth: u32,
    pub block_w: u32,
    pub block_h: u32,
    pub block_stride: u32,
    pub init: InitColor,
    pub block_sizes: Vec<u32>,
}

impl Attr {
    pub fn nblocks(&self) -> u32 {
        self.cols * self.rows
    }

    pub fn block_bytes(&self) -> usize {
        if self.channels == 0 {
            (self.block_w * self.block_h) as usize
        } else {
            ((self.block_w + 64) * self.block_w * 4) as usize
        }
    }

    pub fn decode(data: &[u8]) -> Result<Attr> {
        if data.len() < 16 {
            return Err(Error::format("attribute too small"));
        }
        let sizes: [u32; 4] = [
            u32::from_be_bytes(data[0..4].try_into().unwrap()),
            u32::from_be_bytes(data[4..8].try_into().unwrap()),
            u32::from_be_bytes(data[8..12].try_into().unwrap()),
            u32::from_be_bytes(data[12..16].try_into().unwrap()),
        ];
        let mut p = 16usize;
        let (name, np) = crate::chunk::read_utf16be_tag(data, p)?;
        p = np;
        if name != "Parameter" {
            return Err(Error::format(format!("expected Parameter, got {name}")));
        }
        let width = read_u32(data, &mut p)?;
        let height = read_u32(data, &mut p)?;
        let cols = read_u32(data, &mut p)?;
        let rows = read_u32(data, &mut p)?;
        let color_mode = read_u32(data, &mut p)?;
        let alpha_flag = read_u32(data, &mut p)?;
        let channels = read_u32(data, &mut p)?;
        let bit_depth = read_u32(data, &mut p)?;
        let _geom = [read_u32(data, &mut p)?, read_u32(data, &mut p)?, read_u32(data, &mut p)?, read_u32(data, &mut p)?];
        let dims = [read_u32(data, &mut p)?, read_u32(data, &mut p)?, read_u32(data, &mut p)?, read_u32(data, &mut p)?];
        let sub = [read_u32(data, &mut p)?, read_u32(data, &mut p)?, read_u32(data, &mut p)?, read_u32(data, &mut p)?];
        let _ = sub;

        let (name, np) = crate::chunk::read_utf16be_tag(data, p)?;
        p = np;
        if name != "InitColor" {
            return Err(Error::format(format!("expected InitColor, got {name}")));
        }
        let _magic = read_u32(data, &mut p)?;
        let has = read_u32(data, &mut p)?;
        let rgba = read_u32(data, &mut p)?;
        let nextra = read_u32(data, &mut p)?;
        let ich = read_u32(data, &mut p)?;
        let mut extra = Vec::with_capacity(nextra as usize);
        for _ in 0..nextra {
            extra.push(read_u32(data, &mut p)?);
        }

        let (name, np) = crate::chunk::read_utf16be_tag(data, p)?;
        p = np;
        if name != "BlockSize" {
            return Err(Error::format(format!("expected BlockSize, got {name}")));
        }
        let _m12 = read_u32(data, &mut p)?;
        let nblocks = read_u32(data, &mut p)?;
        let _bch = read_u32(data, &mut p)?;
        let mut block_sizes = Vec::with_capacity(nblocks as usize);
        for _ in 0..nblocks {
            block_sizes.push(read_u32(data, &mut p)?);
        }
        if p != data.len() {
            return Err(Error::format(format!(
                "attribute has {} trailing bytes",
                data.len() - p
            )));
        }
        if nblocks != cols * rows {
            return Err(Error::format(format!(
                "block count {nblocks} != cols*rows {}",
                cols * rows
            )));
        }
        let _ = sizes;
        // `dims` is (row_stride, ?, block_w, block_h). For 1-bit surfaces the
        // row stride is 32 bytes for a 256-pixel block, so `dims[0]` is not the
        // block width.
        let block_w = dims[2];
        let block_h = dims[3];
        Ok(Attr {
            width,
            height,
            cols,
            rows,
            color_mode,
            alpha_flag,
            channels,
            bit_depth,
            block_w,
            block_h,
            block_stride: dims[3],
            init: InitColor { has_color: has != 0, rgba, extra, channels: ich },
            block_sizes,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let n = self.block_sizes.len() as u32;
        let mut out = Vec::with_capacity(16 + 102 + 42 + 4 * self.init.extra.len() as usize + 34 + 4 * n as usize);
        let init_len = 42 + 4 * self.init.extra.len() as u32;
        let block_len = 34 + 4 * n;
        out.extend_from_slice(&16u32.to_be_bytes());
        out.extend_from_slice(&102u32.to_be_bytes());
        out.extend_from_slice(&init_len.to_be_bytes());
        out.extend_from_slice(&block_len.to_be_bytes());
        out.extend_from_slice(&crate::chunk::utf16be_raw("Parameter"));
        for v in [self.width, self.height, self.cols, self.rows] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        for v in [self.color_mode, self.alpha_flag, self.channels, self.bit_depth] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        for v in [65536u32, 4, 1024, 1] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        for v in [self.block_w, 65536, self.block_h, self.block_stride] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        for v in [8u32, 8, 0, 0] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&crate::chunk::utf16be_raw("InitColor"));
        out.extend_from_slice(&20u32.to_be_bytes());
        out.extend_from_slice(&(self.init.has_color as u32).to_be_bytes());
        out.extend_from_slice(&self.init.rgba.to_be_bytes());
        out.extend_from_slice(&(self.init.extra.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.init.channels.to_be_bytes());
        for v in &self.init.extra {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&crate::chunk::utf16be_raw("BlockSize"));
        out.extend_from_slice(&12u32.to_be_bytes());
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(&self.channels.to_be_bytes());
        for v in &self.block_sizes {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }
}

fn read_u32(data: &[u8], p: &mut usize) -> Result<u32> {
    let v = data
        .get(*p..*p + 4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
        .ok_or_else(|| Error::format("truncated attribute"))?;
    *p += 4;
    Ok(v)
}
