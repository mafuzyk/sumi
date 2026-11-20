//! Raster block codec: the `BlockDataBeginChunk` stream inside a `CHNKExta`.

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

use crate::attr::Attr;
use crate::{Error, Result};

pub const BEGIN: &str = "BlockDataBeginChunk";
pub const END: &str = "BlockDataEndChunk";

#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub index: u32,
    pub decompressed_size: u32,
    pub width: u32,
    pub height: u32,
    pub compressed: Option<Vec<u8>>,
    pub record_size: u32,
}

/// Walk the block stream and return the compressed payloads. Does not inflate.
pub fn parse_blocks(payload: &[u8], attr: &Attr) -> Result<Vec<BlockInfo>> {
    let mut out = Vec::with_capacity(attr.block_sizes.len());
    let mut p = 0usize;
    for (i, &expected) in attr.block_sizes.iter().enumerate() {
        let start = p;
        let size = read_u32(payload, &mut p)?;
        if size != expected {
            return Err(Error::format(format!(
                "block {i}: record size {size} != Attribute.BlockSize {expected}"
            )));
        }
        let (name, np) = crate::chunk::read_utf16be_tag(payload, p)?;
        p = np;
        if name != BEGIN {
            return Err(Error::format(format!("block {i}: expected {BEGIN}, got {name}")));
        }
        let index = read_u32(payload, &mut p)?;
        let decompressed_size = read_u32(payload, &mut p)?;
        let width = read_u32(payload, &mut p)?;
        let height = read_u32(payload, &mut p)?;
        let has = read_u32(payload, &mut p)?;
        let compressed = if has != 0 {
            let section_size = read_u32(payload, &mut p)?;
            let comp = read_u32_le(payload, &mut p)?;
            if comp + 4 != section_size {
                return Err(Error::format(format!(
                    "block {i}: section size {section_size} != compressed {comp} + 4"
                )));
            }
            let data = payload
                .get(p..p + comp as usize)
                .ok_or_else(|| Error::format("truncated zlib data"))?
                .to_vec();
            p += comp as usize;
            Some(data)
        } else {
            None
        };
        let (name, np) = crate::chunk::read_utf16be_tag(payload, p)?;
        p = np;
        if name != END {
            return Err(Error::format(format!("block {i}: expected {END}, got {name}")));
        }
        let consumed = p - start;
        if consumed != size as usize {
            return Err(Error::format(format!(
                "block {i}: consumed {consumed} != record size {size}"
            )));
        }
        out.push(BlockInfo {
            index,
            decompressed_size,
            width,
            height,
            compressed,
            record_size: size,
        });
    }
    Ok(out)
}

/// Decode an entire surface to straight RGBA8 at `attr.width x attr.height`.
pub fn decode_offscreen(payload: &[u8], attr: &Attr) -> Result<Vec<u8>> {
    let w = attr.width as usize;
    let h = attr.height as usize;
    let bw = attr.block_w as usize;
    let bh = attr.block_h as usize;
    let cols = attr.cols as usize;
    let mut out = vec![0u8; w * h * 4];
    if attr.init.has_color {
        let c = attr.init.rgba;
        let rgba = [(c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8];
        for px in out.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
    }
    let blocks = parse_blocks(payload, attr)?;
    for b in &blocks {
        let Some(comp) = &b.compressed else { continue };
        let raw = inflate(comp, b.decompressed_size as usize)?;
        let tile = block_to_rgba(&raw, attr)?;
        let bx = (b.index as usize % cols) * bw;
        let by = (b.index as usize / cols) * bh;
        let x1 = (bx + bw).min(w);
        let y1 = (by + bh).min(h);
        if x1 <= bx || y1 <= by {
            continue;
        }
        for y in by..y1 {
            let src_off = (y - by) * bw * 4;
            let dst_off = (y * w + bx) * 4;
            let n = (x1 - bx) * 4;
            out[dst_off..dst_off + n].copy_from_slice(&tile[src_off..src_off + n]);
        }
    }
    Ok(out)
}

pub fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut d = ZlibDecoder::new(data);
    let mut out = Vec::with_capacity(expected);
    d.read_to_end(&mut out)?;
    if out.len() != expected {
        return Err(Error::format(format!(
            "inflated {} bytes, header says {expected}",
            out.len()
        )));
    }
    Ok(out)
}

fn inflate_any(data: &[u8]) -> Result<Vec<u8>> {
    let mut d = ZlibDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out)?;
    Ok(out)
}

/// Convert one decompressed block payload into `block_h x block_w` RGBA8.
pub fn block_to_rgba(raw: &[u8], attr: &Attr) -> Result<Vec<u8>> {
    let bw = attr.block_w as usize;
    let bh = attr.block_h as usize;
    match attr.channels {
        4 => {
            let stride = bw * 4;
            let expect = (bh + 64) * stride;
            if raw.len() != expect {
                return Err(Error::format(format!(
                    "RGBA block is {} bytes, expected {expect}",
                    raw.len()
                )));
            }
            let mut out = vec![0u8; bw * bh * 4];
            for y in 0..bh {
                let row = &raw[(y + 64) * stride..(y + 65) * stride];
                let dst = &mut out[y * stride..(y + 1) * stride];
                for x in 0..bw {
                    dst[x * 4] = row[x * 4 + 2]; // R <- B
                    dst[x * 4 + 1] = row[x * 4 + 1];
                    dst[x * 4 + 2] = row[x * 4]; // B <- R
                    dst[x * 4 + 3] = 0;
                }
            }
            // Alpha lives in rows 0..64: row = y/4, sub-y selects a 64-column
            // strip, x/4 is the block column and x%4 the sub-pixel channel.
            for y in 0..bh {
                for x in 0..bw {
                    let a = raw[(y / 4) * stride + (((y % 4) * 64 + x / 4) * 4) + (x % 4)];
                    out[(y * bw + x) * 4 + 3] = a;
                }
            }
            Ok(out)
        }
        1 => {
            // Two planes: plane0 = alpha, plane1 = grey value. Both are 8bpp
            // normally and packed 1-bit when the block is eight times smaller.
            let plane = bw * bh;
            let mut out = vec![0u8; bw * bh * 4];
            if raw.len() * 8 == plane * 2 {
                for i in 0..plane {
                    let a = (raw[i / 8] >> (7 - (i % 8))) & 1;
                    let v = (raw[plane / 8 + i / 8] >> (7 - (i % 8))) & 1;
                    // The value plane is white where set; clear bits are the
                    // ink (text glyphs inside a speech bubble).
                    let v = if v == 1 { 255 } else { 0 };
                    out[i * 4] = v;
                    out[i * 4 + 1] = v;
                    out[i * 4 + 2] = v;
                    out[i * 4 + 3] = if a == 1 { 255 } else { 0 };
                }
                return Ok(out);
            }
            if raw.len() < plane * 2 {
                return Err(Error::format(format!(
                    "grey block is {} bytes, expected {} (block {bw}x{bh})",
                    raw.len(),
                    plane * 2
                )));
            }
            for i in 0..plane {
                let a = raw[i];
                let v = raw[plane + i];
                out[i * 4] = v;
                out[i * 4 + 1] = v;
                out[i * 4 + 2] = v;
                out[i * 4 + 3] = a;
            }
            Ok(out)
        }
        0 => {
            // Alpha-only block (masks / selections); colour comes from init.
            // CSP writes two encodings: packed 1-bit (bit clear = reveal) and
            // a plain 8bpp plane. The decompressed size tells them apart.
            let plane = bw * bh;
            let mut out = vec![0u8; bw * bh * 4];
            if raw.len() * 8 == plane {
                for i in 0..plane {
                    let byte = raw[i / 8];
                    let bit = (byte >> (7 - (i % 8))) & 1;
                    out[i * 4 + 3] = if bit == 1 { 255 } else { 0 };
                }
                return Ok(out);
            }
            if raw.len() < plane {
                return Err(Error::format("alpha block too small"));
            }
            for i in 0..plane {
                out[i * 4 + 3] = raw[i];
            }
            Ok(out)
        }
        n => Err(Error::unsupported(format!("channel count {n}"))),
    }
}

/// Encode RGBA8 pixels (block_w x block_h) into the on-disk block payload.
pub fn rgba_to_block(rgba: &[u8], attr: &Attr, compression: Compression) -> Result<Vec<u8>> {
    let bw = attr.block_w as usize;
    let bh = attr.block_h as usize;
    if rgba.len() != bw * bh * 4 {
        return Err(Error::format("rgba_to_block: wrong input size"));
    }
    match attr.channels {
        4 => {
            let stride = bw * 4;
            let mut raw = vec![0u8; (bh + 64) * stride];
            for y in 0..bh {
                let src = &rgba[y * stride..(y + 1) * stride];
                let dst = &mut raw[(y + 64) * stride..(y + 65) * stride];
                for x in 0..bw {
                    dst[x * 4] = src[x * 4 + 2];
                    dst[x * 4 + 1] = src[x * 4 + 1];
                    dst[x * 4 + 2] = src[x * 4];
                    dst[x * 4 + 3] = 0;
                }
            }
            for y in 0..bh {
                for x in 0..bw {
                    raw[(y / 4) * stride + (((y % 4) * 64 + x / 4) * 4) + (x % 4)] =
                        rgba[(y * bw + x) * 4 + 3];
                }
            }
            let mut e = ZlibEncoder::new(Vec::new(), compression);
            e.write_all(&raw)?;
            Ok(e.finish()?)
        }
        1 => {
            let plane = bw * bh;
            let mut raw = vec![0u8; plane * 2];
            for i in 0..plane {
                raw[i] = rgba[i * 4 + 3];
                raw[plane + i] = rgba[i * 4];
            }
            let mut e = ZlibEncoder::new(Vec::new(), compression);
            e.write_all(&raw)?;
            Ok(e.finish()?)
        }
        0 => {
            let plane = bw * bh;
            let mut raw = vec![0u8; plane];
            for i in 0..plane {
                raw[i] = rgba[i * 4 + 3];
            }
            let mut e = ZlibEncoder::new(Vec::new(), compression);
            e.write_all(&raw)?;
            Ok(e.finish()?)
        }
        n => Err(Error::unsupported(format!("channel count {n}"))),
    }
}

/// Build the complete `CHNKExta` payload for one surface.
pub fn encode_offscreen(tiles: &[Vec<u8>], attr: &mut Attr, compression: Compression) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut sizes = Vec::with_capacity(tiles.len());
    for (i, tile) in tiles.iter().enumerate() {
        let start = body.len();
        let empty = tile.iter().all(|&b| b == 0);
        if empty {
            // 104-byte empty record.
            body.extend_from_slice(&104u32.to_be_bytes());
            body.extend_from_slice(&crate::chunk::utf16be_tag(BEGIN));
            body.extend_from_slice(&(i as u32).to_be_bytes());
            body.extend_from_slice(&(attr.block_bytes() as u32).to_be_bytes());
            body.extend_from_slice(&attr.block_w.to_be_bytes());
            body.extend_from_slice(&attr.block_h.to_be_bytes());
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&crate::chunk::utf16be_tag(END));
        } else {
            let comp = rgba_to_block(tile, attr, compression)?;
            let size = 4u32 + 4 + 38 + 20 + 8 + comp.len() as u32 + 4 + 34;
            body.extend_from_slice(&size.to_be_bytes());
            body.extend_from_slice(&crate::chunk::utf16be_tag(BEGIN));
            body.extend_from_slice(&(i as u32).to_be_bytes());
            body.extend_from_slice(&(attr.block_bytes() as u32).to_be_bytes());
            body.extend_from_slice(&attr.block_w.to_be_bytes());
            body.extend_from_slice(&attr.block_h.to_be_bytes());
            body.extend_from_slice(&1u32.to_be_bytes());
            body.extend_from_slice(&(comp.len() as u32 + 4).to_be_bytes());
            body.extend_from_slice(&(comp.len() as u32).to_le_bytes());
            body.extend_from_slice(&comp);
            body.extend_from_slice(&crate::chunk::utf16be_tag(END));
        }
        sizes.push((body.len() - start) as u32);
    }
    // BlockStatus
    body.extend_from_slice(&crate::chunk::utf16be_tag("BlockStatus"));
    body.extend_from_slice(&12u32.to_be_bytes());
    body.extend_from_slice(&(tiles.len() as u32).to_be_bytes());
    body.extend_from_slice(&4u32.to_be_bytes());
    for _ in tiles {
        body.extend_from_slice(&1u32.to_be_bytes());
    }
    // BlockCheckSum: CSP treats zero as "no checksum" and accepts the file.
    body.extend_from_slice(&crate::chunk::utf16be_tag("BlockCheckSum"));
    body.extend_from_slice(&12u32.to_be_bytes());
    body.extend_from_slice(&(tiles.len() as u32).to_be_bytes());
    body.extend_from_slice(&4u32.to_be_bytes());
    for _ in tiles {
        body.extend_from_slice(&0u32.to_be_bytes());
    }
    attr.block_sizes = sizes;
    Ok(body)
}

fn read_u32(data: &[u8], p: &mut usize) -> Result<u32> {
    let v = data
        .get(*p..*p + 4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
        .ok_or_else(|| Error::format("truncated block stream"))?;
    *p += 4;
    Ok(v)
}

fn read_u32_le(data: &[u8], p: &mut usize) -> Result<u32> {
    let v = data
        .get(*p..*p + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| Error::format("truncated block stream"))?;
    *p += 4;
    Ok(v)
}

pub fn decode_any(payload: &[u8]) -> Result<Vec<u8>> {
    inflate_any(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_attr(bit_depth: u32, block_w: u32, block_h: u32, channels: u32) -> Attr {
        Attr {
            width: block_w * 2,
            height: block_h * 2,
            cols: 2,
            rows: 2,
            color_mode: 1,
            alpha_flag: 1,
            channels,
            bit_depth,
            block_w,
            block_h,
            block_stride: block_w,
            init: crate::attr::InitColor::default(),
            block_sizes: Vec::new(),
        }
    }

    #[test]
    fn packed_one_bit_mask_sets_alpha_for_set_bits() {
        let attr = mask_attr(1, 256, 256, 0);
        // First 32 bytes are row 0: 0xF0 then zeros -> the first four pixels
        // of the block are inside the mask, the rest are outside.
        let mut raw = vec![0u8; 256 * 256 / 8];
        raw[0] = 0xF0;
        let rgba = block_to_rgba(&raw, &attr).unwrap();
        assert_eq!(rgba[0..4], [0, 0, 0, 255]);
        assert_eq!(rgba[3 * 4 + 3], 255);
        assert_eq!(rgba[4 * 4 + 3], 0);
        assert_eq!(rgba[(255 * 256 + 200) * 4 + 3], 0);
    }

    #[test]
    fn eight_bit_mask_still_decodes() {
        let attr = mask_attr(8, 256, 256, 0);
        let mut raw = vec![0u8; 256 * 256];
        raw[0] = 255;
        raw[1] = 128;
        let rgba = block_to_rgba(&raw, &attr).unwrap();
        assert_eq!(rgba[3], 255);
        assert_eq!(rgba[7], 128);
        assert_eq!(rgba[11], 0);
    }

    #[test]
    fn packed_one_bit_grey_uses_set_bits_as_white_fill() {
        let attr = mask_attr(1, 256, 256, 1);
        let mut raw = vec![0u8; 256 * 256 / 8 * 2];
        raw[0] = 0x80; // alpha plane, pixel 0
        raw[256 * 256 / 8] = 0x80; // value plane, pixel 0
        let rgba = block_to_rgba(&raw, &attr).unwrap();
        // Set value bit -> white bubble fill; clear -> ink (black glyph).
        assert_eq!(rgba[0..4], [255, 255, 255, 255]);
        assert_eq!(rgba[4 + 3], 0);
    }
}
