//! Custom Canvas, as the Android app's (ui/CustomCanvasUi.kt), over the whole window: the
//! sections (dimensions, colour profile, time-lapse settings, canvas properties) on the left, the
//! chosen one on the right under the canvas's name, Cancel and Done. Sizes are typed on a keypad of
//! its own (or the keyboard), in millimetres, centimetres, inches or pixels; the background's colour
//! is picked on the compact picker.

use egui::{Align2, Color32, Id, Order, Pos2, Rect, Sense, Stroke, UiBuilder, pos2, vec2};
use sumi_core::{
    CanvasPreset, Palettes, SRGB, cmyk_profiles, color_profiles, is_cmyk_profile, layers_for, per_inch, preset_height,
    preset_number, preset_width,
};

use crate::colours::HarmonyKind;
use crate::compact_colours::{CompactColours, Picked};
use crate::icons::{self, Icon};
use crate::look::{Look, Themed, Weight, argb, pc, text};

pub(crate) const SCREEN: Themed = Themed(0xFF211F22, 0xFFEFE8DF);
pub(crate) const PANEL: Themed = Themed(0xFF2C292D, 0xFFF7F2EA);
const INK: Themed = pc::TEXT;
const IDLE_INK: Themed = pc::BAR_TEXT;
pub(crate) const EDITING: Themed = pc::ACTIVE;
const KEYPAD: Themed = Themed(0xFF1B191C, 0xFFE2D9CC);
const DIGIT: Themed = Themed(0xFF474045, 0xFFFFFFFF);
const KEY: Themed = Themed(0xFF332E32, 0xFFCFC4B6);
const UNIT_CHOSEN: Themed = Themed(0xFFA89B9E, 0xFF7D7068);
const UNIT_CHOSEN_INK: Themed = Themed(0xFF211F22, 0xFFFFFFFF);
const SWITCH_TRACK: Themed = Themed(0xFF383337, 0xFFD8CEBF);
const SWITCH_IDLE: Themed = pc::TEXT;
const CAPSULE: Themed = Themed(0xFF1B191C, 0xFFFBF8F3);
const CAPSULE_EDGE: Themed = crate::panel::DOCK_EDGE;
const CAPSULE_INK: Themed = pc::BAR_TEXT;
const DONE: Themed = pc::ACTIVE;
const SWATCH_EDGE: Themed = crate::panel::DOCK_EDGE;
const ROW: f32 = 47.0;
/// The page, right of the sections.
const PAGE_LEFT: f32 = 372.0;
const PAGE_TOP: f32 = 120.0;
/// Where the last row the keypad types into (DPI) ends, down from the top: two rows and their
/// line, 24 between the groups, one row.
const TYPED_BOTTOM: f32 = PAGE_TOP + ROW * 2.0 + 1.0 + 24.0 + ROW;

/// The units sizes are typed in: the key's name, the unit's (as presets keep it) and its mark.
const UNITS: [(&str, &str, &str); 4] =
    [("Millimeters", "mm", "mm"), ("Centimeters", "cm", "cm"), ("Inches", "in", "\u{2033}"), ("Pixels", "px", "px")];

const SECTIONS: [(&str, &Icon, f32); 4] = [
    ("Dimensions", &icons::INFO_DIMENSIONS, 50.0),
    ("Color profile", &icons::INFO_PROFILE, 35.0),
    ("Time-lapse settings", &icons::INFO_VIDEO, 60.0),
    ("Canvas properties", &icons::TAB_CANVAS, 40.0),
];

/// The fields the keypad types into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Width,
    Height,
    Dpi,
}

/// How Custom Canvas ended.
pub enum Done {
    Cancel,
    /// Done, as this preset.
    Made(CanvasPreset),
}

pub struct CustomCanvas {
    /// The preset edited (its place in the list); none for a new one.
    pub editing: Option<usize>,
    section: usize,
    name: String,
    /// The size in the unit chosen (its place in [`UNITS`]), and the DPI.
    width: f32,
    height: f32,
    dpi: f32,
    unit: usize,
    /// The field typed into, and what has been typed until it is left.
    field: Option<Field>,
    typed: String,
    background: u32,
    hidden: bool,
    resolution: u32,
    quality: u32,
    hevc: bool,
    profile: String,
    profile_dialog: Option<crate::file_chooser::Response>,
    /// Which profiles the list shows.
    cmyk: bool,
    /// The background's colour being picked.
    picking: Option<CompactColours>,
}

impl CustomCanvas {
    /// Custom Canvas on `start` (the preset at that place in the list), or on a new canvas.
    pub fn new(start: Option<(usize, CanvasPreset)>) -> CustomCanvas {
        let (editing, p) = match start {
            Some((i, p)) => (Some(i), p),
            None => (
                None,
                CanvasPreset {
                    name: "Untitled Canvas".into(),
                    unit: "px".into(),
                    size_width: 1080.0,
                    size_height: 1080.0,
                    dpi: 300.0,
                    background: 0xFF_FFFF,
                    background_hidden: false,
                    time_lapse_resolution: 0,
                    time_lapse_quality: 1,
                    time_lapse_hevc: false,
                    profile: SRGB.into(),
                },
            ),
        };
        CustomCanvas {
            editing,
            section: 0,
            name: p.name.clone(),
            width: p.size_width,
            height: p.size_height,
            dpi: p.dpi,
            unit: UNITS.iter().position(|u| u.1 == p.unit).unwrap_or(3),
            field: Some(Field::Width),
            typed: String::new(),
            background: p.background,
            hidden: p.background_hidden,
            resolution: p.time_lapse_resolution,
            quality: p.time_lapse_quality,
            hevc: p.time_lapse_hevc,
            cmyk: is_cmyk_profile(p.profile.clone()),
            profile: p.profile,
            profile_dialog: None,
            picking: None,
        }
    }

    fn preset(&self) -> CanvasPreset {
        let name = if self.name.trim().is_empty() { "Untitled Canvas".to_owned() } else { self.name.trim().to_owned() };
        CanvasPreset {
            name,
            unit: UNITS.get(self.unit).map_or("px", |u| u.1).to_owned(),
            size_width: self.width,
            size_height: self.height,
            dpi: self.dpi,
            background: self.background,
            background_hidden: self.hidden,
            time_lapse_resolution: self.resolution,
            time_lapse_quality: self.quality,
            time_lapse_hevc: self.hevc,
            profile: self.profile.clone(),
        }
    }

    /// What typing has made of the field so far goes into it.
    fn commit(&mut self) {
        if let Ok(v) = self.typed.parse::<f32>()
            && v > 0.0
        {
            match self.field {
                Some(Field::Width) => self.width = v,
                Some(Field::Height) => self.height = v,
                Some(Field::Dpi) => self.dpi = v.clamp(1.0, 9999.0),
                None => {}
            }
        }
        self.typed.clear();
    }

    /// The size kept, in another unit: straight from one length to another, through the pixels
    /// otherwise.
    fn convert(&mut self, to: usize) {
        let p = self.preset();
        let (px_w, px_h) = (preset_width(p.clone()) as f32, preset_height(p) as f32);
        let from = UNITS.get(self.unit).and_then(|u| per_inch(u.1.into()));
        let into = UNITS.get(to).and_then(|u| per_inch(u.1.into()));
        let change = |v: f32, px: f32| match (into, from) {
            (Some(k), Some(f)) => v / f * k,
            (Some(k), None) => px / self.dpi * k,
            (None, _) => px,
        };
        (self.width, self.height) = (change(self.width, px_w), change(self.height, px_h));
        self.unit = to;
    }

    fn key(&mut self, k: char) {
        match k {
            '\u{8}' => _ = self.typed.pop(),
            '.' => {
                if !self.typed.contains('.') && self.field != Some(Field::Dpi) && self.unit != 3 {
                    self.typed.push('.');
                }
            }
            d if d.is_ascii_digit() && self.typed.len() < 7 => self.typed.push(d),
            _ => {}
        }
    }

    fn next(&mut self) {
        self.commit();
        self.field = Some(match self.field {
            Some(Field::Width) => Field::Height,
            Some(Field::Height) => Field::Dpi,
            _ => Field::Width,
        });
    }

    /// Shows it over the whole window; returns how it ended, once it has.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        look: &mut Look,
        palettes: &Palettes,
        harmony: HarmonyKind,
    ) -> Option<Done> {
        if let Some(paths) = crate::file_chooser::poll(&mut self.profile_dialog)
            && let Some(path) = paths.first()
            && let Some(name) = crate::files::import_profile_path(path)
        {
            self.cmyk = is_cmyk_profile(name.clone());
            self.profile = name;
        }
        let screen = ctx.content_rect();
        let wide = screen.width() > screen.height();
        let mut done = None;
        let mut name_focused = false;
        egui::Area::new(Id::unique("custom canvas")).fixed_pos(screen.min).order(Order::Foreground).show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::click());
            let painter = ui.painter().clone();
            painter.rect_filled(screen, 0, argb(SCREEN));
            let ink = argb(INK);
            // the sections
            let rail =
                Rect::from_min_max(screen.min + vec2(10.0, 10.0), pos2(screen.left() + 329.0, screen.bottom() - 10.0));
            painter.rect_filled(rail, 20.0, argb(PANEL));
            text(&painter, rail.min + vec2(24.0, 16.0), Align2::LEFT_TOP, "Custom Canvas", 22.0, Weight::Bold, ink);
            for (i, (label, icon, size)) in SECTIONS.into_iter().enumerate() {
                let row = Rect::from_min_size(
                    pos2(rail.left() + 8.0, rail.top() + 59.0 + 62.0 * i as f32),
                    vec2(304.0, 52.0),
                );
                let chosen = i == self.section;
                if chosen {
                    painter.rect_filled(row, 26.0, argb(pc::ACCENT));
                }
                let colour = if chosen { argb(pc::ACCENT_INK) } else { argb(IDLE_INK) };
                // icons centred 43 in, whatever their size; the names 69 in
                look.icon(ui, icon, pos2(rail.left() + 43.0, row.center().y), size, colour);
                text(
                    &painter,
                    pos2(rail.left() + 69.0, row.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    15.0,
                    Weight::Regular,
                    colour,
                );
                if ui.interact(row, ui.scope_id().with(("section", i)), Sense::click()).clicked() {
                    self.commit();
                    self.section = i;
                }
            }
            // the name, centred over the page (upright, from its left)
            let field = if wide {
                Rect::from_min_max(
                    pos2(screen.left() + PAGE_LEFT, screen.top() + 28.0),
                    pos2(screen.right() - 32.0, screen.top() + 56.0),
                )
            } else {
                Rect::from_min_max(
                    pos2(screen.left() + 394.0, screen.top() + 28.0),
                    pos2(screen.right() - 160.0, screen.top() + 56.0),
                )
            };
            let edit = egui::TextEdit::singleline(&mut self.name)
                .font(Weight::Bold.font(22.0))
                .text_color(ink)
                .frame(egui::Frame::NONE)
                .margin(vec2(0.0, 0.0))
                .align(if wide { Align2::CENTER_CENTER } else { Align2::LEFT_CENTER })
                .desired_width(field.width())
                .char_limit(64)
                .show(&mut ui.new_child(UiBuilder::new().max_rect(field)));
            name_focused = edit.response.has_focus();
            // Cancel, and Import beside it on the colour profile page, then Done
            let tick = Rect::from_min_size(pos2(screen.right() - 20.0 - 44.0, screen.top() + 20.0), vec2(44.0, 45.0));
            let words: &[&str] = if self.section == 1 { &["Cancel", "Import"] } else { &["Cancel"] };
            match capsule_words(ui, tick.left() - 13.0, screen.top() + 21.0, words, CAPSULE_INK) {
                Some("Cancel") => done = Some(Done::Cancel),
                Some(_) => {
                    if cfg!(target_os = "linux") {
                        self.profile_dialog = Some(crate::file_chooser::open("Import color profile", &["icc", "icm"]));
                    } else if let Some(name) = crate::files::import_profile() {
                        self.cmyk = is_cmyk_profile(name.clone());
                        self.profile = name;
                    }
                }
                None => {}
            }
            painter.rect_filled(tick, 22.0, argb(DONE));
            look.icon(ui, &icons::DONE_TICK, tick.center(), 40.0, argb(pc::ACCENT_INK));
            if ui.interact(tick, ui.scope_id().with("done"), Sense::click()).clicked() {
                self.commit();
                done = Some(Done::Made(self.preset()));
            }
            let page = Rect::from_min_max(
                pos2(screen.left() + PAGE_LEFT, screen.top() + PAGE_TOP),
                pos2(screen.right() - 32.0, screen.bottom()),
            );
            match self.section {
                0 => self.dimensions(ui, page),
                1 => self.profiles(ui, look, page),
                2 => self.time_lapse(ui, look, page),
                _ => self.properties(ui, page),
            }
            if self.section == 0 && self.field.is_some() {
                self.keypad(ui, screen, wide);
            }
        });
        // the keyboard types sizes too, while the name isn't being typed
        if self.section == 0 && self.field.is_some() && !name_focused {
            let mut keys = Vec::new();
            ctx.input(|i| {
                for e in &i.events {
                    match e {
                        egui::Event::Text(t) => keys.extend(t.chars().filter(|c| c.is_ascii_digit() || *c == '.')),
                        egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => keys.push('\u{8}'),
                        egui::Event::Key { key: egui::Key::Tab, pressed: true, .. } => keys.push('\t'),
                        egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => keys.push('\n'),
                        _ => {}
                    }
                }
            });
            for k in keys {
                match k {
                    '\t' => self.next(),
                    '\n' => {
                        self.commit();
                        self.field = None;
                    }
                    k => self.key(k),
                }
            }
        }
        // the background's colour, in the compact picker hanging from its swatch's row
        if let Some(pick) = self.picking.as_mut() {
            let size = pick.size();
            let rect = Rect::from_min_size(pos2(screen.right() - size.x, screen.top() + 168.0), size);
            let p3 = sumi_core::colours_in_p3(self.profile.clone());
            match pick.show(ctx, look, rect, rect.left() + 200.0, palettes, harmony, p3) {
                Some(Picked::Colour(rgb)) => self.background = rgb,
                Some(Picked::Closed) => self.picking = None,
                None => {}
            }
        }
        done
    }

    /// Width and height, then DPI and the most layers a canvas that size holds.
    fn dimensions(&mut self, ui: &egui::Ui, page: Rect) {
        let mark = UNITS.get(self.unit).map_or("px", |u| u.2);
        let shown = |me: &Self, f: Field, v: String| {
            if me.field == Some(f) && !me.typed.is_empty() { me.typed.clone() } else { v }
        };
        let first = group(ui, page.left_top(), page.width(), 2);
        let rows = [
            ("Width", shown(self, Field::Width, preset_number(self.width)), mark, Some(Field::Width)),
            ("Height", shown(self, Field::Height, preset_number(self.height)), mark, Some(Field::Height)),
        ];
        let second_top = first.bottom() + 24.0;
        let p = self.preset();
        let layers = layers_for(preset_width(p.clone()), preset_height(p)).to_string();
        let more = [
            ("DPI", shown(self, Field::Dpi, format!("{}", self.dpi.round() as i64)), "", Some(Field::Dpi)),
            ("Maximum Layers", layers, "", None),
        ];
        group(ui, pos2(page.left(), second_top), page.width(), 2);
        for (k, (label, value, unit, f)) in rows.into_iter().chain(more).enumerate() {
            let top = if k < 2 { first.top() } else { second_top } + (ROW + 1.0) * (k % 2) as f32;
            let row = Rect::from_min_size(pos2(page.left(), top), vec2(page.width(), ROW));
            let ink = argb(INK);
            text(
                ui.painter(),
                pos2(row.left() + 16.0, row.center().y),
                Align2::LEFT_CENTER,
                label,
                15.0,
                Weight::Regular,
                ink,
            );
            let editing = f.is_some() && self.field == f;
            let unit_shown = if unit.is_empty() { String::new() } else { format!(" {unit}") };
            let at = pos2(row.right() - 16.0, row.center().y);
            let after = text(ui.painter(), at, Align2::RIGHT_CENTER, &unit_shown, 15.0, Weight::Regular, ink);
            let colour = if editing { argb(EDITING) } else { ink };
            text(
                ui.painter(),
                pos2(after.left(), row.center().y),
                Align2::RIGHT_CENTER,
                &value,
                15.0,
                Weight::Regular,
                colour,
            );
            if let Some(f) = f
                && ui.interact(row, ui.scope_id().with(("size row", k)), Sense::click()).clicked()
            {
                self.commit();
                self.field = Some(f);
            }
        }
    }

    /// RGB or CMYK, then the profiles of that kind, the chosen one blue.
    fn profiles(&mut self, ui: &egui::Ui, look: &mut Look, page: Rect) {
        let switch = Rect::from_min_size(page.left_top(), vec2(page.width(), 34.0));
        ui.painter().rect_filled(switch, 17.0, argb(SWITCH_TRACK));
        for (i, (label, icon)) in [("RGB", &icons::PROFILE_RGB), ("CMYK", &icons::PROFILE_CMYK)].into_iter().enumerate()
        {
            let half = Rect::from_min_size(
                switch.min + vec2(2.0 + (switch.width() - 4.0) / 2.0 * i as f32, 2.0),
                vec2((switch.width() - 4.0) / 2.0, 30.0),
            );
            let on = self.cmyk == (i == 1);
            if on {
                ui.painter().rect_filled(half, 15.0, argb(pc::ACCENT));
            }
            let ink = if on { argb(pc::ACCENT_INK) } else { argb(SWITCH_IDLE) };
            let label_width = 15.0 * 0.6 * label.len() as f32;
            let start = half.center().x - (20.0 + 8.0 + label_width) / 2.0;
            look.icon(ui, icon, pos2(start + 10.0, half.center().y), 20.0, ink);
            text(
                ui.painter(),
                pos2(start + 28.0, half.center().y),
                Align2::LEFT_CENTER,
                label,
                15.0,
                Weight::Regular,
                ink,
            );
            if ui.interact(half, ui.scope_id().with(("kind", i)), Sense::click()).clicked() {
                self.cmyk = i == 1;
            }
        }
        let names = if self.cmyk { cmyk_profiles() } else { color_profiles() };
        let top = switch.bottom() + 24.0;
        group(ui, pos2(page.left(), top), page.width(), names.len());
        for (i, name) in names.iter().enumerate() {
            let row = Rect::from_min_size(pos2(page.left(), top + (ROW + 1.0) * i as f32), vec2(page.width(), ROW));
            if choice_row(
                ui,
                row,
                name,
                *name == self.profile,
                None,
                false,
                ("profile", i),
                look,
                i == 0,
                i + 1 == names.len(),
            ) {
                self.profile = name.clone();
            }
        }
    }

    /// The video's resolution, its quality, and HEVC.
    fn time_lapse(&mut self, ui: &egui::Ui, look: &mut Look, page: Rect) {
        let sizes = [("1080p", "1920 \u{d7} 1080px"), ("2K", "2732 \u{d7} 2048px"), ("4K", "3840 \u{d7} 2160px")];
        group(ui, page.left_top(), page.width(), 3);
        for (i, (label, detail)) in sizes.into_iter().enumerate() {
            let row =
                Rect::from_min_size(pos2(page.left(), page.top() + (ROW + 1.0) * i as f32), vec2(page.width(), ROW));
            if choice_row(
                ui,
                row,
                label,
                i as u32 == self.resolution,
                Some(detail),
                false,
                ("resolution", i),
                look,
                i == 0,
                i == 2,
            ) {
                self.resolution = i as u32;
            }
        }
        let top = page.top() + (ROW + 1.0) * 3.0 - 1.0 + 24.0;
        let qualities = ["Low quality", "Good quality", "Studio quality", "Lossless"];
        group(ui, pos2(page.left(), top), page.width(), 4);
        for (i, label) in qualities.into_iter().enumerate() {
            let row = Rect::from_min_size(pos2(page.left(), top + (ROW + 1.0) * i as f32), vec2(page.width(), ROW));
            if choice_row(ui, row, label, i as u32 == self.quality, None, true, ("quality", i), look, i == 0, i == 3) {
                self.quality = i as u32;
            }
        }
        let top = top + (ROW + 1.0) * 4.0 - 1.0 + 24.0;
        group(ui, pos2(page.left(), top), page.width(), 1);
        let row = Rect::from_min_size(pos2(page.left(), top), vec2(page.width(), ROW));
        if switch_line(ui, row, "HEVC", self.hevc, "hevc") {
            self.hevc = !self.hevc;
        }
    }

    /// The background's colour (a tap opens the compact picker), and whether it is hidden.
    fn properties(&mut self, ui: &egui::Ui, page: Rect) {
        group(ui, page.left_top(), page.width(), 1);
        let row = Rect::from_min_size(page.left_top(), vec2(page.width(), ROW));
        let ink = argb(INK);
        text(
            ui.painter(),
            pos2(row.left() + 16.0, row.center().y),
            Align2::LEFT_CENTER,
            "Background color",
            15.0,
            Weight::Regular,
            ink,
        );
        let swatch = pos2(row.right() - 16.0 - 12.0, row.center().y);
        let rgb = self.background;
        ui.painter().circle_filled(swatch, 12.0, Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
        ui.painter().circle_stroke(swatch, 11.5, Stroke::new(1.0, argb(SWATCH_EDGE)));
        if ui.interact(row, ui.scope_id().with("background"), Sense::click()).clicked() {
            let mut pick = CompactColours::new(self.background, 0.5);
            pick.arrow_below = false;
            self.picking = Some(pick);
        }
        let top = row.bottom() + 24.0;
        group(ui, pos2(page.left(), top), page.width(), 1);
        let row = Rect::from_min_size(pos2(page.left(), top), vec2(page.width(), ROW));
        if switch_line(ui, row, "Background hidden", self.hidden, "hidden") {
            self.hidden = !self.hidden;
        }
    }

    /// The keypad: units on the left, the digits, and Backspace, Next and Done on the right. On a
    /// wide window its keys are 150 x 87.5 (9 apart down) in a 457 panel, the rows from 47 below
    /// its top; upright 120 x 71 (8 apart) in a 342 one, centred in it. In a window too short for
    /// that, it and its keys are that much shorter, and it never starts above the rows it types
    /// into.
    fn keypad(&mut self, ui: &egui::Ui, screen: Rect, wide: bool) {
        let full = if wide { 457.0 } else { 342.0 };
        let fit = ((screen.height() - TYPED_BOTTOM) / full).clamp(0.35, 1.0);
        let (w, h, down, across) = if wide { (150.0, 87.5, 9.0, 9.0) } else { (120.0, 71.0, 8.0, 9.0) };
        let (h, down) = (h * fit, down * fit);
        // fit the whole keypad's width, 10 in from each side when narrow, and never enlarge it
        let whole = w + 30.0 + w * 3.0 + across * 2.0 + 30.0 + w;
        let xfit = ((screen.width() - 20.0) / whole).min(1.0);
        let (w, across, gap) = (w * xfit, across * xfit, 30.0 * xfit);
        // never over the rows it types into, even where it has to be shorter than half
        let panel_top = (screen.bottom() - full * fit).max(screen.top() + TYPED_BOTTOM);
        let panel = Rect::from_min_max(pos2(screen.left(), panel_top), screen.max);
        ui.painter().rect_filled(panel, 0, argb(KEYPAD));
        // the panel takes its own presses
        ui.interact(panel, ui.scope_id().with("keypad"), Sense::click());
        let content = vec2(w + gap + w * 3.0 + across * 2.0 + gap + w, h * 4.0 + down * 3.0);
        let left = panel.center().x - content.x / 2.0;
        let top = if wide { panel.top() + 47.0 * fit } else { panel.center().y - content.y / 2.0 };
        let key = |label: &str, rect: Rect, fill: Themed, ink: Color32, size: f32, id: (&str, usize)| {
            ui.painter().rect_filled(rect, 8.0, argb(fill));
            text(ui.painter(), rect.center(), Align2::CENTER_CENTER, label, size, Weight::Regular, ink);
            ui.interact(rect, ui.scope_id().with(id), Sense::click()).clicked()
        };
        let ink = argb(INK);
        for (i, (label, _, _)) in UNITS.into_iter().enumerate() {
            let rect = Rect::from_min_size(pos2(left, top + (h + down) * i as f32), vec2(w, h));
            let chosen = i == self.unit;
            let (fill, colour) = if chosen { (UNIT_CHOSEN, argb(UNIT_CHOSEN_INK)) } else { (KEY, ink) };
            if key(label, rect, fill, colour, 15.0, ("unit", i)) && !chosen {
                self.commit();
                self.convert(i);
            }
        }
        let digits_left = left + w + gap;
        for (r, row) in ["789", "456", "123"].into_iter().enumerate() {
            for (c, d) in row.chars().enumerate() {
                let rect = Rect::from_min_size(
                    pos2(digits_left + (w + across) * c as f32, top + (h + down) * r as f32),
                    vec2(w, h),
                );
                if key(&d.to_string(), rect, DIGIT, ink, 26.0, ("digit", r * 3 + c)) {
                    self.key(d);
                }
            }
        }
        let last = top + (h + down) * 3.0;
        if key(
            "0",
            Rect::from_min_size(pos2(digits_left, last), vec2(w * 2.0 + across, h)),
            DIGIT,
            ink,
            26.0,
            ("digit", 10),
        ) {
            self.key('0');
        }
        if key(
            ".",
            Rect::from_min_size(pos2(digits_left + (w + across) * 2.0, last), vec2(w, h)),
            DIGIT,
            ink,
            26.0,
            ("digit", 11),
        ) {
            self.key('.');
        }
        let right = digits_left + w * 3.0 + across * 2.0 + gap;
        if key("Backspace", Rect::from_min_size(pos2(right, top), vec2(w, h)), KEY, ink, 15.0, ("side", 0)) {
            self.key('\u{8}');
        }
        if key("Next", Rect::from_min_size(pos2(right, top + h + down), vec2(w, h)), KEY, ink, 15.0, ("side", 1)) {
            self.next();
        }
        let done = Rect::from_min_size(pos2(right, top + (h + down) * 2.0), vec2(w, h * 2.0 + down));
        if key("Done", done, KEY, ink, 15.0, ("side", 2)) {
            self.commit();
            self.field = None;
        }
    }
}

/// A header's capsule of words, ending at `right`: each a button. Returns the one tapped.
pub(crate) fn capsule_words<'a>(
    ui: &egui::Ui,
    right: f32,
    top: f32,
    words: &[&'a str],
    ink: Themed,
) -> Option<&'a str> {
    let galleys: Vec<_> = words
        .iter()
        .map(|w| ui.painter().layout_no_wrap((*w).to_owned(), Weight::Regular.font(17.0), argb(ink)))
        .collect();
    let width: f32 = galleys.iter().map(|g| g.size().x + 26.0).sum();
    let capsule = Rect::from_min_size(pos2(right - width, top), vec2(width, 43.0));
    ui.painter().rect_filled(capsule, 21.5, argb(CAPSULE));
    ui.painter().rect_stroke(capsule, 21.5, Stroke::new(1.0, argb(CAPSULE_EDGE)), egui::StrokeKind::Inside);
    let mut x = capsule.left();
    let mut tapped = None;
    for (word, galley) in words.iter().zip(galleys) {
        let button = Rect::from_min_size(pos2(x, top), vec2(galley.size().x + 26.0, 43.0));
        let at = button.center() - galley.size() / 2.0;
        ui.painter().galley(at, galley.clone(), argb(ink));
        if ui.interact(button, ui.scope_id().with(("capsule", *word)), Sense::click()).clicked() {
            tapped = Some(*word);
        }
        x = button.right();
    }
    tapped
}

/// A group of `rows` rows from `at`, `width` wide (rounded 16), a line between each; returns it.
fn group(ui: &egui::Ui, at: Pos2, width: f32, rows: usize) -> Rect {
    let rect = Rect::from_min_size(at, vec2(width, (ROW + 1.0) * rows as f32 - 1.0));
    ui.painter().rect_filled(rect, 16.0, argb(PANEL));
    for k in 1..rows {
        let line = Rect::from_min_size(pos2(rect.left(), rect.top() + (ROW + 1.0) * k as f32 - 1.0), vec2(width, 1.0));
        ui.painter().rect_filled(line, 0, argb(SCREEN));
    }
    rect
}

/// A choice in a group: blue when chosen (rounded with the group at its ends), a detail at its
/// right or, chosen, a tick. Returns whether it was tapped.
#[allow(clippy::too_many_arguments)]
fn choice_row(
    ui: &egui::Ui,
    row: Rect,
    label: &str,
    chosen: bool,
    detail: Option<&str>,
    tick: bool,
    id: (&str, usize),
    look: &mut Look,
    first: bool,
    last: bool,
) -> bool {
    if chosen {
        let corners = egui::CornerRadius {
            nw: if first { 16 } else { 0 },
            ne: if first { 16 } else { 0 },
            sw: if last { 16 } else { 0 },
            se: if last { 16 } else { 0 },
        };
        ui.painter().rect_filled(row, corners, argb(pc::ACCENT));
    }
    let ink = if chosen { argb(pc::ACCENT_INK) } else { argb(INK) };
    text(ui.painter(), pos2(row.left() + 16.0, row.center().y), Align2::LEFT_CENTER, label, 15.0, Weight::Regular, ink);
    if let Some(d) = detail {
        text(
            ui.painter(),
            pos2(row.right() - 16.0, row.center().y),
            Align2::RIGHT_CENTER,
            d,
            15.0,
            Weight::Regular,
            ink,
        );
    }
    if tick && chosen {
        look.icon(ui, &icons::MENU_TICK, pos2(row.right() - 16.0 - 10.0, row.center().y), 20.0, argb(pc::ACCENT_INK));
    }
    ui.interact(row, ui.scope_id().with(id), Sense::click()).clicked()
}

/// A switch's row: its name, and the switch at its right. Returns whether it was tapped.
fn switch_line(ui: &egui::Ui, row: Rect, label: &str, on: bool, id: &str) -> bool {
    text(
        ui.painter(),
        pos2(row.left() + 16.0, row.center().y),
        Align2::LEFT_CENTER,
        label,
        15.0,
        Weight::Regular,
        argb(INK),
    );
    crate::transform::switch(ui, pos2(row.right() - 8.0 - 63.0, row.center().y - 14.0), on, id);
    ui.interact(row, ui.scope_id().with(id), Sense::click()).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rectangles painted with one of the keypad's key fills, as that frame painted them.
    fn painted_key_backgrounds(shapes: &[egui::epaint::ClippedShape], colours: &[Color32]) -> Vec<(Color32, Rect)> {
        fn walk(shape: &egui::Shape, colours: &[Color32], out: &mut Vec<(Color32, Rect)>) {
            match shape {
                egui::Shape::Rect(rect) if colours.contains(&rect.fill) => out.push((rect.fill, rect.rect)),
                egui::Shape::Vec(items) => items.iter().for_each(|item| walk(item, colours, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, colours, &mut out);
        }
        out
    }

    /// The compact clients are landscape, so the 828-point keypad is laid out in `wide` form and
    /// must keep every key wholly inside the viewport: the 4 units, the 11 digits (789/456/123,
    /// 0 and .) and Backspace/Next/Done. At 788 their text centres fit while the key backgrounds
    /// do not, so this looks at painted rectangles, not labels.
    #[test]
    fn compact_keypad_keeps_every_key_inside_the_viewport() {
        for (width, height) in [(640.0, 600.0), (788.0, 556.0)] {
            let ctx = egui::Context::default();
            crate::look::install_fonts(&ctx);
            // The Area's first frame is an invisible sizing pass, and its fade must not tint fills.
            ctx.all_styles_mut(|style| style.animation_time = 0.0);
            let mut look = Look::default();
            let palettes = Palettes::open(String::new());
            let mut canvas = CustomCanvas::new(None);
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height));
            let key = [argb(Themed(KEY.0, KEY.0)), argb(Themed(KEY.1, KEY.1))];
            let digit = [argb(Themed(DIGIT.0, DIGIT.0)), argb(Themed(DIGIT.1, DIGIT.1))];
            let unit = [argb(Themed(UNIT_CHOSEN.0, UNIT_CHOSEN.0)), argb(Themed(UNIT_CHOSEN.1, UNIT_CHOSEN.1))];
            let colours: Vec<Color32> = [key, digit, unit].into_iter().flatten().collect();
            let mut frame = None;
            for step in 0..3 {
                let raw = egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(1.0 + f64::from(step) / 60.0),
                    ..Default::default()
                };
                let mut out = ctx.run_ui(raw, |ui| {
                    let ctx = ui.ctx().clone();
                    let _ = canvas.show(&ctx, &mut look, &palettes, HarmonyKind::Complementary);
                });
                // the icons' first uploads: the key shapes are read, not the textures
                out.textures_delta.clear();
                frame = Some(out);
            }
            let keys = painted_key_backgrounds(&frame.unwrap().shapes, &colours);
            let count = |pair: [Color32; 2]| keys.iter().filter(|(fill, _)| pair.contains(fill)).count();
            let (key_count, digit_count, unit_count) = (count(key), count(digit), count(unit));
            assert!(
                digit_count >= 11 && unit_count >= 1 && key_count >= 6 && keys.len() >= 18,
                "expected the units, digits and side keys at {width}x{height}; got {key_count} other, \
                 {digit_count} digits, {unit_count} chosen unit"
            );
            let clipped: Vec<Rect> =
                keys.iter().map(|(_, rect)| *rect).filter(|rect| !screen.contains_rect(*rect)).collect();
            assert!(
                clipped.is_empty(),
                "{} of {} key backgrounds leave the {width}x{height} viewport: {clipped:?}",
                clipped.len(),
                keys.len()
            );
        }
    }

    /// At the smallest supported client the keypad's panel must still begin under the rows it
    /// types into. It used to stop shrinking at half, so it painted over the DPI and Maximum
    /// Layers rows and took their presses.
    #[test]
    fn compact_keypad_stays_below_the_rows_it_types_into() {
        for (width, height) in [(640.0, 480.0), (640.0, 515.0)] {
            let ctx = egui::Context::default();
            crate::look::install_fonts(&ctx);
            ctx.all_styles_mut(|style| style.animation_time = 0.0);
            let mut look = Look::default();
            let palettes = Palettes::open(String::new());
            let mut canvas = CustomCanvas::new(None);
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height));
            let mut frame = None;
            for step in 0..3 {
                let raw = egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(1.0 + f64::from(step) / 60.0),
                    ..Default::default()
                };
                let mut out = ctx.run_ui(raw, |ui| {
                    let ctx = ui.ctx().clone();
                    let _ = canvas.show(&ctx, &mut look, &palettes, HarmonyKind::Complementary);
                });
                // the icons' first uploads: the key shapes are read, not the textures
                out.textures_delta.clear();
                frame = Some(out);
            }
            let shapes = frame.unwrap().shapes;
            let both = |t: Themed| [argb(Themed(t.0, t.0)), argb(Themed(t.1, t.1))];
            let panels: Vec<Rect> = painted_key_backgrounds(&shapes, &both(KEYPAD))
                .into_iter()
                .map(|(_, rect)| rect)
                // the panel is the full-width band at the bottom; the dark capsule shares its fill
                .filter(|rect| rect.width() == screen.width() && rect.bottom() == screen.bottom())
                .collect();
            assert_eq!(panels.len(), 1, "one keypad panel at {width}x{height}: {panels:?}");
            let panel = panels[0];
            // the page's own groups, right of the sections rail, painted in PANEL
            let rows: Vec<Rect> = painted_key_backgrounds(&shapes, &both(PANEL))
                .into_iter()
                .map(|(_, rect)| rect)
                .filter(|rect| rect.left() >= screen.left() + PAGE_LEFT)
                .collect();
            assert!(rows.len() >= 2, "the page's groups at {width}x{height}: {rows:?}");
            // the DPI row is the last one the keypad types into: its bottom is TYPED_BOTTOM
            let typed_bottom = screen.top() + TYPED_BOTTOM;
            assert!(
                panel.top() >= typed_bottom,
                "the keypad starts at {} and covers the typed rows ending at {typed_bottom} at {width}x{height}",
                panel.top()
            );
            let keys = painted_key_backgrounds(&shapes, &[both(KEY), both(DIGIT), both(UNIT_CHOSEN)].concat());
            assert!(keys.len() >= 18, "{} keys at {width}x{height}", keys.len());
            let outside: Vec<Rect> =
                keys.into_iter().map(|(_, rect)| rect).filter(|rect| !screen.contains_rect(*rect)).collect();
            assert!(outside.is_empty(), "{} keys leave {width}x{height}: {outside:?}", outside.len());
        }
    }
}
