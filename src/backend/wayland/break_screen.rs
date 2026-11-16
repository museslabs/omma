//! one overlay per display. the warning's bead flies to the middle of the display that it hung
//! on, and counts the break down as its color drains. the other displays show a bead of their
//! own. the buttons are glass capsules.
//!
//! each display gets two surfaces. the backdrop covers all of the display, bars too. the other
//! surface holds the bead, name and buttons over the area that the bars leave. the warning hung
//! in that area too, so the bead's flight starts where the warning's bead was.

use std::path::{Path, PathBuf};

use wayland_client::QueueHandle;
use wayland_client::protocol::wl_output::WlOutput;

use super::anim::{ARRIVE, Anim, Curve, EASE, Fill, Prop};
use super::bead::Bead;
use super::button::{Button, ButtonStyle, Surface, hint};
use super::desktop::{Desktop, Look};
use super::gl::{Affine, Gl, Rect, Shape, Texture, premul};
use super::picture::Pictures;
use super::text::{Alignment, Fit, Fonts, Style, Text, Weight};
use super::warning::{
    BEAD as HANGING_BEAD, CROSS_FADE, FLIGHT_DELAY, HANG_FROM_RIGHT, HandOff, THREAD,
};
use super::wl::{Anchor, KeyboardInteractivity, Layer, Shell, Spec};
use crate::app::format::fmt_count;
use crate::app::{Action, Built, Buttons};
use crate::mati::motion::{FLIGHT, SETTLE};
use crate::queue::Break;

/// over a sharp desktop, the backdrop must be darker than the macOS scrim over its blur.
/// where the compositor blurs, the backdrop is the tint and scrim of the macOS material
const BACKDROP: [f32; 4] = [0.07, 0.07, 0.08, 0.90];
const BACKDROP_BLURRED: [f32; 4] = [0.06, 0.06, 0.07, 0.74];
const BUTTON_HEIGHT: f32 = 40.0;
/// the bead's diameter, and at most this share of the display's height
const BEAD_SIZE: f32 = 224.0;
const BEAD_SHARE: f32 = 0.28;
/// from the bead up to the baseline of the break's name, and down to the buttons
const BEAD_GAP: f32 = 40.0;
/// the break's name, over the bead
const TITLE: Style = Style::new(34.0, Weight::Semibold);
/// the bead's blur as it materializes and as it goes, and at the fastest part of its flight
const BEAD_BLUR: f64 = 14.0;
const FLIGHT_BLUR: f64 = 7.0;
const FLIGHT_BLUR_DURATION: f64 = 0.5;
/// a bead with no thread to fly from grows from this scale, and every bead shrinks to it as
/// it goes
const BEAD_REST_SCALE: f32 = 0.95;
/// the name's blur radius as it leaves
const BLUR_RADIUS: f64 = 16.0;
/// the image enters with this much blur on top, and sharpens to its own softness
const IMAGE_BLUR_ENTRANCE: f64 = 58.0;
/// below 1.0 the dark backdrop bleeds through and darkens the image so text stays legible
const IMAGE_OPACITY: f32 = 0.75;
/// the image, and a bead with no thread to fly from, start to resolve this long after the
/// screen appears. they take this long to come into focus
const ENTRANCE_DELAY: f64 = 0.06;
const ENTRANCE: f64 = 0.55;
const SETTLED: f64 = ENTRANCE_DELAY + ENTRANCE;
/// the name starts its entrance this long after the screen appears, as the bead lands, and
/// the buttons this long
const LABEL_BEGINS: f64 = 0.36;
const BUTTONS_BEGIN: f64 = 0.90;
const BUTTON_STAGGER: f64 = 0.06;
const BUTTON_FADE: f64 = 0.24;
/// how far above its place the name starts, and how far below theirs the buttons do
const LABEL_DROP: f64 = 18.0;
const BUTTONS_RISE: f64 = 10.0;
const LABEL_SCALE: f64 = 0.96;
/// the backdrop takes this long to fade when the break ends, and the bead, name and buttons
/// this long
const EXIT_DURATION: f64 = 0.36;
const CONTENT_EXIT: f64 = 0.28;
const BUTTON_STYLE: ButtonStyle = ButtonStyle {
    surface: Surface::Glass,
    height: BUTTON_HEIGHT,
    min_width: 120.0,
    padding: 74.0,
    title: 15.0,
    hint: 12.0,
};

/// where an overlay's bead comes from
#[derive(Clone, Copy)]
enum Arrival {
    /// the warning's bead, at this rect of the content surface, and the direction it looks in
    Warned(Rect, (f64, f64)),
    /// no warning showed: a bead drops from where the warning's bead hangs
    Dropped(Rect),
    /// another display: one materializes with the screen
    Materialised,
}

pub struct BreakScreen {
    overlays: Vec<Overlay>,
    look: Look,
    animate: bool,
    /// the break's image, softened for each display that shows it. None for solid surfaces
    image: Option<PathBuf>,
    /// the break's length, which the bead drains against, and the count now
    total: u64,
    remaining: u64,
    name: String,
    buttons: Buttons,
    /// when it started to leave, and how long it takes
    pub leaving: Option<(f64, f64)>,
    hand_off: Option<HandOff>,
}

struct Overlay {
    output: WlOutput,
    backdrop: u32,
    content: u32,
    /// when it showed up, which its entrance counts from, and when its buttons wake
    shown_at: f64,
    wake_at: f64,
    /// the display that takes the keyboard, for the shortcuts
    primary: bool,
    arrival: Option<Arrival>,
    /// filled in once the content surface has its size
    scene: Option<Scene>,
    backdrop_opacity: Prop,
    image_blur: Prop,
    /// the softened image, until it is uploaded as `image`
    bitmap: Option<image::RgbaImage>,
    image: Option<Texture>,
}

struct Scene {
    /// the content surface's size that it is laid out for
    size: (u32, u32),
    bead: Bead,
    title: String,
    name: Option<(f32, Text)>,
    name_rect: Rect,
    name_opacity: Prop,
    name_scale: Prop,
    name_drop: Prop,
    name_blur: Prop,
    foreground: Prop,
    buttons: Vec<Button>,
    pressed: Option<usize>,
}

impl BreakScreen {
    /// covers every display with `brk`, and counts down from `remaining`. the bead flies in
    /// from `hand_off`, the warning's bead, on its display, or drops on `home` without one.
    /// None with no display to show it on
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        shell: &mut Shell,
        qh: &QueueHandle<Desktop>,
        pictures: &mut Pictures,
        look: Look,
        brk: &Break,
        remaining: u64,
        buttons: Buttons,
        hand_off: Option<HandOff>,
        home: Option<WlOutput>,
        now: f64,
    ) -> Option<(BreakScreen, Built)> {
        let outputs: Vec<(WlOutput, (u32, u32))> = shell
            .outputs
            .iter()
            .map(|o| (o.wl.clone(), o.logical_size()))
            .collect();
        if outputs.is_empty() {
            return None;
        }
        // the display the bead flies in on takes the keyboard
        let primary = hand_off
            .as_ref()
            .and_then(|h| h.output.clone())
            .or(home)
            .filter(|output| outputs.iter().any(|(o, _)| o == output))
            .unwrap_or_else(|| outputs[0].0.clone());

        // the image is softened for each display's size, as it will fill it. solid surfaces
        // show none
        let image = brk.image.clone().filter(|_| !look.solid);
        let mut image_result = brk.image.as_ref().map(|_| Ok(()));
        let mut overlays = Vec::new();
        for (output, size) in outputs {
            let is_primary = output == primary;
            // the display that the bead flies in on, or drops on, finds out where from once it
            // has its size. on the other displays, a bead materializes
            let arrival = (!is_primary).then_some(Arrival::Materialised);
            let (overlay, shown) = Overlay::open(
                shell,
                qh,
                pictures,
                look,
                image.as_deref(),
                (output, size),
                is_primary,
                arrival,
                now,
            )?;
            if let Some(Err(problem)) = shown {
                image_result = Some(Err(problem));
            }
            overlays.push(overlay);
        }

        let settled = if look.animate { SETTLED } else { CROSS_FADE };
        let screen = BreakScreen {
            overlays,
            look,
            animate: look.animate,
            image,
            total: remaining.max(1),
            remaining,
            name: brk.name.clone(),
            buttons,
            leaving: None,
            hand_off,
        };
        Some((
            screen,
            Built {
                settling: std::time::Duration::from_secs_f64(settled),
                image: image_result,
            },
        ))
    }

    pub fn update_countdown(&mut self, remaining: u64, now: f64) {
        self.remaining = remaining;
        let left = self.left(remaining);
        for scene in self.overlays.iter_mut().filter_map(|o| o.scene.as_mut()) {
            scene
                .bead
                .set_count(&fmt_count(remaining), self.animate, now);
            scene.bead.drain_to(left, now, self.animate);
        }
    }

    /// how much color the bead aims for while `remaining` shows, which is where the next
    /// second leaves it. that way the color is gone as the break ends
    fn left(&self, remaining: u64) -> f64 {
        remaining.saturating_sub(1) as f64 / self.total as f64
    }

    pub fn hide(&mut self, shell: &Shell, now: f64) {
        if self.leaving.is_some() {
            return;
        }
        let animate = self.animate;
        // under reduced motion the screen fades away as one
        let fading = if animate { EXIT_DURATION } else { CROSS_FADE };
        let content = if animate { CONTENT_EXIT } else { CROSS_FADE };
        for overlay in &mut self.overlays {
            // the user's app gets the keyboard back at once
            if overlay.primary {
                shell.set_keyboard(overlay.content, KeyboardInteractivity::None);
            }
            let from = overlay.backdrop_opacity.at(now);
            overlay
                .backdrop_opacity
                .add(Anim::tween(from, 0.0, now, fading, EASE, Fill::Exit));
            let Some(scene) = &mut overlay.scene else {
                continue;
            };
            for button in &mut scene.buttons {
                button.sleep(now);
            }
            scene.pressed = None;
            for prop in [&mut scene.foreground, &mut scene.bead.opacity] {
                let from = prop.at(now);
                prop.add(Anim::tween(from, 0.0, now, content, EASE, Fill::Exit));
            }
            if !animate {
                continue;
            }
            // the blur bites from the first frame and the fade trails it, so each is seen to
            // go soft before it is gone. the bead shrinks a little on the blur's curve
            let blur = scene.name_blur.at(now);
            scene.name_blur.add(Anim::tween(
                blur,
                BLUR_RADIUS,
                now,
                content,
                ARRIVE,
                Fill::Exit,
            ));
            let bead = &mut scene.bead;
            let frame = bead.frame();
            let (cx, cy) = frame.center();
            let to = Rect::new(
                cx - frame.w * BEAD_REST_SCALE / 2.0,
                cy - frame.h * BEAD_REST_SCALE / 2.0,
                frame.w * BEAD_REST_SCALE,
                frame.h * BEAD_REST_SCALE,
            );
            let (scale, x, y) = bead.toward(to);
            for (prop, there) in [
                (&mut bead.scale, scale),
                (&mut bead.shift_x, x),
                (&mut bead.shift_y, y),
            ] {
                let from = prop.at(now);
                prop.add(Anim::tween(from, there, now, content, ARRIVE, Fill::Exit));
            }
            let blur = bead.blur.at(now);
            bead.blur.add(Anim::tween(
                blur,
                BEAD_BLUR,
                now,
                content,
                ARRIVE,
                Fill::Exit,
            ));
        }
        self.leaving = Some((now, fading + 0.05));
    }

    /// whether it has no display left to show on
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    pub fn gone(&self, t: f64) -> bool {
        self.leaving.is_some_and(|(at, takes)| t >= at + takes)
    }

    pub fn surfaces(&self) -> impl Iterator<Item = u32> + '_ {
        self.overlays
            .iter()
            .flat_map(|overlay| [overlay.backdrop, overlay.content])
    }

    /// whether the surface `id` changes from frame to frame at `t`
    pub fn moving(&self, id: u32, t: f64) -> bool {
        if self.leaving.is_some() {
            return true;
        }
        self.overlays.iter().any(|overlay| {
            if overlay.backdrop == id {
                overlay.backdrop_opacity.moving(t) || overlay.image_blur.moving(t)
            } else if overlay.content == id {
                overlay.scene.as_ref().is_none_or(|scene| {
                    scene.bead.moving(t)
                        || [
                            &scene.name_opacity,
                            &scene.name_scale,
                            &scene.name_drop,
                            &scene.name_blur,
                            &scene.foreground,
                        ]
                        .iter()
                        .any(|prop| prop.moving(t))
                        || scene.buttons.iter().any(|b| b.moving(t))
                }) || t < overlay.wake_at
            } else {
                false
            }
        })
    }

    /// whether the surface `id` can draw now. a display's content waits for its backdrop to
    /// show first, which keeps the backdrop under it
    pub fn may_draw(&self, id: u32, shell: &Shell) -> bool {
        self.overlays
            .iter()
            .find(|overlay| overlay.content == id)
            .is_none_or(|overlay| shell.get(overlay.backdrop).is_some_and(|s| s.mapped))
    }

    /// wakes the buttons once they show, so keys typed as the break begins do not skip it
    fn wake(&mut self, t: f64) {
        if self.leaving.is_some() {
            return;
        }
        for overlay in &mut self.overlays {
            let Some(scene) = overlay.scene.as_mut().filter(|_| t >= overlay.wake_at) else {
                continue;
            };
            for button in &mut scene.buttons {
                button.awake = true;
            }
        }
    }

    pub fn draw(
        &mut self,
        id: u32,
        size: (u32, u32),
        gl: &mut Gl,
        fonts: &mut Fonts,
        t: f64,
    ) -> anyhow::Result<()> {
        self.wake(t);
        let Some(index) = self
            .overlays
            .iter()
            .position(|o| o.backdrop == id || o.content == id)
        else {
            return Ok(());
        };
        let (w, h) = (size.0 as f32, size.1 as f32);
        if self.overlays[index].backdrop == id {
            return self.draw_backdrop(index, gl, (w, h), t);
        }
        // a new size, from a resolution or scale change, lays the scene out again. it keeps
        // its timeline, so nothing enters again
        let laid_out = self.overlays[index].scene.as_ref().map(|scene| scene.size);
        if laid_out != Some(size) && (laid_out.is_none() || self.leaving.is_none()) {
            if let Some(mut old) = self.overlays[index].scene.take() {
                old.delete(gl);
            }
            let scene = self.lay_out(index, fonts, size, laid_out.is_none(), t);
            self.overlays[index].scene = Some(scene);
        }
        let Some(scene) = &mut self.overlays[index].scene else {
            return Ok(());
        };
        scene.draw(gl, fonts, t)
    }

    fn draw_backdrop(
        &mut self,
        index: usize,
        gl: &mut Gl,
        (w, h): (f32, f32),
        t: f64,
    ) -> anyhow::Result<()> {
        let blurred = self.look.blurred;
        let solid = self.look.solid;
        let overlay = &mut self.overlays[index];
        let opacity = overlay.backdrop_opacity.at(t) as f32;
        let full = Rect::new(0.0, 0.0, w, h);
        if solid {
            gl.shape(full, &Shape::filled([0.10, 0.10, 0.10, 1.0], 0.0), opacity);
            return Ok(());
        }
        let base = if blurred { BACKDROP_BLURRED } else { BACKDROP };
        if let Some(bitmap) = overlay.bitmap.take() {
            overlay.image =
                Some(gl.upload(bitmap.width(), bitmap.height(), bitmap.as_raw(), false));
        }
        let Some(image) = &overlay.image else {
            gl.shape(full, &Shape::filled(premul(base, 1.0), 0.0), opacity);
            return Ok(());
        };
        // the image fills the display, cropped to keep its shape, over the dark backdrop and
        // under a scrim that evens it out
        let (iw, ih) = (image.width as f32, image.height as f32);
        let fill = (w / iw).max(h / ih);
        let shown = Rect::new(
            (w - iw * fill) / 2.0,
            (h - ih * fill) / 2.0,
            iw * fill,
            ih * fill,
        );
        let draw = |gl: &mut Gl| -> anyhow::Result<()> {
            gl.shape(full, &Shape::filled([0.0, 0.0, 0.0, 1.0], 0.0), 1.0);
            let blur = overlay.image_blur.at(t) as f32;
            if blur > 0.05 {
                let (layer, back) = gl.begin_layer(full)?;
                gl.image(image, shown, [1.0; 4], [0.0, 0.0, 1.0, 1.0]);
                gl.end_layer(back);
                gl.composite(layer, IMAGE_OPACITY, blur)?;
            } else {
                gl.image(image, shown, [IMAGE_OPACITY; 4], [0.0, 0.0, 1.0, 1.0]);
            }
            gl.shape(full, &Shape::filled([0.0, 0.0, 0.0, 0.32], 0.0), 1.0);
            Ok(())
        };
        if opacity < 0.998 {
            let (layer, back) = gl.begin_layer(full)?;
            draw(gl)?;
            gl.end_layer(back);
            gl.composite(layer, opacity, 0.0)
        } else {
            draw(gl)
        }
    }

    /// lays out a display's bead, name and buttons for its content surface's `size`. with
    /// `entering`, they enter, from when the display showed up. without it, they show as they
    /// are at `t`, on the same timeline
    fn lay_out(
        &mut self,
        index: usize,
        fonts: &mut Fonts,
        size: (u32, u32),
        entering: bool,
        t: f64,
    ) -> Scene {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let now = self.overlays[index].shown_at;
        let awake = t >= self.overlays[index].wake_at;
        let animate = self.animate;
        let (cx, cy) = (w / 2.0, h / 2.0);
        let d = (h * BEAD_SHARE).min(BEAD_SIZE).round();
        let bead_y = (cy - d / 2.0).round();
        let mut bead = Bead::new(d, true, self.look.solid);
        bead.origin = ((cx - d / 2.0).round(), bead_y);
        bead.set_count(&fmt_count(self.remaining), false, now);

        let msg_w = (w - 80.0).clamp(1.0, 720.0);
        let name_rect = Rect::new(cx - msg_w / 2.0, 0.0, msg_w, 0.0);

        let mut buttons: Vec<Button> = [
            (self.buttons.skip, "Skip", Action::Skip),
            (self.buttons.postpone, "Postpone", Action::Postpone),
        ]
        .into_iter()
        .filter_map(|(button, title, action)| {
            let key = button?.key;
            Some(Button::new(
                fonts,
                title,
                key,
                key.map(hint),
                action,
                BUTTON_STYLE,
            ))
        })
        .collect();
        let gap = 12.0;
        let total: f32 = buttons.iter().map(|b| b.rect.w).sum::<f32>()
            + gap * (buttons.len() as f32 - 1.0).max(0.0);
        let mut x = (cx - total / 2.0).round();
        for (i, button) in buttons.iter_mut().enumerate() {
            button.rect.x = x;
            button.rect.y = bead_y + d + BEAD_GAP;
            x += button.rect.w + gap;
            // a button takes neither clicks nor its shortcut until it shows
            button.awake = awake;
            if !entering {
                continue;
            }
            let begin = now + BUTTONS_BEGIN + i as f64 * BUTTON_STAGGER;
            button.opacity.add(Anim::tween(
                0.0,
                1.0,
                begin,
                BUTTON_FADE,
                EASE,
                Fill::Entrance,
            ));
            if animate {
                button.drop.add(Anim::spring(
                    BUTTONS_RISE,
                    0.0,
                    begin,
                    SETTLE,
                    Fill::Entrance,
                ));
            }
        }

        let mut scene = Scene {
            size,
            bead,
            title: self.name.clone(),
            name: None,
            name_rect,
            name_opacity: Prop::new(1.0),
            name_scale: Prop::new(1.0),
            name_drop: Prop::new(0.0),
            name_blur: Prop::new(0.0),
            foreground: Prop::new(1.0),
            buttons,
            pressed: None,
        };

        let settled = if animate { SETTLED } else { CROSS_FADE };
        let overlay = &self.overlays[index];
        let arrival = overlay.arrival.unwrap_or_else(|| match &self.hand_off {
            Some(warned) => {
                // the warning hung at the top right of this same area
                let dx = w - warned.surface_width;
                Arrival::Warned(
                    Rect {
                        x: warned.rect.x + dx,
                        ..warned.rect
                    },
                    warned.look,
                )
            }
            None => {
                let x = w - HANG_FROM_RIGHT;
                Arrival::Dropped(Rect::new(
                    x - HANGING_BEAD / 2.0,
                    THREAD,
                    HANGING_BEAD,
                    HANGING_BEAD,
                ))
            }
        });
        let bead = &mut scene.bead;
        let left = self.left(self.remaining);
        if !entering {
            // the drift and the wave go on from where they were, and stay still under reduced
            // motion
            if animate {
                bead.flow(now);
                bead.drift(now + settled);
            }
            bead.drain_to(left, t, false);
            return scene;
        }
        if animate {
            match arrival {
                // the bead the warning showed blurs only with its speed. its count comes up as
                // it grows, and its rings ease back from where they were looking
                Arrival::Warned(from, (x, y)) => {
                    let begin = now + FLIGHT_DELAY;
                    bead.blur.add(Anim::keys(
                        &[0.0, FLIGHT_BLUR, 0.0],
                        &[0.0, 0.3, 1.0],
                        &[Curve::Linear, Curve::Linear],
                        begin,
                        FLIGHT_BLUR_DURATION,
                        Fill::Entrance,
                    ));
                    bead.reveal_count(begin, 0.3);
                    bead.look_away_from(x, y, begin, 0.3);
                    fly_in(bead, from, begin);
                }
                Arrival::Dropped(from) => {
                    materialise(bead, now, 0.4);
                    fly_in(bead, from, now);
                }
                Arrival::Materialised => {
                    let begin = now + ENTRANCE_DELAY;
                    materialise(bead, begin, ENTRANCE);
                    let frame = bead.frame();
                    let (bx, by) = frame.center();
                    let small = frame.w * BEAD_REST_SCALE;
                    fly_in(
                        bead,
                        Rect::new(bx - small / 2.0, by - small / 2.0, small, small),
                        begin,
                    );
                }
            }
            bead.flow(now);
            bead.drift(now + settled);
            // the name comes down onto the bead as it lands, and grows a little as it sharpens
            let begin = now + LABEL_BEGINS;
            scene
                .name_opacity
                .add(Anim::tween(0.0, 1.0, begin, 0.3, EASE, Fill::Entrance));
            scene.name_scale.add(Anim::spring(
                LABEL_SCALE,
                1.0,
                begin,
                SETTLE,
                Fill::Entrance,
            ));
            scene.name_drop.add(Anim::spring(
                -LABEL_DROP,
                0.0,
                begin,
                SETTLE,
                Fill::Entrance,
            ));
            scene
                .name_blur
                .add(Anim::tween(14.0, 0.0, begin, 0.5, EASE, Fill::Entrance));
        } else {
            // reduced motion: the screen fades up as one, and nothing moves
            for prop in [&mut bead.opacity, &mut scene.name_opacity] {
                prop.add(Anim::tween(0.0, 1.0, now, CROSS_FADE, EASE, Fill::Entrance));
            }
        }
        scene.bead.drain_to(left, now + settled, animate);
        scene
    }

    pub fn pointer_motion(&mut self, id: u32, x: f32, y: f32, now: f64) {
        for scene in self.scenes_on(id) {
            for button in &mut scene.buttons {
                let over = button.contains(x, y);
                button.hover(over, now);
            }
        }
    }

    pub fn pointer_leave(&mut self, id: u32, now: f64) {
        for scene in self.scenes_on(id) {
            for button in &mut scene.buttons {
                button.hover(false, now);
            }
        }
    }

    pub fn press(&mut self, id: u32, x: f32, y: f32) {
        for scene in self.scenes_on(id) {
            scene.pressed = scene.buttons.iter().position(|b| b.contains(x, y));
        }
    }

    pub fn release(&mut self, id: u32, x: f32, y: f32) -> Option<Action> {
        self.scenes_on(id).find_map(|scene| {
            let pressed = scene.pressed.take()?;
            let button = &scene.buttons[pressed];
            button.contains(x, y).then_some(button.action)
        })
    }

    /// the action of an awake button whose shortcut `key` is
    pub fn key(&self, key: crate::config::KeySpec) -> Option<Action> {
        self.overlays
            .iter()
            .filter_map(|overlay| overlay.scene.as_ref())
            .flat_map(|scene| &scene.buttons)
            .find(|button| button.awake && button.key == Some(key))
            .map(|button| button.action)
    }

    fn scenes_on(&mut self, id: u32) -> impl Iterator<Item = &mut Scene> {
        self.overlays
            .iter_mut()
            .filter(move |overlay| overlay.content == id)
            .filter_map(|overlay| overlay.scene.as_mut())
    }

    /// brings the overlays in line with the displays: drops those whose display went away or
    /// whose surfaces the compositor closed, and covers displays that came while the break
    /// shows. if the display with the keyboard went away, another one takes it
    pub fn reconcile(
        &mut self,
        shell: &mut Shell,
        qh: &QueueHandle<Desktop>,
        pictures: &mut Pictures,
        gl: &Gl,
        now: f64,
    ) {
        self.overlays.retain_mut(|overlay| {
            let gone = !shell.outputs.iter().any(|o| o.wl == overlay.output)
                || [overlay.backdrop, overlay.content]
                    .iter()
                    .any(|&id| shell.get(id).is_none_or(|s| s.closed));
            if gone {
                overlay.release(gl);
                shell.destroy(overlay.backdrop);
                shell.destroy(overlay.content);
            }
            !gone
        });
        if self.leaving.is_some() || self.overlays.is_empty() {
            return;
        }
        let uncovered: Vec<_> = shell
            .outputs
            .iter()
            .filter(|o| !self.overlays.iter().any(|overlay| overlay.output == o.wl))
            .map(|o| (o.wl.clone(), o.logical_size()))
            .collect();
        for output in uncovered {
            let opened = Overlay::open(
                shell,
                qh,
                pictures,
                self.look,
                self.image.as_deref(),
                output,
                false,
                Some(Arrival::Materialised),
                now,
            );
            if let Some((overlay, _)) = opened {
                self.overlays.push(overlay);
            }
        }
        if !self.overlays.iter().any(|overlay| overlay.primary) {
            let overlay = &mut self.overlays[0];
            overlay.primary = true;
            shell.set_keyboard(overlay.content, KeyboardInteractivity::Exclusive);
        }
    }

    pub fn prune(&mut self, t: f64) {
        for overlay in &mut self.overlays {
            overlay.backdrop_opacity.prune(t);
            overlay.image_blur.prune(t);
            if let Some(scene) = &mut overlay.scene {
                scene.bead.prune(t);
                for prop in [
                    &mut scene.name_opacity,
                    &mut scene.name_scale,
                    &mut scene.name_drop,
                    &mut scene.name_blur,
                    &mut scene.foreground,
                ] {
                    prop.prune(t);
                }
            }
        }
    }

    pub fn destroy(mut self, gl: &Gl, shell: &mut Shell) {
        for mut overlay in self.overlays.drain(..) {
            overlay.release(gl);
            shell.destroy(overlay.backdrop);
            shell.destroy(overlay.content);
        }
    }
}

impl Overlay {
    /// a backdrop over all of `output`, bars too, and a content surface over the area that the
    /// bars leave, which takes the keyboard when `primary`. the backdrop fades in from `now`.
    /// the second value says whether the image showed, when there is one
    #[allow(clippy::too_many_arguments)]
    fn open(
        shell: &mut Shell,
        qh: &QueueHandle<Desktop>,
        pictures: &mut Pictures,
        look: Look,
        image: Option<&Path>,
        (output, size): (WlOutput, (u32, u32)),
        primary: bool,
        arrival: Option<Arrival>,
        now: f64,
    ) -> Option<(Overlay, Option<Result<(), String>>)> {
        let mut shown = None;
        let bitmap = image.and_then(|path| {
            let soft = pictures.softened(path, size);
            shown = Some(
                soft.as_ref()
                    .map(|_| ())
                    .ok_or_else(|| format!("can't show {} as an image", path.display())),
            );
            soft
        });
        let backdrop = shell.create(
            qh,
            Spec {
                output: Some(output.clone()),
                layer: Layer::Overlay,
                anchor: Anchor::all(),
                size: (0, 0),
                exclusive_zone: -1,
                keyboard: KeyboardInteractivity::None,
            },
        )?;
        let content = shell.create(
            qh,
            Spec {
                output: Some(output.clone()),
                layer: Layer::Overlay,
                anchor: Anchor::all(),
                size: (0, 0),
                // in the area the bars leave, as the warning was
                exclusive_zone: 0,
                keyboard: if primary {
                    KeyboardInteractivity::Exclusive
                } else {
                    KeyboardInteractivity::None
                },
            },
        )?;
        if shell.can_blur() && !look.solid {
            shell.set_blur(qh, backdrop, &[Rect::new(0.0, 0.0, 100_000.0, 100_000.0)]);
        }
        let mut backdrop_opacity = Prop::new(1.0);
        let duration = if look.animate { 0.5 } else { CROSS_FADE };
        backdrop_opacity.add(Anim::tween(0.0, 1.0, now, duration, EASE, Fill::Entrance));
        let mut image_blur = Prop::new(0.0);
        if look.animate {
            image_blur.add(Anim::tween(
                IMAGE_BLUR_ENTRANCE,
                0.0,
                now + ENTRANCE_DELAY,
                ENTRANCE,
                EASE,
                Fill::Entrance,
            ));
        }
        let overlay = Overlay {
            output,
            backdrop,
            content,
            shown_at: now,
            wake_at: now + BUTTONS_BEGIN + BUTTON_STAGGER + BUTTON_FADE,
            primary,
            arrival,
            scene: None,
            backdrop_opacity,
            image_blur,
            bitmap,
            image: None,
        };
        Some((overlay, shown))
    }

    /// lets go of its textures
    fn release(&mut self, gl: &Gl) {
        if let Some(image) = self.image.take() {
            image.delete(gl);
        }
        if let Some(mut scene) = self.scene.take() {
            scene.delete(gl);
        }
    }
}

impl Scene {
    /// deletes its textures
    fn delete(&mut self, gl: &Gl) {
        self.bead.delete(gl);
        if let Some((_, name)) = self.name.take() {
            name.texture.delete(gl);
        }
        for button in self.buttons.drain(..) {
            button.delete(gl);
        }
    }

    fn draw(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let foreground = self.foreground.at(t) as f32;
        let scale = gl.scale();
        if self
            .name
            .as_ref()
            .is_none_or(|(s, _)| (s - scale).abs() > 1e-3)
        {
            if let Some((_, old)) = self.name.take() {
                old.texture.delete(gl);
            }
            let fit = Fit {
                width: Some(self.name_rect.w),
                lines: 2,
                alignment: Alignment::Center,
            };
            let text = fonts.render(gl, &self.title, TITLE, fit, scale);
            self.name = Some((scale, text));
        }
        let Some((_, name)) = &self.name else {
            return Ok(());
        };
        // the name's last line sits its baseline a gap above the bead
        let bead = self.bead.frame();
        let top = bead.y - BEAD_GAP - name.last_baseline;
        let rect = Rect::new(self.name_rect.x, top.round(), name.width, name.height);
        let opacity = self.name_opacity.at(t) as f32 * foreground;
        if opacity > 0.002 {
            let (mx, my) = rect.center();
            let s = self.name_scale.at(t) as f32;
            let moved = Affine::scale_about(s, s, mx, my)
                .then(Affine::translate(0.0, self.name_drop.at(t) as f32));
            let blur = self.name_blur.at(t) as f32;
            let white = premul([1.0, 1.0, 1.0, 0.92], 1.0);
            if blur > 0.05 {
                let (layer, back) = gl.begin_layer(rect.inset(-3.0 * blur))?;
                gl.image(&name.texture, rect, white, [0.0, 0.0, 1.0, 1.0]);
                gl.end_layer(back);
                gl.with(moved, |gl| gl.composite(layer, opacity, blur))?;
            } else {
                gl.with(moved, |gl| {
                    gl.image(
                        &name.texture,
                        rect,
                        premul([1.0, 1.0, 1.0, 0.92], opacity),
                        [0.0, 0.0, 1.0, 1.0],
                    );
                });
            }
        }
        for button in &mut self.buttons {
            button.draw(gl, fonts, t, true, foreground);
        }
        self.bead.draw(gl, fonts, t)
    }
}

/// springs the bead from looking like it fills `from` to where it sits, from `begin`
fn fly_in(bead: &mut Bead, from: Rect, begin: f64) {
    let (scale, x, y) = bead.toward(from);
    bead.scale
        .add(Anim::spring(scale, 1.0, begin, FLIGHT, Fill::Entrance));
    bead.shift_x
        .add(Anim::spring(x, 0.0, begin, FLIGHT, Fill::Entrance));
    bead.shift_y
        .add(Anim::spring(y, 0.0, begin, FLIGHT, Fill::Entrance));
}

/// fades the bead in from `begin` as its pigment comes out of blur
fn materialise(bead: &mut Bead, begin: f64, duration: f64) {
    bead.opacity
        .add(Anim::tween(0.0, 1.0, begin, duration, EASE, Fill::Entrance));
    bead.blur.add(Anim::tween(
        BEAD_BLUR,
        0.0,
        begin,
        duration,
        EASE,
        Fill::Entrance,
    ));
}
