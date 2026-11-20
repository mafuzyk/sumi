//! The interface over the canvas: the bars (see [`crate::chrome`]), the popovers hanging from
//! the top bar, and the keyboard's shortcuts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, Key, Modifiers, Rect, Stroke, Ui, vec2};
use sumi_core::{
    Blocked, CanvasInfo, CanvasSession, GesturePurpose, KeyCombo, KeyShortcut, ModeButton, ModeSwitch, Modes,
    QuickAction, ToolKind, TouchCommand, Trigger, mode_switch,
};

use crate::actions::{Act, Actions};
use crate::alert::{Alert, Answer, Asking};
use crate::chrome::{BarButton, Lit, SideBar};
use crate::colour_drop::{ColourDrop, Touch};
use crate::colours::{Colours, hsv_to_rgb, of_hex, srgb, take_rgb, to_hex};
use crate::copy_paste::Choice;
use crate::files::Format;
use crate::input::Input;
use crate::layers::Layers;
use crate::library::Library;
use crate::look::{Look, Themed, argb, pc};
use crate::overlays::{Overlays, snapped_to};
use crate::system_clipboard::{Pasted, SystemClipboard};

/// Native windows and menus: the ground while dark and the light look's surface, so they sit off
/// the bar's own surface. A selected row takes the identity's line; links the secondary ink.
const POPOVER_FILL: Themed = Themed(0xFF211F22, 0xFFF7F2EA);
const NATIVE_SELECTION: Themed = crate::panel::DOCK_EDGE;
const NATIVE_LINK: Themed = pc::BAR_TEXT;
/// A tool button held this long (seconds), as a long press, takes the current brush.
const TOOL_HOLD_S: f64 = 0.4;
/// The pictures and the videos Insert a file and Insert a photo take (and a drop on the canvas).
const PICTURES: [&str; 8] = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];
const VIDEOS: [&str; 6] = ["mp4", "m4v", "mov", "wmv", "avi", "mkv"];

/// The canvas, less the opaque Layers dock and opaque status bar.
fn canvas_without_chrome(canvas: Rect, dock: Option<Rect>, status_bar: Option<Rect>) -> Rect {
    let canvas = canvas_without_layers_dock(canvas, dock);
    match status_bar.filter(|s| s.intersects(canvas)) {
        Some(status) => Rect::from_min_max(canvas.min, egui::pos2(canvas.max.x, canvas.max.y.min(status.top()))),
        None => canvas,
    }
}

/// The canvas, less the opaque Layers dock.
fn canvas_without_layers_dock(canvas: Rect, dock: Option<Rect>) -> Rect {
    let Some(dock) = dock.filter(|d| d.intersects(canvas)) else { return canvas };
    if dock.center().x >= canvas.center().x {
        Rect::from_min_max(canvas.min, egui::pos2(canvas.max.x.min(dock.left()), canvas.max.y))
    } else {
        Rect::from_min_max(egui::pos2(canvas.min.x.max(dock.right()), canvas.min.y), canvas.max)
    }
}

/// The canvas's fit insets (screen pixels: left, top, right, bottom): the side bar's, the top
/// bar's, the Layers dock's, and the opaque status bar's. The opaque dock and opaque status bar
/// keep their part even without Fit Canvas Inside Interface; full screen clears them all.
fn fit_insets(
    fit_inside: bool,
    bars_hidden: bool,
    side_bar_shown: bool,
    right_handed: bool,
    dock_width: f32,
    status_bar_shown: bool,
    scale: f32,
) -> [f32; 4] {
    if bars_hidden {
        return [0.0; 4];
    }
    let dock = if dock_width.is_finite() { dock_width.max(0.0) } else { 0.0 } * scale;
    let side =
        if fit_inside && side_bar_shown { (pc::SIDE_BAR_WIDTH + crate::panel::POPOVER_GAP) * scale } else { 0.0 };
    let top = if fit_inside { pc::BAR_HEIGHT * scale } else { 0.0 };
    let bottom = if status_bar_shown { crate::chrome::STATUS_BAR_HEIGHT * scale } else { 0.0 };
    // the dock sits opposite the side bar
    if right_handed { [dock, top, side, bottom] } else { [side, top, dock, bottom] }
}

/// Whether `path` is named as a picture is.
fn is_picture(path: &std::path::Path) -> bool {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    PICTURES.contains(&extension.as_str())
}

/// Why layer or group `name`, which would hide a change, takes none, by what hides it (`by`).
fn hidden_message(name: &str, by: Option<sumi_core::HiddenBy>) -> String {
    use sumi_core::HiddenBy as H;
    let why = match by {
        Some(H::MaskLayer) => "its layer is hidden",
        Some(H::Group) => "its group is hidden",
        Some(H::Inside) => "a layer in it is hidden",
        Some(H::Solo) => "another layer is shown alone",
        Some(H::Itself) | None => "it is hidden",
    };
    format!("\u{201c}{name}\u{201d} can't be changed while {why}.")
}
/// A video's frames come in at the animation's frame rate, or this one; this many at most.
const VIDEO_FPS: u32 = 15;
const MOST_VIDEO_FRAMES: u32 = 999;

/// What the interface asks the app to do once the frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Back to the gallery, the artwork kept.
    Gallery,
    New,
    Open,
    Save,
    SaveAs,
    /// The artwork written as this format; a PDF keeps its pictures at this JPEG quality.
    Export(Format, u8),
    /// The file a plugin makes of the artwork's layers (by its place among those offered).
    PluginExport(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Popover {
    Actions,
    Adjustments,
    Brushes,
    Layers,
    Colour,
}

/// The background's colour picked in the colour panel: as the panel shows it (hue, saturation,
/// brightness), the colour last sent to the background (0xRRGGBB), and whether any was.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BackgroundPick {
    hsv: [f32; 3],
    sent: u32,
    changed: bool,
}

impl BackgroundPick {
    /// Starts on the background's colour `rgb`, a grey taking the hue `hue`.
    fn new(rgb: u32, hue: f32) -> BackgroundPick {
        let mut hsv = [hue, 0.0, 0.0];
        take_rgb(&mut hsv, rgb);
        BackgroundPick { hsv, sent: rgb, changed: false }
    }

    /// What the panel shows now (`hsv`): the colour to send to the background while it is open, if
    /// it differs from the last one sent.
    fn follow(&mut self, hsv: [f32; 3]) -> Option<u32> {
        self.hsv = hsv;
        let rgb = to_hex(hsv_to_rgb(hsv));
        (rgb != self.sent).then(|| {
            self.sent = rgb;
            self.changed = true;
            rgb
        })
    }

    /// The colour to keep as one undo step as the panel closes: the last sent, if any was.
    fn kept(self) -> Option<u32> {
        self.changed.then_some(self.sent)
    }
}

pub struct Interface {
    pub actions: Vec<Action>,
    /// Where the canvas shows, clear of the bar (points).
    pub canvas_rect: Rect,
    /// The Layers dock's rect this frame, if it shows: the canvas and fitting keep clear of it.
    dock_rect: Option<Rect>,
    /// The window's width in points, updated when opened and each frame for status bar fitting.
    window_width: Option<f32>,
    /// A popover is open: a press on the canvas only closes it.
    pub popover_open: bool,
    /// Only a panel is open (none of its menus): a press on the canvas goes to the canvas, waiting
    /// to be a tap, which only closes the panel, or a drag, which closes it and paints.
    pub panel_lets_through: bool,
    /// The last press went to the canvas, which closes the panel in its own time.
    canvas_pressed: bool,
    /// How far the chrome has receded this frame (0..[`crate::look::CHROME_DIM`]): the top bar, the
    /// side bar, the status bar and the Layers dock step back by it while a stroke is painted, and
    /// breathe back as it ends.
    chrome_dim: f32,
    /// The egui frame last stepped by the dim: egui can run the UI twice a frame (a pass discarded
    /// for layout), so the dim advances once per frame rather than once per pass.
    dim_frame: u64,
    /// The file the artwork was last saved to (or opened from, if it saves back there).
    pub path: Option<PathBuf>,
    /// The artwork's edits when it was last saved: anything else is unsaved.
    pub saved_edits: u64,
    /// Inserts being read, each on a thread of its own.
    /// Inserts being read, each what it came to and whether it is to be transformed.
    inserting: Vec<std::sync::mpsc::Receiver<(Inserted, bool)>>,
    /// A Save As being written: where and at which edits, or why it couldn't be.
    pub saving: Option<std::sync::mpsc::Receiver<Result<(std::path::PathBuf, u64), String>>>,
    open: Option<Popover>,
    /// The background's colour being picked, in the colour panel shown in the Layers panel's
    /// place.
    background_pick: Option<BackgroundPick>,
    pub(crate) colours: Colours,
    drop: ColourDrop,
    library: Library,
    overlays: Overlays,
    /// The interface is hidden (full screen).
    bars_hidden: bool,
    /// A layer copied or cut, to paste.
    clipboard: Option<sumi_core::ThumbnailImage>,
    /// What other apps copied, which Paste takes once it is the newer.
    system_clipboard: SystemClipboard,
    /// Paste's button shows, the window in front (it is looked at again as that begins).
    paste_shown: bool,
    /// The Copy & Paste panel is up.
    copy_paste: bool,
    /// Layer Select's list: where (points) and the layers drawn there, top first.
    layer_choice: Option<(egui::Pos2, Vec<u64>)>,
    actions_panel: Actions,
    /// A plugin's panel's slider being moved: its row, and where it is.
    plugin_panel_held: Option<(usize, f32)>,
    /// A plugin's panel took Space as the key press it asked for: let go, it is no tap.
    space_to_panel: bool,
    layers: Layers,
    /// The Navigator above the Layers in the dock: the canvas at a glance, and the view.
    navigator: crate::navigator::Navigator,
    pub look: Look,
    side_bar: SideBar,
    /// Transform: its box over the canvas and its toolbar, while open.
    transform: crate::transform::Transform,
    /// Selection: its toolbar and what shows while selecting, while open.
    selection: crate::selection::Selection,
    /// The gesture settings (kept between runs), and their page while it is open.
    gestures: std::sync::Arc<sumi_core::GestureSettings>,
    gesture_controls: crate::gesture_controls::GestureControls,
    /// Canvas information, while it is open.
    canvas_info: Option<crate::canvas_info::CanvasInfo>,
    /// The reference window.
    reference: crate::reference::Reference,
    /// Crop and Resize.
    crop: crate::crop::Crop,
    /// The drawing guide's editor.
    guide_editor: crate::guide_editor::GuideEditor,
    /// The artwork's folder in the gallery.
    pub artwork_dir: Option<String>,
    quick_menu: crate::quick_menu::QuickMenu,
    /// A pen button set to the eyedropper is held: its loupe follows the pen until it is let go.
    pen_picking: bool,
    /// The pen's first and second buttons as held last frame: let go, one held to erase gives the
    /// eraser back.
    pen_held: [bool; 2],
    /// An open adjustment's bar, panel and tap menu; Gradient Map's library (kept between runs).
    adjustments: crate::adjustments::Adjustments,
    gradients: std::sync::Arc<sumi_core::Gradients>,
    styled: bool,
    /// The current layer turned a change away: why, and the alert saying so (offering to unlock or
    /// show it, or open Layers).
    blocked: Option<(Blocked, u64)>,
    blocked_alert: Alert,
    model_warning_alert: Alert,
    /// Share's PDF asks how well it keeps its pictures first (for this format).
    pdf_alert: Alert,
    pdf_for: Option<Format>,
    /// A plugin file's one screen, and the file while it asks.
    plugin_alert: Alert,
    plugin_file: Option<Vec<u8>>,
    /// Non-fatal import/export change notes shown inside the window.
    note_alert: Alert,
    /// An export under way, as its card shows it.
    pub export_card: crate::export_card::ExportCard,
    /// The brushes changed (the library closed, a slider let go, a notch saved): keep them.
    pub keep_brushes: bool,
    /// A tool button held long enough has acted (its tap, when let go, does nothing).
    tool_hold_acted: bool,
    brushes_were_open: bool,
    /// The Prefs tab's settings (kept between runs), whether the side bar shows, and the licences
    /// while they show.
    prefs: crate::prefs::Prefs,
    side_bar_shown: bool,
    licences: bool,
    /// The keyboard shortcuts' list, from Help or while Ctrl (⌘) is held.
    shortcut_list: crate::shortcuts::Shortcuts,
    /// Animation Assist's bar, while it is on, and sharing the layers as an animation.
    animation: crate::animation::AnimationBar,
    animated_export: Option<crate::animated_export::AnimatedExport>,
    /// The export screen just ended, kept a frame: its preview's textures were drawn in the frame
    /// it ended in, and a texture let go of is gone before that frame is.
    ended_export: Option<crate::animated_export::AnimatedExport>,
    /// The artwork was opened from the gallery's Share only to be shared as an animation: when
    /// that ends, the gallery comes back.
    exporting_from_gallery: bool,
    /// The artwork's time-lapse: recorded, replayed, made into a video.
    time_lapse: crate::timelapse::TimeLapseUi,
    /// The brush studio, while open (the app draws its pad).
    pub studio: crate::studio::Studio,
    /// Advanced settings' page is up.
    settings: bool,
    /// The single touch gestures companion, with Advanced settings' switch on.
    companion: crate::companion::Companion,
    /// How many onion skin frames there were before the QuickMenu last turned them off.
    onion_kept: u32,
    /// Recolor, while on, and the colour it last filled with.
    recolor_rgb: [f32; 3],
    /// The brush in use (its tool, set and place in it) and the one chosen before it with the
    /// same tool, for Previous brush.
    brush_now: Option<(ToolKind, u32, u32)>,
    brush_last: Option<(u32, u32)>,
    /// The artwork paints on a model; its Lighting Studio, while open.
    model: bool,
    lighting: crate::lighting::LightingStudio,
    /// The colour last named (Color Description Notifications) and when.
    named: Option<(u32, Instant)>,
    /// Text being added or edited.
    text: crate::text::TextEditor,
    /// A field had the keys as the last frame ended.
    typing: bool,
}

impl Default for Interface {
    fn default() -> Interface {
        Interface {
            actions: Vec::new(),
            canvas_rect: Rect::EVERYTHING,
            dock_rect: None,
            window_width: None,
            popover_open: false,
            panel_lets_through: false,
            canvas_pressed: false,
            chrome_dim: 0.0,
            dim_frame: u64::MAX,
            path: None,
            saved_edits: 0,
            saving: None,
            inserting: Vec::new(),
            open: None,
            background_pick: None,
            colours: Colours::default(),
            drop: ColourDrop::default(),
            library: Library::default(),
            overlays: Overlays::default(),
            bars_hidden: false,
            clipboard: None,
            system_clipboard: SystemClipboard::new(is_picture),
            paste_shown: false,
            copy_paste: false,
            layer_choice: None,
            actions_panel: Actions::default(),
            plugin_panel_held: None,
            space_to_panel: false,
            layers: Layers::default(),
            navigator: crate::navigator::Navigator::default(),
            look: Look::default(),
            side_bar: SideBar::default(),
            transform: crate::transform::Transform::default(),
            selection: crate::selection::Selection::default(),
            gestures: sumi_core::GestureSettings::new(crate::files::kept_gestures()),
            gesture_controls: crate::gesture_controls::GestureControls::default(),
            canvas_info: None,
            reference: crate::reference::Reference::default(),
            crop: crate::crop::Crop::default(),
            guide_editor: crate::guide_editor::GuideEditor::default(),
            artwork_dir: None,
            quick_menu: crate::quick_menu::QuickMenu::default(),
            pen_picking: false,
            pen_held: [false; 2],
            adjustments: crate::adjustments::Adjustments::default(),
            gradients: sumi_core::Gradients::open(
                crate::files::app_dir().join("gradients.json").to_string_lossy().into_owned(),
            ),
            styled: false,
            blocked: None,
            blocked_alert: Alert::default(),
            model_warning_alert: Alert::default(),
            pdf_alert: Alert::default(),
            pdf_for: None,
            plugin_alert: Alert::default(),
            plugin_file: None,
            note_alert: Alert::default(),
            export_card: crate::export_card::ExportCard::default(),
            keep_brushes: false,
            tool_hold_acted: false,
            brushes_were_open: false,
            prefs: crate::prefs::Prefs::kept(),
            side_bar_shown: true,
            licences: false,
            shortcut_list: Default::default(),
            animation: crate::animation::AnimationBar::default(),
            animated_export: None,
            ended_export: None,
            exporting_from_gallery: false,
            time_lapse: crate::timelapse::TimeLapseUi::default(),
            studio: crate::studio::Studio::default(),
            settings: false,
            named: None,
            model: false,
            lighting: crate::lighting::LightingStudio::default(),
            companion: crate::companion::Companion::default(),
            onion_kept: 1,
            recolor_rgb: [0.0; 3],
            brush_now: None,
            brush_last: None,
            text: crate::text::TextEditor::default(),
            typing: false,
        }
    }
}

impl Interface {
    /// A Save As written meanwhile: its file kept as where the artwork saves, or why it couldn't
    /// be told. True while one is still being written.
    pub fn saved_as(&mut self) -> bool {
        let Some(saving) = &self.saving else { return false };
        match saving.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return true,
            Ok(Ok((path, edits))) => {
                self.path = Some(path);
                self.saved_edits = edits;
            }
            Ok(Err(e)) => crate::files::tell("Can't save", &e),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
        self.saving = None;
        false
    }

    /// `session` is shown now (at `scale` window pixels a point, in a window `window` points
    /// big): it paints in the colour chosen.
    pub fn opened(&mut self, session: &Arc<CanvasSession>, scale: f32, window: egui::Vec2) {
        // each artwork its own Reference window, none if it never had one
        self.reference.restore(session);
        self.model = session.model_info().is_some();
        self.lighting.forget();
        // brush files given while no canvas was open
        self.library.add_brushes(crate::files::take_pending_brushes(), true);
        session.set_touch_layer_opacity(None);
        if self.model {
            crate::lighting::light_model(session);
            self.model_warnings(session);
        }
        self.layers.forget();
        self.navigator.forget();
        self.background_pick = None;
        self.text.forget();
        crate::text::prepare_fonts();
        self.transform.on = false;
        self.selection.on = false;
        self.drop.end_filling();
        session.use_colour_history(self.colours.history.clone());
        session.set_touch_settings(self.gestures.touch_settings(self.prefs.rapid_undo_ms()));
        session.use_gradients(self.gradients.clone());
        self.prefs.apply(session);
        self.time_lapse.open(self.artwork_dir.as_deref(), session.edits());
        self.adjustments.menu_at = None;
        self.colours.send(session);
        // as it was left, or fitted (Canvas Orientation Memory)
        if !self.prefs.orientation_memory {
            session.forget_saved_view();
        }
        // the first fit keeps clear of the dock too: the session places its view in the window
        // when it is shown, and a later inset change only moves the fit target, not a kept view
        self.dock_rect = crate::panel::layers_dock(
            Rect::from_min_size(egui::Pos2::ZERO, window),
            self.dock_wanted(session),
            self.prefs.right_handed,
        );
        self.window_width = Some(window.x);
        self.place_canvas(session, scale);
    }

    /// The artwork is put away (to the gallery, for another, or with the window): brushes coming
    /// in from files are waited for, Recolor ends keeping what it filled, and Layer Opacity ends.
    pub fn closing(&mut self, session: &CanvasSession) {
        // a plugin's panel goes with the artwork it was open over
        self.prefs.plugins.close_panel();
        self.plugin_panel_held = None;
        self.reference.keep(session);
        self.end_background_pick(session);
        self.dock_rect = None;
        self.library.finish_importing(session);
        self.end_recolor(session);
        session.set_touch_layer_opacity(None);
    }

    /// A new artwork just started: without time-lapse if that is turned off.
    pub fn started_new(&mut self) {
        if self.prefs.no_time_lapse {
            self.time_lapse.set_recording(false);
        }
    }

    /// The artwork just opened from the gallery's Share, to be shared as `format`: the gallery
    /// comes back once that ends.
    pub fn share_animated_from_gallery(&mut self, format: crate::animated_export::Animated) {
        self.animated_export = Some(crate::animated_export::AnimatedExport::new(format));
        self.exporting_from_gallery = true;
    }

    /// Whether the canvas is shown on a second display too.
    /// What an artwork dragged out of the gallery becomes (Advanced settings' Drag and Drop export).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn drag_format(&self) -> crate::files::Format {
        crate::files::DRAG_FORMATS.get(self.prefs.drag_format).copied().unwrap_or(crate::files::Format::Sumi)
    }

    /// The plugins, which say how big the interface shows.
    pub fn plugins(&self) -> &sumi_core::Plugins {
        &self.prefs.plugins
    }

    /// The plugins, to keep: what opens a plugin's artwork holds them.
    pub fn plugins_shared(&self) -> Arc<sumi_core::Plugins> {
        Arc::clone(&self.prefs.plugins)
    }

    /// Tap Assistance is on (Advanced settings).
    pub fn tap_assistance(&self) -> bool {
        self.prefs.tap_assistance
    }

    pub fn project_canvas(&self) -> bool {
        self.prefs.project_canvas
    }

    /// Project canvas goes off (its window closed on the other display).
    pub fn stop_projecting(&mut self) {
        self.prefs.project_canvas = false;
        self.prefs.keep();
    }

    /// Whether the status bar shows at the bottom of the workspace in a window `window_width`
    /// points wide (hidden in full screen, under Canvas Info / Gesture Controls, or when the
    /// remaining strip beside the Layers dock is narrower than `STATUS_BAR_MIN_WIDTH`).
    fn status_bar_shown(&self, window_width: f32) -> bool {
        !self.bars_hidden
            && self.canvas_info.is_none()
            && !self.gesture_controls.open
            && crate::chrome::status_bar_fits(window_width, self.dock_rect)
    }

    /// Fitting the canvas keeps it clear of the bar and the side bar, and always clear of the
    /// opaque Layers dock and opaque status bar when shown.
    pub fn place_canvas(&self, session: &CanvasSession, scale: f32) {
        let [left, top, right, bottom] = self.fit_insets();
        session.set_fit_insets(left * scale, top * scale, right * scale, bottom * scale);
    }

    /// The interface's fit insets this frame (points: left, top, right, bottom), as
    /// [`fit_insets`] gives them for the canvas's scale.
    fn fit_insets(&self) -> [f32; 4] {
        let window_width = self.window_width.unwrap_or(f32::INFINITY);
        fit_insets(
            self.prefs.fit_inside,
            self.bars_hidden,
            self.side_bar_shown,
            self.prefs.right_handed,
            self.dock_rect.map_or(0.0, |r| r.width()),
            self.status_bar_shown(window_width),
            1.0,
        )
    }

    /// The area of a `window` the canvas shows in (points): the window less the interface its
    /// fit keeps clear of, which is where the view is centred and the Navigator's rectangle read.
    fn canvas_area(&self, window: Rect) -> Rect {
        let [left, top, right, bottom] = self.fit_insets();
        Rect::from_min_max(
            egui::pos2(window.left() + left, window.top() + top),
            egui::pos2(window.right() - right, window.bottom() - bottom),
        )
    }

    /// The app's fonts and look, set the first time anything shows.
    pub fn style_once(&mut self, ctx: &egui::Context) {
        if !self.styled {
            style(ctx);
            self.styled = true;
        }
    }

    /// The interface for a frame. Escape closes what is open (each popover, panel, menu, alert and
    /// page sees to it, taking it with [`escape`]); one nothing took turns the canvas back upright,
    /// unless it only let go of a field (egui drops the field's keys as the frame begins).
    pub fn show(&mut self, ui: &mut Ui, session: &std::sync::Arc<CanvasSession>, input: &mut Input) {
        let typing = self.typing;
        _ = self.show_frame(ui, session, input);
        let ctx = ui.ctx();
        if !typing && escape_untaken(ctx) && self.canvas_shown() && input.idle() {
            session.settle_upright();
        }
        self.typing = ctx.memory(|m| m.focused().is_some());
        input.keys_taken = self.keys_taken();
        input.canvas_shown = self.canvas_shown();
    }

    /// The touches' settings this frame: while Polyline is armed, the one-finger gestures that would
    /// paint or fire a tap/hold action wait; the real settings return afterwards (no disk write).
    fn effective_touch_settings(&self) -> sumi_core::TouchSettings {
        let mut settings = self.gestures.touch_settings(self.prefs.rapid_undo_ms());
        if self.selection.polyline_armed() {
            settings.finger_paint = false;
            settings.touch = None;
            settings.hold = None;
            settings.modify_touch = None;
            settings.hover_touch = false;
        }
        settings
    }

    fn show_frame(&mut self, ui: &mut Ui, session: &std::sync::Arc<CanvasSession>, input: &mut Input) -> Option<Rect> {
        let ctx = ui.ctx().clone();
        self.style_once(&ctx);
        sync_native_theme(&ctx);
        self.ended_export = None;
        self.dock_rect = None;
        crate::panel::layers_dock_at(&ctx, None);
        self.canvas_pressed = std::mem::take(&mut input.canvas_pressed);
        // looked at again soon while an insert is read, or brushes made
        let brushes_coming = self.brushes_added(session);
        // the autosave told of when it fails (once until a save works again), the palettes too
        if let Some(why) = session.take_save_failure() {
            self.overlays.notice(sumi_core::save_failure_said(why), true);
        }
        // a crop or a smudge the GPU had no memory for
        if let Some(why) = session.take_failure() {
            self.overlays.notice(why, true);
        }
        if self.colours.palettes.take_failure().is_some() {
            self.overlays.notice("The palettes couldn't be kept.", true);
        }
        // a mask current paints with masks' own colour
        let on_mask = session.layers().iter().any(|l| l.current && l.kind == sumi_core::LayerType::Mask);
        self.colours.follow_mask(session, on_mask);
        let paste_shown = (self.open == Some(Popover::Actions) || self.copy_paste) && ctx.input(|i| i.focused);
        if paste_shown && !self.paste_shown {
            self.system_clipboard.offer();
        }
        self.paste_shown = paste_shown;
        let pasting = self.pasted(session);
        if self.inserted(session) || brushes_coming || pasting {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        // the pen's barrel buttons: what Gesture controls set Pen button and Pen second button to
        // do (the first, set to nothing, the eraser and back; set to erase, the eraser while it is
        // held), where the pointer is; one set to the eyedropper picks while it is held, where the
        // pen hovers, and lets go to take it; with the canvas out of sight they do nothing, then
        // or later: the tool would change unseen (and one held to erase is let go)
        let covered = self.studio.open || self.time_lapse.replaying();
        let presses = crate::pen::take_button_presses().map(|n| if covered { 0 } else { n });
        let held = crate::pen::buttons_held().map(|h| h && !covered);
        let purposes = [Trigger::PenDoubleTap, Trigger::PenSqueeze].map(|t| self.gestures.purpose_of(t));
        let (x, y) = input.cursor().unwrap_or_default();
        for (button, (&pressed, &is_held)) in presses.iter().zip(&held).enumerate() {
            if purposes.get(button).copied().flatten() == Some(GesturePurpose::Eyedropper) {
                if is_held || pressed > 0 {
                    self.overlays.pick(&ctx, session, x, y);
                    self.pen_picking = true;
                    ctx.request_repaint();
                }
            } else {
                for _ in 0..pressed {
                    input.commands.extend(session.touch_pen_button(button == 1, x, y));
                    self.library.tool_changed();
                }
            }
            // let go since the last frame (or pressed and let go within this one)
            let was_held = self.pen_held.get(button).copied().unwrap_or(false);
            if !is_held && (was_held || pressed > 0) && session.touch_pen_button_up(button == 1) {
                self.library.tool_changed();
            }
        }
        self.pen_held = held;
        let eyedropper_held = held.iter().zip(&purposes).any(|(h, p)| *h && *p == Some(GesturePurpose::Eyedropper));
        if self.pen_picking && !eyedropper_held {
            self.pen_picking = false;
            if let Some(rgb) = self.overlays.finish_pick() {
                self.colours.set_rgb(session, rgb);
            }
        }
        self.commands(&ctx, session, input);
        // the brush library closed: what was done in it is kept
        let brushes_open = self.open == Some(Popover::Brushes);
        self.keep_brushes |= self.brushes_were_open && !brushes_open;
        self.brushes_were_open = brushes_open;
        let info = session.info();
        // a snapped shape's handles sit at window points, and the view can move under them from a
        // zoom, a pan or a turn that comes in through the input layer rather than as a command
        if self.overlays.shape.is_some() {
            self.overlays.shape = session.quickshape_info();
        }
        self.follow_brush(session, info.tool);
        self.shortcuts(&ctx, session, &info);
        self.overlays.cursor_style = self.prefs.cursor_style;
        input.hover_shown = false;
        input.turns_model = self.model && !session.model_flat();
        // a mode over the canvas takes the brush's drag with it
        if !self.canvas_shown() {
            input.set_painting(false);
        }
        // the chrome recedes a few percent while a stroke is painted on the canvas, and breathes
        // back as it ends: one smoothed scalar that the bars, the side bar and the dock all read.
        // Reduce motion (the system's, which is the session's too) keeps it still: no dim at all
        // rather than a still one, as a bar that changes as a stroke begins is motion however quiet.
        let stroke = self.canvas_shown()
            && input.pressing()
            && session.adjustment_shown().adjustment.is_none()
            && !self.selection.on
            && !self.transform.on;
        let target = crate::look::chrome_dim_target(stroke, crate::colour_drop::reduce_motion());
        // egui may run the UI twice in one frame when a pass is discarded for layout; both passes
        // report the same `cumulative_frame_nr`, so step the dim only for the first pass of a frame.
        let frame = ctx.cumulative_frame_nr();
        if crate::look::dim_step_due(frame, self.dim_frame) {
            self.dim_frame = frame;
            let seconds = ctx.input(|i| i.stable_dt).clamp(0.0, 0.25);
            self.chrome_dim = crate::look::settle_dim(self.chrome_dim, target, seconds);
            // only while the dim is still settling towards its target, never a frame for nothing
            if crate::look::dim_needs_repaint(self.chrome_dim, target) {
                ctx.request_repaint();
            }
        }
        crate::panel::chrome_dim_at(&ctx, self.chrome_dim);
        session.set_touch_companion(match self.companion.mode {
            crate::companion::Mode::None => None,
            crate::companion::Mode::Zoom => Some(sumi_core::CompanionMode::Zoom),
            crate::companion::Mode::Move => Some(sumi_core::CompanionMode::Move),
        });
        self.recolor_frame(&ctx, session);
        if let Some(opacity) = session.touch_layer_opacity().filter(|_| !self.bars_hidden) {
            crate::adjustments::layer_opacity_bar(&ctx, opacity, pc::BAR_HEIGHT);
        }
        let colour = srgb(self.colours.rgb());
        self.time_lapse.record(&ctx, session, input.idle());
        // the brush studio over everything (the library stays open under it)
        if let Some(new) = self.library.studio.take() {
            self.studio.plugin_rows = crate::studio::plugin_rows(&self.prefs.plugins);
            self.studio.begin(&ctx, session, new);
        }
        if self.studio.open {
            self.canvas_rect = Rect::NOTHING;
            self.animation.reset(session);
            if let Some(crate::studio::Asked::Closed) = self.studio.show(&ctx, &mut self.look, session, &mut self.prefs)
            {
                self.library.library_changed();
                self.keep_brushes = true;
            }
            session.set_backdrops(Vec::new());
            self.popover_open = false;
            return None;
        }
        // the time-lapse replayed over everything
        if self.time_lapse.replaying() {
            self.canvas_rect = Rect::NOTHING;
            self.animation.reset(session);
            if let Some(said) = self.time_lapse.show(&ctx, &mut self.look, &session.name()) {
                crate::files::tell("Time-lapse", said);
            }
            session.set_backdrops(Vec::new());
            self.popover_open = false;
            return None;
        }
        // sharing as an animation: its screen over everything
        if let Some(export) = self.animated_export.as_mut() {
            self.canvas_rect = Rect::NOTHING;
            self.animation.reset(session);
            let ended = export.show(&ctx, &mut self.look, session);
            if let Some(ended) = ended {
                self.ended_export = self.animated_export.take();
                if matches!(ended, crate::animated_export::Ended::Failed) {
                    crate::files::tell("Share", "The animation could not be exported.");
                }
                if std::mem::take(&mut self.exporting_from_gallery) {
                    self.actions.push(Action::Gallery);
                }
            }
            session.set_backdrops(Vec::new());
            self.popover_open = false;
            return None;
        }
        // the Lighting Studio: the model among its lights, and the studio's bar
        if self.lighting.open {
            self.canvas_rect = Rect::EVERYTHING;
            if let Some(notice) = self.lighting.show(&ctx, &mut self.look, session) {
                self.overlays.notice(notice, true);
            }
            self.overlays.show(ui, &mut self.look, colour, false);
            session.set_backdrops(Vec::new());
            self.popover_open = true;
            return None;
        }
        // Crop and Resize: the canvas, its frame and header alone
        if self.crop.open {
            self.canvas_rect = Rect::EVERYTHING;
            self.crop.show(&ctx, &mut self.look, session);
            session.set_backdrops(Vec::new());
            self.popover_open = false;
            return None;
        }
        // the drawing guide's editor: the canvas, the guide and the editor alone
        if self.guide_editor.open {
            self.canvas_rect = Rect::EVERYTHING;
            guide_lines(ui.painter(), session);
            let mut glass = Vec::new();
            self.guide_editor.show(&ctx, &mut self.look, session, &mut glass);
            session.set_backdrops(glass);
            self.popover_open = false;
            return None;
        }
        // the drawing guide over the canvas, under the bars (and shown full screen too)
        guide_lines(ui.painter(), session);
        if self.bars_hidden {
            // full screen: the canvas alone (the overlays still show)
            self.canvas_rect = Rect::EVERYTHING;
            self.dock_rect = None;
            self.navigator.close();
            crate::panel::layers_dock_at(&ctx, None);
            session.set_fit_insets(0.0, 0.0, 0.0, 0.0);
            self.hover_cursor(&ctx, session, &info, input);
            self.overlays.show(ui, &mut self.look, colour, false);
            self.drop.show(&ctx, session, to_hex(self.colours.rgb()), 0.0);
            self.copy_paste_panel(&ctx, session);
            self.layer_choice(&ctx, session);
            self.quick_menu(&ctx, session);
            self.blocked_alert(&ctx, session);
            self.pdf_quality_alert(&ctx);
            self.plugin_alert(&ctx);
            self.note_alert(&ctx);
            self.export_card.show(&ctx, &mut self.look);
            self.touch_companion(&ctx, session);
            if self.show_interface_button(&ctx) {
                self.toggle_bars(session);
            }
            self.text_editor(&ctx, session);
            session.set_backdrops(Vec::new());
            self.popover_open = self.text.editing();
            return None;
        }
        let dock = self.layers_dock_rect(&ctx, session);
        self.dock_rect = dock;
        crate::panel::layers_dock_at(&ctx, dock);
        // a popover left open from a narrow window gives way to the dock
        if Self::layers_dock_mode(&ctx) && self.open == Some(Popover::Layers) {
            self.open = None;
            self.layers.close_menus();
        }
        // hidden by a mode that takes the canvas, or left behind by a narrower window: nothing
        // of the dock's stays open (except the narrow popover that is actually up)
        if dock.is_none() && self.open != Some(Popover::Layers) {
            self.layers.close_menus();
        }
        let lit = Lit {
            actions: self.open == Some(Popover::Actions),
            transform: self.transform.on,
            selection: self.selection.on,
            adjustments: self.open == Some(Popover::Adjustments)
                || session.adjustment_shown().adjustment.is_some()
                || session.recolor_on()
                || session.touch_layer_opacity().is_some(),
            layers: self.open == Some(Popover::Layers) || self.background_pick.is_some(),
            layers_docked: dock.is_some(),
            // the ordinary dock keeps the active tool lit; its menus and the colour picker are modal
            panel_open: matches!(self.open, Some(Popover::Layers | Popover::Colour)) || self.background_pick.is_some(),
        };
        let bar = crate::chrome::top_bar(
            ui,
            &mut self.look,
            &info,
            &session.current_brush().name,
            colour,
            self.colours.hsv[2],
            &lit,
        );
        let held = self.tool_hold(&ctx, session, &bar);
        match bar.pressed.filter(|_| !held) {
            Some(BarButton::Gallery) => self.actions.push(Action::Gallery),
            Some(BarButton::Actions) => self.toggle(session, Popover::Actions),
            Some(BarButton::Layers) => self.layers_pressed(&ctx, session),
            Some(BarButton::Brushes) => self.toggle(session, Popover::Brushes),
            Some(b @ (BarButton::Paint | BarButton::Smudge | BarButton::Erase)) => {
                let tool = match b {
                    BarButton::Smudge => ToolKind::Smudge,
                    BarButton::Erase => ToolKind::Erase,
                    _ => ToolKind::Paint,
                };
                // leaving a mode for a tool keeps what it made, and only picks the tool; the chosen
                // tool again opens its brushes
                let goes_on = self.switch_modes(session, ModeButton::Tool);
                if goes_on && info.tool == tool && self.open.is_none_or(|o| o == Popover::Brushes) {
                    self.toggle(session, Popover::Brushes);
                } else {
                    self.choose_tool(session, tool);
                    self.open = None;
                }
            }
            Some(BarButton::Transform) => self.toggle_transform(session),
            Some(BarButton::Selection) => self.toggle_selection(session),
            Some(BarButton::Adjustments) => self.adjustments_pressed(session),
            Some(BarButton::Colour) | None => {}
        }
        // the colour button: tapped it opens the colours, held it goes back to the previous
        // colour, and dragged out its colour fills where it is let go (ColorDrop)
        if let Some(rect) = bar.rect(BarButton::Colour) {
            let id = ui.scope_id().with("colour button");
            let rgb = to_hex(self.colours.rgb());
            match self.drop.source(ui, session, id, rect, rgb, false, true, None) {
                Some(Touch::Tap) => self.toggle(session, Popover::Colour),
                Some(Touch::Hold) => {
                    if self.colours.previous(session) {
                        self.overlays.notice("Previous colour", false);
                    }
                }
                Some(Touch::Dragged) => self.open = None,
                None => {}
            }
        }
        let status_rect = (self.canvas_info.is_none()
            && !self.gesture_controls.open
            && self.status_bar_shown(ctx.content_rect().width()))
        .then(|| crate::chrome::status_bar_rect(ctx.content_rect(), dock))
        .flatten();
        self.canvas_rect = canvas_without_chrome(ui.available_rect_before_wrap(), dock, status_rect);
        // the bars are glass over the canvas: the top bar's frost, the side bar's and its tracks'
        let (mut backdrops, mut windows) = (Vec::new(), Vec::new());
        let top_bar = ctx.content_rect().with_max_y(ctx.content_rect().top() + crate::look::pc::BAR_HEIGHT);
        crate::chrome::frost(&mut backdrops, top_bar, 0.0, ctx.pixels_per_point());
        // a mode taking the screen puts the side bar away
        let adjusting = session.adjustment_shown();
        let side_bar = self.side_bar_shown
            && !self.transform.on
            && !self.selection.on
            && session.touch_layer_opacity().is_none()
            && !self.text.editing()
            && (adjusting.adjustment.is_none() || adjusting.pencil);
        crate::panel::side_bar_at(&ctx, self.side_bar_shown, self.prefs.right_handed);
        let taps = if side_bar {
            let modify_lit = session.pick_next();
            let right = self.prefs.right_handed;
            self.side_bar.show(ui, &mut self.look, session, &info, modify_lit, right, &mut backdrops, &mut windows)
        } else {
            crate::chrome::SideTaps::default()
        };
        self.keep_brushes |= taps.brush_changed;
        if taps.undo {
            self.undo(session);
        }
        if taps.redo {
            self.redo(session);
        }
        // the modify button tapped does what Tap □ is set to (opens or closes the eyedropper at
        // first); held, touches do what □ + Touch and □ + Pen are set to (unless one did, its tap
        // then does nothing)
        // Alt held is the button held; tapped it does nothing, being in the system's own shortcuts
        let used = session.set_modify_held(taps.modify_down || input.modify_key());
        // Space tapped, not moving the canvas, opens or closes the QuickMenu (or does what a plugin
        // rebinding keys takes it as)
        let space_tapped = std::mem::take(&mut input.space_tapped);
        let to_panel = std::mem::take(&mut self.space_to_panel);
        self.space_to_panel = to_panel && !space_tapped && input.moving_view();
        if space_tapped && !to_panel && !self.keys_taken() && self.canvas_shown() {
            let space = KeyCombo { key: "space".into(), command: false, shift: false, alt: false };
            if let Some(press) = self.prefs.plugins.key_press(space) {
                self.press(&ctx, session, &info, &press);
            }
        }
        if taps.modify_tapped && !used {
            self.tap_modify(&ctx, session);
        }
        self.hover_cursor(&ctx, session, &info, input);
        self.selection_pointer(&ctx, input);
        if let Some(kind) = self.overlays.show(ui, &mut self.look, colour, true) {
            self.overlays.shape = session.quickshape_choose(kind).or(self.overlays.shape.take());
        }
        // the notice band frosts the canvas under it
        if let Some(r) = self.overlays.frost.take() {
            let ppp = ctx.pixels_per_point();
            backdrops.extend([r.left(), r.top(), r.right(), r.bottom(), 0.0].map(|v| v * ppp));
            backdrops.push(1.0);
        }
        // its bar tapped, the snapped shape's handles show and take drags
        session.set_shape_editing(self.overlays.shape_editing);
        let tool_button = match info.tool {
            ToolKind::Smudge => BarButton::Smudge,
            ToolKind::Erase => BarButton::Erase,
            ToolKind::Paint => BarButton::Paint,
        };
        let anchor = |p: Popover| match p {
            Popover::Actions => bar.rect(BarButton::Actions),
            Popover::Adjustments => bar.rect(BarButton::Adjustments),
            Popover::Brushes => bar.rect(BarButton::Brushes).or_else(|| bar.rect(tool_button)),
            Popover::Layers => bar.rect(BarButton::Layers),
            Popover::Colour => bar.rect(BarButton::Colour),
        };
        // it opens as the adjustments, however it was left
        if self.open != Some(Popover::Adjustments) {
            self.adjustments.showing_filters = false;
        }
        // the persistent Layers dock, behind any popover that opens over it
        if let Some(dock) = self.dock_rect {
            self.layers.plugin_actions = self.prefs.plugins.actions_in(sumi_core::PluginPlace::Layer);
            self.layers.layer_kinds = self.prefs.plugins.layer_kinds();
            let dock_modal = self.layers.modal();
            // its own menus/alerts stay live; otherwise the body gives way to another popover or
            // the background's picker, and raw touches never run under either or under a modal
            let interactive = dock_modal || (self.background_pick.is_none() && self.open.is_none());
            let gestures = self.background_pick.is_none() && self.open.is_none() && !dock_modal;
            // the Navigator shares the dock, above the Layers: the same width and frame, where
            // the dock is tall enough for both and the canvas is a page rather than a 3D scene
            let model_scene = session.model_info().is_some() && !session.model_flat();
            let (navigator, layers) = crate::navigator::Navigator::shown(dock, model_scene)
                .map_or((None, dock), |(navigator, layers)| (Some(navigator), layers));
            match navigator {
                Some(section) => {
                    // where the view is read: the window less the interface both keep clear
                    let area = self.canvas_area(ctx.content_rect());
                    if self.navigator.show(
                        &ctx,
                        &mut self.look,
                        session,
                        section,
                        area,
                        !self.prefs.right_handed,
                        self.reference.on,
                        interactive,
                    ) {
                        self.reference.on = !self.reference.on;
                    }
                }
                None => self.navigator.close(),
            }
            _ = self.layers.show_dock(
                &ctx,
                &mut self.look,
                session,
                &mut self.colours,
                layers,
                !self.prefs.right_handed,
                interactive,
                gestures,
                &mut backdrops,
            );
            self.layers_events(&ctx, session);
        } else {
            // no dock (compact window, full screen, a mode taking the canvas): no Navigator either
            self.navigator.close();
        }
        if let Some(open) = self.open {
            self.popover(&ctx, session, open, anchor(open), &mut backdrops);
        }
        // the background's picker is transient over the dock, separate from it
        if self.background_pick.is_some()
            && self.open != Some(Popover::Layers)
            && Self::layers_dock_mode(&ctx)
            && self.prefs.layers_dock
        {
            if let Some(button) = bar.rect(BarButton::Layers) {
                let modal = self.colours.modal();
                let (shown, done) = self.background_panel(&ctx, session, button, &mut backdrops);
                let outside = !self.canvas_pressed
                    && ctx.input(|i| {
                        i.pointer.any_pressed()
                            && i.pointer.interact_pos().is_some_and(|p| !shown.contains(p) && !button.contains(p))
                    });
                // a menu/hex pad takes Escape itself; otherwise only an Escape no one else took
                // closes it, and it is marked taken so the canvas does not also settle upright
                let escaped = !modal && escape_untaken(&ctx) && escape(&ctx);
                if Self::picker_dismissed(done, modal, outside, escaped) {
                    self.end_background_pick(session);
                }
            } else {
                self.end_background_pick(session);
            }
        }
        // the background's colour is kept, however its panel closed
        let picker_open = self.background_pick.is_some()
            && !self.bars_hidden
            && (self.open == Some(Popover::Layers) || (Self::layers_dock_mode(&ctx) && self.prefs.layers_dock));
        if !picker_open {
            self.end_background_pick(session);
        }
        // the colour panel dragged out by its grabber: a window over the canvas until closed, its
        // Classic as the plugins on now draw it
        if self.colours.floating.is_some() {
            self.colours.set_triangle(self.prefs.plugins.colour_triangle());
            let picker = self.prefs.plugins.picker_on().then(|| (Arc::clone(&self.prefs.plugins), Arc::clone(session)));
            self.colours.set_picker(picker);
        }
        self.colours.show_floating(&ctx, &mut self.look, session, &mut backdrops);
        // the colour model chosen on the panel, kept; the panel works in the one kept
        if let Some(model) = self.colours.model_chosen.take() {
            self.prefs.colour_model = model;
            self.prefs.keep();
        }
        self.colours.set_model(self.prefs.colour_model);
        if self.open != Some(Popover::Actions) {
            self.actions_panel.closed();
        }
        self.transform.show(&ctx, &mut self.look, session, &mut backdrops);
        self.plugin_panel(&ctx, session, &mut backdrops);
        crate::model_picture::show(&ctx, &mut self.look, session);
        self.selection.pointer_suppressed = input.polyline_gesture;
        if let Some(on) = self.selection.show(&ctx, &mut self.look, session, pc::BAR_HEIGHT, self.model) {
            self.prefs.selection_x_ray = on;
            self.prefs.keep();
        }
        if input.polyline_gesture && input.idle() {
            input.polyline_gesture = false;
        }
        input.polyline = self.selection.polyline_armed();
        if let Some(asked) = self.adjustments.show(
            &ctx,
            &mut self.look,
            session,
            pc::BAR_HEIGHT,
            &self.gradients,
            &self.colours,
            &mut backdrops,
        ) {
            self.adjustment_asked(session, asked);
        }
        let hold = std::time::Duration::from_secs_f32(self.gestures.delay(GesturePurpose::Eyedropper).max(0.0));
        let picked = self.reference.show(&ctx, &mut self.look, session, hold);
        // kept with the artwork as it changes, a toggle elsewhere last frame included
        self.reference.keep(session);
        match picked {
            Some(crate::reference::Picked::Moving { at, side, rgba, rgb }) => {
                self.overlays.pick_reference(&ctx, at, side, &rgba, rgb);
            }
            Some(crate::reference::Picked::Done) => {
                if let Some(rgb) = self.overlays.finish_pick() {
                    self.colours.set_rgb(session, rgb);
                }
            }
            None => {}
        }
        // a full-screen page covers the timeline, as it covers the canvas
        let mut fit_rect = None;
        if self.canvas_info.is_none() && !self.gesture_controls.open {
            if self.status_bar_shown(ctx.content_rect().width()) {
                fit_rect = crate::chrome::status_bar(&ctx, session, &info, self.dock_rect);
            }
            self.animation.tweens = self.prefs.plugins.tweens();
            self.animation.frame_actions = self.prefs.plugins.actions_in(sumi_core::PluginPlace::Frame);
            let said = self.animation.show(&ctx, &mut self.look, session, &self.colours.palettes, self.colours.harmony);
            if let Some(key) = self.animation.run_action.take() {
                self.run_plugin_action(&ctx, session, key);
            }
            if let Some(crate::animation::Said::NoRoom) = said {
                crate::files::no_room(session);
            }
        } else {
            self.animation.reset(session);
        }
        self.drop.show(&ctx, session, to_hex(self.colours.rgb()), pc::BAR_HEIGHT);
        self.copy_paste_panel(&ctx, session);
        self.layer_choice(&ctx, session);
        self.quick_menu(&ctx, session);
        self.blocked_alert(&ctx, session);
        _ = self.model_warning_alert.show(&ctx, &mut self.look);
        self.pdf_quality_alert(&ctx);
        self.plugin_alert(&ctx);
        self.note_alert(&ctx);
        self.export_card.show(&ctx, &mut self.look);
        self.text_editor(&ctx, session);
        // a gesture setting changed: the touches follow it, and it is kept
        if self.gesture_controls.show(&ctx, &mut self.look, &self.gestures) {
            session.set_touch_settings(self.gestures.touch_settings(self.prefs.rapid_undo_ms()));
            crate::files::keep_gestures(&self.gestures.kept());
        }
        if let Some(info) = self.canvas_info.as_mut() {
            use crate::canvas_info::Ended;
            let ended = info.show(&ctx, &mut self.look, session);
            if let Some(failure) = info.take_failure() {
                self.overlays.notice(failure, true);
            }
            match ended {
                // another profile: the canvas and the current colour turned into it
                Some(Ended::Done(Some(profile))) => {
                    let (from, rgb) = (session.profile(), to_hex(self.colours.rgb()));
                    if session.change_profile(profile.clone()) {
                        self.colours.set_rgb(session, crate::canvas_info::colour_in(rgb, &from, &profile));
                    }
                    self.canvas_info = None;
                }
                Some(_) => self.canvas_info = None,
                None => {}
            }
        }
        if let Some(said) = self.time_lapse.show(&ctx, &mut self.look, &session.name()) {
            crate::files::tell("Time-lapse", said);
        }
        self.touch_companion(&ctx, session);
        self.advanced_settings(&ctx, session);
        if self.licences && crate::licences::show(&ctx) {
            self.licences = false;
        }
        let keys_to_canvas = self.canvas_shown() && !ctx.egui_wants_keyboard_input() && !self.keys_taken();
        self.shortcut_list.show(&ctx, keys_to_canvas, &self.prefs.plugins);
        self.name_colour(&ctx);
        self.window_width = Some(ctx.content_rect().width());
        self.place_canvas(session, ctx.pixels_per_point());
        session.set_glass(backdrops, windows);
        if input.moving_view() && !ctx.is_pointer_over_egui() {
            let icon = if input.zooming_view() { egui::CursorIcon::ZoomIn } else { egui::CursorIcon::Grab };
            ctx.set_cursor_icon(icon);
        }
        // an ordinary dock lets the canvas be worked as ever; only its menus, the colour picker
        // and the other popovers take the presses
        let picker_open = self.background_pick.is_some()
            && !self.bars_hidden
            && (self.open == Some(Popover::Layers) || (Self::layers_dock_mode(&ctx) && self.prefs.layers_dock));
        let dock_modal = self.dock_rect.is_some() && self.layers.modal();
        self.popover_open = self.open.is_some() || self.settings || self.text.editing() || dock_modal || picker_open;
        self.panel_lets_through = self.open.is_some_and(|p| !self.modal(p))
            && !dock_modal
            && !picker_open
            && !self.settings
            && !self.text.editing();
        session.set_touch_settings(self.effective_touch_settings());
        fit_rect
    }

    /// The touch companion, with its switch on in Advanced settings.
    fn touch_companion(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        use crate::companion::Asked;
        if !self.prefs.touch_companion {
            return;
        }
        let mode = self.companion.mode;
        match self.companion.show(ctx, &mut self.look) {
            Some(Asked::Undo) => self.undo(session),
            Some(Asked::Redo) => self.redo(session),
            Some(Asked::Fit) => session.settle_to_fit(),
            None => {}
        }
        // its Zoom or Move turned on or off
        if self.companion.mode != mode {
            self.feedback();
        }
    }

    /// A click for an action done, with Feedback Sounds on.
    fn feedback(&self) {
        if self.prefs.feedback_sounds {
            crate::sound::click();
        }
    }

    /// Recolor on, the reticle in the middle of the window, or off (keeping what it filled).
    fn toggle_recolor(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        if !session.recolor_on() && !self.takes_changes(session) {
            return;
        }
        // on, it ends; else the other modes end
        if !self.switch_modes(session, ModeButton::Recolor) {
            return;
        }
        self.open = None;
        let centre = ctx.content_rect().center() * ctx.pixels_per_point();
        self.recolor_rgb = self.colours.rgb();
        let [r, g, b] = self.recolor_rgb;
        session.recolor_start(centre.x, centre.y, r, g, b, self.prefs.recolor_threshold);
    }

    /// Layer Opacity on for the current layer (a paint layer or a group): a drag across the canvas
    /// changes its opacity, read in the band under the top bar. The other modes end.
    fn start_layer_opacity(&mut self, session: &CanvasSession) {
        use sumi_core::LayerType;
        let Some(layer) = session.layers().into_iter().find(|l| l.current) else { return };
        if !matches!(layer.kind, LayerType::Paint | LayerType::Group) {
            return;
        }
        self.switch_modes(session, ModeButton::LayerOpacity);
        self.open = None;
        session.set_touch_layer_opacity(Some(layer.id));
    }

    /// Recolor ends, keeping what it filled; its Flood kept.
    fn end_recolor(&mut self, session: &CanvasSession) {
        if session.recolor_on() {
            session.recolor_end();
            self.prefs.keep();
        }
    }

    /// Recolor, while on: its reticle and toolbar, the drags and taps on the canvas, the colour
    /// changing; a panel opening (but Colors) ends it.
    fn recolor_frame(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        if !session.recolor_on() {
            return;
        }
        if self.open.is_some_and(|p| p != Popover::Colour) {
            self.end_recolor(session);
            return;
        }
        let rgb = self.colours.rgb();
        let threshold = self.prefs.recolor_threshold;
        if rgb != self.recolor_rgb {
            self.recolor_rgb = rgb;
            let [r, g, b] = rgb;
            session.recolor_colour(r, g, b);
        }
        crate::recolor::reticle(ctx, session, rgb);
        if !self.bars_hidden
            && let Some(t) = crate::adjust_panels::recolor_toolbar(ctx, threshold)
        {
            self.prefs.recolor_threshold = t;
            session.recolor_threshold(t);
        }
    }

    /// Follows the brush in use: one chosen in place of another with the same tool becomes the
    /// one Previous brush goes back to.
    fn follow_brush(&mut self, session: &CanvasSession, tool: ToolKind) {
        let b = session.current_brush();
        let now = (tool, b.set, b.index);
        if let Some((was_tool, set, index)) = self.brush_now
            && was_tool == tool
            && (set, index) != (b.set, b.index)
        {
            self.brush_last = Some((set, index));
        }
        self.brush_now = Some(now);
    }

    /// Advanced settings' page, while it is up: a change is kept and the artwork follows it.
    fn advanced_settings(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        use crate::settings::Said;
        if !self.settings {
            return;
        }
        match crate::settings::show(ctx, &mut self.prefs) {
            Some(Said::Changed) => {
                self.prefs.apply(session);
                // the companion put away, its Zoom or Move goes off
                if !self.prefs.touch_companion {
                    self.companion.mode = crate::companion::Mode::None;
                }
            }
            Some(Said::Licences) => self.licences = true,
            Some(Said::Done) => self.settings = false,
            None => {}
        }
    }

    /// With Color Description Notifications, the colour named in a message as it changes: at
    /// once, or while it keeps changing, the latest at most every 150 ms.
    fn name_colour(&mut self, ctx: &egui::Context) {
        const GAP: std::time::Duration = std::time::Duration::from_millis(150);
        if !self.prefs.colour_names {
            self.named = None;
            return;
        }
        let rgb = to_hex(self.colours.rgb());
        let (last, at) = *self.named.get_or_insert((rgb, Instant::now()));
        if rgb == last {
            return;
        }
        let since = at.elapsed();
        if since >= GAP {
            self.overlays.notice(sumi_core::colour_name(rgb), false);
            self.named = Some((rgb, Instant::now()));
        } else {
            ctx.request_repaint_after(GAP - since);
        }
    }

    /// Text being edited, and the question about rasterizing a text layer.
    fn text_editor(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        use crate::text::Said;
        match self.text.show(ctx, &mut self.look, session) {
            Some(Said::Notice(text, warning)) => self.overlays.notice(text, warning),
            Some(Said::Undo) => self.undo(session),
            Some(Said::Redo) => self.redo(session),
            None => {}
        }
    }

    /// New text over the canvas, in the colour chosen.
    fn add_text(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        self.open = None;
        if !self.text.add(ctx, session, self.colours.rgb()) {
            crate::files::no_room(session);
        }
    }

    /// While selecting, the pointer over the canvas shows how: a fine cross where it points and
    /// the selection's kind beside it (wand, lasso, rectangle, ellipse), in place of the system's
    /// pointer. The desktop's own, as a mouse is used (the tablet shows none).
    fn selection_pointer(&mut self, ctx: &egui::Context, input: &Input) {
        if !self.selection.on || self.popover_open || !input.inside() {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let Some(at) = input.cursor().map(|(x, y)| egui::pos2(x, y) / ppp) else { return };
        if !self.canvas_rect.contains(at) || ctx.layer_id_at(at).is_some_and(|l| l.order != egui::Order::Background) {
            return;
        }
        ctx.set_cursor_icon(egui::CursorIcon::None);
        let icon = self.selection.kind_icon();
        let look = &mut self.look;
        egui::Area::new(egui::Id::unique("selection pointer"))
            .order(egui::Order::Tooltip)
            .interactable(false)
            .fixed_pos(at)
            .show(ctx, |ui| {
                fine_cross(ui.painter(), at);
                // the kind below right of it, dark in a light halo (seen on any colour)
                let beside = at + egui::vec2(20.0, 20.0);
                for nudge in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
                    look.icon(ui, icon, beside + egui::vec2(nudge.0, nudge.1), 26.0, Color32::WHITE);
                }
                look.icon(ui, icon, beside, 26.0, Color32::from_gray(0x20));
            });
        // the cross follows the pointer: ask for a frame while it moves, not at the display rate
        // with a still cursor (the window already redraws for each pointer event)
        if ctx.input(|i| i.pointer.delta().length_sq() > 0.0) {
            ctx.request_repaint();
        }
    }

    /// The brush's outline where the mouse hovers over the canvas, as the pen's hover shows it,
    /// while nothing is pressed (and while it would paint there). Where the canvas would paint, the
    /// system's pointer gives way to the outline; where none shows (the brush too small to see,
    /// its outline none, or the brush cursor off) to a fine cross; and while a stroke is drawn to
    /// nothing, the stroke showing where the pen is (with the outline, if Prefs show it then).
    /// Painting from afar (a mouse, a pen without a screen) the eye is on the screen and the hand
    /// isn't, and the system's pointer stays where the pen went down, so the outline or the cross
    /// go on through the stroke.
    fn hover_cursor(&mut self, ctx: &egui::Context, session: &CanvasSession, info: &CanvasInfo, input: &mut Input) {
        let shown = session.adjustment_shown();
        let painting = shown.adjustment.is_none() && !self.selection.on && !self.transform.on;
        // what the brush's own drag needs: the canvas would paint, and no popover has taken it
        input.set_painting(painting && !self.popover_open);
        // (the size and opacity show beside the pointer only while its drag sets them)
        self.overlays.show_readout(None);
        let on_canvas = |at: egui::Pos2| {
            self.canvas_rect.contains(at) && ctx.layer_id_at(at).is_none_or(|l| l.order == egui::Order::Background)
        };
        // Liquify's brush shows as a ring of its reach, hovering and pressed
        if let (Some(sumi_core::Adjustment::Liquify), Some(&size)) = (shown.adjustment, shown.liquify.first()) {
            let ppp = ctx.pixels_per_point();
            input.hover_shown = true;
            let at = input.hovering().map(|(x, y)| egui::pos2(x, y) / ppp).or_else(|| ctx.pointer_latest_pos());
            if let Some(at) = at.filter(|at| on_canvas(*at)) {
                ctx.set_cursor_icon(egui::CursorIcon::None);
                hover_ring(ctx, at, sumi_core::liquify_reach(size) * info.zoom / ppp);
            }
            return;
        }
        if !input.idle() {
            if painting
                && input.pressing()
                && let Some(at) = ctx.pointer_latest_pos().filter(|at| on_canvas(*at))
            {
                ctx.set_cursor_icon(egui::CursorIcon::None);
                if input.pressing_from_afar() && !self.overlays.outline_shown() && self.overlays.picking.is_none() {
                    hover_cross(ctx, at);
                }
            }
            return;
        }
        // the outline or the cross follows the pointer, so its moves draw a frame
        input.hover_shown = painting;
        let ppp = ctx.pixels_per_point();
        let at = input.hovering().filter(|_| painting).map(|(x, y)| egui::pos2(x, y) / ppp).filter(|at| on_canvas(*at));
        self.overlays.hover(session, info, ppp, at.filter(|_| self.prefs.cursor_shown(true)));
        if let Some(at) = at.filter(|_| !self.overlays.outline_shown()) {
            ctx.set_cursor_icon(egui::CursorIcon::None);
            hover_cross(ctx, at);
        }
        // a modifier drag sets the brush's size and opacity: both show beside the pointer
        let readout = input.brush_drag_at().map(|(x, y)| {
            let diameter = info.brush_diameter.round().max(0.0) as i32;
            let percent = (info.opacity * 100.0).round().clamp(0.0, 100.0) as i32;
            (egui::pos2(x, y) / ppp, format!("{diameter} px · {percent}%"))
        });
        self.overlays.show_readout(readout);
    }

    /// The modify button tapped: what Tap □ is set to do, as the Android app does it.
    fn tap_modify(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        match self.gestures.purpose_of(Trigger::TapModify) {
            Some(GesturePurpose::Eyedropper) => session.set_pick_next(!session.pick_next()),
            Some(GesturePurpose::Quickmenu) => self.quick_menu.toggle(ctx.content_rect().center()),
            Some(GesturePurpose::CopyPaste) => self.copy_paste = true,
            Some(GesturePurpose::Quickshape) => {
                if let Some(info) = session.quickshape_last() {
                    self.snapped(info);
                }
            }
            _ => {}
        }
    }

    /// Does what the touches asked.
    fn commands(&mut self, ctx: &egui::Context, session: &CanvasSession, input: &mut Input) {
        let mut slid = None;
        let mut sliding = None;
        let mut stroke_ended = false;
        for command in std::mem::take(&mut input.commands) {
            stroke_ended |= command == TouchCommand::StrokeEnded;
            match command {
                TouchCommand::Undo => self.undo(session),
                TouchCommand::Redo => self.redo(session),
                TouchCommand::ClearLayer => self.clear_layer(session),
                TouchCommand::ToggleBars => self.toggle_bars(session),
                TouchCommand::Fit => session.settle_to_fit(),
                TouchCommand::QuickShapeLast => {
                    if let Some(info) = session.quickshape_last() {
                        self.snapped(info);
                    }
                }
                TouchCommand::Pick { x, y } => self.overlays.pick(ctx, session, x, y),
                TouchCommand::PickDone => {
                    if let Some(rgb) = self.overlays.finish_pick() {
                        self.colours.set_rgb(session, rgb);
                    }
                }
                TouchCommand::PickCancel => self.overlays.picking = None,
                TouchCommand::ShapeSnapped => {
                    if let Some(info) = session.quickshape_info() {
                        self.snapped(info);
                    }
                }
                // a snapped shape the pen moved, dragged or snapped perfect: as it now is
                TouchCommand::ShapeChanged | TouchCommand::StrokeEnded => {
                    if self.overlays.shape.is_some() {
                        self.overlays.shape = session.quickshape_info().or(self.overlays.shape.take());
                    }
                }
                TouchCommand::ShapeEnded => {
                    self.overlays.shape = None;
                    self.overlays.shape_editing = false;
                }
                // (from afar it shows however it is set to, the hand not being on the stroke)
                TouchCommand::Cursor { x, y }
                    if self.prefs.cursor_shown(false) || (self.prefs.brush_cursor && input.pressing_from_afar()) =>
                {
                    self.overlays.cursor(ctx, session, Some((x, y)));
                }
                // not shown while painting: the outline hovering where the stroke began goes away
                // (else it stays there, left behind)
                TouchCommand::Cursor { .. } => self.overlays.cursor(ctx, session, None),
                TouchCommand::CursorGone => self.overlays.cursor(ctx, session, None),
                TouchCommand::FillThreshold { threshold } => {
                    self.drop.slid(threshold);
                    slid = Some(threshold);
                }
                TouchCommand::Filled => {
                    self.drop.filled();
                    slid = None;
                }
                TouchCommand::TransformTouched => self.transform.touched(),
                TouchCommand::TransformMoved { transform } => self.transform.moved(transform),
                TouchCommand::TransformHeld { handle, x, y } => self.transform.held(handle, egui::pos2(x, y)),
                TouchCommand::TransformLetGo { handle, x, y } => self.transform.let_go(handle, egui::pos2(x, y)),
                TouchCommand::SelectionDrag { x0, y0, x1, y1 } => {
                    self.selection.dragged(session, egui::pos2(x0, y0), egui::pos2(x1, y1));
                }
                TouchCommand::SelectionThreshold { x, y, threshold, done } => {
                    self.selection.slid(session, egui::pos2(x, y), threshold, done);
                    sliding = (!done).then_some((x, y, threshold));
                }
                TouchCommand::SelectionChanged => self.selection.changed(session),
                // the freehand path is read as it is drawn, each frame
                TouchCommand::FreehandChanged => {}
                // an adjustment's amount is read as it is drawn, each frame; a touch on the canvas
                // closes Layer / Pencil
                TouchCommand::AdjustChanged => self.adjustments.modes_open = false,
                TouchCommand::AdjustMenu { x, y } => {
                    self.adjustments.modes_open = false;
                    self.adjustments.menu_at = Some(egui::pos2(x, y) / ctx.pixels_per_point());
                }
                TouchCommand::TransformNumeric { angle, x, y, right } => {
                    self.transform.numbers(angle, egui::pos2(x, y) / ctx.pixels_per_point(), right);
                }
                // Recolor's reticle moved or tapped elsewhere: the fill follows at once
                TouchCommand::RecolorMoved { x, y } => session.recolor_move_to(x, y),
                TouchCommand::RecolorTapped { x, y } => session.recolor_tap(x, y),
                TouchCommand::CopyPaste => self.copy_paste = true,
                TouchCommand::LayerSelect { x, y } => self.layer_select(ctx, session, x, y),
                TouchCommand::QuickMenu { x, y } => self.quick_menu.toggle(egui::pos2(x, y) / ctx.pixels_per_point()),
                // what a pen's own settings ask its button to show
                TouchCommand::ShowColours => self.toggle(session, Popover::Colour),
                TouchCommand::ShowBrushes => self.toggle(session, Popover::Brushes),
                // a press on the canvas with a panel open: dragged or held, it goes on; a tap only
                // closes the panel
                TouchCommand::PanelFades | TouchCommand::PanelTapped => self.open = None,
                // a stroke or a fill on a text layer: it takes them once rasterized
                TouchCommand::TextLayer => self.text.ask_rasterize(),
                // (the pen's hover, which sets the brush with a finger, isn't the desktop's)
                TouchCommand::ViewMoved | TouchCommand::BrushChanged => {}
            }
        }
        // a slid threshold redoes the whole fill: once a frame, at the last
        if let Some(threshold) = slid {
            session.pour_threshold(threshold);
        }
        // and so does a colour selection's
        if let Some((x, y, threshold)) = sliding {
            session.show_colour_selection(x, y, threshold);
        }
        // a stroke on a locked or hidden layer, or a group, drew nothing: say why (a text layer
        // takes none until it is rasterized, which it asks about itself)
        let on_text = session.layers().iter().any(|l| l.current && l.text);
        if stroke_ended && !on_text {
            let why = session.why_unpainted();
            self.say_blocked(session, why);
        }
    }

    /// A stroke snapped to a shape: its bar shows, and the notice says what it is.
    fn snapped(&mut self, info: sumi_core::QuickShapeInfo) {
        self.overlays.notice(snapped_to(info.kind), false);
        self.overlays.shape = Some(info);
        self.overlays.shape_editing = false;
    }

    /// Undo or redo stepped through a snapped shape: as it now is, if it is still there.
    fn follow_shape(&mut self, session: &CanvasSession) {
        self.overlays.shape = session.quickshape_info();
        self.overlays.shape_editing &= self.overlays.shape.is_some();
    }

    /// Undo (recoloring, the core takes back the open fill first).
    fn undo(&mut self, session: &CanvasSession) {
        self.feedback();
        // transforming, the transform's last change goes back, the box with it
        if self.transform.on {
            self.transform.moved(session.transform_apply(sumi_core::TransformAction::Undo));
            return;
        }
        // a finished text's step goes back to editing that text first, as a step of its own
        if !self.text.editing()
            && !self.transform.on
            && let Some(id) = session.text_undo_layer()
            && self.text.edit(session, id)
        {
            return;
        }
        // editing text, a change to its box's width goes back a step at a time, still editing
        if self.text.editing()
            && let Some(name) = session.text_box_undo()
        {
            self.text.reread(session);
            self.step_notice(session, true, Some(name));
            return;
        }
        // undoing while editing text otherwise undoes the whole edit
        self.text.end(session, true);
        let could = session.info().can_undo;
        let name = session.undo();
        if could || name.is_some() {
            self.step_notice(session, true, name);
        }
        self.follow_shape(session);
        // a model's tile chosen before comes back, with its channel's colour
        self.layers.follow_channel(session, &mut self.colours);
    }

    /// The notice an undo (`undo`) or a redo of the step `name` shows, unless a plugin hides it.
    fn step_notice(&mut self, session: &CanvasSession, undo: bool, name: Option<String>) {
        if let Some(text) = session.step_notice(undo, name) {
            self.overlays.notice(text, false);
        }
    }

    fn redo(&mut self, session: &CanvasSession) {
        self.feedback();
        // transforming, nothing is redone until it ends
        if self.transform.on {
            return;
        }
        if self.text.editing()
            && let Some(name) = session.text_box_redo()
        {
            self.text.reread(session);
            self.step_notice(session, false, Some(name));
            return;
        }
        // text being edited is kept first
        let editing = self.text.editing();
        self.text.end(session, true);
        let could = session.info().can_redo;
        // a text edit redone is edited again
        let text = if editing { None } else { session.text_redo_layer() };
        let name = session.redo();
        if could {
            self.step_notice(session, false, name);
        }
        self.follow_shape(session);
        self.layers.follow_channel(session, &mut self.colours);
        if let Some(id) = text {
            self.text.edit(session, id);
        }
    }

    fn clear_layer(&mut self, session: &CanvasSession) {
        if self.takes_changes(session) {
            session.clear_layer();
            self.overlays.notice("Layer cleared", false);
        }
    }

    /// Layer Select at window point (`x`, `y`): the top layer drawn there becomes current, and
    /// the others drawn there are listed to choose from.
    fn layer_select(&mut self, ctx: &egui::Context, session: &CanvasSession, x: f32, y: f32) {
        // on a model, the mesh there is picked
        if self.model && !session.model_flat() {
            if let Some(said) = session.pick_submesh_at(x, y) {
                self.layers.follow_picked_mesh(session);
                self.overlays.notice(said, false);
            }
            return;
        }
        let ids = session.layers_at(x, y);
        let Some(&top) = ids.first() else {
            crate::files::say("Nothing is drawn there.");
            return;
        };
        session.select_layer(top);
        self.layer_choice = (ids.len() > 1).then(|| (egui::pos2(x, y) / ctx.pixels_per_point(), ids));
    }

    /// The QuickMenu, while it shows: what a button does is done.
    fn quick_menu(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        match self.quick_menu.show(ctx, &mut self.look, session, &mut self.keep_brushes) {
            Some(crate::quick_menu::Ran::Action(action, at)) => self.run_quick(ctx, session, action, at),
            // a button set to a brush gives it to the current tool
            Some(crate::quick_menu::Ran::Brush(b)) if session.select_brush(b.set, b.index) => {
                self.keep_brushes = true;
                self.library.library_changed();
            }
            _ => {}
        }
    }

    /// A QuickMenu button's action, the menu opened at `at` (points).
    fn run_quick(&mut self, ctx: &egui::Context, session: &CanvasSession, action: QuickAction, at: egui::Pos2) {
        let current = session.layers().into_iter().find(|l| l.current);
        let id = current.as_ref().map(|l| l.id);
        if let Some(a) = sumi_core::quick_action_adjustment(action) {
            self.start_adjustment(session, a);
            return;
        }
        use sumi_core::GuideType;
        let companion = |mode: crate::companion::Mode, now: crate::companion::Mode| {
            if now == mode { crate::companion::Mode::None } else { mode }
        };
        match action {
            QuickAction::Adjustments => self.adjustments_pressed(session),
            QuickAction::Liquify => self.start_adjustment(session, sumi_core::Adjustment::Liquify),
            QuickAction::Clone => self.start_adjustment(session, sumi_core::Adjustment::Clone),
            QuickAction::Guide2d => toggle_guide_kind(session, GuideType::Grid),
            QuickAction::GuideIsometric => toggle_guide_kind(session, GuideType::Isometric),
            QuickAction::GuidePerspective => toggle_guide_kind(session, GuideType::Perspective),
            QuickAction::GuideSymmetry => toggle_guide_kind(session, GuideType::Symmetry),
            QuickAction::DrawingAssist => _ = session.set_guide_assisted(!session.guide().assisted),
            QuickAction::AnimationAssist | QuickAction::PageAssist => {
                self.animation.reset(session);
                let pages = action == QuickAction::PageAssist;
                let on = session.animation_settings().is_some_and(|a| a.pages == pages);
                if pages {
                    session.set_page_assist(!on);
                } else {
                    session.set_animation_assist(!on);
                }
            }
            QuickAction::NextFrame | QuickAction::PreviousFrame => {
                // Animation Assist's next or previous frame drawn on, staying in its frames
                let frames = session.animation_frames();
                if let Some(at) = session.current_frame() {
                    let to = if action == QuickAction::NextFrame {
                        (at as usize + 1).min(frames.len().saturating_sub(1))
                    } else {
                        (at as usize).saturating_sub(1)
                    };
                    if to != at as usize
                        && let Some(&layer) = frames.get(to)
                    {
                        session.select_layer(layer);
                    }
                }
            }
            QuickAction::OnionSkin => {
                // onion skins off, or back to as many as they were
                if let Some(mut s) = session.animation_settings() {
                    if s.onion_frames > 0 {
                        self.onion_kept = s.onion_frames;
                        s.onion_frames = 0;
                    } else {
                        s.onion_frames = self.onion_kept.max(1);
                    }
                    session.set_animation_settings(s);
                }
            }
            QuickAction::BrushScaling => {
                self.prefs.brush_on_screen = !self.prefs.brush_on_screen;
                self.prefs.apply(session);
                self.prefs.keep();
            }
            QuickAction::ProjectCanvas => {
                self.prefs.project_canvas = !self.prefs.project_canvas;
                self.prefs.keep();
            }
            QuickAction::Reference => self.reference.on = !self.reference.on,
            QuickAction::Recolor => self.toggle_recolor(ctx, session),
            QuickAction::LayerOpacity => self.start_layer_opacity(session),
            QuickAction::PreviousBrush => {
                // back to the brush chosen before this one (and Previous brush comes back here)
                if let Some((set, index)) = self.brush_last
                    && session.select_brush(set, index)
                {
                    self.keep_brushes = true;
                    self.library.library_changed();
                }
            }
            QuickAction::CompanionMove => {
                self.feedback();
                self.companion.mode = companion(crate::companion::Mode::Move, self.companion.mode);
            }
            QuickAction::CompanionZoom => {
                self.feedback();
                self.companion.mode = companion(crate::companion::Mode::Zoom, self.companion.mode);
            }
            QuickAction::NewLayer => {
                if session.add_layer() == 0 {
                    crate::files::no_room(session);
                }
            }
            QuickAction::DuplicateLayer => self.duplicate_current(session),
            QuickAction::DeleteLayer => _ = id.map(|id| session.delete_layer(id)),
            QuickAction::MergeDown => _ = id.map(|id| session.merge_down(id)),
            QuickAction::ClearLayer => self.clear_layer(session),
            QuickAction::FillLayer => _ = id.map(|id| session.fill_layer(id)),
            QuickAction::SelectLayer => _ = id.map(|id| session.select_layer_contents(id)),
            QuickAction::AlphaLock => _ = current.map(|l| session.set_layer_alpha_lock(l.id, !l.alpha_lock)),
            QuickAction::LayerVisibility => _ = current.map(|l| session.tap_visibility(l.id)),
            QuickAction::Solo => {
                if let Some(said) = id.and_then(|id| session.hold_visibility(id)) {
                    self.overlays.notice(said, false);
                }
            }
            QuickAction::Copy => self.copy_layer(session),
            QuickAction::CopyCanvas => self.copy_canvas(session),
            QuickAction::Cut => self.cut_layer(session),
            QuickAction::Paste => self.paste(ctx, false),
            QuickAction::CopyPasteMenu => self.copy_paste = true,
            QuickAction::FlipHorizontal => session.flip_canvas(true),
            QuickAction::FlipVertical => session.flip_canvas(false),
            QuickAction::Undo => self.undo(session),
            QuickAction::Redo => self.redo(session),
            QuickAction::Paint | QuickAction::Smudge | QuickAction::Erase => {
                self.feedback();
                self.end_recolor(session);
                session.set_tool(match action {
                    QuickAction::Smudge => ToolKind::Smudge,
                    QuickAction::Erase => ToolKind::Erase,
                    _ => ToolKind::Paint,
                });
                self.library.tool_changed();
            }
            QuickAction::Eyedropper => session.set_pick_next(true),
            QuickAction::SwapColors => {
                if self.colours.previous(session) {
                    self.overlays.notice("Previous colour", false);
                }
            }
            QuickAction::Selection => self.toggle_selection(session),
            QuickAction::Transform => self.toggle_transform(session),
            QuickAction::Colors => self.toggle(session, Popover::Colour),
            QuickAction::Layers => self.layers_pressed(ctx, session),
            QuickAction::Brushes => self.toggle(session, Popover::Brushes),
            QuickAction::Interface => self.toggle_bars(session),
            QuickAction::FitCanvas => session.settle_to_fit(),
            QuickAction::ActualSize => session.settle_to_actual_size(),
            QuickAction::Quickshape => {
                if let Some(info) = session.quickshape_last() {
                    self.snapped(info);
                }
            }
            QuickAction::QuickshapeOn => {
                let on = self.gestures.is_on(GesturePurpose::Quickshape, "DRAW_HOLD".into());
                self.gestures.set(GesturePurpose::Quickshape, "DRAW_HOLD".into(), !on);
                session.set_touch_settings(self.gestures.touch_settings(self.prefs.rapid_undo_ms()));
                crate::files::keep_gestures(&self.gestures.kept());
            }
            QuickAction::AddText => self.add_text(ctx, session),
            QuickAction::LayerSelect => {
                let ppp = ctx.pixels_per_point();
                self.layer_select(ctx, session, at.x * ppp, at.y * ppp);
            }
            _ => {}
        }
    }

    /// Layer Select's list, while it is up.
    fn layer_choice(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        let Some((at, ids)) = &self.layer_choice else { return };
        let Some(chose) = crate::layer_choice::show(ctx, *at, ids, &session.layers()) else { return };
        if let crate::layer_choice::Chose::Layer(id) = chose {
            session.select_layer(id);
        }
        self.layer_choice = None;
    }

    /// Transform on (the current layer, or what is selected of it, in a box) or off (kept).
    /// A picture the user picks put on a new layer of its own, then transformed (a moving GIF's
    /// frames each on a layer of its own, for the timeline), as Insert a file and Insert a photo
    /// do.
    fn insert_picture(&mut self, session: &Arc<CanvasSession>, photo: bool, private: bool) {
        let title = if photo { "Insert a photo" } else { "Insert a file" };
        let Some(path) = rfd::FileDialog::new()
            .set_title(title)
            .add_filter("Pictures and videos", &[&PICTURES[..], &VIDEOS[..]].concat())
            .pick_file()
        else {
            return;
        };
        self.insert_files(session, vec![path], private);
    }

    /// Files dropped on the canvas: a picture inserted as Insert a file does (several each a layer,
    /// in turn); brushes (ours or another app's) added to the library.
    pub fn dropped(&mut self, session: &Arc<CanvasSession>, paths: &[std::path::PathBuf]) {
        let mut brushes = Vec::new();
        let mut pictures = Vec::new();
        for path in paths {
            let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if PICTURES.contains(&extension.as_str()) || VIDEOS.contains(&extension.as_str()) {
                pictures.push(path.clone());
            } else if crate::files::is_brush_file(path)
                && let Ok(bytes) = std::fs::read(path)
            {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                brushes.push((bytes, name));
            }
        }
        self.library.add_brushes(brushes, true);
        if !pictures.is_empty() {
            self.open = None;
            self.insert_files(session, pictures, false);
        }
    }

    /// Brushes read from files meanwhile added to the library and told of (when asked to), the
    /// library kept. True while more are being read or wait to be added.
    fn brushes_added(&mut self, session: &CanvasSession) -> bool {
        for (any, tell) in self.library.added(session, self.studio.open) {
            if tell {
                let said = if any { "Brushes added to the library." } else { "This file has no brushes in it." };
                self.overlays.notice(said, false);
            } else if !any {
                crate::files::say("This file has no brushes in it.");
            }
        }
        self.keep_brushes |= std::mem::take(&mut self.library.changed);
        self.library.importing()
    }

    /// The picture in file `path` as a new layer (a private one, if `private`), ready to
    /// transform; a GIF's frames each a layer.
    fn insert_files(&mut self, session: &Arc<CanvasSession>, paths: Vec<std::path::PathBuf>, private: bool) {
        // read one after another on a thread of their own (a video's frames take seconds), what
        // each came to told on a frame after (`inserted`); a picture alone is transformed
        let transform = paths.len() == 1;
        let (session, (done, inserting)) = (Arc::clone(session), std::sync::mpsc::channel());
        std::thread::spawn(move || {
            for path in paths {
                if done.send((read_insert(&session, &path, private), transform)).is_err() {
                    return;
                }
            }
        });
        self.inserting.push(inserting);
    }

    /// What inserts read meanwhile came to: a picture alone transformed, frames told of (Animation
    /// Assist on for a video's), or why it couldn't be. True while one is still being read.
    fn inserted(&mut self, session: &CanvasSession) -> bool {
        let mut came = Vec::new();
        // each read's results so far, kept while more may come
        self.inserting.retain(|r| {
            loop {
                match r.try_recv() {
                    Ok(i) => came.push(i),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break true,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break false,
                }
            }
        });
        for (inserted, transform) in came {
            match inserted {
                Inserted::Picture(true) => {
                    if transform {
                        self.toggle_transform(session);
                    }
                }
                Inserted::Picture(false) => crate::files::no_room(session),
                Inserted::Frames { added, full, assist } => {
                    if assist {
                        self.animation.reset(session);
                        session.set_animation_assist(true);
                    }
                    if added > 0 {
                        self.overlays.notice("Frames saved to timeline", false);
                    }
                    if full {
                        crate::files::no_room(session);
                    }
                }
                Inserted::Failed(what) => crate::files::tell("Can't insert it", what),
            }
        }
        !self.inserting.is_empty()
    }

    fn toggle_transform(&mut self, session: &CanvasSession) {
        if !self.transform.on && !self.takes_changes(session) {
            return;
        }
        // on, it ends; else the other modes end
        if !self.switch_modes(session, ModeButton::Transform) {
            return;
        }
        self.open = None;
        session.set_pick_next(false);
        // nothing there to move: it says so and stays out of transform
        if let Err(said) = self.transform.begin(session) {
            self.overlays.notice(said, false);
        }
    }

    /// Opens adjustment `a` on the current layer (anything else open ends first).
    fn start_adjustment(&mut self, session: &CanvasSession, a: sumi_core::Adjustment) {
        let why = session.why_not_adjusted(a);
        if !self.say_blocked(session, why) {
            return;
        }
        self.switch_modes(session, ModeButton::Adjustment);
        self.open = None;
        session.set_pick_next(false);
        session.adjustment_start(a);
    }

    /// Opens the plugin's filter `id` on the current layer, as [`Self::start_adjustment`] does. The
    /// artwork is saved first, the first time in a session.
    /// The panel a plugin's action showed, while it is open: a touch on it told to the plugin on a
    /// thread of its own (it may take seconds), Done closing it.
    fn plugin_panel(&mut self, ctx: &egui::Context, session: &Arc<CanvasSession>, backdrops: &mut Vec<f32>) {
        let Some(panel) = self.prefs.plugins.panel() else {
            self.plugin_panel_held = None;
            return;
        };
        let asked = crate::actions::plugin_panel(ctx, &mut self.look, &panel, &mut self.plugin_panel_held, backdrops);
        match asked {
            Some(crate::actions::PanelTouch::Touch(row, value)) => {
                let (plugins, session, ctx) = (Arc::clone(&self.prefs.plugins), Arc::clone(session), ctx.clone());
                std::thread::spawn(move || {
                    plugins.panel_touched(row, value, session);
                    ctx.request_repaint();
                });
            }
            Some(crate::actions::PanelTouch::Close) => self.prefs.plugins.close_panel(),
            None => {}
        }
    }

    /// A plugin's action `key` run on the artwork, on a thread of its own (it may take seconds): the
    /// panel closes so what it does shows; anything it says, or its stopping, is told as a failure
    /// is.
    fn run_plugin_action(&mut self, ctx: &egui::Context, session: &Arc<CanvasSession>, key: String) {
        // (what it may paint on, it checks: not a locked layer)
        self.open = None;
        let (plugins, session, ctx) = (Arc::clone(&self.prefs.plugins), Arc::clone(session), ctx.clone());
        std::thread::spawn(move || {
            plugins.run_action(key, session);
            ctx.request_repaint();
        });
    }

    fn start_plugin_filter(&mut self, session: &CanvasSession, id: String) {
        if !self.takes_changes(session) {
            return;
        }
        self.switch_modes(session, ModeButton::Adjustment);
        self.open = None;
        session.set_pick_next(false);
        let plugins = std::sync::Arc::clone(&self.prefs.plugins);
        if let Some(why) = session.plugin_filter_start(plugins, id, self.artwork_dir.clone()) {
            self.overlays.notice(why, true);
        }
    }

    /// Ends the open adjustment: `commit` keeps it (and says so), otherwise the layer goes back.
    fn end_adjustment(&mut self, session: &CanvasSession, commit: bool) {
        self.adjustments.menu_at = None;
        if session.adjustment_end(commit) && commit {
            self.overlays.notice("Adjustment applied", false);
        }
    }

    /// The adjustments button: closes an open adjustment (keeping it), or opens the menu.
    fn adjustments_pressed(&mut self, session: &CanvasSession) {
        if self.switch_modes(session, ModeButton::Adjustments) {
            self.toggle(session, Popover::Adjustments);
        }
    }

    /// What the tap menu asked of the open adjustment.
    fn adjustment_asked(&mut self, session: &CanvasSession, asked: crate::adjustments::Asked) {
        use crate::adjustments::Asked;
        match asked {
            Asked::Apply => {
                session.adjustment_apply();
                self.overlays.notice("Adjustment applied", false);
            }
            Asked::Undo => session.adjustment_undo(),
            Asked::Preview => session.adjustment_preview(!session.adjustment_shown().preview),
            Asked::Cancel => self.end_adjustment(session, false),
            Asked::Reset => session.adjustment_reset(),
        }
    }

    /// Selection on (touches select, as its toolbar says) or off (a freehand path drawn is closed).
    fn toggle_selection(&mut self, session: &CanvasSession) {
        // on, it ends; else the other modes end
        if !self.switch_modes(session, ModeButton::Selection) {
            return;
        }
        self.open = None;
        session.set_pick_next(false);
        self.selection.open(session);
    }

    /// The Copy & Paste panel, while it is up: what was chosen there is done, and it goes.
    fn copy_paste_panel(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        if !self.copy_paste {
            return;
        }
        let can_paste = self.can_paste();
        let Some(choice) = crate::copy_paste::show(ctx, &mut self.look, can_paste) else { return };
        self.copy_paste = false;
        match choice {
            Choice::Close => {}
            Choice::Cut => self.cut_layer(session),
            Choice::Copy => self.copy_layer(session),
            Choice::CopyAll => self.copy_canvas(session),
            Choice::Duplicate => self.duplicate_current(session),
            // nothing cut (a locked layer, an empty one): what is on the clipboard stays there
            Choice::CutAndPaste => {
                if self.layer_cut(session) {
                    self.paste_own(session, false);
                }
            }
            Choice::Paste => self.paste(ctx, false),
        }
    }

    /// The current layer (or what is selected of it) cut, to paste.
    fn cut_layer(&mut self, session: &CanvasSession) {
        self.layer_cut(session);
    }

    /// As [`Self::cut_layer`]; whether anything was.
    fn layer_cut(&mut self, session: &CanvasSession) -> bool {
        self.open = None;
        if !self.takes_changes(session) {
            return false;
        }
        let Some(cut) = session.cut_layer() else { return false };
        self.clipboard = Some(cut);
        self.system_clipboard.copied_here(self.clipboard.as_ref());
        self.overlays.notice("Layer cut", false);
        true
    }

    /// The current layer (or what is selected of it) copied, to paste.
    fn copy_layer(&mut self, session: &CanvasSession) {
        self.open = None;
        if let Some(copied) = session.copy_layer() {
            self.clipboard = Some(copied);
            self.system_clipboard.copied_here(self.clipboard.as_ref());
            self.overlays.notice("Layer copied", false);
        }
    }

    /// The whole picture (or what is selected of it) copied, to paste.
    fn copy_canvas(&mut self, session: &CanvasSession) {
        self.open = None;
        if let Some(copied) = session.copy_canvas() {
            self.clipboard = Some(copied);
            self.system_clipboard.copied_here(self.clipboard.as_ref());
            self.overlays.notice("Canvas copied", false);
        }
    }

    /// Whether there is anything for Paste to take, as far as is known.
    fn can_paste(&self) -> bool {
        self.clipboard.is_some() || self.system_clipboard.offers
    }

    /// Paste (on a new private layer, one the gallery's thumbnail leaves out, if `private`): what
    /// was copied last, here or in another app. The system's clipboard is read first, so what
    /// comes of it comes on a later frame (`pasted`).
    fn paste(&mut self, ctx: &egui::Context, private: bool) {
        self.open = None;
        self.system_clipboard.paste(private);
        ctx.request_repaint();
    }

    /// What Paste found on the system's clipboard, once it has: another app's picture comes in as
    /// an inserted one does, ready to transform. True while a look at it is under way.
    fn pasted(&mut self, session: &Arc<CanvasSession>) -> bool {
        match self.system_clipboard.take_pasted() {
            Some(Pasted::Own { private }) => self.paste_own(session, private),
            // (a mask that takes what is pasted has no use for a copy of the picture)
            Some(Pasted::Picture { image, private: false }) if session.paste_picture_into_mask(image.clone()) => {}
            Some(Pasted::Picture { image, private }) => {
                if self.model && !session.model_flat() {
                    if !session.model_picture_begin(image, true) {
                        crate::files::no_room(session);
                    }
                } else if session.insert_image(image, private).is_some() {
                    self.toggle_transform(session);
                } else {
                    crate::files::no_room(session);
                }
            }
            Some(Pasted::Files { paths, private }) => self.insert_files(session, paths, private),
            None => {}
        }
        self.system_clipboard.busy()
    }

    /// What was copied or cut here, pasted as a new layer (a private one, if `private`).
    fn paste_own(&mut self, session: &CanvasSession, private: bool) {
        self.open = None;
        let Some(image) = self.clipboard.clone() else { return };
        if self.model && !session.model_flat() {
            if !session.model_picture_begin(image, false) {
                crate::files::no_room(session);
            }
            return;
        }
        let pasted = if private { session.paste_private(image) } else { session.paste(image) };
        if pasted.is_none() {
            crate::files::no_room(session);
        }
    }

    /// What is selected copied and pasted as a new layer; nothing selected, the layer duplicated.
    fn duplicate_current(&mut self, session: &CanvasSession) {
        if session.has_selection() {
            self.copy_layer(session);
            self.paste_own(session, false);
        } else if let Some(current) = session.layers().into_iter().find(|l| l.current)
            && session.duplicate_layer(current.id).is_none()
        {
            crate::files::no_room(session);
        }
    }

    /// Whether the current layer takes a change; if not (locked, hidden or left out of the
    /// picture, or an empty group), an alert says why. What was tried is not done afterwards.
    fn takes_changes(&mut self, session: &CanvasSession) -> bool {
        let why = session.why_blocked();
        self.say_blocked(session, why)
    }

    /// As [`Self::takes_changes`], for why the current layer took nothing (`why`, if anything).
    fn say_blocked(&mut self, session: &CanvasSession, why: Option<Blocked>) -> bool {
        let Some(why) = why else { return true };
        let Some(l) = session.layers().into_iter().find(|l| l.current) else { return true };
        let (title, message, icon, extra) = match why {
            Blocked::Locked => (
                "Layer is locked",
                format!("\u{201c}{}\u{201d} can't be changed while it is locked.", l.name),
                &crate::icons::PAGE_LOCKED,
                Some("Unlock"),
            ),
            Blocked::Hidden | Blocked::GroupHidden => (
                if why == Blocked::Hidden { "Layer is hidden" } else { "Group is hidden" },
                hidden_message(&l.name, session.hidden_by()),
                &crate::icons::EYE_STRUCK,
                Some("Show"),
            ),
            Blocked::Group => (
                "Group is selected",
                format!("\u{201c}{}\u{201d} is a group: choose one of its layers to paint on.", l.name),
                &crate::icons::FOLDER,
                None,
            ),
            Blocked::EmptyGroup => (
                "Group is empty",
                format!("\u{201c}{}\u{201d} holds no layers to change.", l.name),
                &crate::icons::FOLDER,
                None,
            ),
            Blocked::MeshHidden => (
                "Mesh is hidden",
                format!(
                    "\u{201c}{}\u{201d} can't be painted while it is hidden.",
                    session.meant_mesh_name().unwrap_or_default()
                ),
                &crate::icons::EYE_STRUCK,
                Some("Show"),
            ),
            Blocked::AlphaLocked => (
                "Alpha Lock is on",
                format!("\u{201c}{}\u{201d} can't be liquified while its alpha is locked.", l.name),
                &crate::icons::PAGE_LOCKED,
                Some("Turn Off"),
            ),
            Blocked::MeshLocked => (
                "Mesh is locked",
                format!(
                    "\u{201c}{}\u{201d} can't be painted while it is locked.",
                    session.meant_mesh_name().unwrap_or_default()
                ),
                &crate::icons::PAGE_LOCKED,
                Some("Unlock"),
            ),
        };
        self.blocked = Some((why, l.id));
        self.blocked_alert.ask(Asking {
            title: title.into(),
            message,
            action: "Open Layers".into(),
            icon: Some(icon),
            destructive: false,
            extra: extra.map(Into::into),
            first: None,
            alone: false,
        });
        false
    }

    /// The PDF quality alert, while it shows: the PDF made at the quality picked.
    fn pdf_quality_alert(&mut self, ctx: &egui::Context) {
        let Some(answer) = self.pdf_alert.show(ctx, &mut self.look) else { return };
        let Some(format) = self.pdf_for.take() else { return };
        if let Some(quality) = crate::alert::pdf_jpeg(answer) {
            self.actions.push(Action::Export(format, quality));
            self.open = None;
        }
    }

    /// Asks whether to import the plugin file at `path`: nothing in it runs, and nothing is kept
    /// until the answer comes. What can't be imported says why.
    pub fn offer_plugin_file(&mut self, path: &std::path::Path) {
        let cant = "Can't import this plugin";
        let Some(bytes) = sumi_core::read_plugin_file(path) else {
            self.plugin_alert.tell(cant, "This file can't be read.", None);
            return;
        };
        let offer = self.prefs.plugins.inspect(bytes.clone());
        if let Some(why) = offer.refused {
            self.plugin_alert.tell(cant, &why, None);
            return;
        }
        self.plugin_file = Some(bytes);
        self.plugin_alert.ask(Asking {
            title: offer.name,
            message: offer.message,
            action: "Import".to_owned(),
            icon: None,
            destructive: false,
            extra: None,
            first: None,
            alone: false,
        });
    }

    /// A plugin file's screen, while it shows: answered, the file is imported, or it was only
    /// being told something.
    pub fn plugin_alert(&mut self, ctx: &egui::Context) {
        if let Some(said) = self.prefs.plugins.take_moved_notice() {
            self.plugin_alert.tell("Now a plugin", &said, None);
        }
        let Some(answer) = self.plugin_alert.show(ctx, &mut self.look) else { return };
        let Some(bytes) = self.plugin_file.take().filter(|_| answer == Answer::Action) else { return };
        match self.prefs.plugins.install(bytes) {
            Some(why) => self.plugin_alert.tell("Can't import this plugin", &why, None),
            None => self.prefs.keep(),
        }
    }

    /// Shows queued non-fatal import/export change notes in an in-window alert.
    pub fn note_alert(&mut self, ctx: &egui::Context) {
        if !self.note_alert.is_open()
            && !self.blocked_alert.is_open()
            && !self.model_warning_alert.is_open()
            && !self.pdf_alert.is_open()
            && !self.plugin_alert.is_open()
            && let Some((title, text)) = crate::files::take_next_note()
        {
            self.note_alert.tell(&title, &text, None);
        }
        _ = self.note_alert.show(ctx, &mut self.look);
    }

    /// Says what a model newly opened may not paint well with.
    fn model_warnings(&mut self, session: &CanvasSession) {
        let warnings = session.take_model_warnings();
        if warnings.is_empty() {
            return;
        }
        let names: Vec<String> = warnings.into_iter().map(sumi_core::model_warning_name).collect();
        self.model_warning_alert.ask(Asking {
            title: "Unsupported model features".into(),
            message: format!("Painting on this model may not go as expected because of:\n{}", names.join("\n")),
            action: "OK".into(),
            icon: None,
            destructive: false,
            extra: None,
            first: None,
            alone: true,
        });
    }

    /// The blocked alert, while it shows: unlock or show the layer, or open Layers.
    fn blocked_alert(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        let Some(answer) = self.blocked_alert.show(ctx, &mut self.look) else { return };
        let Some((why, id)) = self.blocked.take() else { return };
        match answer {
            Answer::Extra if why == Blocked::Locked => _ = session.set_layer_locked(id, false),
            Answer::Extra if why == Blocked::AlphaLocked => _ = session.set_layer_alpha_lock(id, false),
            // its own box ticked (what else hides it stays)
            Answer::Extra if matches!(why, Blocked::Hidden | Blocked::GroupHidden) => {
                if let Some(said) = session.show_layer(id) {
                    self.overlays.notice(said, false);
                }
            }
            Answer::Extra if matches!(why, Blocked::MeshHidden | Blocked::MeshLocked) => session.free_meant_meshes(),
            Answer::Action => self.open_layers(ctx, session),
            _ => {}
        }
    }

    /// The interface hides (full screen) or comes back; hiding, it closes its panels.
    /// A tool button held (as long as a long press): that tool takes the brush the current one has
    /// (erasing with the painting brush, say) and becomes the tool. True while the press holding
    /// it has acted, so that its letting go doesn't tap it too.
    fn tool_hold(&mut self, ctx: &egui::Context, session: &CanvasSession, bar: &crate::chrome::Bar) -> bool {
        let tool = |b: BarButton| match b {
            BarButton::Paint => Some(ToolKind::Paint),
            BarButton::Smudge => Some(ToolKind::Smudge),
            BarButton::Erase => Some(ToolKind::Erase),
            _ => None,
        };
        if let Some((target, since)) = bar.down.and_then(|(b, since)| tool(b).map(|t| (t, since)))
            && !self.tool_hold_acted
        {
            let left = TOOL_HOLD_S - (ctx.input(|i| i.time) - since);
            if left <= 0.0 {
                self.tool_hold_acted = true;
                self.tool_held(session, target);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
            }
        }
        let acted = self.tool_hold_acted;
        if bar.down.is_none() {
            self.tool_hold_acted = false;
        }
        acted
    }

    /// The tool `target` takes the current brush, as its button held does.
    fn tool_held(&mut self, session: &CanvasSession, target: ToolKind) {
        let brush = session.current_brush();
        if session.info().tool == target || !self.switch_modes(session, ModeButton::ToolHeld) {
            return;
        }
        self.choose_tool(session, target);
        if session.select_brush(brush.set, brush.index) {
            self.keep_brushes = true;
        }
        if self.open == Some(Popover::Brushes) {
            self.open = None;
        }
        let doing = match target {
            ToolKind::Paint => "Painting",
            ToolKind::Smudge => "Smudging",
            ToolKind::Erase => "Erasing",
        };
        self.overlays.notice(format!("{doing} with the current brush"), false);
    }

    /// With the interface hidden, a button in the top left (44 square, 20 in) that brings it
    /// back: the full screen glyph, drawn wider than it is tall (36.7 by 32). True when tapped.
    fn show_interface_button(&mut self, ctx: &egui::Context) -> bool {
        let screen = ctx.content_rect();
        let button = Rect::from_min_size(screen.left_top() + vec2(20.0, 0.0), vec2(44.0, 44.0));
        let mut tapped = false;
        egui::Area::new(egui::Id::unique("show the interface"))
            .fixed_pos(button.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                tapped = ui.allocate_rect(button, egui::Sense::click()).clicked();
                let glyph = Rect::from_center_size(button.center() + vec2(0.0, 0.3), vec2(36.7, 32.0));
                self.look.icon_in(ui, &crate::icons::FULL_SCREEN, glyph, argb(pc::SIDE_GLYPH));
            });
        tapped
    }

    /// The interface hidden or shown again (not while transforming).
    fn toggle_bars(&mut self, session: &CanvasSession) {
        if !self.switch_modes(session, ModeButton::Bars) {
            return;
        }
        self.bars_hidden = !self.bars_hidden;
        if self.bars_hidden {
            self.open = None;
            self.layers.close_menus();
            self.end_background_pick(session);
        }
    }

    /// The Layers dock's state (the preference, the full screen, and the modes that take the
    /// canvas) and whether it fits this window's width.
    fn dock_wanted(&self, session: &CanvasSession) -> bool {
        let adjusting = session.adjustment_shown();
        self.prefs.layers_dock
            && !self.bars_hidden
            && self.canvas_shown()
            && !self.transform.on
            && !(adjusting.adjustment.is_some() && !adjusting.pencil)
            && session.touch_layer_opacity().is_none()
            && !self.text.editing()
    }

    /// Where the persistent Layers dock shows this frame, if it does.
    fn layers_dock_rect(&self, ctx: &egui::Context, session: &CanvasSession) -> Option<Rect> {
        crate::panel::layers_dock(ctx.content_rect(), self.dock_wanted(session), self.prefs.right_handed)
    }

    /// Whether the window is wide enough for the dock; narrower windows keep the popover and
    /// leave the preference alone.
    fn layers_dock_mode(ctx: &egui::Context) -> bool {
        ctx.content_rect().width() >= crate::panel::LAYERS_DOCK_MIN_WIDTH
    }

    /// The Layers button, L and the QuickMenu's Layers: on a wide window they toggle the dock's
    /// preference (keeping what is being done on the canvas); on a narrow one, the old popover.
    fn layers_pressed(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        // the background's picker is transient over the dock: this closes it first
        if self.background_pick.is_some() {
            self.end_background_pick(session);
            self.open = None;
            return;
        }
        self.feedback();
        if Self::layers_dock_mode(ctx) {
            self.prefs.layers_dock = !self.prefs.layers_dock;
            self.prefs.keep();
            if self.open == Some(Popover::Layers) {
                self.open = None;
            }
            if !self.prefs.layers_dock {
                self.layers.close_menus();
            }
        } else {
            self.toggle(session, Popover::Layers);
        }
    }

    /// Open Layers (a blocked layer's alert): the dock is shown, not toggled away, and the
    /// preference stays on. Narrow windows open the popover.
    fn open_layers(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        self.feedback();
        if self.background_pick.is_some() {
            self.end_background_pick(session);
        }
        if Self::layers_dock_mode(ctx) {
            self.open = None;
            if !self.prefs.layers_dock {
                self.prefs.layers_dock = true;
                self.prefs.keep();
            }
        } else {
            self.open = Some(Popover::Layers);
        }
    }

    /// A panel's button: the panel opens (or closes), what works on the canvas ending first.
    fn toggle(&mut self, session: &CanvasSession, popover: Popover) {
        self.feedback();
        if popover != Popover::Layers {
            if self.background_pick.is_some() {
                self.end_background_pick(session);
            }
            // another panel takes over: nothing of the dock's stays open behind it
            self.layers.close_menus();
        }
        self.switch_modes(session, ModeButton::Panel { colour: popover == Popover::Colour });
        // the colour button with the panel floating: it goes back in its place
        if popover == Popover::Colour && self.colours.floating.take().is_some() {
            self.open = None;
        }
        self.open = if self.open == Some(popover) { None } else { Some(popover) };
    }

    /// The modes open now, as the core's [`mode_switch`] takes them.
    fn open_modes(&self, session: &CanvasSession) -> Modes {
        Modes {
            quickshape: self.overlays.shape.is_some(),
            filling: self.drop.filling(),
            recolor: session.recolor_on(),
            layer_opacity: session.touch_layer_opacity().is_some(),
            transform: self.transform.on,
            selection: self.selection.on,
            adjustment: session.adjustment_shown().adjustment.is_some(),
        }
    }

    /// `button` pressed: the modes it leaves behind end first (keeping what they made), as the
    /// core says; true if it goes on to do what it does.
    fn switch_modes(&mut self, session: &CanvasSession, button: ModeButton) -> bool {
        let ModeSwitch { end, goes_on } = mode_switch(button, self.open_modes(session));
        if end.quickshape {
            session.quickshape_done();
            self.overlays.shape = None;
            self.overlays.shape_editing = false;
        }
        if end.filling {
            self.drop.end_filling();
        }
        if end.recolor {
            self.end_recolor(session);
        }
        if end.layer_opacity {
            session.set_touch_layer_opacity(None);
        }
        if end.transform {
            self.transform.end(session, true);
        }
        if end.selection {
            self.selection.close(session);
        }
        if end.adjustment {
            self.end_adjustment(session, true);
        }
        goes_on
    }

    /// What the Layers panel or dock asked for, once it is drawn: a plugin's action, a notice,
    /// copied contents, a text edit, layer opacity, or the background's colour.
    fn layers_events(&mut self, ctx: &egui::Context, session: &Arc<CanvasSession>) {
        if let Some(key) = self.layers.run_action.take() {
            self.run_plugin_action(ctx, session, key);
        }
        if let Some((text, warning)) = self.layers.notice.take() {
            self.overlays.notice(text, warning);
        }
        if let Some(copied) = self.layers.copied.take() {
            self.clipboard = Some(copied);
            self.system_clipboard.copied_here(self.clipboard.as_ref());
        }
        if std::mem::take(&mut self.layers.share) {
            self.actions.push(Action::Export(Format::ChosenLayerPngs, 100));
        }
        if let Some(id) = self.layers.edit_text.take() {
            self.open = None;
            self.text.edit(session, id);
        }
        if std::mem::take(&mut self.layers.layer_opacity) {
            self.start_layer_opacity(session);
        }
        // its background row tapped: the colour panel in its place, on the background's
        if std::mem::take(&mut self.layers.pick_background) {
            let b = session.background();
            self.background_pick = Some(BackgroundPick::new(to_hex([b.r, b.g, b.b]), self.colours.hsv[0]));
        }
    }

    /// The colour panel picking the background's colour, hanging from the Layers button: what is
    /// picked goes to the background as it changes, the brush keeping its colour; its tick closes
    /// it. Returns where it is.
    fn background_panel(
        &mut self,
        ctx: &egui::Context,
        session: &Arc<CanvasSession>,
        button: Rect,
        backdrops: &mut Vec<f32>,
    ) -> (Rect, bool) {
        let Some(pick) = self.background_pick.as_mut() else { return (Rect::NOTHING, false) };
        self.colours.set_triangle(self.prefs.plugins.colour_triangle());
        let picker = self.prefs.plugins.picker_on().then(|| (Arc::clone(&self.prefs.plugins), Arc::clone(session)));
        self.colours.set_picker(picker);
        let brush = std::mem::replace(&mut self.colours.hsv, pick.hsv);
        self.colours.background = true;
        let history = self.prefs.colour_history_shown;
        let shown = self.colours.show(
            ctx,
            &mut self.look,
            session,
            &mut self.drop,
            button,
            &mut self.prefs.colour_history_shown,
            backdrops,
        );
        if self.prefs.colour_history_shown != history {
            self.prefs.keep();
        }
        self.colours.background = false;
        let picked = std::mem::replace(&mut self.colours.hsv, brush);
        if let Some(rgb) = pick.follow(picked) {
            let [r, g, b] = of_hex(rgb);
            session.set_background_color(r, g, b, false);
        }
        let done = std::mem::take(&mut self.colours.background_done);
        (shown, done)
    }

    /// The background's colour picked is kept, as one undo step (if it changed), and the panel
    /// goes back to the brush's.
    fn end_background_pick(&mut self, session: &CanvasSession) {
        if let Some(rgb) = self.background_pick.take().and_then(BackgroundPick::kept) {
            let [r, g, b] = of_hex(rgb);
            session.set_background_color(r, g, b, true);
        }
    }

    /// The open panel, hanging from its button; a press anywhere else, or Escape, closes it.
    fn popover(
        &mut self,
        ctx: &egui::Context,
        session: &Arc<CanvasSession>,
        which: Popover,
        button: Option<Rect>,
        backdrops: &mut Vec<f32>,
    ) {
        let Some(button) = button else {
            self.open = None;
            return;
        };
        // while one of the panel's menus is open, a press elsewhere only closes the menu
        let modal = self.modal(which);
        let shown = match which {
            Popover::Brushes => {
                let shown = self.library.show(ctx, &mut self.look, session, button, backdrops);
                // what was done in the library is kept
                self.keep_brushes |= std::mem::take(&mut self.library.changed);
                shown
            }
            Popover::Layers if self.background_pick.is_some() => {
                let (shown, done) = self.background_panel(ctx, session, button, backdrops);
                if done {
                    self.open = None;
                }
                shown
            }
            Popover::Layers => {
                self.layers.plugin_actions = self.prefs.plugins.actions_in(sumi_core::PluginPlace::Layer);
                self.layers.layer_kinds = self.prefs.plugins.layer_kinds();
                let shown = self.layers.show(ctx, &mut self.look, session, &mut self.colours, button, backdrops);
                self.layers_events(ctx, session);
                shown
            }
            Popover::Colour => {
                self.colours.set_triangle(self.prefs.plugins.colour_triangle());
                let picker =
                    self.prefs.plugins.picker_on().then(|| (Arc::clone(&self.prefs.plugins), Arc::clone(session)));
                self.colours.set_picker(picker);
                let shown = self.prefs.colour_history_shown;
                let history = &mut self.prefs.colour_history_shown;
                let rect = self.colours.show(ctx, &mut self.look, session, &mut self.drop, button, history, backdrops);
                if self.prefs.colour_history_shown != shown {
                    self.prefs.keep();
                }
                // the header's transparent swatch asks for the Erase tool: it goes by the same
                // path as the toolbar's button and `E`, the panel closing as it does for a tool
                // chosen there
                if self.colours.take_erase_request() {
                    self.tool_key(session, ToolKind::Erase);
                    self.open = None;
                }
                // dragged out, it floats instead
                if self.colours.floating.is_some() {
                    self.open = None;
                }
                rect
            }
            Popover::Adjustments => {
                use crate::adjustments::MenuRow;
                let filters = self.prefs.plugins.filters();
                let actions = self.prefs.plugins.actions_in(sumi_core::PluginPlace::Adjustments);
                let turned = self.adjustments.showing_filters && (!filters.is_empty() || !actions.is_empty());
                let (shown, tapped) =
                    crate::adjustments::menu(ctx, &mut self.look, button, backdrops, &filters, &actions, turned);
                match tapped {
                    Some(MenuRow::Open(a)) => self.start_adjustment(session, a),
                    Some(MenuRow::Plugins) => self.adjustments.showing_filters = true,
                    Some(MenuRow::Back) => self.adjustments.showing_filters = false,
                    Some(MenuRow::Filter(id)) => {
                        self.adjustments.showing_filters = false;
                        self.start_plugin_filter(session, id);
                    }
                    Some(MenuRow::Action(key)) => {
                        self.adjustments.showing_filters = false;
                        self.run_plugin_action(ctx, session, key);
                    }
                    None => {}
                }
                shown
            }
            Popover::Actions => {
                let assist = session.animation_settings();
                let showing = crate::actions::Showing {
                    can_paste: self.can_paste(),
                    animation: assist.as_ref().is_some_and(|a| !a.pages),
                    pages: assist.as_ref().is_some_and(|a| a.pages),
                    time_lapse: self.time_lapse.recording(),
                    time_lapse_recorded: self.time_lapse.recorded(),
                    reference: self.reference.on,
                    guide: session.guide().on,
                    side_bar: self.side_bar_shown,
                    model: self.model,
                    model_flat: self.model && session.model_flat(),
                    paint_through: self.model && session.paint_through(),
                    svg: session.exports_svg(),
                    plugin_exports: session.plugin_exports().into_iter().map(|e| e.label).collect(),
                    // what the two native documents cannot hold, read under their rows: the
                    // writer's own check, without pixels or files
                    clip: crate::files::ClipChanges::of(session.clip_preflight(false)),
                    kra: crate::files::ClipChanges::of(session.clip_preflight(true)),
                };
                let (shown, act) =
                    self.actions_panel.show(ctx, &mut self.look, button, &showing, &mut self.prefs, backdrops);
                match act {
                    Some(Act::ImportPlugin) => {
                        if let Some(path) = crate::files::pick_plugin() {
                            self.offer_plugin_file(&path);
                        }
                    }
                    Some(Act::PluginAction) => {
                        if let Some(key) = self.actions_panel.take_plugin_action() {
                            self.run_plugin_action(ctx, session, key);
                        }
                    }
                    Some(Act::Prefs) => {
                        self.prefs.apply(session);
                        session.set_touch_settings(self.gestures.touch_settings(self.prefs.rapid_undo_ms()));
                        self.prefs.keep();
                    }
                    Some(Act::SideBar) => self.side_bar_shown = !self.side_bar_shown,
                    Some(Act::TimeLapseReplay) => {
                        self.open = None;
                        self.time_lapse.replay();
                    }
                    Some(Act::TimeLapseRecording) => self.time_lapse.set_recording(!self.time_lapse.recording()),
                    Some(Act::ExportTimeLapse) => {
                        self.open = None;
                        self.time_lapse.ask_length();
                    }
                    Some(Act::AnimatedExport(format)) => {
                        self.open = None;
                        self.animated_export = Some(crate::animated_export::AnimatedExport::new(format));
                    }
                    Some(act @ (Act::AnimationAssist | Act::PageAssist)) => {
                        self.animation.reset(session);
                        let pages = act == Act::PageAssist;
                        let on = session.animation_settings().is_some_and(|a| a.pages == pages);
                        if pages {
                            session.set_page_assist(!on);
                        } else {
                            session.set_animation_assist(!on);
                        }
                    }
                    Some(Act::Licences) => self.licences = true,
                    Some(Act::Shortcuts) => {
                        self.open = None;
                        self.shortcut_list.open = true;
                    }
                    Some(Act::ModelFlat) => session.set_model_flat(!session.model_flat()),
                    Some(Act::ModelAnimated(format)) => {
                        self.open = None;
                        self.animated_export = Some(crate::animated_export::AnimatedExport::model(format));
                    }
                    Some(Act::PaintThrough) => session.set_paint_through(!session.paint_through()),
                    Some(Act::LightingStudio) => {
                        self.open = None;
                        self.lighting.begin(session);
                    }
                    Some(Act::AdvancedSettings) => {
                        self.open = None;
                        self.settings = true;
                    }
                    Some(Act::Flip { horizontal }) => session.flip_canvas(horizontal),
                    Some(Act::AddText) => self.add_text(ctx, session),
                    Some(
                        act @ (Act::InsertFile | Act::InsertPhoto | Act::InsertPrivateFile | Act::InsertPrivatePhoto),
                    ) => {
                        self.open = None;
                        let photo = matches!(act, Act::InsertPhoto | Act::InsertPrivatePhoto);
                        let private = matches!(act, Act::InsertPrivateFile | Act::InsertPrivatePhoto);
                        self.insert_picture(session, photo, private);
                    }
                    Some(Act::Share(format @ (Format::Pdf | Format::LayersPdf))) => {
                        // how well it keeps its pictures, asked first
                        self.pdf_for = Some(format);
                        self.pdf_alert.ask(crate::alert::pdf_quality());
                    }
                    Some(Act::Share(format)) => {
                        self.actions.push(Action::Export(format, 100));
                        self.open = None;
                    }
                    Some(Act::PluginExport(i)) => {
                        self.actions.push(Action::PluginExport(i));
                        self.open = None;
                    }
                    Some(Act::About) => crate::files::about(),
                    Some(Act::GestureControls) => {
                        self.open = None;
                        self.gesture_controls.open = true;
                    }
                    Some(Act::Reference) => self.reference.on = !self.reference.on,
                    Some(Act::DrawingGuide) => _ = session.set_guide_on(!session.guide().on),
                    Some(Act::EditGuide) => {
                        self.open = None;
                        self.guide_editor.begin(ctx, session);
                    }
                    Some(Act::Crop) => {
                        self.open = None;
                        self.crop.begin(session);
                    }
                    Some(Act::CanvasInfo) => {
                        self.open = None;
                        self.canvas_info = Some(crate::canvas_info::CanvasInfo::new(
                            session,
                            self.artwork_dir.clone(),
                            self.time_lapse.info(),
                            self.time_lapse.settings(),
                        ));
                    }
                    Some(Act::Cut) => self.cut_layer(session),
                    Some(Act::Copy) => self.copy_layer(session),
                    Some(Act::CopyCanvas) => self.copy_canvas(session),
                    Some(Act::Paste) => self.paste(ctx, false),
                    Some(Act::PastePrivate) => self.paste(ctx, true),
                    // the panel does these itself
                    Some(
                        Act::Page(_)
                        | Act::ScalingMenu
                        | Act::TipAttachment
                        | Act::LightInterface
                        | Act::RightHanded
                        | Act::BrushCursor
                        | Act::ProjectCanvas
                        | Act::ResetInput,
                    )
                    | None => {}
                }
                shown
            }
        };
        if !modal {
            self.close_outside(ctx, shown, button);
        }
    }

    /// One of the menus of popover `which` is open.
    fn modal(&self, which: Popover) -> bool {
        match which {
            Popover::Layers if self.background_pick.is_some() => self.colours.modal(),
            Popover::Layers => self.layers.modal(),
            Popover::Brushes => self.library.modal(),
            Popover::Colour => self.colours.modal(),
            Popover::Actions => self.licences || self.actions_panel.modal(),
            Popover::Adjustments => false,
        }
    }

    /// Whether the background picker closes: its tick (`done`), or, unless one of its menus/hex
    /// pad is up, a press outside it or Escape. Escape is only read when no menu took it.
    fn picker_dismissed(done: bool, modal: bool, outside: bool, escaped: bool) -> bool {
        done || (!modal && (outside || escaped))
    }

    /// A press anywhere but on the popover at `shown` and its `button`, or Escape, closes it (one
    /// the canvas took closes it as the touches say).
    fn close_outside(&mut self, ctx: &egui::Context, shown: Rect, button: Rect) {
        let pressed_outside = !self.canvas_pressed
            && ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.interact_pos().is_some_and(|p| !shown.contains(p) && !button.contains(p))
            });
        if escape(ctx) || pressed_outside {
            self.open = None;
        }
    }

    /// A hex keypad, or text being edited, takes the keys (a text field's are egui's own).
    fn keys_taken(&self) -> bool {
        self.text.editing()
            || ((self.open == Some(Popover::Colour)
                || self.colours.floating.is_some()
                || self.background_pick.is_some())
                && self.colours.takes_keys())
            || self.animation.takes_keys()
    }

    /// Nothing covers the canvas: no full-screen page or editor is open.
    fn canvas_shown(&self) -> bool {
        !(self.studio.open
            || self.time_lapse.replaying()
            || self.animated_export.is_some()
            || self.lighting.open
            || self.crop.open
            || self.guide_editor.open
            || self.gesture_controls.open
            || self.canvas_info.is_some()
            || self.settings)
    }

    /// A hardware keyboard's shortcuts: the core's (keys.rs), and the desktop's own: Ctrl (⌘) with
    /// N, O, S (and Shift) for its files, with +, - and 0 for its view. A plugin rebinding keys takes
    /// each press as it says first; a plugin's panel asking for a key press takes the next.
    fn shortcuts(&mut self, ctx: &egui::Context, session: &Arc<CanvasSession>, info: &CanvasInfo) {
        // a full-screen page or editor covers the canvas: none of these keys reaches it (its own
        // page sees Escape and its own keys; shortcuts skip Escape and Space in any case)
        if !self.canvas_shown() {
            return;
        }
        // a text field, or the hex keypad, takes the keys; text being edited takes them all
        if ctx.egui_wants_keyboard_input() || self.keys_taken() {
            return;
        }
        let keys: Vec<(Key, Modifiers, bool)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, repeat, .. } => Some((*key, *modifiers, *repeat)),
                    _ => None,
                })
                .collect()
        });
        let mut asking = self.prefs.plugins.panel().is_some_and(|panel| panel.key_press);
        for (key, modifiers, repeat) in keys {
            let Some(name) = key_name(key) else { continue };
            let press =
                KeyCombo { key: name.into(), command: modifiers.command, shift: modifiers.shift, alt: modifiers.alt };
            if asking {
                if !repeat {
                    // the one press it asked for, told on a thread of its own (it may take a while)
                    asking = false;
                    match key {
                        Key::Escape => _ = escape(ctx),
                        Key::Space => self.space_to_panel = true,
                        _ => {}
                    }
                    let (plugins, session, ctx) = (Arc::clone(&self.prefs.plugins), Arc::clone(session), ctx.clone());
                    std::thread::spawn(move || {
                        plugins.panel_key(press, session);
                        ctx.request_repaint();
                    });
                }
                ctx.input_mut(|i| i.consume_key(modifiers, key));
                continue;
            }
            // R tapped turns the canvas a step, held it turns it by dragging, and Space tapped opens
            // the QuickMenu, held it moves the canvas: the window's input sees to them (input.rs),
            // as to Escape and F11 elsewhere
            if key == Key::R && !modifiers.command && !modifiers.alt {
                ctx.input_mut(|i| i.consume_key(modifiers, key));
                continue;
            }
            if matches!(key, Key::Space | Key::Escape | Key::F11) {
                continue;
            }
            let did = match self.prefs.plugins.key_press(press) {
                Some(press) => self.press(ctx, session, info, &press),
                // a key a plugin took away does nothing
                None => true,
            };
            if did {
                ctx.input_mut(|i| i.consume_key(modifiers, key));
            }
        }
    }

    /// Does what `press` does (as a plugin rebinding keys took it): the desktop's own, else the
    /// core's shortcut on it. True if it did something.
    fn press(&mut self, ctx: &egui::Context, session: &CanvasSession, info: &CanvasInfo, press: &KeyCombo) -> bool {
        if press.command {
            // Ctrl (⌘) with + or = zooms in about the window's middle, with - out, with 0 fits the
            // canvas (the desktop's own, as the R key's turn)
            let middle = ctx.content_rect().center() * ctx.pixels_per_point();
            match press.key.as_str() {
                "s" => self.actions.push(if press.shift { Action::SaveAs } else { Action::Save }),
                // Ctrl+Shift+N is the core's (a new layer), not another new artwork
                "n" if !press.shift => self.actions.push(Action::New),
                "o" => self.actions.push(Action::Open),
                "=" | "plus" => session.view_gesture(middle.x, middle.y, 0.0, 0.0, KEY_ZOOM, 0.0),
                "-" => session.view_gesture(middle.x, middle.y, 0.0, 0.0, 1.0 / KEY_ZOOM, 0.0),
                "0" => session.settle_to_fit(),
                _ => {
                    return self.canvas_press(ctx, session, info, press);
                }
            }
            return true;
        }
        self.canvas_press(ctx, session, info, press)
    }

    /// Does the core's shortcut on `press`, if it has one; true if it did.
    fn canvas_press(
        &mut self,
        ctx: &egui::Context,
        session: &CanvasSession,
        info: &CanvasInfo,
        press: &KeyCombo,
    ) -> bool {
        let shortcut = sumi_core::key_shortcut(
            press.key.clone(),
            press.command,
            press.shift,
            press.alt,
            self.transform.on || self.selection.nudges(),
        );
        let Some(shortcut) = shortcut else { return false };
        self.key_shortcut(ctx, session, info, shortcut);
        true
    }

    /// Does what a keyboard shortcut says.
    fn key_shortcut(&mut self, ctx: &egui::Context, session: &CanvasSession, info: &CanvasInfo, shortcut: KeyShortcut) {
        let middle = ctx.content_rect().center();
        match shortcut {
            KeyShortcut::Undo => self.undo(session),
            KeyShortcut::Redo => self.redo(session),
            KeyShortcut::CopyLayer => self.copy_layer(session),
            KeyShortcut::CutLayer => self.cut_layer(session),
            KeyShortcut::Paste => self.paste(ctx, false),
            KeyShortcut::Paint => self.tool_key(session, ToolKind::Paint),
            KeyShortcut::Erase => self.tool_key(session, ToolKind::Erase),
            KeyShortcut::Eyedropper => session.set_pick_next(!session.pick_next()),
            KeyShortcut::Selection => self.toggle_selection(session),
            KeyShortcut::Transform => self.toggle_transform(session),
            KeyShortcut::HueSaturationBrightness => self.start_adjustment(session, sumi_core::Adjustment::Hsb),
            KeyShortcut::Colours => self.toggle(session, Popover::Colour),
            KeyShortcut::Actions => self.toggle(session, Popover::Actions),
            KeyShortcut::Layers => self.layers_pressed(ctx, session),
            KeyShortcut::SwapColours => self.colours.swap(session),
            KeyShortcut::Deselect => self.selection.clear(session),
            KeyShortcut::DuplicateLayer => self.duplicate_current(session),
            KeyShortcut::NewLayer => self.run_quick(ctx, session, QuickAction::NewLayer, middle),
            KeyShortcut::FlipHorizontal => self.run_quick(ctx, session, QuickAction::FlipHorizontal, middle),
            KeyShortcut::Interface => self.toggle_bars(session),
            KeyShortcut::ClearLayer => self.clear_layer(session),
            KeyShortcut::CopyCanvas => self.run_quick(ctx, session, QuickAction::CopyCanvas, middle),
            KeyShortcut::ColourBalance => self.run_quick(ctx, session, QuickAction::ColorBalance, middle),
            KeyShortcut::PerspectiveGuide => self.run_quick(ctx, session, QuickAction::GuidePerspective, middle),
            KeyShortcut::QuickMenu => self.quick_menu.toggle(middle),
            KeyShortcut::GreyView => session.set_grey_view(!session.grey_view()),
            KeyShortcut::BrushSize { by } => session.set_brush_size((info.size + by).clamp(0.0, 1.0)),
            KeyShortcut::Nudge { dx, dy } if self.transform.on => self.transform.moved(session.transform_arrow(dx, dy)),
            // the strokes the selection takes, and the selection with them
            KeyShortcut::Nudge { dx, dy } => {
                session.nudge_plugin_items(dx, dy);
            }
        }
    }

    /// A tool's key: whatever mode is open ends, keeping what it made, as for the tool's button.
    fn tool_key(&mut self, session: &CanvasSession, tool: ToolKind) {
        self.switch_modes(session, ModeButton::Tool);
        self.choose_tool(session, tool);
    }

    /// A tool chosen (as its button does when it isn't the current one).
    fn choose_tool(&mut self, session: &CanvasSession, tool: ToolKind) {
        self.feedback();
        self.end_recolor(session);
        session.set_tool(tool);
        self.library.tool_changed();
    }
}

/// How far Ctrl with + or - zooms the canvas each time.
const KEY_ZOOM: f32 = 1.25;

/// Whether Escape was pressed this frame and nothing open has taken it yet: the first thing to ask
/// closes with it and marks it taken, so one press closes one thing, and the canvas does not also
/// turn upright ([`Interface::show`]).
pub fn escape(ctx: &egui::Context) -> bool {
    if !ctx.input(|i| i.key_pressed(Key::Escape)) {
        return false;
    }
    let pass = ctx.cumulative_pass_nr();
    let taken = ctx.data_mut(|d| {
        let taken = d.get_temp::<u64>(escape_id()) == Some(pass);
        if !taken {
            d.insert_temp(escape_id(), pass);
        }
        taken
    });
    !taken
}

/// Escape was pressed this frame, and nothing open took it.
fn escape_untaken(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.key_pressed(Key::Escape))
        && ctx.data(|d| d.get_temp::<u64>(escape_id())) != Some(ctx.cumulative_pass_nr())
}

fn escape_id() -> egui::Id {
    egui::Id::unique("escape taken")
}

/// The name the core's shortcuts know `key` by (keys.rs, as a plugin is told a press), if it has
/// one.
fn key_name(key: Key) -> Option<&'static str> {
    Some(match key {
        Key::OpenBracket | Key::OpenCurlyBracket => "[",
        Key::CloseBracket | Key::CloseCurlyBracket => "]",
        Key::Semicolon => ";",
        Key::Delete => "delete",
        Key::Backspace => "backspace",
        Key::ArrowLeft => "left",
        Key::ArrowRight => "right",
        Key::ArrowUp => "up",
        Key::ArrowDown => "down",
        Key::Space => "space",
        Key::Escape => "escape",
        Key::Minus => "-",
        Key::Equals => "=",
        Key::Plus => "plus",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::Backslash => "\\",
        Key::Quote => "'",
        Key::Backtick => "`",
        Key::F1 => "f1",
        Key::F2 => "f2",
        Key::F3 => "f3",
        Key::F4 => "f4",
        Key::F5 => "f5",
        Key::F6 => "f6",
        Key::F7 => "f7",
        Key::F8 => "f8",
        Key::F9 => "f9",
        Key::F10 => "f10",
        Key::F11 => "f11",
        Key::F12 => "f12",
        // the letters and digits by their names ("A", "0")
        _ => Some(key.name()).filter(|n| n.len() == 1)?,
    })
}

/// The drawing guide on as `kind`, or off if it already is that.
fn toggle_guide_kind(session: &CanvasSession, kind: sumi_core::GuideType) {
    let g = session.guide();
    if g.on && g.kind == kind {
        session.set_guide_on(false);
        return;
    }
    if !g.on {
        session.set_guide_on(true);
    }
    session.set_guide_kind(kind);
}

/// The drawing guide's lines, painted on `painter`: the main panel's own, before the top bar is, so
/// that the bar (and everything over the canvas) shows above them, as the canvas under it does.
fn guide_lines(painter: &egui::Painter, session: &CanvasSession) {
    let g = session.guide();
    if !g.on {
        return;
    }
    let ppp = painter.ctx().pixels_per_point();
    let [r, gr, b] = [0, 1, 2].map(|k| (g.color.get(k).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8);
    let colour = Color32::from_rgba_unmultiplied(r, gr, b, (g.opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
    let stroke = Stroke::new(0.25 + 1.5 * g.thickness, colour);
    for &[x0, y0, x1, y1] in g.lines.as_chunks::<4>().0 {
        painter.line_segment([egui::pos2(x0, y0) / ppp, egui::pos2(x1, y1) / ppp], stroke);
    }
}

/// The fine cross over the canvas at `at` (points), above all else.
fn hover_cross(ctx: &egui::Context, at: egui::Pos2) {
    egui::Area::new(egui::Id::unique("hover cross"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .fixed_pos(at)
        .show(ctx, |ui| fine_cross(ui.painter(), at));
}

/// A fine ring `radius` points round `at`, a cross at its middle, seen on any colour.
fn hover_ring(ctx: &egui::Context, at: egui::Pos2, radius: f32) {
    egui::Area::new(egui::Id::unique("hover ring")).order(egui::Order::Tooltip).interactable(false).fixed_pos(at).show(
        ctx,
        |ui| {
            for (width, colour) in [(3.0, Color32::WHITE), (1.0, Color32::BLACK)] {
                ui.painter().circle_stroke(at, radius, Stroke::new(width, colour));
            }
            fine_cross(ui.painter(), at);
        },
    );
}

/// The desktop's pointer over the canvas where no outline shows: a fine cross at `at` (points),
/// dark on a light edge, seen on any colour.
fn fine_cross(painter: &egui::Painter, at: egui::Pos2) {
    for (width, colour) in [(3.0, Color32::WHITE), (1.0, Color32::BLACK)] {
        let stroke = Stroke::new(width, colour);
        painter.line_segment([at - vec2(7.0, 0.0), at + vec2(7.0, 0.0)], stroke);
        painter.line_segment([at - vec2(0.0, 7.0), at + vec2(0.0, 7.0)], stroke);
    }
}

fn style(ctx: &egui::Context) {
    crate::look::install_fonts(ctx);
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |s| {
            // a native row's selected ink is selection.stroke, as egui's Button uses it
            s.visuals.selection.bg_fill = themed(theme, NATIVE_SELECTION);
            s.visuals.selection.stroke = Stroke::new(1.0, themed(theme, pc::TEXT));
            s.visuals.hyperlink_color = themed(theme, NATIVE_LINK);
            // the fields' cursor is the active ink, seen on the dark and the light interface
            s.visuals.text_cursor.stroke = Stroke::new(2.0, argb(pc::ACCENT));
            // native menus and combo popups use the same ink as the custom panels
            s.visuals.override_text_color = Some(themed(theme, pc::TEXT));
            s.visuals.window_fill = themed(theme, POPOVER_FILL);
            s.visuals.panel_fill = themed(theme, pc::BAR);
            s.visuals.slider_trailing_fill = true;
            s.spacing.button_padding = vec2(10.0, 5.0);
            s.spacing.item_spacing = vec2(8.0, 6.0);
        });
    }
    sync_native_theme(ctx);
}

/// `colour` as `theme` shows it, with a plugin's accent still taking its place.
fn themed(theme: egui::Theme, colour: Themed) -> Color32 {
    let value = match theme {
        egui::Theme::Dark => colour.0,
        egui::Theme::Light => colour.1,
    };
    argb(Themed(value, value))
}

/// Keeps egui's own widgets and menus on the same explicit look as the custom chrome. The
/// preference starts dark and changes only when the user chooses light (never from the OS).
fn sync_native_theme(ctx: &egui::Context) {
    let wanted = if crate::look::light() { egui::Theme::Light } else { egui::Theme::Dark };
    // Pin the preference itself: comparing the effective colour would leave System enabled when
    // the OS happens to be dark, and a later OS change could flip the native widgets.
    ctx.set_theme(wanted);
}

/// What an insert read on its own thread came to.
enum Inserted {
    /// A picture on a layer of its own (to transform), or none for want of room.
    Picture(bool),
    /// Frames each on a layer of its own (a moving picture's or a video's): how many, whether the
    /// canvas ran out of room, and whether Animation Assist comes on with them.
    Frames { added: u32, full: bool, assist: bool },
    /// It couldn't be read.
    Failed(&'static str),
}

/// The file at `path` brought onto the canvas (private layers, if `private`): a video's frames
/// each fitted on a layer of their own at the animation's frame rate (or 15 a second; for each
/// time a frame falls due, the video's frame nearest it; 999 at most), a moving picture's frames,
/// or a picture on a layer of its own.
fn read_insert(session: &CanvasSession, path: &std::path::Path, private: bool) -> Inserted {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if VIDEOS.contains(&extension.as_str()) {
        let settings = session.animation_settings();
        let fps = f64::from(settings.as_ref().map_or(VIDEO_FPS, |a| a.fps).clamp(1, 60));
        let (mut added, mut full, mut start) = (0u32, false, None);
        let read = crate::video::read_frames(path, |rgba, width, height, at| {
            // its time from the first frame's (a video may start late); the frames falling due
            // nearer it than the next one's take it
            let at = at - *start.get_or_insert(at);
            while !full && added < MOST_VIDEO_FRAMES && at + 0.5 / fps + 1e-3 >= f64::from(added) / fps {
                let image = sumi_core::ThumbnailImage { width, height, rgba: rgba.to_vec() };
                if session.insert_image(image, private).is_some() {
                    added += 1;
                } else {
                    full = true;
                }
            }
            !full && added < MOST_VIDEO_FRAMES
        });
        return match (added, read) {
            (0, Ok(_)) if full => Inserted::Frames { added: 0, full: true, assist: false },
            (0, Err(e)) if e == crate::video::NO_FFMPEG => Inserted::Failed(crate::video::NO_FFMPEG),
            (0, _) => Inserted::Failed("This video can't be opened."),
            _ => Inserted::Frames { added, full, assist: settings.is_none_or(|a| a.pages) },
        };
    }
    let Ok(bytes) = std::fs::read(path) else { return Inserted::Failed("This picture can't be opened.") };
    if let Some(done) = session.insert_gif_frames(bytes.clone(), private) {
        if done.added == 0 && !done.full {
            return Inserted::Failed("This picture can't be opened.");
        }
        return Inserted::Frames { added: done.added, full: done.full, assist: false };
    }
    match image::load_from_memory(&bytes) {
        Ok(picture) => {
            let rgba = picture.to_rgba8();
            let image = sumi_core::ThumbnailImage { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() };
            let on_model = session.model_info().is_some() && !session.model_flat();
            Inserted::Picture(if on_model {
                session.model_picture_begin(image, true)
            } else {
                session.insert_image(image, private).is_some()
            })
        }
        Err(_) => Inserted::Failed("This picture can't be opened."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_go_by_the_names_the_core_knows_and_go_as_they_are_with_no_plugin() {
        let named = [
            (Key::Z, "Z"),
            (Key::Num0, "0"),
            (Key::OpenCurlyBracket, "["),
            (Key::Space, "space"),
            (Key::Escape, "escape"),
            (Key::Plus, "plus"),
            (Key::Equals, "="),
            (Key::Backslash, "\\"),
            (Key::F1, "f1"),
            (Key::F12, "f12"),
            (Key::ArrowUp, "up"),
        ];
        for (key, name) in named {
            assert_eq!(key_name(key), Some(name), "{key:?}");
        }
        assert_eq!(key_name(Key::Tab), None);
        assert_eq!(key_name(Key::F13), None);
        // with no plugin rebinding keys, each press goes as it is (its key as the core names it)
        let plugins = sumi_core::Plugins::new();
        let press = KeyCombo { key: "Z".into(), command: true, shift: false, alt: false };
        assert_eq!(plugins.key_press(press).map(|p| (p.key, p.command)), Some(("z".to_owned(), true)));
        assert_eq!(plugins.key_shortcut_list(true, false), sumi_core::key_shortcut_list(true, false));
    }

    #[test]
    fn a_background_pick_sends_each_change_and_keeps_the_last_as_it_closes() {
        // white, a grey, starts on the brush's hue
        let mut pick = BackgroundPick::new(0xFFFFFF, 0.6);
        assert_eq!(pick.hsv, [0.6, 0.0, 1.0]);
        // shown as it was: nothing to send, nothing to keep
        assert_eq!(pick.follow([0.6, 0.0, 1.0]), None);
        assert_eq!(pick.kept(), None);
        // each new colour sent once, the last kept
        assert_eq!(pick.follow([0.0, 1.0, 1.0]), Some(0xFF0000));
        assert_eq!(pick.follow([0.0, 1.0, 1.0]), None);
        assert_eq!(pick.follow([1.0 / 3.0, 1.0, 1.0]), Some(0x00FF00));
        assert_eq!(pick.kept(), Some(0x00FF00));
        // back where it began is still a change to keep (the live changes were sent)
        assert_eq!(pick.follow([0.6, 0.0, 1.0]), Some(0xFFFFFF));
        assert_eq!(pick.kept(), Some(0xFFFFFF));
    }

    #[test]
    fn escape_closes_one_thing_per_press_and_leaves_none_for_settle_upright() {
        let ctx = egui::Context::default();
        let press = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(100.0, 100.0))),
            events: vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: Some(Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        // epaint panics if a frame's texture deltas are dropped unapplied
        let frame = || {
            let mut out = ctx.run_ui(press(), |_| {});
            out.textures_delta.clear();
        };
        frame();
        // the first thing that asks closes with this press; the rest in the same pass do not
        assert!(escape(&ctx));
        assert!(!escape(&ctx));
        assert!(!escape(&ctx));
        assert!(!escape_untaken(&ctx), "a taken press must not also turn the canvas upright");
        // a fresh press in a new pass is there for one taker again
        frame();
        assert!(escape_untaken(&ctx));
        assert!(escape(&ctx));
        assert!(!escape(&ctx));
    }
}

#[cfg(test)]
mod layers_dock_tests {
    use super::*;
    use egui::pos2;

    fn canvas() -> Rect {
        Rect::from_min_max(pos2(0.0, pc::BAR_HEIGHT), pos2(1440.0, 900.0))
    }

    #[test]
    fn the_canvas_keeps_clear_of_the_layers_dock() {
        let right = Rect::from_min_max(pos2(1140.0, pc::BAR_HEIGHT), pos2(1440.0, 900.0));
        assert_eq!(canvas_without_layers_dock(canvas(), Some(right)).max.x, 1140.0);
        let left = Rect::from_min_max(pos2(0.0, pc::BAR_HEIGHT), pos2(300.0, 900.0));
        assert_eq!(canvas_without_layers_dock(canvas(), Some(left)).min.x, 300.0);
        assert_eq!(canvas_without_layers_dock(canvas(), None), canvas());
    }

    #[test]
    fn the_canvas_area_is_where_the_view_fits() {
        let window = Rect::from_min_max(pos2(0.0, 0.0), pos2(1440.0, 900.0));
        let mut state = Interface::default();
        // whatever the kept preferences, the camera fits clear of the dock and the status bar
        {
            let prefs = &mut state.prefs;
            prefs.fit_inside = false;
            prefs.right_handed = false;
        }
        state.window_width = Some(window.width());
        state.dock_rect = Some(Rect::from_min_max(pos2(1140.0, pc::BAR_HEIGHT), pos2(1440.0, 900.0)));
        let [left, top, right, bottom] = state.fit_insets();
        let area = state.canvas_area(window);
        assert_eq!(area, Rect::from_min_max(pos2(left, top), pos2(1440.0 - right, 900.0 - bottom)));
        // the opaque dock keeps its part, and the status bar its strip along the bottom
        assert_eq!((area.min.x, area.min.y), (0.0, 0.0));
        assert_eq!(area.max.x, 1140.0);
        assert_eq!(area.max.y, window.bottom() - crate::chrome::STATUS_BAR_HEIGHT);
    }

    #[test]
    fn the_navigator_shares_the_dock_above_the_layers_and_goes_with_it() {
        for (width, height, shown) in [(1440.0_f32, 900.0_f32, true), (800.0, 600.0, false)] {
            let ctx = egui::Context::default();
            let session = CanvasSession::new(640, 480);
            session.resize_window(width as u32, height as u32);
            session.show_in(width as u32, height as u32);
            let mut state = Interface::default();
            state.opened(&session, 1.0, vec2(width, height));
            let mut input = Input::default();
            let raw = || egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height))),
                ..Default::default()
            };
            // Areas size on their first pass; warm up so the dock is registered at its place
            for _ in 0..3 {
                let mut out = ctx.run_ui(raw(), |ui| {
                    let _ = state.show_frame(ui, &session, &mut input);
                });
                // the fonts' first uploads: this test reads the shapes' layers, not the textures
                out.textures_delta.clear();
            }
            let section =
                state.dock_rect.and_then(|dock| crate::navigator::Navigator::shown(dock, false)).map(|(s, _)| s);
            assert_eq!(section.is_some(), shown, "the Navigator shows with the dock ({width}x{height})");
            if let Some(section) = section {
                let navigator = egui::LayerId::new(egui::Order::Middle, egui::Id::unique("navigator dock"));
                assert_eq!(
                    ctx.layer_id_at(section.center()),
                    Some(navigator),
                    "the Navigator paints in the dock at {width}x{height}"
                );
            } else {
                assert_eq!(state.dock_rect, None, "a compact window keeps the popover, and no Navigator");
            }
            // a 3D scene has no page to show at a glance: the Layers keep the whole dock
            assert_eq!(
                state.dock_rect.and_then(|dock| crate::navigator::Navigator::shown(dock, true)),
                None,
                "the Navigator keeps away from a 3D scene ({width}x{height})"
            );
        }
    }

    #[test]
    fn the_dock_picker_does_not_take_its_menus_presses_or_escape() {
        // its own tick always closes it
        assert!(Interface::picker_dismissed(true, true, false, false));
        assert!(Interface::picker_dismissed(true, false, false, false));
        // a press outside, or Escape, only closes it while none of its menus or hex pad is up
        assert!(!Interface::picker_dismissed(false, true, true, false));
        assert!(!Interface::picker_dismissed(false, true, false, true));
        assert!(Interface::picker_dismissed(false, false, true, false));
        assert!(Interface::picker_dismissed(false, false, false, true));
    }

    #[test]
    fn fitting_keeps_the_canvas_clear_of_the_dock_with_or_without_the_bars() {
        let (side, dock, scale) = (pc::SIDE_BAR_WIDTH + crate::panel::POPOVER_GAP, 300.0, 1.0);
        let status = crate::chrome::STATUS_BAR_HEIGHT;
        // without Fit Canvas Inside Interface, only the opaque dock and opaque status bar
        assert_eq!(fit_insets(false, false, true, false, dock, true, scale), [0.0, 0.0, dock, status]);
        assert_eq!(fit_insets(false, false, true, true, dock, true, scale), [dock, 0.0, 0.0, status]);
        // with it, the side bar on its side and the top bar; the dock stays opposite
        assert_eq!(fit_insets(true, false, true, false, dock, true, scale), [side, pc::BAR_HEIGHT, dock, status]);
        assert_eq!(fit_insets(true, false, true, true, dock, true, scale), [dock, pc::BAR_HEIGHT, side, status]);
        // compact windows or hidden status bar omit the bottom inset
        assert_eq!(fit_insets(false, false, true, false, 0.0, false, scale), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(fit_insets(true, false, false, false, 0.0, false, scale), [0.0, pc::BAR_HEIGHT, 0.0, 0.0]);
        // scaled DPI multiplies the status bar inset too
        assert_eq!(fit_insets(false, false, true, false, dock, true, 2.0), [0.0, 0.0, dock * 2.0, status * 2.0]);
        // full screen, before any early return
        assert_eq!(fit_insets(true, true, true, true, dock, true, scale), [0.0; 4]);
        assert_eq!(fit_insets(false, true, true, false, dock, true, scale), [0.0; 4]);
    }

    #[test]
    fn status_bar_shown_respects_fullscreen_modals_and_compact_widths() {
        let mut ui = Interface::default();
        ui.dock_rect = Some(Rect::from_min_max(pos2(1140.0, pc::BAR_HEIGHT), pos2(1440.0, 900.0)));
        assert!(ui.status_bar_shown(1440.0));
        // compact width without dock still fits >= 240
        ui.dock_rect = None;
        assert!(ui.status_bar_shown(800.0));
        // below 240 points wide, status bar hides so bottom inset is zero
        assert!(!ui.status_bar_shown(200.0));
        // full screen clears it
        ui.bars_hidden = true;
        assert!(!ui.status_bar_shown(1440.0));
        ui.bars_hidden = false;
        // gesture controls full-screen page hides it
        ui.gesture_controls.open = true;
        assert!(!ui.status_bar_shown(1440.0));
    }

    #[test]
    fn status_bar_fit_button_clicks_and_blocks_canvas_strokes_with_or_without_dock() {
        for (w, h) in [(1440.0_f32, 900.0_f32), (788.0_f32, 556.0_f32)] {
            let ctx = egui::Context::default();
            let mut ui_state = Interface::default();
            let mut input = Input::default();
            let session = CanvasSession::new(640, 480);
            session.resize_window(w as u32, h as u32);
            session.show_in(w as u32, h as u32);
            ui_state.opened(&session, 1.0, egui::vec2(w, h));
            // Zoom in so view_moving() starts false and settle_to_fit() starts a view motion
            session.view_gesture(w / 2.0, h / 2.0, 0.0, 0.0, 2.0, 0.0);
            assert!(!session.view_moving());

            let run_frame = |ui_state: &mut Interface, input: &mut Input, events: Vec<egui::Event>| -> Option<Rect> {
                let raw = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(w, h))),
                    events,
                    ..Default::default()
                };
                let mut fit_rect = None;
                let mut out = ctx.run_ui(raw, |egui_ui| {
                    fit_rect = ui_state.show_frame(egui_ui, &session, input);
                });
                // the fonts' and icons' first uploads: the clicks, not the textures, are read
                out.textures_delta.clear();
                fit_rect
            };

            // Two warm-up frames so egui Areas complete sizing pass and become interactable
            _ = run_frame(&mut ui_state, &mut input, Vec::new());
            let fit_rect = run_frame(&mut ui_state, &mut input, Vec::new()).expect("status bar Fit rect");
            let fit_pt = fit_rect.center();

            // 1. Lower canvas and x=950 (previously shadowed by layers dock overhang) remain paintable
            let mut canvas_probes = vec![pos2(w * 0.45, h - 40.0)];
            if w >= 1200.0 {
                canvas_probes.push(pos2(950.0, 500.0));
                canvas_probes.push(pos2(950.0, h - 40.0));
            }
            for pt in canvas_probes {
                assert!(
                    ui_state.canvas_rect.contains(pt)
                        && ctx.layer_id_at(pt).is_none_or(|l| l.order == egui::Order::Background),
                    "canvas probe {pt:?} ({w}x{h}) blocked! canvas_rect={:?}, layer={:?}",
                    ui_state.canvas_rect,
                    ctx.layer_id_at(pt),
                );
            }

            // 2. Entire opaque status bar (bar body + middle + Fit pill) must be excluded from canvas
            for bar_pt in [pos2(200.0, h - 13.0), pos2(w * 0.45, h - 13.0), fit_pt] {
                assert!(
                    !ui_state.canvas_rect.contains(bar_pt)
                        && ctx.layer_id_at(bar_pt)
                            == Some(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("sumi status bar"))),
                    "status bar at {bar_pt:?} ({w}x{h}) leaked or occluded! canvas_rect={:?}, layer={:?}",
                    ui_state.canvas_rect,
                    ctx.layer_id_at(bar_pt),
                );
            }

            // 3. Clicking the rendered Fit button center must trigger settle_to_fit()
            _ = run_frame(
                &mut ui_state,
                &mut input,
                vec![
                    egui::Event::PointerMoved(fit_pt),
                    egui::Event::PointerButton {
                        pos: fit_pt,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            _ = run_frame(
                &mut ui_state,
                &mut input,
                vec![egui::Event::PointerButton {
                    pos: fit_pt,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert!(
                session.view_moving(),
                "Fit click at {fit_pt:?} ({w}x{h}) did not trigger settle_to_fit! layer={:?}, dock={:?}",
                ctx.layer_id_at(fit_pt),
                ui_state.dock_rect,
            );
        }
    }

    #[test]
    fn docked_layers_button_uses_quiet_plate_leaving_active_tool_unique_rose_fill() {
        let previous = crate::look::light();
        let ctx = egui::Context::default();
        let session = CanvasSession::new(640, 480);
        session.resize_window(1440, 900);
        session.show_in(1440, 900);
        let mut state = Interface::default();
        state.opened(&session, 1.0, vec2(1440.0, 900.0));
        state.prefs.layers_dock = true;
        crate::look::set_light(false);
        let mut input = Input::default();
        let raw = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))),
            ..Default::default()
        };
        let mut last = None;
        for _ in 0..3 {
            crate::look::set_light(false);
            let mut out = ctx.run_ui(raw(), |ui| {
                let _ = state.show_frame(ui, &session, &mut input);
            });
            out.textures_delta.clear();
            last = Some(out);
        }
        let rose = argb(pc::TOOL_PLATE);
        crate::look::set_light(previous);

        assert!(state.dock_rect.is_some(), "WIDE 1440x900 should show the Layers dock");
        let output = last.expect("rendered frame");
        let mut rose_plates = 0usize;
        fn walk(shape: &egui::Shape, rose: Color32, count: &mut usize) {
            match shape {
                egui::Shape::Vec(items) => items.iter().for_each(|s| walk(s, rose, count)),
                egui::Shape::Rect(r) if r.fill == rose && r.rect.bottom() <= pc::BAR_HEIGHT + 0.5 => {
                    *count += 1;
                }
                _ => {}
            }
        }
        for clipped in &output.shapes {
            walk(&clipped.shape, rose, &mut rose_plates);
        }
        assert_eq!(rose_plates, 1, "only the active tool should hold the rose TOOL_PLATE fill while docked");
    }
}

#[cfg(test)]
mod brush_pill_tests {
    use super::*;
    use egui::{Event, FullOutput, Modifiers, PointerButton, Pos2, RawInput, Shape, pos2};

    const WIDE: (f32, f32) = (1440.0, 900.0);
    const COMPACT: (f32, f32) = (788.0, 556.0);

    struct Harness {
        ctx: egui::Context,
        state: Interface,
        input: Input,
        session: Arc<CanvasSession>,
        width: f32,
        height: f32,
        time: f64,
    }

    impl Harness {
        fn new(width: f32, height: f32) -> Harness {
            let ctx = egui::Context::default();
            let session = CanvasSession::new(640, 480);
            session.resize_window(width as u32, height as u32);
            session.show_in(width as u32, height as u32);
            let mut state = Interface::default();
            state.opened(&session, 1.0, vec2(width, height));
            let mut harness = Harness { ctx, state, input: Input::default(), session, width, height, time: 0.0 };
            // Areas size on their first pass; warm up so the bar and its popover are interactable.
            for _ in 0..3 {
                harness.frame(Vec::new());
            }
            harness
        }

        fn frame(&mut self, events: Vec<Event>) -> FullOutput {
            self.time += 1.0 / 60.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(self.width, self.height))),
                time: Some(self.time),
                events,
                ..RawInput::default()
            };
            let Harness { ctx, state, input, session, .. } = self;
            let mut out = ctx.run_ui(raw, |ui| {
                let _ = state.show_frame(ui, session, input);
            });
            // the fonts' and icons' first uploads: these tests read the shapes, not the textures
            out.textures_delta.clear();
            out
        }
    }

    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    fn click(harness: &mut Harness, at: Pos2) {
        harness.frame(vec![Event::PointerMoved(at)]);
        harness.frame(vec![button(at, true)]);
        harness.frame(vec![button(at, false)]);
    }

    fn each_shape(shape: &Shape, clip: Rect, visit: &mut impl FnMut(&Shape, Rect)) {
        match shape {
            Shape::Vec(shapes) => {
                for shape in shapes {
                    each_shape(shape, clip, visit);
                }
            }
            _ => visit(shape, clip),
        }
    }

    /// Text as the last frame paints it: where it is drawn, how far it is clipped, and its words.
    fn painted_texts(output: &FullOutput) -> Vec<(Rect, Rect, String, bool)> {
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            each_shape(&clipped.shape, clipped.clip_rect, &mut |shape, clip| {
                if let Shape::Text(text) = shape {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    texts.push((rect, clip, text.galley.text().to_owned(), text.galley.elided));
                }
            });
        }
        texts
    }

    /// The central readout drawn by the top bar (32 points tall, unlike the 44-point bar itself).
    fn brush_pill(output: &FullOutput) -> Option<Rect> {
        let mut pill = None;
        for clipped in &output.shapes {
            each_shape(&clipped.shape, clipped.clip_rect, &mut |shape, _| {
                if let Shape::Rect(rect) = shape {
                    let r = rect.rect;
                    if (r.height() - 32.0).abs() < 0.6
                        && r.width() >= 100.0
                        && r.top() >= -0.5
                        && r.bottom() <= pc::BAR_HEIGHT + 0.5
                    {
                        pill = Some(r);
                    }
                }
            });
        }
        pill
    }

    /// Where the top bar's actions end and its tools begin at `width`, from the bar's measured gaps.
    fn central_band(width: f32) -> (f32, f32) {
        let (tool_gap, action_gap) = pc::bar_gaps(width);
        let actions_end = pc::BAR_MARGIN + pc::GALLERY_WIDTH + pc::BAR_BUTTON_WIDTH * 4.0 + action_gap * 4.0;
        let tools_left = width - pc::BAR_MARGIN - pc::BAR_BUTTON_WIDTH - (pc::BAR_BUTTON_WIDTH + tool_gap) * 4.0;
        (actions_end, tools_left)
    }

    fn rename_current_brush(session: &CanvasSession, name: &str) {
        let _ = session.studio_begin();
        assert_eq!(session.set_studio_name(name.to_owned()), name);
        session.studio_end(true);
        assert_eq!(session.current_brush().name, name);
    }

    fn luminance(colour: Color32) -> f32 {
        fn channel(v: u8) -> f32 {
            let c = f32::from(v) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (mut hi, mut lo) = (luminance(a), luminance(b));
        if hi < lo {
            (hi, lo) = (lo, hi);
        }
        (hi + 0.05) / (lo + 0.05)
    }

    fn armed_polyline() -> Harness {
        let mut h = Harness::new(800.0, 600.0);
        h.state.selection.open(&h.session);
        _ = h.frame(Vec::new());
        let at =
            painted_texts(&h.frame(Vec::new())).into_iter().find(|(_, _, t, _)| t == "Polyline").unwrap().0.center();
        click(&mut h, at);
        assert!(h.state.selection.polyline_armed());
        h
    }

    fn canvas_touch(h: &mut Harness, id: u64, phase: winit::event::TouchPhase, at: (f32, f32)) {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Force, Touch, WindowEvent};
        let event = WindowEvent::Touch(Touch {
            device_id: DeviceId::dummy(),
            phase,
            location: PhysicalPosition::new(at.0 as f64, at.1 as f64),
            force: Some(Force::Normalized(0.5)),
            id,
        });
        h.input.on_event(&h.session, &event, false, |_| false);
    }

    #[test]
    fn polyline_armed_by_the_ui_touch_keeps_the_persisted_selection() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Force, Touch, TouchPhase, WindowEvent};

        let mut harness = Harness::new(800.0, 600.0);
        harness.state.selection.open(&harness.session);
        let m = harness.session.view_matrix();
        let [a, b, c, d, e, f] = m.get(..6).and_then(|v| <[f32; 6]>::try_from(v).ok()).expect("the view's six numbers");
        let at = |x: f32, y: f32| (a * x + c * y + e, b * x + d * y + f);
        let (old0, old1) = (at(20.0, 20.0), at(300.0, 220.0));
        harness.session.select_shape(false, old0.0, old0.1, old1.0, old1.1, sumi_core::SelectionJoin::Add);
        assert_eq!(harness.session.save_selection(), 1);
        let before = harness.session.saved_selection_thumbnail(0, 640, 480).expect("the selection before");
        assert_eq!((before.width, before.height), (640, 480), "full-size mask");
        let before = before.rgba;

        _ = harness.frame(Vec::new());
        let output = harness.frame(Vec::new());
        let polyline = painted_texts(&output)
            .into_iter()
            .find(|(_, _, text, _)| text == "Polyline")
            .expect("the Polyline segment")
            .0
            .center();
        click(&mut harness, polyline);
        assert!(harness.state.selection.polyline_armed());
        assert!(harness.input.polyline, "the Interface must pass the armed flag to Input");
        _ = harness.frame(Vec::new());
        harness.input.commands.clear();

        crate::pen::pointer_kind(9200, false);
        let path = [(340.0, 260.0), (620.0, 260.0), (620.0, 460.0), (340.0, 460.0), (340.0, 260.0)];
        for (i, &(x, y)) in path.iter().enumerate() {
            let (x, y) = at(x, y);
            let phase = match i {
                0 => TouchPhase::Started,
                i if i == path.len() - 1 => TouchPhase::Ended,
                _ => TouchPhase::Moved,
            };
            let event = WindowEvent::Touch(Touch {
                device_id: DeviceId::dummy(),
                phase,
                location: PhysicalPosition::new(x as f64, y as f64),
                force: Some(Force::Normalized(0.5)),
                id: 9200,
            });
            harness.input.on_event(&harness.session, &event, false, |_| false);
        }
        assert!(
            !harness.input.commands.iter().any(|c| matches!(c, sumi_core::TouchCommand::StrokeEnded)),
            "the armed touch must not paint"
        );
        let pending = harness.session.freehand_path();
        assert!(pending.is_empty(), "freehand path left: {}", pending.len());
        harness.state.selection.close(&harness.session);
        assert_eq!(harness.session.save_selection(), 2);
        let after = harness.session.saved_selection_thumbnail(1, 640, 480).expect("the selection after");
        assert_eq!((after.width, after.height), (640, 480), "full-size mask");
        assert_eq!(before, after.rgba, "the UI-armed Polyline must not select from a native touch");
    }

    #[test]
    fn polyline_armed_native_pinch_still_zooms_without_changing_the_selection() {
        let mut h = armed_polyline();
        let m = h.session.view_matrix();
        let [a, b, c, d, e, f] = m.get(..6).and_then(|v| <[f32; 6]>::try_from(v).ok()).expect("the view's six numbers");
        let at = |x: f32, y: f32| (a * x + c * y + e, b * x + d * y + f);
        let (o0, o1) = (at(20.0, 20.0), at(300.0, 220.0));
        h.session.select_shape(false, o0.0, o0.1, o1.0, o1.1, sumi_core::SelectionJoin::Add);
        assert_eq!(h.session.save_selection(), 1);
        let before = h.session.saved_selection_thumbnail(0, 640, 480).expect("the selection before").rgba;
        let z = h.session.view_matrix()[0].hypot(h.session.view_matrix()[1]);
        crate::pen::pointer_kind(9401, false);
        crate::pen::pointer_kind(9402, false);
        canvas_touch(&mut h, 9401, winit::event::TouchPhase::Started, at(300.0, 240.0));
        canvas_touch(&mut h, 9402, winit::event::TouchPhase::Started, at(340.0, 240.0));
        for step in 1..=5 {
            let d = step as f32 * 20.0;
            canvas_touch(&mut h, 9401, winit::event::TouchPhase::Moved, at(300.0 - d, 240.0));
            canvas_touch(&mut h, 9402, winit::event::TouchPhase::Moved, at(340.0 + d, 240.0));
        }
        canvas_touch(&mut h, 9401, winit::event::TouchPhase::Ended, at(200.0, 240.0));
        canvas_touch(&mut h, 9402, winit::event::TouchPhase::Ended, at(440.0, 240.0));
        assert!(h.input.polyline_gesture);
        let z2 = h.session.view_matrix()[0].hypot(h.session.view_matrix()[1]);
        assert!(z2 > z * 1.2, "{z2} from {z}");
        _ = h.frame(Vec::new());
        assert!(!h.input.polyline_gesture, "the multi-finger latch must clear");
        assert_eq!(h.session.save_selection(), 2);
        let after = h.session.saved_selection_thumbnail(1, 640, 480).expect("the selection after").rgba;
        assert_eq!(before, after);
    }

    #[test]
    fn deselect_with_the_selection_tool_closed_leaves_the_canvas_painting() {
        let mut h = Harness::new(800.0, 600.0);
        // Ctrl+D with the selection tool closed: it clears what is selected, and the canvas goes on
        // painting instead of taking the next touch as a new selection.
        h.state.selection.clear(&h.session);
        let sample = |x: f32, y: f32| sumi_core::PenSample {
            x,
            y,
            pressure: 1.0,
            tilt: 0.0,
            orientation: 0.0,
            time_ms: 0.0,
            roll: 0.0,
        };
        let out = h.session.touch_down(1, sumi_core::PointerKind::Mouse, vec![sample(100.0, 100.0)]);
        assert!(
            out.iter().any(|c| matches!(c, TouchCommand::Cursor { .. })),
            "a stroke must begin, not a selection: {out:?}"
        );
        let up = h.session.touch_up(1, vec![sample(100.0, 100.0)]);
        assert!(up.iter().any(|c| matches!(c, TouchCommand::StrokeEnded)), "{up:?}");
    }

    #[test]
    fn escape_restores_the_selection_touch_and_the_real_finger_settings() {
        let mut h = armed_polyline();
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        _ = h.frame(vec![esc]);
        let m = h.session.view_matrix();
        let [a, b, c, d, e, f] = m.get(..6).and_then(|v| <[f32; 6]>::try_from(v).ok()).expect("the view's six numbers");
        let at = |x: f32, y: f32| (a * x + c * y + e, b * x + d * y + f);
        crate::pen::pointer_kind(9500, false);
        canvas_touch(&mut h, 9500, winit::event::TouchPhase::Started, at(100.0, 100.0));
        canvas_touch(&mut h, 9500, winit::event::TouchPhase::Moved, at(200.0, 100.0));
        canvas_touch(&mut h, 9500, winit::event::TouchPhase::Moved, at(200.0, 200.0));
        canvas_touch(&mut h, 9500, winit::event::TouchPhase::Ended, at(100.0, 200.0));
        assert!(!h.session.freehand_path().is_empty(), "the selection touch must be restored");
        h.state.selection.close(&h.session);
        crate::pen::pointer_kind(9501, false);
        canvas_touch(&mut h, 9501, winit::event::TouchPhase::Started, at(300.0, 300.0));
        canvas_touch(&mut h, 9501, winit::event::TouchPhase::Moved, at(400.0, 300.0));
        canvas_touch(&mut h, 9501, winit::event::TouchPhase::Ended, at(400.0, 300.0));
        assert!(
            h.input.commands.iter().any(|c| matches!(c, sumi_core::TouchCommand::StrokeEnded)),
            "the real finger paint must return"
        );
    }

    fn guide(output: &FullOutput, from: Pos2, to: Pos2) -> bool {
        let mut yes = false;
        for clipped in &output.shapes {
            each_shape(&clipped.shape, clipped.clip_rect, &mut |shape, _| {
                if let Shape::LineSegment { points, stroke } = shape {
                    yes |= stroke.width == 1.0 && points[0].distance(from) < 1.0 && points[1].distance(to) < 1.0;
                }
            });
        }
        yes
    }
    #[test]
    fn polyline_guide_stays_out_of_the_chrome() {
        let mut h = armed_polyline();
        let vertex = pos2(200.0, 200.0);
        let inside = pos2(300.0, 250.0);
        click(&mut h, vertex);
        let output = h.frame(vec![Event::PointerMoved(inside)]);
        assert!(guide(&output, vertex, inside), "the inside preview");
        let output = h.frame(vec![Event::PointerMoved(pos2(1.0, 1.0))]);
        assert!(!guide(&output, vertex, pos2(1.0, 1.0)), "no guide over the top bar");
        click(&mut h, pos2(1.0, 1.0));
        assert!(guide(&h.frame(vec![Event::PointerMoved(inside)]), vertex, inside), "a top bar click adds no vertex");
    }

    #[test]
    fn the_native_egui_theme_and_popup_fills_follow_the_explicit_light_preference() {
        let previous = crate::look::light();
        crate::look::set_light(false);
        let mut harness = Harness::new(COMPACT.0, COMPACT.1);
        // Interface::default reads the preference file; pin the tested look after it.
        crate::look::set_light(false);
        let _ = harness.frame(Vec::new());
        let dark_theme = harness.ctx.theme();
        let dark_style = harness.ctx.style_of(egui::Theme::Dark);
        crate::look::set_light(true);
        let _ = harness.frame(Vec::new());
        let light_theme = harness.ctx.theme();
        let light_style = harness.ctx.style_of(egui::Theme::Light);
        crate::look::set_light(false);
        let _ = harness.frame(Vec::new());
        let back_theme = harness.ctx.theme();
        let back_fill = harness.ctx.style_of(egui::Theme::Dark).visuals.window_fill;
        // Never leave the global look swapped for another test, even when asserting.
        crate::look::set_light(previous);
        assert_eq!(dark_theme, egui::Theme::Dark);
        assert_eq!(light_theme, egui::Theme::Light, "the native widgets stayed dark in the light look");
        assert_eq!(back_theme, egui::Theme::Dark, "the native widgets did not return to dark");
        assert_ne!(dark_style.visuals.window_fill, light_style.visuals.window_fill);
        assert_eq!(back_fill, dark_style.visuals.window_fill, "the dark native fill was not kept");
        for (what, fill, ink) in [
            ("dark", dark_style.visuals.window_fill, dark_style.visuals.text_color()),
            ("light", light_style.visuals.window_fill, light_style.visuals.text_color()),
        ] {
            let got = contrast(fill, ink);
            assert!(got >= 4.5, "{what} native popup ink: measured {got:.2}");
        }
    }

    #[test]
    fn the_native_style_pairs_keep_measured_foreground_contrast() {
        let ctx = egui::Context::default();
        style(&ctx);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let s = ctx.style_of(theme);
            let pairs = [
                // what egui paints a normal and a selected native row, and a hyperlink, with
                ("normal popup", s.visuals.text_color(), s.visuals.window_fill),
                ("selected row", s.visuals.selection.stroke.color, s.visuals.selection.bg_fill),
                ("hyperlink", s.visuals.hyperlink_color, s.visuals.window_fill),
            ];
            for (what, ink, surface) in pairs {
                let got = contrast(ink, surface);
                assert!(got >= 4.5, "{theme:?} native {what}: measured {got:.2} between ink and surface");
            }
        }
    }

    #[test]
    fn the_native_theme_sync_pins_the_dark_preference_instead_of_leaving_system() {
        let previous = crate::look::light();
        crate::look::set_light(false);
        let ctx = egui::Context::default();
        // egui's default is System with no system theme, which resolves to the Dark fallback:
        // effective Dark, but still free to follow the OS. The sync must pin Dark explicitly.
        assert_eq!(ctx.options(|o| o.theme_preference), egui::ThemePreference::System);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        style(&ctx);
        let preference = ctx.options(|o| o.theme_preference);
        let effective = ctx.theme();
        crate::look::set_light(previous);
        assert_eq!(
            preference,
            egui::ThemePreference::Dark,
            "the native sync left System enabled, so an OS light change could flip the widgets"
        );
        assert_eq!(effective, egui::Theme::Dark);
    }

    #[test]
    fn the_current_brush_name_and_readout_are_rendered_inside_the_pill_at_wide_and_compact_widths() {
        for (width, height) in [WIDE, COMPACT] {
            let mut harness = Harness::new(width, height);
            let output = harness.frame(Vec::new());
            let pill = brush_pill(&output).unwrap_or_else(|| panic!("no central brush pill at {width}"));
            let (band_left, band_right) = central_band(width);
            assert!(pill.height() >= 32.0 - 0.5, "pill is shorter than 32 at {width}: {pill:?}");
            assert!(
                pill.left() >= band_left - 0.5 && pill.right() <= band_right + 0.5,
                "pill overlaps an action or tool at {width}: {pill:?} in {band_left}..{band_right}"
            );
            if width == COMPACT.0 {
                assert!(pill.width() < 216.0, "compact pill should adapt below its widest, got {pill:?}");
            }
            let name = harness.session.current_brush().name;
            let texts = painted_texts(&output);
            let name_text = texts
                .iter()
                .find(|(_, _, text, _)| text.as_str() == name.as_str())
                .unwrap_or_else(|| panic!("the current brush name {name:?} is not painted whole at {width}"));
            assert!(
                pill.contains_rect(name_text.0),
                "the brush name paints outside the pill at {width}: {name_text:?} in {pill:?}"
            );
            assert!(
                pill.contains_rect(name_text.1),
                "the brush name is clipped outside the pill at {width}: {name_text:?} in {pill:?}"
            );
            let readout = texts
                .iter()
                .find(|(rect, _, text, _)| rect.top() < pc::BAR_HEIGHT && text.contains(" px"))
                .unwrap_or_else(|| panic!("no size readout is painted at {width}"));
            assert!(
                pill.contains_rect(readout.0),
                "the readout paints outside the pill at {width}: {readout:?} in {pill:?}"
            );
            assert!(pill.contains_rect(readout.1), "the readout is clipped outside the pill at {width}");
            let opacity = texts.iter().find(|(rect, _, text, _)| rect.top() < pc::BAR_HEIGHT && text.contains('%'));
            if width == WIDE.0 {
                assert!(opacity.is_some(), "the wide pill has room for the opacity and should show it");
            } else {
                assert!(opacity.is_none(), "the compact pill gave the default brush name away for the opacity");
            }
        }
    }

    #[test]
    fn a_long_brush_name_is_elided_inside_the_compact_pill_with_its_readout() {
        let mut harness = Harness::new(COMPACT.0, COMPACT.1);
        let long = "Calligraphy Pressure Fine Liner With Texture 03";
        rename_current_brush(&harness.session, long);
        let output = harness.frame(Vec::new());
        let pill = brush_pill(&output).expect("no central brush pill at compact width");
        assert!(pill.width() <= 216.0 + 0.5, "pill is wider than its cap: {pill:?}");
        let texts = painted_texts(&output);
        let shown = texts
            .iter()
            .find(|(_, _, text, _)| text.starts_with("Calli") && text.ends_with('\u{2026}'))
            .unwrap_or_else(|| panic!("the long brush name was not elided: {texts:?}"));
        assert!(pill.contains_rect(shown.0), "the elided name paints outside the pill: {shown:?}");
        assert!(pill.contains_rect(shown.1), "the elided name is clipped outside the pill: {shown:?}");
        let readout = texts
            .iter()
            .find(|(rect, _, text, _)| rect.top() < pc::BAR_HEIGHT && text.contains(" px"))
            .expect("the size readout went away with a long name");
        assert!(pill.contains_rect(readout.0), "the readout paints outside the pill: {readout:?}");
        assert!(pill.contains_rect(readout.1), "the readout is clipped outside the pill: {readout:?}");
        assert!(
            texts.iter().all(|(rect, _, text, _)| rect.top() >= pc::BAR_HEIGHT || !text.contains('%')),
            "a long name took the space the compact pill keeps for its size, not the opacity"
        );
        let (band_left, band_right) = central_band(COMPACT.0);
        for (rect, clip, text, _) in &texts {
            if text.starts_with("Calli") || (rect.top() < pc::BAR_HEIGHT && text.contains(" px")) {
                assert!(
                    rect.left() >= band_left - 0.5 && rect.right() <= band_right + 0.5,
                    "readout paints outside the central band: {text:?} at {rect:?} in {band_left}..{band_right}"
                );
                assert!(
                    clip.left() >= band_left - 0.5 && clip.right() <= band_right + 0.5,
                    "readout is clipped outside the central band: {text:?} at {clip:?}"
                );
            }
        }
    }

    #[test]
    fn compact_keeps_the_default_name_and_size_whole_and_drops_opacity_before_eliding() {
        for (width, height, opacity_expected) in [(COMPACT.0, COMPACT.1, false), (WIDE.0, WIDE.1, true)] {
            let mut harness = Harness::new(width, height);
            let output = harness.frame(Vec::new());
            let pill = brush_pill(&output).expect("no central brush pill");
            let name = harness.session.current_brush().name;
            let texts = painted_texts(&output);
            let shown_name = texts
                .iter()
                .find(|(rect, _, text, _)| rect.top() < pc::BAR_HEIGHT && text.as_str() == name.as_str())
                .unwrap_or_else(|| panic!("the default brush name {name:?} was cut at {width}"));
            assert!(pill.contains_rect(shown_name.0), "the whole name paints outside the pill at {width}");
            let size = texts
                .iter()
                .find(|(rect, _, text, _)| rect.top() < pc::BAR_HEIGHT && text.contains(" px"))
                .unwrap_or_else(|| panic!("the brush size went missing at {width}"));
            assert!(pill.contains_rect(size.0), "the brush size paints outside the pill at {width}");
            let has_opacity = texts.iter().any(|(rect, _, text, _)| rect.top() < pc::BAR_HEIGHT && text.contains('%'));
            assert_eq!(
                has_opacity, opacity_expected,
                "opacity shown {has_opacity} at {width}, expected {opacity_expected}"
            );
        }
    }

    #[test]
    fn clicking_the_pill_opens_the_existing_brush_library_for_each_tool_without_painting() {
        for tool in [ToolKind::Paint, ToolKind::Erase, ToolKind::Smudge] {
            let mut harness = Harness::new(COMPACT.0, COMPACT.1);
            harness.session.set_tool(tool);
            let _ = harness.frame(Vec::new());
            let output = harness.frame(Vec::new());
            let pill = brush_pill(&output).unwrap_or_else(|| panic!("no pill for {tool:?}"));
            let edits = harness.session.edits();
            click(&mut harness, pill.center());
            assert_eq!(harness.state.open, Some(Popover::Brushes), "the pill opened no brushes for {tool:?}");
            assert_eq!(harness.session.info().tool, tool, "the pill changed the chosen tool for {tool:?}");
            assert_eq!(harness.session.edits(), edits, "the pill painted into the artwork for {tool:?}");
            assert!(!harness.state.canvas_rect.contains(pill.center()), "the pill sits over the canvas");
        }
    }

    #[test]
    fn the_pill_click_is_taken_across_the_full_44_point_top_bar_height() {
        // The upper probe is the measured one (cy - 19); the top one also clears egui's 5-point
        // hit-test radius, so it only reaches the pill when the whole bar height takes the tap.
        for probe_y in [pc::BAR_HEIGHT / 2.0 - 19.0, 0.25] {
            let mut harness = Harness::new(COMPACT.0, COMPACT.1);
            let output = harness.frame(Vec::new());
            let pill = brush_pill(&output).expect("no central brush pill at compact width");
            let probe = pos2(pill.center().x, probe_y);
            assert!(!pill.contains(probe), "the probe should be above the 32-point visual pill: {pill:?}");
            click(&mut harness, probe);
            assert_eq!(
                harness.state.open,
                Some(Popover::Brushes),
                "the full-height hit band did not open the brushes at {probe:?}"
            );
        }
    }

    #[test]
    fn at_640_the_brush_button_still_opens_the_library_when_the_pill_gives_way() {
        let mut harness = Harness::new(640.0, 480.0);
        let output = harness.frame(Vec::new());
        assert!(brush_pill(&output).is_none(), "a 640 wide bar has no room for the central pill");
        let (tool_gap, _) = pc::bar_gaps(640.0);
        let paint_left = 640.0 - pc::BAR_MARGIN - pc::BAR_BUTTON_WIDTH - (pc::BAR_BUTTON_WIDTH + tool_gap) * 4.0;
        let paint = pos2(paint_left + pc::BAR_BUTTON_WIDTH / 2.0, pc::BAR_HEIGHT / 2.0);
        let edits = harness.session.edits();
        click(&mut harness, paint);
        assert_eq!(harness.state.open, Some(Popover::Brushes), "the Brush button no longer reaches the library");
        assert_eq!(harness.session.info().tool, ToolKind::Paint, "the Brush button changed the tool");
        assert_eq!(harness.session.edits(), edits, "the Brush button painted into the artwork");
    }

    #[test]
    fn the_transparent_swatch_switches_to_the_erase_tool_through_the_interface() {
        let mut harness = Harness::new(WIDE.0, WIDE.1);
        harness.session.set_tool(ToolKind::Paint);
        harness.state.open = Some(Popover::Colour);
        let _ = harness.frame(Vec::new());
        assert_eq!(harness.state.open, Some(Popover::Colour), "the colour panel did not open");
        let (brush, second) = (harness.state.colours.hsv, harness.state.colours.secondary);
        let kept = harness.state.colours.history.colours();
        let edits = harness.session.edits();
        // the header's swatch asks for the Erase tool: the interface takes it by the same path as
        // the toolbar's button and `E`, closing the panel as those do
        harness.state.colours.request_erase();
        let _ = harness.frame(Vec::new());
        assert_eq!(harness.session.info().tool, ToolKind::Erase, "the request chose no eraser");
        assert_eq!(harness.state.open, None, "the colour panel stayed open after the tool changed");
        assert_eq!(harness.state.colours.hsv, brush, "the transparent swatch changed the colour");
        assert_eq!(harness.state.colours.secondary, second, "the transparent swatch changed the second colour");
        assert_eq!(harness.state.colours.history.colours(), kept, "the transparent swatch changed the history");
        assert_eq!(harness.session.edits(), edits, "choosing the eraser painted into the artwork");
    }

    /// The top bar's own fills as a frame paints it: the band along the top, the window's width and
    /// about `pc::BAR_HEIGHT` tall (its own surface, and the glass laid over it). egui leaves the
    /// top panel's one-point separator margin out of the frame's painted rect, so the surface lands
    /// a point short of the full bar height: allow a couple of points rather than an exact match.
    fn bar_fills(output: &FullOutput, width: f32) -> Vec<Color32> {
        let mut fills = Vec::new();
        for clipped in &output.shapes {
            each_shape(&clipped.shape, clipped.clip_rect, &mut |shape, _| {
                if let Shape::Rect(rect) = shape {
                    let r = rect.rect;
                    if r.top().abs() < 0.6 && (r.height() - pc::BAR_HEIGHT).abs() <= 2.0 && r.width() >= width - 0.6 {
                        fills.push(rect.fill);
                    }
                }
            });
        }
        fills
    }

    /// The left button pressed at `at` (window pixels) on the canvas, as the input layer hears it:
    /// the canvas paints from there. True if the input layer took it as a press on the canvas.
    fn press_canvas(harness: &mut Harness, at: Pos2) -> bool {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        let moved = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(f64::from(at.x), f64::from(at.y)),
        };
        harness.input.on_event(&harness.session, &moved, false, |_| false);
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        harness.input.on_event(&harness.session, &press, false, |_| false)
    }

    /// The left button let go at `at`, ending the stroke the press began.
    fn release_canvas(harness: &mut Harness, at: Pos2) {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        let moved = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(f64::from(at.x), f64::from(at.y)),
        };
        harness.input.on_event(&harness.session, &moved, false, |_| false);
        let lifted = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        harness.input.on_event(&harness.session, &lifted, false, |_| false);
    }

    #[test]
    fn the_chrome_recedes_while_the_canvas_is_painted_and_breathes_back() {
        // pinned dark for the whole test, as the look is global (restored before asserting)
        let previous = crate::look::light();
        crate::look::set_light(false);
        let mut h = Harness::new(WIDE.0, WIDE.1);
        crate::look::set_light(false);
        let bar = argb(pc::BAR);
        let at = h.state.canvas_rect.center();
        let rest = h.frame(Vec::new());
        let at_rest = bar_fills(&rest, h.width).contains(&bar);
        let idle_dim = h.state.chrome_dim;
        assert!(press_canvas(&mut h, at), "a press on the canvas begins a stroke");
        for _ in 0..4 {
            _ = h.frame(Vec::new());
        }
        let painted = h.frame(Vec::new());
        let painting_dim = h.state.chrome_dim;
        let painted_fills = bar_fills(&painted, h.width);
        // let go, and it breathes back
        release_canvas(&mut h, at);
        for _ in 0..120 {
            _ = h.frame(Vec::new());
        }
        let back = h.frame(Vec::new());
        let back_dim = h.state.chrome_dim;
        let back_fills = bar_fills(&back, h.width);
        crate::look::set_light(previous);

        assert!(at_rest, "the bar at rest: {:?}", bar_fills(&rest, h.width));
        assert_eq!(idle_dim, 0.0, "nothing is painted, so nothing recedes");
        if crate::colour_drop::reduce_motion() {
            assert_eq!(painting_dim, 0.0, "reduce motion leaves the chrome still");
            assert!(painted_fills.contains(&bar), "and the bar unchanged: {painted_fills:?}");
        } else {
            assert!(painting_dim > 0.0, "the chrome steps back while a stroke is painted");
            let receded = crate::look::receded(bar, painting_dim);
            assert!(!painted_fills.contains(&bar), "no bar at rest while painting: {painted_fills:?}");
            assert!(painted_fills.contains(&receded), "the bar recedes by the dim: {painted_fills:?}");
        }
        assert_eq!(back_dim, 0.0, "the dim settles back to nothing");
        assert!(back_fills.contains(&bar), "the bar at rest again: {back_fills:?}");
    }
}
