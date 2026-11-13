use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSButton, NSColor, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSFontWeightMedium, NSForegroundColorAttributeName, NSGlassEffectView, NSGlassEffectViewStyle,
    NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use crate::config::{Key, KeySpec};

pub struct Hosted {
    button: Retained<NSButton>,
    /// the button's own key equivalent, which it has only while awake
    key: Option<(Retained<NSString>, NSEventModifierFlags)>,
    awake: Cell<bool>,
}

define_class!(
    /// a button in its container, which is what goes in the view hierarchy. the container
    /// can put the button to sleep: asleep it takes neither clicks nor its key, and looks no
    /// different, where a disabled button would dim its title
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
    }
);

impl ButtonHost {
    /// awake, the button takes clicks and its key. a new button is awake
    pub fn set_awake(&self, awake: bool) {
        let Hosted {
            button,
            key,
            awake: now,
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

pub enum GlassButtonWidth {
    Fixed(f64),
    Fit {
        min_width: f64,
        horizontal_padding: f64,
    },
}

pub struct GlassButtonStyle {
    pub height: f64,
    pub width: GlassButtonWidth,
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
    button.setBordered(false);
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

    if matches!(style.width, GlassButtonWidth::Fit { .. }) {
        button.sizeToFit();
    }

    let width = match style.width {
        GlassButtonWidth::Fixed(width) => width,
        GlassButtonWidth::Fit {
            min_width,
            horizontal_padding,
        } => (button.frame().size.width + horizontal_padding).max(min_width),
    };
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, style.height));
    button.setFrame(rect);

    let host = ButtonHost::alloc(mtm).set_ivars(Hosted {
        button: button.clone(),
        key,
        awake: Cell::new(false),
    });
    // SAFETY: NSView's designated initializer, with the argument type it declares
    let host: Retained<ButtonHost> = unsafe { msg_send![super(host), initWithFrame: rect] };
    host.setWantsLayer(true);
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), rect);
    glass.setCornerRadius(style.height / 2.0);
    glass.setStyle(NSGlassEffectViewStyle::Regular);
    glass.setContentView(Some(&button));
    host.addSubview(&glass);
    host.set_awake(true);
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
