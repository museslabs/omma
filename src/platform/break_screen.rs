//! The break screen: one fullscreen overlay window per display in Apple's
//! Liquid Glass style — a frosted, blurred backdrop (the desktop, softly
//! defocused) dimmed evenly for the eyes, with the break message, a live
//! countdown and (unless strict) Skip / Postpone laid directly on the glass.
//! The buttons are `NSGlassEffectView` capsules (the Control Center chip look)
//! wrapping a borderless `NSButton` that fires `skipBreak:` / `postponeBreak:`
//! on the delegate.

use core::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor,
    NSEventModifierFlags, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightThin, NSForegroundColorAttributeName, NSGlassEffectView, NSGlassEffectViewStyle,
    NSPanel, NSScreen, NSTextAlignment, NSTextField, NSTitlePosition, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_image::CIFilter;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSMutableAttributedString, NSNumber, NSObjectNSKeyValueCoding,
    NSPoint, NSRange, NSRect, NSSize, NSString, NSTimer,
};
use objc2_quartz_core::{
    kCAFillModeBackwards, kCAFillModeBoth, kCAMediaTimingFunctionEaseInEaseOut, CABasicAnimation,
    CACurrentMediaTime, CALayer, CAMediaTiming, CAMediaTimingFunction, CASpringAnimation,
};

/// Window level above normal windows and fullscreen apps.
const SCREEN_SAVER_LEVEL: isize = 1000;
/// Darkness of the dim scrim laid over the blur. High enough to even out a
/// busy desktop into a calm, uniformly dark backdrop with consistent contrast.
const SCRIM_ALPHA: f64 = 0.6;
/// Height of the glass capsule buttons.
const BUTTON_HEIGHT: f64 = 46.0;
/// Gaussian-blur radius the foreground resolves from on entry and dissolves
/// back into on exit — the "materialise from blur" look.
const BLUR_RADIUS: f64 = 16.0;
/// How long the exit blur-out plays before the windows are torn down.
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
    /// The foreground container (message, countdown, buttons) per window. Kept
    /// so the exit transition can blur and fade it back out.
    foregrounds: Vec<Retained<NSView>>,
    /// The backdrop container (blur + scrim) per window, faded on exit.
    backdrops: Vec<Retained<NSView>>,
    /// Whether to play the entrance/exit transitions and per-tick cross-fades;
    /// off when the user has asked macOS to minimise motion.
    animate: bool,
    visible: bool,
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
            visible: false,
        }
    }

    pub fn show(
        &mut self,
        name: &str,
        duration: i64,
        allow_skip: bool,
        allow_postpone: bool,
        skip_key: &str,
        postpone_key: &str,
        target: &AnyObject,
    ) {
        self.teardown_now();
        let mtm = self.mtm;
        let weight_regular = unsafe { NSFontWeightRegular };
        let weight_thin = unsafe { NSFontWeightThin };

        // Respect the system "Reduce Motion" setting: when it's on we lay the
        // overlay out statically and skip every blur, slide and cross-fade.
        self.animate = !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        let animate = self.animate;
        let now = CACurrentMediaTime();

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

            // Backdrop: the frosted desktop blur and the dim scrim, grouped in
            // their own layer-backed container so they fade in and out as one.
            let backdrop = NSView::initWithFrame(NSView::alloc(mtm), full);
            backdrop.setWantsLayer(true);
            content.addSubview(&backdrop);

            let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
            blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            blur.setState(NSVisualEffectState::Active);
            backdrop.addSubview(&blur);

            let scrim = NSBox::initWithFrame(NSBox::alloc(mtm), full);
            scrim.setBoxType(NSBoxType::Custom);
            scrim.setTitlePosition(NSTitlePosition::NoTitle);
            scrim.setBorderWidth(0.0);
            scrim.setCornerRadius(0.0);
            scrim.setFillColor(&NSColor::colorWithWhite_alpha(0.0, SCRIM_ALPHA));
            backdrop.addSubview(&scrim);

            // Foreground: message, countdown and buttons, in their own
            // layer-backed container. A single gaussian blur on this layer lets
            // the whole group materialise from blur into focus on the way in,
            // and dissolve back into blur on the way out.
            let foreground = NSView::initWithFrame(NSView::alloc(mtm), full);
            foreground.setWantsLayer(true);
            content.addSubview(&foreground);

            // Break message, laid directly on the glass (wraps to two lines).
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

            // Big countdown number, monospaced digits so it doesn't jitter.
            let count_font = NSFont::monospacedDigitSystemFontOfSize_weight(100.0, weight_thin);
            let white = NSColor::whiteColor();
            let count_label = make_label(
                mtm,
                &duration.to_string(),
                &count_font,
                &white,
                NSRect::new(
                    NSPoint::new(cx - 250.0, cy - 90.0),
                    NSSize::new(500.0, 130.0),
                ),
                1,
            );
            foreground.addSubview(&count_label);

            // Glass capsule buttons, centered side by side below the countdown.
            let gap = 16.0;
            let mut buttons: Vec<Retained<NSGlassEffectView>> = Vec::new();
            if allow_skip {
                buttons.push(make_button(mtm, "Skip", skip_key, target, sel!(skipBreak:)));
            }
            if allow_postpone {
                buttons.push(make_button(
                    mtm,
                    "Postpone",
                    postpone_key,
                    target,
                    sel!(postponeBreak:),
                ));
            }
            if !buttons.is_empty() {
                let widths: Vec<f64> = buttons.iter().map(|b| b.frame().size.width).collect();
                let total: f64 = widths.iter().sum::<f64>() + gap * (buttons.len() as f64 - 1.0);
                let by = cy - 170.0;
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

            // Choreograph the entrance: the backdrop fades up; the foreground
            // resolves out of the blur while the message slides down from above
            // and the buttons slide up from below. The countdown stays put —
            // it simply sharpens out of the blur as the foreground does.
            if animate {
                if let Some(layer) = backdrop.layer() {
                    layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.5, false), None);
                }
                if let Some(layer) = foreground.layer() {
                    let begin = now + 0.06;
                    attach_blur(&layer);
                    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.55, false), None);
                    layer.addAnimation_forKey(
                        &blur_radius(BLUR_RADIUS, 0.0, begin, 0.55, false),
                        None,
                    );
                    // The blur is only for the transition; drop it once the
                    // entrance settles so the static break renders crisp.
                    detach_blur_after(layer, 0.75);
                }
                let label_begin = now + 0.68;
                let buttons_begin = label_begin + 0.1;
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

        // A break can open from the once-a-second timer, with another app in
        // the foreground. Only the *active* app can own a key window, and the
        // cooperative `activate()` is throttled for a background agent waking on
        // a timer — so without forcing it the overlay never becomes key and the
        // Skip/Postpone keys are dead until the user clicks the screen.
        //
        // Promote to a regular app and force activation so the panel becomes key
        // straight away; `hide()` drops us back to Accessory. The Dock icon this
        // briefly creates is hidden behind the full-screen overlay (already
        // ordered front above), and reverting on hide returns focus to whatever
        // the user was doing.
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        force_activate(&app);
        if let Some(first) = self.windows.first() {
            first.makeKeyAndOrderFront(None);
        }

        self.visible = true;
    }

    pub fn update_countdown(&self, remaining: i64, _total: i64) {
        let text = NSString::from_str(&remaining.max(0).to_string());
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
        // No animation (Reduce Motion): tear the overlay down immediately.
        if !self.animate {
            self.teardown_now();
            return;
        }

        // Reverse of the entrance: the backdrop fades out while the foreground
        // dissolves back into blur. The animations hold their final state
        // (`hold`), so the overlay stays faded/blurred right up to teardown.
        let now = CACurrentMediaTime();
        for backdrop in &self.backdrops {
            if let Some(layer) = backdrop.layer() {
                layer.addAnimation_forKey(&fade(1.0, 0.0, now, EXIT_DURATION, true), None);
            }
        }
        for foreground in &self.foregrounds {
            if let Some(layer) = foreground.layer() {
                attach_blur(&layer);
                layer.addAnimation_forKey(&fade(1.0, 0.0, now, EXIT_DURATION, true), None);
                layer.addAnimation_forKey(
                    &blur_radius(0.0, BLUR_RADIUS, now, EXIT_DURATION, true),
                    None,
                );
            }
        }

        // Defer the actual teardown until the blur-out finishes. The windows
        // move into the timer block so they stay alive (and on screen) for the
        // animation; the block orders them out and restores Accessory mode.
        let windows: Vec<Retained<BreakWindow>> = self.windows.drain(..).collect();
        let mtm = self.mtm;
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            for window in &windows {
                window.orderOut(None);
            }
            NSApplication::sharedApplication(mtm)
                .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        });
        // SAFETY: the timer is scheduled on, and fires on, the main run loop,
        // where these main-thread-only windows are safe to touch.
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
        self.visible = false;
    }

    /// Tear the overlay down at once, with no exit animation: order every
    /// window out, forget the views, and drop back to a menu-bar-only Accessory
    /// app (we promote to Regular in `show()` so the overlay can take keyboard
    /// focus). Used when reopening and when Reduce Motion is on.
    fn teardown_now(&mut self) {
        let had_windows = !self.windows.is_empty();
        for window in self.windows.drain(..) {
            window.orderOut(None);
        }
        self.backdrops.clear();
        self.foregrounds.clear();
        self.countdown_labels.clear();
        self.visible = false;

        if had_windows {
            let app = NSApplication::sharedApplication(self.mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }
}

/// Force the app to the foreground. We use the deprecated forceful activation
/// deliberately: the cooperative `activate()` is routinely throttled for a
/// background agent waking on a timer, leaving the break overlay unable to
/// become key. The forceful variant is exactly the override this needs.
#[allow(deprecated)]
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
    // NSPanel hides itself when its app deactivates; a menu-bar (Accessory)
    // app is never "active", so without this the overlay never shows.
    window.setHidesOnDeactivate(false);
    // Clear background so the behind-window blur can sample the desktop, and a
    // dark appearance so the glass reads dark and the native buttons render as
    // light-on-dark controls that stay legible.
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
    // We keep a Retained reference for the screen's lifetime and order it out
    // rather than closing, so opting out of release-on-close is sound.
    unsafe { window.setReleasedWhenClosed(false) };
    window
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

/// Apply the fill behaviour shared by the basic animations. When `hold` is set
/// (exit), the animation keeps its final value on screen instead of snapping
/// back to the model value, since the windows are about to be torn down. When
/// not (entrance), `Backwards` fill holds the *start* value until `begin`, so
/// callers can stagger entrances by pushing `begin` later.
fn apply_fill(anim: &CABasicAnimation, hold: bool) {
    if hold {
        anim.setFillMode(unsafe { kCAFillModeBoth });
        anim.setRemovedOnCompletion(false);
    } else {
        anim.setFillMode(unsafe { kCAFillModeBackwards });
    }
}

/// A basic animation from `from` to `to` on `key_path`, eased smoothly in and
/// out over `duration` starting at `begin` (in `CACurrentMediaTime` units).
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

/// An opacity fade between `from` and `to`. See [`apply_fill`] for `hold`.
fn fade(from: f64, to: f64, begin: f64, duration: f64, hold: bool) -> Retained<CABasicAnimation> {
    let anim = basic("opacity", from, to, begin, duration);
    apply_fill(&anim, hold);
    anim
}

/// Animates the foreground's gaussian-blur radius (see [`attach_blur`]) so the
/// content resolves from blur into focus, or dissolves back. See [`apply_fill`].
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

/// A scale animation used for countdown number changes.
fn scale(from: f64, to: f64, begin: f64, duration: f64) -> Retained<CABasicAnimation> {
    let anim = basic("transform.scale", from, to, begin, duration);
    apply_fill(&anim, false);
    anim
}

/// A scale animation that can optionally hold its final presentation value.
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

/// A vertical slide that eases from `from_y` points off its resting spot up to
/// rest. Positive `from_y` starts above (slides down); negative starts below
/// (slides up).
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

/// Keep an entrance layer invisible until `begin`, then reveal it while it
/// springs in and resolves from blur to sharp.
fn animate_delayed_entrance(layer: Retained<CALayer>, from_y: f64, begin: f64, now: f64) {
    attach_blur(&layer);
    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.18, false), None);
    layer.addAnimation_forKey(&blur_radius(12.0, 0.0, begin, 0.42, false), None);
    layer.addAnimation_forKey(&slide_y(from_y, begin), None);
    detach_blur_after(layer, begin - now + 0.5);
}

/// Swap countdown digits with two static layers: the old digit shrinks/blurs
/// out while the new digit starts larger/blurred and resolves in-place.
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
        attach_blur(&layer);
        layer.addAnimation_forKey(&fade(1.0, 0.0, now, 0.3, true), None);
        layer.addAnimation_forKey(&scale_hold(1.0, 0.88, now, 0.3, true), None);
        layer.addAnimation_forKey(&blur_radius(0.0, 10.0, now, 0.3, true), None);
    }
    if let Some(layer) = label.layer() {
        attach_blur(&layer);
        layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.3, false), None);
        layer.addAnimation_forKey(&scale(1.12, 1.0, now, 0.3), None);
        layer.addAnimation_forKey(&blur_radius(10.0, 0.0, now, 0.3, false), None);
        detach_blur_after(layer, 0.34);
    }
    remove_view_after(outgoing, 0.34);
}

/// Attach a named gaussian blur (resting radius 0) to `layer`, so the
/// `filters.blur.inputRadius` key path is animatable for the blur in/out.
fn attach_blur(layer: &CALayer) {
    let name = NSString::from_str("CIGaussianBlur");
    let Some(filter) = (unsafe { CIFilter::filterWithName(&name) }) else {
        return;
    };
    unsafe {
        filter.setDefaults();
        let radius = NSString::from_str("inputRadius");
        filter.setValue_forKey(Some(&*NSNumber::numberWithDouble(0.0)), &radius);
        filter.setName(&NSString::from_str("blur"));
        let item: &AnyObject = &filter;
        let filters = NSArray::from_slice(&[item]);
        layer.setFilters(Some(&filters));
    }
    // Let the blur bleed past the layer's edge instead of clipping at it.
    layer.setMasksToBounds(false);
}

/// Remove `layer`'s blur filter after `delay` seconds, so the static break
/// renders crisp once the entrance blur-in has finished.
fn detach_blur_after(layer: Retained<CALayer>, delay: f64) {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| unsafe {
        layer.setFilters(None);
    });
    // SAFETY: scheduled on and fired from the main run loop, where this
    // main-thread layer is safe to mutate.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &block);
    }
}

/// Remove a temporary view once its presentation animation has finished.
fn remove_view_after(view: Retained<NSTextField>, delay: f64) {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        view.removeFromSuperview();
    });
    // SAFETY: scheduled on and fired from the main run loop, where this
    // main-thread view is safe to mutate.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &block);
    }
}

/// A Liquid Glass capsule button: a borderless `NSButton` (white title, click
/// target/action) embedded in an `NSGlassEffectView` rounded to a capsule. The
/// returned view is sized to fit its title; the caller sets its origin.
///
/// `key` is the config key spec (e.g. `"escape"`, `"space"`, `"cmd+s"`); when
/// it names a key it becomes the button's key equivalent so the action fires on
/// that keypress, and a dim hint (e.g. `Esc`) is appended to the title.
fn make_button(
    mtm: MainThreadMarker,
    title: &str,
    key: &str,
    target: &AnyObject,
    action: objc2::runtime::Sel,
) -> Retained<NSGlassEffectView> {
    let keybind = parse_keybind(key);

    // The full title carries an optional dim key hint after the label.
    let title_ns = NSString::from_str(title);
    let full = match &keybind {
        Some(kb) => format!("{title}  {}", kb.label),
        None => title.to_string(),
    };
    let full_ns = NSString::from_str(&full);
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&full_ns, Some(target), Some(action), mtm)
    };
    button.setBordered(false);

    // Borderless buttons draw their title in the control colour; force white so
    // it stays legible on the dark glass. The hint is dimmer and smaller.
    let font = NSFont::systemFontOfSize_weight(16.0, unsafe { NSFontWeightMedium });
    let attributed =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &full_ns);
    let white = NSColor::whiteColor();
    let title_range = NSRange::new(0, title_ns.length());
    unsafe {
        attributed.addAttribute_value_range(NSForegroundColorAttributeName, &*white, title_range);
        attributed.addAttribute_value_range(NSFontAttributeName, &*font, title_range);
    }
    if keybind.is_some() {
        let hint_font = NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium });
        let dim = NSColor::colorWithWhite_alpha(1.0, 0.55);
        let start = title_ns.length();
        let hint_range = NSRange::new(start, full_ns.length() - start);
        unsafe {
            attributed.addAttribute_value_range(NSForegroundColorAttributeName, &*dim, hint_range);
            attributed.addAttribute_value_range(NSFontAttributeName, &*hint_font, hint_range);
        }
    }
    button.setAttributedTitle(&attributed);

    // Fire the action on the configured keypress while the break window is key.
    if let Some(kb) = &keybind {
        button.setKeyEquivalent(&kb.key_equivalent);
        button.setKeyEquivalentModifierMask(kb.mods);
    }

    button.sizeToFit();

    let cw = (button.frame().size.width + 48.0).max(120.0);
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(cw, BUTTON_HEIGHT));
    let capsule = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), rect);
    capsule.setWantsLayer(true);
    capsule.setCornerRadius(BUTTON_HEIGHT / 2.0);
    capsule.setStyle(NSGlassEffectViewStyle::Regular);
    button.setFrame(rect);
    capsule.setContentView(Some(&button));
    capsule
}

/// A parsed key spec ready to apply to an `NSButton`: the key-equivalent string
/// AppKit matches against, its required modifier mask, and a human label for
/// the on-screen hint (e.g. `⌘S`, `Esc`).
struct KeyBind {
    key_equivalent: Retained<NSString>,
    mods: NSEventModifierFlags,
    label: String,
}

/// Parse a config key spec like `"s"`, `"escape"`, or `"cmd+shift+p"` into a
/// [`KeyBind`]. `+`-separated tokens are modifiers (`cmd`, `shift`,
/// `alt`/`option`, `ctrl`) plus exactly one key. Returns `None` when the spec
/// is empty or names no key.
fn parse_keybind(spec: &str) -> Option<KeyBind> {
    let mut mods = NSEventModifierFlags::empty();
    let mut symbols = String::new();
    let mut key: Option<(String, String)> = None;
    for token in spec.split('+') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "super" | "meta" => {
                mods |= NSEventModifierFlags::Command;
                symbols.push('⌘');
            }
            "shift" => {
                mods |= NSEventModifierFlags::Shift;
                symbols.push('⇧');
            }
            "alt" | "option" | "opt" => {
                mods |= NSEventModifierFlags::Option;
                symbols.push('⌥');
            }
            "ctrl" | "control" => {
                mods |= NSEventModifierFlags::Control;
                symbols.push('⌃');
            }
            _ => key = Some(key_equivalent_for(token)),
        }
    }
    let (equiv, label) = key?;
    Some(KeyBind {
        key_equivalent: NSString::from_str(&equiv),
        mods,
        label: format!("{symbols}{label}"),
    })
}

/// Map a key token to `(key-equivalent string, display label)`. Named keys
/// resolve to their control characters; anything else is taken as a literal
/// character, lowercased so a plain letter matches without requiring Shift.
fn key_equivalent_for(token: &str) -> (String, String) {
    match token.to_ascii_lowercase().as_str() {
        "escape" | "esc" => ("\u{1b}".into(), "Esc".into()),
        "space" => (" ".into(), "Space".into()),
        "enter" | "return" => ("\r".into(), "↩".into()),
        "tab" => ("\t".into(), "⇥".into()),
        "delete" | "backspace" => ("\u{8}".into(), "⌫".into()),
        other => (other.to_string(), other.to_uppercase()),
    }
}

// Silence "field is never read" for `visible`; it documents intent and may be
// used by future logic.
impl BreakScreen {
    #[allow(dead_code)]
    pub fn is_visible(&self) -> bool {
        self.visible
    }
}
