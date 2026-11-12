//! what the warning and the break screen share: the panel, labels, delayed work and the
//! Core Animation helpers behind their blur, fade and slide transitions.

use core::ptr::NonNull;
use std::cell::Cell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSBackingStoreType, NSColor,
    NSFont, NSPanel, NSScreenSaverWindowLevel, NSTextAlignment, NSTextField,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_image::CIFilter;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNumber, NSObjectNSKeyValueCoding, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSString, NSTimer,
};
use objc2_quartz_core::{
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CASpringAnimation,
    kCAFillModeBackwards, kCAFillModeBoth, kCAMediaTimingFunctionEaseInEaseOut,
};

define_class!(
    /// the ivar says whether the panel can become the key window
    // SAFETY: NSPanel can be subclassed, and nothing here breaks its requirements. each
    // override has the signature AppKit declares, which registering the class checks.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaPanel"]
    #[ivars = bool]
    pub struct Panel;

    impl Panel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            *self.ivars()
        }

        /// a key nothing handles is dropped instead of beeping
        #[unsafe(method(noResponderFor:))]
        fn no_responder_for(&self, _selector: Sel) {}
    }
);

/// a borderless, see-through panel above every window, fullscreen apps included, and on
/// every Space. it never activates omma, so the app the user is in stays active. with
/// `takes_keys` it can still become the key window and get keystrokes, which a borderless
/// window otherwise never does; focus goes back to the user's app when it goes away.
pub fn make_panel(mtm: MainThreadMarker, frame: NSRect, takes_keys: bool) -> Retained<Panel> {
    let this = Panel::alloc(mtm).set_ivars(takes_keys);
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    // SAFETY: NSWindow's designated initializer, with the argument types it declares
    let panel: Retained<Panel> = unsafe {
        msg_send![
            super(this),
            initWithContentRect: frame,
            styleMask: style,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ]
    };

    panel.setOpaque(false);
    // native shadows follow the panel's rectangle, not the glass drawn inside it
    panel.setHasShadow(false);
    // a panel hides when its app isn't active, and omma never activates
    panel.setHidesOnDeactivate(false);
    // clear, so the glass and the behind-window blur sample the desktop. the dark
    // appearance keeps the glass and its text legible.
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    // a see-through window lets clicks through where it's transparent unless told
    // otherwise; this one takes them over its whole frame, faded out or not
    panel.setIgnoresMouseEvents(false);
    // SAFETY: an immutable AppKit constant
    if let Some(appearance) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        panel.setAppearance(Some(&appearance));
    }
    panel.setLevel(NSScreenSaverWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel
}

pub struct LabelStyle<'a> {
    pub font: &'a NSFont,
    pub color: &'a NSColor,
    pub alignment: NSTextAlignment,
    pub max_lines: isize,
}

/// a layer-backed label, so it can be animated
pub fn make_label(
    mtm: MainThreadMarker,
    text: &str,
    frame: NSRect,
    style: &LabelStyle<'_>,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setWantsLayer(true);
    label.setFrame(frame);
    label.setAlignment(style.alignment);
    label.setFont(Some(style.font));
    label.setTextColor(Some(style.color));
    label.setMaximumNumberOfLines(style.max_lines);
    label
}

/// false when the user has asked macOS to reduce motion: then windows appear and go at once,
/// with no blur, slide or fade
pub fn animations_on() -> bool {
    !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// runs `f` once, `delay` seconds from now, on the main run loop. like the app's tick, it
/// runs in the common modes, so an open status menu doesn't hold it up.
pub fn after(_mtm: MainThreadMarker, delay: f64, f: impl FnOnce() + 'static) {
    let f = Cell::new(Some(f));
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
        if let Some(f) = f.take() {
            f();
        }
    });
    // SAFETY: the marker proves this is the main thread, and the timer goes on the main run
    // loop and fires there, the only thread the block's captures may be used on. the mode
    // is an immutable Foundation constant.
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(delay, false, &block);
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
}

/// what an animation leaves behind
#[derive(Clone, Copy)]
pub enum Fill {
    /// shows the start value from now until `begin`, so entrances can be staggered, then
    /// hands back to the layer's own value
    Entrance,
    /// keeps the end value until the layer goes away
    Exit,
}

fn basic(
    key_path: &str,
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    fill: Fill,
) -> Retained<CABasicAnimation> {
    let anim = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: NSNumbers, the value type these key paths animate
    unsafe {
        anim.setFromValue(Some(&*NSNumber::numberWithDouble(from)));
        anim.setToValue(Some(&*NSNumber::numberWithDouble(to)));
    }
    anim.setDuration(duration);
    anim.setBeginTime(begin);
    // SAFETY: an immutable Core Animation constant
    anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
        kCAMediaTimingFunctionEaseInEaseOut
    })));
    set_fill(&anim, fill);
    anim
}

fn set_fill(anim: &CABasicAnimation, fill: Fill) {
    // SAFETY: immutable Core Animation constants
    match fill {
        Fill::Entrance => anim.setFillMode(unsafe { kCAFillModeBackwards }),
        Fill::Exit => {
            anim.setFillMode(unsafe { kCAFillModeBoth });
            anim.setRemovedOnCompletion(false);
        }
    }
}

pub fn fade(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    fill: Fill,
) -> Retained<CABasicAnimation> {
    basic("opacity", from, to, begin, duration, fill)
}

/// animates the blur that `attach_blur` adds
pub fn blur_radius(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    fill: Fill,
) -> Retained<CABasicAnimation> {
    basic("filters.blur.inputRadius", from, to, begin, duration, fill)
}

pub fn scale(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    fill: Fill,
) -> Retained<CABasicAnimation> {
    basic("transform.scale", from, to, begin, duration, fill)
}

pub fn slide_y(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    fill: Fill,
) -> Retained<CABasicAnimation> {
    basic("transform.translation.y", from, to, begin, duration, fill)
}

/// springs into place from `from_y` points away: positive starts above, negative below
pub fn spring_y(from_y: f64, begin: f64) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(
        "transform.translation.y",
    )));
    // SAFETY: NSNumbers, the value type this key path animates
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
    set_fill(&anim, Fill::Entrance);
    anim
}

/// the key of the animation `blur_out` adds
const BLUR_OUT: &str = "blur-out";

/// gives the layer a gaussian blur named "blur" whose radius `blur_radius` animates.
/// `resting` is the radius while no animation runs: 0.0 for a passing blur, more to keep
/// the layer soft.
pub fn attach_blur(layer: &CALayer, resting: f64) {
    // SAFETY: CIGaussianBlur takes an NSNumber for inputRadius, and a layer's filters are
    // CIFilters
    unsafe {
        let Some(filter) = CIFilter::filterWithName(&NSString::from_str("CIGaussianBlur")) else {
            return;
        };
        filter.setDefaults();
        filter.setValue_forKey(
            Some(&*NSNumber::numberWithDouble(resting)),
            &NSString::from_str("inputRadius"),
        );
        filter.setName(&NSString::from_str("blur"));
        let item: &AnyObject = &filter;
        layer.setFilters(Some(&NSArray::from_slice(&[item])));
    }
}

/// blurs a leaving layer from sharp to `radius`, and holds the blur until the layer goes away
pub fn blur_out(layer: &CALayer, radius: f64, begin: f64, duration: f64) {
    attach_blur(layer, 0.0);
    layer.addAnimation_forKey(
        &blur_radius(0.0, radius, begin, duration, Fill::Exit),
        Some(&NSString::from_str(BLUR_OUT)),
    );
}

/// drops a passing blur once its animation is over, so the layer renders without the
/// filter. a layer that has started to leave in the meantime keeps its blur.
pub fn detach_blur_after(mtm: MainThreadMarker, layer: Retained<CALayer>, delay: f64) {
    after(mtm, delay, move || {
        let leaving = layer
            .animationKeys()
            .is_some_and(|keys| keys.containsObject(&NSString::from_str(BLUR_OUT)));
        if !leaving {
            // SAFETY: None clears the filters
            unsafe { layer.setFilters(None) };
        }
    });
}

#[cfg(test)]
mod tests {
    use objc2::ClassType;

    use super::Panel;

    /// registering the class makes objc2 check each override against the signature AppKit
    /// declares, which otherwise happens only at the first warning
    #[test]
    fn the_panel_class_registers() {
        let _ = Panel::class();
    }
}
