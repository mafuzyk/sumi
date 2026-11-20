//! Minimal layered Photoshop PSD/PSB reader and writer.
//!
//! The reader handles the common 8-bit RGB(A) baseline: layer rectangles,
//! names, opacity, visibility, blend keys, raw and PackBits channel data, plus
//! the flattened merged image. The writer emits layered documents with four
//! channels per layer and a merged RGBA image so other applications can show a
//! preview without understanding our channel layout.

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::blend;
use crate::render::Image;

#[derive(Debug, Clone)]
pub struct PsdLayer {
    pub name: String,
    pub image: Image,
    /// Top-left of the layer rectangle in canvas coordinates.
    pub offset: (i32, i32),
    pub opacity: f64,
    pub visible: bool,
    pub blend: i64,
    pub clip: bool,
    /// User layer mask: alpha carries the coverage, 255 reveals.
    pub mask: Option<Image>,
    /// Top-left of the mask rectangle in canvas coordinates.
    pub mask_offset: (i32, i32),
}

#[derive(Debug, Clone)]
pub struct PsdDocument {
    pub width: u32,
    pub height: u32,
    /// Bottom-to-top, matching Photoshop's layer records.
    pub layers: Vec<PsdLayer>,
    pub merged: Option<Image>,
    /// Embedded ICC profile (image resource 1039), when present.
    pub icc_profile: Option<Vec<u8>>,
    /// Features present in the file that this reader drops. Callers must
    /// surface these instead of treating the conversion as lossless.
    pub unsupported: Vec<String>,
}

#[derive(Clone, Copy)]
pub struct PsdImageRef<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

#[derive(Clone, Copy)]
pub struct PsdLayerData<'a> {
    pub name: &'a str,
    pub image: PsdImageRef<'a>,
    pub offset: (i32, i32),
    pub opacity: f64,
    pub visible: bool,
    pub blend: i64,
    pub clip: bool,
    /// Optional user layer mask; alpha carries the coverage.
    pub mask: Option<PsdImageRef<'a>>,
    /// Top-left of the mask rectangle in canvas coordinates.
    pub mask_offset: (i32, i32),
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    psb: bool,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], psb: bool) -> Self {
        Reader { data, pos: 0, psb }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!("truncated PSD at byte {} (wanted {n})", self.pos);
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn block_len(&mut self) -> Result<usize> {
        let value = if self.psb {
            self.u64()?
        } else {
            u64::from(self.u32()?)
        };
        usize::try_from(value).context("PSD length does not fit memory")
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n)?;
        Ok(())
    }
}

fn blend_to_key(mode: i64) -> &'static [u8; 4] {
    match mode {
        blend::MODE_DARKEN => b"dark",
        blend::MODE_MULTIPLY => b"mul ",
        blend::MODE_COLOR_BURN => b"idiv",
        blend::MODE_LINEAR_BURN => b"lbrn",
        blend::MODE_SUBTRACT => b"fsub",
        blend::MODE_DARKER_COLOR => b"dkCl",
        blend::MODE_LIGHTEN => b"lite",
        blend::MODE_SCREEN => b"scrn",
        blend::MODE_COLOR_DODGE => b"div ",
        blend::MODE_ADD => b"lddg",
        blend::MODE_LIGHTER_COLOR => b"lgCl",
        blend::MODE_OVERLAY => b"over",
        blend::MODE_SOFT_LIGHT => b"sLit",
        blend::MODE_HARD_LIGHT => b"hLit",
        blend::MODE_VIVID_LIGHT => b"vLit",
        blend::MODE_LINEAR_LIGHT => b"lLit",
        blend::MODE_PIN_LIGHT => b"pLit",
        blend::MODE_HARD_MIX => b"hMix",
        blend::MODE_DIFFERENCE => b"diff",
        blend::MODE_EXCLUSION => b"smud",
        blend::MODE_HUE => b"hue ",
        blend::MODE_SATURATION => b"sat ",
        blend::MODE_COLOR => b"colr",
        blend::MODE_LUMINOSITY => b"lum ",
        blend::MODE_DIVIDE => b"div ",
        _ => b"norm",
    }
}

fn key_to_blend(key: &[u8]) -> i64 {
    match key {
        b"dark" => blend::MODE_DARKEN,
        b"mul " => blend::MODE_MULTIPLY,
        b"idiv" => blend::MODE_COLOR_BURN,
        b"lbrn" => blend::MODE_LINEAR_BURN,
        b"fsub" => blend::MODE_SUBTRACT,
        b"dkCl" => blend::MODE_DARKER_COLOR,
        b"lite" => blend::MODE_LIGHTEN,
        b"scrn" => blend::MODE_SCREEN,
        b"div " => blend::MODE_COLOR_DODGE,
        b"lddg" => blend::MODE_ADD,
        b"lgCl" => blend::MODE_LIGHTER_COLOR,
        b"over" => blend::MODE_OVERLAY,
        b"sLit" => blend::MODE_SOFT_LIGHT,
        b"hLit" => blend::MODE_HARD_LIGHT,
        b"vLit" => blend::MODE_VIVID_LIGHT,
        b"lLit" => blend::MODE_LINEAR_LIGHT,
        b"pLit" => blend::MODE_PIN_LIGHT,
        b"hMix" => blend::MODE_HARD_MIX,
        b"diff" => blend::MODE_DIFFERENCE,
        b"smud" => blend::MODE_EXCLUSION,
        b"hue " => blend::MODE_HUE,
        b"sat " => blend::MODE_SATURATION,
        b"colr" => blend::MODE_COLOR,
        b"lum " => blend::MODE_LUMINOSITY,
        _ => 0,
    }
}

fn decode_packbits(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < input.len() && out.len() < expected {
        let header = input[i] as i8;
        i += 1;
        if header >= 0 {
            let count = header as usize + 1;
            if i + count > input.len() {
                bail!("PackBits literal overruns channel");
            }
            out.extend_from_slice(&input[i..i + count]);
            i += count;
        } else if header != -128 {
            let count = (-(header as i16) + 1) as usize;
            let Some(&value) = input.get(i) else {
                bail!("PackBits repeat overruns channel");
            };
            i += 1;
            out.extend(std::iter::repeat(value).take(count));
        }
    }
    out.truncate(expected);
    if out.len() != expected {
        bail!(
            "PackBits channel decoded to {} bytes, expected {expected}",
            out.len()
        );
    }
    Ok(out)
}

/// Layer rectangles come from the file. Reject ones that cannot be decoded
/// within the documented budget before any buffer is allocated.
const MAX_LAYER_PIXELS: i64 = 1 << 28;

/// End of a section whose declared length may exceed the addressable bytes.
///
/// PSB section lengths are 64-bit, so a crafted file can declare a section that
/// wraps the byte cursor. Clamp to the file instead of trusting the addition.
fn section_end(start: usize, len: usize, limit: usize) -> Result<usize> {
    let end = start
        .checked_add(len)
        .context("PSD section length is out of range")?;
    Ok(end.min(limit))
}

fn checked_layer_size(left: i32, top: i32, right: i32, bottom: i32) -> Result<(u32, u32)> {
    let width = (i64::from(right) - i64::from(left)).max(0);
    let height = (i64::from(bottom) - i64::from(top)).max(0);
    let pixels = width.saturating_mul(height);
    if pixels > MAX_LAYER_PIXELS {
        bail!(
            "PSD layer is {width}x{height} ({pixels} pixels); the supported limit is {MAX_LAYER_PIXELS}"
        );
    }
    Ok((width as u32, height as u32))
}

fn decode_channel(data: &[u8], width: u32, height: u32, psb: bool) -> Result<Vec<u8>> {
    if data.len() < 2 {
        bail!("PSD channel has no compression tag");
    }
    let compression = u16::from_be_bytes([data[0], data[1]]);
    let payload = &data[2..];
    let expected = width as usize * height as usize;
    match compression {
        0 => {
            if payload.len() < expected {
                bail!("raw PSD channel is short: {} < {expected}", payload.len());
            }
            Ok(payload[..expected].to_vec())
        }
        1 => {
            let row_count = height as usize;
            let count_bytes = if psb { 4 } else { 2 };
            if payload.len() < row_count * count_bytes {
                bail!("RLE PSD channel is missing row lengths");
            }
            let mut counts = Vec::with_capacity(row_count);
            let mut at = 0;
            for _ in 0..row_count {
                let n = if psb {
                    let b = &payload[at..at + 4];
                    at += 4;
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
                } else {
                    let b = &payload[at..at + 2];
                    at += 2;
                    u16::from_be_bytes([b[0], b[1]]) as usize
                };
                counts.push(n);
            }
            let mut out = Vec::with_capacity(expected);
            for count in counts {
                if at + count > payload.len() {
                    bail!("RLE row overruns channel");
                }
                let row = decode_packbits(&payload[at..at + count], width as usize)?;
                out.extend_from_slice(&row);
                at += count;
            }
            Ok(out)
        }
        other => bail!("unsupported PSD channel compression {other}"),
    }
}

fn read_planar_image(
    data: &[u8],
    width: u32,
    height: u32,
    channels: u16,
    psb: bool,
    color_mode: u16,
) -> Result<Image> {
    if data.len() < 2 {
        bail!("PSD merged image has no compression tag");
    }
    let compression = u16::from_be_bytes([data[0], data[1]]);
    let used = usize::from(channels.min(4));
    let expected = width as usize * height as usize;
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(used);
    match compression {
        0 => {
            let need = expected * used;
            if data.len() < 2 + need {
                bail!("raw merged image is short");
            }
            for c in 0..used {
                let at = 2 + c * expected;
                planes.push(data[at..at + expected].to_vec());
            }
        }
        1 => {
            let mut at = 2;
            for _ in 0..used {
                let (plane, consumed) = decode_rle_payload(&data[at..], width, height, psb)?;
                planes.push(plane);
                at += consumed;
            }
        }
        other => bail!("unsupported merged PSD compression {other}"),
    }
    let mut out = Image::new(width, height);
    let plane_len = expected;
    for i in 0..plane_len {
        if color_mode == 4 {
            let c = planes.get(0).map_or(0, |p| p[i]);
            let m = planes.get(1).map_or(0, |p| p[i]);
            let y = planes.get(2).map_or(0, |p| p[i]);
            let k = planes.get(3).map_or(0, |p| p[i]);
            let rgb = crate::color::cmyk_to_rgb(
                c as f32 / 255.0,
                m as f32 / 255.0,
                y as f32 / 255.0,
                k as f32 / 255.0,
            );
            out.data[i * 4..i * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        } else {
            let r = planes.get(0).map_or(0, |p| p[i]);
            let g = planes.get(1).map_or(r, |p| p[i]);
            let b = planes.get(2).map_or(r, |p| p[i]);
            let a = planes.get(3).map_or(255, |p| p[i]);
            out.data[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, a]);
        }
    }
    Ok(out)
}

fn decode_rle_payload(data: &[u8], width: u32, height: u32, psb: bool) -> Result<(Vec<u8>, usize)> {
    let expected = width as usize * height as usize;
    let count_bytes = if psb { 4 } else { 2 };
    let mut counts = Vec::with_capacity(height as usize);
    let mut at = 0usize;
    for _ in 0..height {
        if at + count_bytes > data.len() {
            bail!("RLE channel is missing row lengths");
        }
        let n = if psb {
            let b = &data[at..at + 4];
            at += 4;
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
        } else {
            let b = &data[at..at + 2];
            at += 2;
            u16::from_be_bytes([b[0], b[1]]) as usize
        };
        counts.push(n);
    }
    let mut out = Vec::with_capacity(expected);
    for count in counts {
        if at + count > data.len() {
            bail!("RLE row overruns channel");
        }
        let row = decode_packbits(&data[at..at + count], width as usize)?;
        out.extend_from_slice(&row);
        at += count;
    }
    Ok((out, at))
}

fn decode_stream_channel(
    data: &[u8],
    width: u32,
    height: u32,
    compression: u16,
    psb: bool,
) -> Result<(Vec<u8>, usize)> {
    let expected = width as usize * height as usize;
    match compression {
        0 => {
            if data.len() < 2 + expected {
                bail!("raw layer channel is short");
            }
            Ok((data[2..2 + expected].to_vec(), 2 + expected))
        }
        1 => {
            let (plane, used) = decode_rle_payload(&data[2..], width, height, psb)?;
            Ok((plane, used + 2))
        }
        other => bail!("unsupported PSD channel compression {other}"),
    }
}

fn read_pascal_name(data: &[u8]) -> (String, usize) {
    if data.is_empty() {
        return (String::new(), 0);
    }
    let len = data[0] as usize;
    let end = (1 + len).min(data.len());
    let name = String::from_utf8_lossy(&data[1..end]).into_owned();
    let consumed = ((1 + len + 3) / 4) * 4;
    (name, consumed.min(data.len()))
}

/// Parse a layered PSD/PSB document.
pub fn read_psd(path: &Path) -> Result<PsdDocument> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() < 26 || &bytes[..4] != b"8BPS" {
        bail!("not a Photoshop PSD/PSB file");
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    let psb = match version {
        1 => false,
        2 => true,
        _ => bail!("unsupported PSD version {version}"),
    };
    let mut r = Reader::new(&bytes, psb);
    r.skip(12)?; // signature + version + reserved
    let channels = r.u16()?;
    let height = r.u32()?;
    let width = r.u32()?;
    let depth = r.u16()?;
    let color_mode = r.u16()?;
    if depth != 8 {
        bail!("only 8-bit PSD/PSB files are supported (got {depth}-bit)");
    }
    if color_mode != 3 && color_mode != 4 {
        bail!("only RGB and CMYK PSD/PSB files are supported (color mode {color_mode})");
    }
    // The header decides the merged-image allocation before any layer is read;
    // validate it with checked arithmetic instead of trusting the file.
    let header_pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("PSD dimensions overflow")?;
    if width == 0 || height == 0 || header_pixels > MAX_LAYER_PIXELS as u64 {
        bail!(
            "PSD canvas is {width}x{height} ({header_pixels} pixels); the supported limit is {MAX_LAYER_PIXELS}"
        );
    }
    let color_len = r.u32()? as usize;
    r.skip(color_len)?;
    let resources_len = r.u32()? as usize;
    let resources_end = section_end(r.pos, resources_len, bytes.len())?;
    let mut icc_profile = None;
    while r.pos + 12 <= resources_end {
        let signature = r.take(4)?.to_vec();
        let resource_id = r.u16()?;
        let name_len = r.u8()? as usize;
        // Pascal resource name, including the length byte, padded to even.
        let name_total = 1 + name_len;
        let name_padded = (name_total + 1) / 2 * 2;
        r.skip(name_padded.saturating_sub(1))?;
        let size = r.u32()? as usize;
        if section_end(r.pos, size, resources_end)? > resources_end {
            break;
        }
        let payload = r.take(size)?;
        if signature == b"8BIM" && resource_id == 1039 {
            icc_profile = Some(payload.to_vec());
        }
        if size % 2 == 1 && r.pos < resources_end {
            r.skip(1)?;
        }
    }
    r.pos = resources_end;

    let layer_mask_len = r.block_len()?;
    let layer_mask_end = section_end(r.pos, layer_mask_len, bytes.len())?;
    let layer_info_len = r.block_len()?;
    let layer_info_end = section_end(r.pos, layer_info_len, layer_mask_end)?;
    let count = r.i16()?.unsigned_abs() as usize;
    struct Record {
        name: String,
        top: i32,
        left: i32,
        bottom: i32,
        right: i32,
        opacity: f64,
        visible: bool,
        blend: i64,
        clip: bool,
        channels: Vec<(i16, usize)>,
        /// Layer mask rectangle from the extra-data mask block, if present.
        mask_rect: Option<(i32, i32, i32, i32)>,
        /// Mask default colour: 0 hides, 255 reveals outside the rectangle.
        mask_default: Option<u8>,
    }
    let mut groups_dropped = false;
    let mut unsupported_masks = false;
    let mut mask_notes: Vec<String> = Vec::new();
    let mut unknown_channels: std::collections::BTreeSet<i16> = std::collections::BTreeSet::new();
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let top = r.i32()?;
        let left = r.i32()?;
        let bottom = r.i32()?;
        let right = r.i32()?;
        let channel_count = r.u16()?;
        let mut chans = Vec::with_capacity(channel_count as usize);
        let mut declared = std::collections::BTreeSet::new();
        for _ in 0..channel_count {
            let id = r.i16()?;
            let len = r.block_len()?;
            // A repeated id makes the last plane silently win, so the record
            // is ambiguous: reject it instead of guessing which one is real.
            if !declared.insert(id) {
                bail!("PSD layer declares channel {id} twice");
            }
            chans.push((id, len));
        }
        let signature = r.take(4)?;
        if signature != b"8BIM" {
            bail!("PSD layer has invalid blend signature");
        }
        let key = r.take(4)?.to_vec();
        let opacity = r.u8()? as f64 / 255.0;
        let clipping = r.u8()?;
        let flags = r.u8()?;
        r.skip(1)?; // filler
        // The layer-record extra-data length stays 32-bit in PSB; only the
        // section and channel lengths widen to 64 bits.
        let extra_len = r.u32()? as usize;
        let extra = r.take(extra_len)?;
        let mut er = Reader::new(extra, psb);
        let mask_len = er.u32()? as usize;
        let mask_block = er.take(mask_len.min(er.remaining()))?;
        // 4 x i32 rectangle, default colour and flags. Anything after that is
        // either the real-user-mask header (only with a -3 channel) or mask
        // parameters, neither of which this reader models.
        let mask_rect = if mask_block.len() >= 18 {
            let read = |at: usize| {
                i32::from_be_bytes([
                    mask_block[at],
                    mask_block[at + 1],
                    mask_block[at + 2],
                    mask_block[at + 3],
                ])
            };
            Some((read(0), read(4), read(8), read(12)))
        } else {
            None
        };
        let mask_default = mask_block.get(16).copied();
        // Blending ranges length is always 32-bit in the documented layout.
        let ranges_len = er.u32()? as usize;
        er.skip(ranges_len)?;
        let name_start = er.pos;
        let (mut name, name_len) = read_pascal_name(&extra[name_start..]);
        er.pos = (name_start + name_len).min(extra.len());
        // Look for the Unicode name (`8BIMluni`) in additional layer data.
        while er.remaining() >= 12 {
            let sig = er.take(4)?;
            let k = er.take(4)?;
            let n = er.u32()? as usize;
            let payload = er.take(n.min(er.remaining()))?;
            if k == b"lsct" || k == b"lsdk" {
                groups_dropped = true;
            }
            if sig == b"8BIM" && k == b"luni" && payload.len() >= 4 {
                let units =
                    u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
                let body = &payload[4..];
                let mut utf16 = Vec::with_capacity(units);
                let mut i = 0;
                while i + 1 < body.len() && utf16.len() < units {
                    utf16.push(u16::from_be_bytes([body[i], body[i + 1]]));
                    i += 2;
                }
                while utf16.last() == Some(&0) {
                    utf16.pop();
                }
                if let Ok(decoded) = String::from_utf16(&utf16) {
                    name = decoded;
                }
            }
        }
        records.push(Record {
            name,
            top,
            left,
            bottom,
            right,
            opacity,
            // Adobe: bit 1 set means the layer is hidden.
            visible: flags & 2 == 0,
            blend: key_to_blend(&key),
            clip: clipping != 0,
            channels: chans,
            mask_rect,
            mask_default,
        });
    }
    // Layer channel data follows the layer records inside the layer-info block.
    let mut layers = Vec::with_capacity(records.len());
    for record in records {
        let (rw, rh) = checked_layer_size(record.left, record.top, record.right, record.bottom)?;
        let mut planes: [Option<Vec<u8>>; 4] = [None, None, None, None];
        // Transparency has its own slot: in CMYK documents channel 3 is K, and
        // using the alpha slot for it turned black into white.
        let mut alpha: Option<Vec<u8>> = None;
        let mut mask_plane: Option<Vec<u8>> = None;
        let mask_size = match record.mask_rect {
            Some((top, left, bottom, right)) => {
                let (mw, mh) = checked_layer_size(left, top, right, bottom)?;
                if mw == 0 || mh == 0 {
                    None
                } else {
                    Some((mw, mh))
                }
            }
            None => None,
        };
        for (id, len) in record.channels {
            if len < 2 || section_end(r.pos, len, layer_info_end)? > layer_info_end {
                bail!("PSD layer channel length is out of range");
            }
            let data = r.take(len)?;
            if id == -2 {
                // User layer mask: white reveals, which is the mask alpha slot.
                match mask_size {
                    Some((mw, mh)) => mask_plane = Some(decode_channel(data, mw, mh, psb)?),
                    None => {
                        unsupported_masks = true;
                    }
                }
                continue;
            }
            let plane = decode_channel(data, rw, rh, psb)?;
            match id {
                -1 => alpha = Some(plane),
                0..=3 => planes[id as usize] = Some(plane),
                other => {
                    unknown_channels.insert(other);
                }
            }
        }
        let mut image = Image::new(rw, rh);
        let mask = match (mask_plane, record.mask_rect) {
            (Some(plane), Some((top, left, bottom, right))) => {
                let (mw, mh) = checked_layer_size(left, top, right, bottom)?;
                let mut mask = Image::new(mw, mh);
                for (index, value) in plane.iter().take((mw * mh) as usize).enumerate() {
                    // PSD stores the mask as a coverage plane: 255 reveals.
                    mask.data[index * 4..index * 4 + 4].copy_from_slice(&[255, 255, 255, *value]);
                }
                Some((mask, (left, top)))
            }
            _ => None,
        };
        let len = rw as usize * rh as usize;
        for i in 0..len {
            if color_mode == 4 {
                let c = planes[0].as_ref().map_or(0, |p| p[i]);
                let m = planes[1].as_ref().map_or(0, |p| p[i]);
                let y = planes[2].as_ref().map_or(0, |p| p[i]);
                let k = planes[3].as_ref().map_or(0, |p| p[i]);
                let rgb = crate::color::cmyk_to_rgb(
                    c as f32 / 255.0,
                    m as f32 / 255.0,
                    y as f32 / 255.0,
                    k as f32 / 255.0,
                );
                let a = alpha.as_ref().map_or(255, |p| p[i]);
                image.data[i * 4..i * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
            } else {
                let red = planes[0].as_ref().map_or(0, |p| p[i]);
                let green = planes[1].as_ref().map_or(red, |p| p[i]);
                let blue = planes[2].as_ref().map_or(red, |p| p[i]);
                let a = alpha.as_ref().map_or(255, |p| p[i]);
                image.data[i * 4..i * 4 + 4].copy_from_slice(&[red, green, blue, a]);
            }
        }
        if record.mask_default == Some(255) {
            mask_notes.push(format!(
                "layer \"{}\" has a white mask default; outside the mask rectangle it is applied as hidden",
                record.name
            ));
        }
        let (mask, mask_offset) = match mask {
            Some((mask, offset)) => (Some(mask), offset),
            None => (None, (0, 0)),
        };
        layers.push(PsdLayer {
            name: record.name,
            image,
            offset: (record.left, record.top),
            opacity: record.opacity,
            visible: record.visible,
            blend: record.blend,
            clip: record.clip,
            mask,
            mask_offset,
        });
    }
    // The global layer-mask record lives inside the layer/mask section,
    // immediately after the layer-info payload.
    if r.pos + 4 <= layer_mask_end {
        let global_len = r.u32()? as usize;
        r.skip(global_len.min(layer_mask_end.saturating_sub(r.pos)))?;
    }
    r.pos = layer_mask_end.min(bytes.len());
    let mut unsupported = Vec::new();
    if groups_dropped {
        unsupported.push("group boundaries are flattened".to_string());
    }
    if unsupported_masks {
        unsupported.push("a layer mask channel has no usable mask rectangle".to_string());
    }
    for note in mask_notes {
        unsupported.push(note);
    }
    if !unknown_channels.is_empty() {
        let ids = unknown_channels
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        unsupported.push(format!("channels {ids} are dropped (masks or spot colours)"));
    }
    if r.pos + 2 <= bytes.len() {
        let merged =
            read_planar_image(&bytes[r.pos..], width, height, channels, psb, color_mode).ok();
        Ok(PsdDocument {
            unsupported,
            width,
            height,
            layers,
            merged,
            icc_profile,
        })
    } else {
        Ok(PsdDocument {
            width,
            height,
            layers,
            merged: None,
            icc_profile,
            unsupported,
        })
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn put_len(out: &mut Vec<u8>, value: usize, psb: bool) {
    if psb {
        put_u64(out, value as u64);
    } else {
        put_u32(out, value as u32);
    }
}
fn put_pascal_padded(out: &mut Vec<u8>, name: &str) {
    let bytes = name.as_bytes();
    let len = bytes.len().min(255);
    out.push(len as u8);
    out.extend_from_slice(&bytes[..len]);
    let total = 1 + len;
    let padded = (total + 3) / 4 * 4;
    out.extend(std::iter::repeat(0).take(padded - total));
}
fn put_luni(out: &mut Vec<u8>, name: &str) {
    let units: Vec<u16> = name.encode_utf16().collect();
    // The declared count excludes any terminator; padding fills the block.
    // psd-tools reads the count literally, so a trailing NUL here showed up
    // inside the layer name of every exported file.
    let size = 4 + units.len() * 2;
    out.extend_from_slice(b"8BIMluni");
    put_u32(out, size as u32);
    put_u32(out, units.len() as u32);
    for unit in units {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    if size % 4 != 0 {
        out.extend(std::iter::repeat(0).take(4 - size % 4));
    }
}

/// Write a layered PSD/PSB document. Layers are supplied bottom-to-top.
pub fn write_psd_layers(
    path: &Path,
    width: u32,
    height: u32,
    layers: &[PsdLayerData<'_>],
    merged: &Image,
    psb: bool,
) -> Result<()> {
    write_psd_layers_with_icc(path, width, height, layers, merged, psb, None)
}

/// Same as `write_psd_layers`, embedding an ICC profile as image resource 1039
/// so a document opened from PSD keeps its colour profile on save.
pub fn write_psd_layers_with_icc(
    path: &Path,
    width: u32,
    height: u32,
    layers: &[PsdLayerData<'_>],
    merged: &Image,
    psb: bool,
    icc: Option<&[u8]>,
) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("cannot write an empty PSD");
    }
    if merged.width != width || merged.height != height {
        bail!("merged image does not match the PSD canvas");
    }
    if (psb && (width > 300_000 || height > 300_000))
        || (!psb && (width > 30_000 || height > 30_000))
    {
        bail!("document exceeds the PSD/PSB dimension limit");
    }
    for layer in layers {
        if layer.image.width == 0 || layer.image.height == 0 {
            bail!("PSD layers cannot be empty");
        }
    }

    let mut records = Vec::new();
    let mut channel_data = Vec::new();
    for layer in layers {
        let w = layer.image.width;
        let h = layer.image.height;
        records.extend_from_slice(&layer.offset.1.to_be_bytes());
        records.extend_from_slice(&layer.offset.0.to_be_bytes());
        records.extend_from_slice(&((layer.offset.1 as i64 + h as i64) as i32).to_be_bytes());
        records.extend_from_slice(&((layer.offset.0 as i64 + w as i64) as i32).to_be_bytes());
        let mask = layer
            .mask
            .filter(|mask| mask.width > 0 && mask.height > 0);
        let channel_count: u16 = if mask.is_some() { 5 } else { 4 };
        records.extend_from_slice(&channel_count.to_be_bytes());
        let channel_len = 2 + w as usize * h as usize;
        for id in [-1i16, 0, 1, 2] {
            records.extend_from_slice(&id.to_be_bytes());
            put_len(&mut records, channel_len, psb);
        }
        if let Some(mask) = mask {
            records.extend_from_slice(&(-2i16).to_be_bytes());
            put_len(
                &mut records,
                2 + mask.width as usize * mask.height as usize,
                psb,
            );
        }
        records.extend_from_slice(b"8BIM");
        records.extend_from_slice(blend_to_key(layer.blend));
        records.push((layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
        records.push(if layer.clip { 1 } else { 0 });
        // Bit 1 set means hidden (psd-tools reads `not flags & 2`).
        records.push(if layer.visible { 0 } else { 2 });
        records.push(0);
        let mut extra = Vec::new();
        // Layer mask data stays 32-bit even in PSB. The block is a rectangle,
        // a default colour and flags; the pixels travel in channel -2.
        match mask {
            Some(mask) => {
                put_u32(&mut extra, 18);
                extra.extend_from_slice(&layer.mask_offset.1.to_be_bytes());
                extra.extend_from_slice(&layer.mask_offset.0.to_be_bytes());
                extra.extend_from_slice(
                    &((i64::from(layer.mask_offset.1) + i64::from(mask.height)) as i32)
                        .to_be_bytes(),
                );
                extra.extend_from_slice(
                    &((i64::from(layer.mask_offset.0) + i64::from(mask.width)) as i32)
                        .to_be_bytes(),
                );
                extra.push(0); // default colour: black
                extra.push(0); // flags
            }
            None => put_u32(&mut extra, 0),
        }
        put_u32(&mut extra, 0); // blending ranges
        put_pascal_padded(&mut extra, layer.name);
        put_luni(&mut extra, layer.name);
        put_u32(&mut records, extra.len() as u32); // extra-data length (32-bit even in PSB)
        records.extend_from_slice(&extra);

        // Channel payloads follow the record order: alpha, red, green, blue.
        for channel in [-1i16, 0, 1, 2] {
            channel_data.extend_from_slice(&0u16.to_be_bytes());
            let index = match channel {
                -1 => 3,
                0 => 0,
                1 => 1,
                _ => 2,
            };
            for px in layer.image.data.chunks_exact(4) {
                channel_data.push(px[index]);
            }
        }
        if let Some(mask) = mask {
            channel_data.extend_from_slice(&0u16.to_be_bytes());
            for px in mask.data.chunks_exact(4) {
                channel_data.push(px[3]);
            }
        }
    }

    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&(layers.len() as i16).to_be_bytes());
    layer_info.extend_from_slice(&records);
    layer_info.extend_from_slice(&channel_data);
    layer_info.extend_from_slice(&0u32.to_be_bytes()); // global layer mask info

    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&(if psb { 2u16 } else { 1u16 }).to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    put_u32(&mut out, 0); // colour mode data
    let mut resources = Vec::new();
    if let Some(profile) = icc {
        resources.extend_from_slice(b"8BIM");
        resources.extend_from_slice(&1039u16.to_be_bytes());
        // Pascal resource name: empty, padded to an even length.
        resources.extend_from_slice(&[0, 0]);
        put_u32(&mut resources, profile.len() as u32);
        resources.extend_from_slice(profile);
        if profile.len() % 2 == 1 {
            resources.push(0);
        }
    }
    put_u32(&mut out, resources.len() as u32);
    out.extend_from_slice(&resources);
    let mut layer_mask = Vec::new();
    put_len(&mut layer_mask, layer_info.len(), psb);
    layer_mask.extend_from_slice(&layer_info);
    put_len(&mut out, layer_mask.len(), psb);
    out.extend_from_slice(&layer_mask);

    out.extend_from_slice(&0u16.to_be_bytes()); // merged raw compression
    for channel in 0..4 {
        for px in merged.data.chunks_exact(4) {
            out.push(px[channel]);
        }
    }
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_round_trips_literals_and_repeats() {
        let encoded = [2u8, 7, 8, 9, 0xfe, 42];
        let decoded = decode_packbits(&encoded, 6).unwrap();
        assert_eq!(decoded, [7, 8, 9, 42, 42, 42]);
    }

    /// One 2x2 layer whose record declares exactly `ids` as channel ids, each
    /// with a raw 2x2 plane. Hand-built so a malformed record can be expressed
    /// without a writer that refuses to produce one.
    fn psd_with_layer_channels(ids: &[i16]) -> Vec<u8> {
        psd_with_mask_block(ids, None)
    }

    /// Same hand-built layer, with an optional 18-byte mask block.
    fn psd_with_mask_block(ids: &[i16], mask_block: Option<[u8; 18]>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"8BPS");
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        out.extend_from_slice(&(ids.len() as u16).to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes()); // height
        out.extend_from_slice(&2u32.to_be_bytes()); // width
        out.extend_from_slice(&8u16.to_be_bytes());
        out.extend_from_slice(&3u16.to_be_bytes()); // RGB
        out.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
        out.extend_from_slice(&0u32.to_be_bytes()); // image resources

        let mut info = Vec::new();
        info.extend_from_slice(&1i16.to_be_bytes()); // one layer
        for value in [0i32, 0, 2, 2] {
            info.extend_from_slice(&value.to_be_bytes()); // top, left, bottom, right
        }
        info.extend_from_slice(&(ids.len() as u16).to_be_bytes());
        for id in ids {
            info.extend_from_slice(&id.to_be_bytes());
            info.extend_from_slice(&6u32.to_be_bytes()); // 2-byte tag + 4 raw bytes
        }
        info.extend_from_slice(b"8BIM");
        info.extend_from_slice(b"norm");
        info.extend_from_slice(&[255, 0, 0, 0]); // opacity, clipping, flags, filler
        let mask_extra = mask_block.map_or(0, |_| 18);
        info.extend_from_slice(&(10u32 + mask_extra).to_be_bytes()); // extra data length
        match mask_block {
            Some(block) => {
                info.extend_from_slice(&18u32.to_be_bytes());
                info.extend_from_slice(&block);
            }
            None => info.extend_from_slice(&0u32.to_be_bytes()),
        }
        info.extend_from_slice(&0u32.to_be_bytes()); // blending ranges
        info.extend_from_slice(&[0u8, 0u8]); // empty pascal name
        for _ in ids {
            info.extend_from_slice(&0u16.to_be_bytes()); // raw compression
            info.extend_from_slice(&[9u8, 9, 9, 9]);
        }

        out.extend_from_slice(&(info.len() as u32 + 4).to_be_bytes());
        out.extend_from_slice(&(info.len() as u32).to_be_bytes());
        out.extend_from_slice(&info);
        out.extend_from_slice(&0u32.to_be_bytes()); // global layer mask info
        out
    }

    #[test]
    fn a_section_length_past_the_end_of_memory_is_rejected() {
        // PSB section lengths are 64-bit, so a crafted file can declare a
        // section that overflows the byte cursor. That must be an error, not a
        // panic or a wrapped position.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height
        bytes.extend_from_slice(&1u32.to_be_bytes()); // width
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
        bytes.extend_from_slice(&u64::MAX.to_be_bytes()); // layer-and-mask length
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overflow.psb");
        std::fs::write(&path, bytes).unwrap();
        let error = read_psd(&path).unwrap_err().to_string();
        assert!(
            error.contains("out of range") || error.contains("truncated"),
            "a section beyond the file must be named, got: {error}"
        );
    }

    #[test]
    fn psb_layer_mask_round_trips() {
        // PSB writes the -2 channel length as 64-bit; the mask block itself
        // stays 32-bit, so both widths are exercised by one file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mask.psb");
        let image = Image::filled(8, 6, [30, 60, 90, 255]);
        let mut mask = Image::filled(4, 3, [255, 255, 255, 255]);
        mask.data[3] = 0;
        let layers = [PsdLayerData {
            name: "Masked",
            image: PsdImageRef {
                width: 8,
                height: 6,
                data: &image.data,
            },
            offset: (0, 0),
            opacity: 1.0,
            visible: true,
            blend: 0,
            clip: false,
            mask: Some(PsdImageRef {
                width: 4,
                height: 3,
                data: &mask.data,
            }),
            mask_offset: (1, 2),
        }];
        write_psd_layers(&path, 8, 6, &layers, &image, true).unwrap();
        let parsed = read_psd(&path).unwrap();
        let layer = &parsed.layers[0];
        assert_eq!(layer.mask_offset, (1, 2));
        let mask = layer.mask.as_ref().expect("the PSB mask must survive");
        assert_eq!((mask.width, mask.height), (4, 3));
        assert_eq!(mask.data[3], 0);
        assert_eq!(mask.data[4 + 3], 255);
    }

    #[test]
    fn a_white_mask_default_is_announced() {
        let mut block = [0u8; 18];
        block[8..12].copy_from_slice(&2i32.to_be_bytes()); // bottom
        block[12..16].copy_from_slice(&2i32.to_be_bytes()); // right
        block[16] = 255; // white default: reveals outside the rectangle
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("white-default.psd");
        std::fs::write(
            &path,
            psd_with_mask_block(&[0, 1, 2, -1, -2], Some(block)),
        )
        .unwrap();
        let parsed = read_psd(&path).expect("the file parses");
        assert!(parsed.layers[0].mask.is_some(), "the mask plane is kept");
        assert!(
            parsed
                .unsupported
                .iter()
                .any(|note| note.contains("white mask default")),
            "a white default changes the area outside the rectangle: {:?}",
            parsed.unsupported
        );
    }

    #[test]
    fn a_mask_channel_without_a_rectangle_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maskless.psd");
        // The record carries a -2 plane but its mask block is empty, so the
        // pixels cannot be placed: that must be announced, not dropped.
        std::fs::write(&path, psd_with_layer_channels(&[0, 1, 2, -1, -2])).unwrap();
        let parsed = read_psd(&path).expect("the file still parses");
        assert!(!parsed.layers.is_empty());
        assert!(
            parsed
                .unsupported
                .iter()
                .any(|note| note.contains("no usable mask rectangle")),
            "a placed-less mask channel must be reported: {:?}",
            parsed.unsupported
        );
        assert!(parsed.layers[0].mask.is_none());
    }

    #[test]
    fn duplicate_layer_channel_ids_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicate.psd");
        std::fs::write(&path, psd_with_layer_channels(&[0, 0, 1, 2])).unwrap();
        let error = read_psd(&path).unwrap_err().to_string();
        assert!(
            error.contains("twice"),
            "a repeated channel id must be named, got: {error}"
        );

        let path = dir.path().join("distinct.psd");
        std::fs::write(&path, psd_with_layer_channels(&[0, 1, 2, -1])).unwrap();
        let parsed = read_psd(&path).expect("distinct channel ids are valid");
        assert_eq!(parsed.layers.len(), 1);
    }

    #[test]
    fn oversized_header_dimensions_are_rejected() {
        // 8BPS v1, 4 channels, 60000x60000, 8-bit RGB, no colour data.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&4u16.to_be_bytes());
        bytes.extend_from_slice(&60_000u32.to_be_bytes());
        bytes.extend_from_slice(&60_000u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.psd");
        std::fs::write(&path, bytes).unwrap();
        let error = read_psd(&path).unwrap_err().to_string();
        assert!(error.contains("supported limit"), "{error}");
    }

    #[test]
    fn external_psb_is_read_through_the_wide_length_fields() {
        // Written by ImageMagick 7 (independent implementation): a 6x4 PSB,
        // version 2, so every section length is 64-bit.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/external_single.psb");
        let parsed = read_psd(&path).expect("read the external PSB");
        assert_eq!((parsed.width, parsed.height), (6, 4));
        let merged = parsed.merged.as_ref().expect("merged image");
        assert_eq!(&merged.data[0..4], &[200, 30, 40, 255]);
    }

    #[test]
    fn external_16_bit_psb_is_rejected_with_its_depth() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/external_16bit.psb");
        let error = read_psd(&path).unwrap_err().to_string();
        assert!(
            error.contains("16-bit"),
            "a 16-bit document must be named, got: {error}"
        );
    }

    #[test]
    fn external_cmyk_layer_keeps_the_k_channel() {
        // ImageMagick wrote cmyk(0,0,0,255): pure black. Mapping channel 3 into
        // the alpha slot dropped K and produced white.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/external_cmyk.psd");
        let parsed = read_psd(&path).expect("read the external CMYK PSD");
        let image = &parsed.layers[0].image;
        let pixel = &image.data[0..4];
        assert_eq!(&pixel[..3], &[0, 0, 0], "K must reach the CMYK conversion");
        assert_eq!(pixel[3], 255);
    }

    #[test]
    fn oversized_layer_rectangle_is_rejected_before_allocating() {
        let error = checked_layer_size(0, 0, 60_000, 60_000).unwrap_err();
        assert!(error.to_string().contains("supported limit"), "{error}");
        // A rectangle with no area stays an empty layer, not an error.
        assert_eq!(checked_layer_size(10, 10, 5, 5).unwrap(), (0, 0));
    }

    #[test]
    fn exported_unicode_name_has_no_trailing_nul() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("names.psd");
        let image = Image::filled(4, 4, [1, 2, 3, 255]);
        let layers = [PsdLayerData {
            name: "Papel",
            image: PsdImageRef {
                width: 4,
                height: 4,
                data: &image.data,
            },
            offset: (0, 0),
            opacity: 1.0,
            visible: true,
            blend: 0,
            clip: false,
            mask: None,
            mask_offset: (0, 0),
        }];
        write_psd_layers(&path, 4, 4, &layers, &image, false).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let luni = bytes
            .windows(8)
            .position(|window| window == b"8BIMluni")
            .expect("unicode name block");
        // Signature is 8 bytes, then the block size, then the character count.
        let count = u32::from_be_bytes([
            bytes[luni + 12],
            bytes[luni + 13],
            bytes[luni + 14],
            bytes[luni + 15],
        ]);
        assert_eq!(count, 5, "the count must exclude the terminator");
        let parsed = read_psd(&path).unwrap();
        assert_eq!(parsed.layers[0].name, "Papel");
    }

    #[test]
    fn layer_mask_round_trips_through_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mask.psd");
        let image = Image::filled(8, 6, [30, 60, 90, 255]);
        let mut mask = Image::filled(4, 3, [255, 255, 255, 255]);
        for y in 0..3 {
            for x in 0..4 {
                let alpha = if x >= 2 {
                    0
                } else if x == 1 {
                    128
                } else {
                    255
                };
                mask.data[((y * 4 + x) * 4 + 3) as usize] = alpha;
            }
        }
        let layers = [PsdLayerData {
            name: "Masked",
            image: PsdImageRef {
                width: 8,
                height: 6,
                data: &image.data,
            },
            offset: (0, 0),
            opacity: 1.0,
            visible: true,
            blend: 0,
            clip: false,
            mask: Some(PsdImageRef {
                width: 4,
                height: 3,
                data: &mask.data,
            }),
            mask_offset: (2, 1),
        }];
        write_psd_layers(&path, 8, 6, &layers, &image, false).unwrap();
        let parsed = read_psd(&path).unwrap();
        let layer = &parsed.layers[0];
        assert_eq!(layer.mask_offset, (2, 1), "the mask keeps its own origin");
        let mask = layer.mask.as_ref().expect("the layer mask must survive");
        assert_eq!((mask.width, mask.height), (4, 3));
        assert_eq!(mask.data[3], 255, "the left half reveals");
        assert_eq!(mask.data[2 * 4 + 3], 0, "the right half hides");
        assert_eq!(
            mask.data[1 * 4 + 3],
            128,
            "fractional mask coverage must survive, not just black and white"
        );
        assert!(
            !parsed
                .unsupported
                .iter()
                .any(|note| note.contains("channels")),
            "a supported mask must not be reported as a dropped channel: {:?}",
            parsed.unsupported
        );
    }

    #[test]
    fn exported_icc_profile_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icc.psd");
        let image = Image::filled(4, 4, [9, 9, 9, 255]);
        let layers = [PsdLayerData {
            name: "Layer",
            image: PsdImageRef {
                width: 4,
                height: 4,
                data: &image.data,
            },
            offset: (0, 0),
            opacity: 1.0,
            visible: true,
            blend: 0,
            clip: false,
            mask: None,
            mask_offset: (0, 0),
        }];
        // A header that passes `IccProfile::parse` plus a body byte.
        let mut profile = vec![0u8; 132];
        profile[0..4].copy_from_slice(&132u32.to_be_bytes());
        profile[12..16].copy_from_slice(b"mntr");
        profile[16..20].copy_from_slice(b"RGB ");
        profile[20..24].copy_from_slice(b"XYZ ");
        profile[36..40].copy_from_slice(b"acsp");
        profile[131] = 7;
        write_psd_layers_with_icc(&path, 4, 4, &layers, &image, false, Some(&profile)).unwrap();
        let parsed = read_psd(&path).unwrap();
        assert_eq!(
            parsed.icc_profile.as_deref(),
            Some(profile.as_slice()),
            "the exported PSD must carry its ICC profile"
        );
    }

    #[test]
    fn external_psd_visible_flag_is_respected() {
        // Authored with psd-tools 1.26.0, an independent implementation: two
        // overlapping layers, the top one (Blue) hidden. Its own reader reports
        // Red visible at (0,0) and Blue hidden at (8,8).
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/external_hidden_layer.psd");
        let parsed = read_psd(&path).expect("read the external PSD");
        let names: Vec<&str> = parsed.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Red", "Blue"], "stacking order must match the file");
        assert!(parsed.layers[0].visible, "Red is visible in the external file");
        assert!(!parsed.layers[1].visible, "Blue is hidden in the external file");
        assert_eq!(parsed.layers[1].offset, (8, 8));
    }

    #[test]
    fn layered_psd_round_trips_names_pixels_and_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layers.psd");
        let mut bottom = Image::new(8, 6);
        bottom
            .data
            .chunks_exact_mut(4)
            .for_each(|p| p.copy_from_slice(&[10, 20, 30, 255]));
        let mut top = Image::new(4, 3);
        for (i, px) in top.data.chunks_exact_mut(4).enumerate() {
            px.copy_from_slice(&[(i * 7) as u8, 200, 40, if i % 3 == 0 { 128 } else { 255 }]);
        }
        let merged = bottom.clone();
        let layers = [
            PsdLayerData {
                name: "Base",
                image: PsdImageRef {
                    width: 8,
                    height: 6,
                    data: &bottom.data,
                },
                offset: (0, 0),
                opacity: 1.0,
                visible: true,
                blend: 0,
                clip: false,
                mask: None,
                mask_offset: (0, 0),
            },
            PsdLayerData {
                name: "Detail é",
                image: PsdImageRef {
                    width: 4,
                    height: 3,
                    data: &top.data,
                },
                offset: (2, 1),
                opacity: 0.6,
                visible: false,
                blend: blend::MODE_MULTIPLY,
                clip: true,
                mask: None,
                mask_offset: (0, 0),
            },
        ];
        write_psd_layers(&path, 8, 6, &layers, &merged, false).unwrap();
        let parsed = read_psd(&path).unwrap();
        assert_eq!((parsed.width, parsed.height), (8, 6));
        assert_eq!(parsed.layers.len(), 2);
        assert_eq!(parsed.layers[0].name, "Base");
        assert_eq!(parsed.layers[1].name, "Detail é");
        assert_eq!(parsed.layers[1].offset, (2, 1));
        assert!(!parsed.layers[1].visible);
        assert!((parsed.layers[1].opacity - 0.6).abs() < 0.01);
        assert_eq!(parsed.layers[1].blend, blend::MODE_MULTIPLY);
        assert!(parsed.layers[1].clip);
        assert_eq!(parsed.layers[1].image.data[(1 * 4 + 1) * 4 + 2], 40);
        assert!(parsed.merged.is_some());
    }
}
