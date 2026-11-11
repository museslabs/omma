use core::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont, NSFontWeightMedium,
    NSFontWeightSemibold, NSGlassEffectView, NSGlassEffectViewStyle, NSPanel, NSScreen,
    NSTextAlignment, NSTextField, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_image::CIFilter;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNumber, NSObjectNSKeyValueCoding, NSPoint, NSRect, NSSize,
    NSString, NSTimer,
};
use objc2_quartz_core::{
    CABasicAnimation, CACurrentMediaTime, CALayer, CAMediaTiming, CAMediaTimingFunction,
    kCAFillModeBackwards, kCAFillModeBoth, kCAMediaTimingFunctionEaseInEaseOut,
};

use super::glass_button::{GlassButtonStyle, make_glass_button};

const PANEL_LEVEL: isize = 1000;
const PANEL_W: f64 = 340.0;
const PANEL_H: f64 = 112.0;
const BUTTON_W: f64 = 140.0;
const BUTTON_H: f64 = 36.0;
const HIDE_DURATION: f64 = 0.24;

define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaPreBreakWindow"]
    struct PreBreakWindow;

    impl PreBreakWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }
    }
);

pub struct PreBreakDialog {
    mtm: MainThreadMarker,
    window: Option<Retained<PreBreakWindow>>,
    glass: Option<Retained<NSGlassEffectView>>,
    body: Option<Retained<NSView>>,
    countdown_label: Option<Retained<NSTextField>>,
}

impl PreBreakDialog {
    pub fn new(mtm: MainThreadMarker) -> PreBreakDialog {
        PreBreakDialog {
            mtm,
            window: None,
            glass: None,
            body: None,
            countdown_label: None,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "grouped into a struct when the panel code is shared"
    )]
    pub fn show(
        &mut self,
        _name: &str,
        allow_skip: bool,
        allow_postpone: bool,
        seconds_until_break: u64,
        skip_key: &str,
        postpone_key: &str,
        target: &AnyObject,
    ) {
        self.hide();

        let Some(screen) = NSScreen::mainScreen(self.mtm) else {
            return;
        };
        let visible = screen.visibleFrame();
        let frame = NSRect::new(
            NSPoint::new(
                visible.origin.x + (visible.size.width - PANEL_W) / 2.0,
                visible.origin.y + visible.size.height - PANEL_H - 18.0,
            ),
            NSSize::new(PANEL_W, PANEL_H),
        );

        let window = make_window(self.mtm, frame);
        let Some(content) = window.contentView() else {
            return;
        };
        content.setWantsLayer(true);

        let glass = NSGlassEffectView::initWithFrame(
            NSGlassEffectView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H)),
        );
        glass.setWantsLayer(true);
        glass.setCornerRadius(26.0);
        glass.setStyle(NSGlassEffectViewStyle::Regular);
        if let Some(layer) = glass.layer() {
            center_layer_anchor(&layer, PANEL_W, PANEL_H);
            layer.setMasksToBounds(true);
        }
        content.addSubview(&glass);

        let body = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H)),
        );
        body.setWantsLayer(true);
        glass.setContentView(Some(&body));

        let title = make_label(
            self.mtm,
            "Break soon",
            14.0,
            unsafe { NSFontWeightSemibold },
            0.76,
            NSTextAlignment::Left,
            NSRect::new(NSPoint::new(28.0, 65.0), NSSize::new(130.0, 18.0)),
            1,
        );
        body.addSubview(&title);

        let countdown = make_label(
            self.mtm,
            &fmt_seconds(seconds_until_break),
            28.0,
            unsafe { NSFontWeightMedium },
            0.95,
            NSTextAlignment::Left,
            NSRect::new(NSPoint::new(28.0, 25.0), NSSize::new(130.0, 34.0)),
            1,
        );
        body.addSubview(&countdown);

        let primary_title = if allow_skip { "Skip" } else { "Dismiss" };
        let primary_action = if allow_skip {
            sel!(skip:)
        } else {
            sel!(dismissWarning:)
        };
        let primary_key = if allow_skip { skip_key } else { "" };
        let button_x = PANEL_W - BUTTON_W - 28.0;
        let primary_y = if allow_postpone { 18.0 } else { 38.0 };
        let primary = make_glass_button(
            self.mtm,
            primary_title,
            primary_key,
            target,
            primary_action,
            GlassButtonStyle::fixed(BUTTON_W, BUTTON_H, 13.0, 11.0),
        );
        primary.setFrame(NSRect::new(
            NSPoint::new(button_x, primary_y),
            NSSize::new(BUTTON_W, BUTTON_H),
        ));
        body.addSubview(&primary);

        if allow_postpone {
            let postpone = make_glass_button(
                self.mtm,
                "Postpone",
                postpone_key,
                target,
                sel!(postpone:),
                GlassButtonStyle::fixed(BUTTON_W, BUTTON_H, 13.0, 11.0),
            );
            postpone.setFrame(NSRect::new(
                NSPoint::new(button_x, 58.0),
                NSSize::new(BUTTON_W, BUTTON_H),
            ));
            body.addSubview(&postpone);
        }

        if let Some(layer) = glass.layer() {
            let now = CACurrentMediaTime();
            layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.16, false), None);
            layer.addAnimation_forKey(&slide_y(28.0, 0.0, now, 0.22, false), None);
        }
        if let Some(layer) = body.layer() {
            let now = CACurrentMediaTime();
            attach_blur(&layer);
            layer.addAnimation_forKey(&fade(0.0, 1.0, now + 0.03, 0.2, false), None);
            layer.addAnimation_forKey(&blur_radius(10.0, 0.0, now + 0.03, 0.28, false), None);
            detach_blur_after(layer, 0.36);
        }

        let app = NSApplication::sharedApplication(self.mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        force_activate(&app);
        window.orderFrontRegardless();
        window.makeKeyAndOrderFront(None);
        self.window = Some(window);
        self.glass = Some(glass);
        self.body = Some(body);
        self.countdown_label = Some(countdown);
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_some()
    }

    pub fn update_countdown(&mut self, seconds_until_break: u64) {
        if let Some(label) = &self.countdown_label {
            label.setStringValue(&NSString::from_str(&fmt_seconds(seconds_until_break)));
        }
    }

    pub fn hide(&mut self) {
        self.hide_inner(true);
    }

    pub fn hide_for_break(&mut self) {
        self.hide_inner(false);
    }

    fn hide_inner(&mut self, restore_accessory: bool) {
        let Some(window) = self.window.take() else {
            return;
        };
        if let Some(glass) = self.glass.take()
            && let Some(layer) = glass.layer()
        {
            let now = CACurrentMediaTime();
            attach_blur(&layer);
            layer.addAnimation_forKey(&fade(1.0, 0.0, now, HIDE_DURATION, true), None);
            layer.addAnimation_forKey(&blur_radius(0.0, 22.0, now, HIDE_DURATION, true), None);
            layer.addAnimation_forKey(&slide_y(0.0, 28.0, now, HIDE_DURATION, true), None);
        }
        if let Some(body) = self.body.take()
            && let Some(layer) = body.layer()
        {
            let now = CACurrentMediaTime();
            attach_blur(&layer);
            layer.addAnimation_forKey(&fade(1.0, 0.0, now, HIDE_DURATION, true), None);
            layer.addAnimation_forKey(&blur_radius(0.0, 18.0, now, HIDE_DURATION, true), None);
        }
        self.countdown_label = None;

        let mtm = self.mtm;
        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            window.orderOut(None);
            if restore_accessory {
                NSApplication::sharedApplication(mtm)
                    .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            }
        });
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                HIDE_DURATION + 0.03,
                false,
                &block,
            );
        }
    }
}

fn make_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<PreBreakWindow> {
    let window: Retained<PreBreakWindow> = unsafe {
        let alloc = PreBreakWindow::alloc(mtm);
        msg_send![
            alloc,
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };

    window.setOpaque(false);
    // native shadows follow the panel rect, not the rounded glass, so they leak a jagged outline
    window.setHasShadow(false);
    // accessory apps stay inactive, so NSPanel stays hidden until the break activates the app
    window.setHidesOnDeactivate(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    if let Some(appearance) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        window.setAppearance(Some(&appearance));
    }
    window.setLevel(PANEL_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    unsafe { window.setReleasedWhenClosed(false) };
    window
}

#[expect(
    deprecated,
    reason = "cooperative activate() is throttled for a background app woken by a timer, so the panel never becomes key"
)]
fn force_activate(app: &NSApplication) {
    app.activateIgnoringOtherApps(true);
}

#[expect(
    clippy::too_many_arguments,
    reason = "grouped into a struct when the panel code is shared"
)]
fn make_label(
    mtm: MainThreadMarker,
    text: &str,
    size: f64,
    weight: f64,
    alpha: f64,
    alignment: NSTextAlignment,
    frame: NSRect,
    max_lines: isize,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(frame);
    label.setAlignment(alignment);
    label.setFont(Some(&NSFont::systemFontOfSize_weight(size, weight)));
    label.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, alpha)));
    label.setMaximumNumberOfLines(max_lines);
    label
}

fn fmt_seconds(seconds: u64) -> String {
    format!("{seconds}s")
}

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

fn slide_y(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    hold: bool,
) -> Retained<CABasicAnimation> {
    let anim = basic("transform.translation.y", from, to, begin, duration);
    apply_fill(&anim, hold);
    anim
}

fn center_layer_anchor(layer: &CALayer, width: f64, height: f64) {
    layer.setAnchorPoint(NSPoint::new(0.5, 0.5));
    layer.setPosition(NSPoint::new(width / 2.0, height / 2.0));
}

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
}

fn detach_blur_after(layer: Retained<CALayer>, delay: f64) {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| unsafe {
        layer.setFilters(None);
    });
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &block);
    }
}
