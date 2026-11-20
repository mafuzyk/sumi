//! CSP colour sets.
//!
//! CSP stores the palettes shown in its Color Set palette in two places:
//!
//! * installed `.cls` files under `Settings/PAINT/ColorSet/<language>/`;
//! * the user's active sets in `default.pcs`, an SQLite database whose
//!   `colorset` table holds the set name and a binary blob.
//!
//! Both blobs are little/big-endian variants of the same record format: a
//! colour count followed by RGBA records, with an optional per-colour name.
//! The `.pcs` blob stores each channel as a big-endian u32 whose bytes are all
//! equal (255 -> `ff ff ff ff`) and appends a zero separator; `.cls` stores a
//! straight RGBA byte quadruple and little-endian lengths.

use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OpenFlags};

/// One CSP colour set: a display name and straight-alpha RGBA colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorSet {
    pub name: String,
    pub colors: Vec<[u8; 4]>,
}

impl ColorSet {
    /// Number of columns CSP uses for the default set (48 colours, 8 x 6).
    pub fn columns(&self) -> usize {
        if self.colors.len() == 48 {
            8
        } else {
            7
        }
    }
}

/// Load a `.pcs` database or a `.cls` palette, choosing by extension.
pub fn load_color_sets(path: impl AsRef<Path>) -> Result<Vec<ColorSet>> {
    let path = path.as_ref();
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "pcs" => load_pcs(path),
        "cls" => Ok(vec![load_cls(path)?]),
        _ => bail!("unsupported colour set format: {}", path.display()),
    }
}

/// Read every set from CSP's `default.pcs` database.
pub fn load_pcs(path: impl AsRef<Path>) -> Result<Vec<ColorSet>> {
    let path = path.as_ref();
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open colour set database {}", path.display()))?;
    let mut stmt = db.prepare(
        "select colorsetname, colorsetdata from colorset order by colorsetindex, _PW_ID",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: Option<String> = row.get(0)?;
        let data: Option<Vec<u8>> = row.get(1)?;
        Ok((name, data))
    })?;
    let mut sets = Vec::new();
    for row in rows {
        let (name, data) = row?;
        let Some(data) = data else { continue };
        let set = parse_pcs_blob(name.as_deref().unwrap_or("Color set"), &data)?;
        if !set.colors.is_empty() {
            sets.push(set);
        }
    }
    if sets.is_empty() {
        bail!("no colour sets in {}", path.display());
    }
    Ok(sets)
}

/// Parse the `colorsetdata` blob from `default.pcs`.
pub fn parse_pcs_blob(name: &str, data: &[u8]) -> Result<ColorSet> {
    if data.len() < 4 {
        bail!("colour set blob is too short");
    }
    let count = be_u32(data, 0)? as usize;
    if count > 100_000 {
        bail!("unreasonable colour count {count}");
    }
    let mut offset = 4usize;
    let mut colors = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = be_u32(data, offset)? as usize;
        offset += 4;
        if offset + name_len + 13 > data.len() {
            bail!("colour set record runs past the end of the blob");
        }
        // Names are informational; the colour is what we display.
        offset += name_len;
        let r = be_u32(data, offset)? as u8;
        let g = be_u32(data, offset + 4)? as u8;
        let b = be_u32(data, offset + 8)? as u8;
        // Byte 12 is a zero separator; CSP keeps alpha implicit at 255.
        offset += 13;
        colors.push([r, g, b, 255]);
    }
    Ok(ColorSet {
        name: name.to_string(),
        colors,
    })
}

/// Read a standalone `.cls` palette from CSP's `Settings/PAINT/ColorSet`.
pub fn load_cls(path: impl AsRef<Path>) -> Result<ColorSet> {
    let path = path.as_ref();
    let data = std::fs::read(path)
        .with_context(|| format!("read colour set {}", path.display()))?;
    let fallback = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Color set".into());
    parse_cls(&data, &fallback)
        .with_context(|| format!("parse colour set {}", path.display()))
}

/// Parse an `SLCC` palette. `fallback_name` is used when the header name is
/// absent.
pub fn parse_cls(data: &[u8], fallback_name: &str) -> Result<ColorSet> {
    if data.len() < 64 || &data[..4] != b"SLCC" {
        bail!("not an SLCC colour set");
    }
    let header_name = read_cls_name(data, 10).filter(|name| !name.is_empty());
    let name = header_name.unwrap_or_else(|| fallback_name.to_string());

    // The header length varies with the palette name. Find the one offset
    // whose length-prefixed records tile the file exactly.
    for start in 40..96usize.min(data.len()) {
        if let Some(colors) = parse_cls_records(data, start) {
            if colors.len() >= 2 {
                return Ok(ColorSet { name, colors });
            }
        }
    }
    bail!("no colour records found in SLCC set")
}

fn parse_cls_records(data: &[u8], start: usize) -> Option<Vec<[u8; 4]>> {
    let mut offset = start;
    let mut colors = Vec::new();
    while offset + 4 <= data.len() {
        let len = le_u32(data, offset)? as usize;
        if !(8..=256).contains(&len) || offset + 4 + len > data.len() {
            return None;
        }
        let payload = &data[offset + 4..offset + 4 + len];
        colors.push([payload[0], payload[1], payload[2], payload[3]]);
        offset += 4 + len;
        if offset == data.len() {
            return Some(colors);
        }
    }
    None
}

fn read_cls_name(data: &[u8], offset: usize) -> Option<String> {
    let len = le_u16(data, offset)? as usize;
    if len == 0 || offset + 2 + len > data.len() {
        return None;
    }
    let raw = &data[offset + 2..offset + 2 + len];
    let name = String::from_utf8(raw.to_vec()).ok()?;
    if name
        .chars()
        .all(|c| !c.is_control() && (c.is_ascii_graphic() || c == ' '))
    {
        Some(name)
    } else {
        None
    }
}

fn be_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow::anyhow!("truncated colour set"))?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

fn le_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn le_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcs_blob(colors: &[(u8, u8, u8)], names: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(colors.len() as u32).to_be_bytes());
        for (i, (r, g, b)) in colors.iter().enumerate() {
            let name = names.get(i).copied().unwrap_or("");
            out.extend_from_slice(&(name.len() as u32).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            for channel in [*r, *g, *b] {
                out.extend_from_slice(&[channel; 4]);
            }
            out.push(0);
        }
        out
    }

    #[test]
    fn reads_pcs_colours_and_names() {
        let blob = pcs_blob(&[(0, 0, 0), (255, 50, 100)], &["Preto", "Rosa"]);
        let set = parse_pcs_blob("Padrão", &blob).unwrap();
        assert_eq!(set.name, "Padrão");
        assert_eq!(
            set.colors,
            vec![[0, 0, 0, 255], [255, 50, 100, 255]]
        );
    }

    #[test]
    fn pcs_with_a_truncated_record_is_rejected() {
        let mut blob = pcs_blob(&[(1, 2, 3)], &["x"]);
        blob.truncate(blob.len() - 1);
        assert!(parse_pcs_blob("bad", &blob).is_err());
    }

    #[test]
    fn reads_cls_records() {
        // Header modelled on `001Start.cls`, followed by one named and one
        // unnamed record.
        let mut data = Vec::new();
        data.extend_from_slice(b"SLCC");
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&42u32.to_le_bytes());
        data.extend_from_slice(&6u16.to_le_bytes());
        data.extend_from_slice(b"My set");
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(&6u16.to_le_bytes());
        data.extend_from_slice(b"My set");
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&32u32.to_le_bytes());
        // Named record: RGBA + type + byte length + UTF-16LE "Black".
        let mut named = vec![0, 0, 0, 255];
        named.extend_from_slice(&1u32.to_le_bytes());
        named.extend_from_slice(&10u16.to_le_bytes());
        named.extend_from_slice(&"Black".encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        data.extend_from_slice(&(named.len() as u32).to_le_bytes());
        data.extend_from_slice(&named);
        // Unnamed record.
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&[255, 255, 255, 255, 0, 0, 0, 0]);
        let set = parse_cls(&data, "fallback").unwrap();
        assert_eq!(set.name, "My set");
        assert_eq!(set.colors, vec![[0, 0, 0, 255], [255, 255, 255, 255]]);
    }
}
