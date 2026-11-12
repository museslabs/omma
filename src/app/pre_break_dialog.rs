use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSGlassEffectView,
    NSGlassEffectViewStyle, NSScreen, NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CACurrentMediaTime;

use super::glass_button::{
    Buttons, GlassButtonStyle, GlassButtonWidth, Shortcut, make_glass_button,
};
use super::panel::{
    Fill, LabelStyle, Panel, after, attach_blur, blur_out, blur_radius, detach_blur_after, fade,
    make_label, make_panel, slide_y,
};

const PANEL_W: f64 = 340.0;
const PANEL_H: f64 = 112.0;
const BUTTON_W: f64 = 140.0;
const BUTTON_H: f64 = 36.0;
const HIDE_DURATION: f64 = 0.24;
const BUTTON_STYLE: GlassButtonStyle = GlassButtonStyle {
    height: BUTTON_H,
    width: GlassButtonWidth::Fixed(BUTTON_W),
    title_font_size: 13.0,
    hint_font_size: 11.0,
};

pub struct PreBreakDialog {
    mtm: MainThreadMarker,
    /// None from the moment `hide` starts
    shown: Option<Showing>,
}

struct Showing {
    window: Retained<Panel>,
    glass: Retained<NSGlassEffectView>,
    body: Retained<NSView>,
    countdown: Retained<NSTextField>,
    buttons: Vec<Retained<NSButton>>,
}

impl PreBreakDialog {
    pub fn new(mtm: MainThreadMarker) -> PreBreakDialog {
        PreBreakDialog { mtm, shown: None }
    }

    /// a skip key or a postpone key is a global shortcut: the button shows its hint and the
    /// shortcut is held elsewhere. without skip, the warning offers Dismiss.
    pub fn show(&mut self, seconds_until_break: u64, buttons: Buttons, target: &AnyObject) {
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

        // never key: the warning takes no focus from the app the user is in
        let window = make_panel(self.mtm, frame, false);
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
            layer.setMasksToBounds(true);
        }
        content.addSubview(&glass);

        let body = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H)),
        );
        body.setWantsLayer(true);
        glass.setContentView(Some(&body));

        // SAFETY: immutable AppKit constants
        let (semibold, medium) = unsafe { (NSFontWeightSemibold, NSFontWeightMedium) };
        let title = make_label(
            self.mtm,
            "Break soon",
            NSRect::new(NSPoint::new(28.0, 65.0), NSSize::new(130.0, 18.0)),
            &LabelStyle {
                font: &NSFont::systemFontOfSize_weight(14.0, semibold),
                color: &NSColor::colorWithWhite_alpha(1.0, 0.76),
                alignment: NSTextAlignment::Left,
                max_lines: 1,
            },
        );
        body.addSubview(&title);

        let countdown = make_label(
            self.mtm,
            &fmt_seconds(seconds_until_break),
            NSRect::new(NSPoint::new(28.0, 25.0), NSSize::new(130.0, 34.0)),
            &LabelStyle {
                font: &NSFont::systemFontOfSize_weight(28.0, medium),
                color: &NSColor::colorWithWhite_alpha(1.0, 0.95),
                alignment: NSTextAlignment::Left,
                max_lines: 1,
            },
        );
        body.addSubview(&countdown);

        let (primary_title, primary_action, primary_key) = match buttons.skip {
            Some(skip) => ("Skip", sel!(skip:), skip.key.map(Shortcut::Global)),
            None => ("Dismiss", sel!(dismissWarning:), None),
        };
        let button_x = PANEL_W - BUTTON_W - 28.0;
        let primary_y = if buttons.postpone.is_some() {
            18.0
        } else {
            38.0
        };
        let primary = make_glass_button(
            self.mtm,
            primary_title,
            primary_key,
            target,
            primary_action,
            &BUTTON_STYLE,
        );
        primary.capsule.setFrame(NSRect::new(
            NSPoint::new(button_x, primary_y),
            NSSize::new(BUTTON_W, BUTTON_H),
        ));
        body.addSubview(&primary.capsule);
        let mut shown_buttons = vec![primary.button];

        if let Some(postpone) = buttons.postpone {
            let postpone = make_glass_button(
                self.mtm,
                "Postpone",
                postpone.key.map(Shortcut::Global),
                target,
                sel!(postpone:),
                &BUTTON_STYLE,
            );
            postpone.capsule.setFrame(NSRect::new(
                NSPoint::new(button_x, 58.0),
                NSSize::new(BUTTON_W, BUTTON_H),
            ));
            body.addSubview(&postpone.capsule);
            shown_buttons.push(postpone.button);
        }

        if let Some(layer) = glass.layer() {
            let now = CACurrentMediaTime();
            layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.16, Fill::Entrance), None);
            layer.addAnimation_forKey(&slide_y(28.0, 0.0, now, 0.22, Fill::Entrance), None);
        }
        if let Some(layer) = body.layer() {
            let now = CACurrentMediaTime();
            attach_blur(&layer, 0.0);
            layer.addAnimation_forKey(&fade(0.0, 1.0, now + 0.03, 0.2, Fill::Entrance), None);
            layer.addAnimation_forKey(
                &blur_radius(10.0, 0.0, now + 0.03, 0.28, Fill::Entrance),
                None,
            );
            detach_blur_after(self.mtm, layer, 0.36);
        }

        window.orderFrontRegardless();
        self.shown = Some(Showing {
            window,
            glass,
            body,
            countdown,
            buttons: shown_buttons,
        });
    }

    pub fn is_visible(&self) -> bool {
        self.shown.is_some()
    }

    pub fn update_countdown(&self, seconds_until_break: u64) {
        if let Some(shown) = &self.shown {
            let text = NSString::from_str(&fmt_seconds(seconds_until_break));
            shown.countdown.setStringValue(&text);
        }
    }

    pub fn hide(&mut self) {
        let Some(Showing {
            window,
            glass,
            body,
            buttons,
            ..
        }) = self.shown.take()
        else {
            return;
        };
        // inert while it fades: a disabled button takes no clicks
        for button in &buttons {
            button.setEnabled(false);
        }
        if let Some(layer) = glass.layer() {
            let now = CACurrentMediaTime();
            layer.addAnimation_forKey(&fade(1.0, 0.0, now, HIDE_DURATION, Fill::Exit), None);
            blur_out(&layer, 22.0, now, HIDE_DURATION);
            layer.addAnimation_forKey(&slide_y(0.0, 28.0, now, HIDE_DURATION, Fill::Exit), None);
        }
        if let Some(layer) = body.layer() {
            let now = CACurrentMediaTime();
            layer.addAnimation_forKey(&fade(1.0, 0.0, now, HIDE_DURATION, Fill::Exit), None);
            blur_out(&layer, 18.0, now, HIDE_DURATION);
        }

        after(self.mtm, HIDE_DURATION + 0.03, move || {
            window.orderOut(None)
        });
    }
}

fn fmt_seconds(seconds: u64) -> String {
    format!("{seconds}s")
}
