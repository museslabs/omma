use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSButton, NSColor, NSEventModifierFlags, NSFont, NSFontAttributeName, NSFontWeightMedium,
    NSForegroundColorAttributeName, NSGlassEffectView, NSGlassEffectViewStyle,
};
use objc2_foundation::{
    MainThreadMarker, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use crate::config::{Key, KeySpec};

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

impl GlassButtonStyle {
    pub fn fixed(width: f64, height: f64, title_font_size: f64, hint_font_size: f64) -> Self {
        GlassButtonStyle {
            height,
            width: GlassButtonWidth::Fixed(width),
            title_font_size,
            hint_font_size,
        }
    }

    pub fn fit(
        height: f64,
        min_width: f64,
        horizontal_padding: f64,
        title_font_size: f64,
        hint_font_size: f64,
    ) -> Self {
        GlassButtonStyle {
            height,
            width: GlassButtonWidth::Fit {
                min_width,
                horizontal_padding,
            },
            title_font_size,
            hint_font_size,
        }
    }
}

pub fn make_glass_button(
    mtm: MainThreadMarker,
    title: &str,
    key: Option<KeySpec>,
    target: &AnyObject,
    action: Sel,
    style: GlassButtonStyle,
) -> Retained<NSGlassEffectView> {
    let keybind = key.map(key_equivalent);
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

    let attributed =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &full_ns);
    let title_font =
        NSFont::systemFontOfSize_weight(style.title_font_size, unsafe { NSFontWeightMedium });
    let title_range = NSRange::new(0, title_ns.length());
    unsafe {
        attributed.addAttribute_value_range(
            NSForegroundColorAttributeName,
            &NSColor::whiteColor(),
            title_range,
        );
        attributed.addAttribute_value_range(NSFontAttributeName, &title_font, title_range);
    }

    if keybind.is_some() {
        let hint_font =
            NSFont::systemFontOfSize_weight(style.hint_font_size, unsafe { NSFontWeightMedium });
        let dim = NSColor::colorWithWhite_alpha(1.0, 0.55);
        let start = title_ns.length();
        let hint_range = NSRange::new(start, full_ns.length() - start);
        unsafe {
            attributed.addAttribute_value_range(NSForegroundColorAttributeName, &dim, hint_range);
            attributed.addAttribute_value_range(NSFontAttributeName, &hint_font, hint_range);
        }
    }
    button.setAttributedTitle(&attributed);

    if let Some(kb) = &keybind {
        button.setKeyEquivalent(&NSString::from_str(&kb.key_equivalent));
        button.setKeyEquivalentModifierMask(kb.mods);
    }

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

    let capsule = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), rect);
    capsule.setWantsLayer(true);
    capsule.setCornerRadius(style.height / 2.0);
    capsule.setStyle(NSGlassEffectViewStyle::Regular);
    capsule.setContentView(Some(&button));
    capsule
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
        // NSDeleteCharacter: the delete key sends DEL, not backspace
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
