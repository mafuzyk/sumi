//! The Navigator: the section of the Layers dock above the layers, as the Android app's
//! (ui/NavigatorUi.kt): the whole canvas at a glance, the part of it the window shows, and Fit,
//! − and + for the view. A press or drag on the picture moves the view there, so the rectangle
//! follows the pointer. It shares the dock's frame and width: it is no window of its own and
//! takes no press from the canvas, which the dock already keeps clear of.

use std::time::{Duration, Instant};

use egui::{Align2, Color32, Id, Order, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, Vec2, pos2, vec2};
use sumi_core::CanvasSession;

use crate::look::{Themed, Weight, argb, pc, text};
use crate::menu::{self, Menu};
use crate::panel;

/// The section's height: its header, the picture and the row of controls under them.
const HEIGHT: f32 = 240.0;
/// The least the Layers keep under the Navigator: their dock header, the current layer's strip
/// and the gap under it, their footer, and three rows, as `layers.rs` lays them out at the dock's
/// width (56, 42 points and 54-point rows with their 1-point gaps; the header is the dock's).
const LEAST_LAYERS: f32 = panel::DOCK_HEADER_HEIGHT + 56.0 + 6.0 + 42.0 + 3.0 * (54.0 + 1.0);
/// The least the dock may be for the Navigator and a usable Layers to share it: shorter docks
/// leave it all to the Layers.
const LEAST_DOCK: f32 = HEIGHT + LEAST_LAYERS;
/// How large the canvas's picture is read: its longer side, pixels.
const PICTURE_SIDE: u32 = 320;
/// The canvas is read again at most this often while it changes.
const REFRESH: Duration = Duration::from_millis(300);
/// How far in the section's contents sit from its sides, and from its header and bottom.
const INSET: f32 = 14.0;
const UNDER_HEADER: f32 = 10.0;
const ABOVE_BOTTOM: f32 = 12.0;
/// The row of controls' height, and the gap above it and between its buttons.
const CONTROLS: f32 = 30.0;
const CONTROL_GAP: f32 = 6.0;
/// The − and + buttons' width.
const STEP: f32 = 34.0;
/// How much one press of − or + zooms by.
const ZOOM_STEP: f32 = 1.25;
/// The corners of the picture's controls.
const CORNER: f32 = 6.0;
/// The picture's plate keeps the page's own square corners: a rounded plate would show through
/// the square image drawn over it, its radius gone wherever the image reaches the edge.
const PLATE_CORNER: f32 = 0.0;
/// How wide the title's menu is.
const MENU_WIDTH: f32 = 240.0;
/// The section's picture plate, its own frame and the view's rectangle: the dock's own colours.
const PICTURE_FILL: Themed = panel::DOCK_HEADER_FILL;
const EDGE: Themed = panel::DOCK_EDGE;
/// What the controls rest on, and what they turn to under the pointer.
const CONTROL_FILL: Themed = pc::BAR_PILL;
const CONTROL_HOVER: Themed = pc::ACTION_DISC;

/// Whether the canvas should be read again: the picture's edits (and a model's texture set)
/// changed, and at most every [`REFRESH`], as the reference window reads it.
#[derive(Default)]
struct Refreshed {
    /// The edits the picture was read at, the model's texture set then, and when.
    edits: Option<u64>,
    set: Option<u32>,
    at: Option<Instant>,
}

impl Refreshed {
    /// Whether the canvas as it is now (at `edits`, texture set `set`) should be read at `now`.
    fn due(&self, edits: u64, set: u32, now: Instant) -> bool {
        if self.edits == Some(edits) && self.set == Some(set) {
            return false;
        }
        self.at.is_none_or(|at| now.saturating_duration_since(at) >= REFRESH)
    }

    /// How long until a changed canvas may be read again; none when it may be read now.
    fn wait(&self, now: Instant) -> Option<Duration> {
        self.at.map(|at| REFRESH.saturating_sub(now.saturating_duration_since(at))).filter(|w| !w.is_zero())
    }

    /// The canvas was read at `edits` and texture set `set`, `now`.
    fn took(&mut self, edits: u64, set: u32, now: Instant) {
        self.edits = Some(edits);
        self.set = Some(set);
        self.at = Some(now);
    }

    /// The canvas could not be read at `now` (not made yet, or the read failed): the attempt is
    /// recorded so that the next one waits out [`REFRESH`] rather than coming every frame.
    fn missed(&mut self, now: Instant) {
        self.at = Some(now);
    }

    /// Another artwork: its picture is not this one's, whatever its edits.
    fn forget(&mut self) {
        *self = Refreshed::default();
    }
}

#[derive(Default)]
pub struct Navigator {
    /// The canvas as last read (premultiplied RGBA), and its edits and time.
    picture: Option<TextureHandle>,
    read: Refreshed,
    /// The title's menu, under it: Fit Canvas, and Reference.
    title_menu: Menu,
}

impl Navigator {
    /// Another artwork is shown: its picture is read afresh, whatever the last one's edits were.
    pub fn forget(&mut self) {
        self.picture = None;
        self.read.forget();
        self.title_menu.close();
    }

    /// The dock (or the Navigator) went away: its menu goes with it.
    pub fn close(&mut self) {
        self.title_menu.close();
    }

    /// Splits the dock into the Navigator's section on top and the Layers' below it; none when
    /// the dock is too short for both, where the Layers keep all of it.
    pub fn split(dock: Rect) -> Option<(Rect, Rect)> {
        if dock.height() < LEAST_DOCK {
            return None;
        }
        let navigator = Rect::from_min_max(dock.min, pos2(dock.right(), dock.top() + HEIGHT));
        let layers = Rect::from_min_max(pos2(dock.left(), navigator.bottom()), dock.max);
        Some((navigator, layers))
    }

    /// Splits the dock as [`Navigator::split`] does, but only while there is a page to show at a
    /// glance: a 3D scene (a model not shown flat) has none, so the Layers keep the whole dock.
    pub fn shown(dock: Rect, model_scene: bool) -> Option<(Rect, Rect)> {
        if model_scene { None } else { Self::split(dock) }
    }

    /// Shows the section at `rect`, `right` saying which side of the window the dock is on: the
    /// dock's own frame and header, the canvas with the window's rectangle on it, and the
    /// controls. `visible` is the area of the window the canvas shows in (window points: not the
    /// dock, nor the status bar), which the view's rectangle is read in. `reference_on` ticks
    /// Reference in its title's menu. Returns whether SubView was asked for.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        session: &CanvasSession,
        rect: Rect,
        visible: Rect,
        right: bool,
        reference_on: bool,
        interactive: bool,
    ) -> bool {
        self.read_canvas(ctx, session);
        let info = session.info();
        let page = vec2(info.width as f32, info.height as f32);
        let matrix = affine(&session.view_matrix());
        // the area the canvas shows in, in the window pixels the view's numbers are in
        let scale = ctx.pixels_per_point();
        let visible = visible.intersect(ctx.content_rect());
        let area = Rect::from_min_max(
            pos2(visible.left() * scale, visible.top() * scale),
            pos2(visible.right() * scale, visible.bottom() * scale),
        );
        let preview = preview_rect(rect);
        let mut title = Rect::NOTHING;
        egui::Area::new(Id::unique("navigator dock"))
            .fixed_pos(rect.min)
            .default_size(rect.size())
            .constrain(false)
            .order(Order::Middle)
            .interactable(interactive)
            .show(ctx, |ui| {
                ui.set_max_size(rect.size());
                ui.allocate_rect(rect, if interactive { Sense::click_and_drag() } else { Sense::hover() });
                panel::dock_frame(ui, rect, right);
                // the title sits as the Layers dock's does, its chevron opening its menu
                let (tapped, row) =
                    panel::header_title(ui, look, rect.translate(vec2(0.0, -6.0)), "Navigator", 20.0, true, false);
                title = row;
                if tapped {
                    self.title_menu.open();
                }
                if preview.width() > 0.0 && preview.height() > 0.0 {
                    let painter = ui.painter().with_clip_rect(preview.intersect(ui.clip_rect()));
                    painter.rect_filled(preview, PLATE_CORNER, argb(PICTURE_FILL));
                    let page_rect = fit_page(page, preview);
                    let shows = page.x > 0.0 && page.y > 0.0;
                    if shows && let Some(picture) = self.picture.as_ref() {
                        painter.image(
                            picture.id(),
                            page_rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        painter.rect_stroke(page_rect, 0.0, Stroke::new(1.0, argb(EDGE)), egui::StrokeKind::Inside);
                        // the window stands taller or wider than the page at fit: its rectangle
                        // reaches past the picture and is kept whole, clipped only to the preview
                        if let Some(quad) = visible_quad(&matrix, area) {
                            let points: Vec<Pos2> = quad.iter().map(|&p| on_page(p, page, page_rect)).collect();
                            let shown = clipped_to_rect(&points, preview);
                            if shown.len() >= 3 {
                                painter.add(egui::Shape::convex_polygon(
                                    shown,
                                    Color32::TRANSPARENT,
                                    Stroke::new(1.5, argb(pc::ACCENT)),
                                ));
                            }
                        }
                    } else if let Some(words) = plate_words(session.canvas_made()) {
                        text(
                            &painter,
                            preview.center(),
                            Align2::CENTER_CENTER,
                            words,
                            12.0,
                            Weight::Regular,
                            argb(pc::BAR_TEXT),
                        );
                    }
                    // a press or drag on the picture moves the view there
                    let picture =
                        ui.interact(preview, ui.scope_id().with("navigator picture"), Sense::click_and_drag());
                    let cursor = if picture.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab };
                    let picture = picture.on_hover_cursor(cursor);
                    if interactive
                        && shows
                        && (picture.clicked() || picture.dragged())
                        && let Some(at) = picture.interact_pointer_pos()
                        && let Some(target) = press_target(at, page_rect, page)
                        && let Some((pivot, pan)) = pan_to_centre(&matrix, target, area)
                    {
                        session.view_gesture(pivot.x, pivot.y, pan.x, pan.y, 1.0, 0.0);
                        ctx.request_repaint();
                    }
                    self.controls(ctx, ui, session, &matrix, area, rect, interactive);
                }
                // the two sections are parted as the dock's own strip is, on the dock's own bottom
                // edge so the Layers' fill drawn after it covers none of the line
                ui.painter().line_segment(separator(rect), Stroke::new(1.0, argb(EDGE)));
            });
        self.menu(ctx, look, session, title, reference_on)
    }

    /// The title's menu under it: Fit Canvas, and Reference (ticked while it shows). Returns
    /// whether SubView was asked for.
    fn menu(
        &mut self,
        ctx: &egui::Context,
        look: &mut crate::look::Look,
        session: &CanvasSession,
        title: Rect,
        reference_on: bool,
    ) -> bool {
        let screen = ctx.content_rect();
        let size = vec2(MENU_WIDTH, menu::OPTION_HEIGHT * 2.0 + 14.0);
        let left = title.left().clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
        let rect = Rect::from_min_size(pos2(left, title.bottom() + CONTROL_GAP), size);
        let mut sub_view = false;
        let mut fit_asked = false;
        self.title_menu.show(
            ctx,
            "navigator title menu",
            rect,
            pos2(rect.center().x, rect.top()),
            menu::OVER_A_PANEL,
            &mut Vec::new(),
            |ui| {
                let entries = [("Fit Canvas", false), ("Reference", reference_on)];
                for (row, (label, checked)) in
                    menu::option_rows(rect.left(), rect.top() + 7.0, rect.width(), entries.len()).zip(entries)
                {
                    if menu::option(ui, look, row, label, checked, true, false) {
                        if label == "Fit Canvas" {
                            fit_asked = true;
                        } else {
                            sub_view = true;
                        }
                    }
                }
            },
        );
        if fit_asked {
            self.title_menu.close();
            session.settle_to_fit();
        }
        if sub_view {
            self.title_menu.close();
        }
        sub_view
    }

    /// Fit, − and + under the picture: the whole canvas, and a step out of or into the view about
    /// the middle of the area the canvas shows in. They are always painted, and only pressable
    /// while the section is interactive.
    #[allow(clippy::too_many_arguments)]
    fn controls(
        &self,
        ctx: &egui::Context,
        ui: &Ui,
        session: &CanvasSession,
        matrix: &[f32; 6],
        area: Rect,
        rect: Rect,
        interactive: bool,
    ) {
        let (fit_clicked, step_towards) = paint_controls(ui, controls_rect(rect), interactive);
        if fit_clicked {
            session.settle_to_fit();
        }
        if let Some(towards) = step_towards {
            let zoom = zoom_of(matrix);
            if zoom > 1e-6 {
                let step = zoom_step(zoom, towards) / zoom;
                if step.is_finite() && step > 0.0 && (step - 1.0).abs() > f32::EPSILON {
                    let middle = area.center();
                    session.view_gesture(middle.x, middle.y, 0.0, 0.0, step, 0.0);
                    ctx.request_repaint();
                }
            }
        }
    }

    /// Reads the canvas's picture when its edits changed, at most every [`REFRESH`].
    fn read_canvas(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        let edits = session.edits();
        let set = session.texture_set();
        let now = Instant::now();
        if self.read.due(edits, set, now) {
            match session.canvas_thumbnail(PICTURE_SIDE) {
                Some(picture) => {
                    let image = egui::ColorImage::from_rgba_premultiplied(
                        [picture.width as usize, picture.height as usize],
                        &picture.rgba,
                    );
                    self.picture = Some(ctx.load_texture("navigator canvas", image, egui::TextureOptions::LINEAR));
                    self.read.took(edits, set, now);
                }
                // a canvas still being made, or a read that failed: the attempt is kept, and the
                // next one is looked at again once the refresh is up rather than every frame
                None => {
                    self.read.missed(now);
                    ctx.request_repaint_after(REFRESH);
                }
            }
            return;
        }
        // a change held back by the throttle: read as soon as it is up
        if (self.read.edits != Some(edits) || self.read.set != Some(set))
            && let Some(wait) = self.read.wait(now)
        {
            ctx.request_repaint_after(wait);
        }
    }
}

/// Paints the Fit, − and + row inside `row`, returning whether Fit was clicked and whether a
/// zoom step was clicked (`Some(false)` for out, `Some(true)` for in).
fn paint_controls(ui: &Ui, row: Rect, interactive: bool) -> (bool, Option<bool>) {
    let width = (row.width() - STEP * 2.0 - CONTROL_GAP * 2.0).max(0.0);
    let fit_button = Rect::from_min_size(row.min, vec2(width, row.height()));
    let out = Rect::from_min_size(pos2(fit_button.right() + CONTROL_GAP, row.top()), vec2(STEP, row.height()));
    let into = Rect::from_min_size(pos2(out.right() + CONTROL_GAP, row.top()), vec2(STEP, row.height()));
    let fit_clicked =
        control(ui, fit_button, "Fit", CONTROL_FILL, argb(pc::TEXT), interactive).clicked() && interactive;
    let mut step_towards = None;
    for (button, label, towards) in [(out, "−", false), (into, "+", true)] {
        if control(ui, button, label, CONTROL_FILL, argb(pc::TEXT), interactive).clicked() && interactive {
            step_towards = Some(towards);
        }
    }
    (fit_clicked, step_towards)
}

/// A control at `rect`: its plate and centred label, always painted; the response says whether
/// it was pressed, which only an interactive section's controls can be.
fn control(ui: &Ui, rect: Rect, label: &str, fill: Themed, ink: Color32, interactive: bool) -> egui::Response {
    let response = ui.interact(rect, ui.scope_id().with(("navigator", label)), control_sense(interactive));
    let plate = if interactive && (response.hovered() || response.is_pointer_button_down_on()) {
        argb(CONTROL_HOVER)
    } else {
        argb(fill)
    };
    ui.painter().rect_filled(rect, CORNER, plate);
    text(ui.painter(), rect.center(), Align2::CENTER_CENTER, label, 13.0, Weight::Medium, ink);
    // no hand over a control that takes no press
    if interactive { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// What a control takes from the pointer: presses while the section is interactive, else only
/// hovering, so a control painted under a popover is no invisible hit target.
fn control_sense(interactive: bool) -> Sense {
    if interactive { Sense::click() } else { Sense::hover() }
}

/// Where the section's picture sits in a section `rect` shows.
fn preview_rect(rect: Rect) -> Rect {
    let top = rect.top() + panel::DOCK_HEADER_HEIGHT + UNDER_HEADER;
    let height = (rect.bottom() - ABOVE_BOTTOM - CONTROLS - CONTROL_GAP - top).max(0.0);
    Rect::from_min_size(pos2(rect.left() + INSET, top), vec2((rect.width() - INSET * 2.0).max(0.0), height))
}

/// Where the section's controls sit in a section `rect` shows.
fn controls_rect(rect: Rect) -> Rect {
    let bottom = rect.bottom() - ABOVE_BOTTOM;
    Rect::from_min_size(
        pos2(rect.left() + INSET, bottom - CONTROLS),
        vec2((rect.width() - INSET * 2.0).max(0.0), CONTROLS),
    )
}

/// The view's six numbers, the identity when there are not six (the canvas not shown yet).
fn affine(matrix: &[f32]) -> [f32; 6] {
    matrix.get(..6).and_then(|m| <[f32; 6]>::try_from(m).ok()).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
}

/// How many window pixels one canvas pixel takes (unaffected by the view's turn or mirroring);
/// 0 where the view has no area.
fn zoom_of(matrix: &[f32; 6]) -> f32 {
    let [a, b, ..] = *matrix;
    a.hypot(b)
}

/// The zoom after a press of − (out) or + (in), within what the view allows.
fn zoom_step(zoom: f32, towards: bool) -> f32 {
    let step = if towards { zoom * ZOOM_STEP } else { zoom / ZOOM_STEP };
    step.clamp(sumi_engine::view::MIN_ZOOM, sumi_engine::view::MAX_ZOOM)
}

/// A canvas point as the window pixels `matrix` shows it at.
fn to_screen(matrix: &[f32; 6], canvas: Pos2) -> Pos2 {
    let [a, b, c, d, e, f] = *matrix;
    pos2(a * canvas.x + c * canvas.y + e, b * canvas.x + d * canvas.y + f)
}

/// A window point as the canvas pixels under it (none where the view has no area).
fn to_canvas(matrix: &[f32; 6], at: Pos2) -> Option<Pos2> {
    let [a, b, c, d, e, f] = *matrix;
    let det = a * d - b * c;
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    let (x, y) = (at.x - e, at.y - f);
    Some(pos2((d * x - c * y) / det, (a * y - b * x) / det))
}

/// The window's area `area` (window pixels) as the four corners it covers on the canvas
/// (clockwise from its top left), or none where the view has no area.
fn visible_quad(matrix: &[f32; 6], area: Rect) -> Option<[Pos2; 4]> {
    let corners = [area.left_top(), area.right_top(), area.right_bottom(), area.left_bottom()];
    let mut quad = [Pos2::ZERO; 4];
    for (slot, corner) in quad.iter_mut().zip(corners) {
        *slot = to_canvas(matrix, corner)?;
    }
    Some(quad)
}

/// `points` kept to `rect` (window points): the part of the polygon the preview shows. What the
/// picture shows is clipped here, on the screen, and never on the page before it is placed.
fn clipped_to_rect(points: &[Pos2], rect: Rect) -> Vec<Pos2> {
    let kept = clip(points, |p| p.x, rect.left(), true);
    let kept = clip(&kept, |p| p.x, rect.right(), false);
    let kept = clip(&kept, |p| p.y, rect.top(), true);
    clip(&kept, |p| p.y, rect.bottom(), false)
}

/// `points` clipped to the half-plane `along(p)` above (else below) `bound`; the polygon stays in
/// its order, its edges' crossings added as it goes.
fn clip(points: &[Pos2], along: impl Fn(Pos2) -> f32, bound: f32, above: bool) -> Vec<Pos2> {
    let inside = |p: Pos2| {
        let v = along(p);
        if above { v >= bound } else { v <= bound }
    };
    let mut out = Vec::with_capacity(points.len() + 1);
    if points.is_empty() {
        return out;
    }
    let edges = points.iter().copied().zip(points.iter().copied().cycle().skip(1)).take(points.len());
    for (previous, current) in edges {
        let (previous_in, current_in) = (inside(previous), inside(current));
        if current_in != previous_in {
            let (from, to) = (along(previous), along(current));
            let t = if (to - from).abs() < f32::EPSILON { 0.0 } else { ((bound - from) / (to - from)).clamp(0.0, 1.0) };
            out.push(previous.lerp(current, t));
        }
        if current_in {
            out.push(current);
        }
    }
    out
}

/// A point kept on the page `[0, 0]` to `page`.
fn clamped_to_page(p: Pos2, page: Vec2) -> Pos2 {
    pos2(p.x.clamp(0.0, page.x), p.y.clamp(0.0, page.y))
}

/// Where a canvas `page` pixels big goes in `area`, its shape kept: as large as fits.
fn fit_page(page: Vec2, area: Rect) -> Rect {
    if page.x <= 0.0 || page.y <= 0.0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return Rect::from_center_size(area.center(), Vec2::ZERO);
    }
    let k = (area.width() / page.x).min(area.height() / page.y);
    Rect::from_center_size(area.center(), page * k)
}

/// A canvas point `page` pixels big as it shows on the picture's rectangle `at`.
fn on_page(p: Pos2, page: Vec2, at: Rect) -> Pos2 {
    if page.x <= 0.0 || page.y <= 0.0 {
        return at.min;
    }
    pos2(at.left() + p.x / page.x * at.width(), at.top() + p.y / page.y * at.height())
}

/// The canvas point a press at `at` (window points) moves to: only a press inside the page's
/// rectangle `page_rect` moves the view; a press on the letterbox around the page asks for
/// nothing.
fn press_target(at: Pos2, page_rect: Rect, page: Vec2) -> Option<Pos2> {
    page_rect.contains(at).then(|| clamped_to_page(at_on_page(at, page_rect, page), page))
}

/// A point on the picture's rectangle `at` as the canvas point it stands for.
fn at_on_page(p: Pos2, at: Rect, page: Vec2) -> Pos2 {
    if at.width() <= 0.0 || at.height() <= 0.0 || page.x <= 0.0 || page.y <= 0.0 {
        return Pos2::ZERO;
    }
    pos2((p.x - at.left()) / at.width() * page.x, (p.y - at.top()) / at.height() * page.y)
}

/// The pivot and pan that put the canvas point `target` in the middle of the area `area` the
/// canvas shows in, for [`CanvasSession::view_gesture`]; none where the view has no area.
fn pan_to_centre(matrix: &[f32; 6], target: Pos2, area: Rect) -> Option<(Pos2, Vec2)> {
    let pivot = to_screen(matrix, target);
    let pan = area.center() - pivot;
    (pivot.is_finite() && pan.is_finite()).then_some((pivot, pan))
}

/// The line parting the Navigator from the Layers, at the top of the dock's bottom edge: half a
/// point up, so the Layers' fill drawn after it covers none of the line.
fn separator(rect: Rect) -> [Pos2; 2] {
    let y = rect.bottom() - 0.5;
    [pos2(rect.left(), y), pos2(rect.right(), y)]
}

/// What the picture's plate says when there is no texture to draw on it: "No canvas yet" until
/// the canvas is made, and nothing once it is (the first read may not have come in), where the
/// plate alone is the neutral placeholder.
fn plate_words(canvas_made: bool) -> Option<&'static str> {
    (!canvas_made).then_some("No canvas yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Close enough: the clipping's crossings are interpolated.
    fn near(a: Pos2, b: Pos2) -> bool {
        (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01
    }

    #[test]
    fn the_dock_is_split_into_the_navigator_and_the_layers() {
        let dock = Rect::from_min_max(pos2(1140.0, pc::BAR_HEIGHT), pos2(1440.0, 900.0));
        let (section, layers) = Navigator::split(dock).expect("a 900 tall window has room for both");
        assert_eq!(section.width(), dock.width());
        assert_eq!(section.top(), dock.top());
        assert_eq!(section.height(), HEIGHT);
        assert_eq!(layers.left(), dock.left());
        assert_eq!(layers.top(), section.bottom());
        assert_eq!(layers.bottom(), dock.bottom());
        assert!(layers.height() >= 300.0, "the Layers keep their rows: {}", layers.height());
        // just under the least: the Layers keep it all
        let short = Rect::from_min_size(pos2(1140.0, pc::BAR_HEIGHT), vec2(300.0, LEAST_DOCK - 1.0));
        assert_eq!(Navigator::split(short), None);
        // just over it: the Navigator shows, and the Layers keep their header, the current
        // layer's strip and gap, their footer and three rows (52 + 56 + 6 + 42 + 3 x 55 points)
        let least = Rect::from_min_size(pos2(1140.0, pc::BAR_HEIGHT), vec2(300.0, LEAST_DOCK));
        let (_, layers) = Navigator::split(least).expect("the least dock has room for both");
        assert_eq!(layers.height(), LEAST_LAYERS);
        assert_eq!(LEAST_LAYERS, 52.0 + 56.0 + 6.0 + 42.0 + 3.0 * 55.0);
        // the contents fit the section they are measured in
        let preview = preview_rect(section);
        let controls = controls_rect(section);
        assert_eq!(preview.top(), section.top() + panel::DOCK_HEADER_HEIGHT + UNDER_HEADER);
        assert_eq!(preview.bottom() + CONTROL_GAP, controls.top());
        assert_eq!(controls.bottom(), section.bottom() - ABOVE_BOTTOM);
        assert!(preview.height() > 0.0 && controls.height() == CONTROLS);
        // the sections are parted on the dock's bottom edge, above the Layers' fill
        let [left, right] = separator(section);
        assert_eq!(left.y, section.bottom() - 0.5);
        assert_eq!(right.y, section.bottom() - 0.5);
        assert_eq!((left.x, right.x), (section.left(), section.right()));
    }

    #[test]
    fn the_navigator_keeps_away_from_a_three_d_scene() {
        let dock = Rect::from_min_max(pos2(1140.0, pc::BAR_HEIGHT), pos2(1440.0, 900.0));
        // a page: the dock is split as it always is
        assert_eq!(Navigator::shown(dock, false), Navigator::split(dock));
        // a 3D scene has no page to show at a glance: the Layers keep the whole dock
        assert_eq!(Navigator::shown(dock, true), None);
        assert_eq!(Navigator::shown(dock.with_max_y(dock.top() + LEAST_DOCK - 1.0), false), None);
    }

    #[test]
    fn the_window_is_a_rectangle_on_the_canvas() {
        // a view at half size and moved: its top left on (100, 50) in window pixels
        let matrix = affine(&[0.5, 0.0, 0.0, 0.5, 100.0, 50.0]);
        // the canvas shows in an 800 x 600 area that starts 10, 20 points into the window
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 600.0));
        let quad = visible_quad(&matrix, area).expect("a view with an area");
        assert!(near(quad[0], pos2(-180.0, -60.0)));
        assert!(near(quad[1], pos2(1420.0, -60.0)));
        assert!(near(quad[2], pos2(1420.0, 1140.0)));
        assert!(near(quad[3], pos2(-180.0, 1140.0)));
        // no area at all: nothing to show
        assert_eq!(visible_quad(&[0.0, 0.0, 0.0, 0.0, 10.0, 10.0], area), None);
        // what the picture shows is clipped on the screen, to the preview: a polygon clear of it
        // keeps nothing, and one crossing it keeps its own order, from where its first edge meets
        let preview = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let away = [pos2(200.0, 200.0), pos2(300.0, 200.0), pos2(300.0, 300.0), pos2(200.0, 300.0)];
        assert!(clipped_to_rect(&away, preview).is_empty());
        let crossing = [pos2(-10.0, 20.0), pos2(50.0, 20.0), pos2(50.0, 120.0), pos2(-10.0, 120.0)];
        let kept = clipped_to_rect(&crossing, preview);
        let wanted = [pos2(0.0, 20.0), pos2(50.0, 20.0), pos2(50.0, 100.0), pos2(0.0, 100.0)];
        assert_eq!(kept.len(), wanted.len(), "{kept:?}");
        let start = kept.iter().position(|got| near(*got, wanted[0])).unwrap_or_else(|| panic!("{kept:?}"));
        for (i, want) in wanted.iter().enumerate() {
            let got = kept[(start + i) % kept.len()];
            assert!(near(got, *want), "{got:?} is not {want:?} in {kept:?}");
        }
    }

    #[test]
    fn fitting_the_page_puts_its_middle_in_the_middle_of_the_area() {
        // a 1440 x 856 window, less the 300-point Layers dock and the 26-point status bar
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(1140.0, 830.0));
        let view = sumi_engine::view::View::fit_inside((2000, 1000), (1440, 856), [0.0, 0.0, 300.0, 26.0]);
        let m = view.to_screen();
        let matrix = affine(&[m.a, m.b, m.c, m.d, m.e, m.f]);
        // the fitted page's middle already sits in the middle: a press there moves nothing
        let (pivot, pan) = pan_to_centre(&matrix, pos2(1000.0, 500.0), area).expect("a view with an area");
        assert!(pan.length() < 0.01, "the fit is already centred: {pan:?}");
        assert!((pivot - area.center()).length() < 0.01, "{pivot:?} is not {:?}", area.center());
        // the whole area is on the page at fit: nothing shows past the page's sides, and the
        // window stands taller than the page, so its rectangle reaches above and below it
        let quad = visible_quad(&matrix, area).expect("a view with an area");
        let top = quad.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let bottom = quad.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        assert!(top < 0.0 && bottom > 1000.0, "the whole page is shown: {quad:?}");
        assert!(quad[0].x <= 0.01 && quad[1].x >= 1999.99, "the page spans the area's width: {quad:?}");
    }

    #[test]
    fn a_fitted_page_shows_the_window_past_the_page_edges() {
        let page = vec2(2000.0, 1000.0);
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(1140.0, 830.0));
        let view = sumi_engine::view::View::fit_inside((2000, 1000), (1440, 856), [0.0, 0.0, 300.0, 26.0]);
        let m = view.to_screen();
        let matrix = affine(&[m.a, m.b, m.c, m.d, m.e, m.f]);
        let quad = visible_quad(&matrix, area).expect("a view with an area");
        // the rectangle is not the page's edge: it reaches past the page, over its letterbox
        let preview = Rect::from_min_size(pos2(300.0, 20.0), vec2(280.0, 200.0));
        let page_rect = fit_page(page, preview);
        let points: Vec<Pos2> = quad.iter().map(|&p| on_page(p, page, page_rect)).collect();
        assert!(points.iter().any(|p| p.y < page_rect.top() - 1.0), "past the page's top: {points:?}");
        assert!(points.iter().any(|p| p.y > page_rect.bottom() + 1.0), "past the page's bottom: {points:?}");
        // clipped only to the preview, the rectangle is kept whole rather than cut to the page
        let shown = clipped_to_rect(&points, preview);
        assert_eq!(shown.len(), 4, "the preview holds the whole window's rectangle: {shown:?}");
        assert!(shown.iter().any(|p| p.y < page_rect.top() - 1.0 && p.y >= preview.top() - 0.01), "{shown:?}");
    }

    #[test]
    fn a_turned_view_shows_the_quad_it_covers() {
        // a quarter turn clockwise: its down axis points left, its top left on (300, 0)
        let matrix = affine(&[0.0, 1.0, -1.0, 0.0, 300.0, 0.0]);
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        let quad = visible_quad(&matrix, area).expect("a view with an area");
        assert!(near(quad[0], pos2(0.0, 300.0)));
        assert!(near(quad[1], pos2(0.0, 100.0)));
        assert!(near(quad[2], pos2(100.0, 100.0)));
        assert!(near(quad[3], pos2(100.0, 300.0)));
        assert!((zoom_of(&matrix) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_control_is_painted_and_only_pressable_while_the_section_is_interactive() {
        // under a popover the controls are painted as ever but take no press, so no invisible
        // hit target sits where they are
        assert_eq!(control_sense(true), Sense::click());
        assert_eq!(control_sense(false), Sense::hover());
    }

    #[test]
    fn the_plate_says_no_canvas_only_before_the_canvas_is_made() {
        assert_eq!(plate_words(false), Some("No canvas yet"));
        // a canvas made whose first texture has not uploaded yet says nothing on its plate
        assert_eq!(plate_words(true), None);
    }

    #[test]
    fn pressing_the_picture_puts_that_canvas_point_in_the_middle_within_the_page() {
        let matrix = affine(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let (pivot, pan) = pan_to_centre(&matrix, pos2(100.0, 100.0), area).expect("a view with an area");
        assert_eq!(pivot, pos2(100.0, 100.0));
        assert_eq!(pan, vec2(300.0, 200.0));
        // the area's own origin is where the middle is read from
        let inset = Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 600.0));
        let (_, pan) = pan_to_centre(&matrix, pos2(100.0, 100.0), inset).expect("a view with an area");
        assert_eq!(pan, vec2(310.0, 220.0));
        // a point past the page is kept on it, so the canvas is never lost off the window
        let page = vec2(1000.0, 800.0);
        let target = clamped_to_page(pos2(-50.0, 700.0), page);
        assert_eq!(target, pos2(0.0, 700.0));
        let (pivot, pan) = pan_to_centre(&matrix, target, area).expect("a view with an area");
        assert_eq!((pivot, pan), (pos2(0.0, 700.0), vec2(400.0, -400.0)));
        // the picture's rectangle stands for the page, both ways
        let at = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        assert!(near(on_page(pos2(100.0, 50.0), page, at), pos2(30.0, 26.25)));
        assert!(near(at_on_page(pos2(30.0, 26.25), at, page), pos2(100.0, 50.0)));
    }

    #[test]
    fn only_a_press_on_the_page_moves_the_view() {
        let page = vec2(1000.0, 800.0);
        // a page shown 200 x 160 in a preview letterboxed above and below it
        let page_rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(200.0, 160.0));
        assert_eq!(press_target(pos2(20.0, 40.0), page_rect, page), Some(pos2(0.0, 0.0)));
        assert_eq!(press_target(page_rect.center(), page_rect, page), Some(pos2(500.0, 400.0)));
        // a press on the page's edge stays on the page
        assert_eq!(press_target(pos2(219.0, 200.0), page_rect, page), Some(pos2(995.0, 800.0)));
        // the letterbox around the page is no press at all: it moves nothing
        assert_eq!(press_target(pos2(page_rect.center().x, 20.0), page_rect, page), None);
        assert_eq!(press_target(pos2(page_rect.center().x, 220.0), page_rect, page), None);
        assert_eq!(press_target(pos2(5.0, page_rect.center().y), page_rect, page), None);
    }

    #[test]
    fn zooming_steps_stop_at_what_the_view_allows() {
        assert_eq!(zoom_step(1.0, true), ZOOM_STEP);
        assert_eq!(zoom_step(1.0, false), 1.0 / ZOOM_STEP);
        assert_eq!(zoom_step(sumi_engine::view::MAX_ZOOM, true), sumi_engine::view::MAX_ZOOM);
        assert_eq!(zoom_step(sumi_engine::view::MIN_ZOOM, false), sumi_engine::view::MIN_ZOOM);
        // the picture keeps the canvas's shape in the space it is given
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(272.0, 128.0));
        let picture = fit_page(vec2(200.0, 100.0), area);
        assert!((picture.size() - vec2(256.0, 128.0)).length() < 0.01, "{:?}", picture.size());
        assert!((picture.center() - area.center()).length() < 0.01);
        assert_eq!(fit_page(Vec2::ZERO, area).size(), Vec2::ZERO);
    }

    #[test]
    fn the_canvas_is_read_only_when_it_changes_and_at_most_every_three_hundred_milliseconds() {
        let start = Instant::now();
        let mut read = Refreshed::default();
        assert!(read.due(1, 0, start), "the first read has nothing to wait for");
        read.took(1, 0, start);
        assert!(!read.due(1, 0, start + Duration::from_secs(10)), "no change, no read");
        let soon = start + Duration::from_millis(120);
        assert!(!read.due(2, 0, soon), "a change waits out the 300 ms");
        assert_eq!(read.wait(soon), Some(Duration::from_millis(180)));
        let later = start + REFRESH;
        assert!(read.due(2, 0, later), "the change is read once the 300 ms are up");
        assert_eq!(read.wait(later), None);
        read.took(2, 0, later);
        assert!(!read.due(2, 0, later + REFRESH), "a read picture stays while nothing changes");
        // another texture set of the same edits is another picture: read again at once
        assert!(read.due(2, 1, later + REFRESH), "a model's other texture set is read anew");
        read.took(2, 1, later + REFRESH);
        assert!(!read.due(2, 1, later + REFRESH + Duration::from_secs(10)));
        read.forget();
        assert!(read.due(2, 0, later), "another artwork is read at once");
        // a read that could not be made is tried again after the refresh, not every frame
        let mut read = Refreshed::default();
        read.missed(start);
        assert!(!read.due(1, 0, start + Duration::from_millis(50)), "a failed read waits out the 300 ms");
        assert_eq!(read.wait(start + Duration::from_millis(50)), Some(Duration::from_millis(250)));
        assert!(read.due(1, 0, start + REFRESH), "the canvas is tried again once the 300 ms are up");
        // read then, it stays until it changes
        read.took(1, 0, start + REFRESH);
        assert!(!read.due(1, 0, start + REFRESH + Duration::from_millis(50)));
    }

    #[test]
    fn all_three_controls_stay_painted_while_non_interactive_and_ignore_clicks() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::look::fonts(Vec::new()));
        let row = Rect::from_min_size(pos2(20.0, 180.0), vec2(272.0, CONTROLS));
        let fit_width = (row.width() - STEP * 2.0 - CONTROL_GAP * 2.0).max(0.0);
        let fit_rect = Rect::from_min_size(row.min, vec2(fit_width, row.height()));
        let out_rect = Rect::from_min_size(pos2(fit_rect.right() + CONTROL_GAP, row.top()), vec2(STEP, row.height()));
        let into_rect = Rect::from_min_size(pos2(out_rect.right() + CONTROL_GAP, row.top()), vec2(STEP, row.height()));

        let run = |time: f64, events: Vec<egui::Event>, interactive: bool| {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
                time: Some(time),
                events,
                ..egui::RawInput::default()
            };
            let mut result = (false, None);
            let mut output = ctx.run_ui(raw, |ui| {
                result = paint_controls(ui, row, interactive);
            });
            output.textures_delta.clear();
            (result, output)
        };

        // Click Fit while non-interactive (simulating an open popover over the dock).
        let fit_center = fit_rect.center();
        let _ = run(0.0, vec![egui::Event::PointerMoved(fit_center)], false);
        let _ = run(
            0.016,
            vec![egui::Event::PointerButton {
                pos: fit_center,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        let (non_interactive_result, non_interactive_out) = run(
            0.032,
            vec![egui::Event::PointerButton {
                pos: fit_center,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert_eq!(non_interactive_result, (false, None), "clicks must be ignored when non-interactive");

        fn collect(shape: &egui::Shape, rects: &mut Vec<Rect>, labels: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(items) => items.iter().for_each(|s| collect(s, rects, labels)),
                egui::Shape::Rect(r) if r.fill != Color32::TRANSPARENT => rects.push(r.rect),
                egui::Shape::Text(t) => labels.push(t.galley.text().to_owned()),
                _ => {}
            }
        }

        let (mut rects, mut labels) = (Vec::new(), Vec::new());
        for clipped in &non_interactive_out.shapes {
            collect(&clipped.shape, &mut rects, &mut labels);
        }
        for (name, expected_rect) in [("Fit", fit_rect), ("−", out_rect), ("+", into_rect)] {
            assert!(
                rects.iter().any(|r| (*r).center().distance(expected_rect.center()) < 1.0
                    && (r.width() - expected_rect.width()).abs() < 1.0),
                "missing painted button plate for {name}; painted rects: {rects:?}"
            );
            assert!(labels.iter().any(|l| l == name), "missing painted label {name}; labels: {labels:?}");
        }

        // Verify clicking Fit still works once interactive again.
        let _ = run(0.050, vec![egui::Event::PointerMoved(fit_center)], true);
        let _ = run(
            0.066,
            vec![egui::Event::PointerButton {
                pos: fit_center,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        );
        let (interactive_result, _) = run(
            0.082,
            vec![egui::Event::PointerButton {
                pos: fit_center,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        );
        assert!(interactive_result.0, "Fit click must register when interactive");
    }
}
