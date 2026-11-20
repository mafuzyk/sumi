//! CSFCHUNK container used by CLIP STUDIO PAINT `.clip` files.
//!
//! Layout (all integers big-endian):
//! ```text
//! "CSFCHUNK" | u64 total_size | u64 24 | "CHNKHead" | u64 40
//!   head: u64 version, u64 sqlite_offset, u64 id_len, u8[id_len] id
//! chunks...
//! "CHNKFoot" | u64 0
//! ```
//! A chunk is `tag: [u8;8] | len: u64 | payload: [u8; len]`.
//! `CHNKExta` payload is `u64 id_len, id, u64 data_len, data`.

use std::collections::HashMap;
use std::ops::Range;

use crate::{Error, Result};

pub const MAGIC: &[u8; 8] = b"CSFCHUNK";
pub const HEAD_TAG: &[u8; 8] = b"CHNKHead";
pub const EXTA_TAG: &[u8; 8] = b"CHNKExta";
pub const SQLI_TAG: &[u8; 8] = b"CHNKSQLi";
pub const FOOT_TAG: &[u8; 8] = b"CHNKFoot";

#[derive(Debug, Clone)]
pub struct Head {
    pub version: u64,
    pub sqlite_offset: u64,
    pub identifier: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ExtaChunk {
    pub id: String,
    pub data: Range<usize>,
    pub offset: usize,
}

#[derive(Debug, Default)]
pub struct Container {
    pub head: Head,
    pub exta: Vec<ExtaChunk>,
    pub by_id: HashMap<String, usize>,
    pub sqlite: Range<usize>,
}

impl Default for Head {
    fn default() -> Self {
        Head { version: 256, sqlite_offset: 0, identifier: Vec::new() }
    }
}

fn be_u64(b: &[u8], at: usize) -> Result<u64> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes(s.try_into().unwrap()))
        .ok_or_else(|| Error::format(format!("truncated u64 at {at}")))
}

pub fn parse(data: &[u8]) -> Result<Container> {
    if data.len() < 80 {
        return Err(Error::format("file too small"));
    }
    if &data[0..8] != MAGIC {
        return Err(Error::format("not a CSFCHUNK file"));
    }
    let total = be_u64(data, 8)? as usize;
    if total != data.len() {
        return Err(Error::format(format!(
            "declared size {total} != actual {}",
            data.len()
        )));
    }
    if &data[24..32] != HEAD_TAG {
        return Err(Error::format("missing CHNKHead"));
    }
    let head_len = be_u64(data, 32)? as usize;
    if head_len < 40 || 40 + head_len > data.len() {
        return Err(Error::format("bad head length"));
    }
    let head_body = &data[40..40 + head_len];
    let version = u64::from_be_bytes(head_body[0..8].try_into().unwrap());
    let sqlite_offset = u64::from_be_bytes(head_body[8..16].try_into().unwrap());
    let id_len = u64::from_be_bytes(head_body[16..24].try_into().unwrap()) as usize;
    if 24 + id_len > head_body.len() {
        return Err(Error::format("bad identifier length"));
    }
    let identifier = head_body[24..24 + id_len].to_vec();

    let mut out = Container {
        head: Head { version, sqlite_offset, identifier },
        ..Default::default()
    };
    let mut off = 40 + head_len;
    loop {
        if off + 16 > data.len() {
            return Err(Error::format("chunk stream ends without CHNKFoot"));
        }
        let tag = &data[off..off + 8];
        let len = be_u64(data, off + 8)? as usize;
        let body = off + 16;
        if body + len > data.len() {
            return Err(Error::format(format!(
                "chunk {:?} at {off} overruns file",
                String::from_utf8_lossy(tag)
            )));
        }
        if tag == SQLI_TAG {
            out.sqlite = body..body + len;
        } else if tag == EXTA_TAG {
            let payload = &data[body..body + len];
            let id_len = be_u64(payload, 0)? as usize;
            if 8 + id_len + 8 > payload.len() {
                return Err(Error::format("bad CHNKExta header"));
            }
            let id = String::from_utf8_lossy(&payload[8..8 + id_len]).into_owned();
            let data_len = be_u64(payload, 8 + id_len)? as usize;
            let start = body + 8 + id_len + 8;
            if start + data_len > body + len {
                return Err(Error::format("bad CHNKExta data length"));
            }
            let idx = out.exta.len();
            out.by_id.insert(id.clone(), idx);
            out.exta.push(ExtaChunk { id, data: start..start + data_len, offset: off });
        } else if tag == FOOT_TAG {
            break;
        } else {
            return Err(Error::format(format!(
                "unknown chunk {:?} at {off}",
                String::from_utf8_lossy(tag)
            )));
        }
        off = body + len;
    }
    if out.sqlite.is_empty() {
        return Err(Error::format("missing CHNKSQLi chunk"));
    }
    if out.head.sqlite_offset as usize != out.sqlite.start - 16 {
        // The head points at the chunk tag, not the payload.
        return Err(Error::format("CHNKHead.sqlite_offset does not match CHNKSQLi"));
    }
    Ok(out)
}

/// Build a container from a header identifier, a SQLite database and external chunks.
pub fn build(identifier: &[u8], sqlite: &[u8], exta: &[(String, Vec<u8>)]) -> Vec<u8> {
    let head_len = 40u64;
    let mut out = Vec::with_capacity(sqlite.len() + 4096);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&0u64.to_be_bytes()); // total, patched later
    out.extend_from_slice(&24u64.to_be_bytes());
    out.extend_from_slice(HEAD_TAG);
    out.extend_from_slice(&head_len.to_be_bytes());
    let head_start = out.len();
    out.extend_from_slice(&256u64.to_be_bytes()); // version
    out.extend_from_slice(&0u64.to_be_bytes()); // sqlite offset, patched later
    out.extend_from_slice(&(identifier.len() as u64).to_be_bytes());
    out.extend_from_slice(identifier);
    debug_assert_eq!(out.len() - head_start, head_len as usize);

    // Reserve room for the SQLite offset before writing chunks so it can be
    // filled with the absolute offset of the CHNKSQLi tag.
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    for (id, data) in exta {
        let tag_off = out.len();
        out.extend_from_slice(EXTA_TAG);
        out.extend_from_slice(&((8 + id.len() + 8 + data.len()) as u64).to_be_bytes());
        out.extend_from_slice(&(id.len() as u64).to_be_bytes());
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&(data.len() as u64).to_be_bytes());
        out.extend_from_slice(data);
        pending.push((id.clone(), (tag_off as u64).to_be_bytes().to_vec()));
    }
    let sqlite_tag_offset = out.len();
    out.extend_from_slice(SQLI_TAG);
    out.extend_from_slice(&(sqlite.len() as u64).to_be_bytes());
    out.extend_from_slice(sqlite);
    out.extend_from_slice(FOOT_TAG);
    out.extend_from_slice(&0u64.to_be_bytes());

    let total = out.len() as u64;
    out[8..16].copy_from_slice(&total.to_be_bytes());
    out[head_start + 8..head_start + 16]
        .copy_from_slice(&(sqlite_tag_offset as u64).to_be_bytes());
    out
}

/// Rewrite a container while keeping chunk order, replacing payloads by id and
/// returning the new external-chunk offsets keyed by id.
pub fn rebuild(
    identifier: &[u8],
    sqlite: &[u8],
    order: &[String],
    payloads: &HashMap<String, Vec<u8>>,
) -> (Vec<u8>, HashMap<String, u64>) {
    let head_len = 40u64;
    let mut out = Vec::with_capacity(sqlite.len() + 4096);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&24u64.to_be_bytes());
    out.extend_from_slice(HEAD_TAG);
    out.extend_from_slice(&head_len.to_be_bytes());
    let head_start = out.len();
    out.extend_from_slice(&256u64.to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&(identifier.len() as u64).to_be_bytes());
    out.extend_from_slice(identifier);

    let mut offsets = HashMap::new();
    for id in order {
        let Some(data) = payloads.get(id) else { continue };
        offsets.insert(id.clone(), out.len() as u64);
        out.extend_from_slice(EXTA_TAG);
        out.extend_from_slice(&((8 + id.len() + 8 + data.len()) as u64).to_be_bytes());
        out.extend_from_slice(&(id.len() as u64).to_be_bytes());
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&(data.len() as u64).to_be_bytes());
        out.extend_from_slice(data);
    }
    let sqlite_tag_offset = out.len() as u64;
    out.extend_from_slice(SQLI_TAG);
    out.extend_from_slice(&(sqlite.len() as u64).to_be_bytes());
    out.extend_from_slice(sqlite);
    out.extend_from_slice(FOOT_TAG);
    out.extend_from_slice(&0u64.to_be_bytes());
    let total = out.len() as u64;
    out[8..16].copy_from_slice(&total.to_be_bytes());
    out[head_start + 8..head_start + 16].copy_from_slice(&sqlite_tag_offset.to_be_bytes());
    (out, offsets)
}

/// UTF-16BE string with a u32 length prefix, without alignment padding.
pub fn utf16be_raw(name: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + name.len() * 2);
    v.extend_from_slice(&(name.len() as u32).to_be_bytes());
    for c in name.encode_utf16() {
        v.extend_from_slice(&c.to_be_bytes());
    }
    v
}

/// Alias kept for call sites that predate the unpadded tag finding.
pub fn utf16be_tag(name: &str) -> Vec<u8> {
    utf16be_raw(name)
}

pub fn read_utf16be_tag(data: &[u8], at: usize) -> Result<(String, usize)> {
    let n = data
        .get(at..at + 4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()) as usize)
        .ok_or_else(|| Error::format("truncated tag"))?;
    let end = at + 4 + n * 2;
    let raw = data
        .get(at + 4..end)
        .ok_or_else(|| Error::format("truncated tag body"))?;
    let mut units = Vec::with_capacity(n);
    for c in raw.chunks_exact(2) {
        units.push(u16::from_be_bytes([c[0], c[1]]));
    }
    let s = String::from_utf16(&units).map_err(|_| Error::format("bad UTF-16 tag"))?;
    Ok((s, end))
}
