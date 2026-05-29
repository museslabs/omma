//! Visual preview for dialing in the Liquid Glass break screen.
//! Buttons are NSGlassEffectView capsules (the Control Center chip look).
//! Env: SCRIM (dim alpha), TINT (capsule white-tint alpha), STYLE (0=Regular,
//! 1=Clear), FS=1 (fullscreen), LEVEL (window level).

#[cfg(target_os = "macos")]
fn main() {
    use objc2::rc::Retained;
    use objc2::{AnyThread, MainThreadOnly};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication,
        NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor,
        NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightThin,
        NSForegroundColorAttributeName, NSGlassEffectView, NSGlassEffectViewStyle, NSScreen,
        NSTextAlignment, NSTextField, NSTitlePosition, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{
        MainThreadMarker, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString,
    };

    let scrim: f64 = std::env::var("SCRIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.6);
    let tint: f64 = std::env::var("TINT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let style: isize = std::env::var("STYLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let fs: bool = std::env::var("FS").map(|v| v == "1").unwrap_or(false);
    let level: isize = std::env::var("LEVEL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let screen = NSScreen::mainScreen(mtm).unwrap();
    let (win_rect, sty) = if fs {
        (screen.frame(), NSWindowStyleMask::Borderless)
    } else {
        (
            NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(1100.0, 720.0)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
        )
    };
    let w = win_rect.size.width;
    let h = win_rect.size.height;

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            win_rect,
            sty,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    if fs {
        window.setLevel(level);
    }
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    if let Some(a) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        window.setAppearance(Some(&a));
    }
    let content = window.contentView().unwrap();
    let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));

    let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
    blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    content.addSubview(&blur);

    if scrim > 0.0 {
        let s = NSBox::initWithFrame(NSBox::alloc(mtm), full);
        s.setBoxType(NSBoxType::Custom);
        s.setTitlePosition(NSTitlePosition::NoTitle);
        s.setBorderWidth(0.0);
        s.setCornerRadius(0.0);
        s.setFillColor(&NSColor::colorWithWhite_alpha(0.0, scrim));
        content.addSubview(&s);
    }

    let cx = w / 2.0;
    let cy = h / 2.0;

    let msg = {
        let s = NSString::from_str("Focus on a point in the far distance");
        let l = NSTextField::labelWithString(&s, mtm);
        l.setFrame(NSRect::new(
            NSPoint::new(cx - 450.0, cy + 70.0),
            NSSize::new(900.0, 44.0),
        ));
        l.setAlignment(NSTextAlignment::Center);
        l.setFont(Some(&NSFont::systemFontOfSize_weight(28.0, unsafe {
            NSFontWeightRegular
        })));
        l.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.95)));
        l
    };
    content.addSubview(&msg);

    let count = {
        let s = NSString::from_str("15");
        let l = NSTextField::labelWithString(&s, mtm);
        l.setFrame(NSRect::new(
            NSPoint::new(cx - 250.0, cy - 90.0),
            NSSize::new(500.0, 130.0),
        ));
        l.setAlignment(NSTextAlignment::Center);
        l.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
            100.0,
            unsafe { NSFontWeightThin },
        )));
        l.setTextColor(Some(&NSColor::whiteColor()));
        l
    };
    content.addSubview(&count);

    // Glass capsule buttons (Control Center chip look).
    let make_btn = |title: &str| -> Retained<NSGlassEffectView> {
        let s = NSString::from_str(title);
        let button = unsafe { NSButton::buttonWithTitle_target_action(&s, None, None, mtm) };
        button.setBordered(false);
        let font = NSFont::systemFontOfSize_weight(16.0, unsafe { NSFontWeightMedium });
        let astr =
            NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &s);
        let white = NSColor::whiteColor();
        let range = NSRange::new(0, s.length());
        unsafe {
            astr.addAttribute_value_range(NSForegroundColorAttributeName, &*white, range);
            astr.addAttribute_value_range(NSFontAttributeName, &*font, range);
        }
        button.setAttributedTitle(&astr);
        button.sizeToFit();
        let tw = button.frame().size.width;
        let ch = 46.0;
        let cw = (tw + 48.0).max(120.0);
        let cap_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(cw, ch));
        let cap = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), cap_rect);
        cap.setCornerRadius(ch / 2.0);
        cap.setStyle(if style == 1 {
            NSGlassEffectViewStyle::Clear
        } else {
            NSGlassEffectViewStyle::Regular
        });
        if tint > 0.0 {
            cap.setTintColor(Some(&NSColor::colorWithWhite_alpha(1.0, tint)));
        }
        button.setFrame(cap_rect);
        cap.setContentView(Some(&button));
        cap
    };
    let skip = make_btn("Skip");
    let postpone = make_btn("Postpone");
    let sw = skip.frame().size.width;
    let pw = postpone.frame().size.width;
    let bh = skip.frame().size.height;
    let gap = 16.0;
    let total = sw + pw + gap;
    let by = cy - 170.0;
    skip.setFrame(NSRect::new(
        NSPoint::new(cx - total / 2.0, by),
        NSSize::new(sw, bh),
    ));
    postpone.setFrame(NSRect::new(
        NSPoint::new(cx - total / 2.0 + sw + gap, by),
        NSSize::new(pw, bh),
    ));
    content.addSubview(&skip);
    content.addSubview(&postpone);

    window.makeKeyAndOrderFront(None);
    if fs {
        window.orderFrontRegardless();
    }
    app.activate();
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}
