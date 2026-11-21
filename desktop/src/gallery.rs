//! The gallery, as the Android app's (ui/GalleryScreen.kt): every artwork as a thumbnail with its
//! name and size, a stack as its top artwork over two turned cards; over them the app's name (in a
//! stack, the way back and its name), Select, Import, Photo and +, which opens the New canvas list
//! (the window's size, then the presets). Select picks artworks to stack, preview, duplicate or
//! delete. The artworks are the core's store (gallery.rs), kept in the app's folder; the presets
//! are the core's too, kept as the Android app keeps them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, Id, Order, Pos2, Rect, ScrollArea, Sense, Stroke, TextureHandle, Ui, UiBuilder, Vec2, pos2, vec2,
};
use sumi_core::{
    ArtworkStore, CanvasPreset, GalleryArtwork, GalleryEntry, P3, preset_label, preset_width, presets_from_json,
    profile_chip,
};

use crate::animated_export::Animated;
use crate::icons;
use crate::look::{Look, Themed, Weight, argb, text};
use crate::panel;
use crate::timelapse::TimeLapsePlayer;

/// The gallery's ground and its words: the identity's dark ground (#211F22), surface and raised
/// plates, the rose accent for what is chosen, and the paper look derived from them.
pub const BACKGROUND: Themed = Themed(0xFF211F22, 0xFFEFE8DF);
/// A thumbnail's shadow, dropped below it, and the rim round it.
const THUMB_SHADOW: Themed = Themed(0x52000000, 0x1D000000);
const THUMB_RIM: Themed = Themed(0xFF50484C, 0xFFC9BFB2);
const TITLE: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
const TEXT: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
const SIZE: Themed = Themed(0xFFBDB2B2, 0xFF645C54);
const CAPSULE: Themed = Themed(0xFF2C292D, 0xFFF7F2EA);
const CAPSULE_EDGE: Themed = Themed(0xFF50484C, 0xFFD0C5B8);
const HEADER: f32 = 55.0;
/// The New canvas list: hanging from +, wearing the Inkstone/Washi studio surface.
const NEW_WIDTH: f32 = 347.0;
const LIGHT_PANEL: Themed = Themed(0xFF2C292D, 0xFFF7F2EA);
const LIGHT_TEXT: Themed = crate::look::pc::TEXT;
const LIGHT_ROW_STRONG: Themed = Themed(0xFF383337, 0xFFE5DBD0);
const LIGHT_ROW: Themed = Themed(0xFF332E32, 0xFFEDE5DA);
const LIGHT_SEPARATOR: Themed = crate::panel::DOCK_EDGE;
const LIGHT_CHIP: Themed = Themed(0xFF453E43, 0xFFDCD2C4);
/// "New canvas" (21 above it, 20 below), and the presets' most before they scroll.
const NEW_HEADER: f32 = 21.0 + 31.0 + 20.0;
const PRESETS_MOST: f32 = 376.0;
/// A preset row swiped left: how far it slides, and the Edit and Delete it shows.
const PRESET_REVEAL: f32 = 159.0;
const PRESET_BEHIND: Themed = Themed(0xFF262327, 0xFFE2D8CC);
const PRESET_EDIT: Themed = Themed(0xFF453E43, 0xFFDCD2C4);
const PRESET_EDIT_INK: Themed = crate::look::pc::TEXT;
/// Destructive actions stay a single quiet vermilion, the seal's family rather than the rose.
const PRESET_DELETE: Themed = Themed(0xFFC4534B, 0xFFC4534B);
/// Select: the rose button that ends it, a picked artwork's mark, the More list.
const DONE_FILL: Themed = crate::look::pc::ACTIVE;
const PICKED: Themed = crate::look::pc::ACTIVE;
const UNPICKED_RING: Themed = Themed(0xFFBDB2B2, 0xFF9A8C8E);
const POPOVER: Themed = Themed(0xFF2C292D, 0xFFF7F2EA);
const ACTIONS_GROUP: Themed = Themed(0x33000000, 0xFFECE4D8);
const ACTIONS_SEPARATOR: Themed = Themed(0x14FFFFFF, 0xFFE1D8CC);
const ACTIONS_DISABLED: Themed = Themed(0xFF7A6E72, 0xFF9C918F);
const PANEL_TITLE: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
const PREVIEW_CLOSE: u32 = 0xFFE6DED4;
/// Renaming: the gallery faded back, the artwork fitted in a 281 x 280 frame 20 from the top (12
/// in), the name's field 20 under it.
const RENAME_VEIL: u32 = 0xE6211F22;
/// About: the veil over the gallery, the version's ink, the close button.
const ABOUT_VEIL: u32 = 0xD9211F22;
const ABOUT_INK: u32 = 0xFF5A504E;
const ABOUT_CLOSE: u32 = 0xFFE6DED4;
const ABOUT_CLOSE_INK: u32 = 0xFF302C2E;
/// About's button: 520 under the mark's top, 220 x 38.
const ABOUT_BUTTON: (f32, f32) = (220.0, 38.0);
const ABOUT_BUTTON_DOWN: f32 = 520.0;
const ABOUT_BUTTON_FILL: u32 = 0xFF2C292D;
const ABOUT_BUTTON_INK: u32 = 0xFFBDB2B2;
const RENAME_FRAME: (f32, f32) = (281.0, 280.0);
const RENAME_TOP: f32 = 20.0;
const RENAME_MARGIN: f32 = 12.0;
const RENAME_GAP: f32 = 20.0;
const RENAME_FIELD: (f32, f32) = (262.0, 38.0);
const RENAME_FIELD_FILL: u32 = 0x1AF0EAE6;
/// Renaming's picture rising from its cell, and its fading away when done (seconds).
const RENAME_RISE_S: f32 = 0.3;
const RENAME_FADE_S: f32 = 0.15;
/// A swiped artwork's actions: three pills over its thumbnail, which fades back.
const SWIPE_PILL: (f32, f32) = (94.0, 56.0);
const SWIPE_PILL_FILL: Themed = Themed(0xDB211F22, 0xFFF7F2EA);
const SWIPE_PILL_INK: Themed = Themed(0xFFBDB2B2, 0xFF645C54);
const SWIPE_DELETE_FILL: u32 = 0xFFC4534B;
const SWIPED_FADE: f32 = 0.64;
/// A swiped artwork's actions growing in or going (seconds), on a spring.
const SWIPE_S: f32 = 0.3;
/// Replay is an artwork action: it belongs inside the thumbnail, not in the empty grid gutter.
const TIME_LAPSE_PILL: (f32, f32) = (106.0, 30.0);
const TIME_LAPSE_FILL: Themed = Themed(0xFF383337, 0xFFE3DACE);
const TIME_LAPSE_EDGE: Themed = Themed(0xFF8A7C80, 0xFFA89A9C);
const TIME_LAPSE_INK: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
const TIME_LAPSE_CLOCK: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
/// The studio's compact editorial Start deck: brand, paper choices and two-column sizes.
pub const START_DECK_MIN_WIDTH: f32 = 1040.0;
pub const START_DECK_WIDTH: f32 = 420.0;
/// When the window is shorter, the standard gallery is preferable to a clipped Start deck.
pub const START_DECK_MIN_HEIGHT: f32 = 560.0;
/// The deck is the ground, its hero the one paper plane (a shade of paper in the light look too).
const START_DECK_FILL: Themed = Themed(0xFF211F22, 0xFFE9E1D6);
const START_HERO_FILL: Themed = Themed(0xFFE6DED4, 0xFFF6F1E9);
const START_HERO_WORD: u32 = 0xFF302C2E;
const START_HERO_TAG: u32 = 0xFF615757;
const START_ROW_HOVER: Themed = Themed(0xFF383337, 0xFFE3DACE);
const START_SHAPE_EDGE: Themed = Themed(0xFFBDB2B2, 0xFF9A8C8E);
const START_NOTE_INK: Themed = Themed(0xFFBDB2B2, 0xFF645C54);
const START_BUTTON_FILL: Themed = Themed(0xFF383337, 0xFFDDD3C6);
const START_BUTTON_HOVER: Themed = Themed(0xFF453E43, 0xFFD3C8BA);
const START_BUTTON_EDGE: Themed = Themed(0xFF50484C, 0xFFC2B6A8);
const START_CARD_FILL: Themed = Themed(0xFF2C292D, 0xFFF4ECE1);
const START_CARD_EDGE: Themed = Themed(0xFF51494C, 0xFFD0C1B0);
const START_ACCENT: Themed = crate::look::pc::ACTIVE;
const START_HERO_HEIGHT_MAX: f32 = 176.0;
/// A width or height being typed on the Start deck: which, the text, and whether it has been
/// focused (the whole value is selected on that first frame).
#[derive(Clone)]
struct StartEdit {
    width: bool,
    typed: String,
    focused: bool,
    /// What the field held before it was opened: Escape puts it back.
    before: u32,
    /// The text is a whole number the core can take: Create waits until it is.
    valid: bool,
}

fn start_hero(screen: Rect) -> Rect {
    Rect::from_min_size(
        pos2(screen.left() + 20.0, screen.top() + 20.0),
        vec2(START_DECK_WIDTH - 40.0, (screen.height() * 0.24).clamp(126.0, START_HERO_HEIGHT_MAX)),
    )
}

fn start_section_y(screen: Rect) -> f32 {
    start_hero(screen).bottom() + 24.0
}

fn start_preset_rect(screen: Rect, index: usize) -> Rect {
    let left = screen.left() + 20.0;
    let top = start_section_y(screen) + 34.0;
    let card_h = ((screen.bottom() - top - 104.0) / 3.0).clamp(56.0, 76.0);
    let gap = 8.0;
    let card_w = (START_DECK_WIDTH - 40.0 - gap) / 2.0;
    Rect::from_min_size(
        pos2(left + (index % 2) as f32 * (card_w + gap), top + (index / 2) as f32 * (card_h + gap)),
        vec2(card_w, card_h),
    )
}

/// The Start deck's size fields sit immediately below the preset cards.
fn start_size_layout(screen: Rect) -> (Rect, Rect, Rect) {
    let deck = Rect::from_min_max(screen.min, pos2(screen.left() + START_DECK_WIDTH, screen.bottom()));
    let pad = 20.0;
    let spin_y = start_preset_rect(screen, START_SIZES.len() - 1).bottom() + 16.0;
    let width = Rect::from_min_size(pos2(deck.left() + pad, spin_y), vec2(62.0, 30.0));
    let height = Rect::from_min_size(pos2(width.left() + 135.0, spin_y), vec2(62.0, 30.0));
    let create = Rect::from_min_max(pos2(height.right() + 63.0, spin_y), pos2(deck.right() - pad, spin_y + 30.0));
    (width, height, create)
}

/// The preset the Start deck's Create makes: pixels, as the old fields did.
fn start_preset(width: u32, height: u32) -> CanvasPreset {
    CanvasPreset {
        name: "Custom".to_owned(),
        unit: "px".to_owned(),
        size_width: width as f32,
        size_height: height as f32,
        dpi: 300.0,
        background: 0xFF_FFFF,
        background_hidden: false,
        time_lapse_resolution: 0,
        time_lapse_quality: 1,
        time_lapse_hevc: false,
        profile: sumi_core::SRGB.to_owned(),
    }
}

/// A typed Start deck dimension, kept to what the core lets a canvas be (1 up to its edge most).
fn start_clamped(value: u32) -> u32 {
    preset_width(start_preset(value, value))
}

/// A typed Start deck dimension, only when it is a whole number the core can take.
fn start_typed(text: &str) -> Option<u32> {
    let value = text.trim().parse::<u64>().ok()?;
    let value = u32::try_from(value).ok()?;
    (1..=start_clamped(u32::MAX)).contains(&value).then_some(value)
}

fn commit_start_edit(edit: StartEdit, field: &mut u32) {
    match edit.typed.trim().parse::<u64>() {
        // A number outside the range is visibly clamped to the edge.
        Ok(value) => *field = start_clamped(u32::try_from(value).unwrap_or(u32::MAX)),
        // Text that is not a number goes back to what the field held.
        Err(_) => *field = edit.before,
    }
}

/// The width or height slot of a Start deck's two sizes. The constant indices are always in
/// bounds, so selecting a field cannot panic.
fn size_field(sizes: &mut [u32; 2], width: bool) -> &mut u32 {
    if width { &mut sizes[0] } else { &mut sizes[1] }
}

const START_SIZES: [(&str, u32, u32); 6] = [
    ("Screen", 1920, 1080),
    ("Square", 2048, 2048),
    ("A4 at 300 dpi", 2480, 3508),
    ("A3 at 300 dpi", 3508, 4961),
    ("B4 manga page", 3035, 4299),
    ("Sketch", 1600, 1200),
];
/// Quick paper colour swatches offered on the Start deck header row.
const START_PAPERS: [(&str, u32); 4] =
    [("Pure white", 0xFF_FFFF), ("Warm washi", 0xF5_EFE6), ("Value grey", 0xB8_B4B0), ("Toned kraft", 0xD8_C6AE)];

/// Places the 4 paper swatches right-aligned on the Start deck's "New drawing" header row.
fn start_paper_swatches(screen: Rect, y: f32) -> [(Rect, &'static str, u32); 4] {
    let deck_right = screen.left() + START_DECK_WIDTH;
    let right_center = deck_right - 20.0 - 4.0 - 8.0;
    let cy = y + 9.0;
    let at = |i: usize, (name, rgb): (&'static str, u32)| {
        let cx = right_center - (START_PAPERS.len() - 1 - i) as f32 * 22.0;
        let rect = Rect::from_center_size(pos2(cx, cy), vec2(16.0, 16.0));
        (rect, name, rgb)
    };
    let [p0, p1, p2, p3] = START_PAPERS;
    [at(0, p0), at(1, p1), at(2, p2), at(3, p3)]
}

/// A paper swatch's rim: the accent while it is the chosen paper, else the shape's edge. Hover
/// leaves it alone, so the chosen paper keeps its ring while the pointer passes the others.
fn paper_swatch_rim(selected: bool) -> Stroke {
    if selected { Stroke::new(2.0, argb(crate::look::pc::ACTIVE)) } else { Stroke::new(1.0, argb(START_SHAPE_EDGE)) }
}

/// The plate a paper swatch shows under the pointer, behind its paper: the deck's own row hover,
/// as its preset rows take it, and none with no pointer over it.
fn paper_swatch_plate(hovered: bool) -> Option<Color32> {
    hovered.then(|| argb(START_ROW_HOVER))
}

/// How long after a time-lapse is found being packaged the reader looks again.
const PACKAGING_RETRY: Duration = Duration::from_millis(250);

fn swipe_ease(t: f32) -> f32 {
    crate::look::spring(SWIPE_S, 0.75, 0.0, t)
}
/// Held this long, an artwork (or a stack) lifts to be dragged; while it is, its cell stays behind
/// faint, and what it would be stacked onto is washed active.
const HOLD_S: f64 = 0.4;
const DRAGGED_FADE: f32 = 0.7;
const DROP_WASH: Themed = Themed(0x80D4A8B0, 0x66C99AA3);
const PREVIEW_ARROW: u32 = 0xFFBDB2B2;

/// One of Select's actions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectAction {
    Stack,
    Preview,
    Share,
    Duplicate,
    Delete,
    More,
}

/// What a tap in the gallery asked for.
pub enum Asked {
    /// The artwork in this folder, to open.
    Open(String),
    /// A new artwork as the preset says, in this folder (the gallery's or a stack's).
    New(CanvasPreset, Option<String>),
    /// A new artwork the screen's size, in this folder.
    ScreenSize(Option<String>),
    /// A new artwork of a plugin's kind (`<id>.<key>`), the screen's size, in this folder.
    PluginKind(String, Option<String>),
    Import(Option<String>),
    Photo(Option<String>),
    /// These artworks written as this format, where the user picks.
    Share(Vec<GalleryArtwork>, crate::files::Format),
    /// These artworks written as PDFs keeping their pictures at this JPEG quality.
    SharePdf(Vec<GalleryArtwork>, u8),
    /// The artwork opened to be shared as this animation, the gallery back after.
    ShareAnimated(GalleryArtwork, Animated),
    /// The artwork lifted and dragged out of the window: handed to the system as a file.
    DragOut(GalleryArtwork),
}

/// Share's formats (`size`, arrow left out) beside a swiped artwork's Share `pill`, pointing at it:
/// on the side with more room (the other if that one is too narrow), kept 2 inside `screen`.
/// Where it goes (arrow in), its arrow's tip from its top, and whether it is right of the pill.
fn formats_beside(pill: Rect, screen: Rect, size: egui::Vec2) -> (Rect, f32, bool) {
    let (width, height) = (size.x, size.y);
    let top = (pill.center().y - height / 2.0)
        .clamp(screen.top() + 2.0, (screen.bottom() - 2.0 - height).max(screen.top() + 2.0));
    let ends = panel::POPOVER_CORNER + panel::ARROW_HEIGHT;
    let arrow_y = (pill.center().y - top).clamp(ends, height - ends);
    let total = width + panel::ARROW_HEIGHT;
    let room_right = screen.right() - 2.0 - pill.right() - 2.0;
    let room_left = pill.left() - 2.0 - (screen.left() + 2.0);
    let right = if room_right >= room_left {
        room_right >= total || room_left < total
    } else {
        room_left < total && room_right >= total
    };
    let left = if right { pill.right() + 2.0 } else { (pill.left() - 2.0 - total).max(screen.left() + 2.0) };
    (Rect::from_min_size(pos2(left, top), vec2(total, height)), arrow_y, right)
}

/// A row of Share's formats: a still format, or an animated one (the artwork opened to make it).
#[derive(Clone, Copy)]
enum ShareRow {
    Still(crate::files::Format),
    Animated(Animated),
}

impl ShareRow {
    fn name(self) -> &'static str {
        match self {
            ShareRow::Still(format) => format.name(),
            ShareRow::Animated(format) => format.title(),
        }
    }

    /// The line under this row saying what the format cannot hold of the artworks: only the two
    /// native documents are checked, as they are the ones that cannot hold everything.
    fn note(self, notes: &ShareNotes) -> Option<String> {
        match self {
            ShareRow::Still(crate::files::Format::Clip) => Some(notes.clip.line()),
            ShareRow::Still(crate::files::Format::Kra) => Some(notes.kra.line()),
            _ => None,
        }
    }

    /// Whether tapping the row can share in this format: a native document the pre-flight refused
    /// cannot be written, so its row becomes a no-op while its note still says why.
    fn actionable(self, notes: &ShareNotes) -> bool {
        match self {
            ShareRow::Still(crate::files::Format::Clip) => !matches!(notes.clip, crate::files::ClipChanges::Refused(_)),
            ShareRow::Still(crate::files::Format::Kra) => !matches!(notes.kra, crate::files::ClipChanges::Refused(_)),
            _ => true,
        }
    }
}

/// What each native document would keep differently of the artworks Share's formats are open for.
#[derive(Clone)]
struct ShareNotes {
    clip: crate::files::ClipChanges,
    kra: crate::files::ClipChanges,
}

/// What writing `list` as Krita's format (`kra`), or CLIP STUDIO PAINT's, would keep differently:
/// the first refusal if one artwork refuses, every line the copies would carry otherwise, each
/// named when there is more than one.
///
/// Each artwork's document is read from its `artwork.json` alone: no layer's pixels are opened and
/// no GPU is asked for, so Share's formats can say this as they open.
fn share_changes(list: &[GalleryArtwork], kra: bool) -> crate::files::ClipChanges {
    let many = list.len() > 1;
    let mut lines = Vec::new();
    for artwork in list {
        let named = |line: String| if many { format!("{}: {line}", artwork.name) } else { line };
        match sumi_core::artwork_clip_preflight(artwork.dir.clone(), kra) {
            Ok(changes) => lines.extend(changes.into_iter().map(|change| named(change))),
            Err(sumi_core::CanvasError::File { reason }) => {
                return crate::files::ClipChanges::Refused(named(reason));
            }
        }
    }
    crate::files::ClipChanges::Kept(lines)
}

/// Share's formats, as their list shows them, the animated ones last.
const SHARE_ROWS: [ShareRow; 13] = [
    ShareRow::Still(crate::files::Format::Sumi),
    ShareRow::Still(crate::files::Format::Package),
    ShareRow::Still(crate::files::Format::Psd),
    ShareRow::Still(crate::files::Format::Clip),
    ShareRow::Still(crate::files::Format::Kra),
    ShareRow::Still(crate::files::Format::Pdf),
    ShareRow::Still(crate::files::Format::Jpeg),
    ShareRow::Still(crate::files::Format::Png),
    ShareRow::Still(crate::files::Format::Tiff),
    ShareRow::Animated(Animated::Gif),
    ShareRow::Animated(Animated::Png),
    ShareRow::Animated(Animated::Mp4),
    ShareRow::Animated(Animated::Hevc),
];
const FORMATS_WIDTH: f32 = 260.0;
/// A row of Share's formats, and the line under the two native documents that says what they
/// cannot hold.
const FORMAT_ROW: f32 = 40.0;
const FORMAT_NOTE: f32 = 20.0;

/// What Share's formats list holds: a row for each format, the line under the two native
/// documents, and a line between each two rows.
fn share_group_height() -> f32 {
    let n = SHARE_ROWS.len() as f32;
    FORMAT_ROW * n + FORMAT_NOTE * 2.0 + (n - 1.0)
}

/// Share's formats panel: its header, the list and its bottom, so nothing in it is cut off.
fn share_panel_height() -> f32 {
    55.0 + share_group_height() + 10.0
}

/// Drops cached thumbnails of artworks no longer in `entries`, so a deleted artwork does not keep
/// its texture for the life of the app.
fn retain_live_thumbs<T>(thumbs: &mut HashMap<String, T>, entries: &[GalleryEntry]) {
    let live: HashSet<&str> = entries
        .iter()
        .flat_map(|entry| match entry {
            GalleryEntry::Artwork { artwork } => vec![artwork.dir.as_str()],
            GalleryEntry::Stack { artworks, .. } => artworks.iter().map(|a| a.dir.as_str()).collect(),
        })
        .collect();
    thumbs.retain(|dir, _| live.contains(dir.as_str()));
}

pub struct Gallery {
    pub store: Arc<ArtworkStore>,
    entries: Vec<GalleryEntry>,
    /// The stack open: its folder and name.
    stack: Option<(String, String)>,
    /// Each artwork's thumbnail (by folder), and when the artwork was changed when it was read.
    thumbs: HashMap<String, (i64, Option<TextureHandle>)>,
    /// Whether each artwork has a time-lapse recorded (by folder), when it was changed and the
    /// refresh it was read for.
    lapses: HashMap<String, (i64, u64, bool)>,
    /// Time-lapses being looked for on the reader's thread.
    lapse_reading: std::collections::HashSet<(String, i64)>,
    /// Artworks whose time-lapse cannot be read yet (being packaged): when to look again.
    lapse_waiting: HashMap<String, (i64, Instant)>,
    /// Bumped when the gallery is read again: answers asked for before are then no longer wanted.
    lapse_generation: u64,
    /// The artwork's Time-lapse pill a press went down on, until the pointer is let go.
    pill_press: Option<String>,
    /// The artwork's time-lapse being replayed over the gallery, once one is picked.
    player: Option<TimeLapsePlayer>,
    /// Thumbnails being read on a thread of their own (a gallery of many would hold up the
    /// window), and that thread.
    reading: std::collections::HashSet<(String, i64)>,
    reader: Option<ThumbnailReader>,
    /// Artworks being duplicated on threads of their own (a big one's files take a while): the
    /// gallery is read again as each batch is done.
    duplicating: Vec<std::sync::mpsc::Receiver<()>>,
    new_open: bool,
    presets: Vec<CanvasPreset>,
    /// The kinds of artwork the plugins on make: the New canvas list's last rows.
    pub kinds: Vec<sumi_core::PluginKind>,
    /// The screen's size, pixels: Screen Size's canvas.
    pub screen_px: (u32, u32),
    /// Select: on, the artworks picked (by folder), More's list open, the picked artworks shown
    /// one at a time (and which), and the alert asking before they are deleted.
    selecting: bool,
    picked: Vec<String>,
    more_open: bool,
    preview: Option<(Vec<GalleryArtwork>, usize, bool, f32)>,
    /// The animation of the artwork shown in Preview, by its folder.
    played: Option<Played>,
    /// An artwork two fingers are spreading open into Preview.
    spreading: Option<Spread>,
    alert: crate::alert::Alert,
    /// Share's PDF asks how well it keeps its pictures first (for these artworks).
    pdf_alert: crate::alert::Alert,
    pdf_for: Option<Vec<GalleryArtwork>>,
    /// Non-fatal import/export change notes shown inside the gallery window.
    note_alert: crate::alert::Alert,
    /// An artwork lifted and dragged out past the window's edge.
    dragged_out: Option<GalleryArtwork>,
    /// The artwork being renamed, its name as typed, and whether its field has had the focus.
    renaming: Option<(GalleryArtwork, String, bool)>,
    /// The stack being renamed instead (its folder), shown as its top artwork under its name.
    renaming_stack: Option<String>,
    /// When the renaming began (its picture rising from its cell), and one just done, fading
    /// away (its name as kept, since when).
    rename_since: Option<Instant>,
    rename_fading: Option<(GalleryArtwork, String, Instant)>,
    /// The artwork (by folder) whose actions a swipe left shows, and the one asked about deleting.
    swiped: Option<String>,
    deleting: Option<String>,
    /// How far the drag on a cell has gone.
    dragged: egui::Vec2,
    /// Share's formats, open for these artworks, beside a swiped artwork's Share (or under
    /// Select's, where it last was).
    sharing: Option<(Vec<GalleryArtwork>, Option<Rect>)>,
    /// What sharing those artworks as a `.clip` or a `.kra` would keep differently, read when
    /// Share's formats opened: the folders and times it was read for, and the two lines.
    share_checks: Option<(Vec<(String, i64)>, ShareNotes)>,
    share_button: Rect,
    /// What a hold lifted (by folder) and where it was taken (window points), and where the way
    /// back out of a stack is, to drop it there.
    lifted: Option<(String, Pos2)>,
    back: Rect,
    /// Custom Canvas, open on a new canvas or a preset.
    custom: Option<crate::custom_canvas::CustomCanvas>,
    /// The preset row swiped open, and the one being dragged (and how far it is).
    preset_open: Option<usize>,
    preset_drag: Option<(usize, f32)>,
    /// What the app is, over the gallery, and its mark's picture once read.
    about: bool,
    mark: Option<TextureHandle>,
    /// The Sumi enso brush-circle mark on the Start page hero card.
    enso_mark: Option<TextureHandle>,
    /// Quick custom width, height and paper colour on the Start page's "New drawing" deck, and
    /// the field being typed into there.
    start_size: (u32, u32),
    start_paper: u32,
    start_edit: Option<StartEdit>,
    /// Where each artwork's picture (by folder) was last drawn: what the gallery zooms into and
    /// out of as an artwork opens and closes.
    pictures: HashMap<String, Rect>,
    /// Pictures shrinking under a press, growing as they lift, or going back (by folder).
    lifts: HashMap<String, Lift>,
    /// A stack opening, its artworks flying out of its cell (where it was, since when); or left,
    /// its artworks flying back into it over the gallery coming back.
    opening_stack: Option<(Rect, Instant)>,
    leaving_stack: Option<LeavingStack>,
    /// Where each stack's cell was last drawn (by folder), and the cells drawn last frame: what
    /// flies back into a stack as it is left.
    stack_cells: HashMap<String, Rect>,
    drawn_cells: Vec<(GalleryEntry, Rect)>,
}

/// A stack just left: its folder and name, its artworks and where each was, and since when.
struct LeavingStack {
    dir: String,
    name: String,
    cells: Vec<(GalleryEntry, Rect)>,
    since: Instant,
}

/// A stack's artworks flying out of its cell as it opens, and back into it (as the gallery grows
/// back from 0.8 of its size, fading in) as it is left (seconds, on springs); leaving, its name
/// fading out and the gallery's in.
const STACK_OPEN_S: f32 = 0.45;

/// Two fingers spread on an artwork: let go spread this much or more, it opens in Preview; it
/// shrinks no further than the least; let go, it settles in this long, seconds.
const SPREAD_OPEN: f32 = 1.35;
const SPREAD_LEAST: f32 = 0.6;
const SPREAD_SETTLE_S: f32 = 0.3;

/// An artwork two fingers are spreading open: its picture where it showed, how far the fingers
/// have spread and moved it, and once they let go, since when, from where, and whether it opens.
struct Spread {
    artwork: GalleryArtwork,
    from: Rect,
    scale: f32,
    moved: Vec2,
    let_go: Option<(Instant, Rect, bool)>,
}

impl Spread {
    /// Where the fingers have it: grown about its middle and moved with them.
    fn following(&self) -> Rect {
        Rect::from_center_size(self.from.center() + self.moved, self.from.size() * self.scale)
    }
}
/// Select's marks fading in and out (seconds).
const SELECT_FADE_S: f32 = 0.15;
const STACK_LEAVE_S: f32 = 0.3;
const STACK_GROW_FROM: f32 = 0.8;
const NAME_OUT_S: f32 = 0.07;
const TITLE_IN_S: f32 = 0.11;

/// A picture's size changing: from and to (times its size), since when, and how long (seconds).
struct Lift {
    from: f32,
    to: f32,
    since: Instant,
    secs: f32,
}

impl Lift {
    fn now(&self) -> f32 {
        let t = if self.secs > 0.0 { self.since.elapsed().as_secs_f32() / self.secs } else { 1.0 };
        self.from + (self.to - self.from) * crate::look::cubic_bezier(0.42, 0.0, 0.58, 1.0, t)
    }

    fn moving(&self) -> bool {
        self.since.elapsed().as_secs_f32() < self.secs
    }
}

/// Pressed, a picture waits a moment (so a scroll starting on it leaves it be), then shrinks;
/// held, it grows as it lifts; let go, it goes back (times its size, seconds).
const PRESS_DELAY_S: f64 = 0.1;
const PRESSED: (f32, f32) = (0.87, 0.15);
const LIFTED: (f32, f32) = (1.07, 0.25);
const DROPPED: (f32, f32) = (1.0, 0.25);

impl Gallery {
    pub fn new() -> Gallery {
        let store = ArtworkStore::open(crate::files::app_dir().join("artworks").to_string_lossy().into_owned());
        let presets = presets_from_json(std::fs::read_to_string(presets_path()).ok());
        let mut gallery = Gallery::with_store(store, presets);
        gallery.refresh();
        gallery
    }

    fn with_store(store: Arc<ArtworkStore>, presets: Vec<CanvasPreset>) -> Gallery {
        Gallery {
            store,
            entries: Vec::new(),
            stack: None,
            thumbs: HashMap::new(),
            lapses: HashMap::new(),
            lapse_reading: std::collections::HashSet::new(),
            lapse_waiting: HashMap::new(),
            lapse_generation: 0,
            pill_press: None,
            player: None,
            reading: Default::default(),
            reader: None,
            duplicating: Vec::new(),
            new_open: false,
            presets,
            kinds: Vec::new(),
            screen_px: (1920, 1080),
            selecting: false,
            picked: Vec::new(),
            more_open: false,
            preview: None,
            played: None,
            spreading: None,
            alert: crate::alert::Alert::default(),
            pdf_alert: crate::alert::Alert::default(),
            pdf_for: None,
            note_alert: crate::alert::Alert::default(),
            dragged_out: None,
            renaming: None,
            renaming_stack: None,
            rename_since: None,
            rename_fading: None,
            swiped: None,
            deleting: None,
            dragged: egui::Vec2::ZERO,
            sharing: None,
            share_checks: None,
            share_button: Rect::NOTHING,
            lifted: None,
            back: Rect::NOTHING,
            custom: None,
            preset_open: None,
            preset_drag: None,
            about: false,
            mark: None,
            enso_mark: None,
            start_size: (3000, 2000),
            start_paper: 0xFF_FFFF,
            start_edit: None,
            pictures: HashMap::new(),
            lifts: HashMap::new(),
            opening_stack: None,
            leaving_stack: None,
            stack_cells: HashMap::new(),
            drawn_cells: Vec::new(),
        }
    }

    /// Reads what is in the gallery (or the stack open) again.
    pub fn refresh(&mut self) {
        self.entries = self.store.list(self.stack.as_ref().map(|s| s.0.clone()));
        // the stack open is gone (its last artwork taken out): back to the gallery
        if self.stack.is_some() && self.entries.is_empty() {
            self.stack = None;
            self.entries = self.store.list(None);
        }
        retain_live_thumbs(&mut self.thumbs, &self.entries);
        // the time-lapses may have been recorded, purged or changed while an artwork was open:
        // what was asked for before is no longer wanted
        self.lapse_generation = self.lapse_generation.wrapping_add(1);
        self.lapses.clear();
        self.lapse_reading.clear();
        self.lapse_waiting.clear();
    }

    /// Whether this artwork has a time-lapse recorded, as last read off the window's thread.
    fn recorded(&self, a: &GalleryArtwork) -> bool {
        self.lapses.get(&a.dir).is_some_and(|(when, generation, recorded)| {
            *when == a.modified_ms && *generation == self.lapse_generation && *recorded
        })
    }

    /// Asks the reader's thread which artworks, as last changed, have a time-lapse; a look asked
    /// for asks for another frame, so an idle gallery takes the answer without input.
    fn ensure_time_lapses(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut stale: Vec<(String, i64)> = Vec::new();
        let mut soon: Option<Duration> = None;
        for entry in &self.entries {
            let GalleryEntry::Artwork { artwork } = entry else { continue };
            let when = artwork.modified_ms;
            if self
                .lapses
                .get(&artwork.dir)
                .is_some_and(|(at, generation, _)| *at == when && *generation == self.lapse_generation)
            {
                continue;
            }
            if self.lapse_reading.contains(&(artwork.dir.clone(), when)) {
                continue;
            }
            // being packaged: looked at again a little later, not read half done
            if let Some((waiting_when, at)) = self.lapse_waiting.get(&artwork.dir)
                && *waiting_when == when
                && now < *at
            {
                let left = *at - now;
                soon = Some(soon.map_or(left, |soon| soon.min(left)));
                continue;
            }
            stale.push((artwork.dir.clone(), when));
        }
        for (dir, _) in &stale {
            self.lapse_waiting.remove(dir);
        }
        if let Some(left) = soon {
            ctx.request_repaint_after(left.max(Duration::from_millis(16)));
        }
        if stale.is_empty() {
            return;
        }
        let generation = self.lapse_generation;
        let reader = self.reader.get_or_insert_with(ThumbnailReader::start);
        for (dir, when) in stale {
            self.lapse_reading.insert((dir.clone(), when));
            if reader.asks.send(ReaderAsk::TimeLapse(dir, when, generation)).is_err() {
                self.reader = None;
                self.lapse_reading.clear();
                break;
            }
        }
        // the answers come off the reader's thread: look again for them
        ctx.request_repaint_after(Duration::from_millis(30));
    }

    /// Takes in the time-lapses found since the last frame; answers asked for before the gallery
    /// was read again are no longer wanted.
    fn time_lapses_read(&mut self, ctx: &egui::Context) {
        let mut found = Vec::new();
        let mut stopped = false;
        if let Some(reader) = &self.reader {
            loop {
                match reader.recorded.try_recv() {
                    Ok(answer) => found.push(answer),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        stopped = true;
                        break;
                    }
                }
            }
        }
        let mut retry = None;
        for (dir, when, generation, recorded) in found {
            if generation != self.lapse_generation {
                continue;
            }
            self.lapse_reading.remove(&(dir.clone(), when));
            match recorded {
                crate::timelapse::Recorded::Packaging => {
                    self.lapse_waiting.insert(dir, (when, Instant::now() + PACKAGING_RETRY));
                    retry = Some(PACKAGING_RETRY);
                }
                crate::timelapse::Recorded::Yes => {
                    self.lapse_waiting.remove(&dir);
                    self.lapses.insert(dir, (when, generation, true));
                }
                crate::timelapse::Recorded::No => {
                    self.lapse_waiting.remove(&dir);
                    self.lapses.insert(dir, (when, generation, false));
                }
            }
        }
        if stopped {
            self.reader = None;
            self.lapse_reading.clear();
        }
        if let Some(retry) = retry {
            ctx.request_repaint_after(retry);
        } else if !self.lapse_reading.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    /// Anchor the replay action in the artwork's lower right corner, with room from the edge.
    fn time_lapse_pill(cell: Rect) -> Rect {
        let (width, height) = TIME_LAPSE_PILL;
        let thumb = Self::thumb_box(cell);
        let inset = (thumb.width().min(thumb.height()) * 0.15).clamp(4.0, 9.0);
        let width = width.min((thumb.width() - inset * 2.0).max(20.0));
        let height = height.min((thumb.height() - inset * 2.0).max(16.0));
        Rect::from_min_size(pos2(thumb.right() - width - inset, thumb.bottom() - height - inset), vec2(width, height))
    }

    /// Draws the Time-lapse pill, lit on hover and pressed, and ringed while it has the focus.
    fn paint_time_lapse_pill(ui: &Ui, look: &mut Look, rect: Rect, focused: bool, hovered: bool, down: bool) {
        let shadow = egui::epaint::Shadow { offset: [0, 2], blur: 5, spread: 0, color: Color32::from_black_alpha(72) };
        ui.painter().add(shadow.as_shape(rect, rect.height() / 2.0));
        let fill = if down {
            argb(TIME_LAPSE_FILL).gamma_multiply(0.9)
        } else if hovered {
            argb(TIME_LAPSE_FILL).gamma_multiply(1.12)
        } else {
            argb(TIME_LAPSE_FILL)
        };
        ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
        let edge = if hovered { argb(TIME_LAPSE_CLOCK) } else { argb(TIME_LAPSE_EDGE) };
        ui.painter().rect_stroke(rect, rect.height() / 2.0, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        if rect.width() < 94.0 {
            // Tiny grid cells have space for the clock only; tooltip carries the full name.
            look.icon(ui, &icons::CLOCK, rect.center(), 14.0, argb(TIME_LAPSE_CLOCK));
        } else {
            look.icon(ui, &icons::CLOCK, pos2(rect.left() + 10.0, rect.center().y), 11.0, argb(TIME_LAPSE_CLOCK));
            text(
                ui.painter(),
                pos2(rect.left() + 24.0, rect.center().y),
                Align2::LEFT_CENTER,
                "Replay",
                11.0,
                Weight::SemiBold,
                argb(TIME_LAPSE_INK),
            );
        }
        if focused {
            ui.painter().rect_stroke(
                rect.expand(1.5),
                rect.height() / 2.0 + 1.5,
                Stroke::new(2.0, argb(crate::look::pc::ACCENT)),
                egui::StrokeKind::Inside,
            );
        }
    }

    fn keep_presets(&self) {
        if let Err(e) =
            crate::files::write_whole(&presets_path(), sumi_core::presets_json(self.presets.clone()).as_bytes())
        {
            log::warn!("keeping the presets: {e}");
        }
    }

    /// The folder new artworks go in: the stack open's, or the gallery's.
    pub fn folder(&self) -> Option<String> {
        self.stack.as_ref().map(|s| s.0.clone())
    }

    /// How big the picture of `dir` is now (times its size), going to `to` in `secs` when that
    /// is new.
    fn lift(&mut self, ctx: &egui::Context, dir: &str, (to, secs): (f32, f32)) -> f32 {
        let lift =
            self.lifts.entry(dir.to_owned()).or_insert(Lift { from: 1.0, to: 1.0, since: Instant::now(), secs: 0.0 });
        if lift.to != to {
            *lift = Lift { from: lift.now(), to, since: Instant::now(), secs };
        }
        let now = lift.now();
        if lift.moving() {
            ctx.request_repaint();
        } else if to == 1.0 {
            self.lifts.remove(dir);
        }
        now
    }

    /// Where the artwork in folder `dir` was last drawn, if it was.
    pub fn picture_of(&self, dir: &str) -> Option<Rect> {
        self.pictures.get(dir).copied()
    }

    /// The gallery's own page (its header and grid), as the window's background.
    pub fn layer() -> egui::LayerId {
        egui::LayerId::new(Order::Background, Id::unique("gallery"))
    }

    /// The gallery's page over the canvas opening, moved by `transform` and shown `alpha`; nothing
    /// in it is touched.
    pub fn zooming(&mut self, ctx: &egui::Context, look: &mut Look, transform: egui::emath::TSTransform, alpha: f32) {
        ctx.set_transform_layer(egui::LayerId::new(Order::Foreground, Id::unique("gallery")), transform);
        self.page(ctx, look, Order::Foreground, alpha, true);
    }

    /// Where the artwork grid sits in `screen`: to the right of the Sumi Start deck on wide windows
    /// when not inside a stack, or across the whole window on compact windows.
    pub fn grid_rect(screen: Rect, in_stack: bool) -> Rect {
        let left = if !in_stack && screen.width() >= START_DECK_MIN_WIDTH && screen.height() >= START_DECK_MIN_HEIGHT {
            screen.left() + START_DECK_WIDTH
        } else {
            screen.left()
        };
        Rect::from_min_max(pos2(left, screen.top() + HEADER), screen.max)
    }

    /// The Sumi Start deck on the left of a wide gallery window: the paper hero card with the
    /// enso brush circle, "sumi" wordmark and subtitle, followed by "New drawing" quick presets,
    /// custom pixel dimensions, and "Open a file...".
    fn start_deck(&mut self, ctx: &egui::Context, ui: &mut Ui, screen: Rect) -> Option<Asked> {
        if self.enso_mark.is_none() {
            let bytes = include_bytes!("start_mark.png");
            let image = crate::files::png_rgba(bytes)
                .map(|(w, h, rgba)| egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba));
            self.enso_mark = image.map(|i| ctx.load_texture("sumi enso mark", i, egui::TextureOptions::LINEAR));
        }
        let deck = Rect::from_min_max(screen.min, pos2(screen.left() + START_DECK_WIDTH, screen.bottom()));
        ui.painter().rect_filled(deck, 0.0, argb(START_DECK_FILL));
        let edge_x = deck.right() - 0.5;
        ui.painter().line_segment(
            [pos2(edge_x, deck.top()), pos2(edge_x, deck.bottom())],
            Stroke::new(1.0, argb(crate::panel::DOCK_EDGE)),
        );

        let pad = 20.0;
        let inner_w = START_DECK_WIDTH - pad * 2.0;
        let hero = start_hero(screen);
        // Uncoated-paper cover rather than a generic raised dashboard card.
        ui.painter().rect_filled(hero, 3.0, argb(START_HERO_FILL));
        ui.painter().rect_filled(Rect::from_min_size(hero.min, vec2(4.0, hero.height())), 0.0, argb(START_ACCENT));
        if let Some(t) = &self.enso_mark {
            let side = (hero.height() - 28.0).min(132.0);
            let mark_rect =
                Rect::from_center_size(pos2(hero.left() + 22.0 + side / 2.0, hero.center().y), vec2(side, side));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(t.id(), mark_rect, uv, Color32::WHITE);
        }
        let word_at = pos2(hero.left() + 166.0, hero.center().y - 34.0);
        let word_rect =
            text(ui.painter(), word_at, Align2::LEFT_TOP, "sumi", 43.0, Weight::Medium, argb(START_HERO_WORD));
        let seal = Rect::from_min_size(pos2(word_rect.right() + 10.0, word_rect.top() + 5.0), vec2(17.0, 17.0));
        ui.painter().rect_filled(seal, 3.0, argb(crate::look::pc::SEAL_FILL));
        let seal_center = seal.center();
        let seal_radius = 5.2;
        let seal_start = -1.2_f32;
        let seal_sweep = std::f32::consts::TAU - 0.7;
        let enso: Vec<Pos2> = (0..=20)
            .map(|i| {
                let a = seal_start + seal_sweep * (i as f32 / 20.0);
                seal_center + vec2(a.cos(), a.sin()) * seal_radius
            })
            .collect();
        ui.painter().add(egui::Shape::line(enso, Stroke::new(1.5, argb(crate::look::pc::PAPER))));
        text(
            ui.painter(),
            pos2(hero.left() + 168.0, word_rect.bottom() + 3.0),
            Align2::LEFT_TOP,
            "Pencil, ink & colour",
            13.0,
            Weight::Regular,
            argb(START_HERO_TAG),
        );
        if ui
            .interact(hero, ui.scope_id().with("sumi hero about"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.about = true;
        }

        let mut asked = None;
        let y = start_section_y(screen);
        text(
            ui.painter(),
            pos2(deck.left() + pad + 4.0, y),
            Align2::LEFT_TOP,
            "New canvas",
            16.0,
            Weight::SemiBold,
            argb(TITLE),
        );
        for (swatch, name, rgb) in start_paper_swatches(screen, y) {
            let colour = Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
            let resp = ui
                .interact(swatch.expand(2.0), ui.scope_id().with(("start paper", name)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(format!("Paper: {name}"));
            if let Some(plate) = paper_swatch_plate(resp.hovered()) {
                ui.painter().circle_filled(swatch.center(), swatch.width() / 2.0 + 2.5, plate);
            }
            ui.painter().circle_filled(swatch.center(), swatch.width() / 2.0, colour);
            let selected = self.start_paper == rgb;
            ui.painter().circle_stroke(swatch.center(), swatch.width() / 2.0, paper_swatch_rim(selected));
            if resp.clicked() {
                self.start_paper = rgb;
            }
        }
        let paper_tint =
            Color32::from_rgb((self.start_paper >> 16) as u8, (self.start_paper >> 8) as u8, self.start_paper as u8);
        let (sw, sh) = self.screen_px;
        for (i, &(label, w, h)) in START_SIZES.iter().enumerate() {
            let (pw, ph) = if i == 0 { (sw.max(1), sh.max(1)) } else { (w, h) };
            let row = start_preset_rect(screen, i);
            let resp = ui
                .interact(row, ui.scope_id().with(("start preset", i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let active = resp.hovered() || resp.is_pointer_button_down_on();
            ui.painter().rect_filled(row, 6.0, argb(if active { START_ROW_HOVER } else { START_CARD_FILL }));
            ui.painter().rect_stroke(
                row,
                6.0,
                Stroke::new(1.0, if active { argb(START_ACCENT) } else { argb(START_CARD_EDGE) }),
                egui::StrokeKind::Inside,
            );
            let scale = 30.0 / (pw.max(ph) as f32);
            let shape_size = vec2((pw as f32 * scale).max(8.0), (ph as f32 * scale).max(8.0));
            let holder = Rect::from_center_size(pos2(row.left() + 30.0, row.center().y), vec2(34.0, 32.0));
            let shape = Rect::from_center_size(holder.center(), shape_size);
            ui.painter().rect_filled(shape, 1.5, paper_tint);
            ui.painter().rect_stroke(shape, 1.5, Stroke::new(1.0, argb(START_SHAPE_EDGE)), egui::StrokeKind::Inside);
            text(
                ui.painter(),
                pos2(holder.right() + 10.0, row.center().y - 9.0),
                Align2::LEFT_CENTER,
                label,
                12.5,
                Weight::SemiBold,
                argb(TITLE),
            );
            let dims = format!("{pw} \u{d7} {ph} px");
            text(
                ui.painter(),
                pos2(holder.right() + 10.0, row.center().y + 9.0),
                Align2::LEFT_CENTER,
                &dims,
                10.5,
                Weight::Regular,
                argb(START_NOTE_INK),
            );
            if resp.clicked() {
                if i == 0 {
                    asked = Some(Asked::ScreenSize(self.folder()));
                } else {
                    let preset = CanvasPreset {
                        name: label.to_owned(),
                        unit: "px".to_owned(),
                        size_width: pw as f32,
                        size_height: ph as f32,
                        dpi: 300.0,
                        background: self.start_paper,
                        background_hidden: false,
                        time_lapse_resolution: 0,
                        time_lapse_quality: 1,
                        time_lapse_hevc: false,
                        profile: sumi_core::SRGB.to_owned(),
                    };
                    asked = Some(Asked::New(preset, self.folder()));
                }
            }
        }

        let (width_box, height_box, create_btn) = start_size_layout(screen);
        let spin_y = width_box.top();
        let mut sizes = [self.start_size.0, self.start_size.1];
        let mut edit = self.start_edit.take();
        // A click on Create while the text before this frame was invalid must not create; the
        // field may lose focus to the click in this same frame.
        let invalid_before = edit.as_ref().is_some_and(|e| !e.valid);
        for is_w in [true, false] {
            let val_box = if is_w { width_box } else { height_box };
            let editing = edit.as_ref().is_some_and(|e| e.width == is_w);
            let invalid = edit.as_ref().is_some_and(|e| e.width == is_w && !e.valid);
            ui.painter().rect_filled(val_box, 5.0, argb(CAPSULE));
            let edge = if invalid {
                Color32::from_rgb(224, 96, 96)
            } else if editing {
                argb(crate::look::pc::ACCENT)
            } else {
                argb(START_BUTTON_EDGE)
            };
            ui.painter().rect_stroke(
                val_box,
                5.0,
                Stroke::new(if editing { 1.5 } else { 1.0 }, edge),
                egui::StrokeKind::Inside,
            );
            if editing {
                let Some(mut e) = edit.take() else { continue };
                let field = val_box.shrink2(vec2(5.0, 3.0));
                let mut output = egui::TextEdit::singleline(&mut e.typed)
                    .id(ui.scope_id().with(("start size text", is_w)))
                    .font(Weight::Medium.font(13.0))
                    .text_color(argb(TITLE))
                    .frame(egui::Frame::NONE)
                    .margin(Vec2::ZERO)
                    .align(Align2::CENTER_CENTER)
                    .desired_width(field.width())
                    .char_limit(5)
                    .show(&mut ui.new_child(UiBuilder::new().max_rect(field)));
                if output.response.changed() {
                    match start_typed(&e.typed) {
                        Some(value) => {
                            *size_field(&mut sizes, is_w) = value;
                            e.valid = true;
                        }
                        None => e.valid = false,
                    }
                }
                if !e.focused {
                    output.response.request_focus();
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(e.typed.chars().count()),
                    );
                    output.state.cursor.set_char_range(Some(all));
                    output.state.store(ui.ctx(), output.response.id);
                    e.focused = true;
                }
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if escape || (output.response.lost_focus() && !e.valid && !enter) {
                    // Leaving an invalid field cancels, as Escape does; only Enter takes the
                    // clamped number.
                    *size_field(&mut sizes, is_w) = e.before;
                } else if output.response.lost_focus() || enter {
                    commit_start_edit(e, size_field(&mut sizes, is_w));
                } else {
                    edit = Some(e);
                }
            } else {
                text(
                    ui.painter(),
                    val_box.center(),
                    Align2::CENTER_CENTER,
                    &format!("{}", *size_field(&mut sizes, is_w)),
                    13.0,
                    Weight::Medium,
                    argb(TITLE),
                );
                let field_resp = ui
                    .interact(val_box, ui.scope_id().with(("start custom field", is_w)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::Text);
                if field_resp.clicked() {
                    if let Some(previous) = edit.take() {
                        let previous_is_w = previous.width;
                        commit_start_edit(previous, size_field(&mut sizes, previous_is_w));
                    }
                    let current = *size_field(&mut sizes, is_w);
                    edit = Some(StartEdit {
                        width: is_w,
                        typed: current.to_string(),
                        focused: false,
                        before: current,
                        valid: true,
                    });
                }
            }
            let mut x = val_box.right() + 3.0;
            for (step_label, delta) in [("\u{2212}", -100i64), ("+", 100i64)] {
                let btn = Rect::from_min_size(pos2(x, spin_y), vec2(26.0, 30.0));
                let r = ui
                    .interact(btn, ui.scope_id().with(("start step", is_w, step_label)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let fill = if r.hovered() { argb(START_BUTTON_HOVER) } else { argb(START_BUTTON_FILL) };
                ui.painter().rect_filled(btn, 5.0, fill);
                ui.painter().rect_stroke(btn, 5.0, Stroke::new(1.0, argb(START_BUTTON_EDGE)), egui::StrokeKind::Inside);
                text(
                    ui.painter(),
                    btn.center(),
                    Align2::CENTER_CENTER,
                    step_label,
                    15.0,
                    Weight::SemiBold,
                    argb(TITLE),
                );
                if r.clicked() {
                    if let Some(previous) = edit.take() {
                        let previous_is_w = previous.width;
                        commit_start_edit(previous, size_field(&mut sizes, previous_is_w));
                    }
                    let next =
                        start_clamped((*size_field(&mut sizes, is_w) as i64 + delta).clamp(1, u32::MAX as i64) as u32);
                    *size_field(&mut sizes, is_w) = next;
                }
                x += 28.0;
            }
            if is_w {
                text(
                    ui.painter(),
                    pos2(x + 6.0, spin_y + 15.0),
                    Align2::CENTER_CENTER,
                    "\u{d7}",
                    13.0,
                    Weight::Regular,
                    argb(START_NOTE_INK),
                );
            }
        }
        self.start_size = (sizes[0], sizes[1]);
        self.start_edit = edit;
        let can_create = !invalid_before && self.start_edit.as_ref().is_none_or(|e| e.valid);
        let create_resp = ui.interact(
            create_btn,
            ui.scope_id().with("start create custom"),
            if can_create { Sense::click() } else { Sense::hover() },
        );
        if can_create {
            create_resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        let create_fill = if !can_create {
            argb(START_BUTTON_FILL).gamma_multiply(0.55)
        } else if create_resp.hovered() {
            argb(START_BUTTON_HOVER)
        } else {
            argb(START_BUTTON_FILL)
        };
        ui.painter().rect_filled(create_btn, 5.0, create_fill);
        ui.painter().rect_stroke(create_btn, 5.0, Stroke::new(1.0, argb(START_BUTTON_EDGE)), egui::StrokeKind::Inside);
        text(
            ui.painter(),
            create_btn.center(),
            Align2::CENTER_CENTER,
            "Create",
            13.0,
            Weight::SemiBold,
            if can_create { argb(TITLE) } else { argb(START_NOTE_INK) },
        );
        if can_create && create_resp.clicked() {
            if let Some(previous) = self.start_edit.take() {
                let previous_is_w = previous.width;
                commit_start_edit(previous, size_field(&mut sizes, previous_is_w));
            }
            let (w, h) = (start_clamped(sizes[0]), start_clamped(sizes[1]));
            self.start_size = (w, h);
            let mut preset = start_preset(w, h);
            preset.background = self.start_paper;
            asked = Some(Asked::New(preset, self.folder()));
        }

        // File import is the other way to begin, directly after the custom-size action.
        // Anchoring it to the window bottom made it look unrelated to the canvas controls.
        let open_y = (spin_y + 44.0).min(deck.bottom() - 48.0);
        let open_btn = Rect::from_min_size(pos2(deck.left() + pad, open_y), vec2(inner_w, 32.0));
        let open_resp = ui
            .interact(open_btn, ui.scope_id().with("start open file"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let open_fill = if open_resp.hovered() { argb(START_BUTTON_HOVER) } else { argb(START_BUTTON_FILL) };
        ui.painter().rect_filled(open_btn, 6.0, open_fill);
        ui.painter().rect_stroke(open_btn, 6.0, Stroke::new(1.0, argb(START_BUTTON_EDGE)), egui::StrokeKind::Inside);
        text(
            ui.painter(),
            open_btn.center(),
            Align2::CENTER_CENTER,
            "Open an existing artwork",
            13.5,
            Weight::Medium,
            argb(TITLE),
        );
        if open_resp.clicked() {
            asked = Some(Asked::Import(self.folder()));
        }
        asked
    }

    /// The gallery's page (its header and grid) over the whole window, `order` deep, shown
    /// `alpha`; `covered`, nothing in it is touched. What a tap in it asked for.
    fn page(&mut self, ctx: &egui::Context, look: &mut Look, order: Order, alpha: f32, covered: bool) -> Option<Asked> {
        let screen = ctx.content_rect();
        // shown at once, not faded in as a new area would be
        egui::Area::new(Id::unique("gallery"))
            .fixed_pos(screen.min)
            .order(order)
            .fade_in(false)
            .show(ctx, |ui| {
                ui.multiply_opacity(alpha);
                ui.allocate_rect(screen, Sense::hover());
                ui.painter().rect_filled(screen, 0, argb(BACKGROUND));
                let in_stack = self.stack.is_some();
                let deck_asked = if !in_stack
                    && screen.width() >= START_DECK_MIN_WIDTH
                    && screen.height() >= START_DECK_MIN_HEIGHT
                {
                    self.start_deck(ctx, ui, screen)
                } else {
                    None
                };
                let grid = Self::grid_rect(screen, in_stack);
                let right_header =
                    Rect::from_min_max(pos2(grid.left(), screen.top()), pos2(screen.right(), screen.bottom()));
                let tapped = self.header(ui, look, right_header).or(deck_asked);
                let asked = if self.entries.is_empty() {
                    let at = grid.center();
                    text(
                        ui.painter(),
                        at,
                        Align2::CENTER_CENTER,
                        "Drawings you open or save show up here.",
                        16.0,
                        Weight::Regular,
                        argb(START_NOTE_INK),
                    );
                    tapped
                } else {
                    // a stack just left: the gallery grows back into place under its artworks
                    match self.leaving_stack.as_ref().map(|l| l.since.elapsed().as_secs_f32() / STACK_LEAVE_S) {
                        Some(t) if t < 1.0 => {
                            let e = crate::look::spring(STACK_LEAVE_S, 0.8, 0.0, t);
                            let s = STACK_GROW_FROM + (1.0 - STACK_GROW_FROM) * e;
                            let c = grid.center().to_vec2();
                            let grow = egui::emath::TSTransform::new(c * (1.0 - s), s);
                            ui.with_visual_transform(grow, |ui| {
                                ui.multiply_opacity(e.clamp(0.0, 1.0));
                                self.grid(ui, grid, look)
                            })
                            .inner
                            .or(tapped)
                        }
                        _ => self.grid(ui, grid, look).or(tapped),
                    }
                };
                if covered {
                    // over everything in it: taken before any of it sees a press
                    ui.allocate_rect(screen, Sense::click_and_drag());
                    return None;
                }
                asked
            })
            .inner
    }

    /// Shows the gallery over the whole window; returns what a tap asked for.
    pub fn show(&mut self, ctx: &egui::Context, look: &mut Look, colours: &crate::colours::Colours) -> Option<Asked> {
        self.thumbnails_read(ctx);
        self.time_lapses_read(ctx);
        self.ensure_time_lapses(ctx);
        self.duplicated(ctx);
        // a time-lapse replaying covers the gallery: nothing under it acts
        if let Some(player) = self.player.as_mut() {
            if player.show(ctx, look) {
                self.player = None;
            }
            return None;
        }
        let screen = ctx.content_rect();
        let mut asked = self.page(ctx, look, Order::Background, 1.0, false);
        self.stack_flights(ctx, look);
        if self.new_open && self.custom.is_none() {
            asked = self.new_canvas(ctx, look, screen).or(asked);
        }
        if let Some(custom) = self.custom.as_mut() {
            use crate::custom_canvas::Done;
            match custom.show(ctx, look, &colours.palettes, colours.harmony) {
                Some(Done::Cancel) => self.custom = None,
                // an edited preset is kept in its place; a new one goes at the end of the list
                // and is made into an artwork
                Some(Done::Made(p)) => {
                    let editing = custom.editing;
                    self.custom = None;
                    match editing.and_then(|i| self.presets.get_mut(i)) {
                        Some(kept) => *kept = p,
                        None => {
                            self.presets.push(p.clone());
                            self.new_open = false;
                            asked = Some(Asked::New(p, self.folder()));
                        }
                    }
                    self.keep_presets();
                }
                None => {}
            }
        }
        if self.more_open {
            self.more(ctx, look, screen);
        }
        if self.about {
            self.about(ctx, screen);
        }
        if self.spreading.is_some() || ctx.input(|i| i.multi_touch().is_some()) {
            self.spread(ctx, screen);
        }
        if self.preview.is_some() {
            self.preview(ctx, look, screen);
        }
        if self.renaming.is_some() {
            self.rename(ctx, screen);
        }
        self.rename_fade(ctx, screen);
        if self.sharing.is_some() {
            asked = self.formats(ctx, look, screen).or(asked);
        }
        match self.alert.show(ctx, look) {
            Some(crate::alert::Answer::Action) => {
                if let Some(dir) = self.deleting.take() {
                    self.store.delete(dir);
                    self.refresh();
                } else {
                    for dir in std::mem::take(&mut self.picked) {
                        self.store.delete(dir);
                    }
                    self.end_selecting();
                }
            }
            Some(_) => self.deleting = None,
            None => {}
        }
        if let Some(answer) = self.pdf_alert.show(ctx, look)
            && let Some(list) = self.pdf_for.take()
            && let Some(quality) = crate::alert::pdf_jpeg(answer)
        {
            asked = Some(Asked::SharePdf(list, quality));
        }
        if !self.note_alert.is_open()
            && !self.alert.is_open()
            && !self.pdf_alert.is_open()
            && let Some((title, text)) = crate::files::take_next_note()
        {
            self.note_alert.tell(&title, &text, None);
        }
        _ = self.note_alert.show(ctx, look);
        if let Some(artwork) = self.dragged_out.take() {
            asked = Some(Asked::DragOut(artwork));
        }
        asked
    }

    /// The artworks picked, in the gallery's order.
    fn picked_artworks(&self) -> Vec<GalleryArtwork> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                GalleryEntry::Artwork { artwork } if self.picked.contains(&artwork.dir) => Some(artwork.clone()),
                _ => None,
            })
            .collect()
    }

    fn end_selecting(&mut self) {
        self.selecting = false;
        self.picked.clear();
        self.more_open = false;
        self.refresh();
    }

    /// Whether `action` can act on what is picked now.
    fn can(&self, action: SelectAction) -> bool {
        match action {
            SelectAction::Stack => self.stack.is_none() && self.picked.len() >= 2,
            SelectAction::More => true,
            _ => !self.picked.is_empty(),
        }
    }

    fn act(&mut self, action: SelectAction) {
        if !self.can(action) {
            return;
        }
        match action {
            SelectAction::Stack => {
                let dirs = self.picked_artworks().into_iter().map(|a| a.dir).collect();
                self.store.stack(dirs);
                self.end_selecting();
            }
            SelectAction::Preview => {
                self.more_open = false;
                self.preview = Some((self.picked_artworks(), 0, false, 0.0));
            }
            SelectAction::Duplicate => {
                self.duplicate(self.picked_artworks().into_iter().map(|a| a.dir).collect());
                self.end_selecting();
            }
            SelectAction::Delete => {
                let n = self.picked.len();
                let title = if n == 1 { "Delete 1 artwork?".to_owned() } else { format!("Delete {n} artworks?") };
                self.alert.open(&title, "This can't be undone.", "Delete", Some(&icons::TRASH));
            }
            SelectAction::More => self.more_open = !self.more_open,
            SelectAction::Share => self.sharing = Some((self.picked_artworks(), None)),
        }
    }

    /// Select's title row: the title, then (on a wide window) Stack, Preview, Share, Duplicate and
    /// Delete, or Share, Delete and More, each dimmed until it can act; then the active round button
    /// that ends selecting.
    fn select_header(&mut self, ui: &Ui, look: &mut Look, screen: Rect) {
        let painter = ui.painter();
        let top = screen.top();
        let title = self.stack.as_ref().map_or("Sumi", |s| s.1.as_str()).to_owned();
        text(
            painter,
            pos2(screen.left() + 32.0, top + 11.0),
            Align2::LEFT_TOP,
            &title,
            34.0,
            Weight::Medium,
            argb(TITLE),
        );
        let wide = screen.width() > screen.height();
        let actions: &[(SelectAction, &str, f32)] = if wide {
            &[
                (SelectAction::Stack, "Stack", 65.0),
                (SelectAction::Preview, "Preview", 80.0),
                (SelectAction::Share, "Share", 65.0),
                (SelectAction::Duplicate, "Duplicate", 91.0),
                (SelectAction::Delete, "Delete", 70.0),
            ]
        } else {
            &[
                (SelectAction::Share, "Share", 65.0),
                (SelectAction::Delete, "Delete", 70.0),
                (SelectAction::More, "More", 61.0),
            ]
        };
        let done = Rect::from_min_size(pos2(screen.right() - 20.0 - 44.0, top + 10.0), vec2(44.0, 45.0));
        let width: f32 = actions.iter().map(|a| a.2).sum();
        let capsule = Rect::from_min_size(pos2(done.left() - 13.0 - width, top + 10.0), vec2(width, 45.0));
        painter.rect_filled(capsule, 22.5, argb(CAPSULE));
        painter.rect_stroke(capsule, 22.5, Stroke::new(1.0, argb(CAPSULE_EDGE)), egui::StrokeKind::Inside);
        let mut x = capsule.left();
        let mut chosen = None;
        for &(action, label, w) in actions {
            let button = Rect::from_min_size(pos2(x, capsule.top()), vec2(w, 45.0));
            let ink = if self.can(action) { argb(TITLE) } else { argb(SIZE) };
            text(painter, button.center(), Align2::CENTER_CENTER, label, 16.0, Weight::Regular, ink);
            if action == SelectAction::Share {
                self.share_button = button;
            }
            if ui.interact(button, ui.scope_id().with(("select", label)), Sense::click()).clicked() {
                chosen = Some(action);
            }
            x += w;
        }
        painter.rect_filled(done, 22.0, argb(DONE_FILL));
        look.icon(ui, &icons::DONE_TICK, done.center(), 40.0, argb(crate::look::pc::ACCENT_INK));
        if ui.interact(done, ui.scope_id().with("select done"), Sense::click()).clicked() {
            self.end_selecting();
        }
        if let Some(action) = chosen {
            self.act(action);
        }
    }

    /// More options for the picked artworks, under More: Preview, Stack (two or more), Duplicate.
    fn more(&mut self, ctx: &egui::Context, look: &mut Look, screen: Rect) {
        let width = 250.0;
        let more_x = screen.right() - 20.0 - 44.0 - 13.0 - 61.0 / 2.0;
        let left = (more_x - width / 2.0).clamp(screen.left() + 4.0, screen.right() - 4.0 - width);
        let height = panel::ARROW_HEIGHT + 14.0 + 29.0 + 12.0 + 44.0 * 3.0 + 2.0 + 12.0;
        let rect = Rect::from_min_size(pos2(left, screen.top() + 57.0), vec2(width, height));
        let mut chosen = None;
        let mut close = false;
        egui::Area::new(Id::unique("more options")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            close = ui.allocate_rect(screen, Sense::click()).clicked();
            ui.interact(rect, ui.scope_id().with("panel"), Sense::click_and_drag());
            let body = panel::solid_frame(
                ui,
                look,
                "more options",
                rect,
                more_x - left,
                false,
                panel::POPOVER_CORNER,
                POPOVER,
            );
            let ink = argb(PANEL_TITLE);
            text(
                ui.painter(),
                pos2(body.left() + 24.0, body.top() + 14.0),
                Align2::LEFT_TOP,
                "More Options",
                22.0,
                Weight::SemiBold,
                ink,
            );
            let group = Rect::from_min_size(
                pos2(body.left() + 10.0, body.top() + 14.0 + 29.0 + 12.0),
                vec2(width - 20.0, 44.0 * 3.0 + 2.0),
            );
            ui.painter().rect_filled(group, 18.0, argb(ACTIONS_GROUP));
            for (i, (action, label)) in [
                (SelectAction::Preview, "Preview"),
                (SelectAction::Stack, "Stack"),
                (SelectAction::Duplicate, "Duplicate"),
            ]
            .into_iter()
            .enumerate()
            {
                let row =
                    Rect::from_min_size(pos2(group.left(), group.top() + 45.0 * i as f32), vec2(group.width(), 44.0));
                if i > 0 {
                    let line = Rect::from_min_size(pos2(row.left(), row.top() - 1.0), vec2(row.width(), 1.0));
                    ui.painter().rect_filled(line, 0, argb(ACTIONS_SEPARATOR));
                }
                let on = self.can(action);
                let colour = if on { ink } else { argb(ACTIONS_DISABLED) };
                text(
                    ui.painter(),
                    pos2(row.left() + 16.0, row.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    17.0,
                    Weight::Regular,
                    colour,
                );
                if on && ui.interact(row, ui.scope_id().with(("more", i)), Sense::click()).clicked() {
                    chosen = Some(action);
                }
            }
        });
        if close {
            self.more_open = false;
        }
        if let Some(action) = chosen {
            self.more_open = false;
            self.act(action);
        }
    }

    /// Renaming an artwork: it shows at the top middle, larger, over the gallery faded back, with
    /// its name in a field under it, all of it chosen. Enter, Escape or a tap away keeps the name
    /// (an empty one keeps the old).
    /// The artwork done renaming fading away (0.15 s), as it was left.
    fn rename_fade(&mut self, ctx: &egui::Context, screen: Rect) {
        let Some((a, name, since)) = self.rename_fading.clone() else { return };
        let t = since.elapsed().as_secs_f32() / RENAME_FADE_S;
        if t >= 1.0 {
            self.rename_fading = None;
            return;
        }
        ctx.request_repaint();
        let alpha = 1.0 - crate::look::cubic_bezier(0.42, 0.0, 0.58, 1.0, t);
        egui::Area::new(Id::unique("renamed artwork")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            ui.multiply_opacity(alpha);
            let (picture, field) = Self::rename_places(&a, screen);
            ui.painter().rect_filled(screen, 0, argb(RENAME_VEIL));
            self.thumbnail(ui, picture, &a, 0.0);
            ui.painter().rect_filled(field, 10.0, argb(RENAME_FIELD_FILL));
            text(ui.painter(), field.center(), Align2::CENTER_CENTER, &name, 17.0, Weight::Regular, Color32::WHITE);
        });
    }

    /// Where renaming shows an artwork's picture (fitted in its frame, centred across the top) and
    /// the field under it.
    fn rename_places(a: &GalleryArtwork, screen: Rect) -> (Rect, Rect) {
        let (fw, fh) = (RENAME_FRAME.0 - RENAME_MARGIN * 2.0, RENAME_FRAME.1 - RENAME_MARGIN * 2.0);
        let ratio = if a.width > 0 && a.height > 0 { a.width as f32 / a.height as f32 } else { 0.75 };
        let (aw, ah) = if ratio > fw / fh { (fw, fw / ratio) } else { (fh * ratio, fh) };
        let top = screen.top() + RENAME_TOP + (RENAME_FRAME.1 - ah) / 2.0;
        let picture = Rect::from_min_size(pos2(screen.center().x - aw / 2.0, top), vec2(aw, ah));
        let field_top = screen.top() + RENAME_TOP + RENAME_FRAME.1 + RENAME_GAP;
        let field = Rect::from_min_size(
            pos2(screen.center().x - RENAME_FIELD.0 / 2.0, field_top),
            vec2(RENAME_FIELD.0, RENAME_FIELD.1),
        );
        (picture, field)
    }

    fn rename(&mut self, ctx: &egui::Context, screen: Rect) {
        let Some((a, mut name, mut focused)) = self.renaming.take() else { return };
        let mut done = false;
        // the picture rises from its cell to the top (0.3 s), the field showing as it comes
        let since = *self.rename_since.get_or_insert_with(Instant::now);
        let t = since.elapsed().as_secs_f32() / RENAME_RISE_S;
        let rise = crate::look::cubic_bezier(0.42, 0.0, 0.58, 1.0, t);
        if t < 1.0 {
            ctx.request_repaint();
        }
        egui::Area::new(Id::unique("rename artwork")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            done = ui.allocate_rect(screen, Sense::click()).clicked();
            ui.painter().rect_filled(screen, 0, argb(RENAME_VEIL));
            let (picture, field) = Self::rename_places(&a, screen);
            let from = self.picture_of(&a.dir).unwrap_or(picture);
            let at = Rect::from_min_max(from.min.lerp(picture.min, rise), from.max.lerp(picture.max, rise));
            self.thumbnail(ui, at, &a, 0.0);
            ui.painter().rect_filled(field, 10.0, argb(RENAME_FIELD_FILL).gamma_multiply(rise));
            let inner = field.shrink2(vec2(12.0, 0.0));
            let mut output = egui::TextEdit::singleline(&mut name)
                .font(Weight::Regular.font(17.0))
                .text_color(Color32::WHITE)
                .frame(egui::Frame::NONE)
                .margin(vec2(0.0, 0.0))
                .align(Align2::CENTER_CENTER)
                .desired_width(inner.width())
                .char_limit(256)
                .show(&mut {
                    let mut child = ui.new_child(
                        UiBuilder::new().max_rect(Rect::from_center_size(inner.center(), vec2(inner.width(), 22.0))),
                    );
                    child.multiply_opacity(rise);
                    child
                });
            if !focused {
                output.response.request_focus();
                let all = egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(name.chars().count()),
                );
                output.state.cursor.set_char_range(Some(all));
                output.state.store(ui.ctx(), output.response.id);
                focused = true;
            } else if output.response.lost_focus() {
                done = true;
            }
        });
        if done {
            let kept = if name.trim().is_empty() { a.name.clone() } else { name.trim().to_owned() };
            self.rename_since = None;
            self.rename_fading = Some((a.clone(), kept.clone(), Instant::now()));
            match self.renaming_stack.take() {
                Some(stack) => self.store.rename_stack(stack, kept),
                None => self.store.rename(a.dir, kept),
            }
            self.refresh();
        } else {
            self.renaming = Some((a, name, focused));
        }
    }

    /// What the app is, over the gallery under a near-black veil: its mark 300 across in its
    /// field, its name and its version; the round button at the top right (or Escape) closes it.
    fn about(&mut self, ctx: &egui::Context, screen: Rect) {
        if self.mark.is_none() {
            let bytes = include_bytes!("start_mark.png");
            let image = crate::files::png_rgba(bytes)
                .map(|(w, h, rgba)| egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba));
            self.mark = image.map(|i| ctx.load_texture("app mark", i, egui::TextureOptions::LINEAR));
        }
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        egui::Area::new(Id::unique("about")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::click_and_drag());
            ui.painter().rect_filled(screen, 0, argb(ABOUT_VEIL));
            let field = Rect::from_min_size(pos2(screen.center().x - 150.0, screen.top() + 95.0), vec2(300.0, 300.0));
            ui.painter().rect_filled(field, 67.0, argb(crate::look::pc::PAPER));
            if let Some(t) = &self.mark {
                let inner = field.shrink(28.0);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(t.id(), inner, uv, Color32::WHITE);
            }
            let name = text(
                ui.painter(),
                pos2(field.center().x, field.bottom() + 12.0),
                Align2::CENTER_TOP,
                "Sumi",
                64.0,
                Weight::Medium,
                Color32::WHITE,
            );
            let version = concat!("Version ", env!("SUMI_VERSION"));
            text(
                ui.painter(),
                pos2(field.center().x, name.bottom() + 14.0),
                Align2::CENTER_TOP,
                version,
                13.0,
                Weight::Regular,
                argb(ABOUT_INK),
            );
            let at = pos2(field.center().x - ABOUT_BUTTON.0 / 2.0, field.top() + ABOUT_BUTTON_DOWN);
            let recovery = Rect::from_min_size(at, vec2(ABOUT_BUTTON.0, ABOUT_BUTTON.1));
            ui.painter().rect_filled(recovery, 8.0, argb(ABOUT_BUTTON_FILL));
            let ink = argb(ABOUT_BUTTON_INK);
            text(
                ui.painter(),
                recovery.center(),
                Align2::CENTER_CENTER,
                "Start Gallery Recovery",
                15.0,
                Weight::Regular,
                ink,
            );
            // nothing is looked through while a file is still being written
            let tapped = ui.interact(recovery, ui.scope_id().with("about recovery"), Sense::click()).clicked();
            if tapped && !crate::files::still_writing() {
                self.store.recover_lost_artworks();
                self.refresh();
                close = true;
            }
            let button = Rect::from_min_size(pos2(screen.right() - 31.5 - 22.5, screen.top() + 31.0), vec2(22.5, 22.5));
            ui.painter().circle_filled(button.center(), 11.25, argb(ABOUT_CLOSE));
            let (c, d) = (button.center(), 4.0);
            let stroke = Stroke::new(1.8, argb(ABOUT_CLOSE_INK));
            ui.painter().line_segment([c - vec2(d, d), c + vec2(d, d)], stroke);
            ui.painter().line_segment([c + vec2(d, -d), c + vec2(-d, d)], stroke);
            close |= ui.interact(button.expand(10.0), ui.scope_id().with("about close"), Sense::click()).clicked();
        });
        if close {
            self.about = false;
        }
    }

    /// The picked artworks one at a time over the whole window, on black: a drag across goes to
    /// the next or the last, a tap shows (or hides) the close button and the arrows.
    /// Two fingers spread on an artwork (a touch screen's): its picture follows them out of its
    /// place as the gallery darkens; let go, it settles fitted over black and opens in Preview
    /// among the gallery's artworks or, spread too little, goes back into its place (as the
    /// tablet's).
    fn spread(&mut self, ctx: &egui::Context, screen: Rect) {
        let touch = ctx.input(|i| i.multi_touch());
        if self.spreading.is_none() {
            let Some(t) = touch.filter(|t| t.num_touches == 2) else { return };
            if self.selecting || self.preview.is_some() || self.lifted.is_some() {
                return;
            }
            let found = self.entries.iter().find_map(|e| match e {
                GalleryEntry::Artwork { artwork } => {
                    self.pictures.get(&artwork.dir).filter(|r| r.contains(t.start_pos)).map(|r| (artwork.clone(), *r))
                }
                _ => None,
            });
            let Some((artwork, from)) = found else { return };
            self.spreading = Some(Spread { artwork, from, scale: 1.0, moved: Vec2::ZERO, let_go: None });
        }
        let Some(s) = self.spreading.as_mut() else { return };
        match (touch, s.let_go) {
            (Some(t), None) => {
                s.scale = (s.scale * t.zoom_delta).max(SPREAD_LEAST);
                s.moved += t.translation_delta;
            }
            (None, None) => s.let_go = Some((Instant::now(), s.following(), s.scale >= SPREAD_OPEN)),
            _ => {}
        }
        // fitted to the screen, as Preview shows it
        let fitted = Self::fitted(screen, &s.artwork);
        let full = fitted.translate(vec2(0.0, screen.center().y - fitted.center().y));
        let (rect, settled) = match s.let_go {
            None => (s.following(), None),
            Some((since, held, opens)) => {
                let t = (since.elapsed().as_secs_f32() / SPREAD_SETTLE_S).min(1.0);
                let e = crate::look::cubic_bezier(0.0, 0.0, 0.58, 1.0, t);
                let to = if opens { full } else { s.from };
                (Rect::from_min_max(held.min.lerp(to.min, e), held.max.lerp(to.max, e)), (t >= 1.0).then_some(opens))
            }
        };
        let from = s.from;
        let artwork = s.artwork.clone();
        // how near the whole screen it is: the gallery darkens as far
        let near = ((rect.width() - from.width()) / (full.width() - from.width()).max(1.0)).clamp(0.0, 1.0);
        let texture = self.texture(&artwork);
        egui::Area::new(Id::unique("gallery spread")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0, Color32::BLACK.gamma_multiply(near));
            if let Some(t) = texture {
                egui::Image::new(egui::load::SizedTexture::new(t, rect.size()))
                    .corner_radius(8.0 * (1.0 - near))
                    .paint_at(ui, rect);
            }
        });
        ctx.request_repaint();
        if let Some(opens) = settled {
            self.spreading = None;
            if opens {
                let all: Vec<GalleryArtwork> = self
                    .entries
                    .iter()
                    .filter_map(|e| match e {
                        GalleryEntry::Artwork { artwork } => Some(artwork.clone()),
                        _ => None,
                    })
                    .collect();
                let index = all.iter().position(|a| a.dir == artwork.dir).unwrap_or(0);
                self.preview = Some((all, index, false, 0.0));
            }
        }
    }

    fn preview(&mut self, ctx: &egui::Context, look: &mut Look, screen: Rect) {
        let Some((list, index, controls, moved)) = self.preview.take() else { return };
        let Some(a) = list.get(index).cloned() else { return };
        let (mut index, mut controls, mut moved, mut closed) = (index, controls, moved, false);
        egui::Area::new(Id::unique("preview")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            let response = ui.allocate_rect(screen, Sense::click_and_drag());
            ui.painter().rect_filled(screen, 0, Color32::BLACK);
            self.play(ctx, &a);
            let frame = self.played.as_ref().and_then(Played::frame);
            if let Some(t) = frame.or_else(|| self.texture(&a)) {
                let picture = Self::fitted(screen, &a);
                let picture = picture.translate(vec2(0.0, screen.center().y - picture.center().y));
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(t, picture, uv, Color32::WHITE);
            }
            if response.drag_started() {
                moved = 0.0;
            }
            moved += response.drag_delta().x;
            if response.drag_stopped() {
                if moved < -60.0 && index + 1 < list.len() {
                    index += 1;
                } else if moved > 60.0 && index > 0 {
                    index -= 1;
                }
            } else if response.clicked() {
                controls = !controls;
            }
            if controls {
                // a light disc with a black cross, 41 in from the top right
                let close = Rect::from_center_size(pos2(screen.right() - 41.0, screen.top() + 22.0), vec2(44.0, 44.0));
                ui.painter().circle_filled(close.center(), 23.5 / 2.0, argb(PREVIEW_CLOSE));
                look.icon(ui, &icons::CROSS, close.center(), 15.0, Color32::BLACK);
                closed = ui.interact(close, ui.scope_id().with("preview close"), Sense::click()).clicked();
                for (left, step) in [(true, -1i64), (false, 1)] {
                    let x = if left { screen.left() + 18.0 + 22.0 } else { screen.right() - 18.0 - 22.0 };
                    let button = Rect::from_center_size(pos2(x, screen.center().y), vec2(44.0, 60.0));
                    let (w, h) = (13.5, 23.5);
                    let (tip, back) = if left { (x - w / 2.0, x + w / 2.0) } else { (x + w / 2.0, x - w / 2.0) };
                    let y = screen.center().y;
                    let stroke = Stroke::new(4.0, argb(PREVIEW_ARROW));
                    ui.painter().line_segment([pos2(back, y - h / 2.0), pos2(tip, y)], stroke);
                    ui.painter().line_segment([pos2(tip, y), pos2(back, y + h / 2.0)], stroke);
                    if ui.interact(button, ui.scope_id().with(("preview arrow", left)), Sense::click()).clicked() {
                        index = (index as i64 + step).clamp(0, list.len() as i64 - 1) as usize;
                    }
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                closed = true;
            }
        });
        if closed {
            self.played = None;
        } else {
            self.preview = Some((list, index, controls, moved));
        }
    }

    /// Preview's artwork `a`, if it is animated: its frames read on a thread, then played as its
    /// settings say (its thumbnail shows until then).
    fn play(&mut self, ctx: &egui::Context, a: &GalleryArtwork) {
        if self.played.as_ref().is_none_or(|p| p.dir != a.dir) {
            self.played = Some(Played::start(a.dir.clone(), self.screen_px.0.max(self.screen_px.1)));
        }
        if let Some(p) = self.played.as_mut() {
            p.go_on(ctx);
        }
    }

    /// The app's name (in a stack, the way back and its name), then Select, Import and Photo in a
    /// capsule, and +.
    fn header(&mut self, ui: &Ui, look: &mut Look, screen: Rect) -> Option<Asked> {
        if self.selecting {
            self.select_header(ui, look, screen);
            return None;
        }
        let painter = ui.painter();
        let top = screen.top();
        if let Some((_, name)) = self.stack.clone() {
            let row = Rect::from_min_size(pos2(screen.left() + 20.0, top + 12.0), vec2(40.0, 41.0));
            self.back = row.union(row.translate(vec2(300.0, 0.0)));
            let response = ui.interact(self.back, ui.scope_id().with("back"), Sense::click());
            // active while touched, or while something dragged is over it, to take it out
            let over =
                self.lifted.is_some() && ui.input(|i| i.pointer.latest_pos()).is_some_and(|p| self.back.contains(p));
            let lit = response.is_pointer_button_down_on() || over;
            let ink = if lit { argb(crate::look::pc::ACCENT) } else { argb(TITLE) };
            look.icon(ui, &icons::CHEVRON_LEFT, row.center(), 28.0, ink);
            text(painter, pos2(row.right(), row.center().y), Align2::LEFT_CENTER, &name, 34.0, Weight::Medium, ink);
            if response.clicked() {
                if let Some((dir, name)) = self.stack.take() {
                    let cells = std::mem::take(&mut self.drawn_cells);
                    self.leaving_stack = Some(LeavingStack { dir, name, cells, since: Instant::now() });
                }
                self.refresh();
            }
        } else {
            // a stack just left: its name fades out as the gallery's fades in
            let ease = |t: f32| crate::look::cubic_bezier(0.42, 0.0, 0.58, 1.0, t);
            let left = self.leaving_stack.as_ref().map(|l| (l.name.clone(), l.since.elapsed().as_secs_f32()));
            let title_in = left.as_ref().map_or(1.0, |(_, t)| ease(t / TITLE_IN_S));
            if let Some((name, t)) = &left
                && *t < NAME_OUT_S
            {
                let ink = argb(TITLE).gamma_multiply(1.0 - ease(t / NAME_OUT_S));
                let row = Rect::from_min_size(pos2(screen.left() + 20.0, top + 12.0), vec2(40.0, 41.0));
                look.icon(ui, &icons::CHEVRON_LEFT, row.center(), 28.0, ink);
                text(painter, pos2(row.right(), row.center().y), Align2::LEFT_CENTER, name, 34.0, Weight::Medium, ink);
            }
            let heading_label = if screen.left() > 1.0 { "Recent" } else { "Sumi" };
            let heading_size = if screen.left() > 1.0 { 21.0 } else { 34.0 };
            let heading_top = if screen.left() > 1.0 { top + 19.0 } else { top + 11.0 };
            let name = text(
                painter,
                pos2(screen.left() + 28.0, heading_top),
                Align2::LEFT_TOP,
                heading_label,
                heading_size,
                Weight::Medium,
                argb(TITLE).gamma_multiply(title_in),
            );
            // the app's name tapped: what it is and its version
            if ui.interact(name, ui.scope_id().with("about"), Sense::click()).clicked() {
                self.about = true;
            }
        }
        let plus = Rect::from_min_size(pos2(screen.right() - 20.0 - 44.0, top + 10.0), vec2(44.0, 45.0));
        let capsule_width = 69.0 + 71.0 + 66.0;
        let capsule =
            Rect::from_min_size(pos2(plus.left() - 12.0 - capsule_width, top + 10.0), vec2(capsule_width, 45.0));
        painter.rect_filled(capsule, 22.5, argb(CAPSULE));
        painter.rect_stroke(capsule, 22.5, Stroke::new(1.0, argb(CAPSULE_EDGE)), egui::StrokeKind::Inside);
        let mut x = capsule.left();
        let mut asked = None;
        for (label, width) in [("Select", 69.0), ("Import", 71.0), ("Photo", 66.0)] {
            let button = Rect::from_min_size(pos2(x, capsule.top()), vec2(width, 45.0));
            text(painter, button.center(), Align2::CENTER_CENTER, label, 16.0, Weight::Regular, argb(TEXT));
            if ui.interact(button, ui.scope_id().with(label), Sense::click()).clicked() {
                asked = match label {
                    "Import" => Some(Asked::Import(self.folder())),
                    "Photo" => Some(Asked::Photo(self.folder())),
                    _ => {
                        self.selecting = true;
                        self.new_open = false;
                        None
                    }
                };
            }
            x += width;
        }
        let disc = Rect::from_center_size(plus.center(), vec2(42.0, 42.0));
        painter.rect_filled(disc, 21.0, argb(CAPSULE));
        painter.rect_stroke(disc, 21.0, Stroke::new(1.0, argb(CAPSULE_EDGE)), egui::StrokeKind::Inside);
        look.icon(ui, &icons::ADD, plus.center(), 40.0, argb(TEXT));
        if ui.interact(plus, ui.scope_id().with("new canvas"), Sense::click()).clicked() {
            self.new_open = !self.new_open;
        }
        asked
    }

    /// The artworks and stacks, three across (four on a wide window), scrolling.
    fn grid(&mut self, ui: &mut Ui, rect: Rect, look: &mut Look) -> Option<Asked> {
        let columns = if rect.width() > rect.height() + HEADER { 4 } else { 3 };
        let pitch = rect.width() / columns as f32;
        let rows = self.entries.len().div_ceil(columns);
        let mut asked = None;
        let mut into_stack = false;
        let entries = self.entries.clone();
        // the cells laid out this frame, for where a drag would land
        let mut cells: Vec<(usize, Rect)> = Vec::new();
        let mut drawn = Vec::new();
        // a stack opening: its artworks fly out of its cell to their places
        let opening = self.opening_stack.and_then(|(from, since)| {
            let t = since.elapsed().as_secs_f32() / STACK_OPEN_S;
            (t < 1.0).then(|| (from, crate::look::spring(STACK_OPEN_S, 0.8, 0.0, t)))
        });
        if opening.is_some() {
            ui.ctx().request_repaint();
        } else {
            self.opening_stack = None;
        }
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ScrollArea::vertical().id_salt("gallery grid").show(ui, |ui| {
                let (all, _) = ui.allocate_exact_size(vec2(rect.width(), rows as f32 * pitch + 24.0), Sense::hover());
                for (i, entry) in entries.iter().enumerate() {
                    let (row, column) = (i / columns, i % columns);
                    let cell = Rect::from_min_size(
                        all.min + vec2(column as f32 * pitch, row as f32 * pitch),
                        vec2(pitch, pitch),
                    );
                    if !ui.is_rect_visible(cell) {
                        continue;
                    }
                    let cell = match opening {
                        Some((from, e)) => cell.translate((from.min - cell.min) * (1.0 - e)),
                        None => cell,
                    };
                    drawn.push((entry.clone(), cell));
                    if let GalleryEntry::Stack { dir, .. } = entry {
                        self.stack_cells.insert(dir.clone(), cell);
                    }
                    let response = ui.interact(cell, ui.scope_id().with(("gallery cell", i)), Sense::click_and_drag());
                    cells.push((i, cell));
                    // the Time-lapse pill over its thumbnail is hit-tested before the cell's own
                    // hold, drag and tap: a tap there replays, never opens, renames or moves
                    let pill = match entry {
                        GalleryEntry::Artwork { artwork } if !self.selecting && self.recorded(artwork) => {
                            Some((artwork, Self::time_lapse_pill(cell)))
                        }
                        _ => None,
                    };
                    // the press this cell's pill took goes on until the pointer is let go: moved
                    // off it, the cell is neither swiped nor opened
                    if pill.as_ref().is_some_and(|(_, rect)| {
                        ui.input(|inp| {
                            inp.pointer.any_pressed() && inp.pointer.press_origin().is_some_and(|p| rect.contains(p))
                        })
                    }) && let Some((artwork, _)) = &pill
                    {
                        self.pill_press = Some(artwork.dir.clone());
                    }
                    let on_pill = self.pill_press.as_deref() == Some(entry_dir(entry));
                    // held still, it lifts; a lift let go where it was is no tap
                    if self.lifted.is_none()
                        && self.spreading.is_none()
                        && !self.selecting
                        && !on_pill
                        && response.is_pointer_button_down_on()
                        && !response.dragged()
                    {
                        let held = ui.input(|inp| inp.pointer.press_start_time().map_or(0.0, |t| inp.time - t));
                        if held >= HOLD_S {
                            let at = response.interact_pointer_pos().unwrap_or_else(|| cell.center());
                            self.lifted = Some((entry_dir(entry).to_owned(), at));
                            self.swiped = None;
                        } else {
                            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(HOLD_S - held));
                        }
                    }
                    let tapped = response.clicked() && self.lifted.is_none() && !on_pill;
                    // its picture shrinks under a press and grows as it lifts
                    let dir = entry_dir(entry);
                    let pressed = ui.input(|inp| inp.pointer.press_start_time().map_or(0.0, |t| inp.time - t));
                    let down = response.is_pointer_button_down_on() && !response.dragged();
                    let size = if self.lifted.as_ref().is_some_and(|(d, _)| d == dir) {
                        LIFTED
                    } else if down && pressed >= PRESS_DELAY_S {
                        PRESSED
                    } else {
                        if down {
                            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(PRESS_DELAY_S - pressed));
                        }
                        DROPPED
                    };
                    let scale = self.lift(ui.ctx(), dir, size);
                    match entry {
                        GalleryEntry::Artwork { artwork } => {
                            self.artwork_cell(ui, cell, artwork, scale);
                            // the pill replays the time-lapse; it is all the pill does
                            let mut replay = false;
                            if let Some((artwork, rect)) = pill {
                                let response =
                                    ui.interact(rect, ui.scope_id().with(("time-lapse", &artwork.dir)), Sense::click());
                                let label = format!("View time-lapse for {}", artwork.name);
                                response
                                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
                                let focused = response.has_focus();
                                let response = response
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text("View time-lapse");
                                Self::paint_time_lapse_pill(
                                    ui,
                                    look,
                                    rect,
                                    focused,
                                    response.hovered(),
                                    response.is_pointer_button_down_on(),
                                );
                                replay = response.clicked();
                            }
                            // under its thumbnail (on its name and size), a tap renames it
                            let below = cell.top() + 39.0 + Self::thumb_box(cell).height() + 8.0;
                            let on_name = response.interact_pointer_pos().is_some_and(|p| p.y > below);
                            // dragged across, its actions show (left) or go (right)
                            if response.drag_started() && !on_pill {
                                self.dragged = egui::Vec2::ZERO;
                            }
                            self.dragged += response.drag_delta();
                            if response.drag_stopped()
                                && !on_pill
                                && !self.selecting
                                && self.lifted.is_none()
                                && self.dragged.x.abs() > self.dragged.y.abs()
                            {
                                self.swiped = (self.dragged.x < 0.0).then(|| artwork.dir.clone());
                            }
                            // swiped open, it fades back and its actions grow in over it
                            let swiped = self.swiped.as_ref() == Some(&artwork.dir);
                            let id = ui.scope_id().with(("swipe open", &artwork.dir));
                            let open = ui.ctx().animate_bool_with_time_and_easing(id, swiped, SWIPE_S, swipe_ease);
                            if open > 0.0 {
                                self.swipe_actions(ui, cell, artwork, open, swiped);
                            }
                            // selecting, a tap picks it (or lets it go); a tap puts away actions
                            // showing
                            if tapped && self.swiped.is_some() {
                                self.swiped = None;
                            } else if tapped && on_name && !self.selecting {
                                self.renaming = Some((artwork.clone(), artwork.name.clone(), false));
                                self.renaming_stack = None;
                            } else if tapped && self.selecting {
                                if let Some(at) = self.picked.iter().position(|d| *d == artwork.dir) {
                                    self.picked.remove(at);
                                } else {
                                    self.picked.push(artwork.dir.clone());
                                }
                            } else if tapped {
                                asked = Some(Asked::Open(artwork.dir.clone()));
                            }
                            if replay {
                                // the player covers the gallery: what was open under it goes
                                self.swiped = None;
                                self.sharing = None;
                                self.new_open = false;
                                self.more_open = false;
                                self.lifted = None;
                                self.dragged = egui::Vec2::ZERO;
                                self.player = Some(TimeLapsePlayer::for_gallery(&artwork.dir));
                            }
                        }
                        GalleryEntry::Stack { dir, name, artworks } => {
                            self.stack_cell(ui, cell, name, artworks);
                            // on its name, a tap renames it, as an artwork's does
                            let below = cell.top() + 39.0 + Self::thumb_box(cell).height() + 8.0;
                            let on_name = response.interact_pointer_pos().is_some_and(|p| p.y > below);
                            if tapped
                                && on_name
                                && !self.selecting
                                && let Some(top) = artworks.first()
                            {
                                let shown = GalleryArtwork { name: name.clone(), ..top.clone() };
                                self.renaming = Some((shown, name.clone(), false));
                                self.renaming_stack = Some(dir.clone());
                            } else if tapped && !self.selecting {
                                self.stack = Some((dir.clone(), name.clone()));
                                self.opening_stack = Some((cell, Instant::now()));
                                into_stack = true;
                            }
                        }
                    }
                }
            });
        });
        self.drag(ui, &entries, &cells);
        self.drawn_cells = drawn;
        // the pointer let go: no pill holds this press any more
        if !ui.input(|inp| inp.pointer.any_down()) {
            self.pill_press = None;
        }
        if into_stack {
            self.refresh();
        }
        asked
    }

    /// A stack just left: its artworks flying back into its cell, over the gallery coming back.
    fn stack_flights(&mut self, ctx: &egui::Context, _look: &mut Look) {
        let Some(leaving) = self.leaving_stack.take() else { return };
        let t = leaving.since.elapsed().as_secs_f32() / STACK_LEAVE_S;
        if t >= 1.0 {
            return;
        }
        ctx.request_repaint();
        let e = crate::look::spring(STACK_LEAVE_S, 0.8, 0.0, t);
        let to = self.stack_cells.get(&leaving.dir).copied();
        let screen = ctx.content_rect();
        egui::Area::new(Id::unique("stack left"))
            .fixed_pos(screen.min)
            .order(Order::Foreground)
            .interactable(false)
            .fade_in(false)
            .show(ctx, |ui| {
                for (entry, cell) in &leaving.cells {
                    // to the stack's cell, its top left there (where it isn't known, they just go)
                    let at = match to {
                        Some(to) => cell.translate((to.min - cell.min) * e),
                        None => *cell,
                    };
                    let mut faded = ui.new_child(UiBuilder::new().max_rect(screen));
                    if to.is_none() {
                        faded.multiply_opacity(1.0 - e.clamp(0.0, 1.0));
                    }
                    match entry {
                        GalleryEntry::Artwork { artwork } => _ = self.artwork_cell(&faded, at, artwork, 1.0),
                        GalleryEntry::Stack { name, artworks, .. } => self.stack_cell(&faded, at, name, artworks),
                    }
                }
            });
        self.leaving_stack = Some(leaving);
    }

    /// Where a point over the grid would put what is dragged: onto another's picture (an
    /// artwork's thumbnail as it sits in its box, or a stack's box), when it may be stacked; anywhere
    /// else the slot between cells it is nearest, before a cell's middle or after it.
    fn drop_at(
        entries: &[GalleryEntry],
        cells: &[(usize, Rect)],
        dragged: &str,
        p: Pos2,
        stacking: bool,
    ) -> Option<(Option<String>, usize)> {
        for &(i, cell) in cells {
            if !cell.contains(p) {
                continue;
            }
            let e = entries.get(i)?;
            let frame = Self::thumb_box(cell);
            let picture = match e {
                GalleryEntry::Artwork { artwork } => Self::fitted(frame, artwork),
                GalleryEntry::Stack { .. } => frame,
            };
            if stacking && entry_dir(e) != dragged && picture.contains(p) {
                return Some((Some(entry_dir(e).to_owned()), i));
            }
            return Some((None, if p.x < cell.center().x { i } else { i + 1 }));
        }
        // past the last cell: last
        let &(last, cell) = cells.last()?;
        let past = p.y >= cell.bottom() || (p.y >= cell.top() && p.x >= cell.right());
        (last + 1 == entries.len() && past).then_some((None, entries.len()))
    }

    /// A lifted artwork (or stack) follows the pointer, its cell faint behind; let go, it goes where
    /// it was let go: onto another to stack with it, between cells to move there, or (in a stack)
    /// on the way back, out to the gallery's front.
    fn drag(&mut self, ui: &Ui, entries: &[GalleryEntry], cells: &[(usize, Rect)]) {
        let Some((dir, from)) = self.lifted.clone() else { return };
        let Some(entry) = entries.iter().find(|e| entry_dir(e) == dir).cloned() else {
            self.lifted = None;
            return;
        };
        let (pointer, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_down()));
        // an artwork taken past the window's edge leaves it, as a file for wherever it is let go
        // (the pointer outside, or gone as the window lost it)
        if let GalleryEntry::Artwork { artwork } = &entry
            && down
            && cfg!(windows)
            && pointer.is_none_or(|p| !ui.ctx().content_rect().contains(p))
        {
            self.dragged_out = Some(artwork.clone());
            self.lifted = None;
            return;
        }
        let stacking = self.stack.is_none() && matches!(entry, GalleryEntry::Artwork { .. });
        let target = pointer.and_then(|p| Self::drop_at(entries, cells, &dir, p, stacking));
        for &(i, cell) in cells {
            if entries.get(i).is_some_and(|e| entry_dir(e) == dir) {
                ui.painter().rect_filled(cell, 0, argb(BACKGROUND).gamma_multiply(DRAGGED_FADE));
            }
            if let Some((Some(onto), _)) = &target
                && entries.get(i).is_some_and(|e| entry_dir(e) == onto)
            {
                let frame = Self::thumb_box(cell);
                let picture = match entries.get(i) {
                    Some(GalleryEntry::Artwork { artwork }) => Self::fitted(frame, artwork),
                    _ => frame,
                };
                ui.painter().rect_filled(picture, 8, argb(DROP_WASH));
            }
        }
        // the picture under the pointer, held where it was taken
        let shown = match &entry {
            GalleryEntry::Artwork { artwork } => Some(artwork.clone()),
            GalleryEntry::Stack { artworks, .. } => artworks.first().cloned(),
        };
        if let (Some(a), Some(p), Some(&(_, cell))) =
            (shown, pointer, cells.iter().find(|(i, _)| entries.get(*i).is_some_and(|e| entry_dir(e) == dir)))
        {
            // grown as it lifts, about where it is held
            let k = self.lifts.get(&dir).map_or(LIFTED.0, Lift::now);
            let picture = Self::fitted(Self::thumb_box(cell), &a).translate(p - from);
            let picture = Rect::from_min_max(p + (picture.min - p) * k, p + (picture.max - p) * k);
            let texture = self.texture(&a);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(Order::Tooltip, Id::unique("lifted artwork")));
            let shadow =
                egui::epaint::Shadow { offset: [0, 4], blur: 24, spread: 0, color: Color32::from_black_alpha(0x80) };
            painter.add(shadow.as_shape(picture, 8));
            painter.rect_filled(picture, 8, Color32::WHITE);
            if let Some(t) = texture {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.add(egui::epaint::RectShape::filled(picture, 8, Color32::WHITE).with_texture(t, uv));
            }
        }
        if down {
            ui.ctx().request_repaint();
            return;
        }
        self.lifted = None;
        if self.stack.is_some() && pointer.is_some_and(|p| self.back.contains(p)) {
            self.store.move_entry(dir, 0, Some(self.store.root()));
        } else {
            match target {
                Some((Some(onto), _)) => self.store.put_onto(dir, onto),
                Some((None, slot)) => {
                    let at = entries.iter().position(|e| entry_dir(e) == dir).unwrap_or(slot);
                    let to = if slot > at { slot - 1 } else { slot };
                    if to != at {
                        self.store.move_entry(dir, to as i64, None);
                    }
                }
                None => {}
            }
        }
        self.refresh();
    }

    /// A swiped artwork's actions: Share, Duplicate and Delete as pills across the middle of its
    /// thumbnail's box, the cell faded back under them.
    /// A swiped artwork's actions, `open` of the way in (they grow from a fifth of their size
    /// about their right end, fading in, as the artwork fades back); tappable once `shown`.
    fn swipe_actions(&mut self, ui: &Ui, cell: Rect, a: &GalleryArtwork, open: f32, shown: bool) {
        let alpha = open.clamp(0.0, 1.0);
        ui.painter().rect_filled(cell, 0, argb(BACKGROUND).gamma_multiply(SWIPED_FADE * alpha));
        let frame = Self::thumb_box(cell);
        let width = SWIPE_PILL.0 * 3.0 + 6.0;
        let (k, origin) = (0.2 + 0.8 * open, pos2(cell.center().x + width / 2.0, frame.center().y));
        let grown = |r: Rect| Rect::from_min_max(origin + (r.min - origin) * k, origin + (r.max - origin) * k);
        let mut x = cell.center().x - width / 2.0;
        for (label, delete) in [("Share", false), ("Duplicate", false), ("Delete", true)] {
            let pill =
                Rect::from_min_size(pos2(x, frame.center().y - SWIPE_PILL.1 / 2.0), vec2(SWIPE_PILL.0, SWIPE_PILL.1));
            x += SWIPE_PILL.0 + 3.0;
            let drawn = grown(pill);
            let fill = if delete { argb(SWIPE_DELETE_FILL) } else { argb(SWIPE_PILL_FILL) };
            ui.painter().rect_filled(drawn, 12.0 * k, fill.gamma_multiply(alpha));
            let ink = if delete { Color32::WHITE } else { argb(SWIPE_PILL_INK) };
            let words = (15.0 * k).max(1.0);
            text(
                ui.painter(),
                drawn.center(),
                Align2::CENTER_CENTER,
                label,
                words,
                Weight::SemiBold,
                ink.gamma_multiply(alpha),
            );
            if !shown {
                continue;
            }
            if ui.interact(pill, ui.scope_id().with(("swiped", label)), Sense::click()).clicked() {
                match label {
                    "Duplicate" => {
                        self.swiped = None;
                        self.duplicate(vec![a.dir.clone()]);
                    }
                    "Delete" => {
                        self.swiped = None;
                        self.deleting = Some(a.dir.clone());
                        let title = format!("Delete \u{201c}{}\u{201d}?", a.name);
                        self.alert.open(&title, "This can't be undone.", "Delete", Some(&icons::TRASH));
                    }
                    _ => {
                        self.swiped = None;
                        self.sharing = Some((vec![a.clone()], Some(pill)));
                    }
                }
            }
        }
    }

    /// What sharing these artworks as a `.clip` and a `.kra` would keep differently, read when
    /// Share's formats open: each artwork's `artwork.json` alone, never its layers' pixels. The
    /// answer is kept until the artworks or their files change.
    fn share_notes(&mut self, list: &[GalleryArtwork]) -> ShareNotes {
        let key: Vec<(String, i64)> = list.iter().map(|a| (a.dir.clone(), a.modified_ms)).collect();
        if let Some((cached, notes)) = &self.share_checks
            && *cached == key
        {
            return notes.clone();
        }
        let notes = ShareNotes { clip: share_changes(list, false), kra: share_changes(list, true) };
        self.share_checks = Some((key, notes.clone()));
        notes
    }

    /// Share's formats: beside a swiped artwork's Share, pointing at it, on the side with more
    /// room (the other if that one is too narrow), kept 2 inside the window; or under Select's
    /// Share, centred on it, kept 4 inside. A tap on one shares in it, a tap off closes.
    fn formats(&mut self, ctx: &egui::Context, look: &mut Look, screen: Rect) -> Option<Asked> {
        let (list, from) = self.sharing.clone()?;
        let notes = self.share_notes(&list);
        let width = FORMATS_WIDTH;
        // the two native documents carry a line under their rows, part of the panel's height
        let group_height = share_group_height();
        let height = share_panel_height();
        let (rect, arrow) = match from {
            Some(pill) => {
                let (rect, arrow_y, right) = formats_beside(pill, screen, vec2(width, height));
                (rect, Some((arrow_y, right)))
            }
            None => {
                let centre = self.share_button.center().x;
                let left = (centre - width / 2.0).clamp(screen.left() + 4.0, screen.right() - 4.0 - width);
                (Rect::from_min_size(pos2(left, screen.top() + 69.0), vec2(width, height)), None)
            }
        };
        let (mut chosen, mut close) = (None, false);
        egui::Area::new(Id::unique("share formats")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            close = ui.allocate_rect(screen, Sense::click()).clicked();
            ui.interact(rect, ui.scope_id().with("panel"), Sense::click_and_drag());
            let rect = match arrow {
                // its arrow on its left when it is right of the pill
                Some((arrow_y, right)) => panel::solid_side_frame(
                    ui,
                    look,
                    "share formats",
                    rect,
                    arrow_y,
                    right,
                    panel::POPOVER_CORNER,
                    POPOVER,
                ),
                None => {
                    let shadow = egui::epaint::Shadow {
                        offset: [0, 0],
                        blur: 51,
                        spread: 0,
                        color: argb(panel::POPOVER_SHADOW),
                    };
                    ui.painter().add(shadow.as_shape(rect, panel::POPOVER_CORNER));
                    ui.painter().rect_filled(rect, panel::POPOVER_CORNER, argb(POPOVER));
                    rect
                }
            };
            let ink = argb(PANEL_TITLE);
            text(
                ui.painter(),
                pos2(rect.left() + 22.0, rect.top() + 27.5),
                Align2::LEFT_CENTER,
                "Image Format",
                22.0,
                Weight::SemiBold,
                ink,
            );
            let group =
                Rect::from_min_size(pos2(rect.left() + 10.0, rect.top() + 55.0), vec2(width - 20.0, group_height));
            ui.painter().rect_filled(group, 18.0, argb(ACTIONS_GROUP));
            let note_ink = argb(ACTIONS_DISABLED);
            let mut y = group.top();
            for (i, format) in SHARE_ROWS.into_iter().enumerate() {
                let note = format.note(&notes);
                let row = Rect::from_min_size(pos2(group.left(), y), vec2(group.width(), FORMAT_ROW));
                if i > 0 {
                    let line = Rect::from_min_size(pos2(row.left(), row.top() - 1.0), vec2(row.width(), 1.0));
                    ui.painter().rect_filled(line, 0, argb(ACTIONS_SEPARATOR));
                }
                text(
                    ui.painter(),
                    pos2(row.left() + 16.0, row.center().y),
                    Align2::LEFT_CENTER,
                    format.name(),
                    17.0,
                    Weight::Regular,
                    ink,
                );
                if format.actionable(&notes)
                    && ui.interact(row, ui.scope_id().with(("format", i)), Sense::click()).clicked()
                {
                    chosen = Some(format);
                }
                y = row.bottom();
                if let Some(note) = note {
                    let band = Rect::from_min_size(pos2(group.left(), y), vec2(group.width(), FORMAT_NOTE));
                    let line = one_line(ui, &note, 13.0, Weight::Regular, note_ink, band.width() - 32.0);
                    let at = pos2(band.left() + 16.0, band.center().y - line.size().y / 2.0);
                    ui.painter().galley(at, line, note_ink);
                    y = band.bottom();
                }
            }
        });
        if close || chosen.is_some() {
            self.sharing = None;
        }
        match chosen? {
            // how well a PDF keeps its pictures, asked first
            ShareRow::Still(crate::files::Format::Pdf) => {
                self.pdf_for = Some(list);
                self.pdf_alert.ask(crate::alert::pdf_quality());
                None
            }
            ShareRow::Still(format) => Some(Asked::Share(list, format)),
            // an animation is made from the first of them
            ShareRow::Animated(format) => {
                let first = list.into_iter().next()?;
                self.end_selecting();
                Some(Asked::ShareAnimated(first, format))
            }
        }
    }

    /// The box a cell's thumbnail fits in: the cell's width less 40, its height less 82, 39 down.
    fn thumb_box(cell: Rect) -> Rect {
        let (w, h) = (cell.width() - 40.0, cell.height() - 82.0);
        Rect::from_min_size(pos2(cell.center().x - w / 2.0, cell.top() + 39.0), vec2(w, h))
    }

    /// The artwork's picture as large as fits in `frame`, at its bottom middle.
    fn fitted(frame: Rect, a: &GalleryArtwork) -> Rect {
        let ratio = if a.width > 0 && a.height > 0 { a.width as f32 / a.height as f32 } else { 0.75 };
        let size = if ratio <= frame.width() / frame.height() {
            vec2(frame.height() * ratio, frame.height())
        } else {
            vec2(frame.width(), frame.width() / ratio)
        };
        Rect::from_min_size(pos2(frame.center().x - size.x / 2.0, frame.bottom() - size.y), size)
    }

    /// An artwork's thumbnail: read on the reader's thread the first time (and after it
    /// changes), the one before (or none) shown meanwhile.
    fn texture(&mut self, a: &GalleryArtwork) -> Option<egui::TextureId> {
        let fresh = self.thumbs.get(&a.dir).is_none_or(|(when, _)| *when != a.modified_ms);
        let asked = (a.dir.clone(), a.modified_ms);
        if fresh && !self.reading.contains(&asked) {
            let path = std::path::Path::new(&a.dir).join("thumbnail.png");
            let reader = self.reader.get_or_insert_with(ThumbnailReader::start);
            if reader.asks.send(ReaderAsk::Thumbnail(asked.0.clone(), asked.1, path)).is_ok() {
                self.reading.insert(asked);
            }
        }
        self.thumbs.get(&a.dir).and_then(|(_, t)| t.as_ref().map(TextureHandle::id))
    }

    /// The artworks in `dirs` duplicated, each beside itself, on a thread of their own (one batch
    /// at a time, so each finds the order the last left); the copies show once made.
    fn duplicate(&mut self, dirs: Vec<String>) {
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let (store, (done, duplicating)) = (Arc::clone(&self.store), std::sync::mpsc::channel());
        std::thread::spawn(move || {
            let _one = ONE_AT_A_TIME.lock();
            for dir in dirs {
                store.duplicate(dir);
            }
            let _ = done.send(());
        });
        self.duplicating.push(duplicating);
    }

    /// Duplicates made since the last frame shown (the gallery read again); looked at again soon
    /// while more are being made.
    fn duplicated(&mut self, ctx: &egui::Context) {
        let before = self.duplicating.len();
        self.duplicating.retain(|d| matches!(d.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        if self.duplicating.len() < before {
            self.refresh();
        }
        if !self.duplicating.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// The thumbnails read since the last frame made textures; looked at again soon while more
    /// are coming.
    fn thumbnails_read(&mut self, ctx: &egui::Context) {
        let Some(reader) = &self.reader else { return };
        loop {
            match reader.read.try_recv() {
                Ok((dir, when, image)) => {
                    self.reading.remove(&(dir.clone(), when));
                    // enlarged, each of its pixels a square of its own colour, as the canvas shows them
                    let crisp = egui::TextureOptions {
                        magnification: egui::TextureFilter::Nearest,
                        ..egui::TextureOptions::LINEAR
                    };
                    let texture = image.map(|pixels| ctx.load_texture(format!("thumbnail {dir}"), pixels, crisp));
                    self.thumbs.insert(dir, (when, texture));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // its thread stopped: started again for those still wanted
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.reader = None;
                    self.reading.clear();
                    break;
                }
            }
        }
        if !self.reading.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    /// An artwork's thumbnail in `rect`, turned `turn` degrees: white under its picture, rounded
    /// 8 (but turned), with a soft shadow.
    fn thumbnail(&mut self, ui: &Ui, rect: Rect, a: &GalleryArtwork, turn: f32) {
        let texture = self.texture(a);
        let painter = ui.painter();
        if turn == 0.0 {
            let shadow = egui::epaint::Shadow { offset: [0, 5], blur: 8, spread: 0, color: argb(THUMB_SHADOW) };
            painter.add(shadow.as_shape(rect, 8));
            // a picture with its background hidden shows the gallery through it (white while it loads)
            painter.rect_filled(rect, 8, if texture.is_some() { argb(BACKGROUND) } else { Color32::WHITE });
            if let Some(t) = texture {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.add(egui::epaint::RectShape::filled(rect, 8, Color32::WHITE).with_texture(t, uv));
            }
            painter.rect_stroke(rect, 8, Stroke::new(1.0, argb(THUMB_RIM)), egui::StrokeKind::Inside);
            return;
        }
        let (sin, cos) = turn.to_radians().sin_cos();
        let c = rect.center();
        let turned = |p: Pos2| {
            let d = p - c;
            c + vec2(d.x * cos - d.y * sin, d.x * sin + d.y * cos)
        };
        let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()].map(turned);
        let uvs = [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)];
        let quad = |texture: egui::TextureId| {
            let mut mesh = egui::Mesh::with_texture(texture);
            for (p, uv) in corners.iter().zip(uvs) {
                mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv, color: Color32::WHITE });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            mesh
        };
        painter.add(egui::Shape::convex_polygon(
            corners.to_vec(),
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_black_alpha(0x30)),
        ));
        if let Some(t) = texture {
            painter.add(egui::Shape::mesh(quad(t)));
        }
    }

    /// Draws an artwork's cell; returns where its name is.
    fn artwork_cell(&mut self, ui: &Ui, cell: Rect, a: &GalleryArtwork, scale: f32) -> Rect {
        let frame = Self::thumb_box(cell);
        let picture = Self::fitted(frame, a);
        self.pictures.insert(a.dir.clone(), picture);
        // shrunk or grown about its middle as it is pressed or lifted; spread open, it is drawn
        // where the fingers have it instead
        let picture = Rect::from_center_size(picture.center(), picture.size() * scale);
        if self.spreading.as_ref().is_none_or(|s| s.artwork.dir != a.dir) {
            self.thumbnail(ui, picture, a, 0.0);
        }
        let chosen = self.picked.contains(&a.dir);
        // the marks fade in as Select starts, and out as it ends
        let marks = ui.ctx().animate_bool_with_time_and_easing(
            Id::unique("select marks"),
            self.selecting,
            SELECT_FADE_S,
            |t| crate::look::cubic_bezier(0.42, 0.0, 0.58, 1.0, t),
        );
        if marks > 0.0 {
            // a circle at its top right: filled active with a tick when picked, else a thin light ring
            let mark = pos2(picture.right() - 12.0 - 7.0, picture.top() + 12.0 + 7.0);
            if chosen {
                ui.painter().circle_filled(mark, 7.0, argb(PICKED).gamma_multiply(marks));
            } else {
                let ring = Stroke::new(1.0, argb(UNPICKED_RING).gamma_multiply(marks));
                ui.painter().circle_stroke(mark, 6.5, ring);
            }
        }
        if chosen {
            tick(ui.painter(), pos2(picture.right() - 19.0, picture.top() + 19.0));
        }
        let ink = argb(TEXT);
        let name = one_line(ui, &a.name, 13.0, Weight::SemiBold, ink, frame.width());
        let at = pos2(cell.center().x - name.size().x / 2.0, frame.bottom() + 12.0);
        ui.painter().galley(at, name.clone(), ink);
        let size = format!("{} \u{d7} {}px", a.width, a.height);
        text(
            ui.painter(),
            pos2(cell.center().x, at.y + name.size().y),
            Align2::CENTER_TOP,
            &size,
            10.0,
            Weight::Regular,
            argb(SIZE),
        );
        Rect::from_min_size(at, name.size())
    }

    fn stack_cell(&mut self, ui: &Ui, cell: Rect, name: &str, artworks: &[GalleryArtwork]) {
        let Some(top) = artworks.first() else { return };
        let frame = Self::thumb_box(cell);
        let picture = Self::fitted(frame, top);
        // the top artwork again on two cards under it, a little turned (none for one)
        if artworks.len() > 1 {
            for turn in [5.0, -4.0] {
                self.thumbnail(ui, picture, top, turn);
            }
        }
        self.thumbnail(ui, picture, top, 0.0);
        // selecting, a stack can't be picked: its name dims
        let ink = if self.selecting { argb(SIZE) } else { argb(TEXT) };
        let label = one_line(ui, name, 13.0, Weight::SemiBold, ink, frame.width());
        let at = pos2(cell.center().x - label.size().x / 2.0, frame.bottom() + 12.0);
        ui.painter().galley(at, label.clone(), ink);
        let count = format!("{} artworks", artworks.len());
        text(
            ui.painter(),
            pos2(cell.center().x, at.y + label.size().y),
            Align2::CENTER_TOP,
            &count,
            10.0,
            Weight::Regular,
            argb(SIZE),
        );
    }

    /// The New canvas list hanging from +: the window's size first, then the presets (scrolling
    /// past 376), each with its colour profile and size. A tap off it closes it.
    fn new_canvas(&mut self, ctx: &egui::Context, look: &mut Look, screen: Rect) -> Option<Asked> {
        let left = screen.right() - 2.0 - NEW_WIDTH;
        let arrow = screen.right() - 49.0 - left;
        let list_height = (45.0 + 45.0 * self.presets.len().saturating_sub(1) as f32).min(PRESETS_MOST);
        // a row more for each kind of artwork a plugin makes
        let kinds_height = self.kinds.len() as f32 * (9.5 + 38.0);
        let height = panel::ARROW_HEIGHT + NEW_HEADER + 38.0 + 9.5 + list_height + kinds_height + 30.0;
        let rect = Rect::from_min_size(pos2(left, screen.top() + 56.5), vec2(NEW_WIDTH, height));
        let mut asked = None;
        let mut close = false;
        egui::Area::new(Id::unique("new canvas")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            close = ui.allocate_rect(screen, Sense::click()).clicked();
            ui.interact(rect, ui.scope_id().with("panel"), Sense::click_and_drag());
            let body = panel::solid_frame(ui, look, "new canvas", rect, arrow, false, 32.0, LIGHT_PANEL);
            let ink = argb(LIGHT_TEXT);
            text(
                ui.painter(),
                pos2(body.left() + 23.0, body.top() + 21.0),
                Align2::LEFT_TOP,
                "New canvas",
                26.0,
                Weight::Bold,
                ink,
            );
            // the button that makes a custom canvas: a dark card with a plus
            let custom = Rect::from_min_size(pos2(body.right() - 13.0 - 44.0, body.top() + 18.0), vec2(44.0, 34.0));
            let card = Rect::from_center_size(custom.center(), vec2(27.0, 16.0));
            ui.painter().rect_filled(card, 4.0, ink);
            let plus = pos2(card.left() + 8.0, card.center().y);
            let stroke = Stroke::new(1.6, argb(LIGHT_PANEL));
            ui.painter().line_segment([plus - vec2(3.6, 0.0), plus + vec2(3.6, 0.0)], stroke);
            ui.painter().line_segment([plus - vec2(0.0, 3.6), plus + vec2(0.0, 3.6)], stroke);
            if ui.interact(custom, ui.scope_id().with("custom canvas"), Sense::click()).clicked() {
                self.custom = Some(crate::custom_canvas::CustomCanvas::new(None));
            }
            // the screen's size, a canvas in Display P3
            let (sw, sh) = self.screen_px;
            let row =
                Rect::from_min_size(pos2(body.left() + 9.0, body.top() + NEW_HEADER), vec2(NEW_WIDTH - 18.0, 38.0));
            ui.painter().rect_filled(row, 19.0, argb(LIGHT_ROW_STRONG));
            preset_row(ui, row, "Screen Size", &format!("{sw} \u{d7} {sh}px"), P3);
            if ui.interact(row, ui.scope_id().with("screen size"), Sense::click()).clicked() {
                asked = Some(Asked::ScreenSize(self.folder()));
            }
            let list = Rect::from_min_size(pos2(row.left(), row.bottom() + 9.5), vec2(row.width(), list_height));
            ui.painter().rect_filled(list, 24.0, argb(LIGHT_ROW));
            // the plugins' kinds of artwork under the presets, each the screen's size
            for (k, kind) in self.kinds.iter().enumerate() {
                let at = list.bottom() + 9.5 + k as f32 * (9.5 + 38.0);
                let line = Rect::from_min_size(pos2(row.left(), at), vec2(row.width(), 38.0));
                ui.painter().rect_filled(line, 19.0, argb(LIGHT_ROW_STRONG));
                preset_row(ui, line, &kind.label, &format!("{sw} \u{d7} {sh}px"), P3);
                if ui.interact(line, ui.scope_id().with(("plugin kind", k)), Sense::click()).clicked() {
                    asked = Some(Asked::PluginKind(kind.key.clone(), self.folder()));
                }
            }
            let presets = self.presets.clone();
            ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
                ui.set_clip_rect(list);
                ScrollArea::vertical().id_salt("presets").show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    for (i, p) in presets.iter().enumerate() {
                        let (line, response) =
                            ui.allocate_exact_size(vec2(list.width(), 45.0), Sense::click_and_drag());
                        // slid left under a drag, it shows Edit and Delete behind it
                        let x = match self.preset_drag {
                            Some((d, x)) if d == i => x,
                            _ if self.preset_open == Some(i) => -PRESET_REVEAL,
                            _ => 0.0,
                        };
                        if response.drag_started() {
                            self.preset_drag = Some((i, x));
                        }
                        if let Some((d, x)) = self.preset_drag.as_mut()
                            && *d == i
                        {
                            *x = (*x + response.drag_delta().x).clamp(-PRESET_REVEAL, 0.0);
                        }
                        if response.drag_stopped() {
                            self.preset_open = (x < -PRESET_REVEAL / 2.0).then_some(i);
                            self.preset_drag = None;
                        }
                        if x < 0.0 {
                            ui.painter().rect_filled(line, 0, argb(PRESET_BEHIND));
                            let delete = Rect::from_min_size(
                                pos2(line.right() - 13.0 - 69.0, line.center().y - 16.5),
                                vec2(69.0, 33.0),
                            );
                            let edit = delete.translate(vec2(-72.0, 0.0));
                            for (button, label, fill, ink) in [
                                (edit, "Edit", PRESET_EDIT, argb(PRESET_EDIT_INK)),
                                (delete, "Delete", PRESET_DELETE, Color32::WHITE),
                            ] {
                                ui.painter().rect_filled(button, 7.0, argb(fill));
                                text(
                                    ui.painter(),
                                    button.center(),
                                    Align2::CENTER_CENTER,
                                    label,
                                    15.0,
                                    Weight::SemiBold,
                                    ink,
                                );
                                if ui
                                    .interact(button, ui.scope_id().with(("preset", label, i)), Sense::click())
                                    .clicked()
                                {
                                    self.preset_open = None;
                                    if label == "Edit" {
                                        self.custom =
                                            Some(crate::custom_canvas::CustomCanvas::new(Some((i, p.clone()))));
                                    } else if i < self.presets.len() {
                                        self.presets.remove(i);
                                        self.keep_presets();
                                    }
                                }
                            }
                        }
                        let shown = line.translate(vec2(x, 0.0));
                        if x < 0.0 {
                            ui.painter().rect_filled(shown, 0, argb(LIGHT_ROW));
                        }
                        if i > 0 {
                            let edge = Rect::from_min_size(
                                pos2(shown.left() + 16.0, shown.top()),
                                vec2(shown.width() - 16.0, 1.0),
                            );
                            ui.painter().rect_filled(edge, 0, argb(LIGHT_SEPARATOR));
                        }
                        preset_row(ui, shown, &p.name, &preset_label(p.clone()), &p.profile);
                        // a tap picks it, or puts away a row swiped open
                        if response.clicked() {
                            if self.preset_open.is_some() {
                                self.preset_open = None;
                            } else {
                                asked = Some(Asked::New(p.clone(), self.folder()));
                            }
                        }
                    }
                });
            });
        });
        if asked.is_some() || close {
            self.new_open = false;
        }
        asked
    }
}

/// The folder of what a gallery entry shows.
fn entry_dir(e: &GalleryEntry) -> &str {
    match e {
        GalleryEntry::Artwork { artwork } => &artwork.dir,
        GalleryEntry::Stack { dir, .. } => dir,
    }
}

/// The active ink tick centred at `c`, in a picked artwork's mark.
fn tick(painter: &egui::Painter, c: Pos2) {
    let stroke = Stroke::new(1.5, argb(crate::look::pc::ACCENT_INK));
    painter.line_segment([c + vec2(-3.0, 0.0), c + vec2(-1.0, 2.5)], stroke);
    painter.line_segment([c + vec2(-1.0, 2.5), c + vec2(3.5, -2.5)], stroke);
}

/// A preset's row: its name, then chips for its colour profile and size.
fn preset_row(ui: &Ui, row: Rect, name: &str, size: &str, profile: &str) {
    let ink = argb(LIGHT_TEXT);
    let mut x = row.right() - 16.0;
    for label in [size.to_owned(), profile_chip(profile.to_owned())] {
        let galley = one_line(ui, &label, 13.0, Weight::Regular, ink, f32::INFINITY);
        let chip =
            Rect::from_min_max(pos2(x - galley.size().x - 16.0, row.center().y - 9.0), pos2(x, row.center().y + 9.0));
        ui.painter().rect_filled(chip, 9.0, argb(LIGHT_CHIP));
        ui.painter().galley(pos2(chip.left() + 8.0, chip.center().y - galley.size().y / 2.0), galley, ink);
        x = chip.left() - 4.0;
    }
    let galley = one_line(ui, name, 17.0, Weight::Regular, ink, x - 8.0 - (row.left() + 17.0));
    ui.painter().galley(pos2(row.left() + 17.0, row.center().y - galley.size().y / 2.0), galley, ink);
}

/// `text` on one line at most `max_width` wide, ending in "…" when cut.
fn one_line(ui: &Ui, text: &str, size: f32, weight: Weight, colour: Color32, max_width: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    let format = TextFormat { font_id: weight.font(size), color: colour, ..Default::default() };
    job.append(text, 0.0, format);
    job.wrap = TextWrapping { max_width, max_rows: 1, break_anywhere: true, overflow_character: Some('\u{2026}') };
    ui.painter().layout_job(job)
}

fn presets_path() -> std::path::PathBuf {
    crate::files::app_dir().join("presets.json")
}

/// The most memory (bytes) an animation's frames take in Preview: past it they are smaller.
const PLAYED_MOST: f64 = 256e6;

/// An artwork's frames as read for Preview, and how they play.
struct PlayedFrames {
    pictures: Vec<sumi_core::ThumbnailImage>,
    order: Vec<u32>,
    fps: u32,
    playback: sumi_core::AnimationPlayback,
}

/// An animated artwork in Preview: being read, then played.
struct Played {
    dir: String,
    read: Option<std::sync::mpsc::Receiver<Option<PlayedFrames>>>,
    frames: Vec<TextureHandle>,
    order: Vec<u32>,
    fps: u32,
    playback: sumi_core::AnimationPlayback,
    at: u32,
    step: i32,
    due: Option<Instant>,
}

impl Played {
    /// Starts reading the artwork in `dir`, if it plays, at no more than `side` pixels.
    fn start(dir: String, side: u32) -> Played {
        let (done, read) = std::sync::mpsc::channel();
        let (from, plays) = (dir.clone(), sumi_core::artwork_plays(dir.clone()));
        if plays {
            std::thread::spawn(move || _ = done.send(read_frames(from, side)));
        }
        Played {
            dir,
            read: plays.then_some(read),
            frames: Vec::new(),
            order: Vec::new(),
            fps: 1,
            playback: sumi_core::AnimationPlayback::Loop,
            at: 0,
            step: 1,
            due: None,
        }
    }

    /// Takes in what was read, and moves on when the time has come.
    fn go_on(&mut self, ctx: &egui::Context) {
        if let Some(read) = &self.read {
            match read.try_recv() {
                Ok(frames) => {
                    self.read = None;
                    if let Some(f) = frames {
                        self.frames = f
                            .pictures
                            .iter()
                            .enumerate()
                            .map(|(i, t)| {
                                let size = [t.width as usize, t.height as usize];
                                let image = egui::ColorImage::from_rgba_unmultiplied(size, &t.rgba);
                                ctx.load_texture(format!("played {i}"), image, egui::TextureOptions::LINEAR)
                            })
                            .collect();
                        (self.order, self.fps, self.playback) = (f.order, f.fps.max(1), f.playback);
                        self.due = Some(Instant::now());
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(50)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.read = None,
            }
        }
        let Some(due) = self.due else { return };
        let frame = Duration::from_secs_f64(1.0 / f64::from(self.fps));
        let now = Instant::now();
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }
        let len = u32::try_from(self.order.len()).unwrap_or(0);
        match sumi_core::animation_next_step(self.playback, len, self.at, self.step) {
            Some(next) => {
                (self.at, self.step) = (next.at, next.step);
                self.due = Some(now + frame);
                ctx.request_repaint_after(frame);
            }
            // a single pass stays on its last frame
            None => self.due = None,
        }
    }

    /// The frame showing, once there are frames.
    fn frame(&self) -> Option<egui::TextureId> {
        let index = *self.order.get(self.at as usize)? as usize;
        self.frames.get(index).map(TextureHandle::id)
    }
}

/// The frames of the artwork in `dir`, at no more than `side` pixels and [`PLAYED_MOST`] together.
fn read_frames(dir: String, side: u32) -> Option<PlayedFrames> {
    let session = sumi_core::CanvasSession::open(dir).ok()?;
    // a model goes round
    if session.model_info().is_some() {
        if !session.draw_without_window() {
            return None;
        }
        let count = session.model_preview_frames();
        let fit = (PLAYED_MOST / 4.0 / f64::from(count.max(1))).sqrt() as u32;
        let pictures: Vec<_> = (0..count).filter_map(|i| session.model_preview_picture(i, side.min(fit))).collect();
        let order = (0..pictures.len() as u32).collect();
        let (fps, playback) = (session.model_preview_fps(), sumi_core::AnimationPlayback::Loop);
        return Some(PlayedFrames { pictures, order, fps, playback });
    }
    let settings = session.animation_settings().filter(|s| !s.pages)?;
    if !session.draw_without_window() {
        return None;
    }
    let count = session.animation_frame_count();
    let fit = (PLAYED_MOST / 4.0 / f64::from(count.max(1))).sqrt() as u32;
    let pictures: Vec<_> = (0..count).filter_map(|i| session.animation_frame(i, false, side.min(fit))).collect();
    Some(PlayedFrames { pictures, order: session.animation_sequence(), fps: settings.fps, playback: settings.playback })
}

/// What the gallery asks its reader's thread for.
enum ReaderAsk {
    /// An artwork's thumbnail: its folder, when it was changed and its picture's file.
    Thumbnail(String, i64, std::path::PathBuf),
    /// Whether the artwork in this folder has a time-lapse recorded, when it was changed, and the
    /// refresh that asked.
    TimeLapse(String, i64, u64),
}

/// A thread reading the gallery's thumbnails and looking for its time-lapses one after another;
/// each answer comes back as it is read.
struct ThumbnailReader {
    asks: std::sync::mpsc::Sender<ReaderAsk>,
    read: std::sync::mpsc::Receiver<(String, i64, Option<egui::ColorImage>)>,
    recorded: std::sync::mpsc::Receiver<(String, i64, u64, crate::timelapse::Recorded)>,
}

impl ThumbnailReader {
    fn start() -> ThumbnailReader {
        let (asks, asked) = std::sync::mpsc::channel::<ReaderAsk>();
        let (send, read) = std::sync::mpsc::channel();
        let (record, recorded) = std::sync::mpsc::channel();
        // it ends when the gallery lets go of it
        let _ = std::thread::Builder::new().name("thumbnails".into()).spawn(move || {
            for ask in asked {
                match ask {
                    ReaderAsk::Thumbnail(dir, when, path) => {
                        let image = crate::files::read_png(&path)
                            .ok()
                            .filter(|image| image.rgba.len() == image.width as usize * image.height as usize * 4)
                            .map(|image| {
                                let size = [image.width as usize, image.height as usize];
                                egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba)
                            });
                        if send.send((dir, when, image)).is_err() {
                            return;
                        }
                    }
                    ReaderAsk::TimeLapse(dir, when, generation) => {
                        let found = crate::timelapse::recorded(&dir);
                        if record.send((dir, when, generation, found)).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        ThumbnailReader { asks, read, recorded }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Color32, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Stroke, pos2, vec2};

    use super::{
        Asked, FORMAT_ROW, Gallery, HEADER, SHARE_ROWS, START_ROW_HOVER, ShareRow, TIME_LAPSE_PILL, Weight, argb,
        formats_beside, one_line, paper_swatch_rim, retain_live_thumbs, share_group_height, share_panel_height,
        start_clamped, start_paper_swatches, start_preset, start_section_y, start_size_layout,
    };

    /// The first cell of the grid at this window size, as the gallery lays it out.
    fn first_cell(width: f32, height: f32) -> Rect {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height));
        let grid = Gallery::grid_rect(screen, false);
        let columns = if grid.width() > grid.height() + HEADER { 4 } else { 3 };
        Rect::from_min_size(grid.min, vec2(grid.width() / columns as f32, grid.width() / columns as f32))
    }

    /// Both native documents the app now writes are offered beside the PSD, named as the
    /// Actions panel names them (the rows show [`crate::files::Format::name`]).
    #[test]
    fn share_rows_offer_the_native_clip_studio_and_krita_documents_beside_psd() {
        let stills: Vec<crate::files::Format> = SHARE_ROWS
            .iter()
            .filter_map(|row| match *row {
                ShareRow::Still(format) => Some(format),
                ShareRow::Animated(_) => None,
            })
            .collect();
        let psd = stills.iter().position(|f| *f == crate::files::Format::Psd).expect("the PSD is shared");
        assert_eq!(stills.get(psd + 1).copied(), Some(crate::files::Format::Clip));
        assert_eq!(stills.get(psd + 2).copied(), Some(crate::files::Format::Kra));
        assert_eq!(ShareRow::Still(crate::files::Format::Clip).name(), "CLIP STUDIO (.clip)");
        assert_eq!(ShareRow::Still(crate::files::Format::Kra).name(), "Krita (.kra)");
    }

    /// The two lines under the native documents are part of the panel's height: the formats list
    /// is one row per format, one line between each two, and one under each native document.
    #[test]
    fn share_formats_leave_room_for_what_the_native_documents_cannot_hold() {
        let rows = SHARE_ROWS.len() as f32;
        let without_notes = 55.0 + FORMAT_ROW * rows + (rows - 1.0) + 10.0;
        assert_eq!(share_panel_height(), without_notes + 2.0 * super::FORMAT_NOTE);
        assert_eq!(share_group_height(), share_panel_height() - 65.0);
    }

    /// Only the two native documents say what they cannot hold: every other format holds the
    /// artwork whole, so their rows carry no line.
    #[test]
    fn only_the_native_documents_carry_a_preflight_line() {
        let notes = super::ShareNotes {
            clip: crate::files::ClipChanges::Kept(Vec::new()),
            kra: crate::files::ClipChanges::Refused("Krita cannot hold the clipping".to_owned()),
        };
        assert_eq!(ShareRow::Still(crate::files::Format::Clip).note(&notes).as_deref(), Some("0 changes"));
        assert_eq!(
            ShareRow::Still(crate::files::Format::Kra).note(&notes).as_deref(),
            Some("Krita cannot hold the clipping")
        );
        assert!(ShareRow::Still(crate::files::Format::Psd).note(&notes).is_none());
        assert!(ShareRow::Animated(crate::animated_export::Animated::Gif).note(&notes).is_none());
    }

    /// A native document the pre-flight refused is shown but cannot be tapped to share; one it can
    /// write, and every other format, stay choosable.
    #[test]
    fn a_refused_native_document_cannot_be_chosen_to_share() {
        let notes = super::ShareNotes {
            clip: crate::files::ClipChanges::Refused("cannot hold the clipping".to_owned()),
            kra: crate::files::ClipChanges::Kept(Vec::new()),
        };
        assert!(!ShareRow::Still(crate::files::Format::Clip).actionable(&notes), "a refused .clip row is a no-op");
        assert!(ShareRow::Still(crate::files::Format::Kra).actionable(&notes), "an accepted .kra row can be chosen");
        assert!(ShareRow::Still(crate::files::Format::Psd).actionable(&notes), "other formats are always choosable");
    }

    #[test]
    fn share_formats_go_where_there_is_room_and_point_at_the_pill() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let size = vec2(260.0, 515.0);
        // a pill on the left: the list on its right, its arrow at the pill's middle
        let pill = Rect::from_min_size(pos2(40.0, 300.0), vec2(94.0, 56.0));
        let (rect, arrow_y, right) = formats_beside(pill, screen, size);
        assert!(right);
        assert_eq!(rect.left(), pill.right() + 2.0);
        assert!((rect.top() + arrow_y - pill.center().y).abs() < 0.01);
        // a pill near the right edge: on its left, its right end 2 off the pill
        let pill = Rect::from_min_size(pos2(1300.0, 300.0), vec2(94.0, 56.0));
        let (rect, _, right) = formats_beside(pill, screen, size);
        assert!(!right);
        assert!((rect.right() - (pill.left() - 2.0)).abs() < 0.01);
        // a pill at the very top: kept 2 inside, the arrow no nearer its end than the corner allows
        let pill = Rect::from_min_size(pos2(40.0, 0.0), vec2(94.0, 20.0));
        let (rect, arrow_y, _) = formats_beside(pill, screen, size);
        assert_eq!(rect.top(), 2.0);
        assert!(arrow_y >= crate::panel::POPOVER_CORNER + crate::panel::ARROW_HEIGHT);
    }

    /// A directory of its own for a test, under the system's temporary directory (the build sets
    /// it inside the HDD), made unique by the process and the clock.
    fn scratch(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("sumi-gallery-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn artwork_entry(dir: &std::path::Path, name: &str) -> sumi_core::GalleryEntry {
        sumi_core::GalleryEntry::Artwork {
            artwork: sumi_core::GalleryArtwork {
                dir: dir.to_string_lossy().into_owned(),
                name: name.to_owned(),
                width: 100,
                height: 100,
                modified_ms: 1,
            },
        }
    }

    fn gallery_of(root: &std::path::Path, entry: sumi_core::GalleryEntry) -> Gallery {
        let mut gallery =
            Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
        gallery.entries = vec![entry];
        gallery
    }

    /// An artwork taken out of the gallery gives up its cached thumbnail, while one still listed
    /// keeps its texture.
    #[test]
    fn a_deleted_artwork_gives_up_its_cached_thumbnail() {
        let mut thumbs: std::collections::HashMap<String, ()> =
            ["kept", "gone"].into_iter().map(|dir| (dir.to_owned(), ())).collect();
        let entries = vec![artwork_entry(std::path::Path::new("kept"), "kept")];
        retain_live_thumbs(&mut thumbs, &entries);
        let live: std::collections::HashSet<_> = thumbs.keys().map(String::as_str).collect();
        assert_eq!(live, std::collections::HashSet::from(["kept"]));
    }

    /// Runs one headless frame of the gallery's page; returns what the frame asked for.
    fn frame(
        gallery: &mut Gallery,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        events: Vec<Event>,
        time: &mut f64,
    ) -> Option<super::Asked> {
        *time += 1.0 / 60.0;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            time: Some(*time),
            events,
            ..RawInput::default()
        };
        let mut asked = None;
        let mut out = ctx.run_ui(raw, |ui| {
            asked = gallery.page(ui.ctx(), look, egui::Order::Background, 1.0, false);
        });
        // the fonts' and pictures' uploads: these tests read what the page asked, not the textures
        out.textures_delta.clear();
        asked
    }

    fn press(at: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    fn deck_frame(
        gallery: &mut Gallery,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        screen: Rect,
        events: Vec<Event>,
        time: &mut f64,
    ) -> Option<Asked> {
        *time += 1.0 / 60.0;
        let raw = RawInput { screen_rect: Some(screen), time: Some(*time), events, ..RawInput::default() };
        let mut asked = None;
        let mut out = ctx.run_ui(raw, |ui| {
            asked = gallery.page(ui.ctx(), look, egui::Order::Background, 1.0, false);
        });
        out.textures_delta.clear();
        asked
    }

    fn deck_paint(
        gallery: &mut Gallery,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        screen: Rect,
        events: Vec<Event>,
        time: &mut f64,
    ) -> egui::FullOutput {
        *time += 1.0 / 60.0;
        let raw = RawInput { screen_rect: Some(screen), time: Some(*time), events, ..RawInput::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = gallery.page(ui.ctx(), look, egui::Order::Background, 1.0, false);
        });
        out.textures_delta.clear();
        out
    }

    /// Every circle a frame painted: its centre, its radius, its fill and its ring.
    fn circles(output: &egui::FullOutput) -> Vec<(Pos2, f32, Color32, Stroke)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(Pos2, f32, Color32, Stroke)>) {
            match shape {
                egui::Shape::Circle(circle) => out.push((circle.center, circle.radius, circle.fill, circle.stroke)),
                egui::Shape::Vec(items) => items.iter().for_each(|item| walk(item, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    fn click_at(
        gallery: &mut Gallery,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        screen: Rect,
        time: &mut f64,
        at: egui::Pos2,
    ) -> Option<Asked> {
        let mut asked = None;
        for events in [
            // A frame with the field laid out before the press: its widget must exist to take it.
            Vec::new(),
            vec![Event::PointerMoved(at)],
            vec![press(at, true)],
            vec![press(at, false)],
            Vec::new(),
        ] {
            asked = deck_frame(gallery, ctx, look, screen, events, time).or(asked);
        }
        asked
    }

    #[test]
    fn gallery_open_import_and_photo_buttons_dispatch_the_correct_file_request() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        // Header controls remain wired on the Start Deck as well as in the gallery.
        for (label, at) in [
            ("Open an existing artwork", pos2(210.0, 574.0)),
            ("Import", pos2(1261.0, 34.0)),
            ("Photo", pos2(1328.0, 34.0)),
        ] {
            let root = scratch("picker-action");
            let mut gallery =
                Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
            let (ctx, mut look, mut time) = (egui::Context::default(), crate::look::Look::default(), 0.0);
            crate::look::install_fonts(&ctx);
            let asked = click_at(&mut gallery, &ctx, &mut look, screen, &mut time, at);
            match label {
                "Photo" => assert!(matches!(asked, Some(Asked::Photo(_))), "{label} did not ask for a photo"),
                _ => assert!(matches!(asked, Some(Asked::Import(_))), "{label} did not ask for import"),
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    fn key(key: egui::Key) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    fn type_bad(
        gallery: &mut Gallery,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        screen: Rect,
        time: &mut f64,
        bad: &str,
    ) {
        let typed = if bad.is_empty() { vec![key(egui::Key::Backspace)] } else { vec![Event::Text(bad.to_owned())] };
        deck_frame(gallery, ctx, look, screen, typed, time);
        assert!(gallery.start_edit.as_ref().is_some_and(|e| !e.valid), "the field keeps the invalid text {bad:?}");
    }

    #[test]
    fn start_size_fields_type_exact_pixels_and_create_that_size() {
        use egui::{Event, Key, Modifiers, PointerButton};

        let root = scratch("deck-size");
        let mut gallery =
            Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
        gallery.start_size = (3000, 2000);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let (width_box, height_box, create) = start_size_layout(screen);
        let (ctx, mut look, mut time) = (egui::Context::default(), crate::look::Look::default(), 0.0);
        crate::look::install_fonts(&ctx);
        let press = |at: egui::Pos2, pressed: bool| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut frame = |gallery: &mut Gallery, events| deck_frame(gallery, &ctx, &mut look, screen, events, &mut time);
        // Clicking a field edits it in place: no Custom Canvas, no editor.
        frame(&mut gallery, Vec::new());
        frame(&mut gallery, vec![Event::PointerMoved(width_box.center())]);
        frame(&mut gallery, vec![press(width_box.center(), true)]);
        frame(&mut gallery, vec![press(width_box.center(), false)]);
        assert!(gallery.custom.is_none(), "editing the size must not open Custom Canvas");
        frame(&mut gallery, Vec::new());
        frame(&mut gallery, vec![Event::Text("1920".to_owned())]);
        assert_eq!(gallery.start_size.0, 1920);
        // The other field, then Enter commits it.
        frame(&mut gallery, vec![Event::PointerMoved(height_box.center())]);
        frame(&mut gallery, vec![press(height_box.center(), true)]);
        frame(&mut gallery, vec![press(height_box.center(), false)]);
        frame(&mut gallery, Vec::new());
        frame(&mut gallery, vec![Event::Text("1080".to_owned())]);
        frame(
            &mut gallery,
            vec![Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(gallery.start_size, (1920, 1080));
        // Create asks for exactly those pixels.
        frame(&mut gallery, vec![Event::PointerMoved(create.center())]);
        frame(&mut gallery, vec![press(create.center(), true)]);
        let asked = frame(&mut gallery, vec![press(create.center(), false)]);
        let Some(Asked::New(preset, _)) = asked else {
            panic!("Create did not ask for a new canvas");
        };
        assert_eq!((preset.size_width, preset.size_height), (1920.0, 1080.0));
        assert_eq!((sumi_core::preset_width(preset.clone()), sumi_core::preset_height(preset)), (1920, 1080));
    }

    #[test]
    fn invalid_start_size_text_creates_nothing_and_escape_restores() {
        let root = scratch("deck-invalid");
        let mut gallery =
            Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let (width, _, create) = start_size_layout(screen);
        let (ctx, mut look, mut time) = (egui::Context::default(), crate::look::Look::default(), 0.0);
        crate::look::install_fonts(&ctx);
        for bad in ["abc", "", "99999999999"] {
            // Escape while the field is focused restores what it held before.
            gallery.start_size = (3000, 2000);
            gallery.start_edit = None;
            click_at(&mut gallery, &ctx, &mut look, screen, &mut time, width.center());
            type_bad(&mut gallery, &ctx, &mut look, screen, &mut time, bad);
            deck_frame(&mut gallery, &ctx, &mut look, screen, vec![key(egui::Key::Escape)], &mut time);
            assert_eq!(gallery.start_size, (3000, 2000), "Escape restores the value from before");
            assert!(gallery.start_edit.is_none(), "Escape leaves the field");
            // Clicking Create while invalid asks for nothing, and leaves the old size.
            gallery.start_size = (3000, 2000);
            gallery.start_edit = None;
            click_at(&mut gallery, &ctx, &mut look, screen, &mut time, width.center());
            type_bad(&mut gallery, &ctx, &mut look, screen, &mut time, bad);
            let asked = click_at(&mut gallery, &ctx, &mut look, screen, &mut time, create.center());
            assert!(asked.is_none(), "Create must not make a canvas from {bad:?}");
            assert_eq!(gallery.start_size, (3000, 2000), "invalid input leaves the old size");
        }
    }

    #[test]
    fn start_size_widget_clamps_zero_and_the_edge_on_commit() {
        let root = scratch("deck-edges");
        let mut gallery =
            Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
        gallery.start_size = (3000, 2000);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let (width, height, create) = start_size_layout(screen);
        let (ctx, mut look, mut time) = (egui::Context::default(), crate::look::Look::default(), 0.0);
        crate::look::install_fonts(&ctx);
        click_at(&mut gallery, &ctx, &mut look, screen, &mut time, width.center());
        deck_frame(&mut gallery, &ctx, &mut look, screen, vec![Event::Text("0".to_owned())], &mut time);
        assert!(gallery.start_edit.as_ref().is_some_and(|e| !e.valid));
        deck_frame(&mut gallery, &ctx, &mut look, screen, vec![key(egui::Key::Enter)], &mut time);
        assert_eq!(gallery.start_size.0, 1, "zero commits as the core's least");
        let max = start_clamped(u32::MAX);
        click_at(&mut gallery, &ctx, &mut look, screen, &mut time, height.center());
        deck_frame(&mut gallery, &ctx, &mut look, screen, vec![Event::Text("99999".to_owned())], &mut time);
        assert!(gallery.start_edit.as_ref().is_some_and(|e| !e.valid));
        deck_frame(&mut gallery, &ctx, &mut look, screen, vec![key(egui::Key::Enter)], &mut time);
        assert_eq!(gallery.start_size.1, max, "a huge number commits at the core's edge");
        let asked = click_at(&mut gallery, &ctx, &mut look, screen, &mut time, create.center());
        let Some(Asked::New(preset, _)) = asked else { panic!("Create did not make the clamped canvas") };
        assert_eq!((preset.size_width, preset.size_height), (1.0, max as f32));
    }

    #[test]
    fn start_size_fields_keep_the_cores_bounds() {
        assert_eq!(start_clamped(0), 1);
        assert_eq!(start_clamped(1920), 1920);
        let huge = 99_999;
        let max = sumi_core::preset_width(start_preset(huge, 1));
        assert_eq!(start_clamped(huge), max);
        assert_eq!(sumi_core::preset_height(start_preset(1, huge)), max);
    }

    #[test]
    fn start_deck_paper_swatch_tints_created_canvas_without_shifting_size_fields() {
        let root = scratch("deck-paper-swatch");
        let mut gallery =
            Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
        gallery.start_size = (3000, 2000);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let (width_box, height_box, create) = start_size_layout(screen);
        let header_y = start_section_y(screen);
        let kraft = start_paper_swatches(screen, header_y)[3];
        assert!(width_box.top() > header_y && height_box.top() > header_y);

        let (ctx, mut look, mut time) = (egui::Context::default(), crate::look::Look::default(), 0.0);
        crate::look::install_fonts(&ctx);
        click_at(&mut gallery, &ctx, &mut look, screen, &mut time, kraft.0.center());
        assert_eq!(gallery.start_paper, 0xD8_C6AE);
        assert_eq!(gallery.start_size, (3000, 2000), "picking paper must keep custom dimensions");

        let asked = click_at(&mut gallery, &ctx, &mut look, screen, &mut time, create.center());
        let Some(Asked::New(preset, _)) = asked else {
            panic!("Create did not ask for a new canvas after paper selection");
        };
        assert_eq!(preset.background, 0xD8_C6AE);
        assert_eq!((preset.size_width, preset.size_height), (3000.0, 2000.0));
    }

    /// The paper swatches answer the pointer with the deck's own row-hover plate behind the paper,
    /// and the chosen paper keeps its accent ring while the pointer passes the others.
    #[test]
    fn start_deck_paper_swatches_answer_the_pointer_without_moving_the_chosen_paper() {
        let root = scratch("deck-paper-hover");
        let mut gallery =
            Gallery::with_store(sumi_core::ArtworkStore::open(root.to_string_lossy().into_owned()), Vec::new());
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let header_y = start_section_y(screen);
        let swatches = start_paper_swatches(screen, header_y);
        let (ctx, mut look, mut time) = (egui::Context::default(), crate::look::Look::default(), 0.0);
        crate::look::install_fonts(&ctx);
        let previous = crate::look::light();
        let mut seen = Vec::new();
        for light in [false, true] {
            crate::look::set_light(light);
            // the deck lays itself out before the pointer arrives
            for _ in 0..2 {
                let _ = deck_paint(&mut gallery, &ctx, &mut look, screen, Vec::new(), &mut time);
            }
            let over = circles(&deck_paint(
                &mut gallery,
                &ctx,
                &mut look,
                screen,
                vec![Event::PointerMoved(swatches[2].0.center())],
                &mut time,
            ));
            // egui keeps the pointer's last `interact_pos` through the frame it goes away: the
            // first PointerGone frame still hit-tests where the pointer was. Flush it, then read.
            let _ = deck_paint(&mut gallery, &ctx, &mut look, screen, vec![Event::PointerGone], &mut time);
            let away = circles(&deck_paint(&mut gallery, &ctx, &mut look, screen, vec![Event::PointerGone], &mut time));
            seen.push((light, over, away, argb(START_ROW_HOVER), paper_swatch_rim(true), paper_swatch_rim(false)));
        }
        // Never leave the global look swapped for another test, even when asserting.
        crate::look::set_light(previous);
        assert_eq!(gallery.start_paper, 0xFF_FFFF, "the pointer over the swatches changed the chosen paper");
        // the plate sits just outside the paper's rim, so the swatch keeps its size and its ring
        let plate = |circles: &[(Pos2, f32, Color32, Stroke)], at: Pos2| {
            circles
                .iter()
                .find(|(centre, radius, _, _)| (*centre - at).length() < 0.5 && (*radius - 10.5).abs() < 0.01)
                .map(|(_, _, fill, _)| *fill)
        };
        let rim = |circles: &[(Pos2, f32, Color32, Stroke)], at: Pos2| {
            circles
                .iter()
                .find(|(centre, radius, _, stroke)| {
                    (*centre - at).length() < 0.5 && (*radius - 8.0).abs() < 0.01 && stroke.width > 0.0
                })
                .map(|(_, _, _, stroke)| *stroke)
        };
        for (light, over, away, hover_plate, chosen_rim, idle_rim) in seen {
            let (chosen, passed) = (swatches[0].0.center(), swatches[2].0.center());
            assert_eq!(
                plate(&over, passed),
                Some(hover_plate),
                "the swatch under the pointer showed no plate ({light:?})"
            );
            assert_eq!(plate(&over, chosen), None, "the plate spread to the chosen paper ({light:?})");
            assert_eq!(plate(&away, passed), None, "the plate stayed with no pointer on it ({light:?})");
            assert_eq!(rim(&over, chosen), Some(chosen_rim), "hover moved the chosen paper's ring ({light:?})");
            assert_eq!(rim(&away, chosen), Some(chosen_rim), "the chosen paper lost its ring ({light:?})");
            assert_eq!(rim(&over, passed), Some(idle_rim), "a passed-over swatch rimmed as the chosen one ({light:?})");
            assert_ne!(chosen_rim, idle_rim, "chosen and idle swatches rimmed alike ({light:?})");
        }
    }

    #[test]
    fn replay_action_is_part_of_the_thumbnail_and_fits_a_160_thumbnail() {
        for (width, height) in [(1440.0, 900.0), (800.0, 600.0)] {
            let cell = first_cell(width, height);
            let thumbnail = Gallery::thumb_box(cell);
            let pill = Gallery::time_lapse_pill(cell);
            assert!((pill.width() - 106.0).abs() < 0.01, "{width}x{height}: {}", pill.width());
            assert!((pill.height() - 30.0).abs() < 0.01, "{width}x{height}: {}", pill.height());
            assert!(pill.top() > thumbnail.top(), "{width}x{height}");
            assert!(pill.bottom() < thumbnail.bottom(), "{width}x{height}");
            assert!(pill.left() > thumbnail.left() && pill.right() < thumbnail.right(), "{width}x{height}");
            assert!(pill.width() <= thumbnail.width(), "{width}x{height}");
        }
        // at 800 x 600 the cells are four across: 200 wide, so the thumbnail is 160
        assert!((Gallery::thumb_box(first_cell(800.0, 600.0)).width() - 160.0).abs() < 0.01);
    }

    #[test]
    fn replay_action_keeps_inside_very_small_thumbnail_without_leaking_into_adjacent_cells() {
        let cell = first_cell(480.0, 300.0);
        let thumbnail = Gallery::thumb_box(cell);
        assert_eq!(thumbnail.width(), 80.0);
        let replay = Gallery::time_lapse_pill(cell);
        assert!(replay.width() < TIME_LAPSE_PILL.0);
        assert!(thumbnail.contains(replay.min), "top-left exceeds thumbnail: {replay:?}");
        assert!(thumbnail.contains(replay.max), "bottom-right exceeds thumbnail: {replay:?}");
    }

    #[test]
    fn short_wide_windows_use_the_regular_gallery_instead_of_clipping_start_controls() {
        let short = Rect::from_min_size(pos2(0.0, 0.0), vec2(1040.0, 450.0));
        let tall = Rect::from_min_size(pos2(0.0, 0.0), vec2(1040.0, 600.0));
        assert_eq!(Gallery::grid_rect(short, false).left(), short.left());
        assert_eq!(Gallery::grid_rect(tall, false).left(), tall.left() + super::START_DECK_WIDTH);
        assert!(super::start_size_layout(tall).2.bottom() < tall.bottom());
    }

    #[test]
    fn the_time_lapse_pill_s_words_fit_beside_its_clock() {
        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut width = 0.0;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(raw, |ui| {
            width = one_line(ui, "Replay", 11.0, Weight::SemiBold, Color32::WHITE, f32::INFINITY).size().x;
        });
        out.textures_delta.clear();
        // the clock starts 10 in and the words 24 in, with 8 to spare at the pill's end
        assert!(24.0 + width <= TIME_LAPSE_PILL.0 - 8.0, "the words are {width} wide");
    }

    #[test]
    fn clicking_the_time_lapse_pill_opens_the_player_and_never_the_canvas() {
        let root = scratch("pill-click");
        let art = root.join("artwork-a");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        std::fs::write(lapse.join("frame-000000.jpg"), b"not read here").unwrap();
        let entry = artwork_entry(&art, "Artwork A");
        let dir = art.to_string_lossy().into_owned();
        let mut gallery = gallery_of(&root, entry);
        let (ctx, mut look) = (egui::Context::default(), crate::look::Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        // the time-lapse is looked for off this thread: wait for its answer, as the gallery does
        let mut found = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            gallery.ensure_time_lapses(&ctx);
            gallery.time_lapses_read(&ctx);
            frame(&mut gallery, &ctx, &mut look, Vec::new(), &mut time);
            if gallery.lapses.get(&dir).is_some_and(|(_, _, recorded)| *recorded) {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(found, "the artwork's time-lapse was not found");
        let cell = first_cell(800.0, 600.0);
        let at = Gallery::time_lapse_pill(cell).center();
        let mut asked = None;
        for step in 0..6 {
            let events = match step {
                2 => vec![Event::PointerMoved(at)],
                3 => vec![press(at, true)],
                4 => vec![press(at, false)],
                _ => Vec::new(),
            };
            asked = frame(&mut gallery, &ctx, &mut look, events, &mut time).or(asked);
        }
        assert!(gallery.player.is_some(), "a click on the pill opens the replay");
        assert!(asked.is_none(), "a click on the pill never opens the canvas");
        assert!(gallery.renaming.is_none(), "a click on the pill never renames");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn selecting_hides_the_pill_so_a_tap_there_picks_the_artwork() {
        let root = scratch("pill-selecting");
        let art = root.join("artwork-a");
        std::fs::create_dir_all(art.join("timelapse")).unwrap();
        let entry = artwork_entry(&art, "Artwork A");
        let dir = art.to_string_lossy().into_owned();
        let mut gallery = gallery_of(&root, entry);
        gallery.selecting = true;
        gallery.lapses.insert(dir.clone(), (1, gallery.lapse_generation, true));
        let (ctx, mut look) = (egui::Context::default(), crate::look::Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let at = Gallery::time_lapse_pill(first_cell(800.0, 600.0)).center();
        for step in 0..6 {
            let events = match step {
                2 => vec![Event::PointerMoved(at)],
                3 => vec![press(at, true)],
                4 => vec![press(at, false)],
                _ => Vec::new(),
            };
            frame(&mut gallery, &ctx, &mut look, events, &mut time);
        }
        assert!(gallery.player.is_none(), "selecting shows no pill to open");
        assert!(gallery.picked.contains(&dir), "the tap picks the artwork instead");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A small JPEG at `path`, all one colour.
    fn write_jpeg(path: &std::path::Path) {
        let rgb = vec![127u8; 8 * 8 * 3];
        image::save_buffer_with_format(path, &rgb, 8, 8, image::ExtendedColorType::Rgb8, image::ImageFormat::Jpeg)
            .unwrap();
    }

    /// A press at `from`, a drag to `to`, and the release; returns what the last frame asked for.
    fn drag(
        gallery: &mut Gallery,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        time: &mut f64,
        from: egui::Pos2,
        to: egui::Pos2,
    ) -> Option<super::Asked> {
        let middle = pos2((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
        let touch = |phase, pos| Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(1),
            phase,
            pos,
            force: None,
        };
        let batches = [
            vec![Event::PointerMoved(from)],
            vec![touch(egui::TouchPhase::Start, from), press(from, true)],
            vec![touch(egui::TouchPhase::Move, middle), Event::PointerMoved(middle)],
            vec![touch(egui::TouchPhase::Move, to), Event::PointerMoved(to)],
            vec![touch(egui::TouchPhase::End, to), press(to, false), Event::PointerGone],
        ];
        // a frame to lay the gallery out, as it is before any press in the app
        frame(gallery, ctx, look, Vec::new(), time);
        let mut asked = None;
        for events in batches {
            asked = frame(gallery, ctx, look, events, time).or(asked);
        }
        asked
    }

    #[test]
    fn a_press_on_the_pill_dragged_off_it_never_swipes_or_opens_the_cell() {
        let root = scratch("pill-drag-off");
        let art = root.join("artwork-a");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        write_jpeg(&lapse.join("frame-000000.jpg"));
        let entry = artwork_entry(&art, "Artwork A");
        let mut gallery = gallery_of(&root, entry.clone());
        gallery.lapses.insert(art.to_string_lossy().into_owned(), (1, gallery.lapse_generation, true));
        let (ctx, mut look) = (egui::Context::default(), crate::look::Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let pill = Gallery::time_lapse_pill(first_cell(800.0, 600.0));
        let asked = drag(&mut gallery, &ctx, &mut look, &mut time, pill.center(), pill.center() - vec2(60.0, 0.0));
        assert!(asked.is_none(), "a press on the pill never opens the canvas");
        assert!(gallery.player.is_none(), "a drag off the pill is no replay");
        assert!(gallery.swiped.is_none(), "a press on the pill never swipes the cell");
        assert!(gallery.renaming.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_cell_swipe_that_ends_over_the_pill_still_opens_its_actions() {
        let root = scratch("cell-drag-over-pill");
        let art = root.join("artwork-a");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        write_jpeg(&lapse.join("frame-000000.jpg"));
        let entry = artwork_entry(&art, "Artwork A");
        let dir = art.to_string_lossy().into_owned();
        let mut gallery = gallery_of(&root, entry.clone());
        gallery.lapses.insert(dir.clone(), (1, gallery.lapse_generation, true));
        let (ctx, mut look) = (egui::Context::default(), crate::look::Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let pill = Gallery::time_lapse_pill(first_cell(800.0, 600.0));
        let from = pos2(pill.center().x, pill.bottom() + 7.0);
        let to = pos2(pill.left() + 10.0, pill.center().y);
        let _ = drag(&mut gallery, &ctx, &mut look, &mut time, from, to);
        assert_eq!(
            gallery.swiped.as_deref(),
            Some(dir.as_str()),
            "the cell's swipe still shows its actions: swiped {:?}, dragged {:?}, pill {pill:?}, from {from:?}, to {to:?}, lifted {:?}, pill press {:?}",
            gallery.swiped,
            gallery.dragged,
            gallery.lifted,
            gallery.pill_press
        );
        assert!(gallery.player.is_none(), "a cell swipe that happens to end on the pill is no replay");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn queueing_a_time_lapse_look_asks_for_another_frame() {
        let root = scratch("lapse-repaint");
        let art = root.join("artwork-a");
        std::fs::create_dir_all(art.join("timelapse")).unwrap();
        let mut gallery = gallery_of(&root, artwork_entry(&art, "Artwork A"));
        let ctx = egui::Context::default();
        let raw = || RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            ..RawInput::default()
        };
        // settle whatever a first frame asks for on its own
        let _ = ctx.run_ui(raw(), |_| {}).textures_delta.clear();
        let _ = ctx.run_ui(raw(), |_| {}).textures_delta.clear();
        // the gallery asks inside its frame, as show() does
        let mut output = ctx.run_ui(raw(), |ui| gallery.ensure_time_lapses(ui.ctx()));
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(std::time::Duration::MAX, |viewport| viewport.repaint_delay);
        output.textures_delta.clear();
        assert!(delay <= std::time::Duration::from_millis(30), "an idle gallery waits {delay:?} for its look");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_answer_asked_for_before_a_refresh_is_ignored() {
        let root = scratch("lapse-generation");
        let art = root.join("artwork-a");
        std::fs::create_dir_all(art.join("timelapse")).unwrap();
        let entry = artwork_entry(&art, "Artwork A");
        let dir = art.to_string_lossy().into_owned();
        let mut gallery = gallery_of(&root, entry.clone());
        let (asks, _asks) = std::sync::mpsc::channel();
        let (_thumbnails, thumbnails) = std::sync::mpsc::channel();
        let (answers, recorded) = std::sync::mpsc::channel();
        gallery.reader = Some(super::ThumbnailReader { asks, read: thumbnails, recorded });
        let ctx = egui::Context::default();
        let old_generation = gallery.lapse_generation;
        gallery.ensure_time_lapses(&ctx);
        answers.send((dir.clone(), 1, old_generation, crate::timelapse::Recorded::Yes)).unwrap();
        gallery.time_lapses_read(&ctx);
        assert!(gallery.lapses.get(&dir).is_some_and(|(_, _, recorded)| *recorded));
        gallery.refresh();
        gallery.entries = vec![entry.clone()];
        gallery.ensure_time_lapses(&ctx);
        // an answer asked for before the refresh, for the very same folder and time
        answers.send((dir.clone(), 1, old_generation, crate::timelapse::Recorded::Yes)).unwrap();
        gallery.time_lapses_read(&ctx);
        assert!(!gallery.lapses.contains_key(&dir), "an old answer is not taken");
        assert!(gallery.lapse_reading.contains(&(dir.clone(), 1)), "the new look is still waited for");
        let generation = gallery.lapse_generation;
        answers.send((dir.clone(), 1, generation, crate::timelapse::Recorded::No)).unwrap();
        gallery.time_lapses_read(&ctx);
        assert_eq!(gallery.lapses.get(&dir).map(|(_, _, recorded)| *recorded), Some(false));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_time_lapse_being_packaged_is_replayed_only_after_packaging_ends() {
        let root = scratch("lapse-packaging");
        let art = root.join("artwork-a");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        for i in 0..3 {
            write_jpeg(&lapse.join(format!("frame-{i:06}.jpg")));
        }
        let entry = artwork_entry(&art, "Artwork A");
        let dir = art.to_string_lossy().into_owned();
        let mut gallery = gallery_of(&root, entry);
        let (ctx, mut look) = (egui::Context::default(), crate::look::Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let packaging = crate::timelapse::hold_packaging(&dir);
        let mut retried = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            gallery.ensure_time_lapses(&ctx);
            gallery.time_lapses_read(&ctx);
            frame(&mut gallery, &ctx, &mut look, Vec::new(), &mut time);
            if gallery.lapse_waiting.contains_key(&dir) {
                retried = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(retried, "a time-lapse being packaged is looked at again");
        assert!(!gallery.lapses.get(&dir).is_some_and(|(_, _, recorded)| *recorded), "no replay while packaging");
        drop(packaging);
        let mut found = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while std::time::Instant::now() < deadline {
            gallery.ensure_time_lapses(&ctx);
            gallery.time_lapses_read(&ctx);
            frame(&mut gallery, &ctx, &mut look, Vec::new(), &mut time);
            if gallery.lapses.get(&dir).is_some_and(|(_, _, recorded)| *recorded) {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        assert!(found, "once packaging ends the time-lapse is found");
        let mut player = crate::timelapse::TimeLapsePlayer::for_gallery(&dir);
        for _ in 0..300 {
            time += 1.0 / 60.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                time: Some(time),
                ..RawInput::default()
            };
            let _ = ctx.run_ui(raw, |ui| _ = player.show(ui.ctx(), &mut look)).textures_delta.clear();
            if player.showing() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(player.showing(), "the pictures are read after packaging");
        assert!(player.message().is_none(), "no unreadable message after packaging");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
