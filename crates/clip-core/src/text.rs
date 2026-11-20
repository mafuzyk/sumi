//! Editable text layers.
//!
//! A text layer keeps its source text in `Layer.TextLayerString` (UTF-8) and
//! its style in `Layer.TextLayerAttributes`. Both are written by CSP's own
//! serializer, not by the chunk format: the attribute blob is a little-endian
//! TLV stream:
//!
//! ```text
//! repeated:
//!   u32 tag
//!   u32 size
//!   u8  value[size]
//! ```
//!
//! The tag numbers below are the ones measured on real documents
//! (`lolita-en_01`, `jose-en_01`, `jose-en_02`); unknown tags are preserved in
//! [`TextStyle::raw`] so callers can inspect them while more are decoded.
//!
//! `Layer.TextLayerStringArray` and `Layer.TextLayerAttributesArray` hold
//! extra independently styled runs (for example the "I'm hungry!" shout in
//! `lolita-en_01`). Both are arrays of `[u32 length][payload]` entries.

/// Tag holding the font family name as UTF-8.
pub const TAG_FONT_FAMILY: u32 = 31;
/// Tag holding the font size in 1/100 point (900 = 9.00 pt).
pub const TAG_FONT_SIZE: u32 = 32;
/// Tag holding the fill colour as three little-endian `i32` channels. CSP
/// stores `-1` for 255, so the samples show black (`0,0,0`) and white
/// (`-1,-1,-1`).
pub const TAG_FILL_COLOR: u32 = 34;
/// Pixels per point, scaled by 1000 (`8333` at 600 dpi, `4861` at 350 dpi).
pub const TAG_PX_PER_POINT: u32 = 44;
/// Line spacing parameter. The unit is not certain yet; exposed raw.
pub const TAG_LINE_SPACING: u32 = 50;
/// List of fonts used by the run (length-prefixed names).
pub const TAG_FONT_LIST: u32 = 57;

#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub font_family: String,
    /// Points, from [`TAG_FONT_SIZE`] / 100.
    pub font_size: f64,
    /// RGB, with CSP's `-1` mapped to 255.
    pub fill: [u8; 3],
    pub pixels_per_point: f64,
    pub line_spacing: i64,
    /// Every tag, including ones this module does not name yet.
    pub raw: Vec<(u32, Vec<u8>)>,
}

impl TextStyle {
    fn from_tags(tags: Vec<(u32, Vec<u8>)>) -> TextStyle {
        let font_family = tags
            .iter()
            .find(|(tag, _)| *tag == TAG_FONT_FAMILY)
            .map(|(_, value)| String::from_utf8_lossy(value).to_string())
            .unwrap_or_default();
        let font_size = tags
            .iter()
            .find(|(tag, _)| *tag == TAG_FONT_SIZE)
            .and_then(|(_, value)| read_i32(value))
            .map(|v| v as f64 / 100.0)
            .unwrap_or(0.0);
        let fill = tags
            .iter()
            .find(|(tag, _)| *tag == TAG_FILL_COLOR)
            .map(|(_, value)| read_color(value))
            .unwrap_or([0, 0, 0]);
        let pixels_per_point = tags
            .iter()
            .find(|(tag, _)| *tag == TAG_PX_PER_POINT)
            .and_then(|(_, value)| read_i32(value))
            .map(|v| v as f64 / 1000.0)
            .unwrap_or(72.0);
        let line_spacing = tags
            .iter()
            .find(|(tag, _)| *tag == TAG_LINE_SPACING)
            .and_then(|(_, value)| read_i32(value))
            .map(|v| v as i64)
            .unwrap_or(0);
        TextStyle { font_family, font_size, fill, pixels_per_point, line_spacing, raw: tags }
    }

    /// Raw value of one tag, for diagnostics.
    pub fn tag(&self, tag: u32) -> Option<&[u8]> {
        self.raw
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, value)| value.as_slice())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub style: Option<TextStyle>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLayer {
    /// Main run; `\r\n` separates the explicit line breaks.
    pub text: String,
    pub style: Option<TextStyle>,
    pub extra_runs: Vec<TextRun>,
}

fn read_i32(bytes: &[u8]) -> Option<i32> {
    bytes.get(..4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_color(bytes: &[u8]) -> [u8; 3] {
    let mut out = [0u8; 3];
    for (i, channel) in out.iter_mut().enumerate() {
        *channel = match read_i32(bytes.get(i * 4..).unwrap_or(&[])) {
            Some(-1) => 255,
            Some(v) => v.clamp(0, 255) as u8,
            None => 0,
        };
    }
    out
}

/// Parse one attribute blob into its tag/value pairs.
pub fn parse_attributes(blob: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= blob.len() {
        let tag = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]);
        let size = u32::from_le_bytes([blob[p + 4], blob[p + 5], blob[p + 6], blob[p + 7]]) as usize;
        p += 8;
        if size > blob.len() - p {
            break;
        }
        out.push((tag, blob[p..p + size].to_vec()));
        p += size;
    }
    out
}

/// Parse the length-prefixed UTF-8 string array used for extra text runs.
pub fn parse_string_array(blob: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 4 <= blob.len() {
        let len = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]) as usize;
        p += 4;
        if len > blob.len() - p {
            break;
        }
        out.push(String::from_utf8_lossy(&blob[p..p + len]).to_string());
        p += len;
    }
    out
}

/// Parse the length-prefixed attribute blobs parallel to
/// [`parse_string_array`].
pub fn parse_attributes_array(blob: &[u8]) -> Vec<Vec<(u32, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 4 <= blob.len() {
        let len = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]) as usize;
        p += 4;
        if len > blob.len() - p {
            break;
        }
        out.push(parse_attributes(&blob[p..p + len]));
        p += len;
    }
    out
}

/// Build a [`TextLayer`] from the three raw columns of `Layer`.
pub fn parse_layer(
    text: &str,
    attributes: Option<&[u8]>,
    strings_array: Option<&[u8]>,
    attributes_array: Option<&[u8]>,
) -> TextLayer {
    let style = attributes.map(|a| TextStyle::from_tags(parse_attributes(a)));
    let strings = strings_array.map(parse_string_array).unwrap_or_default();
    let styles = attributes_array.map(parse_attributes_array).unwrap_or_default();
    let extra_runs = strings
        .into_iter()
        .enumerate()
        .map(|(i, text)| TextRun {
            text,
            style: styles
                .get(i)
                .map(|tags| TextStyle::from_tags(tags.clone())),
        })
        .collect();
    TextLayer { text: text.to_string(), style, extra_runs }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii_tag(tag: u32, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
        out
    }

    #[test]
    fn tlv_stream_is_read_up_to_unknown_tags() {
        let mut blob = ascii_tag(TAG_FONT_FAMILY, b"CCJoeKubert");
        blob.extend_from_slice(&ascii_tag(9999, &[1, 2, 3, 4]));
        blob.extend_from_slice(&ascii_tag(TAG_FONT_SIZE, &900i32.to_le_bytes()));
        let tags = parse_attributes(&blob);
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[2].0, TAG_FONT_SIZE);
        let style = TextStyle::from_tags(tags);
        assert_eq!(style.font_family, "CCJoeKubert");
        assert_eq!(style.font_size, 9.0);
        assert_eq!(style.tag(9999), Some([1u8, 2, 3, 4].as_slice()));
    }

    #[test]
    fn fill_color_maps_minus_one_to_255() {
        let mut value = Vec::new();
        value.extend_from_slice(&(-1i32).to_le_bytes());
        value.extend_from_slice(&128i32.to_le_bytes());
        value.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(read_color(&value), [255, 128, 0]);
    }

    #[test]
    fn length_prefixed_arrays_roundtrip() {
        let mut strings = Vec::new();
        for text in ["I’m \r\nhungry!", "SMILE!"] {
            strings.extend_from_slice(&(text.len() as u32).to_le_bytes());
            strings.extend_from_slice(text.as_bytes());
        }
        assert_eq!(parse_string_array(&strings), ["I’m \r\nhungry!", "SMILE!"]);

        let mut attrs = Vec::new();
        let style = ascii_tag(TAG_FONT_FAMILY, b"Tahoma");
        attrs.extend_from_slice(&(style.len() as u32).to_le_bytes());
        attrs.extend_from_slice(&style);
        let parsed = parse_attributes_array(&attrs);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0][0].1, b"Tahoma");
    }
}
