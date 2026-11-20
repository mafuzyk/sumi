//! The panels hanging from the top bar, as the Android app's (ui/Popover.kt, ui/AppRoot.kt): a
//! frosted body with an arrow pointing up at its button, a soft shadow around it, and a header.
//! Its colours are the Sumi identity's: the plum surface as the frosted body, the rose accent for
//! what is chosen, and the line colour where one meets another.

use egui::{Align2, Color32, Pos2, Rect, Sense, Ui, pos2};
use resvg::tiny_skia;

use crate::icons;
use crate::look::{Look, Shade, Themed, Weight, argb, fill, mix, pc, receded, round_rect, text};

/// A panel's own material over the canvas: the surface, with what is behind it showing through,
/// blurred.
pub const POPOVER_FROST: Themed = Themed(0xF22C292D, 0xFFF7F2EA);
pub const POPOVER_SHADOW: Themed = Themed(0x38000000, 0x24000000);
pub const POPOVER_SHADOW_RADIUS: f32 = 51.0;
pub const POPOVER_CORNER: f32 = 18.0;
pub const POPOVER_GAP: f32 = 12.0;
pub const ARROW_HEIGHT: f32 = 12.0;
pub const ARROW_SHIFT: f32 = 2.0;
pub const PANEL_TITLE: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
pub const PANEL_GLYPH: Themed = Themed(0xFFF0EAE6, 0xFF302C2E);
pub const CHEVRON_DISC: Themed = Themed(0xFF383337, 0xFFE7DFD3);
pub const CHEVRON_GLYPH: Themed = Themed(0xFFBDB2B2, 0xFF645C54);

/// Where the side bar is, for the panels to keep clear of: whether it shows, and on the right.
#[derive(Clone, Copy, Default)]
struct SideBarPlace {
    shown: bool,
    right: bool,
}

/// The side bar shows (or not) on the right (or the left), for the panels placed this frame.
pub fn side_bar_at(ctx: &egui::Context, shown: bool, right: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("side bar place"), SideBarPlace { shown, right }));
}

/// Where the side bar is this frame, if it shows.
pub fn side_bar_shown_at(ctx: &egui::Context) -> Option<Rect> {
    let side = ctx.data(|d| d.get_temp::<SideBarPlace>(egui::Id::unique("side bar place")))?;
    side.shown.then(|| crate::chrome::side_bar_rect(ctx.content_rect(), side.right))
}

/// The Layers dock: how wide it is, and the narrowest window that shows it. Narrower windows keep
/// the transient popover, without changing the preference.
pub const LAYERS_DOCK_WIDTH: f32 = 300.0;
pub const LAYERS_DOCK_MIN_WIDTH: f32 = 820.0;
/// The dock is opaque (unlike the frosted popover) and edged on the side facing the canvas,
/// with the studio's structural surfaces (the surface as its base, the raised one as its header)
/// and the line between them.
pub const DOCK_FILL: Themed = Themed(0xFF2C292D, 0xFFF7F2EA);
pub const DOCK_HEADER_FILL: Themed = Themed(0xFF383337, 0xFFECE4D8);
pub const DOCK_EDGE: Themed = Themed(0xFF50484C, 0xFFD0C5B8);
pub const DOCK_HEADER_HEIGHT: f32 = 52.0;

/// Where the Layers dock goes: against the right edge when the side bar is on the left
/// (`!right_handed`), else the left; from under the top bar to the window's bottom. None when it
/// is off or the window is too narrow, where the popover takes its place.
pub fn layers_dock(window: Rect, wanted: bool, right_handed: bool) -> Option<Rect> {
    if !wanted || window.width() < LAYERS_DOCK_MIN_WIDTH {
        return None;
    }
    let right = !right_handed;
    let left = if right { window.right() - LAYERS_DOCK_WIDTH } else { window.left() };
    let rect =
        Rect::from_min_max(pos2(left, window.top() + pc::BAR_HEIGHT), pos2(left + LAYERS_DOCK_WIDTH, window.bottom()));
    (rect.height() > 0.0).then_some(rect)
}

/// The dock's opaque body, its header strip and the edge on its canvas side: no glass, so nothing
/// shows through. Both fills recede by the chrome's dim this frame, as the bars' do: the dock is
/// drawn by the Layers and the Navigator, which this reads from the frame's context rather than
/// being handed it.
pub fn dock_frame(ui: &Ui, rect: Rect, right: bool) {
    let painter = ui.painter();
    let dim = chrome_dim(ui.ctx());
    painter.rect_filled(rect, 0.0, receded(argb(DOCK_FILL), dim));
    let header_bottom = (rect.top() + DOCK_HEADER_HEIGHT).min(rect.bottom());
    let header = Rect::from_min_max(rect.min, pos2(rect.right(), header_bottom));
    painter.rect_filled(header, 0.0, receded(argb(DOCK_HEADER_FILL), dim));
    let edge = egui::Stroke::new(1.0, argb(DOCK_EDGE));
    if header_bottom < rect.bottom() {
        painter.line_segment([pos2(rect.left(), header_bottom), pos2(rect.right(), header_bottom)], edge);
    }
    let x = if right { rect.left() } else { rect.right() };
    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], edge);
}

/// How far the chrome recedes this frame (0..1): the interface sets it once, before the bars and
/// the dock are drawn, and their fills step back by it while a stroke is painted.
pub fn chrome_dim_at(ctx: &egui::Context, dim: f32) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("chrome dim"), dim.clamp(0.0, 1.0)));
}

/// The chrome's dim this frame, none where the interface set none.
pub fn chrome_dim(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::unique("chrome dim"))).unwrap_or(0.0).clamp(0.0, 1.0)
}

/// Where the Layers dock is this frame, if it shows: for the input to keep its presses off the
/// canvas, as the side bar's place is.
pub fn layers_dock_at(ctx: &egui::Context, rect: Option<Rect>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("layers dock"), rect));
}

pub fn layers_dock_shown_at(ctx: &egui::Context) -> Option<Rect> {
    ctx.data(|d| d.get_temp::<Option<Rect>>(egui::Id::unique("layers dock")).flatten())
}

/// Where a panel `width` wide goes in the window, hanging from a button centred at `button_x`:
/// its left edge and its arrow's tip from that edge. It is centred on the button (the tip
/// `ARROW_SHIFT` right of the button's centre) and kept inside the window, clear of the side bar
/// on whichever side it is.
pub fn place(ctx: &egui::Context, width: f32, button_x: f32) -> (f32, f32) {
    let screen = ctx.content_rect();
    let side = ctx
        .data(|d| d.get_temp::<SideBarPlace>(egui::Id::unique("side bar place")))
        .unwrap_or(SideBarPlace { shown: true, right: false });
    let clear = if side.shown { pc::SIDE_BAR_WIDTH + POPOVER_GAP } else { POPOVER_GAP };
    let (min_left, min_right) = if side.right { (POPOVER_GAP, clear) } else { (clear, POPOVER_GAP) };
    let tip = button_x + ARROW_SHIFT;
    let lo = screen.left() + min_left;
    let hi = (screen.right() - min_right - width).max(lo);
    let left = (tip - width / 2.0).clamp(lo, hi);
    (left, tip - left)
}

/// A panel's body (below its arrow) for a panel at `rect` (arrow included).
pub fn body(rect: Rect) -> Rect {
    Rect::from_min_max(pos2(rect.left(), rect.top() + ARROW_HEIGHT), rect.max)
}

/// Draws a panel at `rect` (its arrow on top, the tip `arrow_x` from its left edge): the shadow
/// round it, then its frosted body, whose blur goes into `backdrops` (window pixels).
pub fn frame(ui: &Ui, look: &mut Look, slot: &'static str, rect: Rect, arrow_x: f32, backdrops: &mut Vec<f32>) {
    let body = body(rect);
    let ppp = ui.ctx().pixels_per_point();
    backdrops.extend([body.left(), body.top(), body.right(), body.bottom(), POPOVER_CORNER].map(|v| v * ppp));
    backdrops.push(1.0);
    // only round the panel: none of it shows through
    let shadow = egui::epaint::Shadow {
        offset: [0, 0],
        blur: POPOVER_SHADOW_RADIUS as u8,
        spread: 0,
        color: argb(POPOVER_SHADOW),
    };
    ui.painter().add(shadow.as_shape(body, POPOVER_CORNER));
    let size = rect.size();
    look.shape(ui, slot, (arrow_x * 100.0).round() as i32, rect, |pixmap, t| {
        let mut pb = tiny_skia::PathBuilder::new();
        let (half, tip) = (ARROW_HEIGHT, 2.0);
        // a corner the arrow comes near is rounded less, so that the arrow rises from a straight
        // edge rather than hanging over the curve (at a small interface size, or a button near
        // the window's edge)
        let near = |from: f32| (from - half - tip).clamp(0.0, POPOVER_CORNER);
        let corners = [near(arrow_x), near(size.x - arrow_x), POPOVER_CORNER, POPOVER_CORNER];
        round_rect(&mut pb, Rect::from_min_max(pos2(0.0, ARROW_HEIGHT), size.to_pos2()), corners);
        pb.move_to(arrow_x - half - tip, ARROW_HEIGHT + 0.5);
        pb.line_to(arrow_x - tip, tip);
        pb.quad_to(arrow_x, 0.0, arrow_x + tip, tip);
        pb.line_to(arrow_x + half + tip, ARROW_HEIGHT + 0.5);
        pb.close();
        frost(pixmap, pb, t);
    });
}

/// Draws a floating panel at `rect` (no arrow): the shadow round it, then its frosted body, whose
/// blur goes into `backdrops` (window pixels).
pub fn floating_frame(ui: &Ui, look: &mut Look, slot: &'static str, rect: Rect, backdrops: &mut Vec<f32>) {
    let ppp = ui.ctx().pixels_per_point();
    backdrops.extend([rect.left(), rect.top(), rect.right(), rect.bottom(), POPOVER_CORNER].map(|v| v * ppp));
    backdrops.push(1.0);
    let shadow = egui::epaint::Shadow {
        offset: [0, 0],
        blur: POPOVER_SHADOW_RADIUS as u8,
        spread: 0,
        color: argb(POPOVER_SHADOW),
    };
    ui.painter().add(shadow.as_shape(rect, POPOVER_CORNER));
    let size = rect.size();
    look.shape(ui, slot, (size.y * 10.0).round() as i32, rect, |pixmap, t| {
        let mut pb = tiny_skia::PathBuilder::new();
        round_rect(&mut pb, Rect::from_min_size(pos2(0.0, 0.0), size), [POPOVER_CORNER; 4]);
        frost(pixmap, pb, t);
    });
}

/// Draws a panel at `rect` (arrow included) with its arrow on its `left` side, else its right, the
/// tip `arrow_y` from its top, and `corner` rounding: the shadow round it, then its frosted body,
/// whose blur goes into `backdrops` (window pixels). Returns the body.
#[allow(clippy::too_many_arguments)]
pub fn side_frame(
    ui: &Ui,
    look: &mut Look,
    slot: &'static str,
    rect: Rect,
    arrow_y: f32,
    left: bool,
    corner: f32,
    backdrops: &mut Vec<f32>,
) -> Rect {
    let body = side_body(rect, left);
    let ppp = ui.ctx().pixels_per_point();
    backdrops.extend([body.left(), body.top(), body.right(), body.bottom(), corner].map(|v| v * ppp));
    backdrops.push(1.0);
    let shadow = egui::epaint::Shadow {
        offset: [0, 0],
        blur: POPOVER_SHADOW_RADIUS as u8,
        spread: 0,
        color: argb(POPOVER_SHADOW),
    };
    ui.painter().add(shadow.as_shape(body, corner));
    let size = rect.size();
    let key = ((arrow_y * 100.0).round() as i32, left, (corner * 10.0) as i32);
    look.shape(ui, slot, key, rect, |pixmap, t| frost(pixmap, side_outline(size, arrow_y, left, corner), t));
    body
}

/// As [`side_frame`], filled `colour` (nothing shows through). Returns the body.
#[allow(clippy::too_many_arguments)]
pub fn solid_side_frame(
    ui: &Ui,
    look: &mut Look,
    slot: &'static str,
    rect: Rect,
    arrow_y: f32,
    left: bool,
    corner: f32,
    colour: impl Shade,
) -> Rect {
    let colour = colour.value();
    let body = side_body(rect, left);
    let shadow = egui::epaint::Shadow {
        offset: [0, 0],
        blur: POPOVER_SHADOW_RADIUS as u8,
        spread: 0,
        color: argb(POPOVER_SHADOW),
    };
    ui.painter().add(shadow.as_shape(body, corner));
    let size = rect.size();
    let key = ((arrow_y * 100.0).round() as i32, left, (corner * 10.0) as i32, colour);
    look.shape(ui, slot, key, rect, |pixmap, t| {
        fill(pixmap, side_outline(size, arrow_y, left, corner).finish(), argb(colour), t);
    });
    body
}

/// A side-arrowed panel's body: `rect` less its arrow, on its `left` side or its right.
fn side_body(rect: Rect, left: bool) -> Rect {
    if left {
        Rect::from_min_max(pos2(rect.left() + ARROW_HEIGHT, rect.top()), rect.max)
    } else {
        Rect::from_min_max(rect.min, pos2(rect.right() - ARROW_HEIGHT, rect.bottom()))
    }
}

/// The outline of a panel `size` big (arrow included) with its arrow on its `left` side, else its
/// right, the tip `arrow_y` from its top, rounded `corner`: one shape, filled as one.
fn side_outline(size: egui::Vec2, arrow_y: f32, left: bool, corner: f32) -> tiny_skia::PathBuilder {
    let mut pb = tiny_skia::PathBuilder::new();
    let (half, tip, w) = (ARROW_HEIGHT, 2.0, size.x);
    if left {
        round_rect(&mut pb, Rect::from_min_max(pos2(ARROW_HEIGHT, 0.0), size.to_pos2()), [corner; 4]);
        pb.move_to(ARROW_HEIGHT + 0.5, arrow_y - half - tip);
        pb.line_to(tip, arrow_y - tip);
        pb.quad_to(0.0, arrow_y, tip, arrow_y + tip);
        pb.line_to(ARROW_HEIGHT + 0.5, arrow_y + half + tip);
    } else {
        round_rect(&mut pb, Rect::from_min_max(Pos2::ZERO, pos2(w - ARROW_HEIGHT, size.y)), [corner; 4]);
        pb.move_to(w - ARROW_HEIGHT - 0.5, arrow_y - half - tip);
        pb.line_to(w - tip, arrow_y - tip);
        pb.quad_to(w, arrow_y, w - tip, arrow_y + tip);
        pb.line_to(w - ARROW_HEIGHT - 0.5, arrow_y + half + tip);
    }
    pb.close();
    pb
}

/// Draws a panel at `rect` (arrow included) with its arrow `below` it (else above), the tip
/// `arrow_x` from its left edge, rounded `corner` and filled `colour` (nothing shows through): the
/// shadow round it, then the panel. Returns its body.
#[allow(clippy::too_many_arguments)]
pub fn solid_frame(
    ui: &Ui,
    look: &mut Look,
    slot: &'static str,
    rect: Rect,
    arrow_x: f32,
    below: bool,
    corner: f32,
    colour: impl Shade,
) -> Rect {
    let colour = colour.value();
    let body = if below {
        Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - ARROW_HEIGHT))
    } else {
        Rect::from_min_max(pos2(rect.left(), rect.top() + ARROW_HEIGHT), rect.max)
    };
    let shadow = egui::epaint::Shadow {
        offset: [0, 0],
        blur: POPOVER_SHADOW_RADIUS as u8,
        spread: 0,
        color: argb(POPOVER_SHADOW),
    };
    ui.painter().add(shadow.as_shape(body, corner));
    let size = rect.size();
    let key = ((arrow_x * 100.0).round() as i32, colour, below, (corner * 10.0) as i32);
    look.shape(ui, slot, key, rect, |pixmap, t| {
        let mut pb = tiny_skia::PathBuilder::new();
        let (half, tip, h) = (ARROW_HEIGHT, 2.0, size.y);
        if below {
            round_rect(&mut pb, Rect::from_min_max(Pos2::ZERO, pos2(size.x, h - ARROW_HEIGHT)), [corner; 4]);
            pb.move_to(arrow_x - half - tip, h - ARROW_HEIGHT - 0.5);
            pb.line_to(arrow_x - tip, h - tip);
            pb.quad_to(arrow_x, h, arrow_x + tip, h - tip);
            pb.line_to(arrow_x + half + tip, h - ARROW_HEIGHT - 0.5);
        } else {
            round_rect(&mut pb, Rect::from_min_max(pos2(0.0, ARROW_HEIGHT), size.to_pos2()), [corner; 4]);
            pb.move_to(arrow_x - half - tip, ARROW_HEIGHT + 0.5);
            pb.line_to(arrow_x - tip, tip);
            pb.quad_to(arrow_x, 0.0, arrow_x + tip, tip);
            pb.line_to(arrow_x + half + tip, ARROW_HEIGHT + 0.5);
        }
        pb.close();
        let mut paint = tiny_skia::Paint::default();
        let [r, g, b, a] = argb(colour).to_srgba_unmultiplied();
        paint.set_color_rgba8(r, g, b, a);
        paint.anti_alias = true;
        if let Some(path) = pb.finish() {
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None);
        }
    });
    body
}

/// A panel's outline filled with its frosted material: the arrow and the body are one shape,
/// filled as one, without an even-odd seam.
fn frost(pixmap: &mut tiny_skia::Pixmap, pb: tiny_skia::PathBuilder, t: tiny_skia::Transform) {
    let mut paint = tiny_skia::Paint::default();
    let [r, g, b, a] = argb(POPOVER_FROST).to_srgba_unmultiplied();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    if let Some(path) = pb.finish() {
        pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None);
    }
}

/// Fills a rounded rectangle (corners `[top left, top right, bottom right, bottom left]`) kept as
/// `slot`: for the parts of a panel that follow its rounded corners.
pub fn rounded(ui: &Ui, look: &mut Look, slot: &'static str, rect: Rect, corners: [f32; 4], colour: Color32) {
    let size = rect.size();
    let key = (corners.map(|c| (c * 10.0) as i32), colour.to_array());
    look.shape(ui, slot, key, rect, |pixmap, t| {
        let mut pb = tiny_skia::PathBuilder::new();
        round_rect(&mut pb, Rect::from_min_size(Pos2::ZERO, size), corners);
        fill(pixmap, pb.finish(), colour, t);
    });
}

/// A panel's header: its title (22 semibold, the identity's quieter editorial voice) `start` in,
/// with the chevron disc after it. Returns whether the title was tapped.
pub fn header(ui: &Ui, look: &mut Look, top: Rect, title: &str, start: f32, chevron: bool, chevron_on: bool) -> bool {
    header_title(ui, look, top, title, start, chevron, chevron_on).0
}

/// As [`header`]; also where the title and its chevron are, for a menu to hang from.
pub fn header_title(
    ui: &Ui,
    look: &mut Look,
    top: Rect,
    title: &str,
    start: f32,
    chevron: bool,
    chevron_on: bool,
) -> (bool, Rect) {
    let painter = ui.painter();
    let at = pos2(top.left() + start, top.top() + 19.0);
    let words = text(painter, at, Align2::LEFT_TOP, title, 22.0, Weight::SemiBold, argb(PANEL_TITLE));
    if chevron {
        let centre = pos2(words.right() + 10.0 + 8.5, words.center().y - 1.5);
        painter.circle_filled(centre, 8.5, if chevron_on { argb(pc::ACCENT) } else { argb(CHEVRON_DISC) });
        // the glyph follows the disc it sits on: the accent's ink on the rose, the idle ink otherwise
        let ink = if chevron_on { argb(pc::ACCENT_INK) } else { argb(CHEVRON_GLYPH) };
        look.icon(ui, &icons::CHEVRON_DOWN, centre, 13.0, ink);
    }
    let row = if chevron { Rect::from_min_max(words.min, pos2(words.right() + 27.0, words.bottom())) } else { words };
    (ui.interact(row, ui.scope_id().with(("title", title)), Sense::click()).clicked(), row)
}

/// How much of the ink a hovered row, plate or button takes: the one amount the panels, the menus,
/// the layers and the quick menu share, so a pointer reads the same wherever it rests. A press goes
/// further, as the menus' rows do; nothing animates, so no look has to be told not to.
pub const HOVER_INK: f32 = 0.08;

/// The rose accent on a selected row, else the row's own fill, a step of the ink over it while the
/// pointer is on it. Selection stays the loudest thing in the row: hover never dims it.
pub fn row_fill(selected: bool, hovered: bool, idle: impl Shade) -> Color32 {
    let idle = argb(idle);
    if selected {
        argb(pc::ACCENT)
    } else if hovered {
        mix(idle, argb(pc::TEXT), HOVER_INK)
    } else {
        idle
    }
}

/// The plate a row with no fill of its own takes under the pointer: a step of the ink over whatever
/// is behind it, as the panels' rows take their fill.
pub fn hover_plate(hovered: bool) -> Option<Color32> {
    hovered.then(|| mix(Color32::TRANSPARENT, argb(pc::TEXT), HOVER_INK))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library's set cell, the fill its rows are given, for the hover tests below.
    const ROW_CELL: Themed = Themed(0x4050484C, 0xFFDCD2C4);

    fn window(width: f32) -> Rect {
        Rect::from_min_max(Pos2::ZERO, pos2(width, 900.0))
    }

    #[test]
    fn the_layers_dock_sits_opposite_the_side_bar_under_the_top_bar() {
        for width in [1440.0, 1280.0] {
            let right = layers_dock(window(width), true, false).expect("a wide window shows the dock");
            assert_eq!(right.right(), width);
            assert_eq!(right.width(), LAYERS_DOCK_WIDTH);
            assert_eq!(right.top(), pc::BAR_HEIGHT);
            assert_eq!(right.bottom(), 900.0);
            let left = layers_dock(window(width), true, true).expect("a wide window shows the dock");
            assert_eq!(left.left(), 0.0);
            assert_eq!(left.width(), LAYERS_DOCK_WIDTH);
            assert_eq!(left.top(), pc::BAR_HEIGHT);
            assert_eq!(left.bottom(), 900.0);
        }
    }

    #[test]
    fn a_narrow_window_or_the_dock_turned_off_keeps_the_popover() {
        for width in [800.0, 640.0] {
            assert_eq!(layers_dock(window(width), true, false), None);
        }
        assert_eq!(layers_dock(window(1440.0), false, false), None);
    }

    /// A row under the pointer takes a step of the ink, in either look; the rose accent a selected
    /// row wears wins over it, so selection stays the loudest thing in the row.
    #[test]
    fn a_hovered_row_takes_a_step_of_the_ink_and_selection_outranks_it() {
        let previous = crate::look::light();
        let mut seen = Vec::new();
        for light in [false, true] {
            crate::look::set_light(light);
            seen.push((
                light,
                row_fill(false, false, ROW_CELL),
                row_fill(false, true, ROW_CELL),
                row_fill(true, false, ROW_CELL),
                row_fill(true, true, ROW_CELL),
                argb(pc::ACCENT),
                argb(pc::TEXT),
            ));
        }
        // Never leave the global look swapped for another test, even when asserting.
        crate::look::set_light(previous);
        for (light, idle, hovered, chosen, chosen_over, accent, ink) in seen {
            assert_ne!(hovered, idle, "the pointer left the row as it was ({light:?})");
            assert_ne!(hovered, ink, "the pointer filled the row with the ink outright ({light:?})");
            assert_eq!(chosen, accent, "a selected row lost the accent ({light:?})");
            assert_eq!(chosen_over, chosen, "hover moved the accent off the selected row ({light:?})");
            assert_ne!(chosen, hovered, "hover read as selection ({light:?})");
        }
    }

    /// A row with no fill of its own (a brush cell) takes a plate under the pointer only.
    #[test]
    fn a_row_with_no_fill_of_its_own_takes_the_hover_plate_only_under_the_pointer() {
        let previous = crate::look::light();
        let mut seen = Vec::new();
        for light in [false, true] {
            crate::look::set_light(light);
            seen.push((light, hover_plate(false), hover_plate(true)));
        }
        crate::look::set_light(previous);
        for (light, idle, hovered) in seen {
            assert_eq!(idle, None, "a row with no pointer over it took a plate ({light:?})");
            let plate = hovered.unwrap_or_else(|| panic!("a hovered row took no plate ({light:?})"));
            assert!(plate.a() > 0, "the hover plate is invisible ({light:?})");
            assert!(plate.a() < 255, "the hover plate covers what is behind it ({light:?})");
        }
    }
}
