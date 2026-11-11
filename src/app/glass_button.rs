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
    key: &str,
    target: &AnyObject,
    action: Sel,
    style: GlassButtonStyle,
) -> Retained<NSGlassEffectView> {
    let keybind = parse_keybind(key);
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
        button.setKeyEquivalent(&kb.key_equivalent);
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
    key_equivalent: Retained<NSString>,
    mods: NSEventModifierFlags,
    label: String,
}

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
