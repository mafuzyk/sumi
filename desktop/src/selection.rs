//! The selection tool, as the Android app's (ui/SelectionUi.kt): its floating toolbar (Automatic,
//! Freehand, Rectangle and Ellipse; Add, Remove, Invert, Copy & Paste, Feather, Save & Load, Color
//! Fill, Clear) and popovers, and what shows while a selection is made (the rectangle or ellipse
//! dragged out and the freehand path, dashed; the crosshair and the bar while a colour
//! selection's threshold is slid). The shared touches make the selections.

use std::collections::HashMap;

use egui::{
    Align2, Color32, Id, LayerId, Order, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureOptions, pos2, vec2,
};
use sumi_core::{CanvasSession, LassoAction, SelectKind, SelectTouch, SelectionDisplay, SelectionJoin};

use crate::icons;
use crate::look::{Look, Themed, Weight, argb, mix, text};
use crate::transform::{
    Button, DASH, DASH_GAP, MENU_ARROW, MENU_GROUP, MENU_LINE, MENU_TEXT, MENU_WIDTH, Tapped, button_tip,
    floating_toolbar, menu_slider, menu_title, popover_frame,
};

/// The selection toolbar's buttons with room, and where the window is narrow.
const BUTTON_ROOMY: f32 = 83.0;
// Nine buttons (with a model's X-Ray) still fit a 640-wide window without overlapping.
const BUTTON_NARROW: f32 = 64.0;
const FEATHER_WIDTH: f32 = 250.0;
const EDGE_PIXELS_DEFAULT: i32 = 4;
const EDGE_PIXELS_MAX: i32 = 250;
const START_DOT: u32 = 0xFFC7C7C7;
const MESSAGE_BAR: Themed = Themed(0xFF606060, 0xFFD0D0D0);
const MESSAGE_TEXT: Themed = Themed(0xFFB5B5B5, 0xFF3B3B3B);
const KINDS: [SelectKind; 4] =
    [SelectKind::Automatic, SelectKind::Freehand, SelectKind::Rectangle, SelectKind::Ellipse];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Popup {
    None,
    Feather,
    Saved,
    Fill,
}

pub struct Selection {
    /// The tool is open, how it selects, and whether new selections take away.
    pub on: bool,
    kind: SelectKind,
    /// Fill or erase what a freehand path encloses when let go; None is ordinary selection.
    lasso: Option<LassoAction>,
    removes: bool,
    /// The colour selection's threshold, kept from one to the next; while slid, where and how far.
    threshold: f32,
    sliding: Option<(Pos2, f32)>,
    /// A rectangle or ellipse being dragged out (window pixels).
    drag: Option<(Pos2, Pos2)>,
    /// The polyline being placed (document points), and whether its segment is armed.
    polyline: Vec<Pos2>,
    polyline_armed: bool,
    /// An egui pointer click this frame belongs to a multi-finger gesture, not a Polyline vertex.
    pub pointer_suppressed: bool,
    /// Whether there is a selection; Color Fill on; the feather's amount; the popover open; the
    /// selections kept and their pictures.
    has: bool,
    /// On a plugin's layer, the box round the strokes the selection takes (four corners, x then y,
    /// canvas pixels); empty otherwise. And the artwork's revision and whether anything was selected
    /// when it was read.
    taken: Vec<f32>,
    taken_at: Option<(u64, bool)>,
    color_fill: bool,
    feather: f32,
    edge_pixels: i32,
    popup: Popup,
    saved: u32,
    pictures: HashMap<u32, TextureHandle>,
}

impl Selection {
    /// Label on the Fill toolbar button: reflects the active raster lasso or Color Fill mode.
    fn fill_button_label(&self) -> &'static str {
        match self.lasso {
            Some(LassoAction::Fill) => "Lasso Fill",
            Some(LassoAction::Erase) => "Lasso Erase",
            None if self.color_fill => "Color Fill",
            None => "Fill",
        }
    }

    /// The icon of how it selects now, for the pointer over the canvas.
    pub fn kind_icon(&self) -> &'static icons::Icon {
        if self.polyline_armed {
            return &icons::SHAPE_POLYLINE;
        }
        if let Some(action) = self.lasso {
            return match action {
                LassoAction::Fill => &icons::COLOR_FILL,
                LassoAction::Erase => &icons::ERASE,
            };
        }
        match self.kind {
            SelectKind::Automatic => &icons::AUTOMATIC,
            SelectKind::Freehand => &icons::FREEHAND,
            SelectKind::Rectangle => &icons::RECTANGLE_SHAPE,
            SelectKind::Ellipse => &icons::ELLIPSE_SHAPE,
        }
    }

    /// The Polyline segment is armed, so a canvas press places a vertex instead of painting.
    pub fn polyline_armed(&self) -> bool {
        self.polyline_armed
    }

    fn add_polyline_point(&mut self, point: Pos2) {
        let near = |last: &Pos2| (last.x - point.x).abs() + (last.y - point.y).abs() < 0.5;
        if !self.polyline.last().is_some_and(near) {
            self.polyline.push(point);
        }
    }

    fn cancel_polyline(&mut self) {
        self.polyline.clear();
        self.polyline_armed = false;
    }

    fn finish_polyline(&mut self, session: &CanvasSession) {
        if self.polyline.len() < 3 {
            return;
        }
        let [a, b, c, d, e, f] = view_affine(session);
        let window: Vec<f32> =
            self.polyline.iter().flat_map(|p| [a * p.x + c * p.y + e, b * p.x + d * p.y + f]).collect();
        let join =
            if self.removes && self.kind != SelectKind::Automatic { SelectionJoin::Remove } else { SelectionJoin::Add };
        self.cancel_polyline();
        session.select_polygon(window, join);
        self.changed(session);
        self.touches(session);
    }

    fn polyline_input(&mut self, ctx: &egui::Context, session: &CanvasSession, bar_top: f32) {
        let ppp = ctx.pixels_per_point();
        let (double, clicked, enter, backspace) = ctx.input(|i| {
            (
                i.pointer.button_double_clicked(egui::PointerButton::Primary),
                i.pointer.primary_clicked(),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Backspace),
            )
        });
        if enter {
            self.finish_polyline(session);
            ctx.request_repaint();
            return;
        }
        if backspace {
            self.polyline.pop();
            ctx.request_repaint();
        }
        if double || clicked {
            if self.pointer_suppressed {
                return;
            }
            let Some(at) = ctx.pointer_latest_pos() else { return };
            if at.y < bar_top
                || !ctx.content_rect().contains(at)
                || ctx.layer_id_at(at).is_some_and(|l| l.order != Order::Background)
            {
                return;
            }
            if double {
                self.finish_polyline(session);
                ctx.request_repaint();
                return;
            }
            let Some(point) = polyline_document(session, at, ppp) else { return };
            if self.polyline.len() >= 3
                && self.polyline.first().is_some_and(|first| polyline_screen(session, *first, ppp).distance(at) <= 6.0)
            {
                self.finish_polyline(session);
            } else {
                self.add_polyline_point(point);
            }
            ctx.request_repaint();
        }
    }
}

impl Default for Selection {
    fn default() -> Selection {
        Selection {
            on: false,
            kind: SelectKind::Freehand,
            lasso: None,
            removes: false,
            threshold: 0.5,
            sliding: None,
            drag: None,
            polyline: Vec::new(),
            polyline_armed: false,
            pointer_suppressed: false,
            has: false,
            taken: Vec::new(),
            taken_at: None,
            color_fill: false,
            feather: 0.0,
            edge_pixels: EDGE_PIXELS_DEFAULT,
            popup: Popup::None,
            saved: 0,
            pictures: HashMap::new(),
        }
    }
}

impl Selection {
    /// The tool opens (each time it opens, shapes add to the selection).
    pub fn open(&mut self, session: &CanvasSession) {
        self.removes = false;
        self.lasso = None;
        self.cancel_polyline();
        self.on = true;
        self.has = session.has_selection();
        self.touches(session);
        self.look(session);
    }

    /// The tool closes; a freehand path drawn is closed into a selection.
    pub fn close(&mut self, session: &CanvasSession) {
        self.cancel_polyline();
        if !self.on {
            return;
        }
        // An in-flight lasso is dropped, never closed into a selection: leaving the mode must not
        // turn a fill into a selection.
        if self.lasso.is_some() {
            session.set_touch_lasso(None);
        } else {
            session.close_freehand();
        }
        session.set_touch_selecting(None);
        self.lasso = None;
        self.on = false;
        self.set_popup(session, Popup::None);
        self.drag = None;
        self.sliding = None;
        self.look(session);
    }

    /// The touches select as the tool is set; while the tool is closed they paint as they would,
    /// as a Deselect clears what is selected without arming a selection.
    fn touches(&self, session: &CanvasSession) {
        if !self.on || self.polyline_armed {
            session.set_touch_selecting(None);
            session.set_touch_lasso(None);
            return;
        }
        let join =
            if self.removes && self.kind != SelectKind::Automatic { SelectionJoin::Remove } else { SelectionJoin::Add };
        session.set_touch_selecting(Some(SelectTouch { kind: self.kind, join, threshold: self.threshold }));
        session.set_touch_lasso(self.lasso);
    }

    /// The selection shows tinted while selecting by colour, else striped.
    fn look(&self, session: &CanvasSession) {
        let tint = self.on && self.kind == SelectKind::Automatic;
        session.set_selection_display(if tint { SelectionDisplay::Tint } else { SelectionDisplay::Stripes });
    }

    /// Nothing selected any more (a freehand path being drawn closed first), as Deselect does.
    pub fn clear(&mut self, session: &CanvasSession) {
        self.cancel_polyline();
        if self.lasso.is_some() {
            // a path in flight is dropped, not closed into a selection; the mode stays ready
            session.set_touch_lasso(None);
            session.set_touch_lasso(self.lasso);
        } else {
            session.close_freehand();
        }
        session.select_none();
        self.has = false;
        self.look(session);
        self.touches(session);
    }

    /// A rectangle or ellipse is being dragged out.
    pub fn dragged(&mut self, session: &CanvasSession, a: Pos2, b: Pos2) {
        self.drag = Some((a, b));
        self.set_popup(session, Popup::None);
    }

    /// Another popover, or none: the feather popover closing keeps its feathering as one undo step.
    fn set_popup(&mut self, session: &CanvasSession, p: Popup) {
        if self.popup == Popup::Feather && p != Popup::Feather {
            session.select_feather_done();
        }
        self.popup = p;
    }

    /// A colour selection's threshold slid (kept when `done`).
    pub fn slid(&mut self, session: &CanvasSession, at: Pos2, threshold: f32, done: bool) {
        if done {
            self.sliding = None;
            self.threshold = threshold;
            self.touches(session);
        } else {
            self.sliding = Some((at, threshold));
        }
    }

    /// A selection was made: Color Fill fills it.
    pub fn changed(&mut self, session: &CanvasSession) {
        self.drag = None;
        self.has = session.has_selection();
        if self.color_fill && self.has && self.lasso.is_none() {
            session.fill_selection();
        }
        self.look(session);
    }

    /// Draws what shows while selecting and the toolbar with its popover (on a `model`, with
    /// X-Ray); does what was asked. Returns X-Ray as it is when it was turned on or off.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        look: &mut Look,
        session: &CanvasSession,
        bar_top: f32,
        model: bool,
    ) -> Option<bool> {
        if !self.on {
            self.cancel_polyline();
            self.taken.clear();
            self.taken_at = None;
            return None;
        }
        // A model's textures take no raster lasso: leave the mode rather than offer a no-op.
        if model && self.lasso.is_some() {
            self.lasso = None;
            session.set_touch_lasso(None);
        }
        if model && self.polyline_armed {
            self.cancel_polyline();
            self.touches(session);
        }
        // the strokes the selection takes, read again as the artwork changes
        let at = (session.info().revision, self.has);
        if self.taken_at != Some(at) {
            self.taken = if self.has { session.taken_plugin_items_box() } else { Vec::new() };
            self.taken_at = Some(at);
        }
        let mut x_ray_set = None;
        self.overlay(ctx, session, bar_top);
        let drawing = !session.freehand_path().is_empty();
        let (has, joins) = (self.has, self.kind != SelectKind::Automatic);
        let mut segments: Vec<(&icons::Icon, &str, bool)> = KINDS
            .iter()
            .map(|&k| {
                let (icon, label) = match k {
                    SelectKind::Automatic => (&icons::AUTOMATIC, "Automatic"),
                    SelectKind::Freehand => (&icons::FREEHAND, "Freehand"),
                    SelectKind::Rectangle => (&icons::RECTANGLE_SHAPE, "Rectangle"),
                    SelectKind::Ellipse => (&icons::ELLIPSE_SHAPE, "Ellipse"),
                };
                (icon, label, self.kind == k && !self.polyline_armed)
            })
            .collect();
        if !model {
            segments.push((&icons::SHAPE_POLYLINE, "Polyline", self.polyline_armed));
        }
        let mut buttons = vec![
            Button::new(&icons::SELECT_ADD, "Add", 44.0).active(!joins || !self.removes),
            Button::new(&icons::SELECT_REMOVE, "Remove", 44.0).active(joins && self.removes).enabled(joins),
            Button::new(&icons::INVERT, "Invert", 44.0).enabled(has),
            Button::new(&icons::SELECT_COPY_PASTE, "Copy & Paste", 44.0).enabled(has || drawing),
            Button::new(&icons::FEATHER, "Edges", 44.0).active(self.popup == Popup::Feather).enabled(has || drawing),
            Button::new(&icons::SAVE_LOAD, "Save & Load", 44.0)
                .active(self.popup == Popup::Saved)
                .enabled(has || drawing),
            Button::new(&icons::COLOR_FILL, self.fill_button_label(), 37.0)
                .active(self.color_fill || self.lasso.is_some()),
        ];
        // on a model: a selection reaches every surface under it
        let xray_at = model.then(|| {
            let at = buttons.len();
            buttons.push(Button::new(&icons::X_RAY, "X-Ray", 44.0).active(session.selection_x_ray()));
            at
        });
        let clear_at = buttons.len();
        buttons.push(Button::new(&icons::CLEAR_SELECTION, "Clear", 44.0).enabled(has || drawing));
        let count = buttons.len();
        let tapped =
            floating_toolbar(ctx, look, "selection toolbar", (BUTTON_ROOMY, BUTTON_NARROW), &segments, &buttons, 41.0);
        match tapped {
            Some(Tapped::Background) => self.set_popup(session, Popup::None),
            Some(Tapped::Segment(k)) => {
                let was_polyline = self.polyline_armed;
                self.cancel_polyline();
                if self.lasso.is_some() {
                    self.lasso = None;
                    session.set_touch_lasso(None);
                } else {
                    session.close_freehand();
                }
                if k == KINDS.len() {
                    self.polyline_armed = !was_polyline;
                    self.touches(session);
                } else {
                    self.kind = KINDS.get(k).copied().unwrap_or(SelectKind::Freehand);
                    self.touches(session);
                    self.changed(session);
                }
            }
            Some(Tapped::Button(0)) => {
                self.removes = false;
                self.touches(session);
            }
            Some(Tapped::Button(1)) => {
                self.removes = true;
                self.touches(session);
            }
            Some(Tapped::Button(2)) => {
                session.select_invert();
                self.has = session.has_selection();
                self.look(session);
            }
            Some(Tapped::Button(3)) => {
                session.close_freehand();
                if session.copy_selection_to_layer().is_some() {
                    session.select_none();
                }
                self.has = session.has_selection();
                self.look(session);
            }
            Some(Tapped::Button(4)) => self.open_popup(ctx, session, Popup::Feather),
            Some(Tapped::Button(5)) => self.open_popup(ctx, session, Popup::Saved),
            Some(Tapped::Button(6)) => self.open_popup(ctx, session, Popup::Fill),
            Some(Tapped::Button(i)) if Some(i) == xray_at => {
                let on = !session.selection_x_ray();
                session.set_selection_x_ray(on);
                x_ray_set = Some(on);
            }
            Some(Tapped::Button(i)) if i == clear_at => {
                self.clear(session);
            }
            Some(Tapped::Button(_)) => {}
            None => {}
        }
        if self.polyline_armed && self.popup == Popup::None {
            self.polyline_input(ctx, session, bar_top);
        }
        // Escape closes the popover; one with no popover open disarms an armed lasso: the pending
        // enclosure is dropped and its touch cancelled, so it cannot close into a stray selection.
        // An unarmed Escape is left to the interface, which may settle the canvas upright.
        if self.popup != Popup::None && crate::ui::escape(ctx) {
            self.set_popup(session, Popup::None);
        } else if self.lasso.is_some() && crate::ui::escape(ctx) {
            self.lasso = None;
            // The touch is still down: cancel it so its later samples and lift cannot refill the
            // ordinary freehand path. `touch_cancel` clears a pending lasso path on the way.
            let _ = session.touch_cancel();
            session.set_touch_lasso(None);
            // The toolbar was already painted with the armed label this frame.
            ctx.request_repaint();
        } else if self.polyline_armed && crate::ui::escape(ctx) {
            self.cancel_polyline();
            self.touches(session);
            ctx.request_repaint();
        }
        self.popover(ctx, look, session, count, model);
        x_ray_set
    }

    fn open_popup(&mut self, ctx: &egui::Context, session: &CanvasSession, p: Popup) {
        self.set_popup(session, if self.popup == p { Popup::None } else { p });
        if self.popup == Popup::Feather {
            self.feather = 0.0;
            self.edge_pixels = EDGE_PIXELS_DEFAULT;
        }
        self.saved = session.saved_selection_count();
        if self.popup == Popup::Saved {
            self.pictures.clear();
            for i in 0..self.saved {
                if let Some(t) = session.saved_selection_thumbnail(i, 128, 92) {
                    let image =
                        egui::ColorImage::from_rgba_premultiplied([t.width as usize, t.height as usize], &t.rgba);
                    self.pictures
                        .insert(i, ctx.load_texture(format!("saved selection {i}"), image, TextureOptions::LINEAR));
                }
            }
        }
    }

    /// The popover over the toolbar: Feather's amount, or the selections kept.
    fn popover(&mut self, ctx: &egui::Context, look: &mut Look, session: &CanvasSession, buttons: usize, model: bool) {
        if self.popup == Popup::None {
            return;
        }
        if self.popup == Popup::Fill {
            // One compact menu keeps the toolbar at its old width: Colour Fill stays, and the
            // lasso modes have their full names. A model's textures take no raster lasso.
            let index = 6;
            let rows = if model { 1 } else { 3 };
            let width = 200.0;
            let height = 40.0 + rows as f32 * 44.0 + 16.0 + 20.0;
            let (tip_x, tip_y) = button_tip(ctx, (BUTTON_ROOMY, BUTTON_NARROW), index, buttons);
            let screen = ctx.content_rect();
            let left = (tip_x - width / 2.0).clamp(screen.left(), screen.right() - width);
            let rect = Rect::from_min_size(pos2(left, tip_y - MENU_ARROW - height), vec2(width, height));
            egui::Area::new(Id::unique("selection fill menu")).fixed_pos(rect.min).order(Order::Tooltip).show(
                ctx,
                |ui| {
                    popover_frame(ui, rect, tip_x, tip_y);
                    let inner = rect.shrink(10.0);
                    menu_title(ui, inner, "Fill");
                    let labels = fill_labels(model);
                    for (i, label) in labels.iter().enumerate() {
                        let row = Rect::from_min_size(
                            pos2(inner.left(), inner.top() + 44.0 + 44.0 * i as f32),
                            vec2(inner.width(), 40.0),
                        );
                        let active = match *label {
                            "Color Fill" => self.color_fill,
                            "Lasso Fill" => self.lasso == Some(LassoAction::Fill),
                            _ => self.lasso == Some(LassoAction::Erase),
                        };
                        if active {
                            ui.painter().rect_filled(row, 10, argb(MENU_GROUP));
                        }
                        let icon = match *label {
                            "Color Fill" => &icons::COLOR_FILL,
                            "Lasso Fill" => &icons::FREEHAND,
                            _ => &icons::ERASE,
                        };
                        look.icon(ui, icon, pos2(row.left() + 20.0, row.center().y), 24.0, argb(MENU_TEXT));
                        text(
                            ui.painter(),
                            pos2(row.left() + 40.0, row.center().y),
                            Align2::LEFT_CENTER,
                            label,
                            16.5,
                            Weight::Regular,
                            argb(MENU_TEXT),
                        );
                        if ui.interact(row, ui.scope_id().with(("fill row", i)), Sense::click()).clicked() {
                            match *label {
                                "Color Fill" => {
                                    self.color_fill = !self.color_fill;
                                    self.lasso = None;
                                    session.set_touch_lasso(None);
                                    if self.color_fill && self.has {
                                        session.fill_selection();
                                    }
                                }
                                "Lasso Fill" | "Lasso Erase" => {
                                    self.kind = SelectKind::Freehand;
                                    self.lasso = Some(if *label == "Lasso Fill" {
                                        LassoAction::Fill
                                    } else {
                                        LassoAction::Erase
                                    });
                                    self.color_fill = false;
                                    self.touches(session);
                                }
                                _ => {}
                            }
                            self.popup = Popup::None;
                        }
                    }
                },
            );
            return;
        }
        let index = if self.popup == Popup::Feather { 4 } else { 5 };
        let (tip_x, tip_y) = button_tip(ctx, (BUTTON_ROOMY, BUTTON_NARROW), index, buttons);
        let width = if self.popup == Popup::Feather { FEATHER_WIDTH } else { MENU_WIDTH };
        let height = if self.popup == Popup::Feather { 44.0 + 135.0 + 10.0 + 40.0 } else { 40.0 + 8.0 + 172.0 } + 20.0;
        let screen = ctx.content_rect();
        let left = (tip_x - width / 2.0).clamp(screen.left(), screen.right() - width);
        let rect = Rect::from_min_size(pos2(left, tip_y - MENU_ARROW - height), vec2(width, height));
        egui::Area::new(Id::unique("selection popover")).fixed_pos(rect.min).order(Order::Tooltip).show(ctx, |ui| {
            popover_frame(ui, rect, tip_x, tip_y);
            let inner = rect.shrink(10.0);
            if self.popup == Popup::Feather {
                menu_title(ui, inner, "Selection edges");
                let group = Rect::from_min_size(pos2(inner.left(), inner.top() + 44.0), vec2(inner.width(), 135.0));
                ui.painter().rect_filled(group, 13, argb(MENU_GROUP));
                let amount = self.feather;
                let shown =
                    if amount <= 0.0 { "None".to_owned() } else { format!("{}%", (amount * 100.0).round() as i32) };
                let feather_row = Rect::from_min_size(group.min + vec2(0.0, 8.0), vec2(group.width(), 59.0));
                if let Some(v) = menu_slider(ui, feather_row, "Feather", amount, &shown) {
                    self.feather = v;
                    session.select_feather(v);
                }
                let sep = Rect::from_min_size(pos2(group.left(), feather_row.bottom()), vec2(group.width(), 1.0));
                ui.painter().rect_filled(sep, 0, argb(MENU_LINE));
                let pixels_row = Rect::from_min_size(pos2(group.left(), sep.bottom()), vec2(group.width(), 59.0));
                let px_norm = (self.edge_pixels as f32 / EDGE_PIXELS_MAX as f32).clamp(0.0, 1.0);
                let px_shown = format!("{} px", self.edge_pixels);
                if let Some(v) = menu_slider(ui, pixels_row, "Pixels", px_norm, &px_shown) {
                    self.edge_pixels = (v * EDGE_PIXELS_MAX as f32).round() as i32;
                }
                let btn_top = group.bottom() + 10.0;
                let btn_gap = 8.0;
                let btn_w = (inner.width() - btn_gap) / 2.0;
                let can_apply = self.has && self.edge_pixels > 0;
                let ink = if can_apply { argb(MENU_TEXT) } else { argb(MENU_TEXT).gamma_multiply(0.42) };
                for (i, (label, sign)) in [("Expand", 1_i32), ("Contract", -1_i32)].into_iter().enumerate() {
                    let btn = Rect::from_min_size(
                        pos2(inner.left() + i as f32 * (btn_w + btn_gap), btn_top),
                        vec2(btn_w, 40.0),
                    );
                    let resp = ui.interact(
                        btn,
                        ui.scope_id().with(("selection edge action", label)),
                        if can_apply { Sense::click() } else { Sense::hover() },
                    );
                    let resp = if can_apply { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
                    let fill = if can_apply && resp.is_pointer_button_down_on() {
                        argb(crate::look::pc::ACCENT)
                    } else if can_apply && resp.hovered() {
                        mix(argb(MENU_GROUP), argb(MENU_TEXT), 0.12)
                    } else {
                        argb(MENU_GROUP)
                    };
                    let btn_ink = if can_apply && resp.is_pointer_button_down_on() {
                        argb(crate::look::pc::ACCENT_INK)
                    } else {
                        ink
                    };
                    ui.painter().rect_filled(btn, 10, fill);
                    text(ui.painter(), btn.center(), Align2::CENTER_CENTER, label, 15.5, Weight::Medium, btn_ink);
                    if can_apply && resp.clicked() {
                        session.select_feather_done();
                        session.select_expand_contract(sign * self.edge_pixels);
                        self.feather = 0.0;
                        self.has = session.has_selection();
                        self.look(session);
                    }
                }
                return;
            }
            // the selections kept: a new one, and each to put back
            let ink = argb(MENU_TEXT);
            text(
                ui.painter(),
                pos2(inner.left() + 14.0, inner.top() + 20.0),
                Align2::LEFT_CENTER,
                "Selections",
                24.0,
                Weight::Bold,
                ink,
            );
            let add = Rect::from_min_size(pos2(inner.right() - 40.0, inner.top() + 2.0), vec2(40.0, 36.0));
            look.icon(ui, &icons::ADD, add.center(), 40.0, ink);
            if ui.interact(add, ui.scope_id().with("save selection"), Sense::click()).clicked() {
                self.saved = session.save_selection();
                self.open_popup_refresh(ctx, session);
            }
            for i in 0..self.saved {
                let row = Rect::from_min_size(
                    pos2(inner.left(), inner.top() + 48.0 + 52.0 * i as f32),
                    vec2(inner.width(), 51.0),
                );
                if row.bottom() > inner.bottom() {
                    break;
                }
                ui.painter().rect_filled(row, 13, argb(MENU_GROUP));
                let thumb = Rect::from_min_size(row.min + vec2(0.0, 2.5), vec2(64.0, 46.0));
                let corners = egui::CornerRadius { nw: 13, sw: 13, ne: 0, se: 0 };
                ui.painter().rect_filled(thumb, corners, Color32::BLACK);
                if let Some(t) = self.pictures.get(&i) {
                    let size = t.size_vec2();
                    let k = (thumb.width() / size.x.max(1.0)).min(thumb.height() / size.y.max(1.0));
                    let shown = Rect::from_center_size(thumb.center(), size * k);
                    // rounded with the box where it reaches the box's left side
                    let corners = if shown.left() <= thumb.left() + 0.5 { corners } else { egui::CornerRadius::ZERO };
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    let picture =
                        egui::epaint::RectShape::filled(shown, corners, Color32::WHITE).with_texture(t.id(), uv);
                    ui.painter().add(picture);
                }
                let label = format!("Selection {}", i + 1);
                text(
                    ui.painter(),
                    pos2(thumb.right() + 14.0, row.center().y),
                    Align2::LEFT_CENTER,
                    &label,
                    16.0,
                    Weight::Regular,
                    ink,
                );
                if ui.interact(row, ui.scope_id().with(("saved selection", i)), Sense::click()).clicked() {
                    session.load_selection(i);
                    self.has = session.has_selection();
                    self.look(session);
                }
            }
        });
    }

    /// The kept selections' pictures read again (one was added).
    fn open_popup_refresh(&mut self, ctx: &egui::Context, session: &CanvasSession) {
        self.popup = Popup::None;
        self.open_popup(ctx, session, Popup::Saved);
    }

    /// While a selection is made: the rectangle or ellipse dragged and the freehand path (dashed,
    /// its first point a dot to close it on), and the crosshair and bar while a colour selection's
    /// threshold is slid.
    /// Whether arrow keys move the strokes the selection takes (on a plugin's layer).
    pub fn nudges(&self) -> bool {
        self.on && !self.taken.is_empty()
    }

    fn overlay(&self, ctx: &egui::Context, session: &CanvasSession, bar_top: f32) {
        let ppp = ctx.pixels_per_point();
        let painter = ctx.layer_painter(LayerId::new(Order::Middle, Id::unique("selection overlay")));
        // the strokes the selection takes on a plugin's layer: their box, as the transform box shows
        if self.taken.len() == 8 {
            let view = session.view_matrix();
            let [a, b, c, d, e, f] =
                view.get(..6).and_then(|v| <[f32; 6]>::try_from(v).ok()).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            let corners: Vec<Pos2> = self
                .taken
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[x, y]| pos2(a * x + c * y + e, b * x + d * y + f) / ppp)
                .collect();
            let mut edge = corners.clone();
            edge.extend(corners.first());
            painter.extend(Shape::dashed_line_with_offset(
                &edge,
                Stroke::new(1.0, argb(DASH_GAP)),
                &[5.0],
                &[5.0],
                5.0,
            ));
            painter.extend(Shape::dashed_line_with_offset(&edge, Stroke::new(1.0, argb(DASH)), &[5.0], &[5.0], 0.0));
            for corner in corners {
                crate::transform::node(&painter, corner, argb(crate::transform::NODE_BLUE), 1.0);
            }
        }
        if let Some((a, b)) = self.drag {
            let (a, b) = (a / ppp, b / ppp);
            let r = Rect::from_two_pos(a, b);
            let outline: Vec<Pos2> = if self.kind == SelectKind::Ellipse {
                (0..=96)
                    .map(|k| {
                        let t = k as f32 / 96.0 * std::f32::consts::TAU;
                        pos2(r.center().x + t.cos() * r.width() / 2.0, r.center().y + t.sin() * r.height() / 2.0)
                    })
                    .collect()
            } else {
                vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()]
            };
            dashed(&painter, &outline);
        }
        let path: Vec<Pos2> =
            session.freehand_path().as_chunks::<2>().0.iter().map(|&[x, y]| pos2(x, y) / ppp).collect();
        if let Some(&first) = path.first() {
            dashed(&painter, &path);
            painter.circle_filled(first, 5.5, argb(START_DOT));
        }
        if self.polyline_armed && !self.polyline.is_empty() {
            let points: Vec<Pos2> = self.polyline.iter().map(|&p| polyline_screen(session, p, ppp)).collect();
            dashed(&painter, &points);
            if let Some(&first) = points.first() {
                painter.circle_filled(first, 5.5, argb(START_DOT));
            }
            if let (Some(&last), Some(cursor)) = (points.last(), ctx.pointer_latest_pos()) {
                if ctx.content_rect().contains(cursor)
                    && cursor.y >= bar_top
                    && ctx.layer_id_at(cursor).is_none_or(|l| l.order == Order::Background)
                {
                    painter.line_segment([last, cursor], Stroke::new(1.0, argb(DASH)));
                }
            }
        }
        if let Some((at, threshold)) = self.sliding {
            // a thin cross where the colour is picked
            let p = at / ppp;
            let arm = 8.0;
            painter.line_segment([p - vec2(arm, 0.0), p + vec2(arm, 0.0)], Stroke::new(1.0, argb(DASH)));
            painter.line_segment([p - vec2(0.0, arm), p + vec2(0.0, arm)], Stroke::new(1.0, argb(DASH)));
            let screen = ctx.content_rect();
            let band = Rect::from_min_size(pos2(screen.left(), bar_top), vec2(screen.width(), 24.0));
            painter.rect_filled(band, 0, argb(MESSAGE_BAR));
            let reading = format!("Selection Threshold {:.1}%", (threshold * 1000.0).round() / 10.0);
            text(&painter, band.center(), Align2::CENTER_CENTER, &reading, 14.0, Weight::Regular, argb(MESSAGE_TEXT));
        }
    }
}

/// What the compact Fill menu offers: Color Fill always; the raster lasso modes except on a model.
fn fill_labels(model: bool) -> &'static [&'static str] {
    if model { &["Color Fill"] } else { &["Color Fill", "Lasso Fill", "Lasso Erase"] }
}

/// The view's six numbers, exactly as the selection overlay maps document points to window pixels.
fn view_affine(session: &CanvasSession) -> [f32; 6] {
    session.view_matrix().get(..6).and_then(|v| <[f32; 6]>::try_from(v).ok()).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
}

fn polyline_screen(session: &CanvasSession, point: Pos2, ppp: f32) -> Pos2 {
    let [a, b, c, d, e, f] = view_affine(session);
    pos2(a * point.x + c * point.y + e, b * point.x + d * point.y + f) / ppp
}

fn polyline_document(session: &CanvasSession, at: Pos2, ppp: f32) -> Option<Pos2> {
    let [a, b, c, d, e, f] = view_affine(session);
    let det = a * d - b * c;
    if det.abs() < 1e-6 {
        return None;
    }
    let (sx, sy) = (at.x * ppp - e, at.y * ppp - f);
    let point = pos2((d * sx - c * sy) / det, (a * sy - b * sx) / det);
    let info = session.info();
    Some(pos2(point.x.clamp(0.0, info.width as f32), point.y.clamp(0.0, info.height as f32)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fill_menu_offers_lasso_modes_only_off_a_model() {
        assert_eq!(fill_labels(false), &["Color Fill", "Lasso Fill", "Lasso Erase"]);
        assert_eq!(fill_labels(true), &["Color Fill"]);
    }

    #[test]
    fn tapping_lasso_fill_in_the_fill_menu_arms_the_mode() {
        use egui::{Event, Modifiers, PointerButton, RawInput};

        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(100, 100);
        let mut selection = Selection::default();
        selection.open(&session);
        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut time = 0.0;
        let mut fill_button = Pos2::ZERO;
        let mut lasso_row = Pos2::ZERO;
        for step in 0..8 {
            time += 1.0 / 60.0;
            let events = match step {
                2 => vec![Event::PointerMoved(fill_button)],
                3 => vec![press(fill_button, true)],
                4 => vec![press(fill_button, false)],
                5 => vec![Event::PointerMoved(lasso_row)],
                6 => vec![press(lasso_row, true), press(lasso_row, false)],
                _ => Vec::new(),
            };
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
                time: Some(time),
                events,
                ..RawInput::default()
            };
            let mut output = ctx.clone().run_ui(raw, |ui| {
                let ctx = ui.ctx().clone();
                let _ = selection.show(&ctx, &mut look, &session, 0.0, false);
                if selection.popup == Popup::Fill {
                    let (x, tip_y) = button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), 6, 8);
                    let width = 200.0;
                    let rows = 3.0;
                    let height = 40.0 + rows * 44.0 + 16.0 + 20.0;
                    let left = (x - width / 2.0).clamp(ctx.content_rect().left(), ctx.content_rect().right() - width);
                    let top = tip_y - MENU_ARROW - height;
                    // the menu's second row (Lasso Fill), at its middle
                    lasso_row = pos2(left + width / 2.0, top + 10.0 + 44.0 + 44.0 + 20.0);
                } else {
                    fill_button =
                        pos2(x_or(&ctx, 6, 8), button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), 6, 8).1 + 30.0);
                }
            });
            output.textures_delta.clear();
        }
        assert_eq!(selection.lasso, Some(LassoAction::Fill));
        assert_eq!(selection.kind, SelectKind::Freehand);
        assert!(!selection.color_fill);
    }

    fn x_or(ctx: &egui::Context, index: usize, count: usize) -> f32 {
        button_tip(ctx, (BUTTON_ROOMY, BUTTON_NARROW), index, count).0
    }

    #[test]
    fn selection_toolbar_buttons_do_not_overlap_at_640_788_or_1440() {
        let ctx = egui::Context::default();
        for width in [640.0, 788.0, 1440.0] {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |_| {});
            // the fonts' first uploads: the toolbar's tips are measured, not the textures
            out.textures_delta.clear();
            for count in [8, 9] {
                let tips: Vec<f32> =
                    (0..count).map(|i| button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), i, count).0).collect();
                for pair in tips.windows(2) {
                    assert!(
                        pair[1] - pair[0] >= BUTTON_NARROW - 0.01,
                        "overlap at {width} with {count} buttons: {tips:?}"
                    );
                }
                assert!(tips[0] - BUTTON_NARROW / 2.0 >= ctx.content_rect().left() + 7.9);
                assert!(tips[count - 1] + BUTTON_NARROW / 2.0 <= ctx.content_rect().right() - 7.9);
            }
        }
    }

    #[test]
    fn toolbar_offers_polyline_after_ellipse_at_640() {
        use egui::RawInput;
        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(100, 100);
        let mut selection = Selection::default();
        selection.open(&session);
        let mut time = 0.0;
        let mut texts = Vec::new();
        for _ in 0..2 {
            time += 1.0 / 60.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 600.0))),
                time: Some(time),
                ..RawInput::default()
            };
            let mut output = ctx.clone().run_ui(raw, |ui| {
                let c = ui.ctx().clone();
                let _ = selection.show(&c, &mut look, &session, 0.0, false);
            });
            output.textures_delta.clear();
            texts = collect_rendered_texts(&output.shapes);
        }
        let mut previous = 0.0;
        for label in ["Automatic", "Freehand", "Rectangle", "Ellipse", "Polyline"] {
            let rect = texts.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("missing {label}: {texts:?}")).1;
            assert!(rect.left() >= previous, "{label} overlaps the segment before it: {rect:?}");
            assert!(rect.left() >= 0.0 && rect.right() <= 640.0, "{label} clipped at 640: {rect:?}");
            previous = rect.right();
        }
    }

    #[test]
    fn polyline_segment_arms_and_escape_cancels_it() {
        use egui::{Event, Modifiers, PointerButton, RawInput};
        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(100, 100);
        let mut selection = Selection::default();
        selection.open(&session);
        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut time = 0.0;
        let run =
            |selection: &mut Selection, look: &mut Look, events: Vec<Event>, time: &mut f64| -> Vec<(String, Rect)> {
                *time += 1.0 / 60.0;
                let raw = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 600.0))),
                    time: Some(*time),
                    events,
                    ..RawInput::default()
                };
                let mut output = ctx.clone().run_ui(raw, |ui| {
                    let c = ui.ctx().clone();
                    let _ = selection.show(&c, look, &session, 0.0, false);
                });
                output.textures_delta.clear();
                collect_rendered_texts(&output.shapes)
            };
        _ = run(&mut selection, &mut look, Vec::new(), &mut time);
        let texts = run(&mut selection, &mut look, Vec::new(), &mut time);
        let polyline = texts.iter().find(|(t, _)| t == "Polyline").expect("Polyline segment").1.center();
        for event in [vec![Event::PointerMoved(polyline)], vec![press(polyline, true)], vec![press(polyline, false)]] {
            _ = run(&mut selection, &mut look, event, &mut time);
        }
        _ = run(&mut selection, &mut look, Vec::new(), &mut time);
        assert!(selection.polyline_armed, "clicking Polyline must arm the mode");
        let escape = Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        _ = run(&mut selection, &mut look, vec![escape], &mut time);
        assert!(!selection.polyline_armed && selection.polyline.is_empty(), "Escape must cancel the armed polyline");
    }

    fn collect_rendered_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Rect)> {
        fn walk(shape: &Shape, out: &mut Vec<(String, Rect)>) {
            match shape {
                Shape::Text(t) => {
                    let rect = Rect::from_min_size(t.pos, t.galley.size());
                    out.push((t.galley.text().to_owned(), rect));
                }
                Shape::Vec(items) => {
                    for item in items {
                        walk(item, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn fill_button_label_tracks_lasso_fill_erase_color_fill_and_resets() {
        use egui::{Event, Modifiers, PointerButton, RawInput};

        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(100, 100);
        let mut selection = Selection::default();
        selection.open(&session);
        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let click_at = |selection: &mut Selection,
                        look: &mut Look,
                        target: Pos2,
                        width: f32,
                        time: &mut f64|
         -> Vec<(String, Rect)> {
            for step in 0..3 {
                *time += 1.0 / 60.0;
                let events = match step {
                    0 => vec![Event::PointerMoved(target)],
                    1 => vec![press(target, true)],
                    _ => vec![press(target, false)],
                };
                let raw = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
                    time: Some(*time),
                    events,
                    ..RawInput::default()
                };
                let mut output = ctx.clone().run_ui(raw, |ui| {
                    let ctx = ui.ctx().clone();
                    let _ = selection.show(&ctx, look, &session, 0.0, false);
                });
                output.textures_delta.clear();
            }
            // One settle frame so updated state renders cleanly
            *time += 1.0 / 60.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
                time: Some(*time),
                ..RawInput::default()
            };
            let mut output = ctx.clone().run_ui(raw, |ui| {
                let ctx = ui.ctx().clone();
                let _ = selection.show(&ctx, look, &session, 0.0, false);
            });
            output.textures_delta.clear();
            collect_rendered_texts(&output.shapes)
        };

        let mut time = 0.0;
        let _ = click_at(&mut selection, &mut look, pos2(-10.0, -10.0), 1200.0, &mut time);
        let (x, tip_y) = button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), 6, 8);
        let fill_btn = pos2(x, tip_y + 30.0);
        let menu_width = 200.0;
        let menu_height = 40.0 + 3.0 * 44.0 + 16.0 + 20.0;
        let menu_left = (x - menu_width / 2.0).clamp(0.0, 1200.0 - menu_width);
        let menu_top = tip_y - MENU_ARROW - menu_height;
        let row_center = |row_idx: usize| -> Pos2 {
            pos2(menu_left + menu_width / 2.0, menu_top + 10.0 + 44.0 * (row_idx + 1) as f32 + 20.0)
        };

        // 1. Click Fill -> click Lasso Fill -> toolbar must show "Lasso Fill"
        _ = click_at(&mut selection, &mut look, fill_btn, 1200.0, &mut time);
        assert_eq!(selection.popup, Popup::Fill);
        let texts = click_at(&mut selection, &mut look, row_center(1), 1200.0, &mut time);
        assert_eq!(selection.lasso, Some(LassoAction::Fill));
        assert!(
            texts.iter().any(|(t, _)| t == "Lasso Fill"),
            "expected 'Lasso Fill' toolbar label, got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );

        // 2. Click Fill button -> click Lasso Erase -> toolbar must show "Lasso Erase"
        _ = click_at(&mut selection, &mut look, fill_btn, 1200.0, &mut time);
        assert_eq!(selection.popup, Popup::Fill);
        let texts = click_at(&mut selection, &mut look, row_center(2), 1200.0, &mut time);
        assert_eq!(selection.lasso, Some(LassoAction::Erase));
        assert!(
            texts.iter().any(|(t, _)| t == "Lasso Erase"),
            "expected 'Lasso Erase' toolbar label, got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );

        // 3. Measure actual rendered label bounds at 640, 788, 1440: no overlap or clipping
        for (mode_label, row_idx, is_model) in
            [("Lasso Fill", 1, false), ("Lasso Erase", 2, false), ("Color Fill", 0, true)]
        {
            if is_model {
                // Re-run on model=true: raster lasso modes stay hidden and Color Fill fits with 9 buttons
                let mut model_sel = Selection::default();
                model_sel.open(&session);
                let _ = click_at(&mut model_sel, &mut look, pos2(-10.0, -10.0), 1200.0, &mut time);
                let (mx, mtip_y) = button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), 6, 9);
                let mfill = pos2(mx, mtip_y + 30.0);
                // Open Fill popup on model (1 row)
                for step in 0..3 {
                    time += 1.0 / 60.0;
                    let events = match step {
                        0 => vec![Event::PointerMoved(mfill)],
                        1 => vec![press(mfill, true)],
                        _ => vec![press(mfill, false)],
                    };
                    let raw = RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
                        time: Some(time),
                        events,
                        ..RawInput::default()
                    };
                    let mut out = ctx.clone().run_ui(raw, |ui| {
                        let c = ui.ctx().clone();
                        let _ = model_sel.show(&c, &mut look, &session, 0.0, true);
                    });
                    out.textures_delta.clear();
                }
                let mh = 40.0 + 1.0 * 44.0 + 16.0 + 20.0;
                let ml = (mx - menu_width / 2.0).clamp(0.0, 1200.0 - menu_width);
                let mrow = pos2(ml + menu_width / 2.0, (mtip_y - MENU_ARROW - mh) + 10.0 + 44.0 + 20.0);
                for step in 0..3 {
                    time += 1.0 / 60.0;
                    let events = match step {
                        0 => vec![Event::PointerMoved(mrow)],
                        1 => vec![press(mrow, true)],
                        _ => vec![press(mrow, false)],
                    };
                    let raw = RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
                        time: Some(time),
                        events,
                        ..RawInput::default()
                    };
                    let mut out = ctx.clone().run_ui(raw, |ui| {
                        let c = ui.ctx().clone();
                        let _ = model_sel.show(&c, &mut look, &session, 0.0, true);
                    });
                    out.textures_delta.clear();
                }
                assert!(model_sel.color_fill);
                assert_eq!(model_sel.lasso, None);
                for width in [640.0_f32, 788.0, 1440.0] {
                    time += 1.0 / 60.0;
                    let raw = RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 900.0))),
                        time: Some(time),
                        ..RawInput::default()
                    };
                    let mut out = ctx.clone().run_ui(raw, |ui| {
                        let c = ui.ctx().clone();
                        let _ = model_sel.show(&c, &mut look, &session, 0.0, true);
                    });
                    out.textures_delta.clear();
                    let texts = collect_rendered_texts(&out.shapes);
                    let button_labels: Vec<&(String, Rect)> = texts
                        .iter()
                        .filter(|(t, _)| {
                            matches!(
                                t.as_str(),
                                "Add"
                                    | "Remove"
                                    | "Invert"
                                    | "Copy & Paste"
                                    | "Edges"
                                    | "Save & Load"
                                    | "Color Fill"
                                    | "X-Ray"
                                    | "Clear"
                            )
                        })
                        .collect();
                    assert_eq!(button_labels.len(), 9, "missing model labels at {width}: {button_labels:?}");
                    for pair in button_labels.windows(2) {
                        assert!(
                            pair[0].1.right() + 2.0 <= pair[1].1.left(),
                            "model labels overlap at {width}: {:?} vs {:?}",
                            pair[0],
                            pair[1]
                        );
                    }
                }
                continue;
            }
            _ = click_at(&mut selection, &mut look, fill_btn, 1200.0, &mut time);
            _ = click_at(&mut selection, &mut look, row_center(row_idx), 1200.0, &mut time);
            for width in [640.0_f32, 788.0, 1440.0] {
                let texts = click_at(&mut selection, &mut look, pos2(-10.0, -10.0), width, &mut time);
                let button_labels: Vec<&(String, Rect)> = texts
                    .iter()
                    .filter(|(t, _)| {
                        t.as_str() == mode_label
                            || matches!(
                                t.as_str(),
                                "Add" | "Remove" | "Invert" | "Copy & Paste" | "Edges" | "Save & Load" | "Clear"
                            )
                    })
                    .collect();
                assert_eq!(button_labels.len(), 8, "missing {mode_label} labels at {width}: {button_labels:?}");
                for pair in button_labels.windows(2) {
                    assert!(
                        pair[0].1.right() + 2.0 <= pair[1].1.left(),
                        "{mode_label} button labels overlap at {width}: {:?} vs {:?}",
                        pair[0],
                        pair[1]
                    );
                }
                assert!(button_labels[0].1.left() >= 8.0, "left label clipped at {width}: {:?}", button_labels[0]);
                assert!(
                    button_labels[7].1.right() <= width - 8.0,
                    "right label clipped at {width}: {:?}",
                    button_labels[7]
                );
            }
        }

        // 4. Switching segment clears lasso mode and resets Fill label; Color Fill toggles cleanly
        let rect_segment = collect_rendered_texts(&{
            let mut out = ctx.clone().run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
                    time: Some(time + 1.0 / 60.0),
                    ..RawInput::default()
                },
                |ui| {
                    let c = ui.ctx().clone();
                    let _ = selection.show(&c, &mut look, &session, 0.0, false);
                },
            );
            out.textures_delta.clear();
            out.shapes
        })
        .into_iter()
        .find(|(t, _)| t == "Rectangle")
        .expect("Rectangle segment label")
        .1
        .center();
        let texts = click_at(&mut selection, &mut look, rect_segment, 1200.0, &mut time);
        assert_eq!(selection.lasso, None);
        assert_eq!(selection.kind, SelectKind::Rectangle);
        assert!(
            texts.iter().any(|(t, _)| t == "Fill"),
            "expected 'Fill' after switching segment, got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );
        _ = click_at(&mut selection, &mut look, fill_btn, 1200.0, &mut time);
        let texts = click_at(&mut selection, &mut look, row_center(0), 1200.0, &mut time);
        assert!(selection.color_fill);
        assert!(
            texts.iter().any(|(t, _)| t == "Color Fill"),
            "expected 'Color Fill' toolbar label, got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );

        // Toggle Color Fill off -> back to "Fill"
        _ = click_at(&mut selection, &mut look, fill_btn, 1200.0, &mut time);
        let texts = click_at(&mut selection, &mut look, row_center(0), 1200.0, &mut time);
        assert!(!selection.color_fill);
        assert!(
            texts.iter().any(|(t, _)| t == "Fill"),
            "expected idle 'Fill' label, got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );

        // Arm Lasso Erase then close & reopen -> resets to "Fill"
        _ = click_at(&mut selection, &mut look, fill_btn, 1200.0, &mut time);
        _ = click_at(&mut selection, &mut look, row_center(2), 1200.0, &mut time);
        assert_eq!(selection.lasso, Some(LassoAction::Erase));
        selection.close(&session);
        selection.open(&session);
        let texts = click_at(&mut selection, &mut look, pos2(-10.0, -10.0), 1200.0, &mut time);
        assert!(
            texts.iter().any(|(t, _)| t == "Fill"),
            "expected reset 'Fill' label after close/open, got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn edges_popover_adjusts_pixels_without_editing_and_expand_contract_dispatch_one_undo() {
        use egui::{Event, Key, Modifiers, PointerButton, RawInput};

        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(80, 80);
        assert!(session.prepare_offscreen(), "UI undo test needs software Vulkan");
        session.resize_window(1200, 900);
        session.show_in(1200, 900);
        session.fit_view();

        let mut selection = Selection::default();
        selection.open(&session);

        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut time = 0.0;
        let run = |selection: &mut Selection,
                   look: &mut Look,
                   width: f32,
                   height: f32,
                   model: bool,
                   events: Vec<Event>,
                   time: &mut f64|
         -> Vec<(String, Rect)> {
            *time += 1.0 / 60.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, height))),
                time: Some(*time),
                events,
                ..RawInput::default()
            };
            let mut out = ctx.clone().run_ui(raw, |ui| {
                let c = ui.ctx().clone();
                let _ = selection.show(&c, look, &session, 0.0, model);
            });
            out.textures_delta.clear();
            collect_rendered_texts(&out.shapes)
        };
        let click = |selection: &mut Selection, look: &mut Look, target: Pos2, time: &mut f64| -> Vec<(String, Rect)> {
            for ev in [vec![Event::PointerMoved(target)], vec![press(target, true)], vec![press(target, false)]] {
                _ = run(selection, look, 1200.0, 900.0, false, ev, time);
            }
            run(selection, look, 1200.0, 900.0, false, Vec::new(), time)
        };

        // Warm up two frames at 1200x900 with no selection: Edges button is shown and disabled
        _ = run(&mut selection, &mut look, 1200.0, 900.0, false, Vec::new(), &mut time);
        let texts = run(&mut selection, &mut look, 1200.0, 900.0, false, Vec::new(), &mut time);
        assert!(
            texts.iter().any(|(t, _)| t == "Edges"),
            "toolbar must label button 'Edges', got {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );
        let (edges_x, edges_tip_y) = button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), 4, 8);
        let edges_btn = pos2(edges_x, edges_tip_y + 30.0);
        _ = click(&mut selection, &mut look, edges_btn, &mut time);
        assert_eq!(selection.popup, Popup::None, "Edges disabled without selection/drawing");

        // Create a canonical selection via select_shape and reopen Selection state
        session.select_shape(false, 500.0, 350.0, 700.0, 550.0, SelectionJoin::Add);
        assert!(session.has_selection());
        selection.open(&session);
        _ = run(&mut selection, &mut look, 1200.0, 900.0, false, Vec::new(), &mut time);

        // Open Selection edges popover
        let texts = click(&mut selection, &mut look, edges_btn, &mut time);
        assert_eq!(selection.popup, Popup::Feather);
        assert!(texts.iter().any(|(t, _)| t == "Selection edges"), "missing title: {texts:?}");
        assert!(texts.iter().any(|(t, _)| t == "4 px"), "expected default '4 px': {texts:?}");

        // Locate Pixels slider and Expand / Contract buttons from rendered labels
        let pixels_rect = texts.iter().find(|(t, _)| t == "Pixels").expect("Pixels slider label").1;
        let expand_pt = texts.iter().find(|(t, _)| t == "Expand").expect("Expand button").1.center();
        let contract_pt = texts.iter().find(|(t, _)| t == "Contract").expect("Contract button").1.center();

        // Dragging Pixels slider updates numeric pixel amount with zero undo steps
        let edits_before_slider = session.edits();
        let slider_y = pixels_rect.top() + 36.0;
        let slider_right = pixels_rect.left() + 205.0;
        let texts = click(&mut selection, &mut look, pos2(slider_right, slider_y), &mut time);
        assert!(texts.iter().any(|(t, _)| t == "250 px"), "expected max '250 px': {texts:?}");
        assert_eq!(session.edits(), edits_before_slider, "Pixels slider must not edit selection");

        // Closing popover via Escape performs no expand/contract edit
        _ = run(
            &mut selection,
            &mut look,
            1200.0,
            900.0,
            false,
            vec![Event::Key {
                key: Key::Escape,
                physical_key: Some(Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            &mut time,
        );
        assert_eq!(selection.popup, Popup::None);
        assert_eq!(session.edits(), edits_before_slider, "closing popover must not edit selection");
        // Reopening resets Pixels to default 4 px and no expand/contract step was recorded
        let texts = click(&mut selection, &mut look, edges_btn, &mut time);
        assert!(texts.iter().any(|(t, _)| t == "4 px"), "reopening should reset to 4 px: {texts:?}");

        // Click Expand at default 4 px -> applies exactly one undoable "expand selection" step
        _ = click(&mut selection, &mut look, expand_pt, &mut time);
        assert!(session.has_selection());
        assert!(session.edits() > edits_before_slider);
        assert_eq!(session.undo().as_deref(), Some("expand selection"));
        assert_ne!(session.undo().as_deref(), Some("expand selection"), "must push only one undo step");
        assert_ne!(session.redo(), None);
        assert_eq!(session.redo().as_deref(), Some("expand selection"));

        // Drag Feather preview (~5%, sigma ~5px) then click Expand -> pending feather is recorded first, then expand
        let feather_rect = texts.iter().find(|(t, _)| t == "Feather").expect("Feather slider label").1;
        let feather_y = feather_rect.top() + 36.0;
        let texts = click(&mut selection, &mut look, pos2(feather_rect.left() + 18.0, feather_y), &mut time);
        assert!(!texts.iter().any(|(t, _)| t == "None"), "expected active Feather percent: {texts:?}");
        let texts = click(&mut selection, &mut look, expand_pt, &mut time);
        assert!(texts.iter().any(|(t, _)| t == "None"), "Expand resets Feather preview slider: {texts:?}");
        assert_eq!(session.undo().as_deref(), Some("expand selection"));
        assert_eq!(session.undo().as_deref(), Some("feather"));
        assert!(session.has_selection());

        // Drag Pixels to 250 px and click Contract -> empties selection and updates canonical state
        let texts = click(&mut selection, &mut look, pos2(slider_right, slider_y), &mut time);
        assert!(texts.iter().any(|(t, _)| t == "250 px"));
        _ = click(&mut selection, &mut look, contract_pt, &mut time);
        assert!(!session.has_selection(), "250px contract on 20px box must empty selection");
        assert!(!selection.has, "Selection UI state must reflect emptied selection");
        assert_eq!(session.undo().as_deref(), Some("contract selection"));
        assert!(session.has_selection());

        // Clicking Expand at 0 px is a no-op (no new undo step)
        let slider_left = pixels_rect.left() + 6.0;
        let texts = click(&mut selection, &mut look, pos2(slider_left, slider_y), &mut time);
        assert!(texts.iter().any(|(t, _)| t == "0 px"), "expected '0 px': {texts:?}");
        let edits_before_zero = session.edits();
        _ = click(&mut selection, &mut look, expand_pt, &mut time);
        assert_eq!(session.edits(), edits_before_zero, "0 px expand must not push undo step");
    }

    #[test]
    fn selection_edges_popover_fits_without_clipping_at_640_788_1440_and_480_height() {
        use egui::{Event, Modifiers, PointerButton, RawInput};

        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(80, 80);
        session.resize_window(640, 480);
        session.show_in(640, 480);
        session.fit_view();
        session.select_shape(false, 200.0, 120.0, 440.0, 360.0, SelectionJoin::Add);
        assert!(session.has_selection());

        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut time = 0.0;
        for model in [false, true] {
            let count = if model { 9 } else { 8 };
            for width in [640.0_f32, 788.0, 1440.0] {
                let height = 480.0_f32;
                let mut selection = Selection::default();
                selection.open(&session);
                let mut run = |events: Vec<Event>| {
                    time += 1.0 / 60.0;
                    let raw = RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, height))),
                        time: Some(time),
                        events,
                        ..RawInput::default()
                    };
                    let mut out = ctx.clone().run_ui(raw, |ui| {
                        let c = ui.ctx().clone();
                        let _ = selection.show(&c, &mut look, &session, 0.0, model);
                    });
                    out.textures_delta.clear();
                    collect_rendered_texts(&out.shapes)
                };
                _ = run(Vec::new());
                _ = run(Vec::new());
                let (ex, tip_y) = button_tip(&ctx, (BUTTON_ROOMY, BUTTON_NARROW), 4, count);
                let edges_btn = pos2(ex, tip_y + 30.0);
                for ev in
                    [vec![Event::PointerMoved(edges_btn)], vec![press(edges_btn, true)], vec![press(edges_btn, false)]]
                {
                    _ = run(ev);
                }
                let texts = run(Vec::new());
                assert_eq!(selection.popup, Popup::Feather);

                let title = texts.iter().find(|(t, _)| t == "Selection edges").expect("Selection edges title").1;
                let feather = texts.iter().find(|(t, _)| t == "Feather").expect("Feather slider").1;
                let none_val = texts.iter().find(|(t, _)| t == "None").expect("Feather None value").1;
                let pixels = texts.iter().find(|(t, _)| t == "Pixels").expect("Pixels slider").1;
                let px_val = texts.iter().find(|(t, _)| t == "4 px").expect("Pixels 4 px value").1;
                let expand = texts.iter().find(|(t, _)| t == "Expand").expect("Expand button").1;
                let contract = texts.iter().find(|(t, _)| t == "Contract").expect("Contract button").1;

                for rect in [title, feather, none_val, pixels, px_val, expand, contract] {
                    assert!(
                        rect.top() >= 0.0 && rect.bottom() <= height,
                        "clipped vertically at {width}x{height} (model={model}): {rect:?}"
                    );
                    assert!(
                        rect.left() >= 0.0 && rect.right() <= width,
                        "clipped horizontally at {width}x{height} (model={model}): {rect:?}"
                    );
                }
                assert!(
                    feather.right() + 4.0 <= none_val.left(),
                    "Feather overlaps value: {feather:?} vs {none_val:?}"
                );
                assert!(pixels.right() + 4.0 <= px_val.left(), "Pixels overlaps value: {pixels:?} vs {px_val:?}");
                assert!(
                    expand.right() + 8.0 <= contract.left(),
                    "Expand overlaps Contract: {expand:?} vs {contract:?}"
                );
            }
        }
    }

    #[test]
    fn polyline_repeated_point_within_half_a_pixel_is_ignored() {
        let mut selection = Selection::default();
        selection.polyline_armed = true;
        selection.add_polyline_point(pos2(20.0, 20.0));
        selection.add_polyline_point(pos2(20.2, 20.2));
        assert_eq!(selection.polyline.len(), 1, "a repeated point must not add a vertex");
    }

    #[test]
    fn escape_disarms_the_armed_lasso_without_a_stray_selection() {
        use egui::{Event, Key, Modifiers, RawInput};
        use sumi_core::{PenSample, PointerKind};

        let ctx = egui::Context::default();
        crate::look::install_fonts(&ctx);
        let mut look = Look::default();
        let session = CanvasSession::new(100, 100);
        session.resize_window(1200, 900);
        session.show_in(1200, 900);
        session.fit_view();

        // An existing selection the lasso must not touch, kept as a full-size picture so a stray
        // join would change a byte, not a downsampled average.
        session.select_shape(false, 300.0, 200.0, 500.0, 400.0, SelectionJoin::Add);
        assert!(session.has_selection(), "the persisted selection must be set");
        assert_eq!(session.save_selection(), 1);
        let before_image = session.saved_selection_thumbnail(0, 100, 100).expect("a saved selection's picture");
        assert_eq!((before_image.width, before_image.height), (100, 100), "the picture must not be downscaled");
        let before = before_image.rgba;
        let edits_before = session.edits();

        let mut selection = Selection::default();
        selection.open(&session);
        // the Fill menu's Lasso Fill row (selection.rs:424-433) arms the mode and its touch
        selection.kind = SelectKind::Freehand;
        selection.color_fill = false;
        selection.lasso = Some(LassoAction::Fill);
        selection.touches(&session);

        let sample = |x: f32, y: f32, t: f64| PenSample {
            x,
            y,
            pressure: 1.0,
            tilt: 0.0,
            orientation: 0.0,
            time_ms: t,
            roll: 0.0,
        };
        let mut time = 0.0;
        let run =
            |selection: &mut Selection, look: &mut Look, events: Vec<Event>, time: &mut f64| -> Vec<(String, Rect)> {
                *time += 1.0 / 60.0;
                let raw = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
                    time: Some(*time),
                    events,
                    ..RawInput::default()
                };
                let mut output = ctx.clone().run_ui(raw, |ui| {
                    let c = ui.ctx().clone();
                    let _ = selection.show(&c, look, &session, 0.0, false);
                });
                output.textures_delta.clear();
                collect_rendered_texts(&output.shapes)
            };

        // A new egui Area's first frame is a sizing pass and draws nothing visible (egui 0.37
        // `Area::begin`: `state.is_none()` starts a sizing pass). One warm-up frame, as the Edges
        // test does (lines 1088-1090), lets the toolbar render before the precondition is read.
        _ = run(&mut selection, &mut look, Vec::new(), &mut time);
        let texts = run(&mut selection, &mut look, Vec::new(), &mut time);
        assert!(
            texts.iter().any(|(t, _)| t == "Lasso Fill"),
            "the test must start armed: {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );

        // Every sample must sit inside the 100x100 canvas and right of the persisted selection
        // (its window box is 300..500 by 200..400), so a stray refill cannot hide behind the
        // selection or off-canvas clipping.
        let m = session.view_matrix();
        let [a, b, c, d, e, f] = m.get(..6).and_then(|v| <[f32; 6]>::try_from(v).ok()).expect("the view's six numbers");
        let corner = |x: f32, y: f32| pos2(a * x + c * y + e, b * x + d * y + f);
        let (cw, ch) = (session.info().width as f32, session.info().height as f32);
        let (tl, br) = (corner(0.0, 0.0), corner(cw, ch));
        let canvas_box = Rect::from_min_max(pos2(tl.x.min(br.x), tl.y.min(br.y)), pos2(tl.x.max(br.x), tl.y.max(br.y)));
        let touched = [(620.0, 200.0), (720.0, 200.0), (720.0, 300.0), (720.0, 500.0), (620.0, 500.0)];
        for (x, y) in touched {
            let at = pos2(x, y);
            assert!(canvas_box.contains(at), "the touch sample {at:?} is off the canvas {canvas_box:?}");
            assert!(x > 500.0, "the touch sample {at:?} must stay clear of the persisted selection");
        }

        // the session's finger-touch path (a physical stylus is not exercised here): down, then
        // enough travel to pass the freehand step
        let _ = session.touch_down(1, PointerKind::Finger, vec![sample(620.0, 200.0, 0.0)]);
        let _ = session.touch_move(1, vec![sample(720.0, 200.0, 10.0)]);
        let _ = session.touch_move(1, vec![sample(720.0, 300.0, 20.0)]);
        assert!(session.freehand_path().len() >= 4, "the lasso path must have started before Escape");

        // the key event the interface reads, not a direct method call
        let escape = Event::Key {
            key: Key::Escape,
            physical_key: Some(Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        _ = run(&mut selection, &mut look, vec![escape], &mut time);

        assert_eq!(selection.lasso, None, "Escape must disarm the lasso");
        assert!(session.freehand_path().is_empty(), "Escape must drop the pending enclosure");
        let texts = run(&mut selection, &mut look, Vec::new(), &mut time);
        assert!(
            texts.iter().any(|(t, _)| t == "Fill"),
            "the Fill button must read Fill again: {:?}",
            texts.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
        );
        assert!(!texts.iter().any(|(t, _)| t == "Lasso Fill"));

        // The pointer must stay cancelled after Escape. A disarm-only fix would refill the ordinary
        // freehand path here; these vertices are non-collinear and clear of the selection, so the
        // lift could close them into it. Asserting the path directly catches that before the mask
        // comparison.
        let _ = session.touch_move(1, vec![sample(720.0, 500.0, 30.0)]);
        let _ = session.touch_move(1, vec![sample(620.0, 500.0, 40.0)]);
        assert!(session.freehand_path().is_empty(), "Escape must cancel the active touch, not just the buffered path");
        let _ = session.touch_up(1, vec![sample(620.0, 200.0, 50.0)]);
        assert!(session.freehand_path().is_empty(), "the cancelled touch's lift must not refill a path");
        selection.close(&session);

        assert!(session.has_selection(), "the persisted selection must stay");
        assert_eq!(session.save_selection(), 2);
        let after_image =
            session.saved_selection_thumbnail(1, 100, 100).expect("the selection's picture after the escape");
        assert_eq!((after_image.width, after_image.height), (100, 100), "the picture must not be downscaled");
        // At the mask's own 100x100 size the helper's scale is 1, so each picture byte is the mask
        // byte: this is exact for this fixture, not a general guarantee of the thumbnail API.
        assert_eq!(before, after_image.rgba, "Escape must not join a stray path into the selection");
        // Without a GPU this session records no undo steps: this is a weak public proxy for "no new
        // edit", not proof of undo-stack integrity.
        assert_eq!(session.edits(), edits_before, "Escape must not count an edit");
    }
}

/// A path drawn as the selection's outline: thin dark dashes over light ones.
fn dashed(painter: &egui::Painter, points: &[Pos2]) {
    painter.extend(Shape::dashed_line_with_offset(points, Stroke::new(0.75, argb(DASH_GAP)), &[5.0], &[5.0], 5.0));
    painter.extend(Shape::dashed_line_with_offset(points, Stroke::new(0.75, argb(DASH)), &[5.0], &[5.0], 0.0));
}
