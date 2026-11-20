//! Text layout and rasterisation for Sumi's text tool.
//!
//! CLIP STUDIO PAINT stores text layers with a cached glyph layout, so editing
//! one of its existing text layers would mean rebuilding a proprietary cache.
//! Sumi therefore renders authored text itself: this module shapes a string into
//! placed glyphs (horizontal, vertical, ruby and on-path), and rasterises them
//! with the small pure-Rust font stack that `resvg` already pulls in.
//!
//! Boundaries worth knowing:
//!
//! * Layout is simple advance-width layout (LTR), not a full shaping engine:
//!   ligatures, kerning pairs and complex-script reordering are not applied.
//! * Fonts come from the system through fontconfig; a missing family falls back
//!   to any available face so text never disappears silently.

use crate::render::Image;
use skrifa::raw::TableProvider;
use skrifa::MetadataProvider;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Shader, Transform};

/// Horizontal alignment inside the text box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A styled string plus the options the text tool exposes.
#[derive(Clone, Debug)]
pub struct TextSpec {
    pub string: String,
    pub family: String,
    /// Nominal size in pixels (the document is already at its export scale).
    pub size: f32,
    /// Extra space between glyphs in pixels; may be negative.
    pub tracking: f32,
    /// Baseline distance as a multiple of `size`.
    pub line_height: f32,
    pub color: [u8; 4],
    pub align: TextAlign,
    /// Vertical writing: glyphs stack top to bottom, columns run right to left.
    pub vertical: bool,
    /// Ruby annotations as `(start_byte, end_byte, text)`.
    pub ruby: Vec<(usize, usize, String)>,
    /// Optional polyline the glyphs follow instead of a straight baseline.
    pub on_path: Option<Vec<(f32, f32)>>,
}

impl Default for TextSpec {
    fn default() -> Self {
        TextSpec {
            string: String::new(),
            family: "sans-serif".to_string(),
            size: 32.0,
            tracking: 0.0,
            line_height: 1.25,
            color: [0, 0, 0, 255],
            align: TextAlign::Left,
            vertical: false,
            ruby: Vec::new(),
            on_path: None,
        }
    }
}

impl TextSpec {
    pub fn new(string: impl Into<String>) -> Self {
        TextSpec { string: string.into(), ..TextSpec::default() }
    }
}

/// One glyph after layout. `angle` is in radians, clockwise, for on-path text.
#[derive(Clone, Copy, Debug)]
pub struct PlacedGlyph {
    pub ch: char,
    pub glyph_id: u16,
    /// Pen origin in text-box coordinates.
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub angle: f32,
    pub color: [u8; 4],
    pub ruby: bool,
}

impl PlacedGlyph {
    /// Axis-aligned bounding box of the glyph's pen cell, used by the editor to
    /// size a text layer without re-laying the string out.
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let half = self.size * 0.5;
        (self.x - half, self.y - self.size, self.x + half, self.y + self.size * 0.25)
    }
}

/// System font database, loaded once per process.
pub struct Fonts {
    db: fontdb::Database,
    fallback_cache: std::sync::Mutex<std::collections::HashMap<char, Option<fontdb::ID>>>,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Fonts { db, fallback_cache: Default::default() }
    }

    pub fn families(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .db
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn face(&self, family: &str) -> Option<fontdb::ID> {
        let families = [fontdb::Family::Name(family)];
        let query = fontdb::Query {
            families: &families,
            weight: fontdb::Weight::NORMAL,
            style: fontdb::Style::Normal,
            stretch: fontdb::Stretch::Normal,
        };
        if let Some(id) = self.db.query(&query) {
            return Some(id);
        }
        let families = [fontdb::Family::SansSerif];
        let query = fontdb::Query { families: &families, ..query };
        self.db
            .query(&query)
            .or_else(|| self.db.faces().next().map(|face| face.id))
    }
}

/// Faces that can draw a character the preferred family lacks.
///
/// Coverage is read from each face's cmap; the result is cached per `Fonts`
/// instance so a line with several missing characters does not rescan the
/// system font database for each one.
impl Fonts {
    pub fn fallback_face(&self, ch: char) -> Option<fontdb::ID> {
        if let Some(hit) = self.fallback_cache.lock().ok().and_then(|cache| cache.get(&ch).copied()) {
            return hit;
        }
        let mut found = None;
        for face in self.db.faces() {
            let mut covers = false;
            self.db.with_face_data(face.id, |data, index| {
                covers = skrifa::FontRef::from_index(data, index)
                    .ok()
                    .map(|font| font.charmap().map(ch).is_some())
                    .unwrap_or(false);
            });
            if covers {
                found = Some(face.id);
                break;
            }
        }
        if let Ok(mut cache) = self.fallback_cache.lock() {
            cache.insert(ch, found);
        }
        found
    }

    /// True when the face behind `data` has a glyph for `ch`.
    fn face_covers(data: &[u8], index: u32, ch: char) -> bool {
        skrifa::FontRef::from_index(data, index)
            .ok()
            .map(|font| font.charmap().map(ch).is_some())
            .unwrap_or(false)
    }
}

fn premultiplied_to_straight(pixel: PremultipliedColorU8) -> [u8; 4] {
    let alpha = pixel.alpha();
    if alpha == 0 {
        return [0, 0, 0, 0];
    }
    let straight = |channel: u8| {
        ((channel as u32 * 255 + alpha as u32 / 2) / alpha as u32).min(255) as u8
    };
    [straight(pixel.red()), straight(pixel.green()), straight(pixel.blue()), alpha]
}

/// Lay a spec out without rasterising it. Returns an empty vector when the
/// system has no fonts or the string is empty.
pub fn layout(spec: &TextSpec, fonts: &Fonts) -> Vec<PlacedGlyph> {
    let Some(id) = fonts.face(&spec.family) else {
        return Vec::new();
    };
    let mut primary = Vec::new();
    let mut index = 0u32;
    fonts.db.with_face_data(id, |data, face_index| {
        primary = data.to_vec();
        index = face_index;
    });
    if primary.is_empty() {
        return Vec::new();
    }
    // Shaping and fallback only cover plain horizontal text; vertical writing
    // and text on a path keep using one face.
    let mixed = !spec.vertical
        && spec.on_path.is_none()
        && spec
            .string
            .chars()
            .any(|ch| ch != '\n' && !Fonts::face_covers(&primary, index, ch));
    if mixed {
        layout_mixed(spec, fonts, &primary, index)
    } else {
        layout_with_font(&primary, index, spec)
    }
}

/// One run of a line: consecutive characters drawn with the same face.
struct Run {
    data: Vec<u8>,
    face_index: u32,
    upem: f32,
    text: String,
    start_byte: usize,
}

/// Horizontal text where characters missing from the preferred family are
/// taken from another installed face, shaped run by run.
fn layout_mixed(spec: &TextSpec, fonts: &Fonts, primary: &[u8], primary_index: u32) -> Vec<PlacedGlyph> {
    let Ok(primary_font) = skrifa::FontRef::from_index(primary, primary_index) else {
        return Vec::new();
    };
    let charmap = primary_font.charmap();
    let metrics = primary_font.glyph_metrics(
        skrifa::instance::Size::unscaled(),
        skrifa::instance::LocationRef::default(),
    );
    let primary_upem = primary_font
        .head()
        .ok()
        .map(|head| head.units_per_em() as f32)
        .unwrap_or(1000.0);
    let scale = spec.size / primary_upem.max(1.0);
    let glyph_id = |ch: char| charmap.map(ch).unwrap_or_default();
    let advance = |ch: char| {
        metrics
            .advance_width(glyph_id(ch))
            .map(|width| width * scale)
            .unwrap_or(spec.size * 0.5)
    };
    let line_height = spec.size * spec.line_height.max(0.5);
    let mut faces: std::collections::HashMap<fontdb::ID, (Vec<u8>, u32, f32)> =
        std::collections::HashMap::new();
    let mut glyphs = Vec::new();
    let mut line_byte_start = 0usize;
    for (line_index, line) in spec.string.split('\n').enumerate() {
        let mut runs: Vec<Run> = Vec::new();
        let mut byte = 0usize;
        for ch in line.chars() {
            let (data, face_index, upem) = if Fonts::face_covers(primary, primary_index, ch) {
                (primary.to_vec(), primary_index, primary_upem)
            } else if let Some(fallback) = fonts.fallback_face(ch) {
                let entry = faces.entry(fallback).or_insert_with(|| {
                    let mut data = Vec::new();
                    let mut index = 0u32;
                    fonts.db.with_face_data(fallback, |bytes, face_index| {
                        data = bytes.to_vec();
                        index = face_index;
                    });
                    let upem = skrifa::FontRef::from_index(&data, index)
                        .ok()
                        .and_then(|font| font.head().ok().map(|head| head.units_per_em() as f32))
                        .unwrap_or(1000.0);
                    (data, index, upem)
                });
                (entry.0.clone(), entry.1, entry.2)
            } else {
                (primary.to_vec(), primary_index, primary_upem)
            };
            let same_face = runs
                .last()
                .map(|run| run.face_index == face_index && run.data.as_slice() == data.as_slice())
                .unwrap_or(false);
            if same_face {
                runs.last_mut().expect("run exists").text.push(ch);
            } else {
                runs.push(Run { data, face_index, upem, text: ch.to_string(), start_byte: byte });
            }
            byte += ch.len_utf8();
        }

        let mut shaped_runs = Vec::with_capacity(runs.len());
        for run in &runs {
            let shaped = rustybuzz::Face::from_slice(&run.data, run.face_index)
                .map(|face| shape_line(&face, &run.text, run.upem, spec.size, spec.tracking))
                .unwrap_or_default();
            shaped_runs.push((run, shaped));
        }
        let width: f32 = shaped_runs
            .iter()
            .flat_map(|(_, shaped)| shaped.iter().map(|glyph| glyph.advance))
            .sum();
        let mut pen_x = match spec.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -width * 0.5,
            TextAlign::Right => -width,
        };
        let pen_y = spec.size + line_index as f32 * line_height;
        let line_start = glyphs.len();
        let mut widths = vec![0.0f32; line.chars().count()];
        let mut byte_to_char = std::collections::HashMap::new();
        let mut seen = 0usize;
        for (index, ch) in line.chars().enumerate() {
            byte_to_char.insert(seen, index);
            seen += ch.len_utf8();
        }
        for (run, shaped) in &shaped_runs {
            for glyph in shaped {
                let ch = run
                    .text
                    .get(glyph.cluster..)
                    .and_then(|rest| rest.chars().next())
                    .unwrap_or('\u{fffd}');
                glyphs.push(PlacedGlyph {
                    ch,
                    glyph_id: glyph.glyph_id,
                    x: pen_x + glyph.x_offset,
                    y: pen_y - glyph.y_offset,
                    size: spec.size,
                    angle: 0.0,
                    color: spec.color,
                    ruby: false,
                });
                if let Some(&char_index) = byte_to_char.get(&(run.start_byte + glyph.cluster)) {
                    widths[char_index] += glyph.advance;
                }
                pen_x += glyph.advance;
            }
        }
        place_ruby(
            spec,
            line,
            line_byte_start,
            line_start,
            &widths,
            &glyph_id,
            &advance,
            &mut glyphs,
            line_height,
        );
        line_byte_start += line.len() + 1;
    }
    glyphs
}

/// A shaped glyph: glyph id plus a pen advance and offsets, in pixels at the
/// requested size.
#[derive(Clone, Copy, Debug)]
struct ShapedGlyph {
    glyph_id: u16,
    cluster: usize,
    advance: f32,
    x_offset: f32,
    y_offset: f32,
}

/// Shape one line with HarfBuzz (through `rustybuzz`) at `size` pixels.
///
/// The result is in visual order, which is what the renderer wants: right-to-left
/// runs come back reversed and their advances still count to the right.
fn shape_line(
    face: &rustybuzz::Face,
    line: &str,
    upem: f32,
    size: f32,
    tracking: f32,
) -> Vec<ShapedGlyph> {
    if line.is_empty() {
        return Vec::new();
    }
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(line);
    buffer.guess_segment_properties();
    let shaped = rustybuzz::shape(face, &[], buffer);
    let scale = size / upem.max(1.0);
    shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(info, pos)| ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            cluster: info.cluster as usize,
            advance: pos.x_advance as f32 * scale + tracking,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: pos.y_offset as f32 * scale,
        })
        .collect()
}

/// Horizontal text with a single face, positioned from the shaped run.
fn layout_shaped(
    spec: &TextSpec,
    face: &rustybuzz::Face,
    font: &skrifa::FontRef,
    upem: f32,
) -> Vec<PlacedGlyph> {
    let charmap = font.charmap();
    let metrics = font.glyph_metrics(
        skrifa::instance::Size::unscaled(),
        skrifa::instance::LocationRef::default(),
    );
    let scale = spec.size / upem.max(1.0);
    let glyph_id = |ch: char| charmap.map(ch).unwrap_or_default();
    let advance = |ch: char| {
        metrics
            .advance_width(glyph_id(ch))
            .map(|width| width * scale)
            .unwrap_or(spec.size * 0.5)
    };
    let line_height = spec.size * spec.line_height.max(0.5);
    let mut glyphs = Vec::new();
    let mut line_byte_start = 0usize;
    for (line_index, line) in spec.string.split('\n').enumerate() {
        let shaped = shape_line(face, line, upem, spec.size, spec.tracking);
        let width: f32 = shaped.iter().map(|glyph| glyph.advance).sum();
        let mut pen_x = match spec.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -width * 0.5,
            TextAlign::Right => -width,
        };
        let pen_y = spec.size + line_index as f32 * line_height;
        let line_start = glyphs.len();
        for glyph in &shaped {
            let ch = line
                .get(glyph.cluster..)
                .and_then(|rest| rest.chars().next())
                .unwrap_or('\u{fffd}');
            glyphs.push(PlacedGlyph {
                ch,
                glyph_id: glyph.glyph_id,
                x: pen_x + glyph.x_offset,
                y: pen_y - glyph.y_offset,
                size: spec.size,
                angle: 0.0,
                color: spec.color,
                ruby: false,
            });
            pen_x += glyph.advance;
        }
        let widths = line_char_widths(line, &shaped);
        place_ruby(
            spec,
            line,
            line_byte_start,
            line_start,
            &widths,
            &glyph_id,
            &advance,
            &mut glyphs,
            line_height,
        );
        line_byte_start += line.len() + 1;
    }
    glyphs
}

/// Distribute shaped advances over the characters of the source line so ruby
/// centring still works: a character that starts a cluster owns the cluster's
/// advance, the characters inside a ligature advance by zero.
fn line_char_widths(line: &str, shaped: &[ShapedGlyph]) -> Vec<f32> {
    let mut widths = vec![0.0f32; line.chars().count()];
    let mut byte_to_char = std::collections::HashMap::new();
    let mut byte = 0usize;
    for (index, ch) in line.chars().enumerate() {
        byte_to_char.insert(byte, index);
        byte += ch.len_utf8();
    }
    for glyph in shaped {
        if let Some(&index) = byte_to_char.get(&glyph.cluster) {
            widths[index] += glyph.advance;
        }
    }
    widths
}

/// Layout against explicit font bytes so tests and the CLI can pin a font.
///
/// Horizontal text is shaped with HarfBuzz (`rustybuzz`): ligatures, kerning,
/// context-sensitive forms and right-to-left reordering come from the font's
/// OpenType tables instead of summing per-character advances. Vertical text and
/// fonts the shaper rejects keep the older per-character path.
pub fn layout_with_font(data: &[u8], face_index: u32, spec: &TextSpec) -> Vec<PlacedGlyph> {
    let Ok(font) = skrifa::FontRef::from_index(data, face_index) else {
        return Vec::new();
    };
    let upem = font
        .head()
        .ok()
        .map(|head| head.units_per_em() as f32)
        .unwrap_or(1000.0);
    let Some(face) = rustybuzz::Face::from_slice(data, face_index) else {
        return layout_unshaped(data, face_index, spec);
    };
    if spec.vertical {
        return layout_unshaped(data, face_index, spec);
    }
    if let Some(path) = spec.on_path.as_deref() {
        return layout_on_path(spec, path, &face, upem);
    }
    layout_shaped(spec, &face, &font, upem)
}

/// Layout against explicit font bytes so tests and the CLI can pin a font.
fn layout_unshaped(data: &[u8], face_index: u32, spec: &TextSpec) -> Vec<PlacedGlyph> {
    let Ok(font) = skrifa::FontRef::from_index(data, face_index) else {
        return Vec::new();
    };
    let charmap = font.charmap();
    let metrics = font.glyph_metrics(
        skrifa::instance::Size::unscaled(),
        skrifa::instance::LocationRef::default(),
    );
    let upem = font.head().map(|head| head.units_per_em() as f32).unwrap_or(1000.0);
    let scale = spec.size / upem.max(1.0);
    let glyph_id = |ch: char| charmap.map(ch).unwrap_or_default();
    let advance = |ch: char| {
        metrics
            .advance_width(glyph_id(ch))
            .map(|width| width * scale)
            .unwrap_or(spec.size * 0.5)
            + spec.tracking
    };
    // Vertical writing advances by the font's own cell (`vmtx`), one em for
    // CJK faces; `line_height` only applies to horizontal lines and to the gap
    // between columns, which a cell covers by itself.
    let vertical_face = ttf_parser::Face::parse(data, face_index).ok();
    let vertical_advance = |ch: char| -> f32 {
        if let Some(face) = &vertical_face {
            let gid = glyph_id(ch).to_u32() as u16;
            if let Some(advance) = face.glyph_ver_advance(ttf_parser::GlyphId(gid)) {
                return advance as f32 * scale;
            }
        }
        spec.size
    };

    let line_height = spec.size * spec.line_height.max(0.5);
    let mut glyphs = Vec::new();
    let mut line_byte_start = 0usize;
    for (line_index, line) in spec.string.split('\n').enumerate() {
        let widths: Vec<f32> = line.chars().map(advance).collect();
        let line_width = widths.iter().sum::<f32>();
        let mut pen_x = match spec.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -line_width * 0.5,
            TextAlign::Right => -line_width,
        };
        let pen_y = spec.size + line_index as f32 * line_height;
        let column_x = -(line_index as f32) * spec.size;
        let mut vertical_y = spec.size;
        let mut line_start = glyphs.len();
        for (offset, ch) in line.chars().enumerate() {
            let step = widths[offset];
            let mut angle = 0.0;
            let (x, y) = if spec.vertical {
                let y = vertical_y;
                vertical_y += vertical_advance(ch);
                angle = vertical_rotation(ch);
                (column_x, y)
            } else {
                (pen_x, pen_y)
            };
            glyphs.push(PlacedGlyph {
                ch,
                glyph_id: glyph_id(ch).to_u32() as u16,
                x,
                y,
                size: spec.size,
                angle,
                color: spec.color,
                ruby: false,
            });
            if !spec.vertical {
                pen_x += step;
            }
        }
        if !spec.vertical {
            line_start = line_start.min(glyphs.len());
            place_ruby(
                spec,
                line,
                line_byte_start,
                line_start,
                &widths,
                &glyph_id,
                &advance,
                &mut glyphs,
                line_height,
            );
        }
        line_byte_start += line.len() + 1;
    }
    glyphs
}

/// Rotation a character takes in vertical writing, in radians.
///
/// This is a subset of UAX #50's "rotated" orientation: brackets, dashes and
/// ellipses read sideways in vertical Japanese, while ideographs, kana and CJK
/// punctuation such as `。` or `、` stay upright. Upright transliteration of the
/// full table would need the Unicode property itself; the subset is explicit so
/// a missing character shows up as upright instead of being guessed.
pub fn vertical_rotation(ch: char) -> f32 {
    const ROTATED: &[char] = &[
        '(', ')', '[', ']', '{', '}', '<', '>',
        '\u{ff08}', '\u{ff09}', '\u{ff3b}', '\u{ff3d}', '\u{ff5b}', '\u{ff5d}',
        '\u{3008}', '\u{3009}', '\u{300a}', '\u{300b}', '\u{300c}', '\u{300d}',
        '\u{300e}', '\u{300f}', '\u{3010}', '\u{3011}', '\u{3014}', '\u{3015}',
        '\u{3016}', '\u{3017}', '\u{3018}', '\u{3019}', '\u{301a}', '\u{301b}',
        '-', '\u{2010}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
        '\u{2026}', '\u{2025}',
    ];
    if ROTATED.contains(&ch) {
        std::f32::consts::FRAC_PI_2
    } else {
        0.0
    }
}

/// Centre ruby runs over the base glyphs they annotate.
#[allow(clippy::too_many_arguments)]
fn place_ruby(
    spec: &TextSpec,
    line: &str,
    line_offset: usize,
    line_start: usize,
    widths: &[f32],
    glyph_id: &impl Fn(char) -> skrifa::GlyphId,
    advance: &impl Fn(char) -> f32,
    glyphs: &mut Vec<PlacedGlyph>,
    line_height: f32,
) {
    if spec.ruby.is_empty() {
        return;
    }
    for (start, end, text) in &spec.ruby {
        // The annotation carries byte offsets into the whole string; the line
        // knows where it really starts, so two identical lines stay distinct.
        let local_start = start.saturating_sub(line_offset);
        let local_end = end.saturating_sub(line_offset).min(line.len());
        if local_start >= local_end || local_start >= line.len() {
            continue;
        }
        // A range that splits a multibyte character cannot be laid out.
        if !line.is_char_boundary(local_start) || !line.is_char_boundary(local_end) {
            continue;
        }
        let first = line[..local_start].chars().count();
        let last = line[..local_end].chars().count();
        if first >= glyphs.len().saturating_sub(line_start) || last == 0 {
            continue;
        }
        let left = glyphs[line_start + first].x;
        let right = glyphs[line_start + last - 1].x + widths[last - 1];
        let ruby_size = spec.size * 0.5;
        let ruby_scale = ruby_size / spec.size.max(0.001);
        let ruby_width: f32 =
            text.chars().map(|ch| advance(ch) * ruby_scale).sum::<f32>();
        let mut pen_x = (left + right) * 0.5 - ruby_width * 0.5;
        let base_y = glyphs[line_start + first].y;
        for ch in text.chars() {
            glyphs.push(PlacedGlyph {
                ch,
                glyph_id: glyph_id(ch).to_u32() as u16,
                x: pen_x,
                y: base_y - line_height * 0.7,
                size: ruby_size,
                angle: 0.0,
                color: spec.color,
                ruby: true,
            });
            pen_x += advance(ch) * ruby_scale;
        }
    }
}

fn layout_on_path(
    spec: &TextSpec,
    path: &[(f32, f32)],
    face: &rustybuzz::Face,
    upem: f32,
) -> Vec<PlacedGlyph> {
    if path.len() < 2 {
        return Vec::new();
    }
    let mut segments = Vec::new();
    let mut total = 0.0;
    for pair in path.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        let length = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        if length <= f32::EPSILON {
            continue;
        }
        segments.push((x0, y0, x1, y1, length, total));
        total += length;
    }
    let shaped = shape_line(face, &spec.string.replace('\n', " "), upem, spec.size, spec.tracking);
    let mut placed = Vec::new();
    let mut distance = 0.0;
    let text = spec.string.replace('\n', " ");
    for glyph in &shaped {
        if distance > total {
            break;
        }
        let centre = distance + glyph.advance * 0.5;
        let Some((x0, y0, x1, y1, length, start)) =
            segments.iter().find(|segment| centre <= segment.5 + segment.4).copied()
        else {
            break;
        };
        let t = ((centre - start) / length).clamp(0.0, 1.0);
        let ch = text
            .get(glyph.cluster..)
            .and_then(|rest| rest.chars().next())
            .unwrap_or('\u{fffd}');
        placed.push(PlacedGlyph {
            ch,
            glyph_id: glyph.glyph_id,
            x: x0 + (x1 - x0) * t,
            y: y0 + (y1 - y0) * t,
            size: spec.size,
            angle: (y1 - y0).atan2(x1 - x0),
            color: spec.color,
            ruby: false,
        });
        distance += glyph.advance;
    }
    placed
}

/// Rasterise a laid-out spec into a fresh image. The image is exactly the size
/// of the glyph bounds plus a small padding, so a text layer never carries a
/// huge transparent margin.
pub fn render(spec: &TextSpec, fonts: &Fonts) -> Option<(Image, (i32, i32))> {
    let glyphs = layout(spec, fonts);
    if glyphs.is_empty() {
        return None;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for glyph in &glyphs {
        let (x0, y0, x1, y1) = glyph.bounds();
        min_x = min_x.min(x0);
        min_y = min_y.min(y0);
        max_x = max_x.max(x1);
        max_y = max_y.max(y1);
    }
    let pad = (spec.size * 0.2).ceil();
    let origin_x = (min_x - pad).floor();
    let origin_y = (min_y - pad).floor();
    let width = (max_x - origin_x + pad).ceil().max(1.0) as u32;
    let height = (max_y - origin_y + pad).ceil().max(1.0) as u32;
    let id = fonts.face(&spec.family)?;
    let mut image = Image::new(width, height);
    fonts.db.with_face_data(id, |data, face_index| {
        if let Some(rendered) =
            render_glyphs(data, face_index, &glyphs, width, height, origin_x, origin_y)
        {
            image = rendered;
        }
    });
    Some((image, (origin_x as i32, origin_y as i32)))
}

/// Rasterise already-placed glyphs, offsetting the text box by `origin`.
pub fn render_glyphs(
    font_data: &[u8],
    face_index: u32,
    glyphs: &[PlacedGlyph],
    width: u32,
    height: u32,
    origin_x: f32,
    origin_y: f32,
) -> Option<Image> {
    let font = skrifa::FontRef::from_index(font_data, face_index).ok()?;
    let outlines = font.outline_glyphs();
    let upem = font.head().map(|head| head.units_per_em() as f32).unwrap_or(1000.0);
    let mut pixmap = Pixmap::new(width, height)?;
    for glyph in glyphs {
        let glyph_id = skrifa::GlyphId::new(glyph.glyph_id as u32);
        let Some(outline) = outlines.get(glyph_id) else {
            continue;
        };
        let mut builder = PathBuilder::new();
        let mut pen = TinySkiaPen { builder: &mut builder };
        let settings = skrifa::outline::DrawSettings::unhinted(
            skrifa::instance::Size::new(glyph.size),
            skrifa::instance::LocationRef::default(),
        );
        if outline.draw(settings, &mut pen).is_err() {
            continue;
        }
        let Some(path) = builder.finish() else {
            continue;
        };
        let mut paint = Paint {
            shader: Shader::SolidColor(tiny_skia::Color::from_rgba8(
                glyph.color[0],
                glyph.color[1],
                glyph.color[2],
                glyph.color[3],
            )),
            anti_alias: true,
            ..Paint::default()
        };
        paint.anti_alias = true;
        let transform = Transform::from_translate(glyph.x - origin_x, glyph.y - origin_y)
            .pre_rotate_at(glyph.angle, 0.0, 0.0);
        pixmap.fill_path(&path, &paint, FillRule::Winding, transform, None);
    }
    let mut out = Image::new(width, height);
    for (index, pixel) in pixmap.pixels().iter().enumerate() {
        out.data[index * 4..index * 4 + 4].copy_from_slice(&premultiplied_to_straight(*pixel));
    }
    let _ = upem;
    Some(out)
}

/// Adapter that turns skrifa's outline callbacks into a `tiny-skia` path.
struct TinySkiaPen<'a> {
    builder: &'a mut PathBuilder,
}

impl skrifa::outline::OutlinePen for TinySkiaPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, -y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, -y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.builder.quad_to(cx, -cy, x, -y);
    }

    fn curve_to(&mut self, cx1: f32, cy1: f32, cx2: f32, cy2: f32, x: f32, y: f32) {
        self.builder.cubic_to(cx1, -cy1, cx2, -cy2, x, -y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_font() -> Vec<u8> {
        let fonts = Fonts::new();
        let mut data = Vec::new();
        fonts.db.with_face_data(
            fonts.face("sans-serif").expect("a system sans-serif face"),
            |bytes, _| data = bytes.to_vec(),
        );
        assert!(!data.is_empty(), "the system must expose at least one font");
        data
    }

    fn family_font(family: &str) -> (Vec<u8>, u32) {
        let fonts = Fonts::new();
        let id = fonts
            .face(family)
            .unwrap_or_else(|| panic!("no installed face for {family}"));
        let mut data = Vec::new();
        let mut index = 0u32;
        fonts.db.with_face_data(id, |bytes, face_index| {
            data = bytes.to_vec();
            index = face_index;
        });
        assert!(!data.is_empty(), "{family} must expose font bytes");
        (data, index)
    }

    fn nominal_glyph(data: &[u8], index: u32, ch: char) -> u16 {
        let font = skrifa::FontRef::from_index(data, index).expect("font");
        font.charmap().map(ch).unwrap_or_default().to_u32() as u16
    }

    #[test]
    fn a_character_missing_from_the_family_is_taken_from_a_fallback_face() {
        let fonts = Fonts::new();
        let fallback = fonts
            .fallback_face('\u{6f22}')
            .expect("an installed face must cover the CJK character");
        let mut data = Vec::new();
        let mut index = 0u32;
        fonts.db.with_face_data(fallback, |bytes, face_index| {
            data = bytes.to_vec();
            index = face_index;
        });
        let expected = nominal_glyph(&data, index, '\u{6f22}');
        assert!(expected != 0, "the fallback face has a real glyph");

        let spec = TextSpec { family: "DejaVu Sans".to_string(), size: 40.0, ..TextSpec::new("A\u{6f22}") };
        let glyphs = layout(&spec, &fonts);
        assert_eq!(glyphs.len(), 2, "one glyph per character");
        assert_eq!(
            glyphs[1].glyph_id, expected,
            "the CJK character keeps the fallback face's glyph, not .notdef"
        );
        assert!(glyphs[1].x > glyphs[0].x, "the fallback run advances after the latin one");
    }

    #[test]
    fn kerning_pulls_the_second_glyph_left_of_its_plain_advance() {
        let (data, index) = family_font("DejaVu Sans");
        let size = 40.0;
        let aa = layout_with_font(&data, index, &TextSpec { size, ..TextSpec::new("AA") });
        let pair = layout_with_font(&data, index, &TextSpec { size, ..TextSpec::new("AV") });
        assert_eq!(pair.len(), 2, "two glyphs for AV");
        let plain_advance = aa[1].x - aa[0].x;
        let kerned = pair[1].x - pair[0].x;
        assert!(
            kerned < plain_advance - 0.5,
            "kerning must pull V left: {kerned} vs the plain advance {plain_advance}"
        );
    }

    #[test]
    fn hebrew_is_positioned_in_visual_order() {
        let (data, index) = family_font("DejaVu Sans");
        let alef = nominal_glyph(&data, index, '\u{05D0}');
        let bet = nominal_glyph(&data, index, '\u{05D1}');
        assert!(alef != 0 && bet != 0, "DejaVu Sans must cover Hebrew");
        let glyphs =
            layout_with_font(&data, index, &TextSpec { size: 40.0, ..TextSpec::new("\u{05D0}\u{05D1}") });
        assert_eq!(glyphs.len(), 2);
        assert_eq!(
            (glyphs[0].glyph_id, glyphs[1].glyph_id),
            (bet, alef),
            "the shaped run is laid out right to left"
        );
        assert!(glyphs[0].x < glyphs[1].x, "visual order advances to the right");
    }

    #[test]
    fn a_combining_mark_is_composed_or_attached_to_its_base() {
        let (data, index) = family_font("DejaVu Sans");
        let glyphs = layout_with_font(
            &data,
            index,
            &TextSpec { size: 40.0, ..TextSpec::new("e\u{0301}") },
        );
        match glyphs.as_slice() {
            [single] => assert_eq!(
                single.glyph_id,
                nominal_glyph(&data, index, '\u{00e9}'),
                "the shaper composed the mark into the precomposed glyph"
            ),
            [base, mark] => {
                assert!(base.glyph_id != mark.glyph_id, "base and mark are distinct glyphs");
                assert!(
                    (mark.x - base.x).abs() < 1.0,
                    "an attached mark must not advance past its base: {} vs {}",
                    mark.x,
                    base.x
                );
            }
            other => panic!("unexpected glyph count for e + acute: {}", other.len()),
        }
    }

    #[test]
    fn advances_grow_with_the_string_and_the_size() {
        let data = any_font();
        let small = layout_with_font(&data, 0, &TextSpec { size: 20.0, ..TextSpec::new("Hello") });
        let large = layout_with_font(&data, 0, &TextSpec { size: 40.0, ..TextSpec::new("Hello") });
        assert_eq!(small.len(), 5);
        assert_eq!(large.len(), 5);
        assert!(small[4].x > small[0].x, "glyphs advance to the right");
        assert!(
            large[4].x > small[4].x * 1.5,
            "a larger size must advance further: {} vs {}",
            large[4].x,
            small[4].x
        );
    }

    #[test]
    fn lines_stack_by_the_line_height() {
        let data = any_font();
        let spec = TextSpec { size: 20.0, line_height: 1.5, ..TextSpec::new("A\nB") };
        let glyphs = layout_with_font(&data, 0, &spec);
        assert_eq!(glyphs.len(), 2);
        assert!((glyphs[1].y - glyphs[0].y - 30.0).abs() < 0.01);
    }

    #[test]
    fn alignment_moves_the_line_left_of_the_origin() {
        let data = any_font();
        let left = layout_with_font(&data, 0, &TextSpec { size: 20.0, ..TextSpec::new("M") });
        let right = layout_with_font(
            &data,
            0,
            &TextSpec { size: 20.0, align: TextAlign::Right, ..TextSpec::new("M") },
        );
        assert!(left[0].x >= -0.01);
        assert!(right[0].x < -0.01, "right alignment pulls the pen left");
    }

    #[test]
    fn vertical_brackets_rotate_and_ideographs_do_not() {
        assert_eq!(vertical_rotation('\u{300c}'), std::f32::consts::FRAC_PI_2);
        assert_eq!(vertical_rotation('\u{ff08}'), std::f32::consts::FRAC_PI_2);
        assert_eq!(vertical_rotation('\u{2026}'), std::f32::consts::FRAC_PI_2);
        assert_eq!(vertical_rotation('\u{6f22}'), 0.0);
        assert_eq!(vertical_rotation('\u{3002}'), 0.0, "ideographic full stop stays upright");
        assert_eq!(vertical_rotation('\u{3042}'), 0.0, "kana stays upright");

        let (data, index) = family_font("DejaVu Sans");
        let glyphs = layout_with_font(
            &data,
            index,
            &TextSpec { size: 40.0, vertical: true, ..TextSpec::new("\u{300c}\u{6f22}") },
        );
        assert_eq!(glyphs[0].angle, std::f32::consts::FRAC_PI_2);
        assert_eq!(glyphs[1].angle, 0.0);
    }

    #[test]
    fn vertical_cells_use_the_font_vertical_advance() {
        let (data, index) = family_font("Noto Sans CJK HK");
        let spec = TextSpec { size: 40.0, vertical: true, ..TextSpec::new("\u{6f22}\u{5b57}") };
        let glyphs = layout_with_font(&data, index, &spec);
        assert_eq!(glyphs.len(), 2);
        let step = glyphs[1].y - glyphs[0].y;
        assert!(
            (step - 40.0).abs() < 2.0,
            "a CJK cell should advance by the font's one-em vertical metric, got {step}"
        );
    }

    #[test]
    fn vertical_columns_are_one_cell_apart() {
        let (data, index) = family_font("Noto Sans CJK HK");
        let spec = TextSpec { size: 40.0, vertical: true, ..TextSpec::new("\u{6f22}\n\u{5b57}") };
        let glyphs = layout_with_font(&data, index, &spec);
        assert_eq!(glyphs.len(), 2);
        let dx = glyphs[0].x - glyphs[1].x;
        assert!(
            (dx - 40.0).abs() < 2.0,
            "columns run right to left one cell apart, got {dx}"
        );
    }

    #[test]
    fn vertical_text_stacks_glyphs_downwards() {
        let data = any_font();
        let spec = TextSpec { size: 20.0, vertical: true, ..TextSpec::new("AB") };
        let glyphs = layout_with_font(&data, 0, &spec);
        assert_eq!(glyphs.len(), 2);
        assert!((glyphs[0].x - glyphs[1].x).abs() < 0.01, "one column");
        assert!(glyphs[1].y > glyphs[0].y, "glyphs stack downwards");
    }

    #[test]
    fn ruby_glyphs_are_smaller_and_above_the_baseline() {
        let data = any_font();
        let spec = TextSpec {
            size: 40.0,
            ruby: vec![(0, 1, "ab".to_string())],
            ..TextSpec::new("X")
        };
        let glyphs = layout_with_font(&data, 0, &spec);
        let base = glyphs.iter().find(|glyph| !glyph.ruby).expect("base glyph");
        let ruby: Vec<_> = glyphs.iter().filter(|glyph| glyph.ruby).collect();
        assert_eq!(ruby.len(), 2);
        for glyph in ruby {
            assert!(glyph.size < base.size);
            assert!(glyph.y < base.y, "ruby sits above the base line");
        }
    }

    #[test]
    fn ruby_on_the_second_of_two_identical_lines_stays_on_that_line() {
        let data = any_font();
        // Two identical lines: the annotation belongs to the second one, so a
        // lookup that searches for the line text by value would put it on the
        // first line.
        let spec = TextSpec {
            size: 20.0,
            line_height: 1.5,
            ruby: vec![(2, 3, "a".to_string())],
            ..TextSpec::new("a\na")
        };
        let glyphs = layout_with_font(&data, 0, &spec);
        let base: Vec<_> = glyphs.iter().filter(|glyph| !glyph.ruby).collect();
        let ruby: Vec<_> = glyphs.iter().filter(|glyph| glyph.ruby).collect();
        assert_eq!(base.len(), 2);
        assert_eq!(ruby.len(), 1, "one ruby run on the second line");
        assert!(
            ruby[0].y < base[1].y && ruby[0].y > base[0].y,
            "ruby must sit above the second line, not the first: ruby y {}, lines {} and {}",
            ruby[0].y,
            base[0].y,
            base[1].y
        );
    }

    #[test]
    fn ruby_on_a_multibyte_line_covers_the_right_glyphs() {
        let data = any_font();
        // "日本語": each character is three bytes, so the middle one is [3, 6).
        let spec = TextSpec {
            size: 30.0,
            ruby: vec![(3, 6, "x".to_string())],
            ..TextSpec::new("日本語")
        };
        let glyphs = layout_with_font(&data, 0, &spec);
        let base: Vec<_> = glyphs.iter().filter(|glyph| !glyph.ruby).collect();
        assert_eq!(base.len(), 3);
        let ruby = glyphs.iter().find(|glyph| glyph.ruby).expect("ruby glyph");
        let middle = base[1].x;
        assert!(
            (ruby.x - middle).abs() < (base[2].x - base[1].x),
            "ruby must sit over the second character, not the first"
        );
    }

    #[test]
    fn a_ruby_range_that_splits_a_character_is_skipped() {
        let data = any_font();
        // Byte 1 is inside the first character of "日" (three bytes): slicing
        // there used to panic.
        let spec = TextSpec {
            size: 30.0,
            ruby: vec![(0, 1, "x".to_string())],
            ..TextSpec::new("日本語")
        };
        let glyphs = layout_with_font(&data, 0, &spec);
        assert_eq!(glyphs.iter().filter(|glyph| glyph.ruby).count(), 0);
        assert_eq!(glyphs.len(), 3, "the base text still lays out");
    }

    #[test]
    fn on_path_text_follows_the_polyline() {
        let data = any_font();
        let spec = TextSpec {
            size: 16.0,
            on_path: Some(vec![(0.0, 100.0), (200.0, 100.0)]),
            ..TextSpec::new("abcd")
        };
        let glyphs = layout_with_font(&data, 0, &spec);
        assert_eq!(glyphs.len(), 4);
        assert!(glyphs.windows(2).all(|pair| pair[1].x > pair[0].x));
        assert!(glyphs.iter().all(|glyph| (glyph.y - 100.0).abs() < 0.01));
        assert!(glyphs.iter().all(|glyph| glyph.angle.abs() < 0.01));
    }

    #[test]
    fn authoring_a_string_produces_painted_pixels() {
        let fonts = Fonts::new();
        let spec = TextSpec { size: 48.0, ..TextSpec::new("Sumi") };
        let (image, origin) = render(&spec, &fonts).expect("rendered text");
        assert!(image.width > 40 && image.height > 20);
        let painted = image.data.chunks_exact(4).filter(|pixel| pixel[3] > 0).count();
        assert!(painted > 100, "the glyphs must cover pixels, got {painted}");
        assert!(origin.0 <= 0 && origin.1 <= 0, "origin sits outside the ink box");
    }

    #[test]
    fn an_empty_string_renders_nothing() {
        let fonts = Fonts::new();
        assert!(render(&TextSpec::new(""), &fonts).is_none());
    }
}
