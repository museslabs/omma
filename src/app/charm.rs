//! the warning's bead on its thread, hanging off the menu-bar icon. it swings like a
//! pendulum on a display link: grab it and it follows the pointer, let go and the swing keeps
//! the release velocity. its inner rings lean toward the pointer, as if it watched you.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSEvent, NSView};
use objc2_core_foundation::{CGAffineTransform, CGSize};
use objc2_core_graphics::CGColor;
use objc2_foundation::{
    MainThreadMarker, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString,
};
use objc2_quartz_core::{CADisplayLink, CALayer};

use super::bead::Bead;
use super::panel::{
    Fill, POP, arrive, attach_blur, blur_out, blur_radius, detach_blur_after, fade, presented,
    property, scale_about, spring, without_actions,
};

/// ω² of the swing, per second squared
const STIFFNESS: f64 = 42.0;
/// light while it hangs in the warning, letting a swing ring on; heavy as it settles on the
/// warning's way out
const DAMPING_FREE: f64 = 1.7;
const DAMPING_SETTLE: f64 = 14.0;
/// the swing it drops in with, in radians per second: about 18° to one side
const DROP_KICK: f64 = 2.0;
/// left hanging still this long, it stirs again with a smaller swing, to the other side
/// each time
const STIR_AFTER: f64 = 3.5;
const STIR_KICK: f64 = 1.1;
/// below this angle and speed it counts as hanging still
const STILL_ANGLE: f64 = 0.035;
const STILL_SPEED: f64 = 0.2;
/// the charm swells in from this scale like a bubble, thread and bead as one, and shrinks back
/// to it. the bead's colour blurs this much as it does
const BUBBLE_FROM: f64 = 0.95;
const BUBBLE_BLUR: f64 = 8.0;
/// past this angle a drag meets more and more resistance instead of a hard stop
const SWING_LIMIT: f64 = 1.15;
/// how much of a grab around the bead counts, beyond its edge
const GRAB_SLOP: f64 = 6.0;
/// the look's springs: stiffness and damping, critically damped
const LOOK_STIFFNESS: f64 = 190.0;
const LOOK_DAMPING: f64 = 27.0;
/// the pointer this far from the bead, or further, makes it look all the way over
const LOOK_REACH: f64 = 140.0;

pub struct Swing {
    bead: Bead,
    thread: Retained<CALayer>,
    /// where the thread hangs from, in the view
    pivot: NSPoint,
    /// from the pivot to the bead's centre
    length: f64,
    theta: f64,
    omega: f64,
    /// lightly damped, while it hangs in a warning
    free: bool,
    /// how long it has hung still, and which way the last stir went
    still_for: f64,
    stir_left: bool,
    drag: Option<Drag>,
    /// whether its window takes clicks now: only with the pointer on the bead, or in a drag.
    /// the rest of the time they go through to whatever is under the window
    takes_clicks: bool,
    last_frame: Option<f64>,
    /// the look: position and velocity, x and y on their own springs
    look: [f64; 4],
    link: Option<Retained<CADisplayLink>>,
}

struct Drag {
    /// the angle between where it was grabbed and the bead, kept through the drag
    offset: f64,
    /// recent (time, angle) pairs, for the velocity at release
    history: Vec<(f64, f64)>,
}

define_class!(
    // SAFETY: NSView can be subclassed. each override has the signature AppKit declares,
    // which registering the class checks, and `step:` is the display link's callback.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaCharm"]
    #[ivars = RefCell<Swing>]
    pub struct Charm;

    impl Charm {
        /// the warning never takes focus, so the first click has to count
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        /// the bead is the only thing to grab, and the charm takes the grab itself rather than
        /// the glass inside the bead
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            // the point is in the superview's coordinates. the charm is a window's content
            // view, whose superview has the window's own coordinates
            let point = self.convertPoint_fromView(point, None);
            if self.ivars().borrow().over_bead(point) {
                let this: &NSView = self;
                std::ptr::from_ref(this).cast_mut()
            } else {
                std::ptr::null_mut()
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut swing = self.ivars().borrow_mut();
            if !swing.free || !swing.over_bead(point) {
                return;
            }
            let angle = swing.angle_at(point);
            swing.drag = Some(Drag {
                offset: angle - swing.theta,
                history: vec![(event.timestamp(), swing.theta)],
            });
            swing.omega = 0.0;
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mut swing = self.ivars().borrow_mut();
            let angle = swing.angle_at(point);
            let Some(drag) = &mut swing.drag else {
                return;
            };
            let theta = rubber_band(angle - drag.offset);
            drag.history.push((event.timestamp(), theta));
            drag.history.retain(|(time, _)| event.timestamp() - time <= 0.10);
            swing.theta = theta;
            swing.place();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            let mut swing = self.ivars().borrow_mut();
            let Some(drag) = swing.drag.take() else {
                return;
            };
            // the swing carries on at the speed it was let go
            if let (Some(&(t0, a0)), Some(&(t1, a1))) = (drag.history.first(), drag.history.last())
                && t1 > t0
                && event.timestamp() - t1 <= 0.08
            {
                swing.omega = ((a1 - a0) / (t1 - t0)).clamp(-8.0, 8.0);
            }
        }

        #[unsafe(method(step:))]
        fn step(&self, link: &CADisplayLink) {
            let window = self.window();
            let screen_origin = window.as_ref().map(|w| w.frame().origin);
            let mut swing = self.ivars().borrow_mut();
            let now = link.timestamp();
            let dt = swing
                .last_frame
                .map_or(1.0 / 60.0, |last| (now - last).clamp(0.0, 0.032));
            swing.last_frame = Some(now);
            if swing.drag.is_none() {
                let damping = if swing.free {
                    DAMPING_FREE
                } else {
                    DAMPING_SETTLE
                };
                swing.omega += (-STIFFNESS * swing.theta - damping * swing.omega) * dt;
                swing.theta += swing.omega * dt;
                swing.stir(dt);
            }
            let mut takes_clicks = swing.drag.is_some();
            if let Some(origin) = screen_origin {
                let mouse = NSEvent::mouseLocation();
                let pointer = NSPoint::new(mouse.x - origin.x, mouse.y - origin.y);
                takes_clicks |= swing.free && swing.over_bead(pointer);
                let center = swing.bead_center();
                swing.follow(pointer.x - center.x, pointer.y - center.y, dt);
            }
            swing.place();
            let changed = takes_clicks != swing.takes_clicks;
            swing.takes_clicks = takes_clicks;
            drop(swing);
            if changed && let Some(window) = window {
                window.setIgnoresMouseEvents(!takes_clicks);
            }
        }
    }
);

impl Charm {
    /// a `size` view with the thread hanging from its top centre, `thread_length` long, and a
    /// bead of `bead` diameter on its end. `scale` is the backing scale of the display it's on
    pub fn new(
        mtm: MainThreadMarker,
        size: NSSize,
        thread_length: f64,
        bead: f64,
        scale: f64,
    ) -> Retained<Charm> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
        let pivot = NSPoint::new(size.width / 2.0, size.height);

        // the thread is a plain layer in a layer-hosting view of its own, under the bead
        let thread_root = CALayer::new();
        let thread_host = NSView::initWithFrame(NSView::alloc(mtm), frame);
        thread_host.setLayer(Some(&thread_root));
        thread_host.setWantsLayer(true);
        let thread = CALayer::new();
        thread.setBounds(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(1.0, thread_length),
        ));
        thread.setAnchorPoint(NSPoint::new(0.5, 1.0));
        thread.setPosition(pivot);
        thread.setBackgroundColor(Some(&CGColor::new_generic_gray(1.0, 0.46)));
        thread.setShadowColor(Some(&CGColor::new_generic_gray(0.0, 1.0)));
        // enough of a dark edge for the thread to show over a white window
        thread.setShadowOpacity(0.35);
        thread.setShadowRadius(1.0);
        thread.setShadowOffset(CGSize::new(0.0, 0.0));
        thread_root.addSublayer(&thread);

        let swing = Swing {
            bead: Bead::new(mtm, bead, scale, false),
            thread,
            pivot,
            length: thread_length + bead / 2.0,
            theta: 0.0,
            omega: 0.0,
            free: false,
            still_for: 0.0,
            stir_left: false,
            drag: None,
            takes_clicks: false,
            last_frame: None,
            look: [0.0; 4],
            link: None,
        };
        let this = Charm::alloc(mtm).set_ivars(RefCell::new(swing));
        // SAFETY: NSView's designated initializer, with the argument type it declares
        let charm: Retained<Charm> = unsafe { msg_send![super(this), initWithFrame: frame] };
        charm.setWantsLayer(true);
        charm.addSubview(&thread_host);
        {
            let swing = charm.ivars().borrow();
            charm.addSubview(&swing.bead.view);
            swing.place();
        }
        charm
    }

    /// starts the swing on the display the window is on, dropping in with a `DROP_KICK`
    /// swing. without `animate` it hangs still and can't be grabbed, for reduced motion. its
    /// window should start out ignoring the mouse: the swing makes it take clicks while the
    /// pointer is on the bead
    pub fn start(&self, animate: bool) {
        if !animate {
            return;
        }
        let target: &AnyObject = self;
        // SAFETY: `step:` takes the display link. the link retains this view until `stop`
        // invalidates it, which `PreBreakDialog::hide` always gets to
        let link = unsafe { self.displayLinkWithTarget_selector(target, sel!(step:)) };
        // SAFETY: the mode is an immutable Foundation constant
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        let mut swing = self.ivars().borrow_mut();
        swing.link = Some(link);
        swing.free = true;
        swing.omega = DROP_KICK;
        swing.bead.blink();
    }

    /// lets go of the bead for good: it can't be grabbed, and swings to rest under heavy
    /// damping until `stop`
    pub fn settle(&self) {
        let mut swing = self.ivars().borrow_mut();
        swing.drag = None;
        swing.free = false;
    }

    /// stops the display link, which also lets go of this view. the bead stays where it was
    pub fn stop(&self) {
        let mut swing = self.ivars().borrow_mut();
        if let Some(link) = swing.link.take() {
            link.invalidate();
        }
        swing.drag = None;
        swing.free = false;
    }

    /// what the charm animates to swell from where it hangs, or shrink back there: key path,
    /// value at rest and value when small
    fn bubble(&self, layer: &CALayer) -> [(&'static str, f64, f64); 3] {
        let pivot = self.ivars().borrow().pivot;
        scale_about(self.frame().size, layer.anchorPoint(), pivot, BUBBLE_FROM)
    }

    /// thread and bead swell in as one, on a spring, as the bead's colour comes out of blur
    pub fn bubble_in(&self, now: f64) {
        let Some(layer) = self.layer() else {
            return;
        };
        layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.22, Fill::Entrance), None);
        for (key, rest, small) in self.bubble(&layer) {
            layer.addAnimation_forKey(
                &spring(key, small, rest, now, POP, Fill::Entrance),
                Some(&NSString::from_str(key)),
            );
        }
        if let Some(content) = self.ivars().borrow().bead.content_layer() {
            attach_blur(&content, 0.0);
            content.setMasksToBounds(false);
            content.addAnimation_forKey(
                &blur_radius(BUBBLE_BLUR, 0.0, now, 0.36, Fill::Entrance),
                None,
            );
            detach_blur_after(self.mtm(), content, 0.45);
        }
    }

    /// the reverse of `bubble_in`, holding at the end until the window goes
    pub fn bubble_out(&self, now: f64, duration: f64) {
        let Some(layer) = self.layer() else {
            return;
        };
        let opacity = presented(&layer, "opacity").unwrap_or(1.0);
        layer.addAnimation_forKey(&fade(opacity, 0.0, now, duration, Fill::Exit), None);
        for (key, rest, small) in self.bubble(&layer) {
            let from = presented(&layer, key).unwrap_or(rest);
            let shrink = property(key, from, small, now, duration, Fill::Exit);
            shrink.setTimingFunction(Some(&arrive()));
            layer.addAnimation_forKey(&shrink, Some(&NSString::from_str(key)));
        }
        if let Some(content) = self.ivars().borrow().bead.content_layer() {
            blur_out(&content, BUBBLE_BLUR, now, duration);
            content.setMasksToBounds(false);
        }
    }
}

impl Swing {
    fn bead_center(&self) -> NSPoint {
        NSPoint::new(
            self.pivot.x + self.length * self.theta.sin(),
            self.pivot.y - self.length * self.theta.cos(),
        )
    }

    /// whether `point`, in the view, is on the bead or close enough to grab it
    fn over_bead(&self, point: NSPoint) -> bool {
        let center = self.bead_center();
        let reach = self.bead.view.frame().size.width / 2.0 + GRAB_SLOP;
        (point.x - center.x).hypot(point.y - center.y) <= reach
    }

    /// the swing's angle with the bead under `point`, 0 straight down and positive to the
    /// right. a point above the pivot counts as just below it
    fn angle_at(&self, point: NSPoint) -> f64 {
        (point.x - self.pivot.x).atan2((self.pivot.y - point.y).max(8.0))
    }

    /// keeps a free bead from hanging dead still: once it has, for a while, it swings again
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

    /// eases the look toward a pointer `dx`, `dy` points from the bead, y up
    fn follow(&mut self, dx: f64, dy: f64, dt: f64) {
        let distance = dx.hypot(dy).max(1.0);
        let reach = (distance / LOOK_REACH).min(1.0);
        let target = [dx / distance * reach, dy / distance * reach];
        let [x, y, vx, vy] = &mut self.look;
        for (pos, vel, goal) in [(x, vx, target[0]), (y, vy, target[1])] {
            *vel += (LOOK_STIFFNESS * (goal - *pos) - LOOK_DAMPING * *vel) * dt;
            *pos += *vel * dt;
        }
    }

    /// puts the thread and bead where the angle and look say
    fn place(&self) {
        let center = self.bead_center();
        let radius = self.bead.view.frame().size.width / 2.0;
        self.bead
            .view
            .setFrameOrigin(NSPoint::new(center.x - radius, center.y - radius));
        let (sin, cos) = self.theta.sin_cos();
        without_actions(|| {
            self.thread.setAffineTransform(CGAffineTransform {
                a: cos,
                b: sin,
                c: -sin,
                d: cos,
                tx: 0.0,
                ty: 0.0,
            });
        });
        self.bead.look(self.look[0], self.look[1]);
    }
}

/// past `SWING_LIMIT` each further radian of drag moves the bead less, toward a soft ceiling
fn rubber_band(angle: f64) -> f64 {
    let over = angle.abs() - SWING_LIMIT;
    if over <= 0.0 {
        return angle;
    }
    angle.signum() * (SWING_LIMIT + over * 0.55 / (1.0 + 1.1 * over))
}

#[cfg(test)]
mod tests {
    use objc2::ClassType;

    use super::*;

    /// registering the class makes objc2 check each override against the signature AppKit
    /// declares, which otherwise happens only at the first warning
    #[test]
    fn the_charm_class_registers() {
        let _ = Charm::class();
    }

    #[test]
    fn the_rubber_band_gives_way_less_the_further_it_goes() {
        assert_eq!(rubber_band(0.5), 0.5);
        assert_eq!(rubber_band(-SWING_LIMIT), -SWING_LIMIT);
        let a = rubber_band(SWING_LIMIT + 0.5) - SWING_LIMIT;
        let b = rubber_band(SWING_LIMIT + 1.0) - rubber_band(SWING_LIMIT + 0.5);
        assert!(a > 0.0 && b > 0.0 && b < a);
        assert_eq!(rubber_band(-2.0), -rubber_band(2.0));
    }
}
