//! the warning: a mati bead drops on a thread from the bar at the top of the display. a
//! compact capsule beside it counts down with skip and postpone buttons. it is one surface at
//! the top right, under the bar, whose clear parts let the pointer through. there is no way to
//! find the tray icon on Wayland, so the bead hangs where a tray usually is.
//!
//! the bead swings like a pendulum. when grabbed, it follows the pointer. when let go, the
//! swing keeps the release velocity. the bar is a wall that flattens its top and annoys it.

use super::anim::{ARRIVE, Anim, EASE, Fill, Prop};
use super::bead::Bead;
use super::button::{Button, ButtonStyle, Surface};
use super::desktop::Look;
use super::gl::{Affine, Gl, Rect, Shape, premul};
use super::text::{Fit, Fonts, Style, Text, Weight};
use super::wl::{Anchor, KeyboardInteractivity, Layer, Shell, Spec};
use crate::app::format::fmt_warning;
use crate::app::{Action, Buttons};
use crate::mati::form::FLAT_MOST;
use crate::mati::motion::{
    ANNOY_AT, ANNOY_HOLD, BRISK, BUBBLE_BLUR, BUBBLE_FROM, DAMPING_FREE, DAMPING_SETTLE, DROP_KICK,
    GRAB_SLOP, LOOK, LOOK_REACH, POP, RAISING, ROUNDING, STIFFNESS, STILL_ANGLE, STILL_SPEED,
    STIR_AFTER, STIR_KICK, WALL_DAMPING, WALL_STIFFNESS, chase, press, pressed,
};

const BUTTON_H: f32 = 40.0;
/// from the capsule's edge to its buttons, and to the top of its text
const PAD: f32 = 16.0;
/// to the text's left edge
const TEXT_INSET: f32 = 20.0;
/// between the heading and the line under it, and between that line and the buttons
const LINE_GAP: f32 = 2.0;
const TEXT_GAP: f32 = 14.0;
/// the type: a heading, the line under it and the buttons, on macOS's text sizes
const HEADING: Style = Style::new(15.0, Weight::Semibold);
const TEXT: Style = Style::new(13.0, Weight::Medium);
const BUTTON_GAP: f32 = 8.0;
/// between "Your break begins in" and the count
const COUNT_GAP: f32 = 4.0;
const CAPSULE_MIN_W: f32 = 336.0;
/// from the capsule's right edge to the thread, and from the bar down to the capsule
const CAPSULE_GAP: f32 = 46.0;
const CAPSULE_DROP: f32 = 12.0;
pub const THREAD: f32 = 42.0;
pub const BEAD: f32 = 64.0;
/// the charm's area: wide enough for the swing out to the bar and a little into it as the
/// bead flattens. it is also deep enough for the thread and the bead
const CHARM_W: f32 = 240.0;
const CHARM_H: f32 = THREAD + BEAD + 18.0;
/// where the bead hangs from, in from the display's right edge, where a tray icon usually sits
pub const HANG_FROM_RIGHT: f32 = CHARM_W / 2.0;
/// the capsule's corners, concentric with its buttons': theirs, plus the padding between
const CAPSULE_RADIUS: f32 = BUTTON_H / 2.0 + PAD;
/// clear space around the capsule, for its shadow
const MARGIN: f32 = 12.0;
const SHADOW_ROOM: f32 = 26.0;
/// the capsule fades in where it sits, a beat after the bead drops, as its text and buttons
/// come out of a slight blur. it leaves the same way
const CONTENT_BLUR: f64 = 4.0;
const SHOW_DELAY: f64 = 0.10;
const SHOW_DURATION: f64 = 0.22;
const HIDE_DURATION: f64 = 0.18;
/// how long the surfaces take to fade in or out under reduced motion
pub const CROSS_FADE: f64 = 0.25;
/// the break's bead takes over this long after the break screen is asked for, and the
/// warning's bead stays until then
pub const FLIGHT_DELAY: f64 = 0.08;
const BUTTON_STYLE: ButtonStyle = ButtonStyle {
    surface: Surface::Fill,
    height: BUTTON_H,
    min_width: 0.0,
    padding: 52.0,
    title: 13.0,
    hint: 11.0,
};

/// the warning's bead as the break screen's bead takes over from it
pub struct HandOff {
    /// in the warning surface's logical pixels
    pub rect: Rect,
    /// how far its rings lean toward the pointer, y up
    pub look: (f64, f64),
    /// the warning surface's width, which is at the right edge of the area under the bar
    pub surface_width: f32,
    pub output: Option<wayland_client::protocol::wl_output::WlOutput>,
}

pub struct Warning {
    pub surface: u32,
    look: Look,
    animate: bool,
    size: (f32, f32),
    capsule: Rect,
    texts: Option<(f32, [Text; 2])>,
    count: Option<(f32, u64, Text)>,
    remaining: u64,
    count_x: f32,
    buttons: Vec<Button>,
    capsule_opacity: Prop,
    body_blur: Prop,
    charm_opacity: Prop,
    charm_scale: Prop,
    thread_scale: Prop,
    swing: Swing,
    bead: Bead,
    /// when the warning started to leave, and how long it takes
    pub leaving: Option<(f64, f64)>,
    pointer: Option<(f32, f32)>,
    pressed: Option<usize>,
    last_frame: Option<f64>,
    /// the input region last set, so it changes only when the bead moves
    input: Option<[i32; 4]>,
}

impl Warning {
    /// the warning, which counts down `remaining` seconds. a skip or postpone key is a global
    /// shortcut held elsewhere, and `hints` are the triggers to show on the skip and postpone
    /// buttons. without skip, it offers Dismiss
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        shell: &mut Shell,
        qh: &wayland_client::QueueHandle<super::desktop::Desktop>,
        fonts: &mut Fonts,
        look: Look,
        remaining: u64,
        buttons: Buttons,
        hints: [Option<String>; 2],
        now: f64,
    ) -> Option<Warning> {
        let [skip_hint, postpone_hint] = hints;
        let mut row = Vec::new();
        if buttons.postpone.is_some() {
            row.push(Button::new(
                fonts,
                "Postpone",
                None,
                postpone_hint,
                Action::Postpone,
                BUTTON_STYLE,
            ));
        }
        row.push(match buttons.skip {
            Some(_) => Button::new(fonts, "Skip", None, skip_hint, Action::Skip, BUTTON_STYLE),
            None => Button::new(
                fonts,
                "Dismiss",
                None,
                None,
                Action::DismissWarning,
                BUTTON_STYLE,
            ),
        });

        let heading_w = fonts.width("Time to rest your eyes", HEADING);
        let prefix_w = fonts.width("Your break begins in", TEXT);
        // as wide as the longest count, to keep any count inside the capsule
        let count_w = fonts.width("10:00", TEXT.tabular());
        let buttons_w: f32 =
            row.iter().map(|b| b.rect.w).sum::<f32>() + BUTTON_GAP * (row.len() as f32 - 1.0);
        let capsule_w = CAPSULE_MIN_W
            .max(PAD + buttons_w + PAD)
            .max(TEXT_INSET + heading_w + TEXT_INSET)
            .max(TEXT_INSET + prefix_w + COUNT_GAP + count_w + TEXT_INSET)
            .ceil();
        let capsule_h =
            PAD + HEADING.line_height() + LINE_GAP + TEXT.line_height() + TEXT_GAP + BUTTON_H + PAD;
        let width = MARGIN + capsule_w + CAPSULE_GAP + HANG_FROM_RIGHT;
        let height = CHARM_H.max(CAPSULE_DROP + capsule_h + SHADOW_ROOM).ceil();
        let capsule = Rect::new(MARGIN, CAPSULE_DROP, capsule_w, capsule_h);
        let mut x = capsule.x + capsule_w - PAD - buttons_w;
        for button in &mut row {
            button.rect.x = x;
            button.rect.y = capsule.y + capsule_h - PAD - BUTTON_H;
            x += button.rect.w + BUTTON_GAP;
        }

        let surface = shell.create(
            qh,
            Spec {
                // the compositor puts it on the display in use
                output: None,
                layer: Layer::Overlay,
                anchor: Anchor::Top | Anchor::Right,
                size: (width.ceil() as u32, height as u32),
                // under the bar, whose bottom edge the bead hangs from
                exclusive_zone: 0,
                // it never takes the keyboard from the app the user is in
                keyboard: KeyboardInteractivity::None,
            },
        )?;

        let pivot = (width - HANG_FROM_RIGHT, 0.0);
        let mut bead = Bead::new(BEAD, false, look.solid);
        let swing = Swing::new(pivot, THREAD, BEAD);
        swing.place(&mut bead);

        let animate = look.animate;
        let mut warning = Warning {
            surface,
            look,
            animate,
            size: (width.ceil(), height),
            capsule,
            texts: None,
            count: None,
            remaining,
            count_x: capsule.x + TEXT_INSET + prefix_w + COUNT_GAP,
            buttons: row,
            capsule_opacity: Prop::new(1.0),
            body_blur: Prop::new(0.0),
            charm_opacity: Prop::new(1.0),
            charm_scale: Prop::new(1.0),
            thread_scale: Prop::new(1.0),
            swing,
            bead,
            leaving: None,
            pointer: None,
            pressed: None,
            last_frame: None,
            input: None,
        };
        if !animate {
            // reduced motion: both fade up where they are, and the bead hangs still
            for prop in [&mut warning.capsule_opacity, &mut warning.charm_opacity] {
                prop.add(Anim::tween(0.0, 1.0, now, CROSS_FADE, EASE, Fill::Entrance));
            }
        } else {
            // thread and bead swell in as one, on a spring, as the bead's color comes out
            // of blur
            warning
                .charm_opacity
                .add(Anim::tween(0.0, 1.0, now, 0.22, EASE, Fill::Entrance));
            warning
                .charm_scale
                .add(Anim::spring(BUBBLE_FROM, 1.0, now, POP, Fill::Entrance));
            warning.bead.blur.add(Anim::tween(
                BUBBLE_BLUR,
                0.0,
                now,
                0.36,
                EASE,
                Fill::Entrance,
            ));
            let begin = now + SHOW_DELAY;
            warning.capsule_opacity.add(Anim::tween(
                0.0,
                1.0,
                begin,
                SHOW_DURATION,
                ARRIVE,
                Fill::Entrance,
            ));
            warning.body_blur.add(Anim::tween(
                CONTENT_BLUR,
                0.0,
                begin,
                SHOW_DURATION,
                ARRIVE,
                Fill::Entrance,
            ));
            warning.swing.free = true;
            warning.swing.omega = DROP_KICK;
            warning.bead.blink(now);
        }
        if shell.can_blur() && !look.solid {
            shell.set_blur(qh, surface, &rounded_rects(capsule, CAPSULE_RADIUS));
        }
        Some(warning)
    }

    pub fn update_countdown(&mut self, remaining: u64) {
        self.remaining = remaining;
    }

    /// the hanging bead, for the break screen's bead to take over from
    pub fn hand_off(&self, shell: &Shell) -> HandOff {
        let (x, y) = self.swing.bead_center();
        let r = BEAD / 2.0;
        HandOff {
            rect: Rect::new(x - r, y - r, BEAD, BEAD),
            look: (self.swing.look[0], self.swing.look[1]),
            surface_width: self.size.0,
            output: shell.get(self.surface).and_then(|s| s.output.clone()),
        }
    }

    /// with `hand_off`, the break screen's bead takes over from this one, which goes as that
    /// one's flight starts, and the thread lifts
    pub fn hide(&mut self, hand_off: bool, now: f64) {
        for button in &mut self.buttons {
            button.sleep(now);
        }
        self.pressed = None;
        if !self.animate {
            // reduced motion: both fade away where they are
            for prop in [&mut self.capsule_opacity, &mut self.charm_opacity] {
                let from = prop.at(now);
                prop.add(Anim::tween(from, 0.0, now, CROSS_FADE, EASE, Fill::Exit));
            }
            self.leaving = Some((now, CROSS_FADE + 0.03));
            return;
        }
        // the capsule fades and blurs away where it is, from wherever its entrance got to
        let from = self.capsule_opacity.at(now);
        self.capsule_opacity.add(Anim::tween(
            from,
            0.0,
            now,
            HIDE_DURATION,
            ARRIVE,
            Fill::Exit,
        ));
        let blur = self.body_blur.at(now);
        self.body_blur.add(Anim::tween(
            blur,
            CONTENT_BLUR,
            now,
            HIDE_DURATION,
            ARRIVE,
            Fill::Exit,
        ));
        self.swing.drag = None;
        if hand_off {
            // the break's bead starts from where this one is, so it holds still
            self.swing.stopped = true;
            self.bead.hide_at(now + FLIGHT_DELAY);
            self.thread_scale
                .add(Anim::tween(1.0, 0.0, now, HIDE_DURATION, EASE, Fill::Exit));
        } else {
            // it swings to rest as it goes, and shrinks back into a bubble
            self.swing.free = false;
            let opacity = self.charm_opacity.at(now);
            self.charm_opacity.add(Anim::tween(
                opacity,
                0.0,
                now,
                HIDE_DURATION,
                EASE,
                Fill::Exit,
            ));
            let scale = self.charm_scale.at(now);
            self.charm_scale.add(Anim::tween(
                scale,
                BUBBLE_FROM,
                now,
                HIDE_DURATION,
                ARRIVE,
                Fill::Exit,
            ));
            let blur = self.bead.blur.at(now);
            self.bead.blur.add(Anim::tween(
                blur,
                BUBBLE_BLUR,
                now,
                HIDE_DURATION,
                ARRIVE,
                Fill::Exit,
            ));
        }
        self.leaving = Some((now, HIDE_DURATION + 0.03));
    }

    pub fn gone(&self, t: f64) -> bool {
        self.leaving.is_some_and(|(at, takes)| t >= at + takes)
    }

    /// whether it changes from frame to frame
    pub fn moving(&self, t: f64) -> bool {
        (self.animate && !self.swing.stopped)
            || self.leaving.is_some()
            || [
                &self.capsule_opacity,
                &self.body_blur,
                &self.charm_opacity,
                &self.charm_scale,
            ]
            .iter()
            .any(|prop| prop.moving(t))
            || self.buttons.iter().any(|b| b.moving(t))
            || self.bead.moving(t)
    }

    pub fn pointer_motion(&mut self, x: f32, y: f32, now: f64) {
        self.pointer = Some((x, y));
        if self.swing.drag.is_some() {
            self.swing.drag_to(x, y, now);
            return;
        }
        for button in &mut self.buttons {
            let over = button.contains(x, y);
            button.hover(over, now);
        }
    }

    pub fn pointer_leave(&mut self, now: f64) {
        self.pointer = None;
        for button in &mut self.buttons {
            button.hover(false, now);
        }
    }

    pub fn press(&mut self, x: f32, y: f32, now: f64) {
        if self.leaving.is_some() {
            return;
        }
        if self.swing.free && self.swing.over_bead(x, y) {
            self.swing.grab(x, y, now);
            return;
        }
        self.pressed = self.buttons.iter().position(|b| b.contains(x, y));
    }

    /// the action of a button pressed and released over it
    pub fn release(&mut self, x: f32, y: f32, now: f64) -> Option<Action> {
        if self.swing.drag.is_some() {
            self.swing.let_go(now);
            return None;
        }
        let pressed = self.pressed.take()?;
        let button = &self.buttons[pressed];
        button.contains(x, y).then_some(button.action)
    }

    /// moves the swing on to `t`
    pub fn advance(&mut self, t: f64) {
        let dt = self
            .last_frame
            .map_or(1.0 / 60.0, |last| (t - last).clamp(0.0, 0.032));
        self.last_frame = Some(t);
        if !self.animate || self.swing.stopped {
            return;
        }
        self.swing.step(dt);
        let pointer = self.pointer.filter(|_| self.leaving.is_none());
        let (cx, cy) = self.swing.bead_center();
        match pointer {
            Some((x, y)) => self.swing.follow(f64::from(x - cx), f64::from(cy - y), dt),
            None => self.swing.follow(0.0, 0.0, dt),
        }
        self.swing.place(&mut self.bead);
    }

    pub fn draw(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let scale = gl.scale();
        self.prepare_text(gl, fonts, scale);
        self.draw_capsule(gl, fonts, t)?;
        self.draw_charm(gl, fonts, t)
    }

    /// the pointer goes through everywhere but the capsule and, while it can be grabbed, the
    /// bead
    pub fn update_input(
        &mut self,
        shell: &Shell,
        qh: &wayland_client::QueueHandle<super::desktop::Desktop>,
    ) {
        let (x, y) = self.swing.bead_center();
        let reach = BEAD / 2.0 + GRAB_SLOP as f32;
        let bead = Rect::new(x - reach, y - reach, 2.0 * reach, 2.0 * reach);
        let grabbable = self.swing.free && self.leaving.is_none();
        let key = if grabbable {
            [bead.x as i32, bead.y as i32, 1, 0]
        } else {
            [0, 0, 0, 0]
        };
        if self.input == Some(key) {
            return;
        }
        self.input = Some(key);
        let mut rects = vec![self.capsule];
        if grabbable {
            rects.push(bead);
        }
        shell.set_input(qh, self.surface, Some(&rects));
    }

    fn prepare_text(&mut self, gl: &Gl, fonts: &mut Fonts, scale: f32) {
        if self
            .texts
            .as_ref()
            .is_none_or(|(s, _)| (s - scale).abs() > 1e-3)
        {
            if let Some((_, old)) = self.texts.take() {
                for text in old {
                    text.texture.delete(gl);
                }
            }
            let render = |fonts: &mut Fonts, text: &str, style| {
                fonts.render(gl, text, style, Fit::LINE, scale)
            };
            self.texts = Some((
                scale,
                [
                    render(fonts, "Time to rest your eyes", HEADING),
                    render(fonts, "Your break begins in", TEXT),
                ],
            ));
        }
        let stale = self
            .count
            .as_ref()
            .is_none_or(|(s, n, _)| (s - scale).abs() > 1e-3 || *n != self.remaining);
        if stale {
            if let Some((_, _, old)) = self.count.take() {
                old.texture.delete(gl);
            }
            let text = fonts.render(
                gl,
                &fmt_warning(self.remaining),
                TEXT.tabular(),
                Fit::LINE,
                scale,
            );
            self.count = Some((scale, self.remaining, text));
        }
    }

    fn draw_capsule(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let opacity = self.capsule_opacity.at(t) as f32;
        if opacity <= 0.002 {
            return Ok(());
        }
        let bounds = self.capsule.inset(-MARGIN).inset(-SHADOW_ROOM / 2.0);
        if opacity < 0.998 {
            let (layer, back) = gl.begin_layer(bounds)?;
            self.capsule_parts(gl, fonts, t)?;
            gl.end_layer(back);
            gl.composite(layer, opacity, 0.0)
        } else {
            self.capsule_parts(gl, fonts, t)
        }
    }

    fn capsule_parts(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let dark = self.look.dark;
        let capsule = self.capsule;
        let shape = if self.look.solid {
            // under the text's own colors, which follow the system's appearance
            let (fill, edge) = if dark { (0.14, 1.0) } else { (0.96, 0.0) };
            Shape {
                fill: [fill, fill, fill, 1.0],
                border: premul([edge, edge, edge, 0.35], 1.0),
                border_width: 1.0,
                radius: CAPSULE_RADIUS,
            }
        } else {
            // where the compositor has a blur, the blur stands in for the glass. otherwise, the
            // capsule is nearly opaque, to stay legible over anything
            let alpha = if self.look.blurred { 0.72 } else { 0.94 };
            let (fill, edge, edge_alpha) = if dark {
                ([0.16, 0.16, 0.17, alpha], 1.0, 0.13)
            } else {
                ([0.97, 0.97, 0.98, alpha], 0.0, 0.10)
            };
            Shape {
                fill: premul(fill, 1.0),
                border: premul([edge, edge, edge, edge_alpha], 1.0),
                border_width: 1.0,
                radius: CAPSULE_RADIUS,
            }
        };
        gl.shadow(
            Rect {
                y: capsule.y + 6.0,
                ..capsule
            },
            CAPSULE_RADIUS,
            12.0,
            premul([0.0, 0.0, 0.0, if dark { 0.35 } else { 0.18 }], 1.0),
        );
        gl.shape(capsule, &shape, 1.0);

        let blur = self.body_blur.at(t) as f32;
        if blur > 0.05 {
            let (layer, back) = gl.begin_layer(capsule.inset(-8.0))?;
            self.body(gl, fonts, t);
            gl.end_layer(back);
            gl.composite(layer, 1.0, blur)
        } else {
            self.body(gl, fonts, t);
            Ok(())
        }
    }

    /// the text and buttons
    fn body(&mut self, gl: &Gl, fonts: &mut Fonts, t: f64) {
        let dark = self.look.dark;
        let label = if dark {
            [1.0, 1.0, 1.0, 0.88]
        } else {
            [0.0, 0.0, 0.0, 0.85]
        };
        let secondary = [label[0], label[1], label[2], 0.5];
        let capsule = self.capsule;
        if let Some((_, [heading, prefix])) = &self.texts {
            let top = capsule.y + PAD;
            gl.image(
                &heading.texture,
                Rect::new(capsule.x + TEXT_INSET, top, heading.width, heading.height),
                premul(label, 1.0),
                [0.0, 0.0, 1.0, 1.0],
            );
            let line = top + HEADING.line_height() + LINE_GAP;
            gl.image(
                &prefix.texture,
                Rect::new(capsule.x + TEXT_INSET, line, prefix.width, prefix.height),
                premul(secondary, 1.0),
                [0.0, 0.0, 1.0, 1.0],
            );
            if let Some((_, _, count)) = &self.count {
                gl.image(
                    &count.texture,
                    Rect::new(self.count_x, line, count.width, count.height),
                    premul(label, 1.0),
                    [0.0, 0.0, 1.0, 1.0],
                );
            }
        }
        for button in &mut self.buttons {
            button.draw(gl, fonts, t, dark, 1.0);
        }
    }

    fn draw_charm(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let opacity = self.charm_opacity.at(t) as f32;
        if opacity <= 0.002 {
            return Ok(());
        }
        let (px, py) = self.swing.pivot;
        let s = self.charm_scale.at(t) as f32;
        let bubble = Affine::scale_about(s, s, px, py);
        let area = Rect::new(px - CHARM_W / 2.0, 0.0, CHARM_W, CHARM_H + 12.0);
        if opacity < 0.998 {
            let (layer, back) = gl.begin_layer(area)?;
            self.charm_parts(gl, fonts, t)?;
            gl.end_layer(back);
            gl.with(bubble, |gl| gl.composite(layer, opacity, 0.0))
        } else {
            gl.with(bubble, |gl| self.charm_parts(gl, fonts, t))
        }
    }

    fn charm_parts(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let (px, py) = self.swing.pivot;
        let length = self.thread_scale.at(t) as f32;
        if length > 0.0 {
            let thread = Rect::new(px - 0.5, py, 1.0, THREAD);
            let hang = Affine::scale_about(1.0, length, px, py).then(Affine::rotate_about(
                -self.swing.shown() as f32,
                px,
                py,
            ));
            gl.with(hang, |gl| {
                // enough of a dark edge for the thread to show over a white window
                gl.shadow(thread, 0.5, 1.0, premul([0.0, 0.0, 0.0, 0.35], 1.0));
                gl.shape(
                    thread,
                    &Shape::filled(premul([1.0, 1.0, 1.0, 0.46], 1.0), 0.5),
                    1.0,
                );
            });
        }
        self.bead.draw(gl, fonts, t)
    }

    pub fn prune(&mut self, t: f64) {
        for prop in [
            &mut self.capsule_opacity,
            &mut self.body_blur,
            &mut self.charm_opacity,
            &mut self.charm_scale,
            &mut self.thread_scale,
        ] {
            prop.prune(t);
        }
        self.bead.prune(t);
    }

    pub fn destroy(mut self, gl: &Gl, shell: &mut Shell) {
        if let Some((_, texts)) = self.texts {
            for text in texts {
                text.texture.delete(gl);
            }
        }
        if let Some((_, _, count)) = self.count {
            count.texture.delete(gl);
        }
        for button in self.buttons {
            button.delete(gl);
        }
        self.bead.delete(gl);
        shell.destroy(self.surface);
    }
}

/// a rounded rectangle as a stack of rectangles, for a region that takes only rectangles
fn rounded_rects(rect: Rect, radius: f32) -> Vec<Rect> {
    let radius = radius.min(rect.w / 2.0).min(rect.h / 2.0);
    let step = 2.0;
    let mut rects = Vec::new();
    let mut y = 0.0;
    while y < radius {
        let dy = radius - (y + step / 2.0);
        let inset = radius - (radius * radius - dy * dy).max(0.0).sqrt();
        for top in [rect.y + y, rect.y + rect.h - y - step] {
            rects.push(Rect::new(rect.x + inset, top, rect.w - 2.0 * inset, step));
        }
        y += step;
    }
    rects.push(Rect::new(
        rect.x,
        rect.y + radius,
        rect.w,
        rect.h - 2.0 * radius,
    ));
    rects
}

struct Drag {
    /// the angle between where it was grabbed and the bead, kept through the drag
    offset: f64,
    /// recent (time, angle) pairs, for the velocity at release
    history: Vec<(f64, f64)>,
}

/// the bead on its thread, in the surface's logical pixels with y down. angles are 0 straight
/// down and positive to the right
struct Swing {
    pivot: (f32, f32),
    /// from the pivot to the bead's center
    length: f64,
    radius: f64,
    /// the angle, either side, at which the bead's top meets the bar
    wall: f64,
    theta: f64,
    omega: f64,
    /// how many pixels the bead's top is pressed flat, and how fast that is changing
    flat: [f64; 2],
    /// how annoyed it is, how fast that is changing, and how much longer it stays annoyed
    annoyed: [f64; 2],
    annoyed_for: f64,
    /// which way the lid slopes, set by the side the bead last hit the bar on
    slope: f64,
    /// lightly damped, while it hangs in a warning
    free: bool,
    /// holds still for good, as the break's bead takes over
    stopped: bool,
    still_for: f64,
    stir_left: bool,
    drag: Option<Drag>,
    /// the look: position and velocity, x and y (up) on their own springs
    look: [f64; 4],
}

impl Swing {
    fn new(pivot: (f32, f32), thread: f32, bead: f32) -> Swing {
        let radius = f64::from(bead) / 2.0;
        let length = f64::from(thread) + radius;
        Swing {
            pivot,
            length,
            radius,
            wall: (radius / length).acos(),
            theta: 0.0,
            omega: 0.0,
            flat: [0.0; 2],
            annoyed: [0.0; 2],
            annoyed_for: 0.0,
            slope: 1.0,
            free: false,
            stopped: false,
            still_for: 0.0,
            stir_left: false,
            drag: None,
            look: [0.0; 4],
        }
    }

    /// the angle the bead shows at. it goes into the bar only as far as its top is
    /// flattened. this keeps the flat on the bar, and the rest of the bead never crosses it
    fn shown(&self) -> f64 {
        let reach = ((self.radius - self.flat[0]) / self.length)
            .clamp(-1.0, 1.0)
            .acos();
        self.theta.clamp(-reach, reach)
    }

    fn bead_center(&self) -> (f32, f32) {
        let (sin, cos) = self.shown().sin_cos();
        (
            self.pivot.0 + (self.length * sin) as f32,
            self.pivot.1 + (self.length * cos) as f32,
        )
    }

    fn over_bead(&self, x: f32, y: f32) -> bool {
        let (cx, cy) = self.bead_center();
        f64::from((x - cx).hypot(y - cy)) <= self.radius + GRAB_SLOP
    }

    /// the swing's angle with the bead under (x, y). a point above the pivot counts as just
    /// below it
    fn angle_at(&self, x: f32, y: f32) -> f64 {
        f64::from(x - self.pivot.0).atan2(f64::from(y - self.pivot.1).max(8.0))
    }

    fn grab(&mut self, x: f32, y: f32, now: f64) {
        let angle = self.angle_at(x, y);
        self.drag = Some(Drag {
            offset: angle - self.theta,
            history: vec![(now, self.theta)],
        });
        self.omega = 0.0;
    }

    fn drag_to(&mut self, x: f32, y: f32, now: f64) {
        let angle = self.angle_at(x, y);
        let wall = self.wall;
        let Some(drag) = &mut self.drag else {
            return;
        };
        let theta = press(angle - drag.offset, wall);
        drag.history.push((now, theta));
        drag.history.retain(|(time, _)| now - time <= 0.10);
        self.theta = theta;
    }

    /// the swing carries on at the speed it was let go
    fn let_go(&mut self, now: f64) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if let (Some(&(t0, a0)), Some(&(t1, a1))) = (drag.history.first(), drag.history.last())
            && t1 > t0
            && now - t1 <= 0.08
        {
            self.omega = ((a1 - a0) / (t1 - t0)).clamp(-8.0, 8.0);
        }
    }

    fn step(&mut self, dt: f64) {
        if self.drag.is_none() {
            let damping = if self.free {
                DAMPING_FREE
            } else {
                DAMPING_SETTLE
            };
            let mut accel = -STIFFNESS * self.theta - damping * self.omega;
            let over = self.theta.abs() - self.wall;
            if over > 0.0 {
                accel -= WALL_STIFFNESS * over * self.theta.signum() + WALL_DAMPING * self.omega;
            }
            self.omega += accel * dt;
            self.theta += self.omega * dt;
            self.stir(dt);
        }
        self.react(dt);
    }

    /// keeps a free bead from hanging dead still: after it hangs still for a while, it swings again
    fn stir(&mut self, dt: f64) {
        if !self.free || self.theta.abs() > STILL_ANGLE || self.omega.abs() > STILL_SPEED {
            self.still_for = 0.0;
            return;
        }
        self.still_for += dt;
        if self.still_for >= STIR_AFTER {
            self.still_for = 0.0;
            self.stir_left = !self.stir_left;
            self.omega = if self.stir_left {
                -STIR_KICK
            } else {
                STIR_KICK
            };
        }
    }

    /// eases the look toward a pointer `dx`, `dy` pixels from the bead, y up
    fn follow(&mut self, dx: f64, dy: f64, dt: f64) {
        let target = if dx == 0.0 && dy == 0.0 {
            [0.0, 0.0]
        } else {
            let distance = dx.hypot(dy).max(1.0);
            let reach = (distance / LOOK_REACH).min(1.0);
            [dx / distance * reach, dy / distance * reach]
        };
        let [x, y, vx, vy] = &mut self.look;
        chase(x, vx, target[0], LOOK, dt);
        chase(y, vy, target[1], LOOK, dt);
    }

    /// flattens the bead's top as far as the swing presses it into the bar, and annoys it
    /// while it is pressed hard
    fn react(&mut self, dt: f64) {
        let goal = pressed(self.theta, self.radius, self.length).min(self.radius * FLAT_MOST);
        let spring = if goal > self.flat[0] { BRISK } else { ROUNDING };
        let [depth, speed] = &mut self.flat;
        chase(depth, speed, goal, spring, dt);
        if goal >= ANNOY_AT {
            self.annoyed_for = ANNOY_HOLD;
            // a bead out to the right has its thread up on its left, and frowns that way
            self.slope = self.theta.signum();
        } else {
            self.annoyed_for = (self.annoyed_for - dt).max(0.0);
        }
        let (goal, spring) = if self.annoyed_for > 0.0 {
            (1.0, BRISK)
        } else {
            (0.0, RAISING)
        };
        let [amount, speed] = &mut self.annoyed;
        chase(amount, speed, goal, spring, dt);
    }

    /// puts the bead where the angle and look say, as flat and annoyed as the bar made it
    fn place(&self, bead: &mut Bead) {
        let (cx, cy) = self.bead_center();
        let r = bead.diameter / 2.0;
        bead.origin = (cx - r, cy - r);
        bead.press(self.flat[0]);
        bead.annoy(self.annoyed[0], self.slope);
        bead.look(self.look[0], self.look[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rounded_region_stays_inside_its_corners() {
        let rect = Rect::new(10.0, 20.0, 200.0, 100.0);
        let rects = rounded_rects(rect, 30.0);
        for r in &rects {
            assert!(r.x >= rect.x - 1e-3 && r.x + r.w <= rect.x + rect.w + 1e-3);
            assert!(r.y >= rect.y - 1e-3 && r.y + r.h <= rect.y + rect.h + 1e-3);
        }
        // the corner's top row is well in from the side, and the middle runs edge to edge
        assert!(rects[0].x > rect.x + 10.0);
        assert_eq!(rects.last().unwrap().w, rect.w);
    }

    #[test]
    fn the_swing_hangs_its_bead_under_the_pivot() {
        let swing = Swing::new((100.0, 0.0), THREAD, BEAD);
        assert_eq!(swing.bead_center(), (100.0, THREAD + BEAD / 2.0));
        assert!(swing.over_bead(100.0, THREAD + 2.0));
        assert!(!swing.over_bead(100.0, 0.0));
        assert!((swing.angle_at(150.0, 50.0) - std::f64::consts::FRAC_PI_4).abs() < 1e-6);
    }

    #[test]
    fn a_kicked_swing_comes_back_to_rest() {
        let mut swing = Swing::new((100.0, 0.0), THREAD, BEAD);
        swing.omega = DROP_KICK;
        for _ in 0..600 {
            swing.step(1.0 / 60.0);
        }
        assert!(swing.theta.abs() < 0.01 && swing.omega.abs() < 0.05);
    }
}
