use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSAppearanceCustomization, NSBezelStyle, NSButton, NSColor,
    NSControlBorderShape, NSControlSize, NSEvent, NSEventModifierFlags, NSFont,
    NSFontAttributeName, NSFontWeightMedium, NSForegroundColorAttributeName, NSGlassEffectView,
    NSGlassEffectViewStyle, NSTrackingArea, NSTrackingAreaOptions, NSView,
};
use objc2_core_graphics::CGColor;
use objc2_foundation::{
    MainThreadMarker, NSMutableAttributedString, NSPoint, NSPointInRect, NSRange, NSRect, NSSize,
    NSString,
};
use objc2_quartz_core::{CACurrentMediaTime, CALayer};

use super::panel::{Fill, arrive, is_dark, presented, property, without_actions};
use crate::config::{Key, KeySpec};

/// under the pointer a glow over a button comes up to this opacity: white in the dark
/// appearance, black in the light. nothing moves. coming is quicker than going
const HOVER_GLOW: f64 = 0.10;
const HOVER_IN: f64 = 0.15;
const HOVER_OUT: f64 = 0.22;

pub struct Hosted {
    button: Retained<NSButton>,
    /// over the button, in its shape
    glow: Retained<CALayer>,
    /// the button's own key equivalent, which it has only while awake
    key: Option<(Retained<NSString>, NSEventModifierFlags)>,
    awake: Cell<bool>,
}

define_class!(
    /// a button in its container, which is what goes in the view hierarchy. the container
    /// shows the hover, a glow that fades where AppKit's own would snap, and can put the
    /// button to sleep: asleep it takes neither clicks nor its key, and looks no different,
    /// where a disabled button would dim its title
    // SAFETY: NSView can be subclassed. each override has the signature AppKit declares,
    // which registering the class checks.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OmmaButtonHost"]
    #[ivars = Hosted]
    pub struct ButtonHost;

    impl ButtonHost {
        /// asleep, a click on the button stops at this view, which does nothing with it
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            // SAFETY: NSView's own hitTest:, with the argument and return types it declares
            let hit: *mut NSView = unsafe { msg_send![super(self), hitTest: point] };
            if hit.is_null() || self.ivars().awake.get() {
                return hit;
            }
            let this: &NSView = self;
            std::ptr::from_ref(this).cast_mut()
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            if self.ivars().awake.get() {
                self.hover(true);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.hover(false);
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            // SAFETY: NSView's own method, which takes and returns nothing
            let _: () = unsafe { msg_send![super(self), viewDidMoveToWindow] };
            self.tint();
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {
            // SAFETY: NSView's own method, which takes and returns nothing
            let _: () = unsafe { msg_send![super(self), viewDidChangeEffectiveAppearance] };
            self.tint();
        }
    }
);

impl ButtonHost {
    /// the glow lightens a dark button and darkens a light one
    fn tint(&self) {
        let white = if is_dark(&self.effectiveAppearance()) {
            1.0
        } else {
            0.0
        };
        without_actions(|| {
            self.ivars()
                .glow
                .setBackgroundColor(Some(&CGColor::new_generic_gray(white, 1.0)));
        });
    }

    /// awake, the button takes clicks and its key and shows a hover. a new button is awake
    pub fn set_awake(&self, awake: bool) {
        let Hosted {
            button,
            key,
            awake: now,
            ..
        } = self.ivars();
        now.set(awake);
        if let Some((key, mods)) = key {
            if awake {
                button.setKeyEquivalent(key);
                button.setKeyEquivalentModifierMask(*mods);
            } else {
                button.setKeyEquivalent(&NSString::new());
            }
        }
        // a pointer that was already on the button when it woke gets its hover
        if awake && let Some(window) = self.window() {
            let at = self.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
            if NSPointInRect(at, self.bounds()) {
                self.hover(true);
            }
        }
    }

    /// eases the button's glow up or away, from wherever it has got to
    fn hover(&self, on: bool) {
        let glow = &self.ivars().glow;
        let from = presented(glow, "opacity").unwrap_or(0.0);
        let to = if on { HOVER_GLOW } else { 0.0 };
        let duration = if on { HOVER_IN } else { HOVER_OUT };
        let anim = property(
            "opacity",
            from,
            to,
            CACurrentMediaTime(),
            duration,
            Fill::Exit,
        );
        anim.setTimingFunction(Some(&arrive()));
        glow.addAnimation_forKey(&anim, Some(&NSString::from_str("opacity")));
    }
}

/// a button's keyboard shortcut. a local one is the button's key equivalent; a global one is
/// held elsewhere, so the button only shows its hint.
#[derive(Clone, Copy)]
pub enum Shortcut {
    Local(KeySpec),
    Global(KeySpec),
}

impl Shortcut {
    fn spec(self) -> KeySpec {
        match self {
            Shortcut::Local(spec) | Shortcut::Global(spec) => spec,
        }
    }
}

/// the buttons a window shows: skip and postpone, when allowed
#[derive(Clone, Copy)]
pub struct Buttons {
    pub skip: Option<Button>,
    pub postpone: Option<Button>,
}

#[derive(Clone, Copy)]
pub struct Button {
    pub key: Option<KeySpec>,
}

/// what the button is made of, for the surface it sits on
pub enum Surface {
    /// a capsule of the system's glass under a title. not AppKit's glass bezel, whose
    /// hover snaps from one colour to the other
    Glass,
    /// a standard push button, for controls already on a surface such as the warning's
    Fill,
}

pub struct GlassButtonStyle {
    pub surface: Surface,
    pub height: f64,
    /// the button is as wide as its title plus `horizontal_padding`, and no narrower than
    /// `min_width`
    pub min_width: f64,
    pub horizontal_padding: f64,
    pub title_font_size: f64,
    pub hint_font_size: f64,
}

pub fn make_glass_button(
    mtm: MainThreadMarker,
    title: &str,
    shortcut: Option<Shortcut>,
    target: &AnyObject,
    action: Sel,
    style: &GlassButtonStyle,
) -> Retained<ButtonHost> {
    let keybind = shortcut.map(|shortcut| key_equivalent(shortcut.spec()));
    let title_ns = NSString::from_str(title);
    let full = match &keybind {
        Some(kb) => format!("{title}  {}", kb.label),
        None => title.to_string(),
    };
    let full_ns = NSString::from_str(&full);
    // SAFETY: `target` implements `action`, and outlives the button: it's the delegate, which
    // lives until the process exits
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&full_ns, Some(target), Some(action), mtm)
    };
    match style.surface {
        Surface::Glass => button.setBordered(false),
        Surface::Fill => {
            button.setBordered(true);
            button.setBezelStyle(NSBezelStyle::Push);
            button.setBorderShape(NSControlBorderShape::Capsule);
        }
    }
    button.setControlSize(NSControlSize::Large);
    button.setAccessibilityLabel(Some(&title_ns));

    let attributed =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &full_ns);
    // SAFETY: an immutable AppKit constant
    let medium = unsafe { NSFontWeightMedium };
    let title_font = NSFont::systemFontOfSize_weight(style.title_font_size, medium);
    let title_range = NSRange::new(0, title_ns.length());
    // SAFETY: the attribute names are immutable AppKit constants, and each value has the type
    // its attribute takes: an NSColor for the color, an NSFont for the font
    unsafe {
        attributed.addAttribute_value_range(
            NSForegroundColorAttributeName,
            &NSColor::labelColor(),
            title_range,
        );
        attributed.addAttribute_value_range(NSFontAttributeName, &title_font, title_range);
    }

    if keybind.is_some() {
        let hint_font = NSFont::systemFontOfSize_weight(style.hint_font_size, medium);
        let dim = NSColor::secondaryLabelColor();
        let start = title_ns.length();
        let hint_range = NSRange::new(start, full_ns.length() - start);
        // SAFETY: as for the title
        unsafe {
            attributed.addAttribute_value_range(NSForegroundColorAttributeName, &dim, hint_range);
            attributed.addAttribute_value_range(NSFontAttributeName, &hint_font, hint_range);
        }
    }
    button.setAttributedTitle(&attributed);

    let key = match (shortcut, &keybind) {
        (Some(Shortcut::Local(_)), Some(kb)) => {
            Some((NSString::from_str(&kb.key_equivalent), kb.mods))
        }
        _ => None,
    };

    button.sizeToFit();

    let width = (button.frame().size.width + style.horizontal_padding).max(style.min_width);
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, style.height));
    button.setFrame(rect);

    let glow = CALayer::new();
    glow.setFrame(rect);
    glow.setCornerRadius(style.height / 2.0);
    glow.setBackgroundColor(Some(&CGColor::new_generic_gray(1.0, 1.0)));
    glow.setOpacity(0.0);
    glow.setZPosition(1.0);
    let host = ButtonHost::alloc(mtm).set_ivars(Hosted {
        button: button.clone(),
        glow: glow.clone(),
        key,
        awake: Cell::new(false),
    });
    // SAFETY: NSView's designated initializer, with the argument type it declares
    let host: Retained<ButtonHost> = unsafe { msg_send![super(host), initWithFrame: rect] };
    host.setWantsLayer(true);
    match style.surface {
        Surface::Glass => {
            let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), rect);
            glass.setCornerRadius(style.height / 2.0);
            glass.setStyle(NSGlassEffectViewStyle::Regular);
            glass.setContentView(Some(&button));
            host.addSubview(&glass);
        }
        Surface::Fill => host.addSubview(&button),
    }
    if let Some(layer) = host.layer() {
        layer.addSublayer(&glow);
    }
    host.set_awake(true);
    // the area follows the view's visible rect, and reports the pointer whether or not
    // omma is active, which it never is
    let owner: &AnyObject = &host;
    // SAFETY: the owner is the view the area goes on, which implements the two methods
    // these options make it send. the area doesn't outlive that view
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(owner),
            None,
        )
    };
    host.addTrackingArea(&area);
    host
}

struct KeyBind {
    key_equivalent: String,
    mods: NSEventModifierFlags,
    /// the hint drawn on the button
    label: String,
}

fn key_equivalent(spec: KeySpec) -> KeyBind {
    let (key_equivalent, label) = match spec.key {
        Key::Escape => ("\u{1b}".into(), "Esc".into()),
        Key::Space => (" ".into(), "Space".into()),
        Key::Enter => ("\r".into(), "↩".into()),
        Key::Tab => ("\t".into(), "⇥".into()),
        // the delete key sends DEL (NSDeleteCharacter), not backspace
        Key::Delete => ("\u{7f}".into(), "⌫".into()),
        // AppKit compares the key equivalent with the typed character, shift applied
        Key::Char(c) => {
            let upper: String = c.to_uppercase().collect();
            let typed = if spec.mods.shift {
                upper.clone()
            } else {
                c.to_string()
            };
            (typed, upper)
        }
    };
    let mut mods = NSEventModifierFlags::empty();
    let mut symbols = String::new();
    for (held, flag, symbol) in [
        (spec.mods.ctrl, NSEventModifierFlags::Control, '⌃'),
        (spec.mods.alt, NSEventModifierFlags::Option, '⌥'),
        (spec.mods.shift, NSEventModifierFlags::Shift, '⇧'),
        (spec.mods.cmd, NSEventModifierFlags::Command, '⌘'),
    ] {
        if held {
            mods |= flag;
            symbols.push(symbol);
        }
    }
    KeyBind {
        key_equivalent,
        mods,
        label: format!("{symbols}{label}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// registering the class makes objc2 check each override against the signature AppKit
    /// declares, which otherwise happens only at the first button
    #[test]
    fn the_button_host_class_registers() {
        use objc2::ClassType;
        let _ = ButtonHost::class();
    }

    fn bind(spec: &str) -> (String, NSEventModifierFlags, String) {
        let kb = key_equivalent(spec.parse().unwrap());
        (kb.key_equivalent, kb.mods, kb.label)
    }

    #[test]
    fn named_keys_send_what_appkit_receives() {
        let none = NSEventModifierFlags::empty();
        assert_eq!(bind("esc"), ("\u{1b}".into(), none, "Esc".into()));
        assert_eq!(bind("space"), (" ".into(), none, "Space".into()));
        assert_eq!(bind("return"), ("\r".into(), none, "↩".into()));
        assert_eq!(bind("tab"), ("\t".into(), none, "⇥".into()));
        assert_eq!(bind("backspace"), ("\u{7f}".into(), none, "⌫".into()));
    }

    #[test]
    fn shift_turns_a_letter_uppercase_and_modifiers_show_in_apple_order() {
        assert_eq!(
            bind("s"),
            ("s".into(), NSEventModifierFlags::empty(), "S".into())
        );
        assert_eq!(
            bind("shift+s"),
            ("S".into(), NSEventModifierFlags::Shift, "⇧S".into())
        );
        assert_eq!(
            bind("cmd+shift+alt+ctrl+delete"),
            (
                "\u{7f}".into(),
                NSEventModifierFlags::Control
                    | NSEventModifierFlags::Option
                    | NSEventModifierFlags::Shift
                    | NSEventModifierFlags::Command,
                "⌃⌥⇧⌘⌫".into()
            )
        );
    }
}
