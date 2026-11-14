//! The scrolling logic, kept free of any device I/O so it can be tested.
//!
//! [`Router`] receives the physical touchpad's events one by one. At each
//! `SYN_REPORT` it decides where the finished frame goes: to the virtual
//! touchpad clone (normal touches) or into wheel events (ring touches).

use evdev::{EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode};

const EV_SYN: u16 = EventType::SYNCHRONIZATION.0;
const EV_KEY: u16 = EventType::KEY.0;
const EV_ABS: u16 = EventType::ABSOLUTE.0;
const SYN_REPORT: u16 = SynchronizationCode::SYN_REPORT.0;
const ABS_X: u16 = evdev::AbsoluteAxisCode::ABS_X.0;
const ABS_Y: u16 = evdev::AbsoluteAxisCode::ABS_Y.0;
const BTN_TOUCH: u16 = KeyCode::BTN_TOUCH.0;
const BUTTONS: [u16; 3] = [
    KeyCode::BTN_LEFT.0,
    KeyCode::BTN_RIGHT.0,
    KeyCode::BTN_MIDDLE.0,
];
const TOOL_FINGERS: [(u16, u32); 5] = [
    (KeyCode::BTN_TOOL_FINGER.0, 1),
    (KeyCode::BTN_TOOL_DOUBLETAP.0, 2),
    (KeyCode::BTN_TOOL_TRIPLETAP.0, 3),
    (KeyCode::BTN_TOOL_QUADTAP.0, 4),
    (KeyCode::BTN_TOOL_QUINTTAP.0, 5),
];

/// Where the pad is, in raw touchpad units.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub cx: f64,
    pub cy: f64,
    pub rx: f64,
    pub ry: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub ring_start: f64,
    pub degrees_per_notch: f64,
    pub reverse: bool,
    pub min_track_radius: f64,
}

/// Wheel movement to emit: hi-res units (120 per notch) and whole notches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wheel {
    pub hires: i32,
    pub notches: i32,
}

impl Wheel {
    pub fn events(self) -> Vec<InputEvent> {
        let rel = EventType::RELATIVE.0;
        let mut ev = vec![InputEvent::new(
            rel,
            RelativeAxisCode::REL_WHEEL_HI_RES.0,
            self.hires,
        )];
        if self.notches != 0 {
            ev.push(InputEvent::new(
                rel,
                RelativeAxisCode::REL_WHEEL.0,
                self.notches,
            ));
        }
        ev
    }
}

/// Turns finger angle changes around the pad centre into wheel movement.
pub struct Scroller {
    geom: Geometry,
    set: Settings,
    last_angle: Option<f64>,
    hires_acc: f64, // pending hi-res units not yet emitted
    notch_acc: i32, // emitted hi-res units not yet reported as full notches
}

impl Scroller {
    pub fn new(geom: Geometry, set: Settings) -> Self {
        Scroller {
            geom,
            set,
            last_angle: None,
            hires_acc: 0.0,
            notch_acc: 0,
        }
    }

    /// Distance from the centre (1.0 = edge) and angle in degrees.
    fn polar(&self, x: i32, y: i32) -> (f64, f64) {
        let dx = (x as f64 - self.geom.cx) / self.geom.rx;
        let dy = (y as f64 - self.geom.cy) / self.geom.ry;
        (dx.hypot(dy), dy.atan2(dx).to_degrees())
    }

    pub fn in_ring(&self, x: i32, y: i32) -> bool {
        self.polar(x, y).0 >= self.set.ring_start
    }

    pub fn start(&mut self, x: i32, y: i32) {
        self.last_angle = Some(self.polar(x, y).1);
        self.hires_acc = 0.0;
        self.notch_acc = 0;
    }

    /// Returns the wheel movement for the finger moving to (x, y), if any.
    pub fn moved(&mut self, x: i32, y: i32) -> Option<(Wheel, f64)> {
        let (r, ang) = self.polar(x, y);
        if r < self.set.min_track_radius {
            self.last_angle = None; // too close to centre: angle is meaningless
            return None;
        }
        let last = self.last_angle.replace(ang)?;
        let d = (ang - last + 180.0).rem_euclid(360.0) - 180.0; // shortest signed delta
        // Touchpad Y grows downward, so positive d is clockwise on screen.
        // Clockwise -> scroll down -> negative wheel values.
        let sign = if self.set.reverse { 1.0 } else { -1.0 };
        self.hires_acc += sign * d / self.set.degrees_per_notch * 120.0;
        let hires = self.hires_acc.trunc() as i32;
        if hires == 0 {
            return None;
        }
        self.hires_acc -= hires as f64;
        self.notch_acc += hires;
        let notches = self.notch_acc / 120; // truncates toward zero
        self.notch_acc -= notches * 120;
        Some((Wheel { hires, notches }, d))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Pass,
    Scroll,
}

/// What to do with one finished frame.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    /// Frame for the virtual touchpad, sent followed by SYN_REPORT. `None` = send nothing.
    pub clone: Option<Vec<InputEvent>>,
    pub wheel: Option<Wheel>,
    /// Human-readable note for --debug.
    pub note: Option<String>,
}

pub struct Router {
    scroller: Scroller,
    pub mode: Mode,
    x: Option<i32>,
    y: Option<i32>,
    tools: [bool; 5],
    frame: Vec<InputEvent>,
}

impl Router {
    pub fn new(scroller: Scroller) -> Self {
        Router {
            scroller,
            mode: Mode::Idle,
            x: None,
            y: None,
            tools: [false; 5],
            frame: Vec::new(),
        }
    }

    fn fingers(&self) -> u32 {
        TOOL_FINGERS
            .iter()
            .zip(self.tools)
            .filter(|(_, on)| *on)
            .map(|((_, n), _)| *n)
            .max()
            .unwrap_or(0)
    }

    /// Feed one event. Returns `Some` when a frame is complete.
    pub fn event(&mut self, ev: InputEvent) -> Option<Output> {
        let (ty, code, value) = (ev.event_type().0, ev.code(), ev.value());
        if ty != EV_SYN {
            self.frame.push(ev);
            if ty == EV_ABS && code == ABS_X {
                self.x = Some(value);
            } else if ty == EV_ABS && code == ABS_Y {
                self.y = Some(value);
            } else if ty == EV_KEY
                && let Some(i) = TOOL_FINGERS.iter().position(|(c, _)| *c == code)
            {
                self.tools[i] = value != 0;
            }
            return None;
        }
        if code != SYN_REPORT {
            self.frame.clear(); // SYN_DROPPED etc.: discard partial frame
            return None;
        }
        let frame = std::mem::take(&mut self.frame);
        let touch = |v: i32| {
            frame
                .iter()
                .any(|f| f.event_type().0 == EV_KEY && f.code() == BTN_TOUCH && f.value() == v)
        };
        let (touch_down, touch_up) = (touch(1), touch(0));
        let mut out = Output::default();

        if self.mode == Mode::Idle && touch_down {
            match (self.x, self.y) {
                (Some(x), Some(y)) if self.fingers() <= 1 && self.scroller.in_ring(x, y) => {
                    self.mode = Mode::Scroll;
                    self.scroller.start(x, y);
                    out.note = Some(format!("scroll start at ({x},{y})"));
                }
                _ => self.mode = Mode::Pass,
            }
        }

        if self.mode == Mode::Scroll {
            if let (Some(x), Some(y)) = (self.x, self.y)
                && let Some((wheel, d)) = self.scroller.moved(x, y)
            {
                out.wheel = Some(wheel);
                out.note = Some(format!(
                    "scroll d={d:+.1} hires={} notches={}",
                    wheel.hires, wheel.notches
                ));
            }
            // Still pass physical button presses through while scrolling.
            let buttons: Vec<_> = frame
                .into_iter()
                .filter(|f| f.event_type().0 == EV_KEY && BUTTONS.contains(&f.code()))
                .collect();
            out.clone = (!buttons.is_empty()).then_some(buttons);
        } else {
            out.clone = Some(frame);
        }

        if touch_up {
            if self.mode == Mode::Scroll {
                out.note = Some("scroll end".into());
            }
            self.mode = Mode::Idle;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEOM: Geometry = Geometry {
        cx: 3000.0,
        cy: 3000.0,
        rx: 2000.0,
        ry: 2000.0,
    };
    const SET: Settings = Settings {
        ring_start: 0.7,
        degrees_per_notch: 20.0,
        reverse: false,
        min_track_radius: 0.35,
    };

    fn key(code: KeyCode, v: i32) -> InputEvent {
        InputEvent::new(EV_KEY, code.0, v)
    }
    fn abs(code: u16, v: i32) -> InputEvent {
        InputEvent::new(EV_ABS, code, v)
    }
    fn syn() -> InputEvent {
        InputEvent::new(EV_SYN, SYN_REPORT, 0)
    }
    /// Point on a circle of radius `r` (fraction of pad radius) at `deg` degrees.
    fn at(r: f64, deg: f64) -> (i32, i32) {
        let a = deg.to_radians();
        (
            (3000.0 + r * 2000.0 * a.cos()).round() as i32,
            (3000.0 + r * 2000.0 * a.sin()).round() as i32,
        )
    }
    /// Send one frame and return its output.
    fn frame(r: &mut Router, evs: &[InputEvent]) -> Output {
        for e in evs {
            assert!(r.event(*e).is_none());
        }
        r.event(syn()).expect("frame output")
    }
    fn touch_down(r: &mut Router, (x, y): (i32, i32), fingers: KeyCode) -> Output {
        frame(
            r,
            &[
                key(KeyCode::BTN_TOUCH, 1),
                key(fingers, 1),
                abs(ABS_X, x),
                abs(ABS_Y, y),
            ],
        )
    }
    fn move_to(r: &mut Router, (x, y): (i32, i32)) -> Output {
        frame(r, &[abs(ABS_X, x), abs(ABS_Y, y)])
    }
    fn router() -> Router {
        Router::new(Scroller::new(GEOM, SET))
    }

    #[test]
    fn touch_in_the_middle_passes_everything_through() {
        let mut r = router();
        let out = touch_down(&mut r, at(0.2, 0.0), KeyCode::BTN_TOOL_FINGER);
        assert_eq!(r.mode, Mode::Pass);
        assert_eq!(out.clone.unwrap().len(), 4);
        assert_eq!(out.wheel, None);
        let out = move_to(&mut r, at(0.9, 0.0)); // wandering into the ring later doesn't scroll
        assert_eq!(out.clone.unwrap().len(), 2);
        assert_eq!(out.wheel, None);
        frame(
            &mut r,
            &[key(KeyCode::BTN_TOUCH, 0), key(KeyCode::BTN_TOOL_FINGER, 0)],
        );
        assert_eq!(r.mode, Mode::Idle);
    }

    #[test]
    fn clockwise_around_the_ring_scrolls_down() {
        let mut r = router();
        let out = touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_FINGER);
        assert_eq!(r.mode, Mode::Scroll);
        assert_eq!(out.clone, None, "ring touches must not reach the pointer");
        let (mut hires, mut notches) = (0, 0);
        for step in 1..=18 {
            // 90 degrees clockwise in 5 degree steps
            let out = move_to(&mut r, at(0.9, step as f64 * 5.0));
            assert_eq!(out.clone, None);
            if let Some(w) = out.wheel {
                hires += w.hires;
                notches += w.notches;
            }
        }
        assert_eq!(
            notches, -4,
            "90 degrees / 20 per notch = 4.5 -> 4 whole notches"
        );
        assert!((hires + 540).abs() <= 1, "hires total {hires}");
    }

    #[test]
    fn anticlockwise_scrolls_up_and_reverse_flips_it() {
        let mut r = router();
        touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_FINGER);
        let w = move_to(&mut r, at(0.9, -30.0)).wheel.unwrap();
        assert!(w.hires > 0 && w.notches == 1);

        let mut r = Router::new(Scroller::new(
            GEOM,
            Settings {
                reverse: true,
                ..SET
            },
        ));
        touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_FINGER);
        let w = move_to(&mut r, at(0.9, -30.0)).wheel.unwrap();
        assert!(w.hires < 0 && w.notches == -1);
    }

    #[test]
    fn crossing_the_180_degree_line_takes_the_short_way() {
        let mut s = Scroller::new(GEOM, SET);
        let (x, y) = at(0.9, 175.0);
        s.start(x, y);
        let (x, y) = at(0.9, -175.0); // +10 degrees clockwise, not -350
        let (w, d) = s.moved(x, y).unwrap();
        assert!((d - 10.0).abs() < 0.5, "d = {d}");
        assert!(w.hires < 0);
    }

    #[test]
    fn two_finger_touches_are_never_taken_over() {
        let mut r = router();
        let out = touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_DOUBLETAP);
        assert_eq!(r.mode, Mode::Pass);
        assert_eq!(out.clone.unwrap().len(), 4);
    }

    #[test]
    fn crossing_the_centre_pauses_tracking() {
        let mut r = router();
        touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_FINGER);
        assert_eq!(move_to(&mut r, at(0.1, 90.0)).wheel, None);
        // First point back outside only re-anchors the angle.
        assert_eq!(move_to(&mut r, at(0.9, 180.0)).wheel, None);
        assert!(move_to(&mut r, at(0.9, 200.0)).wheel.is_some());
    }

    #[test]
    fn buttons_still_click_while_scrolling() {
        let mut r = router();
        touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_FINGER);
        let out = frame(&mut r, &[abs(ABS_X, 4800), key(KeyCode::BTN_LEFT, 1)]);
        assert_eq!(out.clone, Some(vec![key(KeyCode::BTN_LEFT, 1)]));
    }

    #[test]
    fn lifting_ends_the_scroll() {
        let mut r = router();
        touch_down(&mut r, at(0.9, 0.0), KeyCode::BTN_TOOL_FINGER);
        let out = frame(
            &mut r,
            &[key(KeyCode::BTN_TOUCH, 0), key(KeyCode::BTN_TOOL_FINGER, 0)],
        );
        assert_eq!(
            out.clone, None,
            "the lift belongs to the scroll, not the pointer"
        );
        assert_eq!(r.mode, Mode::Idle);
        // Next touch in the middle moves the pointer again.
        touch_down(&mut r, at(0.1, 0.0), KeyCode::BTN_TOOL_FINGER);
        assert_eq!(r.mode, Mode::Pass);
    }

    #[test]
    fn dropped_events_discard_the_partial_frame() {
        let mut r = router();
        assert!(r.event(abs(ABS_X, 100)).is_none());
        assert!(
            r.event(InputEvent::new(
                EV_SYN,
                SynchronizationCode::SYN_DROPPED.0,
                0
            ))
            .is_none()
        );
        let out = r.event(syn()).unwrap();
        assert_eq!(out.clone, Some(vec![]));
    }

    #[test]
    fn slow_movement_accumulates_instead_of_being_lost() {
        let mut s = Scroller::new(GEOM, SET);
        let (x, y) = at(0.9, 0.0);
        s.start(x, y);
        let mut total = 0;
        for i in 1..=200 {
            let (x, y) = at(0.9, i as f64 * 0.1); // 0.1 degree steps, 20 degrees total
            if let Some((w, _)) = s.moved(x, y) {
                total += w.notches;
            }
        }
        assert_eq!(total, -1);
    }
}
