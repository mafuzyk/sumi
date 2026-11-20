//! The window's pointers handed to the session's touches (crates/core's `touch`, shared with the
//! Android app), which paint, move the canvas and say what the app should do. The mouse adds what
//! it can't do by touch: the wheel zooms, and the middle button (or space) moves the canvas; on a
//! model it turns it, as a finger does, and with Shift moves it. Dragged with Space and Ctrl (⌘)
//! held it zooms the canvas, with R held it turns it (with Shift in 15° steps), and R tapped turns
//! it a step (the core's key drags, keys.rs). A pen with those keys held does as the mouse does;
//! otherwise a pen and fingers go on as they do. Pressed with Ctrl and Alt held, or with Shift
//! alone, a pen or the mouse sets the brush's size and opacity as it drags sideways and up or
//! down, painting nothing.

use std::time::{Duration, Instant};

use sumi_core::{CanvasSession, KeyDrag, PenSample, PointerKind, ToolKind, TouchCommand};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// The eyedropper's window, points.
pub const PICK_WINDOW: f32 = 54.0;
/// How much a notch of the wheel zooms by.
const WHEEL_ZOOM: f32 = 1.15;
/// How much a pixel of scrolling zooms by, with Ctrl (or ⌘) held.
const SCROLL_ZOOM: f32 = 0.005;
/// How far a notch of the wheel turns the canvas with Shift held (radians, 5°).
const WHEEL_TURN: f32 = 5.0 * std::f32::consts::PI / 180.0;
/// How far a pixel of scrolling turns it with Shift held (radians, a quarter of a degree).
const SCROLL_TURN: f32 = 0.25 * std::f32::consts::PI / 180.0;

/// R let go within this, with no drag, is a tap: it turns the canvas a step (the touches' tap).
const R_TAP: Duration = Duration::from_millis(300);

/// How far the brush's size goes with a sideways drag: the slider is multiplied by e for this many
/// points, so dragging this far (about 139) to the right doubles it and as far to the left halves
/// it.
const BRUSH_SIZE_DRAG: f32 = 200.0;
/// How far a vertical drag moves the brush's opacity: the whole 0..1 slider in this many points,
/// up for more, down for less.
const BRUSH_OPACITY_DRAG: f32 = 300.0;
/// A pointer's wobble under this as it lands is no drag: the gesture applies nothing until it has
/// gone wider, so a pen resting on glass never nudges the sliders.
const BRUSH_DRAG_SLOP: f32 = 1.0;

/// The mouse's pointer id: touches have the platform's.
const MOUSE: u64 = u64::MAX;

/// Which touch the interface follows as its pointer. egui follows the first touch down and no other
/// until it lifts, so a hand resting on the canvas would keep a finger off the panels and the side
/// bar. A touch landing on the interface while the pointer's touch rests on the canvas takes the
/// pointer over; the resting one is cancelled for the interface and unheard by it from then on
/// (the canvas goes on hearing it as before).
#[derive(Default)]
pub struct InterfaceTouches {
    /// The pointer's touch as it last came, and whether it landed on the canvas.
    pointer: Option<(Touch, bool)>,
    set_aside: Vec<u64>,
}

impl InterfaceTouches {
    /// Forgets every touch being followed or set aside: a lift lost to focus loss or an artwork
    /// change would otherwise leave an id that, reused, sends the new finger to `set_aside`.
    pub fn clear(&mut self) {
        self.pointer = None;
        self.set_aside.clear();
    }

    /// What of `event` the interface hears, in order (`on_interface`: whether a point is on the
    /// interface rather than the canvas): nothing, the event, or the resting touch cancelled and then
    /// the event.
    pub fn route(&mut self, event: WindowEvent, on_interface: impl Fn((f32, f32)) -> bool) -> Vec<WindowEvent> {
        let WindowEvent::Touch(touch) = event else { return vec![event] };
        if self.set_aside.contains(&touch.id) {
            if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.set_aside.retain(|id| *id != touch.id);
            }
            return Vec::new();
        }
        let following = self.pointer.is_some_and(|(p, _)| p.id == touch.id);
        let mut heard = Vec::new();
        match touch.phase {
            TouchPhase::Started => {
                let on_canvas = !on_interface((touch.location.x as f32, touch.location.y as f32));
                match self.pointer {
                    None => self.pointer = Some((touch, on_canvas)),
                    Some((resting, true)) if !on_canvas => {
                        heard.push(WindowEvent::Touch(Touch { phase: TouchPhase::Cancelled, ..resting }));
                        self.set_aside.push(resting.id);
                        self.pointer = Some((touch, false));
                    }
                    Some(_) => {}
                }
            }
            TouchPhase::Moved if following => self.pointer = self.pointer.map(|(_, on)| (touch, on)),
            TouchPhase::Ended | TouchPhase::Cancelled if following => self.pointer = None,
            _ => {}
        }
        heard.push(WindowEvent::Touch(touch));
        heard
    }
}

/// A press on the canvas with Ctrl and Alt held (or Shift alone, as Krita has it) setting the
/// brush's size and opacity as it drags: sideways scales the size, up and down moves the opacity.
/// Nothing is painted, and the sliders are no undo step of their own.
#[derive(Clone, Copy, Debug)]
struct BrushDrag {
    /// Where the pointer went down, window points.
    start: (f32, f32),
    /// The size slider, 0..1, as the drag began.
    size: f32,
    /// The opacity slider, 0..1, as the drag began.
    opacity: f32,
    /// The tool the brush belongs to: a gesture doesn't go on into another tool's brush.
    tool: ToolKind,
    /// Where the pointer is now, window points (the readout sits beside it).
    last: (f32, f32),
    /// The pointer has gone far enough to be a drag rather than a landing's wobble.
    live: bool,
}

impl BrushDrag {
    /// Where this drag takes the sliders at `at`: the size scaled by the horizontal travel (right
    /// for more), the opacity moved by the vertical (up for more), both on their 0..1. None while
    /// the pointer hasn't gone past [`BRUSH_DRAG_SLOP`] yet.
    fn follow(&mut self, at: (f32, f32)) -> Option<(f32, f32)> {
        self.last = at;
        let dx = at.0 - self.start.0;
        // (the screen's y grows down; up is more opaque, as the volume of a thing is)
        let dy = self.start.1 - at.1;
        if !self.live {
            if dx.hypot(dy) < BRUSH_DRAG_SLOP {
                return None;
            }
            self.live = true;
        }
        let size = self.size * (dx / BRUSH_SIZE_DRAG).exp();
        let opacity = self.opacity + dy / BRUSH_OPACITY_DRAG;
        Some((size.clamp(0.0, 1.0), opacity.clamp(0.0, 1.0)))
    }
}

pub struct Input {
    clock: Instant,
    /// What the touches asked, for the app to do at the next frame.
    pub commands: Vec<TouchCommand>,
    /// A press went to the canvas since the interface last looked.
    pub canvas_pressed: bool,
    /// The selection's Polyline segment is armed: canvas presses place vertices, not paint.
    pub polyline: bool,
    /// A multi-finger gesture is in flight while Polyline is armed: suppress its egui click.
    pub polyline_gesture: bool,
    /// The pointers down on the canvas.
    down: Vec<u64>,
    /// Pointers down that the canvas didn't take (pressed on the interface, or on the canvas
    /// while a popover was open, which that press only closes): while one is, no brush outline
    /// hovers (it would stay where the pen went down, as the pen moves on).
    ignored: Vec<u64>,
    /// The mouse: where it is, whether it is over the window, and whether it (or space) moves the
    /// view.
    cursor: Option<(f32, f32)>,
    inside: bool,
    panning: bool,
    /// The mouse dragged with a key held, zooming or turning the canvas (the core's key drag).
    key_drag: Option<KeyDrag>,
    /// The pen moving the view (pressed with space or R held), as the mouse does.
    pen_panning: Option<u64>,
    /// A tablet's pen pressed the mouse pointer (X11, a Mac): its pressure as last read, for the
    /// stroke's moves while it goes unread a moment (another pointer heard from in between).
    pen_pressure: Option<f32>,
    /// The brush cursor follows the mouse hovering, so its moves draw a frame.
    pub hover_shown: bool,
    space: bool,
    /// Space has moved or zoomed the canvas since it went down; and it was tapped, doing neither
    /// (taken by the interface as the QuickMenu).
    space_used: bool,
    pub space_tapped: bool,
    /// R held for the canvas (not typed) since when, and whether the mouse went down since (then
    /// it turned the canvas by dragging, and letting go turns it no step).
    r_key: Option<Instant>,
    r_dragged: bool,
    command_key: bool,
    /// Ctrl and Alt held (on a Mac, Space with Ctrl zooms as with ⌘: the system may keep either).
    control: bool,
    alt: bool,
    shift: bool,
    /// The interface takes the keys (text being edited, the hex keypad): R is typed, not a turn.
    pub keys_taken: bool,
    /// The canvas is in view (no full-screen page or editor over it): R and Space work on it only
    /// then. The interface says so every frame, as it does `keys_taken`.
    pub canvas_shown: bool,
    /// A model is shown (not its 2D texture): dragging the view turns it, as one finger does (the
    /// touches').
    pub turns_model: bool,
    /// The canvas would paint where the pointer is (no adjustment, selection or transform is up):
    /// the interface says so every frame, and the brush's drag lives only while it does.
    painting: bool,
    /// The brush's size and opacity being set by a modifier drag, and the pointer that drags.
    brush_drag: Option<BrushDrag>,
    brush_drag_by: Option<u64>,
}

impl Default for Input {
    fn default() -> Input {
        Input {
            clock: Instant::now(),
            commands: Vec::new(),
            canvas_pressed: false,
            polyline: false,
            polyline_gesture: false,
            down: Vec::new(),
            ignored: Vec::new(),
            cursor: None,
            inside: false,
            panning: false,
            key_drag: None,
            pen_panning: None,
            pen_pressure: None,
            hover_shown: false,
            space: false,
            space_used: false,
            space_tapped: false,
            r_key: None,
            r_dragged: false,
            command_key: false,
            control: false,
            alt: false,
            shift: false,
            keys_taken: false,
            canvas_shown: true,
            turns_model: false,
            painting: false,
            brush_drag: None,
            brush_drag_by: None,
        }
    }
}

impl Input {
    /// Where the mouse hovers (window pixels): over the window, nothing pressed, the view not
    /// moving.
    /// Where the mouse (or a pen moving the pointer) last was over the window, window pixels.
    pub fn cursor(&self) -> Option<(f32, f32)> {
        self.cursor
    }

    /// Whether the mouse is over the window.
    pub fn inside(&self) -> bool {
        self.inside
    }

    pub fn hovering(&self) -> Option<(f32, f32)> {
        self.cursor.filter(|_| self.inside && self.down.is_empty() && self.ignored.is_empty() && !self.moving_view())
    }

    /// Nothing is pressed on the canvas and the view isn't moving.
    pub fn idle(&self) -> bool {
        self.down.is_empty() && !self.panning && self.key_drag.is_none()
    }

    /// Something is pressed on the canvas (a pen, the mouse, a finger), and not to move the view.
    pub fn pressing(&self) -> bool {
        !self.down.is_empty() && !self.moving_view()
    }

    /// What is painting isn't under the hand (a mouse, a tablet's pen without a screen, as far as
    /// the system says): only the screen can show where it is.
    pub fn pressing_from_afar(&self) -> bool {
        self.pressing() && self.down.iter().any(|&id| id == MOUSE || crate::pen::away_from_screen(id))
    }

    /// R and Space act on the canvas only while it is in view and no field takes the keys: a
    /// full-screen page or editor over it must not turn, zoom or open the QuickMenu behind itself.
    fn canvas_keys(&self) -> bool {
        self.canvas_shown && !self.keys_taken
    }

    /// The Space key coming down or going up: held it moves or zooms the canvas, tapped it ends as
    /// the QuickMenu's key. `consumed` is egui's from the last frame. Split out of `on_event` so the
    /// canvas-hidden gate can be tried without a winit `KeyEvent` (which has no public constructor).
    fn space_key(&mut self, session: &CanvasSession, state: ElementState, consumed: bool) {
        let held = state == ElementState::Pressed && !consumed && self.canvas_keys();
        if held && !self.space {
            self.space_used = self.command_key || self.control;
        }
        self.space_tapped |= self.space && !held && !self.space_used;
        self.space = held;
        if !self.space && self.panning && !self.down.contains(&MOUSE) {
            self.panning = false;
            self.pen_panning = None;
        }
        // a zoom ends with it too
        if !self.space && self.key_drag == Some(KeyDrag::Zoom) {
            self.end_key_drag(session);
        }
    }

    /// Alt held as the modify button is (Ctrl with it is AltGr, typing).
    pub fn modify_key(&self) -> bool {
        self.alt && !self.control
    }

    /// Space or R is held, or the mouse moves the view.
    pub fn moving_view(&self) -> bool {
        self.space || self.r_key.is_some() || self.panning || self.key_drag.is_some()
    }

    /// The mouse zooms the view, or would if pressed.
    pub fn zooming_view(&self) -> bool {
        self.key_drag.or_else(|| self.drag_held()) == Some(KeyDrag::Zoom)
    }

    /// The drag the keys held make of the mouse pressed on the canvas: R turns it, Space with
    /// Ctrl (⌘) zooms it (Space alone moves it, as the middle button does).
    fn drag_held(&self) -> Option<KeyDrag> {
        if self.r_key.is_some() {
            Some(KeyDrag::Turn)
        } else if self.space && (self.command_key || self.control) {
            Some(KeyDrag::Zoom)
        } else {
            None
        }
    }

    /// A press at `p` with keys held begins the drag they make of it, if they make one. True if
    /// they do.
    fn begin_key_drag(&mut self, session: &CanvasSession, p: (f32, f32)) -> bool {
        let Some(how) = self.drag_held() else { return false };
        session.key_drag_begin(how, p.0, p.1);
        self.key_drag = Some(how);
        self.space_used = true;
        self.r_dragged |= how == KeyDrag::Turn;
        true
    }

    fn end_key_drag(&mut self, session: &CanvasSession) {
        if self.key_drag.take().is_some() {
            session.key_drag_end();
        }
    }

    /// The canvas would paint where the pointer is, or not, as the interface says for this frame.
    /// The brush's drag ends as it stops being so (an adjustment, the selection or the transform
    /// came up, or a popover took the canvas).
    pub fn set_painting(&mut self, painting: bool) {
        if !painting {
            self.end_brush_drag();
        }
        self.painting = painting;
    }

    /// Where the brush's drag is, window points, once it has gone far enough to set the sliders:
    /// what the interface shows beside the pointer.
    pub fn brush_drag_at(&self) -> Option<(f32, f32)> {
        self.brush_drag.filter(|d| d.live).map(|d| d.last)
    }

    /// Ctrl and Alt held together (the gesture as CSP has it), or Shift alone (as Krita has it;
    /// the selection and the transform, which Shift works in their own turns, are no painting).
    fn brush_modifiers(&self) -> bool {
        (self.control && self.alt) || (self.shift && !self.control && !self.alt)
    }

    /// A press here would begin the brush's drag: the canvas paints, the pointer is on it, the keys
    /// are held, and nothing is down and no view is moving.
    fn brush_drag_ready(&self, over: bool) -> bool {
        self.painting
            && self.brush_modifiers()
            && self.brush_drag.is_none()
            && self.down.is_empty()
            && !self.panning
            && !self.moving_view()
            && self.drag_held().is_none()
            && !over
    }

    fn begin_brush_drag(&mut self, session: &CanvasSession, id: u64, p: (f32, f32)) {
        let info = session.info();
        let drag =
            BrushDrag { start: p, size: info.size, opacity: info.opacity, tool: info.tool, last: p, live: false };
        self.brush_drag = Some(drag);
        self.brush_drag_by = Some(id);
    }

    fn end_brush_drag(&mut self) {
        self.brush_drag = None;
        self.brush_drag_by = None;
    }

    /// The brush's drag follows the pointer `id` at `p`: the sliders go as it moves. True if the
    /// pointer is the drag's, the canvas then hearing nothing of it (and painting nothing).
    fn brush_drag_to(&mut self, session: &CanvasSession, id: u64, p: (f32, f32)) -> bool {
        if self.brush_drag_by != Some(id) {
            return false;
        }
        let Some(tool) = self.brush_drag.as_ref().map(|d| d.tool) else { return false };
        // another tool's brush is another slider: the gesture ends with its tool
        if session.info().tool != tool {
            self.end_brush_drag();
            return false;
        }
        self.cursor = Some(p);
        let Some(drag) = self.brush_drag.as_mut() else { return false };
        if let Some((size, opacity)) = drag.follow(p) {
            session.set_brush_size(size);
            session.set_brush_opacity(opacity);
        }
        true
    }

    fn now_ms(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }

    /// When the touches next need [`Input::tick`], if ever.
    pub fn next_deadline(&self, session: &CanvasSession) -> Option<Instant> {
        let due = session.touch_deadline()?;
        Some(self.clock + Duration::from_secs_f64((due / 1000.0).max(0.0)))
    }

    /// Runs the touches' waits that have run out. True if anything happened.
    pub fn tick(&mut self, session: &CanvasSession) -> bool {
        let out = session.touch_tick(self.now_ms());
        self.take(out)
    }

    fn take(&mut self, out: Vec<TouchCommand>) -> bool {
        let any = !out.is_empty();
        self.commands.extend(out);
        any
    }

    /// What the mouse pointer is now, at `at`: a tablet's pen moving it (X11 gives a pen as the
    /// mouse), as a pen or its eraser end with its pressure there; else the mouse.
    fn mouse_kind(at: (f32, f32)) -> (PointerKind, f32) {
        match crate::pen::mouse_pen_at(at) {
            Some(pen) => (if pen.eraser { PointerKind::StylusEraser } else { PointerKind::Stylus }, pen.pressure),
            None => (PointerKind::Mouse, 1.0),
        }
    }

    /// The pressure of the mouse pointer's sample at `at`: a stroke a pen began keeps the pen's
    /// last pressure through a moment it goes unread, rather than jumping to full as the mouse's.
    fn mouse_pressure(&mut self, at: (f32, f32)) -> f32 {
        let read = crate::pen::mouse_pen_at(at).map(|pen| pen.pressure);
        if self.pen_pressure.is_some() && read.is_some() {
            self.pen_pressure = read;
        }
        read.or(self.pen_pressure).unwrap_or(1.0)
    }

    /// A sample of pointer `id` at `(x, y)`: a pen's with its lean, if the system gave one.
    fn sample(&self, id: u64, (x, y): (f32, f32), pressure: f32) -> Vec<PenSample> {
        let (tilt, orientation) = crate::pen::lean(id).unwrap_or((0.0, 0.0));
        let roll = crate::pen::roll(id).unwrap_or(0.0);
        vec![PenSample { x, y, pressure: pressure.clamp(0.0, 1.0), tilt, orientation, time_ms: self.now_ms(), roll }]
    }

    fn pressed(&mut self, session: &CanvasSession, id: u64, kind: PointerKind, at: (f32, f32), pressure: f32) {
        self.down.retain(|d| *d != id);
        self.down.push(id);
        self.canvas_pressed = true;
        let out = session.touch_down(id, kind, self.sample(id, at, pressure));
        self.take(out);
    }

    fn moved(&mut self, session: &CanvasSession, id: u64, at: (f32, f32), pressure: f32) {
        let out = session.touch_move(id, self.sample(id, at, pressure));
        self.take(out);
    }

    fn lifted(&mut self, session: &CanvasSession, id: u64, at: (f32, f32), pressure: f32, cancelled: bool) {
        self.down.retain(|d| *d != id);
        let out =
            if cancelled { session.touch_cancel_one(id) } else { session.touch_up(id, self.sample(id, at, pressure)) };
        self.take(out);
    }

    /// Acts on `event` (`consumed` when the interface took it; `over` tells whether a point is on
    /// the interface rather than the canvas). True if anything changed. Presses go by `over` alone:
    /// `consumed` is worked out from the last frame, so a press that comes straight after the
    /// pointer (or a finger) arrives from the interface would count as the interface's.
    pub fn on_event(
        &mut self,
        session: &CanvasSession,
        event: &WindowEvent,
        consumed: bool,
        over: impl Fn((f32, f32)) -> bool,
    ) -> bool {
        match event {
            WindowEvent::ModifiersChanged(m) => {
                let state = m.state();
                self.command_key = if cfg!(target_os = "macos") { state.super_key() } else { state.control_key() };
                self.control = state.control_key();
                self.alt = state.alt_key();
                let shift = std::mem::replace(&mut self.shift, state.shift_key());
                // the brush's drag goes with the keys that made it
                if self.brush_drag.is_some() && !self.brush_modifiers() {
                    self.end_brush_drag();
                }
                // a turn goes onto its steps as Shift goes down, and off them as it comes up
                if let (Some(KeyDrag::Turn), Some(p), true) = (self.key_drag, self.cursor, shift != self.shift) {
                    session.key_drag_move(p.0, p.1, self.shift);
                    return true;
                }
                false
            }
            WindowEvent::KeyboardInput { event, .. } if event.logical_key == Key::Named(NamedKey::Space) => {
                self.space_key(session, event.state, consumed);
                true
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } if is_r(event) => {
                match event.state {
                    // (a press as the window comes to the front is a key held from before: no tap)
                    ElementState::Pressed if !event.repeat && !*is_synthetic => {
                        let typed = consumed || !self.canvas_keys() || self.command_key || self.control || self.alt;
                        self.r_key = (!typed).then(Instant::now);
                        self.r_dragged = false;
                    }
                    ElementState::Pressed => {}
                    ElementState::Released => {
                        // tapped, it turns the canvas a step (with Shift back); a drag turned it
                        let tapped = self.r_key.take().is_some_and(|since| since.elapsed() < R_TAP);
                        if tapped && !self.r_dragged && !consumed {
                            session.key_turn(self.shift);
                        }
                        if self.key_drag == Some(KeyDrag::Turn) {
                            self.end_key_drag(session);
                        }
                    }
                }
                true
            }
            WindowEvent::Focused(false) => {
                // keys let go elsewhere never come back up here (nor, perhaps, a press's lift)
                self.space = false;
                self.r_key = None;
                self.command_key = false;
                self.control = false;
                self.alt = false;
                self.shift = false;
                self.end_key_drag(session);
                self.end_brush_drag();
                // (the pen's lift, like a key's, may never come)
                if self.pen_panning.take().is_some() {
                    self.panning = false;
                }
                self.ignored.clear();
                false
            }
            WindowEvent::CursorEntered { .. } | WindowEvent::CursorLeft { .. } => {
                self.inside = matches!(event, WindowEvent::CursorEntered { .. });
                self.hover_shown
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = (position.x as f32, position.y as f32);
                self.inside = true;
                // the brush's drag has the pointer: the sliders go with it, nothing painted
                if self.brush_drag_by == Some(MOUSE) {
                    self.brush_drag_to(session, MOUSE, p);
                    return true;
                }
                if self.key_drag.is_some() {
                    self.cursor = Some(p);
                    session.key_drag_move(p.0, p.1, self.shift);
                    return true;
                }
                if self.pan_to(session, p) {
                    return true;
                }
                if self.down.contains(&MOUSE) {
                    let pressure = self.mouse_pressure(p);
                    self.moved(session, MOUSE, p, pressure);
                    return true;
                }
                // a spray can's sound (a plugin's) rattles shaken above the canvas
                session.sound_hover(p.0, p.1);
                self.hover_shown
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(p) = self.cursor else { return false };
                match (button, state) {
                    // with Ctrl and Alt (or Shift alone) the press sets the brush's size and
                    // opacity instead: nothing is painted, nor is a stroke begun
                    (MouseButton::Left, ElementState::Pressed) if self.brush_drag_ready(over(p)) => {
                        self.begin_brush_drag(session, MOUSE, p);
                        true
                    }
                    (MouseButton::Left, ElementState::Released) if self.brush_drag_by == Some(MOUSE) => {
                        self.end_brush_drag();
                        true
                    }
                    // with R held it turns the canvas, with Space and Ctrl (⌘) zooms it: nothing
                    // is painted
                    (MouseButton::Left, ElementState::Pressed)
                        if self.down.is_empty() && !self.panning && !over(p) && self.drag_held().is_some() =>
                    {
                        self.begin_key_drag(session, p);
                        false
                    }
                    (MouseButton::Left, ElementState::Released) if self.key_drag.is_some() => {
                        self.end_key_drag(session);
                        false
                    }
                    (MouseButton::Left | MouseButton::Middle, ElementState::Pressed)
                        if self.down.is_empty() && !over(p) && (self.space || *button == MouseButton::Middle) =>
                    {
                        self.panning = true;
                        self.space_used = true;
                        false
                    }
                    (MouseButton::Left, ElementState::Pressed) if self.polyline && !self.panning && !over(p) => false,
                    (MouseButton::Left, ElementState::Pressed) if !self.panning && !over(p) => {
                        let (kind, pressure) = Self::mouse_kind(p);
                        self.pen_pressure = (kind != PointerKind::Mouse).then_some(pressure);
                        self.pressed(session, MOUSE, kind, p, pressure);
                        true
                    }
                    (MouseButton::Left, ElementState::Released) if self.down.contains(&MOUSE) => {
                        let pressure = self.mouse_pressure(p);
                        self.pen_pressure = None;
                        self.lifted(session, MOUSE, p, pressure, false);
                        true
                    }
                    (MouseButton::Left | MouseButton::Middle, ElementState::Released) => {
                        self.panning = false;
                        false
                    }
                    _ => false,
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(p) = self.cursor.filter(|p| !consumed && !over(*p)) else { return false };
                // with Shift the wheel turns the canvas about the pointer, up clockwise (the
                // desktop's own, for a mouse or a pen without touch; touch and trackpads turn it
                // as on the tablet); some systems send Shift's wheel sideways
                if self.shift {
                    let turn = match delta {
                        MouseScrollDelta::LineDelta(x, y) => (if *y == 0.0 { *x } else { *y }) * WHEEL_TURN,
                        MouseScrollDelta::PixelDelta(d) => (if d.y == 0.0 { d.x } else { d.y }) as f32 * SCROLL_TURN,
                    };
                    session.view_gesture(p.0, p.1, 0.0, 0.0, 1.0, turn);
                    return true;
                }
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        session.view_gesture(p.0, p.1, 0.0, 0.0, WHEEL_ZOOM.powf(*y), 0.0);
                    }
                    MouseScrollDelta::PixelDelta(d) if self.command_key => {
                        session.view_gesture(p.0, p.1, 0.0, 0.0, (d.y as f32 * SCROLL_ZOOM).exp(), 0.0);
                    }
                    MouseScrollDelta::PixelDelta(d) => {
                        session.view_gesture(p.0, p.1, d.x as f32, d.y as f32, 1.0, 0.0);
                    }
                }
                true
            }
            WindowEvent::PinchGesture { delta, .. } => {
                let Some(p) = self.cursor.filter(|p| !over(*p)) else { return false };
                session.view_gesture(p.0, p.1, 0.0, 0.0, (1.0 + *delta as f32).max(0.01), 0.0);
                true
            }
            WindowEvent::RotationGesture { delta, .. } => {
                let Some(p) = self.cursor.filter(|p| !over(*p)) else { return false };
                // turned anticlockwise is positive; the view turns clockwise on the screen
                session.view_gesture(p.0, p.1, 0.0, 0.0, 1.0, -delta.to_radians());
                true
            }
            WindowEvent::Touch(touch) => self.on_touch(session, touch, &over),
            _ => false,
        }
    }

    /// The pointer at `p`: while the view is being moved, moved by as much. True if it was.
    fn pan_to(&mut self, session: &CanvasSession, p: (f32, f32)) -> bool {
        let last = self.cursor.replace(p);
        let (true, Some(l)) = (self.panning, last) else { return false };
        if self.turns_model && !self.shift {
            session.turn_model(p.0 - l.0, p.1 - l.1);
        } else {
            session.view_gesture(p.0, p.1, p.0 - l.0, p.1 - l.1, 1.0, 0.0);
        }
        true
    }

    fn on_touch(&mut self, session: &CanvasSession, touch: &Touch, over: &impl Fn((f32, f32)) -> bool) -> bool {
        let p = (touch.location.x as f32, touch.location.y as f32);
        // a contact the system took back (a palm, say): what it did goes
        let phase = match touch.phase {
            TouchPhase::Ended if crate::pen::was_taken_back(touch.id) => TouchPhase::Cancelled,
            // a new contact isn't an old one's: that may have gone where nothing heard of it
            TouchPhase::Started => {
                crate::pen::was_taken_back(touch.id);
                TouchPhase::Started
            }
            phase => phase,
        };
        // a pen with its pressure, or a finger: as the system says which (a touch screen may give
        // its fingers a pressure too), else a pressure means a pen
        let force = touch.force.map(|f| f.normalized() as f32);
        // (a pen landing or lifting has no force, its pressure being nothing: its own is taken)
        let pen = match crate::pen::is_pen(touch.id) {
            Some(true) => Some(force.or_else(|| crate::pen::pressure(touch.id)).unwrap_or(1.0)),
            Some(false) => None,
            None => force,
        };
        // a pen turned over rubs out with its eraser end
        let kind = match pen {
            Some(_) if crate::pen::eraser_end(touch.id) => PointerKind::StylusEraser,
            Some(_) => PointerKind::Stylus,
            None => PointerKind::Finger,
        };
        let pressure = pen.unwrap_or(1.0);
        // with keys held the pen moves the view, as the mouse does: Space moves it, Space and Ctrl
        // (⌘) zoom it, R turns it, and nothing is painted. Ctrl and Alt (or Shift alone) dragged
        // sets the brush's size and opacity, as the mouse's press does.
        if pen.is_some() {
            // the drag's own pointer: its moves set the sliders and its lift ends it
            if self.brush_drag_by == Some(touch.id) {
                match phase {
                    TouchPhase::Moved => {
                        self.brush_drag_to(session, touch.id, p);
                        return true;
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        self.end_brush_drag();
                        return true;
                    }
                    _ => {}
                }
            }
            if matches!(phase, TouchPhase::Started) && self.brush_drag_ready(over(p)) {
                self.begin_brush_drag(session, touch.id, p);
                self.cursor = Some(p);
                return true;
            }
            match phase {
                TouchPhase::Started
                    if (self.space || self.r_key.is_some()) && self.down.is_empty() && !self.panning && !over(p) =>
                {
                    if !self.begin_key_drag(session, p) {
                        self.panning = true;
                        self.space_used = true;
                    }
                    self.pen_panning = Some(touch.id);
                    self.cursor = Some(p);
                    return false;
                }
                _ if self.pen_panning != Some(touch.id) => {}
                TouchPhase::Moved if self.key_drag.is_some() => {
                    self.cursor = Some(p);
                    session.key_drag_move(p.0, p.1, self.shift);
                    return true;
                }
                TouchPhase::Moved => return self.pan_to(session, p),
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    self.end_key_drag(session);
                    self.panning = false;
                    self.pen_panning = None;
                    return false;
                }
                TouchPhase::Started => self.pen_panning = None,
            }
            // the pointer goes with it: a pen down moves no mouse (the wheel zooms about where it
            // is, and what shows once it lifts starts from there)
            self.cursor = Some(p);
        }
        let known = self.down.contains(&touch.id);
        match phase {
            TouchPhase::Started => {
                // a touch on the interface is the interface's, as on the tablet: a finger working
                // the side bar beside a hand resting on the canvas is no gesture with it (one kept
                // aside, so no outline hovers while it is down)
                if over(p) {
                    self.ignored.retain(|i| *i != touch.id);
                    self.ignored.push(touch.id);
                    return true;
                }
                if self.polyline && matches!(kind, PointerKind::Stylus | PointerKind::StylusEraser) {
                    self.ignored.retain(|i| *i != touch.id);
                    self.ignored.push(touch.id);
                    return true;
                }
                self.pressed(session, touch.id, kind, p, pressure);
                if self.polyline && self.down.len() >= 2 {
                    self.polyline_gesture = true;
                }
                true
            }
            TouchPhase::Moved if known => {
                self.moved(session, touch.id, p, pressure);
                true
            }
            TouchPhase::Ended if known => {
                self.lifted(session, touch.id, p, pressure, false);
                true
            }
            TouchPhase::Cancelled if known => {
                self.lifted(session, touch.id, p, pressure, true);
                true
            }
            TouchPhase::Ended | TouchPhase::Cancelled if self.ignored.contains(&touch.id) => {
                self.ignored.retain(|i| *i != touch.id);
                true
            }
            _ => false,
        }
    }
}

/// Whether `event` is the R key's: by its letter, or (a layout without Latin letters) by its
/// place, as the interface's own shortcuts take a key.
fn is_r(event: &KeyEvent) -> bool {
    match &event.logical_key {
        Key::Character(c) if c.eq_ignore_ascii_case("r") => true,
        Key::Character(c) => {
            egui::Key::from_name(c).is_none() && event.physical_key == PhysicalKey::Code(KeyCode::KeyR)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, Force};

    use super::*;

    fn shown() -> Arc<CanvasSession> {
        let session = CanvasSession::new(400, 300);
        session.set_display_scale(1.0);
        session.show_in(800, 600);
        session
    }

    /// A pen's touch at (`x`, `y`), the system having said it is one.
    fn pen(id: u64, phase: TouchPhase, x: f64, y: f64) -> WindowEvent {
        crate::pen::pointer_kind(id, true);
        WindowEvent::Touch(Touch {
            device_id: DeviceId::dummy(),
            phase,
            location: PhysicalPosition::new(x, y),
            force: Some(Force::Normalized(0.5)),
            id,
        })
    }

    /// A finger's touch at (`x`, `y`), with a pressure as a laptop's screen may give it, the system
    /// having said it is a finger.
    fn finger(id: u64, phase: TouchPhase, x: f64, y: f64) -> WindowEvent {
        crate::pen::pointer_kind(id, false);
        WindowEvent::Touch(Touch {
            device_id: DeviceId::dummy(),
            phase,
            location: PhysicalPosition::new(x, y),
            force: Some(Force::Normalized(0.5)),
            id,
        })
    }

    fn send(input: &mut Input, session: &CanvasSession, event: &WindowEvent) {
        input.on_event(session, event, false, |_| false);
    }

    /// `id` pressed at (100, 100), moved and lifted at `to`.
    fn drag(input: &mut Input, session: &CanvasSession, id: u64, to: (f64, f64)) {
        send(input, session, &pen(id, TouchPhase::Started, 100.0, 100.0));
        send(input, session, &pen(id, TouchPhase::Moved, to.0, to.1));
        send(input, session, &pen(id, TouchPhase::Ended, to.0, to.1));
    }

    fn painted(input: &Input) -> bool {
        input.commands.iter().any(|c| matches!(c, TouchCommand::Cursor { .. }))
    }

    /// The view's zoom, its turn (radians) and where the canvas's corner shows.
    fn view(session: &CanvasSession) -> (f32, f32, (f32, f32)) {
        let m = session.view_matrix();
        (m[0].hypot(m[1]), m[1].atan2(m[0]), (m[4], m[5]))
    }

    #[test]
    fn r_and_space_are_not_canvas_keys_while_a_full_screen_mode_is_open() {
        let mut input = Input::default();
        assert!(input.canvas_keys());
        input.canvas_shown = false;
        assert!(!input.canvas_keys(), "a full-screen mode must take R and Space from the hidden canvas");
        input.canvas_shown = true;
        input.keys_taken = true;
        assert!(!input.canvas_keys(), "a field taking the keys takes R and Space too");
    }

    #[test]
    fn space_never_opens_the_quick_menu_while_the_canvas_is_hidden() {
        let (session, mut input) = (shown(), Input::default());
        input.canvas_shown = false;
        // winit 0.30's KeyEvent has a private platform field and no public constructor, so the
        // Space branch is fed through the helper that `on_event` calls with the same state.
        input.space_key(&session, ElementState::Pressed, false);
        input.space_key(&session, ElementState::Released, false);
        assert!(!input.space_tapped);
    }

    #[test]
    fn clearing_interface_touches_forgets_a_lost_lift() {
        let mut touches = InterfaceTouches::default();
        let on_interface = |p: (f32, f32)| p.0 < 100.0;
        let phases = |heard: &[WindowEvent]| -> Vec<(u64, TouchPhase)> {
            heard
                .iter()
                .filter_map(|e| match e {
                    WindowEvent::Touch(t) => Some((t.id, t.phase)),
                    _ => None,
                })
                .collect()
        };
        // a hand rests on the canvas, then a finger on the side bar sets it aside
        assert_eq!(
            phases(&touches.route(finger(1, TouchPhase::Started, 400.0, 300.0), on_interface)),
            [(1, TouchPhase::Started)]
        );
        assert_eq!(
            phases(&touches.route(finger(2, TouchPhase::Started, 40.0, 300.0), on_interface)),
            [(1, TouchPhase::Cancelled), (2, TouchPhase::Started)]
        );
        // the hand's lift is lost (focus loss or an artwork change): forgetting both
        touches.clear();
        // its id, reused by a new finger on the interface, now hears the interface again
        assert_eq!(
            phases(&touches.route(finger(1, TouchPhase::Started, 40.0, 300.0), on_interface)),
            [(1, TouchPhase::Started)]
        );
    }

    #[test]
    fn a_finger_on_the_interface_takes_its_pointer_from_a_hand_resting_on_the_canvas() {
        let mut touches = InterfaceTouches::default();
        // the interface is left of x = 100
        let on_interface = |p: (f32, f32)| p.0 < 100.0;
        let phases = |heard: &[WindowEvent]| -> Vec<(u64, TouchPhase)> {
            heard
                .iter()
                .filter_map(|e| match e {
                    WindowEvent::Touch(t) => Some((t.id, t.phase)),
                    _ => None,
                })
                .collect()
        };
        let mut route = |event| phases(&touches.route(event, on_interface));
        // the hand rests on the canvas and moves a little
        assert_eq!(route(finger(1, TouchPhase::Started, 400.0, 300.0)), [(1, TouchPhase::Started)]);
        assert_eq!(route(finger(1, TouchPhase::Moved, 402.0, 301.0)), [(1, TouchPhase::Moved)]);
        // a finger lands on the side bar: the hand is cancelled for the interface first
        let landed = route(finger(2, TouchPhase::Started, 40.0, 300.0));
        assert_eq!(landed, [(1, TouchPhase::Cancelled), (2, TouchPhase::Started)]);
        // the finger drags a slider and lifts; the hand isn't heard from then on
        assert_eq!(route(finger(2, TouchPhase::Moved, 40.0, 250.0)), [(2, TouchPhase::Moved)]);
        assert_eq!(route(finger(1, TouchPhase::Moved, 404.0, 302.0)), []);
        assert_eq!(route(finger(2, TouchPhase::Ended, 40.0, 250.0)), [(2, TouchPhase::Ended)]);
        assert_eq!(route(finger(1, TouchPhase::Ended, 404.0, 302.0)), []);
        // a pinch on the canvas is as it was: its second finger goes on
        assert_eq!(route(finger(3, TouchPhase::Started, 400.0, 300.0)), [(3, TouchPhase::Started)]);
        assert_eq!(route(finger(4, TouchPhase::Started, 500.0, 300.0)), [(4, TouchPhase::Started)]);
        assert_eq!(route(finger(4, TouchPhase::Ended, 500.0, 300.0)), [(4, TouchPhase::Ended)]);
        assert_eq!(route(finger(3, TouchPhase::Ended, 400.0, 300.0)), [(3, TouchPhase::Ended)]);
        // and a finger on the interface first keeps it
        assert_eq!(route(finger(5, TouchPhase::Started, 40.0, 300.0)), [(5, TouchPhase::Started)]);
        assert_eq!(route(finger(6, TouchPhase::Started, 60.0, 300.0)), [(6, TouchPhase::Started)]);
    }

    #[test]
    fn a_finger_on_the_interface_beside_a_resting_hand_is_no_gesture_on_the_canvas() {
        let (session, mut input) = (shown(), Input::default());
        session.set_touch_settings(sumi_core::TouchSettings::default());
        let on_interface = |p: (f32, f32)| p.0 < 100.0;
        let mut send = |event: WindowEvent| input.on_event(&session, &event, false, on_interface);
        send(finger(1, TouchPhase::Started, 400.0, 300.0));
        // a quick tap on the side bar: no two-finger tap (an undo) with the hand
        assert!(send(finger(2, TouchPhase::Started, 40.0, 300.0)));
        assert!(send(finger(2, TouchPhase::Ended, 40.0, 300.0)));
        send(finger(1, TouchPhase::Ended, 400.0, 300.0));
        assert!(!input.commands.iter().any(|c| matches!(c, TouchCommand::Undo)), "{:?}", input.commands);
    }

    #[test]
    fn two_fingers_tapped_right_after_the_mouse_or_pen_undo_at_once() {
        let (session, mut input) = (shown(), Input::default());
        session.set_touch_settings(sumi_core::TouchSettings::default());
        let moved = |x: f64, y: f64| WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(x, y),
        };
        let button = |state| WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left };
        // a tablet's pen as X11 gives it: the mouse, pressed, moved and let go
        send(&mut input, &session, &moved(100.0, 100.0));
        send(&mut input, &session, &button(ElementState::Pressed));
        send(&mut input, &session, &moved(200.0, 150.0));
        send(&mut input, &session, &button(ElementState::Released));
        input.commands.clear();
        let tap = |input: &mut Input, ids: (u64, u64)| {
            send(input, &session, &finger(ids.0, TouchPhase::Started, 300.0, 300.0));
            send(input, &session, &finger(ids.1, TouchPhase::Started, 400.0, 300.0));
            send(input, &session, &finger(ids.0, TouchPhase::Ended, 300.0, 300.0));
            send(input, &session, &finger(ids.1, TouchPhase::Ended, 400.0, 300.0));
        };
        tap(&mut input, (9001, 9002));
        assert!(input.commands.contains(&TouchCommand::Undo), "after the mouse: {:?}", input.commands);
        // and after the pen as a pen (Windows, Wayland)
        drag(&mut input, &session, 7, (200.0, 150.0));
        input.commands.clear();
        tap(&mut input, (9003, 9004));
        assert!(input.commands.contains(&TouchCommand::Undo), "after the pen: {:?}", input.commands);
    }

    #[test]
    fn a_pinch_with_finger_painting_on_leaves_no_stroke_and_zooms_however_hard_the_fingers_press() {
        let (session, mut input) = (shown(), Input::default());
        session.set_touch_settings(sumi_core::TouchSettings::default());
        let before = view(&session).0;
        // the first paints until the second lands, each move its own event, as the system's frames
        // give them
        send(&mut input, &session, &finger(9001, TouchPhase::Started, 300.0, 300.0));
        send(&mut input, &session, &finger(9001, TouchPhase::Moved, 302.0, 301.0));
        assert!(painted(&input), "a finger paints as it lands");
        send(&mut input, &session, &finger(9002, TouchPhase::Started, 500.0, 300.0));
        for step in 1..=10 {
            let out = f64::from(step) * 8.0;
            send(&mut input, &session, &finger(9001, TouchPhase::Moved, 302.0 - out, 300.0));
            send(&mut input, &session, &finger(9002, TouchPhase::Moved, 500.0 + out, 300.0));
        }
        send(&mut input, &session, &finger(9001, TouchPhase::Ended, 222.0, 300.0));
        send(&mut input, &session, &finger(9002, TouchPhase::Ended, 580.0, 300.0));
        assert!(view(&session).0 > before * 1.5, "{} from {before}", view(&session).0);
        assert!(!input.commands.contains(&TouchCommand::StrokeEnded), "{:?}", input.commands);
    }

    #[test]
    fn a_finger_the_system_takes_back_leaves_no_stroke() {
        let (session, mut input) = (shown(), Input::default());
        session.set_touch_settings(sumi_core::TouchSettings::default());
        send(&mut input, &session, &finger(9003, TouchPhase::Started, 300.0, 300.0));
        send(&mut input, &session, &finger(9003, TouchPhase::Moved, 340.0, 320.0));
        assert!(painted(&input), "a finger paints as it lands");
        crate::pen::taken_back(9003);
        send(&mut input, &session, &finger(9003, TouchPhase::Ended, 340.0, 320.0));
        assert!(!input.commands.contains(&TouchCommand::StrokeEnded), "{:?}", input.commands);
        // and only that one: the next finger paints and keeps its stroke
        input.commands.clear();
        send(&mut input, &session, &finger(9004, TouchPhase::Started, 300.0, 300.0));
        send(&mut input, &session, &finger(9004, TouchPhase::Moved, 340.0, 320.0));
        send(&mut input, &session, &finger(9004, TouchPhase::Ended, 340.0, 320.0));
        assert!(input.commands.contains(&TouchCommand::StrokeEnded), "{:?}", input.commands);
    }

    #[test]
    fn polyline_armed_native_pen_paints_nothing_and_leaves_no_path() {
        let session = CanvasSession::new(100, 100);
        session.set_display_scale(1.0);
        session.resize_window(800, 600);
        session.show_in(800, 600);
        session.fit_view();
        let mut input = Input::default();
        input.polyline = true;
        send(&mut input, &session, &pen(9300, TouchPhase::Started, 450.0, 100.0));
        send(&mut input, &session, &pen(9300, TouchPhase::Moved, 550.0, 200.0));
        send(&mut input, &session, &pen(9300, TouchPhase::Ended, 550.0, 200.0));
        assert!(!input.commands.iter().any(|c| matches!(c, TouchCommand::StrokeEnded)));
        assert!(session.freehand_path().is_empty());
    }

    #[test]
    fn a_pen_dragged_with_space_and_ctrl_zooms_the_canvas_as_the_mouse_does_and_paints_nothing() {
        let (session, mut input) = (shown(), Input::default());
        (input.space, input.control) = (true, true);
        let (before, ..) = view(&session);
        drag(&mut input, &session, 7001, (280.0, 100.0));
        let (after, ..) = view(&session);
        // 180 right is e times as near
        assert!((after / before - std::f32::consts::E).abs() < 1e-3, "{before} {after}");
        assert!(!painted(&input) && input.idle(), "{:?}", input.commands);
        // nor is Space's tap left to open the QuickMenu
        input.space = false;
        assert!(input.space_used);
    }

    #[test]
    fn a_pen_dragged_with_r_held_turns_the_canvas_as_the_mouse_does_and_paints_nothing() {
        let (session, mut input) = (shown(), Input::default());
        input.r_key = Some(Instant::now());
        send(&mut input, &session, &pen(7002, TouchPhase::Started, 600.0, 300.0));
        send(&mut input, &session, &pen(7002, TouchPhase::Moved, 400.0, 500.0));
        assert!(input.moving_view() && !input.pressing());
        send(&mut input, &session, &pen(7002, TouchPhase::Ended, 400.0, 500.0));
        // a quarter of the way round the window's middle
        let (_, turn, _) = view(&session);
        assert!((turn - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "{turn}");
        assert!(!painted(&input) && input.r_dragged);
    }

    #[test]
    fn a_pen_dragged_with_space_alone_still_moves_the_canvas() {
        let (session, mut input) = (shown(), Input::default());
        input.space = true;
        let (before, _, (x, y)) = view(&session);
        drag(&mut input, &session, 7003, (130.0, 140.0));
        let (after, _, moved) = view(&session);
        assert_eq!(before, after);
        assert!((moved.0 - x - 30.0).abs() < 1e-3 && (moved.1 - y - 40.0).abs() < 1e-3, "{moved:?}");
        assert!(!painted(&input) && input.idle());
    }

    #[test]
    fn a_zoom_let_go_with_the_pen_down_stops_and_paints_nothing_after() {
        let (session, mut input) = (shown(), Input::default());
        (input.space, input.control) = (true, true);
        send(&mut input, &session, &pen(7004, TouchPhase::Started, 100.0, 100.0));
        // Space let go, as its key's event ends the zoom
        input.space = false;
        input.end_key_drag(&session);
        let zoom = view(&session).0;
        send(&mut input, &session, &pen(7004, TouchPhase::Moved, 300.0, 100.0));
        send(&mut input, &session, &pen(7004, TouchPhase::Ended, 300.0, 100.0));
        assert_eq!(view(&session).0, zoom);
        assert!(!painted(&input) && input.idle());
        // the next stroke paints
        drag(&mut input, &session, 7004, (120.0, 100.0));
        assert!(painted(&input));
    }

    #[test]
    fn a_pen_held_as_the_window_loses_the_keys_moves_the_view_no_more() {
        for keys in [(true, true), (true, false)] {
            let (session, mut input) = (shown(), Input::default());
            (input.space, input.control) = keys;
            send(&mut input, &session, &pen(7005, TouchPhase::Started, 100.0, 100.0));
            send(&mut input, &session, &WindowEvent::Focused(false));
            assert!(input.idle() && !input.moving_view());
            // its lift never came: the next stroke paints all the same, moves and all
            send(&mut input, &session, &pen(7005, TouchPhase::Started, 100.0, 100.0));
            input.commands.clear();
            send(&mut input, &session, &pen(7005, TouchPhase::Moved, 140.0, 100.0));
            assert!(painted(&input), "{keys:?}");
        }
    }

    #[test]
    fn the_wheel_zooms_about_where_the_pen_last_was_though_the_mouse_never_came() {
        let (session, mut input) = (shown(), Input::default());
        drag(&mut input, &session, 7006, (300.0, 200.0));
        let (before, ..) = view(&session);
        let wheel = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, 1.0),
            phase: TouchPhase::Moved,
        };
        assert!(input.on_event(&session, &wheel, false, |_| false));
        assert!((view(&session).0 / before - WHEEL_ZOOM).abs() < 1e-3);
        assert_eq!(input.cursor(), Some((300.0, 200.0)));
    }

    #[test]
    fn fingers_leave_the_pointer_alone() {
        let (session, mut input) = (shown(), Input::default());
        crate::pen::pointer_kind(7007, false);
        let finger = |phase, x, y| {
            let location = PhysicalPosition::new(x, y);
            WindowEvent::Touch(Touch { device_id: DeviceId::dummy(), phase, location, force: None, id: 7007 })
        };
        send(&mut input, &session, &finger(TouchPhase::Started, 100.0, 100.0));
        send(&mut input, &session, &finger(TouchPhase::Moved, 140.0, 100.0));
        assert_eq!(input.cursor(), None);
    }

    #[test]
    fn what_paints_from_afar_is_what_isnt_under_the_hand() {
        let (session, mut input) = (shown(), Input::default());
        // the mouse
        let (device_id, position) = (DeviceId::dummy(), PhysicalPosition::new(100.0, 100.0));
        send(&mut input, &session, &WindowEvent::CursorMoved { device_id, position });
        let press = |state| WindowEvent::MouseInput { device_id, state, button: MouseButton::Left };
        assert!(!input.pressing_from_afar(), "nothing is pressed");
        send(&mut input, &session, &press(ElementState::Pressed));
        assert!(input.pressing_from_afar() && painted(&input));
        send(&mut input, &session, &press(ElementState::Released));
        assert!(!input.pressing_from_afar());
        // a tablet's pen, one the system can't say of, and one in a screen
        crate::pen::pen_on_screen(7008, false);
        crate::pen::pen_on_screen(7010, true);
        for (id, afar) in [(7008, true), (7009, true), (7010, false)] {
            send(&mut input, &session, &pen(id, TouchPhase::Started, 100.0, 100.0));
            assert_eq!(input.pressing_from_afar(), afar, "{id}");
            send(&mut input, &session, &pen(id, TouchPhase::Ended, 100.0, 100.0));
        }
        // a finger
        crate::pen::pointer_kind(7011, false);
        let location = PhysicalPosition::new(100.0, 100.0);
        let phase = TouchPhase::Started;
        let finger = WindowEvent::Touch(Touch { device_id, phase, location, force: None, id: 7011 });
        send(&mut input, &session, &finger);
        assert!(input.pressing() && !input.pressing_from_afar());
    }

    /// A brush's drag begun at (100, 100) with the sliders `size` and `opacity`.
    fn brush_drag(size: f32, opacity: f32) -> BrushDrag {
        BrushDrag { start: (100.0, 100.0), size, opacity, tool: ToolKind::Paint, last: (100.0, 100.0), live: false }
    }

    #[test]
    fn a_brush_drag_scales_the_size_sideways_and_moves_the_opacity_vertically() {
        // a pointer's landing wobble sets nothing
        assert_eq!(brush_drag(0.5, 0.5).follow((100.3, 100.2)), None);
        // to the right the size grows, and no vertical travel leaves the opacity
        let (size, opacity) = brush_drag(0.5, 0.5).follow((100.0 + BRUSH_SIZE_DRAG / 2.0, 100.0)).expect("a drag");
        assert!((size - 0.5 * (0.5f32).exp()).abs() < 1e-5, "{size}");
        assert!((opacity - 0.5).abs() < 1e-5, "{opacity}");
        // as far left as doubling takes right quarters it, and up adds to the opacity, down takes
        let mut drag = brush_drag(0.5, 0.5);
        let left = 100.0 - BRUSH_SIZE_DRAG * std::f32::consts::LN_2;
        assert!((drag.follow((left, 100.0)).expect("a drag").0 - 0.25).abs() < 1e-4);
        let (_, opacity) = drag.follow((100.0, 100.0 - BRUSH_OPACITY_DRAG / 4.0)).expect("a drag");
        assert!((opacity - 0.75).abs() < 1e-4, "{opacity}");
        let (_, opacity) = drag.follow((100.0, 100.0 + BRUSH_OPACITY_DRAG / 4.0)).expect("a drag");
        assert!((opacity - 0.25).abs() < 1e-4, "{opacity}");
        // and both stay on the sliders' 0..1 however far the drag goes: the size grows by e, so
        // left it nears nought without reaching it (a size no brush shows), and the opacity, which
        // moves by so much a point, clamps
        assert_eq!(drag.follow((10_000.0, -10_000.0)).expect("a drag"), (1.0, 1.0));
        let (size, opacity) = drag.follow((-10_000.0, 10_000.0)).expect("a drag");
        assert!(size < 1e-6, "{size}");
        assert_eq!(opacity, 0.0);
    }

    /// The left button pressed or let go at the pointer's last place.
    fn press(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left }
    }

    fn moved_to(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position: PhysicalPosition::new(x, y) }
    }

    #[test]
    fn ctrl_alt_dragging_the_mouse_sets_the_brush_and_paints_nothing() {
        let (session, mut input) = (shown(), Input::default());
        session.set_brush_size(0.4);
        session.set_brush_opacity(0.5);
        input.set_painting(true);
        (input.control, input.alt) = (true, true);
        let before = session.info();
        let (zoom, ..) = view(&session);
        send(&mut input, &session, &moved_to(200.0, 200.0));
        send(&mut input, &session, &press(ElementState::Pressed));
        // as far right as doubles the size, and a quarter of the opacity's K up
        let right = f64::from(BRUSH_SIZE_DRAG * std::f32::consts::LN_2);
        let up = f64::from(BRUSH_OPACITY_DRAG / 4.0);
        send(&mut input, &session, &moved_to(200.0 + right, 200.0 - up));
        let after = session.info();
        assert!((after.size - 0.8).abs() < 1e-3, "{}", after.size);
        assert!((after.opacity - 0.75).abs() < 1e-3, "{}", after.opacity);
        assert!(input.brush_drag_at().is_some());
        // no stroke was begun, nothing was painted, there is no step to undo, and the view stayed
        assert!(!painted(&input), "{:?}", input.commands);
        assert!(session.freehand_path().is_empty());
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.can_undo, before.can_undo);
        assert_eq!(view(&session).0, zoom);
        assert!(input.idle() && !input.pressing() && !input.moving_view());
        send(&mut input, &session, &press(ElementState::Released));
        assert!(input.brush_drag_at().is_none() && input.idle());
    }

    #[test]
    fn a_pen_dragged_with_ctrl_alt_sets_the_brush_and_paints_nothing() {
        let (session, mut input) = (shown(), Input::default());
        session.set_brush_size(0.4);
        session.set_brush_opacity(0.5);
        input.set_painting(true);
        (input.control, input.alt) = (true, true);
        let before = session.info();
        send(&mut input, &session, &pen(7100, TouchPhase::Started, 300.0, 200.0));
        let right = f64::from(BRUSH_SIZE_DRAG * std::f32::consts::LN_2);
        send(&mut input, &session, &pen(7100, TouchPhase::Moved, 300.0 + right, 350.0));
        let after = session.info();
        assert!((after.size - 0.8).abs() < 1e-3, "{}", after.size);
        assert!(after.opacity < 1e-3, "{}", after.opacity);
        assert!(!painted(&input) && input.idle(), "{:?}", input.commands);
        assert!(session.freehand_path().is_empty());
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.can_undo, before.can_undo);
        send(&mut input, &session, &pen(7100, TouchPhase::Ended, 300.0 + right, 350.0));
        assert!(input.brush_drag_at().is_none() && !painted(&input));
    }

    #[test]
    fn shift_dragging_a_painting_canvas_sets_the_brush_as_ctrl_alt_does() {
        let (session, mut input) = (shown(), Input::default());
        session.set_brush_size(0.4);
        session.set_brush_opacity(0.5);
        input.set_painting(true);
        input.shift = true;
        drag(&mut input, &session, 7200, (200.0, 100.0));
        let after = session.info();
        // a hundred points right: e to the half, times 0.4, and no vertical travel
        assert!((after.size - 0.4 * (0.5f32).exp()).abs() < 1e-3, "{}", after.size);
        assert!((after.opacity - 0.5).abs() < 1e-3, "{}", after.opacity);
        assert!(!painted(&input), "{:?}", input.commands);
    }

    #[test]
    fn the_keys_set_the_brush_only_where_the_canvas_would_paint() {
        let (session, mut input) = (shown(), Input::default());
        session.set_brush_size(0.4);
        (input.control, input.alt) = (true, true);
        // painting is off (an adjustment, the selection or the transform is up): a stroke as ever
        send(&mut input, &session, &moved_to(200.0, 200.0));
        send(&mut input, &session, &press(ElementState::Pressed));
        send(&mut input, &session, &moved_to(260.0, 200.0));
        assert!(painted(&input), "{:?}", input.commands);
        assert_eq!(session.info().size, 0.4);
        send(&mut input, &session, &press(ElementState::Released));
        // painting on, the drag sets the brush, and it ends as its keys are let go
        input.set_painting(true);
        input.commands.clear();
        send(&mut input, &session, &moved_to(200.0, 200.0));
        send(&mut input, &session, &press(ElementState::Pressed));
        send(&mut input, &session, &moved_to(240.0, 200.0));
        assert!(input.brush_drag_at().is_some());
        let no_keys = WindowEvent::ModifiersChanged(winit::keyboard::ModifiersState::empty().into());
        send(&mut input, &session, &no_keys);
        assert!(input.brush_drag_at().is_none());
        assert!(!painted(&input), "{:?}", input.commands);
    }

    #[test]
    fn a_brush_drag_ends_with_the_window_losing_focus() {
        let (session, mut input) = (shown(), Input::default());
        input.set_painting(true);
        (input.control, input.alt) = (true, true);
        send(&mut input, &session, &moved_to(200.0, 200.0));
        send(&mut input, &session, &press(ElementState::Pressed));
        send(&mut input, &session, &moved_to(240.0, 200.0));
        assert!(input.brush_drag_at().is_some());
        send(&mut input, &session, &WindowEvent::Focused(false));
        assert!(input.brush_drag_at().is_none());
    }

    #[test]
    fn the_brush_drag_ends_with_its_tool() {
        let (session, mut input) = (shown(), Input::default());
        input.set_painting(true);
        (input.control, input.alt) = (true, true);
        send(&mut input, &session, &moved_to(200.0, 200.0));
        send(&mut input, &session, &press(ElementState::Pressed));
        send(&mut input, &session, &moved_to(240.0, 200.0));
        assert!(input.brush_drag_at().is_some());
        session.set_tool(ToolKind::Erase);
        let size = session.info().size;
        send(&mut input, &session, &moved_to(280.0, 200.0));
        assert!(input.brush_drag_at().is_none());
        assert_eq!(session.info().size, size, "another tool's brush is left alone");
        send(&mut input, &session, &press(ElementState::Released));
    }
}
