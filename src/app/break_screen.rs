//! one fullscreen overlay per display in liquid glass style. the warning's mati bead flies to
//! the middle of the display it hung on and counts the break down as its colour drains; the
//! other displays show a bead of their own. the buttons are glass capsules.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadOnly, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSFont, NSFontWeightSemibold, NSImage, NSScreen, NSTextAlignment,
    NSTextField, NSTitlePosition, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::{CGColor, CGImage};
use objc2_core_image::{CIContext, CIImage};
use objc2_foundation::{MainThreadMarker, NSPoint, NSPointInRect, NSRect, NSSize, NSString};
use objc2_quartz_core::{CACurrentMediaTime, CALayer, CAMediaTiming, kCAGravityResizeAspectFill};

use super::bead::Bead;
use super::glass_button::{
    ButtonHost, Buttons, GlassButtonStyle, Shortcut, Surface, make_glass_button,
};
use super::panel::{
    CROSS_FADE, FLIGHT, Fill, LabelStyle, Panel, SETTLE, after, animations_on, arrive, attach_blur,
    blur_out, blur_pulse, blur_radius, detach_blur_after, fade, keep_dark, make_label, make_panel,
    presented, property, scale_about, solid_surfaces, spring, spring_y,
};
use super::pre_break_dialog::{HandOff, hanging_bead};
use crate::queue::Break;

/// dim scrim over the blur, enough to even out a busy desktop into a uniformly dark backdrop
const SCRIM_ALPHA: f64 = 0.32;
const BUTTON_HEIGHT: f64 = 40.0;
/// the bead's diameter, and at most this share of the display's height
const BEAD_SIZE: f64 = 224.0;
const BEAD_SHARE: f64 = 0.28;
/// from the bead up to the baseline of the break's name, and down to the buttons: the same,
/// so the bead sits evenly between them
const BEAD_GAP: f64 = 40.0;
/// the break's name, over the bead
const TITLE_SIZE: f64 = 34.0;
/// the bead's blur as it materialises and as it goes, and at the fastest part of its flight
/// in from the thread
const BEAD_BLUR: f64 = 14.0;
const FLIGHT_BLUR: f64 = 7.0;
/// how long the flight's blur lasts
const FLIGHT_BLUR_DURATION: f64 = 0.5;
/// a bead with no thread to fly from grows from this scale, and every bead shrinks to it
/// as it goes
const BEAD_REST_SCALE: f64 = 0.95;
/// the flight starts this long after the screen is built, keeping its first frames clear of
/// the windows coming up. the warning's bead stays under it until then
pub const FLIGHT_DELAY: f64 = 0.08;
/// the name's blur radius as it leaves
const BLUR_RADIUS: f64 = 16.0;
/// how soft a custom background image is, in points on the display, eyeballed to match the
/// FullScreenUI desktop blur. the image is blurred this much once, into a bitmap
const IMAGE_SOFTNESS: f64 = 16.0;
/// a pixel of that bitmap covers this many points, which a blur that wide hides
const IMAGE_BITMAP_PIXEL: f64 = 2.0;
/// the image enters with this much blur on top, 60 in all, and sharpens to its own softness
/// as the screen settles
const IMAGE_BLUR_ENTRANCE: f64 = 58.0;
/// below 1.0 the dark void bleeds through and darkens the image so text stays legible
const IMAGE_OPACITY: f32 = 0.75;
/// the image, and a bead with no thread to fly from, start to resolve from blur this long
/// after the screen appears
const ENTRANCE_DELAY: f64 = 0.06;
/// and take this long to come into focus
const ENTRANCE: f64 = 0.55;
/// when the screen is in focus, from the moment it appears
const SETTLED: f64 = ENTRANCE_DELAY + ENTRANCE;
/// the name starts its entrance this long after the screen appears, as the bead lands
const LABEL_BEGINS: f64 = 0.36;
/// and the buttons this long
const BUTTONS_BEGIN: f64 = 0.90;
/// each button after the first follows the one before it by this much
const BUTTON_STAGGER: f64 = 0.06;
/// how long a button takes to fade in, after which it takes clicks and its key
const BUTTON_FADE: f64 = 0.24;
/// how far above its place the name starts, and how far below theirs the buttons do
const LABEL_DROP: f64 = 18.0;
const BUTTONS_RISE: f64 = 10.0;
/// the name grows into place from this scale as it sharpens
const LABEL_SCALE: f64 = 0.96;
/// the backdrop takes this long to fade when the break ends
const EXIT_DURATION: f64 = 0.36;
/// the bead, name and buttons start with it and are gone sooner. nothing sharp is left
/// standing on a desktop that's nearly back
const CONTENT_EXIT: f64 = 0.28;
const BUTTON_STYLE: GlassButtonStyle = GlassButtonStyle {
    surface: Surface::Glass,
    height: BUTTON_HEIGHT,
    min_width: 120.0,
    horizontal_padding: 64.0,
    title_font_size: 15.0,
    hint_font_size: 12.0,
};

/// a break screen that `show` put up
pub struct Built {
    /// how long after `show` returned the countdown comes into focus: the entrance, or the
    /// cross-fade under reduced motion
    pub settling: Duration,
    /// whether the break's image loaded, or why not. None when the break has no image
    pub image: Option<Result<(), String>>,
}

pub struct BreakScreen {
    mtm: MainThreadMarker,
    /// one per display while the break shows; empty from the moment `hide` starts
    overlays: Vec<Overlay>,
    /// `animations_on()` at the latest `show`, so the countdown and `hide` animate only if
    /// the entrance did
    animate: bool,
    /// true from the latest `show` until `hide` starts or the overlays are torn down. a
    /// timer from that `show` checks it, since `hide` may have run first.
    up: Rc<Cell<bool>>,
    /// the break's length, which the bead drains against
    total: u64,
}

struct Overlay {
    window: Retained<Panel>,
    /// desktop blur, image and scrim, faded as one on exit
    backdrop: Retained<NSView>,
    /// name and buttons, faded as one on exit
    foreground: Retained<NSView>,
    /// in the foreground. it blurs as it leaves, which the glass buttons beside it can't
    name: Retained<NSTextField>,
    /// over the foreground, with a blur of its own, to fly while the rest resolves
    bead: Bead,
    buttons: Vec<Retained<ButtonHost>>,
}

/// what an overlay's entrance animates, besides the overlay's own views
struct Entrance {
    /// the custom image
    image: Option<Retained<CALayer>>,
    /// where the bead flies in from, in the window, and with the warning's bead, how far its
    /// rings leaned toward the pointer
    arrival: Option<(NSRect, Option<(f64, f64)>)>,
}

impl BreakScreen {
    pub fn new(mtm: MainThreadMarker) -> BreakScreen {
        BreakScreen {
            mtm,
            overlays: Vec::new(),
            animate: true,
            up: Rc::new(Cell::new(false)),
            total: 1,
        }
    }

    /// covers every display with `brk`, counting down from `remaining`. the bead flies in
    /// from `hand_off`, the warning's bead, or else from the menu-bar icon at `anchor`, on
    /// whichever display has it. None when there was no display to show it on.
    pub fn show(
        &mut self,
        brk: &Break,
        remaining: u64,
        buttons: Buttons,
        target: &AnyObject,
        hand_off: Option<HandOff>,
        anchor: Option<NSPoint>,
    ) -> Option<Built> {
        self.teardown_now();
        let mtm = self.mtm;
        // SAFETY: an immutable AppKit constant
        let weight_semibold = unsafe { NSFontWeightSemibold };

        self.animate = animations_on();
        self.total = remaining.max(1);
        let animate = self.animate;
        let solid = solid_surfaces();

        // with no display, sync tries again on its next tick
        let screens = NSScreen::screens(mtm);
        if screens.count() == 0 {
            return None;
        }
        let (image, mut loaded) = if let Some(path) = brk.image.as_deref() {
            match load_image(path) {
                Some(image) => (Some(image), Some(Ok(()))),
                None => (None, Some(Err(cant_show(path)))),
            }
        } else {
            (None, None)
        };

        // every display's views are built first, and only then animated, so the time it
        // takes to build them doesn't come out of the entrance
        let mut entrances = Vec::new();
        for i in 0..screens.count() {
            let screen = screens.objectAtIndex(i);
            let frame = screen.frame();
            let w = frame.size.width;
            let h = frame.size.height;
            let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
            let cx = w / 2.0;
            let cy = h / 2.0;
            let local = |rect: NSRect| {
                NSRect::new(
                    NSPoint::new(
                        rect.origin.x - frame.origin.x,
                        rect.origin.y - frame.origin.y,
                    ),
                    rect.size,
                )
            };
            let on_this = |point: NSPoint| NSPointInRect(point, frame);

            let window = make_panel(mtm, frame, true);
            // white text over a black scrim, in light mode too
            keep_dark(&window);
            let Some(content) = window.contentView() else {
                continue;
            };
            content.setWantsLayer(true);

            let backdrop = NSView::initWithFrame(NSView::alloc(mtm), full);
            backdrop.setWantsLayer(true);
            if solid && let Some(layer) = backdrop.layer() {
                layer.setBackgroundColor(Some(&CGColor::new_generic_gray(0.10, 1.0)));
            }
            content.addSubview(&backdrop);

            let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
            blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            blur.setState(NSVisualEffectState::Active);
            if !solid {
                backdrop.addSubview(&blur);
            }

            // optional custom image, between blur and scrim, blurred into a soft backdrop
            // instead of a sharp photo
            let image_view = image
                .as_deref()
                .filter(|_| !solid)
                .and_then(|image| make_image_view(mtm, image, full));
            if let Some(view) = &image_view {
                backdrop.addSubview(view);
            } else if image.is_some()
                && !solid
                && let Some(path) = brk.image.as_deref()
            {
                // Core Image couldn't soften it
                loaded = Some(Err(cant_show(path)));
            }

            let scrim = NSBox::initWithFrame(NSBox::alloc(mtm), full);
            scrim.setBoxType(NSBoxType::Custom);
            scrim.setTitlePosition(NSTitlePosition::NoTitle);
            scrim.setBorderWidth(0.0);
            scrim.setCornerRadius(0.0);
            scrim.setFillColor(&NSColor::colorWithWhite_alpha(0.0, SCRIM_ALPHA));
            backdrop.addSubview(&scrim);

            // the name and buttons come in one by one and fade out together. only the name
            // blurs, on a layer of its own: a filter over the glass buttons would flatten
            // them and cost a display-sized blur
            let foreground = NSView::initWithFrame(NSView::alloc(mtm), full);
            foreground.setWantsLayer(true);
            content.addSubview(&foreground);

            let msg_w = (w - 80.0).clamp(1.0, 720.0);
            let title_font = NSFont::systemFontOfSize_weight(TITLE_SIZE, weight_semibold);
            let msg_color = NSColor::colorWithWhite_alpha(1.0, 0.92);
            let name_label = make_label(
                mtm,
                &brk.name,
                NSRect::ZERO,
                &LabelStyle {
                    font: &title_font,
                    color: &msg_color,
                    alignment: NSTextAlignment::Center,
                    max_lines: 2,
                },
            );
            let name_h = name_label
                .cell()
                .map_or(64.0, |cell| {
                    cell.cellSizeForBounds(NSRect::new(
                        NSPoint::new(0.0, 0.0),
                        NSSize::new(msg_w, 1000.0),
                    ))
                    .height
                })
                .ceil();

            let gap = 12.0;
            let buttons: Vec<Retained<ButtonHost>> = [
                (buttons.skip, "Skip", sel!(skip:)),
                (buttons.postpone, "Postpone", sel!(postpone:)),
            ]
            .into_iter()
            .filter_map(|(button, title, action)| {
                let key = button?.key.map(Shortcut::Local);
                Some(make_glass_button(
                    mtm,
                    title,
                    key,
                    target,
                    action,
                    &BUTTON_STYLE,
                ))
            })
            .collect();

            // the bead sits in the middle of the display, with the name over it and the
            // buttons under it
            let d = (h * BEAD_SHARE).min(BEAD_SIZE).round();
            let bead_y = (cy - d / 2.0).round();
            // the label's frame reaches a descender below the baseline
            let name_y = (bead_y + d + BEAD_GAP + title_font.descender()).round();
            name_label.setFrame(NSRect::new(
                NSPoint::new(cx - msg_w / 2.0, name_y),
                NSSize::new(msg_w, name_h),
            ));
            foreground.addSubview(&name_label);

            if !buttons.is_empty() {
                let widths: Vec<f64> = buttons.iter().map(|b| b.frame().size.width).collect();
                let total: f64 = widths.iter().sum::<f64>() + gap * (buttons.len() as f64 - 1.0);
                let by = bead_y - BEAD_GAP - BUTTON_HEIGHT;
                let mut x = cx - total / 2.0;
                for (button, bw) in buttons.iter().zip(widths.iter()) {
                    button.setFrame(NSRect::new(
                        NSPoint::new(x, by),
                        NSSize::new(*bw, BUTTON_HEIGHT),
                    ));
                    foreground.addSubview(button);
                    x += bw + gap;
                }
            }
            // a button takes neither clicks nor its shortcut until it shows, so keys typed
            // as the break starts don't skip it
            for button in &buttons {
                button.set_awake(false);
            }

            let bead = Bead::new(mtm, d, screen.backingScaleFactor(), true);
            bead.set_count(&fmt_count(remaining), &spoken_count(remaining), false);
            bead.view.setFrameOrigin(NSPoint::new(cx - d / 2.0, bead_y));
            content.addSubview(&bead.view);

            // under a bar that hides itself the anchor is the display's top edge, which
            // NSPointInRect counts as outside it
            let home = anchor
                .filter(|a| on_this(NSPoint::new(a.x, a.y - 1.0)))
                .map(|a| local(hanging_bead(a)));
            // the warning's bead flies in on its own display; with no warning, a bead drops
            // from the icon; elsewhere one materialises with the screen
            let arrival = hand_off
                .as_ref()
                .filter(|warned| on_this(center(warned.frame)))
                .map(|warned| (local(warned.frame), Some(warned.look)))
                .or(home.map(|r| (r, None)));

            entrances.push(Entrance {
                image: image_view.as_ref().and_then(|view| view.layer()),
                arrival,
            });
            self.overlays.push(Overlay {
                window,
                backdrop,
                foreground,
                name: name_label,
                bead,
                buttons,
            });
        }

        let now = CACurrentMediaTime();
        for (overlay, entrance) in self.overlays.iter().zip(&entrances) {
            if animate {
                overlay.enter(mtm, entrance, now);
            } else {
                // reduced motion: the screen fades up as one, and nothing moves
                let layers = [
                    overlay.backdrop.layer(),
                    overlay.name.layer(),
                    overlay.bead.view.layer(),
                ];
                for layer in layers.into_iter().flatten() {
                    layer.addAnimation_forKey(
                        &fade(0.0, 1.0, now, CROSS_FADE, Fill::Entrance),
                        None,
                    );
                }
            }
            // the buttons come last, one after the other, once the break has settled
            for (i, button) in overlay.buttons.iter().enumerate() {
                let Some(layer) = button.layer() else {
                    continue;
                };
                // no blur: the button is glass, which a filter would flatten
                let begin = now + BUTTONS_BEGIN + i as f64 * BUTTON_STAGGER;
                layer
                    .addAnimation_forKey(&fade(0.0, 1.0, begin, BUTTON_FADE, Fill::Entrance), None);
                if animate {
                    layer.addAnimation_forKey(&spring_y(-BUTTONS_RISE, begin), None);
                }
            }
        }
        for overlay in &self.overlays {
            overlay.window.orderFrontRegardless();
        }

        // the skip and postpone keys go to the key window
        let first = self.overlays.first()?;
        first.window.makeKeyWindow();
        self.up = Rc::new(Cell::new(true));
        // the countdown starts once the screen is in focus, and the bead drains from then
        let settled = if animate { SETTLED } else { CROSS_FADE };
        let left = self.left(remaining);
        for overlay in &self.overlays {
            overlay.bead.drain_to(left, now + settled, animate);
            if animate {
                overlay.bead.drift(now + settled);
            }
        }
        // the buttons wake once they show, with reduced motion as without
        let up = Rc::clone(&self.up);
        let buttons: Vec<_> = self
            .overlays
            .iter()
            .flat_map(|overlay| overlay.buttons.clone())
            .collect();
        let shown = BUTTONS_BEGIN + BUTTON_STAGGER + BUTTON_FADE;
        let delay = (now + shown - CACurrentMediaTime()).max(0.0);
        after(mtm, delay, move || {
            if up.get() {
                for button in &buttons {
                    button.set_awake(true);
                }
            }
        });
        let settling = (now + settled - CACurrentMediaTime()).max(0.0);
        Some(Built {
            settling: Duration::from_secs_f64(settling),
            image: loaded,
        })
    }

    pub fn is_visible(&self) -> bool {
        !self.overlays.is_empty()
    }

    pub fn update_countdown(&self, remaining: u64) {
        let text = fmt_count(remaining);
        let left = self.left(remaining);
        let now = CACurrentMediaTime();
        for overlay in &self.overlays {
            overlay
                .bead
                .set_count(&text, &spoken_count(remaining), self.animate);
            overlay.bead.drain_to(left, now, self.animate);
        }
    }

    /// how much colour the bead aims for while `remaining` shows, which is where the next
    /// second leaves it. that way the colour is gone as the break ends
    fn left(&self, remaining: u64) -> f64 {
        remaining.saturating_sub(1) as f64 / self.total as f64
    }

    pub fn hide(&mut self) {
        if self.overlays.is_empty() {
            return;
        }
        self.up.set(false);
        let animate = self.animate;
        // under reduced motion the screen fades away as one
        let fading = if animate { EXIT_DURATION } else { CROSS_FADE };

        // reverse of the entrance. animations hold their final state so the overlay stays
        // faded/blurred right up to teardown.
        let now = CACurrentMediaTime();
        let overlays = Rc::new(std::mem::take(&mut self.overlays));
        for overlay in overlays.iter() {
            // inert while it fades. a sleeping button takes neither clicks nor its shortcut,
            // and the overlay still catches clicks, so none reach the app behind it
            for button in &overlay.buttons {
                button.set_awake(false);
            }
            let bead = &overlay.bead;
            // everything goes at once, as one move: the bead, name and buttons dissolve
            // where they are while the backdrop lifts off the desktop under them
            let content = if animate { CONTENT_EXIT } else { CROSS_FADE };
            let fades = [
                (overlay.backdrop.layer(), fading),
                (overlay.foreground.layer(), content),
                (bead.view.layer(), content),
            ];
            for (layer, duration) in fades {
                let Some(layer) = layer else {
                    continue;
                };
                let opacity = presented(&layer, "opacity").unwrap_or(1.0);
                layer.addAnimation_forKey(&fade(opacity, 0.0, now, duration, Fill::Exit), None);
            }
            if !animate {
                continue;
            }
            // the blur bites from the first frame and the fade trails it, so each is seen
            // to go soft before it's gone. the bead shrinks a little on the blur's curve
            if let Some(layer) = overlay.name.layer() {
                blur_out(&layer, BLUR_RADIUS, now, CONTENT_EXIT);
                layer.setMasksToBounds(false);
            }
            if let Some(content) = bead.content_layer() {
                let to = scaled(bead.view.frame(), BEAD_REST_SCALE);
                fly_away(&bead.view, to, now, CONTENT_EXIT);
                blur_out(&content, BEAD_BLUR, now, CONTENT_EXIT);
            }
        }

        // once the backdrop has faded the desktop is back, and clicks go through to it
        let faded = Rc::clone(&overlays);
        after(self.mtm, fading, move || {
            for overlay in faded.iter() {
                overlay.window.setIgnoresMouseEvents(true);
            }
        });
        // the overlays live in the timers, so they stay on screen for the animation
        after(self.mtm, fading + 0.05, move || {
            for overlay in overlays.iter() {
                overlay.window.orderOut(None);
            }
        });
    }

    /// orders every window out and forgets the views, with no exit animation. used when
    /// reopening.
    fn teardown_now(&mut self) {
        self.up.set(false);
        for overlay in self.overlays.drain(..) {
            overlay.window.orderOut(None);
        }
    }
}

impl Overlay {
    /// the full entrance, from `now`: the backdrop fades up and the bead leads; the name
    /// comes down onto it as it lands
    fn enter(&self, mtm: MainThreadMarker, entrance: &Entrance, now: f64) {
        let bead = &self.bead;
        if let Some(layer) = self.backdrop.layer() {
            layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.5, Fill::Entrance), None);
        }
        // only the bead's colour blurs. its glass can't, and fades instead
        if let (Some(layer), Some(content)) = (bead.view.layer(), bead.content_layer()) {
            attach_unclipped_blur(&content, 0.0);
            let settled = match entrance.arrival {
                // the bead the warning showed blurs only with its speed. its count comes
                // up as it grows, and its rings ease back from where they were looking
                Some((from, Some((x, y)))) => {
                    let begin = now + FLIGHT_DELAY;
                    content.addAnimation_forKey(
                        &blur_pulse(FLIGHT_BLUR, begin, FLIGHT_BLUR_DURATION),
                        None,
                    );
                    bead.reveal_count(begin, 0.3);
                    bead.look_away_from(x, y, begin, 0.3);
                    fly_in(&bead.view, from, begin)
                }
                Some((from, None)) => {
                    materialise(&layer, &content, now, 0.4);
                    fly_in(&bead.view, from, now)
                }
                None => {
                    let begin = now + ENTRANCE_DELAY;
                    materialise(&layer, &content, begin, ENTRANCE);
                    let from = scaled(bead.view.frame(), BEAD_REST_SCALE);
                    fly_in(&bead.view, from, begin)
                }
            };
            detach_blur_after(mtm, content, settled - now + 0.05);
        }
        bead.flow();
        // the image sharpens to its own softness, and nothing blurs it from then on
        if let Some(layer) = &entrance.image {
            attach_unclipped_blur(layer, 0.0);
            layer.addAnimation_forKey(
                &blur_radius(
                    IMAGE_BLUR_ENTRANCE,
                    0.0,
                    now + ENTRANCE_DELAY,
                    ENTRANCE,
                    Fill::Entrance,
                ),
                None,
            );
            detach_blur_after(mtm, layer.clone(), SETTLED + 0.05);
        }
        // the name comes down onto the bead as it lands, growing a little as it sharpens
        if let Some(layer) = self.name.layer() {
            let begin = now + LABEL_BEGINS;
            layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.3, Fill::Entrance), None);
            let size = self.name.frame().size;
            let middle = NSPoint::new(size.width / 2.0, size.height / 2.0);
            for (key, rest, small) in scale_about(size, layer.anchorPoint(), middle, LABEL_SCALE) {
                // it starts that much above its place as well
                let from = if key == "transform.translation.y" {
                    small + LABEL_DROP
                } else {
                    small
                };
                layer.addAnimation_forKey(
                    &spring(key, from, rest, begin, SETTLE, Fill::Entrance),
                    None,
                );
            }
            attach_unclipped_blur(&layer, 0.0);
            layer.addAnimation_forKey(&blur_radius(14.0, 0.0, begin, 0.5, Fill::Entrance), None);
            detach_blur_after(mtm, layer, LABEL_BEGINS + 0.6);
        }
    }
}

/// a clock-style countdown, with the same format throughout the break
fn fmt_count(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// the count as VoiceOver reads it
fn spoken_count(seconds: u64) -> String {
    let unit = |n: u64, name: &str| format!("{n} {name}{}", if n == 1 { "" } else { "s" });
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{} remaining", unit(s, "second")),
        (m, 0) => format!("{} remaining", unit(m, "minute")),
        (m, s) => format!("{} {} remaining", unit(m, "minute"), unit(s, "second")),
    }
}

fn center(rect: NSRect) -> NSPoint {
    NSPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    )
}

/// `rect` scaled by `factor` about its centre
fn scaled(rect: NSRect, factor: f64) -> NSRect {
    let c = center(rect);
    let size = NSSize::new(rect.size.width * factor, rect.size.height * factor);
    NSRect::new(
        NSPoint::new(c.x - size.width / 2.0, c.y - size.height / 2.0),
        size,
    )
}

/// the scale and the offsets that make `view` look like it fills `rect`, a rect in its
/// superview, as key path, value at rest and value there. it moves on its layer alone, and
/// the view stays put for AppKit.
fn toward(view: &NSView, layer: &CALayer, rect: NSRect) -> [(&'static str, f64, f64); 3] {
    let frame = view.frame();
    let factor = rect.size.width / frame.size.width;
    // the layer scales about its anchor point, which AppKit may have put at a corner
    let anchor = layer.anchorPoint();
    let dx = rect.origin.x - frame.origin.x - (1.0 - factor) * anchor.x * frame.size.width;
    let dy = rect.origin.y - frame.origin.y - (1.0 - factor) * anchor.y * frame.size.height;
    [
        ("transform.scale", 1.0, factor),
        ("transform.translation.x", 0.0, dx),
        ("transform.translation.y", 0.0, dy),
    ]
}

/// springs `view` from looking like it fills `from` to where it sits, starting at `begin`.
/// returns when it has settled.
fn fly_in(view: &NSView, from: NSRect, begin: f64) -> f64 {
    let Some(layer) = view.layer() else {
        return begin;
    };
    let mut settled = begin;
    for (key, rest, there) in toward(view, &layer, from) {
        let anim = spring(key, there, rest, begin, FLIGHT, Fill::Entrance);
        settled = settled.max(begin + anim.duration());
        layer.addAnimation_forKey(&anim, Some(&NSString::from_str(key)));
    }
    settled
}

/// carries `view` from wherever it is on screen to looking like it fills `to`, and holds it
/// there until the window goes
fn fly_away(view: &NSView, to: NSRect, begin: f64, duration: f64) {
    let Some(layer) = view.layer() else {
        return;
    };
    for (key, rest, there) in toward(view, &layer, to) {
        let from = presented(&layer, key).unwrap_or(rest);
        let anim = property(key, from, there, begin, duration, Fill::Exit);
        anim.setTimingFunction(Some(&arrive()));
        layer.addAnimation_forKey(&anim, Some(&NSString::from_str(key)));
    }
}

/// fades the bead in from `begin` as the blur attached to its `content` clears
fn materialise(layer: &CALayer, content: &CALayer, begin: f64, duration: f64) {
    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, duration, Fill::Entrance), None);
    content.addAnimation_forKey(
        &blur_radius(BEAD_BLUR, 0.0, begin, duration, Fill::Entrance),
        None,
    );
}

/// the problem a break reports for an image it can't show
fn cant_show(path: &Path) -> String {
    format!("can't show {} as an image", path.display())
}

/// None when the file isn't an image AppKit can read
fn load_image(path: &Path) -> Option<Retained<CGImage>> {
    let path = NSString::from_str(path.to_str()?);
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &path)?;
    // SAFETY: a null rect asks for the image at its own size, and it needs no context or
    // hints for that
    unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }
}

/// `image`, softened, filling `frame`. None when Core Image can't render it
fn make_image_view(
    mtm: MainThreadMarker,
    image: &CGImage,
    frame: NSRect,
) -> Option<Retained<NSView>> {
    let soft = soften(image, frame.size)?;
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setWantsLayer(true);
    let layer = view.layer()?;
    let contents: &AnyObject = soft.as_ref();
    // SAFETY: a layer's contents can be a CGImage, and the gravity is an immutable Core
    // Animation constant
    unsafe {
        layer.setContents(Some(contents));
        layer.setContentsGravity(kCAGravityResizeAspectFill);
    }
    layer.setOpacity(IMAGE_OPACITY);
    Some(view)
}

/// `image` blurred once, to look `IMAGE_SOFTNESS` points soft when it fills a display of
/// `size`. no filter then has to blur it for as long as the break lasts. the bitmap is only
/// as fine as that softness can show.
fn soften(image: &CGImage, size: NSSize) -> Option<Retained<CGImage>> {
    let width = CGImage::width(Some(image)) as f64;
    let height = CGImage::height(Some(image)) as f64;
    if width < 1.0 || height < 1.0 {
        return None;
    }
    let (shrink, sigma) = bitmap_for(width, height, size);
    // SAFETY: the image is a valid CGImage, and Core Image only reads it. clamping repeats
    // its edge outward, so the blur doesn't fade the edges into nothing
    unsafe {
        let small = CIImage::imageWithCGImage(image).imageByApplyingTransform(CGAffineTransform {
            a: shrink,
            b: 0.0,
            c: 0.0,
            d: shrink,
            tx: 0.0,
            ty: 0.0,
        });
        let blurred = small
            .imageByClampingToExtent()
            .imageByApplyingGaussianBlurWithSigma(sigma);
        CIContext::context().createCGImage_fromRect(&blurred, small.extent())
    }
}

/// how much to shrink a `width` by `height` image for its softened bitmap, and the blur to
/// give that bitmap, in its pixels
fn bitmap_for(width: f64, height: f64, size: NSSize) -> (f64, f64) {
    // points on the display to a pixel of the image, once it fills the display
    let shown = (size.width / width).max(size.height / height);
    let shrink = (shown / IMAGE_BITMAP_PIXEL).min(1.0);
    (shrink, IMAGE_SOFTNESS / shown * shrink)
}

/// `attach_blur`, letting the blur bleed past the layer's edge instead of being clipped
fn attach_unclipped_blur(layer: &CALayer, resting: f64) {
    attach_blur(layer, resting);
    layer.setMasksToBounds(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pupil_keeps_a_clock_format_throughout_the_break() {
        assert_eq!(fmt_count(0), "0:00");
        assert_eq!(fmt_count(15), "0:15");
        assert_eq!(fmt_count(99), "1:39");
        assert_eq!(fmt_count(100), "1:40");
        assert_eq!(fmt_count(300), "5:00");
        assert_eq!(fmt_count(3600), "60:00");
    }

    #[test]
    fn voiceover_reads_the_count_in_words() {
        assert_eq!(spoken_count(0), "0 seconds remaining");
        assert_eq!(spoken_count(1), "1 second remaining");
        assert_eq!(spoken_count(15), "15 seconds remaining");
        assert_eq!(spoken_count(60), "1 minute remaining");
        assert_eq!(spoken_count(125), "2 minutes 5 seconds remaining");
    }

    #[test]
    fn a_large_image_shrinks_to_a_bitmap_of_two_point_pixels() {
        let display = NSSize::new(1440.0, 900.0);
        // twice the display each way: half a point to a pixel, so it shrinks to a quarter
        let (shrink, sigma) = bitmap_for(2880.0, 1800.0, display);
        assert_eq!(shrink, 0.25);
        assert_eq!(sigma, IMAGE_SOFTNESS / IMAGE_BITMAP_PIXEL);
        // a small one is left as it is, and blurred by as many of its pixels as make up
        // the softness
        let (shrink, sigma) = bitmap_for(360.0, 225.0, display);
        assert_eq!(shrink, 1.0);
        assert_eq!(sigma, IMAGE_SOFTNESS / 4.0);
    }

    #[test]
    fn an_image_file_softens_to_a_bitmap() {
        // a one pixel png
        const PNG: [u8; 70] = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96,
            248, 95, 15, 0, 2, 135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        let path = std::env::temp_dir().join(format!("omma-soften-{}.png", std::process::id()));
        std::fs::write(&path, PNG).unwrap();
        let image = load_image(&path);
        std::fs::remove_file(&path).unwrap();
        let soft = soften(&image.unwrap(), NSSize::new(100.0, 50.0)).unwrap();
        assert_eq!(
            (CGImage::width(Some(&soft)), CGImage::height(Some(&soft))),
            (1, 1)
        );
    }

    #[test]
    fn scaling_keeps_the_centre() {
        let r = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(100.0, 100.0));
        let s = scaled(r, 0.5);
        assert_eq!((s.origin.x, s.origin.y, s.size.width), (35.0, 45.0, 50.0));
    }
}
