//! The break screen: one fullscreen overlay window per display in Apple's
//! Liquid Glass style — a frosted, blurred backdrop (the desktop, softly
//! defocused) dimmed evenly for the eyes, with the break message, a live
//! countdown and (unless strict) Skip / Postpone laid directly on the glass.
//! The buttons are `NSGlassEffectView` capsules (the Control Center chip look)
//! wrapping a borderless `NSButton` that fires `skipBreak:` / `postponeBreak:`
//! on the delegate.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
    NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor,
    NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightThin,
    NSForegroundColorAttributeName, NSGlassEffectView, NSGlassEffectViewStyle, NSPanel, NSScreen,
    NSTextAlignment, NSTextField, NSTitlePosition, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString,
};

/// Window level above normal windows and fullscreen apps.
const SCREEN_SAVER_LEVEL: isize = 1000;
/// Darkness of the dim scrim laid over the blur. High enough to even out a
/// busy desktop into a calm, uniformly dark backdrop with consistent contrast.
const SCRIM_ALPHA: f64 = 0.6;
/// Height of the glass capsule buttons.
const BUTTON_HEIGHT: f64 = 46.0;

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
    visible: bool,
}

impl BreakScreen {
    pub fn new(mtm: MainThreadMarker) -> BreakScreen {
        BreakScreen {
            mtm,
            windows: Vec::new(),
            countdown_labels: Vec::new(),
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
        self.hide();
        let mtm = self.mtm;
        let weight_regular = unsafe { NSFontWeightRegular };
        let weight_thin = unsafe { NSFontWeightThin };

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

            // Frosted-glass backdrop: blur the desktop behind the overlay.
            let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
            blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            blur.setState(NSVisualEffectState::Active);
            content.addSubview(&blur);

            // Dim scrim over the blur so the break stays restful for the eyes.
            let scrim = NSBox::initWithFrame(NSBox::alloc(mtm), full);
            scrim.setBoxType(NSBoxType::Custom);
            scrim.setTitlePosition(NSTitlePosition::NoTitle);
            scrim.setBorderWidth(0.0);
            scrim.setCornerRadius(0.0);
            scrim.setFillColor(&NSColor::colorWithWhite_alpha(0.0, SCRIM_ALPHA));
            content.addSubview(&scrim);

            // Break message, laid directly on the glass (wraps to two lines).
            let msg_w = (w - 160.0).min(1000.0);
            let title_font = NSFont::systemFontOfSize_weight(28.0, weight_regular);
            let msg_color = NSColor::colorWithWhite_alpha(1.0, 0.92);
            let name_label = make_label(
                mtm,
                name,
                &title_font,
                &msg_color,
                NSRect::new(NSPoint::new(cx - msg_w / 2.0, cy + 44.0), NSSize::new(msg_w, 80.0)),
                2,
            );
            content.addSubview(&name_label);

            // Big countdown number, monospaced digits so it doesn't jitter.
            let count_font = NSFont::monospacedDigitSystemFontOfSize_weight(100.0, weight_thin);
            let white = NSColor::whiteColor();
            let count_label = make_label(
                mtm,
                &duration.to_string(),
                &count_font,
                &white,
                NSRect::new(NSPoint::new(cx - 250.0, cy - 90.0), NSSize::new(500.0, 130.0)),
                1,
            );
            content.addSubview(&count_label);
            self.countdown_labels.push(count_label);

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
                    content.addSubview(button);
                    x += bw + gap;
                }
            }

            window.orderFrontRegardless();
            self.windows.push(window);
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
            label.setStringValue(&text);
        }
    }

    pub fn hide(&mut self) {
        let had_windows = !self.windows.is_empty();
        for window in self.windows.drain(..) {
            window.orderOut(None);
        }
        self.countdown_labels.clear();
        self.visible = false;

        // Drop back to a menu-bar-only Accessory app (we promoted to Regular in
        // show() so the overlay could take keyboard focus); the Dock icon goes
        // away and focus returns to the user's previous app. Only when we
        // actually had an overlay up, so an empty hide() stays a no-op.
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
    label.setFrame(frame);
    label.setAlignment(NSTextAlignment::Center);
    label.setFont(Some(font));
    label.setTextColor(Some(color));
    label.setMaximumNumberOfLines(max_lines);
    label
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
    let button =
        unsafe { NSButton::buttonWithTitle_target_action(&full_ns, Some(target), Some(action), mtm) };
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
