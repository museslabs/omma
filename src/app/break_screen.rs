//! one fullscreen overlay per display in liquid glass style. buttons are
//! NSGlassEffectView capsules wrapping a borderless NSButton.

use core::ptr::NonNull;
use std::path::Path;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFont,
    NSFontWeightHeavy, NSFontWeightRegular, NSGlassEffectView, NSImage, NSPanel, NSScreen,
    NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_core_image::CIFilter;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNumber, NSObjectNSKeyValueCoding, NSPoint, NSRect, NSSize,
    NSString, NSTimer,
};
use objc2_quartz_core::{
    CABasicAnimation, CACurrentMediaTime, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CASpringAnimation, kCAFillModeBackwards, kCAFillModeBoth, kCAGravityResizeAspectFill,
    kCAMediaTimingFunctionEaseInEaseOut,
};

use super::glass_button::{GlassButtonStyle, make_glass_button};
use crate::config::KeySpec;

/// above normal windows and fullscreen apps
const SCREEN_SAVER_LEVEL: isize = 1000;
/// dim scrim over the blur, enough to even out a busy desktop into a uniformly dark backdrop
const SCRIM_ALPHA: f64 = 0.25;
const BUTTON_HEIGHT: f64 = 46.0;
/// foreground entry/exit blur radius, the "materialise from blur" look
const BLUR_RADIUS: f64 = 16.0;
/// resting blur on a custom background image, eyeballed to match the FullScreenUI desktop blur
const IMAGE_BLUR_RADIUS_RESTING: f64 = 16.0;
/// image enters from this radius and animates down to resting, sharpening with the foreground
const IMAGE_BLUR_RADIUS_START: f64 = 60.0;
/// below 1.0 the dark void bleeds through and darkens the image so text stays legible
const IMAGE_OPACITY: f32 = 0.75;
const EXIT_DURATION: f64 = 0.32;

define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaBreakWindow"]
    struct BreakWindow;

    impl BreakWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

pub struct BreakScreen {
    mtm: MainThreadMarker,
    windows: Vec<Retained<BreakWindow>>,
    countdown_labels: Vec<Retained<NSTextField>>,
    /// kept so the exit transition can blur and fade out the message/countdown/buttons
    foregrounds: Vec<Retained<NSView>>,
    /// blur + scrim per window, faded on exit
    backdrops: Vec<Retained<NSView>>,
    /// off when the user has asked macos to reduce motion
    animate: bool,
}

impl BreakScreen {
    pub fn new(mtm: MainThreadMarker) -> BreakScreen {
        BreakScreen {
            mtm,
            windows: Vec::new(),
            countdown_labels: Vec::new(),
            foregrounds: Vec::new(),
            backdrops: Vec::new(),
            animate: true,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "grouped into a struct when the panel code is shared"
    )]
    pub fn show(
        &mut self,
        name: &str,
        remaining: u64,
        allow_skip: bool,
        allow_postpone: bool,
        skip_key: Option<KeySpec>,
        postpone_key: Option<KeySpec>,
        image_path: Option<&Path>,
        target: &AnyObject,
    ) {
        self.teardown_now();
        let mtm = self.mtm;
        let weight_regular = unsafe { NSFontWeightRegular };
        let weight_heavy = unsafe { NSFontWeightHeavy };

        // reduce motion: skip every blur, slide and cross-fade
        self.animate = !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        let animate = self.animate;
        let now = CACurrentMediaTime();

        // monospaced so the digits don't shift as they tick
        let count_font = NSFont::monospacedDigitSystemFontOfSize_weight(100.0, weight_heavy);

        let screens = NSScreen::screens(mtm);
        for i in 0..screens.count() {
            let screen = screens.objectAtIndex(i);
            let frame = screen.frame();
            let w = frame.size.width;
            let h = frame.size.height;
            let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
            let cx = w / 2.0;
            let cy = h / 2.0;

            let window = make_window(mtm, frame);
            let Some(content) = window.contentView() else {
                continue;
            };
            content.setWantsLayer(true);

            // backdrop: blur + scrim in one container so they fade as a group
            let backdrop = NSView::initWithFrame(NSView::alloc(mtm), full);
            backdrop.setWantsLayer(true);
            content.addSubview(&backdrop);

            let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
            blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            blur.setState(NSVisualEffectState::Active);
            backdrop.addSubview(&blur);

            // optional custom image, between blur and scrim. a file that isn't an image is skipped.
            // a permanent gaussian blur keeps it a soft backdrop instead of a sharp photo.
            let image_view =
                image_path.and_then(|p| make_image_view(mtm, p, full, screen.backingScaleFactor()));
            if let Some(view) = &image_view {
                backdrop.addSubview(view);
            }

            let scrim = NSBox::initWithFrame(NSBox::alloc(mtm), full);
            scrim.setBoxType(NSBoxType::Custom);
            scrim.setTitlePosition(NSTitlePosition::NoTitle);
            scrim.setBorderWidth(0.0);
            scrim.setCornerRadius(0.0);
            scrim.setFillColor(&NSColor::colorWithWhite_alpha(0.0, SCRIM_ALPHA));
            backdrop.addSubview(&scrim);

            // foreground: message, countdown, buttons. one gaussian blur on this layer lets
            // the group materialise from blur on the way in and dissolve back on the way out.
            let foreground = NSView::initWithFrame(NSView::alloc(mtm), full);
            foreground.setWantsLayer(true);
            content.addSubview(&foreground);

            // wraps to two lines
            let msg_w = (w - 160.0).min(1000.0);
            let title_font = NSFont::systemFontOfSize_weight(28.0, weight_regular);
            let msg_color = NSColor::colorWithWhite_alpha(1.0, 0.92);
            let name_label = make_label(
                mtm,
                name,
                &title_font,
                &msg_color,
                NSRect::new(
                    NSPoint::new(cx - msg_w / 2.0, cy + 44.0),
                    NSSize::new(msg_w, 80.0),
                ),
                2,
            );
            foreground.addSubview(&name_label);

            // each tick cross-fades via update_countdown
            let count_label = make_label(
                mtm,
                &remaining.to_string(),
                &count_font,
                &NSColor::whiteColor(),
                NSRect::new(
                    NSPoint::new(cx - 250.0, cy - 90.0),
                    NSSize::new(500.0, 130.0),
                ),
                1,
            );
            foreground.addSubview(&count_label);

            let gap = 16.0;
            let mut buttons: Vec<Retained<NSGlassEffectView>> = Vec::new();
            if allow_skip {
                buttons.push(make_glass_button(
                    mtm,
                    "Skip",
                    skip_key,
                    target,
                    sel!(skip:),
                    GlassButtonStyle::fit(BUTTON_HEIGHT, 120.0, 48.0, 16.0, 13.0),
                ));
            }
            if allow_postpone {
                buttons.push(make_glass_button(
                    mtm,
                    "Postpone",
                    postpone_key,
                    target,
                    sel!(postpone:),
                    GlassButtonStyle::fit(BUTTON_HEIGHT, 120.0, 48.0, 16.0, 13.0),
                ));
            }
            if !buttons.is_empty() {
                let widths: Vec<f64> = buttons.iter().map(|b| b.frame().size.width).collect();
                let total: f64 = widths.iter().sum::<f64>() + gap * (buttons.len() as f64 - 1.0);
                let by = 150.0;
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

            // entrance choreography: backdrop fades up, foreground resolves out of blur, message
            // slides down from above, buttons slide up from below. countdown just sharpens.
            if animate {
                if let Some(layer) = backdrop.layer() {
                    layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.5, false), None);
                }
                if let Some(layer) = foreground.layer() {
                    let begin = now + 0.06;
                    attach_blur(&layer, 0.0);
                    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.55, false), None);
                    layer.addAnimation_forKey(
                        &blur_radius(BLUR_RADIUS, 0.0, begin, 0.55, false),
                        None,
                    );
                    // drop the transition blur once the entrance settles so the break renders crisp
                    detach_blur_after(layer, 0.75);
                }
                // image holds at its resting radius after the sharpen-in, no detach
                if let Some(layer) = image_view.as_ref().and_then(|v| v.layer()) {
                    let begin = now + 0.06;
                    layer.addAnimation_forKey(
                        &blur_radius(
                            IMAGE_BLUR_RADIUS_START,
                            IMAGE_BLUR_RADIUS_RESTING,
                            begin,
                            0.55,
                            false,
                        ),
                        None,
                    );
                }
                let label_begin = now + 0.68;
                let buttons_begin = label_begin + 0.28;
                if let Some(layer) = name_label.layer() {
                    animate_delayed_entrance(layer, 28.0, label_begin, now);
                }
                for button in &buttons {
                    if let Some(layer) = button.layer() {
                        animate_delayed_entrance(layer, -28.0, buttons_begin, now);
                    }
                }
            }

            window.orderFrontRegardless();
            self.windows.push(window);
            self.backdrops.push(backdrop);
            self.foregrounds.push(foreground);
            self.countdown_labels.push(count_label);
        }

        // only the active app can own a key window. cooperative activate() is throttled for a
        // background app waking on a timer, so without forcing it the overlay never becomes key
        // and skip/postpone keys are dead until the user clicks. promote to regular and force
        // activate so the panel becomes key, hide() drops us back to accessory.
        let app = NSApplication::sharedApplication(mtm);
        let Some(first) = self.windows.first() else {
            return;
        };
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        force_activate(&app);
        first.makeKeyAndOrderFront(None);
    }

    pub fn is_visible(&self) -> bool {
        !self.windows.is_empty()
    }

    pub fn update_countdown(&self, remaining: u64) {
        let text = NSString::from_str(&remaining.to_string());
        for label in &self.countdown_labels {
            if self.animate {
                animate_countdown_swap(self.mtm, label, &text);
            } else {
                label.setStringValue(&text);
            }
        }
    }

    pub fn hide(&mut self) {
        if self.windows.is_empty() {
            return;
        }
        // reduce motion: tear down immediately
        if !self.animate {
            self.teardown_now();
            return;
        }

        // reverse of the entrance. animations hold their final state so the overlay stays
        // faded/blurred right up to teardown.
        let now = CACurrentMediaTime();
        for backdrop in &self.backdrops {
            if let Some(layer) = backdrop.layer() {
                layer.addAnimation_forKey(&fade(1.0, 0.0, now, EXIT_DURATION, true), None);
            }
        }
        for foreground in &self.foregrounds {
            if let Some(layer) = foreground.layer() {
                attach_blur(&layer, 0.0);
                layer.addAnimation_forKey(&fade(1.0, 0.0, now, EXIT_DURATION, true), None);
                layer.addAnimation_forKey(
                    &blur_radius(0.0, BLUR_RADIUS, now, EXIT_DURATION, true),
                    None,
                );
            }
        }

        // defer teardown until the blur-out finishes. windows move into the timer block so
        // they stay on screen for the animation, then the block orders them out and restores
        // accessory mode.
        let windows = std::mem::take(&mut self.windows);
        let mtm = self.mtm;
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            for window in &windows {
                window.orderOut(None);
            }
            NSApplication::sharedApplication(mtm)
                .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        });
        // SAFETY: scheduled on and fired from the main run loop where these main-thread-only
        // windows are safe to touch.
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                EXIT_DURATION + 0.05,
                false,
                &block,
            );
        }

        self.backdrops.clear();
        self.foregrounds.clear();
        self.countdown_labels.clear();
    }

    /// no exit animation. order every window out, forget the views, drop back to accessory.
    /// used when reopening and when reduce motion is on.
    fn teardown_now(&mut self) {
        let had_windows = !self.windows.is_empty();
        for window in self.windows.drain(..) {
            window.orderOut(None);
        }
        self.backdrops.clear();
        self.foregrounds.clear();
        self.countdown_labels.clear();

        if had_windows {
            let app = NSApplication::sharedApplication(self.mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }
}

#[expect(
    deprecated,
    reason = "cooperative activate() is throttled for a background app woken by a timer, so the panel never becomes key"
)]
fn force_activate(app: &NSApplication) {
    app.activateIgnoringOtherApps(true);
}

fn make_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<BreakWindow> {
    let window: Retained<BreakWindow> = unsafe {
        let alloc = BreakWindow::alloc(mtm);
        msg_send![
            alloc,
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };

    window.setOpaque(false);
    window.setHasShadow(false);
    // NSPanel hides on app deactivate, and an accessory app is never "active"
    window.setHidesOnDeactivate(false);
    // clear background so the behind-window blur samples the desktop. dark appearance keeps
    // the glass and native buttons legible.
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    if let Some(appearance) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        window.setAppearance(Some(&appearance));
    }
    window.setLevel(SCREEN_SAVER_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    // we hold a Retained ref for the screen's lifetime and orderOut instead of closing
    unsafe { window.setReleasedWhenClosed(false) };
    window
}

fn make_image_view(
    mtm: MainThreadMarker,
    path: &Path,
    frame: NSRect,
    backing_scale: f64,
) -> Option<Retained<NSView>> {
    let ns_path = NSString::from_str(path.to_str()?);
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path)?;
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setWantsLayer(true);
    let layer = view.layer()?;
    let image_obj: &AnyObject = &image;
    unsafe {
        layer.setContents(Some(image_obj));
        layer.setContentsGravity(kCAGravityResizeAspectFill);
    }
    layer.setContentsScale(backing_scale);
    layer.setOpacity(IMAGE_OPACITY);
    attach_blur(&layer, IMAGE_BLUR_RADIUS_RESTING);
    Some(view)
}

fn make_label(
    mtm: MainThreadMarker,
    text: &str,
    font: &NSFont,
    color: &NSColor,
    frame: NSRect,
    max_lines: isize,
) -> Retained<NSTextField> {
    let s = NSString::from_str(text);
    let label = NSTextField::labelWithString(&s, mtm);
    label.setWantsLayer(true);
    label.setFrame(frame);
    label.setAlignment(NSTextAlignment::Center);
    label.setFont(Some(font));
    label.setTextColor(Some(color));
    label.setMaximumNumberOfLines(max_lines);
    label
}

/// hold=true (exit) keeps the final value on screen instead of snapping back to the model
/// value, since windows are about to be torn down. hold=false (entrance) uses backwards fill so
/// callers can stagger entrances by pushing begin later.
fn apply_fill(anim: &CABasicAnimation, hold: bool) {
    if hold {
        anim.setFillMode(unsafe { kCAFillModeBoth });
        anim.setRemovedOnCompletion(false);
    } else {
        anim.setFillMode(unsafe { kCAFillModeBackwards });
    }
}

fn basic(
    key_path: &str,
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
) -> Retained<CABasicAnimation> {
    let anim = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    unsafe {
        anim.setFromValue(Some(&*NSNumber::numberWithDouble(from)));
        anim.setToValue(Some(&*NSNumber::numberWithDouble(to)));
    }
    anim.setDuration(duration);
    anim.setBeginTime(begin);
    anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
        kCAMediaTimingFunctionEaseInEaseOut
    })));
    anim
}

fn fade(from: f64, to: f64, begin: f64, duration: f64, hold: bool) -> Retained<CABasicAnimation> {
    let anim = basic("opacity", from, to, begin, duration);
    apply_fill(&anim, hold);
    anim
}

fn blur_radius(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    hold: bool,
) -> Retained<CABasicAnimation> {
    let anim = basic("filters.blur.inputRadius", from, to, begin, duration);
    apply_fill(&anim, hold);
    anim
}

fn scale(from: f64, to: f64, begin: f64, duration: f64) -> Retained<CABasicAnimation> {
    let anim = basic("transform.scale", from, to, begin, duration);
    apply_fill(&anim, false);
    anim
}

fn scale_hold(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    hold: bool,
) -> Retained<CABasicAnimation> {
    let anim = basic("transform.scale", from, to, begin, duration);
    apply_fill(&anim, hold);
    anim
}

/// positive from_y starts above (slides down), negative starts below (slides up)
fn slide_y(from_y: f64, begin: f64) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(
        "transform.translation.y",
    )));
    unsafe {
        anim.setFromValue(Some(&*NSNumber::numberWithDouble(from_y)));
        anim.setToValue(Some(&*NSNumber::numberWithDouble(0.0)));
    }
    anim.setMass(1.0);
    anim.setStiffness(180.0);
    anim.setDamping(22.0);
    anim.setInitialVelocity(0.0);
    anim.setDuration(anim.settlingDuration().min(0.7));
    anim.setBeginTime(begin);
    anim.setFillMode(unsafe { kCAFillModeBackwards });
    anim
}

/// invisible until `begin`, then springs in and resolves from blur to sharp
fn animate_delayed_entrance(layer: Retained<CALayer>, from_y: f64, begin: f64, now: f64) {
    attach_blur(&layer, 0.0);
    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.18, false), None);
    layer.addAnimation_forKey(&blur_radius(12.0, 0.0, begin, 0.42, false), None);
    layer.addAnimation_forKey(&slide_y(from_y, begin), None);
    detach_blur_after(layer, begin - now + 0.5);
}

/// old digit shrinks/blurs out, new digit starts larger/blurred and resolves in place
fn animate_countdown_swap(mtm: MainThreadMarker, label: &NSTextField, text: &NSString) {
    let old_text = label.stringValue();
    let outgoing = NSTextField::labelWithString(&old_text, mtm);
    outgoing.setWantsLayer(true);
    outgoing.setFrame(label.frame());
    outgoing.setAlignment(NSTextAlignment::Center);
    outgoing.setMaximumNumberOfLines(1);
    if let Some(font) = label.font() {
        outgoing.setFont(Some(&font));
    }
    if let Some(color) = label.textColor() {
        outgoing.setTextColor(Some(&color));
    }

    if let Some(parent) = unsafe { label.superview() } {
        parent.addSubview(&outgoing);
    }

    label.setStringValue(text);

    let now = CACurrentMediaTime();
    if let Some(layer) = outgoing.layer() {
        attach_blur(&layer, 0.0);
        layer.addAnimation_forKey(&fade(1.0, 0.0, now, 0.3, true), None);
        layer.addAnimation_forKey(&scale_hold(1.0, 0.88, now, 0.3, true), None);
        layer.addAnimation_forKey(&blur_radius(0.0, 10.0, now, 0.3, true), None);
    }
    if let Some(layer) = label.layer() {
        attach_blur(&layer, 0.0);
        layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.3, false), None);
        layer.addAnimation_forKey(&scale(1.12, 1.0, now, 0.3), None);
        layer.addAnimation_forKey(&blur_radius(10.0, 0.0, now, 0.3, false), None);
        detach_blur_after(layer, 0.34);
    }
    remove_view_after(outgoing, 0.34);
}

/// makes filters.blur.inputRadius animatable. `resting` is the radius held when no animation
/// is running, pass 0.0 for a transient blur or a non-zero to keep the layer permanently soft.
fn attach_blur(layer: &CALayer, resting: f64) {
    let name = NSString::from_str("CIGaussianBlur");
    let Some(filter) = (unsafe { CIFilter::filterWithName(&name) }) else {
        return;
    };
    unsafe {
        filter.setDefaults();
        let radius = NSString::from_str("inputRadius");
        filter.setValue_forKey(Some(&*NSNumber::numberWithDouble(resting)), &radius);
        filter.setName(&NSString::from_str("blur"));
        let item: &AnyObject = &filter;
        let filters = NSArray::from_slice(&[item]);
        layer.setFilters(Some(&filters));
    }
    // let the blur bleed past the layer's edge instead of clipping
    layer.setMasksToBounds(false);
}

fn detach_blur_after(layer: Retained<CALayer>, delay: f64) {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| unsafe {
        layer.setFilters(None);
    });
    // SAFETY: scheduled on and fired from the main run loop
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &block);
    }
}

fn remove_view_after(view: Retained<NSTextField>, delay: f64) {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        view.removeFromSuperview();
    });
    // SAFETY: scheduled on and fired from the main run loop
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &block);
    }
}
