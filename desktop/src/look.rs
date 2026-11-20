//! The interface's look, as the Android app has it (ui/Theme.kt, ui/Type.kt, ui/Icons.kt): its
//! colours and sizes (points, which are its dp), the Inter type it bundles, and its icons and
//! shapes, drawn at the size they show.
//!
//! The colours are the Sumi identity's, not the Android app's: an ink-stained atelier of
//! aubergine-black structural surfaces (ground, surface, raised), a rose mineral accent, and a
//! single light paper plane for the artwork and the studio's mark. The light look keeps the same
//! accent, deepened for paper, with the paper as its ground.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, TextureHandle, TextureOptions, Ui, pos2, vec2};
use resvg::tiny_skia;

use crate::icons::Icon;

/// The light interface (Prefs) is on: panels and bars go light grey with dark text.
static LIGHT: AtomicBool = AtomicBool::new(false);

pub fn light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

pub fn set_light(on: bool) {
    LIGHT.store(on, Ordering::Relaxed);
}

/// The accent a plugin gives the interface (0xRRGGBB with the top byte set), or 0: as measured.
static ACCENT_GIVEN: AtomicU32 = AtomicU32::new(0);

/// The interface's active-control colours (0xRRGGBB): the ACTIVE pair, and the legacy indigo,
/// celadon and blue Done keys plugins may still name. A plugin's accent replaces these, so every
/// old key stays recognised.
const ACCENT_KEYS: [u32; 10] = [
    0x0076FD, 0x0190FF, 0x0A84FF, 0x007AFF, 0x3D7AB8, 0x5B9BD5, 0x79AFD2, 0x3A73AB,
    // keep in step with pc::ACTIVE
    0xD4A8B0, 0x814C56,
];

/// The accent (0xRRGGBB) as a plugin gives it, or none: the interface's own active controls.
pub fn set_accent(rgb: Option<u32>) {
    ACCENT_GIVEN.store(rgb.map_or(0, |rgb| 0xFF00_0000 | (rgb & 0xFF_FFFF)), Ordering::Relaxed);
}

/// `v` (0xAARRGGBB), or the accent a plugin gives where `v` is one of the interface's active keys
/// (its alpha kept).
fn recoloured(v: u32) -> u32 {
    recolour(v, ACCENT_GIVEN.load(Ordering::Relaxed))
}

/// `v` (0xAARRGGBB) with `given` (as [`ACCENT_GIVEN`] holds it) in place of the interface's active
/// keys.
fn recolour(v: u32, given: u32) -> u32 {
    if given != 0 && ACCENT_KEYS.contains(&(v & 0xFF_FFFF)) { (v & 0xFF00_0000) | (given & 0xFF_FFFF) } else { v }
}

/// A colour the interface shows as the first (0xAARRGGBB) while dark, the second while light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Themed(pub u32, pub u32);

/// A colour as written: the same either way (0xAARRGGBB), or one for each look.
pub trait Shade: Copy {
    /// The colour now, 0xAARRGGBB.
    fn value(self) -> u32;
}

impl Shade for u32 {
    fn value(self) -> u32 {
        self
    }
}

impl Shade for Themed {
    fn value(self) -> u32 {
        if light() { self.1 } else { self.0 }
    }
}

/// The same colour either way.
impl From<u32> for Themed {
    fn from(v: u32) -> Themed {
        Themed(v, v)
    }
}

/// A colour written 0xAARRGGBB, as the Android app writes them (as it is now, for one with a
/// light look).
pub fn argb(v: impl Shade) -> Color32 {
    let [a, r, g, b] = recoloured(v.value()).to_be_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// `a` turned `t` (0..1) of the way to `b`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_premultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

/// A chrome surface's `fill` as it recedes by `dim` (0..1): the fill stepped that far darker, its
/// alpha kept. Both looks step the same way; the dark atelier's surface and paper's light one both
/// read as stepping back when they darken a few percent, where a lighter paper bar would come
/// forward instead. The ink, glyphs and accent drawn on it keep their own colours.
pub fn receded(fill: Color32, dim: f32) -> Color32 {
    // a dim that isn't a number would blacken the surface outright: none is the safe reading
    let keep = 1.0 - if dim.is_finite() { dim.clamp(0.0, 1.0) } else { 0.0 };
    let [r, g, b, a] = fill.to_srgba_unmultiplied();
    let step = |v: u8| (f32::from(v) * keep).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(step(r), step(g), step(b), a)
}

/// The interface's colours (0xAARRGGBB, dark and light) and sizes (points), named as the Android
/// app's `Pc`. The colours are the Sumi identity's: the dark look is its ink-stained atelier (ground
/// #211F22, surface #2C292D, raised #383337, line #50484C), the light look its paper equivalent; the
/// rose accent (#D4A8B0) is the same hue in both, deepened for the paper look (#814C56) so that ink
/// still reads on it.
pub mod pc {
    use super::Themed;

    /// The structural surfaces and rule the identity names (`--surface`, `--raised`, `--line`),
    /// and their derived paper equivalents.
    const SURFACE: Themed = Themed(0xFF2C292D, 0xFFF7F2EA);
    const RAISED: Themed = Themed(0xFF383337, 0xFFEDE6DB);
    const LINE: Themed = Themed(0xFF50484C, 0xFFD0C5B8);
    /// The identity's ink (`--ink`), the paper (`--paper`), the words on them and the secondary
    /// ink (`--text`, `--muted`).
    const PAPER_PLANE: Themed = Themed(0xFFE6DED4, 0xFFE6DED4);
    const INK: Themed = Themed(0xFF302C2E, 0xFF302C2E);
    const WORDS: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
    const SECONDARY: Themed = Themed(0xFFBDB2B2, 0xFF645C54);
    /// The rose mineral accent (`--accent`): the soft rose on the dark surfaces, the same rose
    /// deepened for paper, where a pale one would not carry ink.
    const ROSE: Themed = Themed(0xFFD4A8B0, 0xFF814C56);

    /// The accent's active control fill: rose, with ink on it in both looks.
    pub const ACTIVE: Themed = ROSE;
    /// The ink on an active control (an inverse of its rose); the old name stays for callers.
    pub const ACCENT_INK: Themed = Themed(INK.0, PAPER_PLANE.1);
    /// The active control, still named for the accent it replaced.
    pub const ACCENT: Themed = ACTIVE;
    /// Paper and ink marks, the same in both looks.
    pub const PAPER: Themed = PAPER_PLANE;
    /// A small cinnabar seal, never a general accent: the one warm red the rose does not take.
    pub const SEAL_FILL: Themed = Themed(0xFFB5322A, 0xFFB5322A);
    pub const SEAL_TINT: Themed = Themed(0xFF3A2624, 0xFFF3E2DA);
    /// Text and a menu's words: warm white on the plum surfaces, ink on paper.
    pub const TEXT: Themed = WORDS;
    pub const MENU_INK: Themed = Themed(0xFFFFFFFF, 0xFF000000);
    /// The bar over the canvas, and the central brush pill's raised plate.
    pub const BAR: Themed = SURFACE;
    pub const BAR_PILL: Themed = RAISED;
    pub const BAR_TEXT: Themed = SECONDARY;
    pub const BAR_GLYPH: Themed = SECONDARY;
    /// The ink glyph of a selected tool, and of both square and round action discs: ink on the rose
    /// plate while dark, paper on the deepened rose while light.
    pub const BAR_GLYPH_SELECTED: Themed = ACCENT_INK;
    pub const ACTION_DISC: Themed = RAISED;
    pub const ACTION_GLYPH: Themed = SECONDARY;
    pub const ACTION_DISC_SELECTED: Themed = ACTIVE;
    /// A selected tool's rose plate; the rim is the plate's own colour.
    pub const TOOL_PLATE: Themed = ACTIVE;
    pub const TOOL_PLATE_EDGE: Themed = ACTIVE;
    pub const COLOR_RING: Themed = Themed(0xFFBDB2B2, 0xFF9A8C8E);
    pub const BAR_HEIGHT: f32 = 44.0;
    pub const BAR_MARGIN: f32 = 40.0;
    pub const BAR_BUTTON_WIDTH: f32 = 44.0;
    pub const BAR_BUTTON_HEIGHT: f32 = 38.0;
    pub const GALLERY_WIDTH: f32 = 58.0;
    pub const ACTION_DISC_SIZE: f32 = 32.0;
    pub const COLOR_DOT: f32 = 31.0;

    pub const SIDE_BAR: Themed = SURFACE;
    pub const SLIDER_TRACK: Themed = Themed(0xFF383337, 0xFFDDD4C7);
    pub const SLIDER_RIM: Themed = Themed(LINE.0, 0xFFB3A79A);
    pub const SLIDER_RIM_WIDTH: f32 = 0.8;
    pub const SLIDER_THUMB: Themed = Themed(PAPER_PLANE.0, INK.1);
    pub const SIDE_GLYPH: Themed = SECONDARY;
    pub const SIDE_GLYPH_DISABLED: Themed = Themed(0x1AFFFFFF, 0x33000000);
    pub const SIDE_BAR_WIDTH: f32 = 51.0;
    pub const SIDE_BAR_HEIGHT: f32 = 519.0;
    pub const SIDE_BAR_CORNER: f32 = 20.0;
    /// Where the side bar's centre sits, as a fraction of the window's height.
    pub const SIDE_BAR_CENTRE: f32 = 0.5234;
    /// Left under the side bar in a window too short for its measured tracks, and how short they go.
    const SHORT_SIDE_BAR_FOOT: f32 = 17.0;
    const SHORTEST_TRACK: f32 = 44.0;
    pub const SLIDER_WIDTH: f32 = 40.0;
    pub const SLIDER_HEIGHT: f32 = 170.0;
    pub const SLIDER_CORNER: f32 = 13.0;
    pub const THUMB_WIDTH: f32 = 32.0;
    pub const THUMB_HEIGHT: f32 = 18.0;
    pub const THUMB_INSET: f32 = 4.0;
    pub const MODIFY_SIZE: f32 = 22.5;
    pub const MODIFY_STROKE: f32 = 2.4;
    pub const MODIFY_CORNER: f32 = 6.0;

    pub const GLASS_MENU_EDGE: Themed = Themed(0x24FFFFFF, 0x14000000);

    /// The gaps between the tool buttons and between the action buttons for a bar `width` wide:
    /// the room left after both margins picks one of five steps, or where its two ends would run
    /// into each other (a narrow window), the first tighter one that keeps them apart.
    pub fn bar_gaps(width: f32) -> (f32, f32) {
        let room = width - BAR_MARGIN * 2.0;
        let measured = if room < 435.0 {
            (2.0, 4.0)
        } else if room < 530.0 {
            (8.0, 14.0)
        } else if room < 625.0 {
            (26.0, 14.0)
        } else if room < 730.0 {
            (8.0, 14.0)
        } else {
            (26.0, 14.0)
        };
        let needs = |(tool, action): (f32, f32)| {
            BAR_MARGIN * 2.0 + GALLERY_WIDTH + (BAR_BUTTON_WIDTH + action) * 4.0 + BAR_BUTTON_WIDTH * 5.0 + tool * 4.0
        };
        [measured, (8.0, 14.0)].into_iter().find(|&gaps| needs(gaps) <= width).unwrap_or((2.0, 4.0))
    }

    /// The side bar in a window `height` tall: its top from the window's, and how long each of its
    /// two tracks is. At `SIDE_BAR_CENTRE` with its measured tracks wherever that clears the top
    /// bar; in a shorter window just under the top bar, its tracks giving up what it is short of,
    /// so that all of it can be reached.
    pub fn side_bar_fit(height: f32) -> (f32, f32) {
        let top = height * SIDE_BAR_CENTRE - SIDE_BAR_HEIGHT / 2.0;
        if top >= BAR_HEIGHT {
            return (top, SLIDER_HEIGHT);
        }
        let rest = SIDE_BAR_HEIGHT - SLIDER_HEIGHT * 2.0;
        (BAR_HEIGHT, ((height - BAR_HEIGHT - SHORT_SIDE_BAR_FOOT - rest) / 2.0).clamp(SHORTEST_TRACK, SLIDER_HEIGHT))
    }
}

/// How lit a button is (0..1): selection is a state, not a transition, so a tool or a shortcut
/// takes effect at once.
pub fn highlight(_ctx: &egui::Context, _id: egui::Id, on: bool) -> f32 {
    if on { 1.0 } else { 0.0 }
}

/// Letter spacing (em) that keeps Inter quiet and even across the interface: less cramped than the
/// Android port's measured widths, which the identity's calm, editorial hierarchy replaces.
const TRACKING: f32 = -0.03;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
    Bold,
}

impl Weight {
    fn name(self) -> &'static str {
        match self {
            Weight::Regular => "Inter-Regular",
            Weight::Medium => "Inter-Medium",
            Weight::SemiBold => "Inter-SemiBold",
            Weight::Bold => "Inter-Bold",
        }
    }

    pub fn font(self, size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(self.name().into()))
    }
}

/// Inter's faces (bundled with the Android app, OFL).
pub static INTER_FACES: [(Weight, &[u8]); 4] = [
    (Weight::Regular, include_bytes!("../../assets/shared/fonts/Inter-Regular.ttf")),
    (Weight::Medium, include_bytes!("../../assets/shared/fonts/Inter-Medium.ttf")),
    (Weight::SemiBold, include_bytes!("../../assets/shared/fonts/Inter-SemiBold.ttf")),
    (Weight::Bold, include_bytes!("../../assets/shared/fonts/Inter-Bold.ttf")),
];

/// Inter (bundled with the Android app, OFL) for all text, egui's own fonts behind it for what it
/// lacks, and the system's behind those for other scripts (read on a thread of their own, put in
/// once read).
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(fonts(Vec::new()));
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let system = system_fonts();
        if !system.is_empty() {
            ctx.set_fonts(fonts(system));
            ctx.request_repaint();
        }
    });
}

/// The app's fonts, with `system`'s `(name, font)` after egui's own.
pub(crate) fn fonts(system: Vec<(String, egui::FontData)>) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mut fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, font) in system {
        fonts.font_data.insert(name.clone(), Arc::new(font));
        if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
            mono.push(name.clone());
        }
        fallback.push(name);
    }
    for &(weight, bytes) in &INTER_FACES {
        let name = weight.name().to_owned();
        fonts.font_data.insert(name.clone(), Arc::new(egui::FontData::from_static(bytes)));
        let mut family = vec![name];
        family.extend(fallback.iter().cloned());
        fonts.families.insert(FontFamily::Name(weight.name().into()), family);
    }
    fonts
        .families
        .insert(FontFamily::Proportional, [Weight::Regular.name().to_owned()].into_iter().chain(fallback).collect());
    fonts
}

/// A Mac's fonts for the scripts Inter and egui's own lack (as the other systems' below), found by
/// their families through Core Text (where the files are differs from one release to the next):
/// `(name, font)` each, a collection's own face of the family.
#[cfg(target_os = "macos")]
fn system_fonts() -> Vec<(String, egui::FontData)> {
    const FAMILIES: [&str; 5] =
        ["PingFang SC", "Hiragino Sans GB", "Apple SD Gothic Neo", "Arial Unicode MS", "Apple Symbols"];
    let mut read: Vec<(std::path::PathBuf, String)> = Vec::new();
    FAMILIES
        .iter()
        .filter_map(|family| {
            let (file, face) = core_text::font_file(family)?;
            // two families in one face are read once
            if read.contains(&(file.clone(), face.clone())) {
                return None;
            }
            let bytes = std::fs::read(&file).ok()?;
            // its face in a collection, by its PostScript name
            let index = swash::FontDataRef::new(&bytes)
                .and_then(|data| {
                    data.fonts().position(|font| {
                        let name = font.localized_strings().find_by_id(swash::StringId::PostScript, None);
                        name.is_some_and(|name| name.to_string() == face)
                    })
                })
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0);
            read.push((file, face));
            let mut font = egui::FontData::from_owned(bytes);
            font.index = index;
            Some((format!("system {family}"), font))
        })
        .collect()
}

/// Core Text's calls for the fonts' files, unsafe as the system's are.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod core_text {
    use std::path::PathBuf;

    use objc2_core_foundation::{CFString, CFURL};
    use objc2_core_text::{CTFont, kCTFontURLAttribute};

    /// The file of the font family `family` and the PostScript name of its face, if the system
    /// has it (asked for one it lacks, Core Text gives another family).
    pub fn font_file(family: &str) -> Option<(PathBuf, String)> {
        // SAFETY: no matrix (the font as it is); the attribute asked of it is its file's URL
        unsafe {
            let font = CTFont::with_name(&CFString::from_str(family), 12.0, std::ptr::null());
            if font.family_name().to_string() != family {
                return None;
            }
            let url = font.attribute(kCTFontURLAttribute)?.downcast::<CFURL>().ok()?;
            Some((url.to_file_path()?, font.post_script_name().to_string()))
        }
    }
}

/// The system's fonts for the scripts Inter and egui's own lack (Chinese, Japanese, Korean,
/// Arabic, Hebrew, Indic, Thai, ...), those of them it has: names (a brush's from another app,
/// an artwork's typed in any language) shown as written. `(name, font)` each.
#[cfg(not(target_os = "macos"))]
fn system_fonts() -> Vec<(String, egui::FontData)> {
    #[cfg(windows)]
    let (dir, files): (std::path::PathBuf, &[&str]) = (
        std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts"),
        &[
            "msyh.ttc",
            "malgun.ttf",
            "segoeui.ttf",
            "Nirmala.ttc",
            "LeelawUI.ttf",
            "ebrima.ttf",
            "gadugi.ttf",
            "seguisym.ttf",
        ],
    );
    #[cfg(unix)]
    let (dir, files): (std::path::PathBuf, &[&str]) = (
        std::path::PathBuf::from("/usr/share/fonts"),
        &[
            "opentype/noto/NotoSansCJK-Regular.ttc",
            "noto-cjk/NotoSansCJK-Regular.ttc",
            "google-noto-cjk/NotoSansCJK-Regular.ttc",
            "truetype/droid/DroidSansFallbackFull.ttf",
            "truetype/noto/NotoSansArabic-Regular.ttf",
            "truetype/noto/NotoSansHebrew-Regular.ttf",
            "truetype/noto/NotoSansDevanagari-Regular.ttf",
            "truetype/noto/NotoSansThai-Regular.ttf",
            "truetype/dejavu/DejaVuSans.ttf",
        ],
    );
    #[cfg(not(any(windows, unix)))]
    let (dir, files): (std::path::PathBuf, &[&str]) = (std::path::PathBuf::new(), &[]);
    files
        .iter()
        .filter_map(|file| {
            let bytes = std::fs::read(dir.join(file)).ok()?;
            // a collection's first face (each of these has its regular weight first)
            Some((format!("system {file}"), egui::FontData::from_owned(bytes)))
        })
        .collect()
}

/// Text in runs of weights, `size` points, placed by `align` at `at`; returns where it went.
pub fn text_runs(
    painter: &Painter,
    at: Pos2,
    align: Align2,
    runs: &[(&str, Weight)],
    size: f32,
    colour: Color32,
) -> Rect {
    tracked_runs(painter, at, align, runs, size, colour, TRACKING)
}

/// The tracking (em) of rows' labels near 15 points, where [`TRACKING`] (set over 18-24 point
/// labels) draws them narrower.
pub const ROW_TRACKING: f32 = -0.018;

/// Text of one weight, its letters `tracking` em apart; see [`text_runs`].
#[allow(clippy::too_many_arguments)]
pub fn text_tracked(
    painter: &Painter,
    at: Pos2,
    align: Align2,
    text: &str,
    size: f32,
    weight: Weight,
    colour: Color32,
    tracking: f32,
) -> Rect {
    tracked_runs(painter, at, align, &[(text, weight)], size, colour, tracking)
}

fn tracked_runs(
    painter: &Painter,
    at: Pos2,
    align: Align2,
    runs: &[(&str, Weight)],
    size: f32,
    colour: Color32,
    tracking: f32,
) -> Rect {
    let mut job = LayoutJob::default();
    for (text, weight) in runs {
        let format = TextFormat {
            font_id: weight.font(size),
            color: colour,
            extra_letter_spacing: tracking * size,
            ..Default::default()
        };
        job.append(text, 0.0, format);
    }
    let galley = painter.layout_job(job);
    let rect = align.anchor_size(at, galley.size());
    painter.galley(rect.min, galley, colour);
    rect
}

/// Text of one weight, `size` points, wrapped `width` wide, laid out to be drawn.
pub fn wrapped(
    painter: &Painter,
    text: &str,
    size: f32,
    weight: Weight,
    colour: Color32,
    width: f32,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    let format = TextFormat {
        font_id: weight.font(size),
        color: colour,
        extra_letter_spacing: TRACKING * size,
        ..Default::default()
    };
    job.append(text, 0.0, format);
    job.wrap.max_width = width;
    painter.layout_job(job)
}

/// Text wrapped `width` wide with its lines centred (about x = 0 where it is drawn) and
/// `line_height` apart.
pub fn wrapped_centred(
    painter: &Painter,
    text: &str,
    size: f32,
    weight: Weight,
    colour: Color32,
    width: f32,
    line_height: f32,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    let format = TextFormat {
        font_id: weight.font(size),
        color: colour,
        extra_letter_spacing: TRACKING * size,
        line_height: Some(line_height),
        ..Default::default()
    };
    job.append(text, 0.0, format);
    job.wrap.max_width = width;
    job.halign = egui::Align::Center;
    painter.layout_job(job)
}

/// How far across (points) the pointer has gone since the last frame while held on `response`, a
/// slider; `held` keeps which slider is held and where the pointer was.
pub fn dragged_by(held: &mut Option<(egui::Id, f32)>, response: &egui::Response) -> f32 {
    match response.interact_pointer_pos().filter(|_| response.is_pointer_button_down_on()) {
        Some(p) => {
            let moved = match *held {
                Some((id, x)) if id == response.id => p.x - x,
                _ => 0.0,
            };
            *held = Some((response.id, p.x));
            moved
        }
        None => {
            if held.is_some_and(|(id, _)| id == response.id) {
                *held = None;
            }
            0.0
        }
    }
}

/// How wide `text` of one weight is at `size` points, as [`text`] draws it.
pub fn text_width(painter: &Painter, text: &str, size: f32, weight: Weight) -> f32 {
    let mut job = LayoutJob::default();
    let format = TextFormat { font_id: weight.font(size), extra_letter_spacing: TRACKING * size, ..Default::default() };
    job.append(text, 0.0, format);
    painter.layout_job(job).size().x
}

/// Text of one weight; see [`text_runs`].
pub fn text(
    painter: &Painter,
    at: Pos2,
    align: Align2,
    text: &str,
    size: f32,
    weight: Weight,
    colour: Color32,
) -> Rect {
    text_runs(painter, at, align, &[(text, weight)], size, colour)
}

/// The whole of a texture.
const ALL: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1.0, 1.0) };

/// Icons and shapes drawn for the screen's density, kept until it changes.
#[derive(Default)]
pub struct Look {
    icons: HashMap<(&'static str, u32), TextureHandle>,
    /// Each drawn shape's texture and what it was drawn from.
    shapes: HashMap<&'static str, (u64, TextureHandle)>,
}

impl Look {
    /// `icon` shown `width` points across (its frame scaled to it), centred at `centre`, in `colour`.
    pub fn icon(&mut self, ui: &Ui, icon: &Icon, centre: Pos2, width: f32, colour: Color32) {
        let ppp = ui.ctx().pixels_per_point();
        let px = (width * ppp).round().max(1.0) as u32;
        let texture = self
            .icons
            .entry((icon.name, px))
            .or_insert_with(|| ui.ctx().load_texture(icon.name, raster_icon(icon, px), TextureOptions::LINEAR));
        let height = width * icon.height / icon.width;
        ui.painter().image(texture.id(), Rect::from_center_size(centre, vec2(width, height)), ALL, colour);
    }

    /// `icon` stretched to fill `rect` (drawn as wide as it, then squeezed or stretched to its
    /// height), in `colour`.
    pub fn icon_in(&mut self, ui: &Ui, icon: &Icon, rect: Rect, colour: Color32) {
        let ppp = ui.ctx().pixels_per_point();
        let px = (rect.width() * ppp).round().max(1.0) as u32;
        let texture = self
            .icons
            .entry((icon.name, px))
            .or_insert_with(|| ui.ctx().load_texture(icon.name, raster_icon(icon, px), TextureOptions::LINEAR));
        ui.painter().image(texture.id(), rect, ALL, colour);
    }

    /// A shape drawn by `draw` (into a pixmap of `rect` at the screen's density, scaled so its
    /// units are points from `rect`'s corner), kept as `slot` until `key` changes.
    pub fn shape(
        &mut self,
        ui: &Ui,
        slot: &'static str,
        key: impl Hash,
        rect: Rect,
        draw: impl FnOnce(&mut tiny_skia::Pixmap, tiny_skia::Transform),
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let (w, h) = ((rect.width() * ppp).ceil().max(1.0) as u32, (rect.height() * ppp).ceil().max(1.0) as u32);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // drawn again when the interface turns light or dark
        (key, w, h, light()).hash(&mut hasher);
        let hash = hasher.finish();
        let fresh = self.shapes.get(slot).is_none_or(|(at, _)| *at != hash);
        if fresh && let Some(mut pixmap) = tiny_skia::Pixmap::new(w, h) {
            draw(&mut pixmap, tiny_skia::Transform::from_scale(ppp, ppp));
            let image = egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data());
            match self.shapes.get_mut(slot) {
                Some((at, texture)) => {
                    texture.set(image, TextureOptions::LINEAR);
                    *at = hash;
                }
                None => {
                    let texture = ui.ctx().load_texture(slot, image, TextureOptions::LINEAR);
                    self.shapes.insert(slot, (hash, texture));
                }
            }
        }
        if let Some((_, texture)) = self.shapes.get(slot) {
            let shown = Rect::from_min_size(rect.min, vec2(w as f32, h as f32) / ppp);
            ui.painter().image(texture.id(), shown, ALL, Color32::WHITE);
        }
    }
}

/// `icon` drawn white `px` pixels across.
fn raster_icon(icon: &Icon, px: u32) -> egui::ColorImage {
    let mut svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}">"#, icon.width, icon.height);
    for part in icon.parts {
        if part.fill {
            let rule = if part.even_odd { "evenodd" } else { "nonzero" };
            svg.push_str(&format!(r##"<path d="{}" fill="#fff" fill-rule="{rule}"/>"##, part.d));
        }
        if part.stroke > 0.0 {
            svg.push_str(&format!(
                r##"<path d="{}" fill="none" stroke="#fff" stroke-width="{}" stroke-linecap="round" stroke-linejoin="round"/>"##,
                part.d, part.stroke
            ));
        }
    }
    svg.push_str("</svg>");
    let ph = ((px as f32) * icon.height / icon.width).round().max(1.0) as u32;
    let drawn = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok().and_then(|tree| {
        let mut pixmap = tiny_skia::Pixmap::new(px, ph)?;
        let scale = px as f32 / icon.width;
        resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        Some(pixmap)
    });
    match drawn {
        Some(p) => egui::ColorImage::from_rgba_premultiplied([px as usize, ph as usize], p.data()),
        None => egui::ColorImage::filled([1, 1], Color32::TRANSPARENT),
    }
}

/// A rounded rectangle's outline: corners `[top left, top right, bottom right, bottom left]`.
pub fn round_rect(pb: &mut tiny_skia::PathBuilder, r: Rect, [tl, tr, br, bl]: [f32; 4]) {
    // the control points' share of a quarter circle's radius
    const K: f32 = 0.552_284_8;
    let (x0, y0, x1, y1) = (r.left(), r.top(), r.right(), r.bottom());
    pb.move_to(x0 + tl, y0);
    pb.line_to(x1 - tr, y0);
    pb.cubic_to(x1 - tr + tr * K, y0, x1, y0 + tr - tr * K, x1, y0 + tr);
    pb.line_to(x1, y1 - br);
    pb.cubic_to(x1, y1 - br + br * K, x1 - br + br * K, y1, x1 - br, y1);
    pb.line_to(x0 + bl, y1);
    pb.cubic_to(x0 + bl - bl * K, y1, x0, y1 - bl + bl * K, x0, y1 - bl);
    pb.line_to(x0, y0 + tl);
    pb.cubic_to(x0, y0 + tl - tl * K, x0 + tl - tl * K, y0, x0 + tl, y0);
    pb.close();
}

/// Fills `path` in `colour`, even-odd.
pub fn fill(pixmap: &mut tiny_skia::Pixmap, path: Option<tiny_skia::Path>, colour: Color32, at: tiny_skia::Transform) {
    let Some(path) = path else { return };
    let mut paint = tiny_skia::Paint::default();
    let [r, g, b, a] = colour.to_srgba_unmultiplied();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, tiny_skia::FillRule::EvenOdd, at, None);
}

/// A cubic Bézier easing from (0, 0) to (1, 1) through (`x1`, `y1`) and (`x2`, `y2`), as CSS and
/// Compose give them, at `t`.
/// A spring settling over its own time (`seconds`, as damped as `damping`, starting at
/// `velocity`), as the Android app's (ui/Motion.kt, Spring): how far along it is at `fraction`
/// (0..1) of that time, overshooting while it is under-damped.
pub fn spring(seconds: f32, damping: f32, velocity: f32, fraction: f32) -> f32 {
    if fraction >= 1.0 {
        return 1.0;
    }
    let z = f64::from(damping).max(0.01);
    let v = f64::from(velocity);
    // how far from its end it can still be at time `t` at frequency `w`
    let envelope = |w: f64, t: f64| {
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let c2 = (v - z * w) / wd;
            (1.0 + c2 * c2).sqrt() * (-z * w * t).exp()
        } else if z == 1.0 {
            (1.0 + (v - w).abs() * t) * (-w * t).exp()
        } else {
            let s = (z * z - 1.0).sqrt();
            let (r1, r2) = (-w * (z - s), -w * (z + s));
            let c1 = (v + r2) / (r1 - r2);
            c1.abs() * (r1 * t).exp() + (-1.0 - c1).abs() * (r2 * t).exp()
        }
    };
    // the slowest frequency that is within a thousandth of its end by the time given
    let seconds = f64::from(seconds);
    let (mut lo, mut hi) = (0.01, 2000.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if envelope(mid, seconds) > 1e-3 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (w, t) = (hi, f64::from(fraction.max(0.0)) * seconds);
    let off = if z < 1.0 {
        let wd = w * (1.0 - z * z).sqrt();
        (-z * w * t).exp() * (-(wd * t).cos() + (v - z * w) / wd * (wd * t).sin())
    } else if z == 1.0 {
        (-1.0 + (v - w) * t) * (-w * t).exp()
    } else {
        let s = (z * z - 1.0).sqrt();
        let (r1, r2) = (-w * (z - s), -w * (z + s));
        let c1 = (v + r2) / (r1 - r2);
        c1 * (r1 * t).exp() + (-1.0 - c1) * (r2 * t).exp()
    };
    (1.0 + off) as f32
}

pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let curve = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
    let slope =
        |a: f32, b: f32, s: f32| 3.0 * a * (1.0 - s).powi(2) + 6.0 * (b - a) * s * (1.0 - s) + 3.0 * (1.0 - b) * s * s;
    // where along the curve x is t: Newton's method, then halving if that didn't settle
    let mut s = t;
    for _ in 0..8 {
        let (dx, d) = (curve(x1, x2, s) - t, slope(x1, x2, s));
        if dx.abs() < 1e-5 || d.abs() < 1e-6 {
            break;
        }
        s = (s - dx / d).clamp(0.0, 1.0);
    }
    if (curve(x1, x2, s) - t).abs() >= 1e-4 {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            s = (lo + hi) / 2.0;
            if curve(x1, x2, s) < t {
                lo = s;
            } else {
                hi = s;
            }
        }
    }
    curve(y1, y2, s)
}

/// How far the chrome recedes while a stroke is painted: a few percent, quiet enough that only the
/// artwork's edge reads as bright.
pub const CHROME_DIM: f32 = 0.06;

/// What the chrome's dim settles towards: [`CHROME_DIM`] while a stroke is painted and motion is
/// welcome, else none. With reduced motion asked for, the chrome doesn't change at all rather than
/// changing at once: a bar that moves as a stroke begins is motion, however quiet.
pub fn chrome_dim_target(painting: bool, reduced_motion: bool) -> f32 {
    if painting && !reduced_motion { CHROME_DIM } else { 0.0 }
}

/// The chrome's dim `seconds` on from `from`, towards `target`: it recedes quickly as a stroke
/// begins and breathes back slowly as it ends, each an exponential approach of its own time
/// constant. It lands exactly on its target once it is within a thousandth of it, so the interface
/// stops asking for frames rather than settling towards nothing forever. Where [`spring`] owns a
/// transition with a known start and end, this steps on from wherever the dim was left: a stroke
/// begun mid-breath takes over from there without restarting.
pub fn settle_dim(from: f32, target: f32, seconds: f32) -> f32 {
    let target = if target.is_finite() { target.clamp(0.0, 1.0) } else { 0.0 };
    if !from.is_finite() {
        return target;
    }
    let from = from.clamp(0.0, 1.0);
    if !seconds.is_finite() || seconds <= 0.0 {
        return from;
    }
    // a stroke's start has the drop done in a blink
    const RECEDE: f32 = 0.045;
    // its end takes a breath longer
    const RETURN: f32 = 0.13;
    let tau = if target > from { RECEDE } else { RETURN };
    let next = target + (from - target) * (-seconds / tau).exp();
    if (next - target).abs() < 1e-3 { target } else { next }
}

/// Whether the chrome still needs frames for its dim: only while it differs from its target.
/// [`settle_dim`] lands exactly on the target, so a settled chrome stops asking for frames rather
/// than repainting forever with nothing animating.
pub fn dim_needs_repaint(dim: f32, target: f32) -> bool {
    dim != target
}

/// Whether the dim should step this pass: once per egui frame, not once per UI pass. egui re-runs
/// the UI when a pass is discarded for layout; both passes of one frame report the same frame
/// number, so comparing it with the frame last stepped advances the animation just once a frame.
pub fn dim_step_due(frame: u64, last_stepped: u64) -> bool {
    frame != last_stepped
}

#[cfg(test)]
mod tests {
    use super::{
        CHROME_DIM, argb, chrome_dim_target, dim_needs_repaint, dim_step_due, highlight, pc, receded, recolour,
        settle_dim, spring,
    };
    use crate::menu;
    use egui::Color32;

    #[test]
    fn a_plugin_s_accent_takes_the_place_of_the_interface_s_accents_only() {
        let red = 0xFF00_0000 | 0xFD0000;
        // none given: as measured
        assert_eq!(recolour(pc::ACCENT.0, 0), pc::ACCENT.0);
        // the active pair, a Done button's blue, a see-through accent; not a grey or a near blue
        assert_eq!(recolour(pc::ACCENT.0, red), 0xFFFD0000);
        assert_eq!(recolour(pc::ACCENT.1, red), 0xFFFD0000);
        assert_eq!(recolour(0xFF0190FF, red), 0xFFFD0000);
        assert_eq!(recolour(0x800076FD, red), 0x80FD0000);
        // the selected plate still answers a plugin's accent in both looks
        assert_eq!(recolour(pc::TOOL_PLATE.1, red), 0xFFFD0000);
        assert_eq!(recolour(pc::TOOL_PLATE_EDGE.1, red), 0xFFFD0000);
        assert_eq!(recolour(pc::TOOL_PLATE_EDGE.0, red), 0xFFFD0000);
        // old light selected-row indigo stays a plugin-recolourable key
        assert_eq!(recolour(0xFF3A73AB, red), 0xFFFD0000);
        assert_eq!(recolour(0xFF262626, red), 0xFF262626);
        assert_eq!(recolour(0xFF0076FE, red), 0xFF0076FE);
    }

    #[test]
    fn the_selected_tool_plate_is_the_rose_accent_and_its_glyph_reads_in_both_looks() {
        // Sumi: the rose plate carries ink in the dark look and paper on the deepened rose in the
        // light one; it is the accent, not a neutral plate.
        for (what, plate, glyph, accent) in [
            ("dark", pc::TOOL_PLATE.0, pc::BAR_GLYPH_SELECTED.0, pc::ACCENT.0),
            ("light", pc::TOOL_PLATE.1, pc::BAR_GLYPH_SELECTED.1, pc::ACCENT.1),
        ] {
            assert_eq!(plate, accent, "{what} active plate is not the accent: {plate:#08X}");
            let channels = [(plate >> 16) & 0xFF, (plate >> 8) & 0xFF, plate & 0xFF];
            let high = channels.iter().copied().max().unwrap();
            let low = channels.iter().copied().min().unwrap();
            assert!(high - low >= 20, "{what} accent plate carries no rose: {plate:#08X}");
            let got = contrast(plate, glyph);
            assert!(got >= 4.5, "{what} active plate/glyph: measured {got:.2}, wanted at least 4.5");
        }
    }

    #[test]
    fn the_active_pair_keeps_measured_contrast_in_both_looks() {
        for (what, fill, ink) in [("dark", pc::ACTIVE.0, pc::ACCENT_INK.0), ("light", pc::ACTIVE.1, pc::ACCENT_INK.1)] {
            let got = contrast(fill, ink);
            assert!(got >= 4.5, "{what} active fill/ink: measured {got:.2}, wanted at least 4.5");
        }
    }

    #[test]
    fn a_lit_button_is_instant_not_a_fade() {
        let ctx = egui::Context::default();
        let id = egui::Id::unique("lit");
        assert_eq!(highlight(&ctx, id, true), 1.0);
        assert_eq!(highlight(&ctx, id, false), 0.0);
    }

    #[test]
    fn a_plugin_accent_reaches_the_selected_tool_plate_in_both_looks() {
        let red = 0xFF00_0000 | 0xFD0000;
        assert_eq!(recolour(pc::TOOL_PLATE.0, red), red, "the dark active plate ignored the plugin accent");
        assert_eq!(recolour(pc::TOOL_PLATE.1, red), red, "the light active plate ignored the plugin accent");
    }

    #[test]
    fn a_plugin_accent_leaves_text_and_paper_alone() {
        let red = 0xFF00_0000 | 0xFD0000;
        assert_eq!(recolour(pc::TEXT.0, red), pc::TEXT.0);
        assert_eq!(recolour(pc::PAPER.0, red), pc::PAPER.0);
        assert_eq!(recolour(pc::BAR_GLYPH_SELECTED.0, red), pc::BAR_GLYPH_SELECTED.0);
        assert_eq!(recolour(0xFFE6DED4, red), 0xFFE6DED4);
    }

    /// The relative luminance of an opaque 0xRRGGBB colour: a measurement, not a certification.
    fn luminance(rgb: u32) -> f32 {
        fn channel(v: f32) -> f32 {
            let c = v / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        let r = channel(f32::from(((rgb >> 16) & 0xFF) as u8));
        let g = channel(f32::from(((rgb >> 8) & 0xFF) as u8));
        let b = channel(f32::from((rgb & 0xFF) as u8));
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (mut hi, mut lo) = (luminance(a), luminance(b));
        if hi < lo {
            (hi, lo) = (lo, hi);
        }
        (hi + 0.05) / (lo + 0.05)
    }

    /// `fg` (0xAARRGGBB) laid over the opaque `bg` (0xRRGGBB), as a translucent bar overlays its base.
    fn over(fg: u32, bg: u32) -> u32 {
        let a = f32::from(((fg >> 24) & 0xFF) as u8) / 255.0;
        let blend = |shift: u32| {
            let f = f32::from(((fg >> shift) & 0xFF) as u8);
            let b = f32::from(((bg >> shift) & 0xFF) as u8);
            ((f * a + b * (1.0 - a)).round() as u32) << shift
        };
        0xFF00_0000 | blend(16) | blend(8) | blend(0)
    }

    #[test]
    fn the_sumi_surfaces_keep_measured_ink_contrast_in_both_looks() {
        let dark_bar = over(pc::BAR.0, pc::BAR_PILL.0);
        let light_bar = over(pc::BAR.1, pc::BAR_PILL.1);
        let measured = [
            ("bar dark ink", dark_bar, pc::BAR_TEXT.0, 4.5),
            ("bar light ink", light_bar, pc::BAR_TEXT.1, 4.5),
            ("bar dark accent", pc::BAR.0, pc::ACCENT.0, 4.5),
            ("bar light accent", pc::BAR.1, pc::ACCENT.1, 4.5),
            ("pill dark name", pc::BAR_PILL.0, pc::TEXT.0, 4.5),
            ("pill light name", pc::BAR_PILL.1, pc::TEXT.1, 4.5),
            ("pill dark readout", pc::BAR_PILL.0, pc::BAR_TEXT.0, 4.5),
            ("pill light readout", pc::BAR_PILL.1, pc::BAR_TEXT.1, 4.5),
            ("tool plate dark glyph", pc::TOOL_PLATE.0, pc::BAR_GLYPH_SELECTED.0, 3.0),
            ("tool plate light glyph", pc::TOOL_PLATE.1, pc::BAR_GLYPH_SELECTED.1, 3.0),
            // the seal stays a readable mark on the paper plane, apart from the rose accent
            ("seal on paper", pc::SEAL_FILL.0, pc::PAPER.0, 4.5),
        ];
        for (what, surface, ink, least) in measured {
            let got = contrast(surface, ink);
            assert!(got >= least, "{what}: measured {got:.2}, wanted at least {least}");
        }
        // each surface and ink carries a real light value, not a duplicated dark one
        for themed in [
            pc::BAR,
            pc::BAR_PILL,
            pc::TEXT,
            pc::BAR_TEXT,
            pc::ACTION_DISC,
            pc::TOOL_PLATE,
            pc::TOOL_PLATE_EDGE,
            pc::SIDE_BAR,
            pc::SIDE_GLYPH,
            pc::BAR_GLYPH_SELECTED,
        ] {
            assert_ne!(themed.0, themed.1, "{themed:?} has no separate light value");
        }
    }

    /// The opaque ground a menu shows on: its own glass while dark, and the paper it fills over a
    /// panel while light (both mostly solid, so their fill is what is read).
    fn menu_grounds() -> (u32, u32) {
        (menu::GLASS.0 & 0x00FF_FFFF, menu::OVER_PANEL.1 & 0x00FF_FFFF)
    }

    #[test]
    fn the_menus_destructive_ink_reads_on_both_grounds() {
        let (dark, light) = menu_grounds();
        for (what, ground, ink) in [("dark", dark, menu::DANGER.0), ("light", light, menu::DANGER.1)] {
            let got = contrast(ground, ink);
            assert!(got >= 4.5, "{what} menu destructive ink: measured {got:.2}, wanted at least 4.5");
            // still a wrecker's red, and not the seal's cinnabar
            let (r, g, b) = ((ink >> 16) & 0xFF, (ink >> 8) & 0xFF, ink & 0xFF);
            assert!(r > g && r > b && r - g >= 40, "{what} destructive ink lost its red: {ink:#08X}");
            assert_ne!(ink, pc::SEAL_FILL.0, "{what} destructive ink is the seal colour");
        }
    }

    #[test]
    fn the_menus_edges_and_rules_stay_visible_on_both_grounds() {
        let (dark, light) = menu_grounds();
        // decorative hairline, not text: measured against the 2.6 the line was raised to
        for (what, line) in [("edge", menu::EDGE), ("rule", menu::RULE)] {
            let (on_dark, on_light) = (contrast(dark, line), contrast(light, line));
            assert!(on_dark >= 2.6, "{what} on the dark ground: measured {on_dark:.2}, wanted at least 2.6");
            assert!(on_light >= 2.6, "{what} on the light surface: measured {on_light:.2}, wanted at least 2.6");
        }
    }

    #[test]
    fn the_top_bars_ends_never_meet_and_a_wide_enough_window_keeps_its_measured_step() {
        assert_eq!(pc::bar_gaps(720.0), (8.0, 14.0));
        assert_eq!(pc::bar_gaps(1200.0), (26.0, 14.0));
        // the step for this width has gaps of 26 and 14, which would take 694
        assert_eq!(pc::bar_gaps(650.0), (8.0, 14.0));
        assert_eq!(pc::bar_gaps(610.0), (2.0, 4.0));
        for width in 558..2000 {
            let width = width as f32;
            let (tool, action) = pc::bar_gaps(width);
            let needs = pc::BAR_MARGIN * 2.0
                + pc::GALLERY_WIDTH
                + (pc::BAR_BUTTON_WIDTH + action) * 4.0
                + pc::BAR_BUTTON_WIDTH * 5.0
                + tool * 4.0;
            assert!(needs <= width, "{width}");
        }
    }

    #[test]
    fn the_side_bar_stays_as_measured_or_fits_under_the_top_bar() {
        // portrait top 333.5, landscape top 130
        assert!((pc::side_bar_fit(1133.0).0 - 333.5).abs() < 0.1);
        assert!((pc::side_bar_fit(744.0).0 - 130.0).abs() < 0.1);
        for height in 580..2000 {
            assert_eq!(pc::side_bar_fit(height as f32).1, pc::SLIDER_HEIGHT);
        }
        for height in 480..580 {
            let height = height as f32;
            let (top, track) = pc::side_bar_fit(height);
            assert_eq!(top, pc::BAR_HEIGHT);
            assert!(top + pc::SIDE_BAR_HEIGHT - (pc::SLIDER_HEIGHT - track) * 2.0 <= height, "{height}");
        }
    }

    #[test]
    fn a_spring_starts_still_overshoots_and_settles() {
        assert!(spring(0.3, 0.75, 0.0, 0.0).abs() < 1e-3);
        assert!((spring(0.3, 0.75, 0.0, 0.999) - 1.0).abs() < 0.01);
        assert_eq!(spring(0.3, 0.75, 0.0, 1.0), 1.0);
        // under-damped, it goes past its end on the way
        assert!((0..100).any(|i| spring(0.3, 0.75, 0.0, i as f32 / 100.0) > 1.0));
        // damped enough, it doesn't
        assert!((0..100).all(|i| spring(0.3, 1.2, 0.0, i as f32 / 100.0) <= 1.0));
    }

    #[test]
    fn the_chrome_recedes_only_while_a_stroke_is_painted_and_shows_no_motion_reduced() {
        // at rest, and with a stroke painted, and with motion asked to be kept still
        assert_eq!(chrome_dim_target(false, false), 0.0);
        assert_eq!(chrome_dim_target(false, true), 0.0);
        assert_eq!(chrome_dim_target(true, true), 0.0, "reduce motion keeps the chrome still");
        let painting = chrome_dim_target(true, false);
        assert!((0.05..=0.08).contains(&painting), "a quiet few percent: {painting}");
    }

    #[test]
    fn the_chrome_dim_recedes_quickly_and_breathes_back_to_nothing() {
        let target = CHROME_DIM;
        // a stroke begins: it rises towards the target, never overshooting it
        let mut dim = 0.0;
        for _ in 0..12 {
            let next = settle_dim(dim, target, 1.0 / 60.0);
            assert!(next >= dim && next <= target, "{next} from {dim}");
            dim = next;
        }
        assert!(dim > 0.0 && (dim - target).abs() < 1e-3, "it settles on the target: {dim}");
        // the stroke ends: it comes back, and stops exactly at none
        let mut back = dim;
        for _ in 0..120 {
            back = settle_dim(back, 0.0, 1.0 / 60.0);
        }
        assert_eq!(back, 0.0);
        assert_eq!(settle_dim(0.0, 0.0, 1.0 / 60.0), 0.0, "nothing to draw at rest");
        // quicker to recede than to breathe back, the same frame given to each
        let recede = settle_dim(0.0, target, 1.0 / 60.0);
        let breathe = target - settle_dim(target, 0.0, 1.0 / 60.0);
        assert!(recede > breathe, "{recede} from {breathe}");
        // a frame with no time, or a broken one, stands still
        assert_eq!(settle_dim(0.02, target, 0.0), 0.02);
        assert_eq!(settle_dim(0.02, target, f32::NAN), 0.02);
        assert_eq!(settle_dim(0.02, target, f32::INFINITY), 0.02);
        // a dim that came to nothing but a number lands on its target instead
        assert_eq!(settle_dim(f32::NAN, target, 1.0 / 60.0), target);
    }

    #[test]
    fn the_dim_only_repaints_while_it_is_still_settling() {
        let target = CHROME_DIM;
        assert!(!dim_needs_repaint(0.0, 0.0), "idle: both at rest, no repaint");
        assert!(dim_needs_repaint(0.0, target), "receding towards the dim");
        assert!(dim_needs_repaint(0.02, target), "still settling towards the dim");
        assert!(!dim_needs_repaint(target, target), "settled at the dim: no repaint");
        assert!(dim_needs_repaint(target, 0.0), "breathing back to nothing");
        assert!(dim_needs_repaint(0.04, 0.0), "still returning");
        assert!(!dim_needs_repaint(0.0, 0.0), "settled at nothing: no repaint");
        // once settle_dim lands on the target, the repaint stops
        let landed = settle_dim(target - 5e-4, target, 1.0 / 60.0);
        assert_eq!(landed, target);
        assert!(!dim_needs_repaint(landed, target));
    }

    #[test]
    fn the_dim_steps_once_per_frame_and_not_on_a_discarded_pass() {
        assert!(dim_step_due(0, u64::MAX), "the first frame steps");
        assert!(dim_step_due(7, 6), "a new frame steps");
        assert!(!dim_step_due(7, 7), "a second pass of the same frame does not");
        assert!(!dim_step_due(7, 7), "nor a third pass of it");
        assert!(dim_step_due(8, 7), "the next frame steps again");
    }

    #[test]
    fn a_receding_fill_steps_a_few_percent_darker_and_keeps_its_ink_readable() {
        assert_eq!(receded(argb(pc::BAR), 0.0), argb(pc::BAR), "at rest, untouched");
        assert_eq!(receded(argb(pc::BAR), f32::NAN), argb(pc::BAR), "a broken dim darkens nothing");
        assert_eq!(receded(argb(pc::BAR), -1.0), argb(pc::BAR), "below none, none");
        for (what, fill, ink) in [
            ("bar dark", pc::BAR.0, pc::TEXT.0),
            ("bar light", pc::BAR.1, pc::TEXT.1),
            ("dock dark", crate::panel::DOCK_FILL.0, pc::TEXT.0),
            ("dock light", crate::panel::DOCK_FILL.1, pc::TEXT.1),
            ("dock header dark", crate::panel::DOCK_HEADER_FILL.0, pc::TEXT.0),
            ("dock header light", crate::panel::DOCK_HEADER_FILL.1, pc::TEXT.1),
        ] {
            let dimmed = receded(argb(fill), CHROME_DIM);
            assert_eq!(dimmed.a(), 255, "{what}: the fill stays opaque");
            let before = luminance(fill & 0xFF_FFFF);
            let after = luminance(as_rgb(dimmed));
            assert!(after < before, "{what}: {after} from {before}, the fill steps back");
            // the step is a few percent of the fill's own colour, and no more than a fifth of its
            // measured luminance (the light look's near-white needs only a shade to read as receding)
            assert!((before - after) / before < 0.2, "{what}: {after} from {before}, a quiet step");
            let readable = contrast(as_rgb(dimmed), ink & 0xFF_FFFF);
            assert!(readable >= 4.5, "{what}: the ink on the receded fill measures {readable:.2}");
        }
    }

    /// An opaque colour as 0xRRGGBB.
    fn as_rgb(c: Color32) -> u32 {
        let [r, g, b, _] = c.to_array();
        (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
    }

    #[test]
    fn all_atelier_subsystems_keep_dark_light_separation_and_contrast() {
        for (name, themed) in [
            ("custom_canvas::SCREEN", crate::custom_canvas::SCREEN),
            ("custom_canvas::PANEL", crate::custom_canvas::PANEL),
            ("custom_canvas::EDITING", crate::custom_canvas::EDITING),
            ("colours::VALUE_GROUP", crate::colours::VALUE_GROUP),
            ("colours::HEX_PAD_FILL", crate::colours::HEX_PAD_FILL),
            ("colours::HEX_KEY_FILL", crate::colours::HEX_KEY_FILL),
            ("colours::HEX_KEY_INK", crate::colours::HEX_KEY_INK),
            ("quick_menu::BUTTON_FILL", crate::quick_menu::BUTTON_FILL),
            ("quick_menu::PICKER_FILL", crate::quick_menu::PICKER_FILL),
            ("quick_menu::ROW_FILL", crate::quick_menu::ROW_FILL),
            ("quick_menu::ROW_INK", crate::quick_menu::ROW_INK),
            ("shortcuts::FILL", crate::shortcuts::FILL),
            ("shortcuts::TITLE", crate::shortcuts::TITLE),
            ("shortcuts::INK", crate::shortcuts::INK),
            ("overlays::NOTICE_FILL", crate::overlays::NOTICE_FILL),
            ("overlays::NOTICE_INK", crate::overlays::NOTICE_INK),
        ] {
            assert_ne!(themed.0, themed.1, "{name} duplicates dark and light values");
        }

        let notice_dark = over(crate::overlays::NOTICE_FILL.0, 0xFF_FFFF);
        let notice_light = over(crate::overlays::NOTICE_FILL.1, 0x00_0000);
        let pairs = [
            ("custom_canvas dark editing", crate::custom_canvas::PANEL.0, crate::custom_canvas::EDITING.0),
            ("custom_canvas light editing", crate::custom_canvas::PANEL.1, crate::custom_canvas::EDITING.1),
            ("hex pad dark", crate::colours::HEX_PAD_FILL.0, crate::colours::HEX_KEY_INK.0),
            ("hex pad light", crate::colours::HEX_PAD_FILL.1, crate::colours::HEX_KEY_INK.1),
            ("hex key dark", crate::colours::HEX_KEY_FILL.0, crate::colours::HEX_KEY_INK.0),
            ("hex key light", crate::colours::HEX_KEY_FILL.1, crate::colours::HEX_KEY_INK.1),
            ("quick picker dark", over(crate::quick_menu::PICKER_FILL.0, 0xFF_FFFF), crate::quick_menu::ROW_INK.0),
            ("quick picker light", over(crate::quick_menu::PICKER_FILL.1, 0x00_0000), crate::quick_menu::ROW_INK.1),
            ("quick row dark", crate::quick_menu::ROW_FILL.0, crate::quick_menu::ROW_INK.0),
            ("quick row light", crate::quick_menu::ROW_FILL.1, crate::quick_menu::ROW_INK.1),
            ("shortcuts title dark", crate::shortcuts::FILL.0, crate::shortcuts::TITLE.0),
            ("shortcuts title light", crate::shortcuts::FILL.1, crate::shortcuts::TITLE.1),
            ("shortcuts ink dark", crate::shortcuts::FILL.0, crate::shortcuts::INK.0),
            ("shortcuts ink light", crate::shortcuts::FILL.1, crate::shortcuts::INK.1),
            ("overlays notice dark", notice_dark, crate::overlays::NOTICE_INK.0),
            ("overlays notice light", notice_light, crate::overlays::NOTICE_INK.1),
        ];
        for (what, surface, ink) in pairs {
            let got = contrast(surface, ink);
            assert!(got >= 4.5, "{what}: measured {got:.2}, wanted at least 4.5");
        }
    }
}
