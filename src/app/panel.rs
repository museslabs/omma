//! what the warning and the break screen share: the panel, labels, delayed work and the
//! Core Animation helpers behind their blur, fade and slide transitions.

use core::ptr::NonNull;
use std::cell::Cell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSBackingStoreType, NSColor, NSFont, NSPanel, NSScreenSaverWindowLevel, NSTextAlignment,
    NSTextField, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_image::CIFilter;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNumber, NSObjectNSKeyValueCoding, NSPoint, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};
use objc2_quartz_core::{
    CAAnimation, CABasicAnimation, CAKeyframeAnimation, CALayer, CAMediaTiming,
    CAMediaTimingFunction, CASpringAnimation, CATransaction, kCAFillModeBackwards, kCAFillModeBoth,
    kCAMediaTimingFunctionEaseInEaseOut, kCAMediaTimingFunctionLinear,
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
    // clear, so the glass and the behind-window blur sample the desktop. its appearance
    // is the system's, light or dark
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    // a see-through window lets clicks through where it's transparent unless told
    // otherwise; this one takes them over its whole frame, faded out or not
    panel.setIgnoresMouseEvents(false);
    panel.setLevel(NSScreenSaverWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel
}

/// for a panel that darkens what's behind it whatever the system's appearance. its glass
/// and text have to be the dark ones
pub fn keep_dark(panel: &Panel) {
    // SAFETY: an immutable AppKit constant
    if let Some(appearance) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        panel.setAppearance(Some(&appearance));
    }
}

pub fn is_dark(appearance: &NSAppearance) -> bool {
    // SAFETY: immutable AppKit constants
    let (light, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    appearance
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[light, dark]))
        .is_some_and(|name| name.isEqualToString(dark))
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

/// false when the user has asked macOS to reduce motion. windows then cross-fade in and
/// out over `CROSS_FADE`, and nothing moves, blurs or flips.
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

/// how long a window takes to fade in or out under reduced motion
pub const CROSS_FADE: f64 = 0.25;

/// what an animation leaves behind
#[derive(Clone, Copy)]
pub enum Fill {
    /// shows the start value from now until `begin`, so entrances can be staggered, then
    /// hands back to the layer's own value
    Entrance,
    /// keeps the end value until the layer goes away
    Exit,
}

/// animates the number at `key_path` on an ease-in-out curve
pub fn property(
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

pub fn set_fill(anim: &CAAnimation, fill: Fill) {
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
    property("opacity", from, to, begin, duration, fill)
}

/// animates the blur that `attach_blur` adds
pub fn blur_radius(
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    fill: Fill,
) -> Retained<CABasicAnimation> {
    property("filters.blur.inputRadius", from, to, begin, duration, fill)
}

/// the scale and the offsets that make a layer of `size`, anchored at `anchor` (0…1 of its
/// size), look `factor` as big about `origin`, a point in its own coordinates. animated
/// together on one curve, the layer grows from that point or shrinks into it.
pub fn scale_about(
    size: NSSize,
    anchor: NSPoint,
    origin: NSPoint,
    factor: f64,
) -> [(&'static str, f64, f64); 3] {
    let shift = 1.0 - factor;
    [
        ("transform.scale", 1.0, factor),
        (
            "transform.translation.x",
            0.0,
            shift * (origin.x - anchor.x * size.width),
        ),
        (
            "transform.translation.y",
            0.0,
            shift * (origin.y - anchor.y * size.height),
        ),
    ]
}

/// for things that arrive: off the mark at once, then a long, gentle settle
pub fn arrive() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.32, 0.72, 0.0, 1.0)
}

/// slow off the mark, fastest midway and slow into the end, for a move between two held
/// states such as a blink's open and shut
pub fn travel() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.65, 0.0, 0.35, 1.0)
}

pub fn linear() -> Retained<CAMediaTimingFunction> {
    // SAFETY: an immutable Core Animation constant
    CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionLinear })
}

/// changes layer properties at once, without the implicit animation Core Animation adds
pub fn without_actions(f: impl FnOnce()) {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    f();
    CATransaction::commit();
}

/// the number at `key_path` as it's on screen now, partway through any animation
pub fn presented(layer: &CALayer, key_path: &str) -> Option<f64> {
    // SAFETY: the presentation layer is a read-only copy of the layer as it's drawn, and
    // reading a number through key-value coding leaves it alone
    let value = unsafe {
        let shown = layer.presentationLayer()?;
        shown.valueForKeyPath(&NSString::from_str(key_path))?
    };
    value.downcast::<NSNumber>().ok().map(|n| n.doubleValue())
}

/// a spring of unit mass. each of these overshoots a little, a quarter of the way to ringing
/// at most, and settles
#[derive(Clone, Copy)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
}

/// text and controls easing into place
pub const SETTLE: Spring = Spring {
    stiffness: 180.0,
    damping: 22.0,
};
/// a surface popping open
pub const POP: Spring = Spring {
    stiffness: 300.0,
    damping: 26.0,
};
/// the bead's flight across the display
pub const FLIGHT: Spring = Spring {
    stiffness: 150.0,
    damping: 20.0,
};

/// animates the number at `key_path` on `spring`, for as long as the spring takes to settle
pub fn spring(
    key_path: &str,
    from: f64,
    to: f64,
    begin: f64,
    spring: Spring,
    fill: Fill,
) -> Retained<CASpringAnimation> {
    let anim = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str(key_path)));
    // SAFETY: NSNumbers, the value type these key paths animate
    unsafe {
        anim.setFromValue(Some(&*NSNumber::numberWithDouble(from)));
        anim.setToValue(Some(&*NSNumber::numberWithDouble(to)));
    }
    anim.setMass(1.0);
    anim.setStiffness(spring.stiffness);
    anim.setDamping(spring.damping);
    anim.setInitialVelocity(0.0);
    anim.setDuration(anim.settlingDuration().min(1.2));
    anim.setBeginTime(begin);
    set_fill(&anim, fill);
    anim
}

/// springs into place from `from_y` points away: positive starts above, negative below
pub fn spring_y(from_y: f64, begin: f64) -> Retained<CASpringAnimation> {
    spring(
        "transform.translation.y",
        from_y,
        0.0,
        begin,
        SETTLE,
        Fill::Entrance,
    )
}

/// the blur `attach_blur` adds, from sharp up to `peak` early on and back to sharp, the way
/// something blurs as it picks up speed
pub fn blur_pulse(peak: f64, begin: f64, duration: f64) -> Retained<CAKeyframeAnimation> {
    let anim = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(
        "filters.blur.inputRadius",
    )));
    let numbers =
        |values: [f64; 3]| NSArray::from_retained_slice(&values.map(NSNumber::numberWithDouble));
    // SAFETY: an array of NSNumbers is an array of objects, and it's only read. NSNumbers
    // are the value type this key path animates
    unsafe {
        let values: Retained<NSArray> = Retained::cast_unchecked(numbers([0.0, peak, 0.0]));
        anim.setValues(Some(&values));
    }
    anim.setKeyTimes(Some(&numbers([0.0, 0.3, 1.0])));
    anim.setDuration(duration);
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

/// blurs a leaving layer to `radius`, from sharp or from however blurred its entrance had
/// left it, and holds the blur until the layer goes away. most of the blur comes in the
/// first frames, ahead of a fade over the same time, for the layer to be seen to soften
pub fn blur_out(layer: &CALayer, radius: f64, begin: f64, duration: f64) {
    let from = presented(layer, "filters.blur.inputRadius").unwrap_or(0.0);
    attach_blur(layer, 0.0);
    let blur = blur_radius(from, radius, begin, duration, Fill::Exit);
    blur.setTimingFunction(Some(&arrive()));
    layer.addAnimation_forKey(&blur, Some(&NSString::from_str(BLUR_OUT)));
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
