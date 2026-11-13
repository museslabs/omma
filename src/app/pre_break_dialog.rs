//! the warning: a mati bead drops off the menu-bar icon on a thread, and a compact glass
//! notification beside it counts down with skip and postpone buttons. they're two panels,
//! which keeps the clear space between them from taking clicks.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSColor, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSGlassEffectView,
    NSGlassEffectViewStyle, NSScreen, NSTextAlignment, NSTextField, NSView,
};
use objc2_core_graphics::CGColor;
use objc2_foundation::{MainThreadMarker, NSPoint, NSPointInRect, NSRect, NSSize, NSString};
use objc2_quartz_core::CACurrentMediaTime;

use super::charm::Charm;
use super::glass_button::{
    ButtonHost, Buttons, GlassButtonStyle, Shortcut, Surface, make_glass_button,
};
use super::panel::{
    CROSS_FADE, Fill, LabelStyle, Panel, after, animations_on, arrive, attach_blur, blur_out,
    blur_radius, detach_blur_after, fade, make_label, make_panel, presented, solid_surfaces,
    system_is_dark,
};

const BUTTON_H: f64 = 40.0;
/// from the capsule's edge to its buttons, and to the top of its text
const PAD: f64 = 16.0;
/// to the text's left edge: a label keeps 2 pt clear of its own frame, which puts the letters
/// 20 pt in, level with where their tops come under `PAD`
const TEXT_INSET: f64 = 18.0;
/// between the heading and the line under it, and between that line and the buttons
const LINE_GAP: f64 = 2.0;
const TEXT_GAP: f64 = 14.0;
/// the type: a heading, the line under it and the buttons, on macOS's text sizes
const HEADING_SIZE: f64 = 15.0;
const TEXT_SIZE: f64 = 13.0;
const BUTTON_GAP: f64 = 8.0;
/// between "Your break begins in" and the count
const COUNT_GAP: f64 = 4.0;
/// the capsule's narrowest, and the closest it comes to the display's edge
const CAPSULE_MIN_W: f64 = 336.0;
const SCREEN_EDGE: f64 = 12.0;
/// from the capsule's right edge to the thread, and from the menu bar down to the capsule
const CAPSULE_GAP: f64 = 46.0;
const CAPSULE_DROP: f64 = 12.0;
const THREAD: f64 = 42.0;
const BEAD: f64 = 64.0;
/// the charm's panel: wide enough for the swing, rubber band included, and deep enough for
/// thread and bead
const CHARM_W: f64 = 240.0;
const CHARM_H: f64 = THREAD + BEAD + 18.0;
/// the capsule's corners, concentric with its buttons': theirs, plus the padding between
const CAPSULE_RADIUS: f64 = BUTTON_H / 2.0 + PAD;
/// clear space around the capsule in its panel, for the glass's own shadow.
/// it reaches up to the menu bar and no further, so the bar still takes its clicks
const CAPSULE_MARGIN: f64 = CAPSULE_DROP;
/// the capsule fades in where it sits, a beat after the bead drops, as its text and buttons
/// come out of a slight blur, and it leaves the same way. nothing moves or changes size
const CONTENT_BLUR: f64 = 4.0;
const SHOW_DELAY: f64 = 0.10;
const SHOW_DURATION: f64 = 0.22;
const HIDE_DURATION: f64 = 0.18;
const BUTTON_STYLE: GlassButtonStyle = GlassButtonStyle {
    surface: Surface::Fill,
    height: BUTTON_H,
    min_width: 0.0,
    horizontal_padding: 24.0,
    title_font_size: TEXT_SIZE,
    hint_font_size: 11.0,
};

/// the warning's bead as the break screen's bead takes over from it
pub struct HandOff {
    /// in screen coordinates
    pub frame: NSRect,
    /// how far its rings lean toward the pointer, as `Bead::look` takes it
    pub look: (f64, f64),
}

pub struct PreBreakDialog {
    mtm: MainThreadMarker,
    /// None from the moment `hide` starts
    shown: Option<Showing>,
}

struct Showing {
    /// `animations_on()` at `show`, so `hide` animates only if the entrance did
    animate: bool,
    capsule_window: Retained<Panel>,
    capsule: Retained<NSView>,
    /// the text and buttons, which blur as the capsule comes and goes
    body: Retained<NSView>,
    countdown: Retained<NSTextField>,
    buttons: Vec<Retained<ButtonHost>>,
    charm_window: Retained<Panel>,
    charm: Retained<Charm>,
}

impl PreBreakDialog {
    pub fn new(mtm: MainThreadMarker) -> PreBreakDialog {
        PreBreakDialog { mtm, shown: None }
    }

    /// `anchor` is under the menu-bar icon on the bar's bottom edge, in screen coordinates;
    /// without it the warning hangs at the top right of the main display. a skip or postpone key is a
    /// global shortcut, held by `Hotkeys`; the button only shows its hint. without skip, the
    /// warning offers Dismiss.
    pub fn show(
        &mut self,
        seconds_until_break: u64,
        buttons: Buttons,
        target: &AnyObject,
        anchor: Option<NSPoint>,
    ) {
        self.hide(false);
        let mtm = self.mtm;
        let Some(anchor) = anchor.or_else(|| fallback_anchor(mtm)) else {
            return;
        };

        // SAFETY: immutable AppKit constants
        let (medium, semibold) = unsafe { (NSFontWeightMedium, NSFontWeightSemibold) };
        let heading = make_label(
            mtm,
            "Time to rest your eyes",
            NSRect::ZERO,
            &LabelStyle {
                font: &NSFont::systemFontOfSize_weight(HEADING_SIZE, semibold),
                color: &NSColor::labelColor(),
                alignment: NSTextAlignment::Left,
                max_lines: 1,
            },
        );
        heading.sizeToFit();
        let prefix = make_label(
            mtm,
            "Your break begins in",
            NSRect::ZERO,
            &LabelStyle {
                font: &NSFont::systemFontOfSize_weight(TEXT_SIZE, medium),
                color: &NSColor::secondaryLabelColor(),
                alignment: NSTextAlignment::Left,
                max_lines: 1,
            },
        );
        prefix.sizeToFit();
        let countdown = make_label(
            mtm,
            "10:00",
            NSRect::ZERO,
            &LabelStyle {
                font: &NSFont::monospacedDigitSystemFontOfSize_weight(TEXT_SIZE, medium),
                color: &NSColor::labelColor(),
                alignment: NSTextAlignment::Left,
                max_lines: 1,
            },
        );
        // as wide as the longest count, to keep any count inside the capsule
        countdown.sizeToFit();
        countdown.setStringValue(&NSString::from_str(&fmt_seconds(seconds_until_break)));

        let mut row = Vec::new();
        if let Some(postpone) = buttons.postpone {
            row.push(make_glass_button(
                mtm,
                "Postpone",
                postpone.key.map(Shortcut::Global),
                target,
                sel!(postpone:),
                &BUTTON_STYLE,
            ));
        }
        row.push(match buttons.skip {
            Some(skip) => make_glass_button(
                mtm,
                "Skip",
                skip.key.map(Shortcut::Global),
                target,
                sel!(skip:),
                &BUTTON_STYLE,
            ),
            None => make_glass_button(
                mtm,
                "Dismiss",
                None,
                target,
                sel!(dismissWarning:),
                &BUTTON_STYLE,
            ),
        });

        let prefix_w = prefix.frame().size.width;
        let count_w = countdown.frame().size.width;
        let buttons_w: f64 = row.iter().map(|b| b.frame().size.width).sum::<f64>()
            + BUTTON_GAP * (row.len() as f64 - 1.0);
        let capsule_w = CAPSULE_MIN_W
            .max(PAD + buttons_w + PAD)
            .max(TEXT_INSET + heading.frame().size.width + TEXT_INSET)
            .max(TEXT_INSET + prefix_w + COUNT_GAP + count_w + TEXT_INSET);
        let heading_h = heading.frame().size.height;
        let text_h = prefix.frame().size.height;
        let capsule_h = PAD + heading_h + LINE_GAP + text_h + TEXT_GAP + BUTTON_H + PAD;

        let screen = screen_at(mtm, anchor);
        let screen_frame = screen
            .as_ref()
            .map_or(NSRect::ZERO, |screen| screen.frame());
        let left = screen_frame.origin.x + SCREEN_EDGE;
        let right = screen_frame.origin.x + screen_frame.size.width - SCREEN_EDGE - capsule_w;
        let capsule_x = (anchor.x - CAPSULE_GAP - capsule_w).clamp(left, right.max(left));
        let capsule_frame = NSRect::new(
            NSPoint::new(capsule_x, anchor.y - CAPSULE_DROP - capsule_h),
            NSSize::new(capsule_w, capsule_h),
        );
        let local = NSRect::new(NSPoint::new(0.0, 0.0), capsule_frame.size);
        let placed = NSRect::new(
            NSPoint::new(CAPSULE_MARGIN, CAPSULE_MARGIN),
            capsule_frame.size,
        );
        let window_frame = NSRect::new(
            NSPoint::new(
                capsule_frame.origin.x - CAPSULE_MARGIN,
                capsule_frame.origin.y - CAPSULE_MARGIN,
            ),
            NSSize::new(
                capsule_w + 2.0 * CAPSULE_MARGIN,
                capsule_h + 2.0 * CAPSULE_MARGIN,
            ),
        );

        // neither panel ever becomes key, so the warning takes no focus from the user's app
        let capsule_window = make_panel(mtm, window_frame, false);
        let Some(content) = capsule_window.contentView() else {
            return;
        };
        content.setWantsLayer(true);
        let body = NSView::initWithFrame(NSView::alloc(mtm), local);
        body.setWantsLayer(true);
        let capsule: Retained<NSView> = if solid_surfaces() {
            let view = NSView::initWithFrame(NSView::alloc(mtm), placed);
            view.setWantsLayer(true);
            if let Some(layer) = view.layer() {
                // under the labels' own colours, which follow the system's appearance
                let (fill, edge) = if system_is_dark(mtm) {
                    (0.14, 1.0)
                } else {
                    (0.96, 0.0)
                };
                layer.setBackgroundColor(Some(&CGColor::new_generic_gray(fill, 1.0)));
                layer.setCornerRadius(CAPSULE_RADIUS);
                layer.setBorderWidth(1.0);
                layer.setBorderColor(Some(&CGColor::new_generic_gray(edge, 0.35)));
            }
            view.addSubview(&body);
            view
        } else {
            let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), placed);
            glass.setCornerRadius(CAPSULE_RADIUS);
            // the system's regular glass, with no tint of ours. AppKit gives it the
            // appearance's shade and the Liquid Glass slider's setting itself, as it does
            // Control Center's; the clear style takes neither, and on macOS 27 a tint
            // colour comes out as a flat fill over the glass
            glass.setStyle(NSGlassEffectViewStyle::Regular);
            glass.setContentView(Some(&body));
            Retained::into_super(glass)
        };
        capsule.setWantsLayer(true);
        content.addSubview(&capsule);
        let heading_y = capsule_h - PAD - heading_h;
        heading.setFrameOrigin(NSPoint::new(TEXT_INSET, heading_y));
        body.addSubview(&heading);

        let text_y = heading_y - LINE_GAP - text_h;
        prefix.setFrameOrigin(NSPoint::new(TEXT_INSET, text_y));
        countdown.setFrame(NSRect::new(
            NSPoint::new(TEXT_INSET + prefix_w + COUNT_GAP, text_y),
            NSSize::new(count_w, text_h),
        ));
        body.addSubview(&prefix);
        body.addSubview(&countdown);
        let mut x = capsule_w - PAD - buttons_w;
        for button in &row {
            let width = button.frame().size.width;
            button.setFrame(NSRect::new(
                NSPoint::new(x, PAD),
                NSSize::new(width, BUTTON_H),
            ));
            body.addSubview(button);
            x += width + BUTTON_GAP;
        }

        let charm_frame = NSRect::new(
            NSPoint::new(anchor.x - CHARM_W / 2.0, anchor.y - CHARM_H),
            NSSize::new(CHARM_W, CHARM_H),
        );
        let charm_window = make_panel(mtm, charm_frame, false);
        // the panel is wide, for the swing, and mostly empty: clicks go through it to the
        // app underneath. the charm takes them only while the pointer is on the bead
        charm_window.setIgnoresMouseEvents(true);
        let scale = screen.map_or(2.0, |screen| screen.backingScaleFactor());
        let charm = Charm::new(mtm, charm_frame.size, THREAD, BEAD, scale);
        charm_window.setContentView(Some(&charm));

        let animate = animations_on();
        let now = CACurrentMediaTime();
        if !animate {
            // reduced motion: both fade up where they are
            for layer in [capsule.layer(), charm.layer()].into_iter().flatten() {
                layer.addAnimation_forKey(&fade(0.0, 1.0, now, CROSS_FADE, Fill::Entrance), None);
            }
        } else {
            charm.bubble_in(now);
            let begin = now + SHOW_DELAY;
            if let Some(layer) = capsule.layer() {
                let appear = fade(0.0, 1.0, begin, SHOW_DURATION, Fill::Entrance);
                appear.setTimingFunction(Some(&arrive()));
                layer.addAnimation_forKey(&appear, None);
            }
            if let Some(layer) = body.layer() {
                attach_blur(&layer, 0.0);
                let sharpen = blur_radius(CONTENT_BLUR, 0.0, begin, SHOW_DURATION, Fill::Entrance);
                sharpen.setTimingFunction(Some(&arrive()));
                layer.addAnimation_forKey(&sharpen, None);
                detach_blur_after(mtm, layer, SHOW_DELAY + SHOW_DURATION + 0.05);
            }
        }

        capsule_window.orderFrontRegardless();
        charm_window.orderFrontRegardless();
        charm.start(animate);
        self.shown = Some(Showing {
            animate,
            capsule_window,
            capsule,
            body,
            countdown,
            buttons: row,
            charm_window,
            charm,
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

    /// the hanging bead, for the break screen's bead to take over from
    pub fn hand_off(&self) -> Option<HandOff> {
        let shown = self.shown.as_ref()?;
        let origin = shown.charm_window.frame().origin;
        let bead = shown.charm.bead_frame();
        Some(HandOff {
            frame: NSRect::new(
                NSPoint::new(origin.x + bead.origin.x, origin.y + bead.origin.y),
                bead.size,
            ),
            look: shown.charm.look(),
        })
    }

    /// with `hand_off`, the break screen's bead takes over from this one, which goes at once
    pub fn hide(&mut self, hand_off: bool) {
        let Some(Showing {
            animate,
            capsule_window,
            capsule,
            body,
            buttons,
            charm_window,
            charm,
            ..
        }) = self.shown.take()
        else {
            return;
        };
        // inert while it fades, since a sleeping button takes no clicks
        for button in &buttons {
            button.set_awake(false);
        }
        charm_window.setIgnoresMouseEvents(true);
        let now = CACurrentMediaTime();
        if !animate {
            // reduced motion: both fade away where they are
            charm.stop();
            for layer in [capsule.layer(), charm.layer()].into_iter().flatten() {
                let opacity = presented(&layer, "opacity").unwrap_or(1.0);
                layer.addAnimation_forKey(&fade(opacity, 0.0, now, CROSS_FADE, Fill::Exit), None);
            }
            after(self.mtm, CROSS_FADE + 0.03, move || {
                capsule_window.orderOut(None);
                charm_window.orderOut(None);
            });
            return;
        }
        if let Some(layer) = capsule.layer() {
            // it fades and blurs away where it is, from wherever its entrance had got to
            let opacity = presented(&layer, "opacity").unwrap_or(1.0);
            let vanish = fade(opacity, 0.0, now, HIDE_DURATION, Fill::Exit);
            vanish.setTimingFunction(Some(&arrive()));
            layer.addAnimation_forKey(&vanish, None);
        }
        if let Some(layer) = body.layer() {
            blur_out(&layer, CONTENT_BLUR, now, HIDE_DURATION);
        }
        if hand_off {
            // the break's bead starts from where this one is, so it holds still
            charm.stop();
            charm.hand_off(now, HIDE_DURATION);
        } else {
            // it swings to rest as it goes
            charm.settle();
            charm.bubble_out(now, HIDE_DURATION);
        }
        after(self.mtm, HIDE_DURATION + 0.03, move || {
            charm.stop();
            capsule_window.orderOut(None);
            charm_window.orderOut(None);
        });
    }
}

/// under the menu bar at the top right of the main display, where a status item sits
fn fallback_anchor(mtm: MainThreadMarker) -> Option<NSPoint> {
    let screen = NSScreen::mainScreen(mtm)?;
    let frame = screen.frame();
    Some(NSPoint::new(
        frame.origin.x + frame.size.width - 120.0,
        menu_bar_edge(&screen),
    ))
}

/// the height of the menu bar's bottom edge on `screen`, which is the top of the area the
/// bar keeps clear. a bar that hides itself keeps none, and this is the screen's top
pub fn menu_bar_edge(screen: &NSScreen) -> f64 {
    let visible = screen.visibleFrame();
    visible.origin.y + visible.size.height
}

/// the display under the menu-bar edge at `point`, or the main one. under a bar that hides
/// itself, `point` is the display's top edge, which NSPointInRect counts as outside it
fn screen_at(mtm: MainThreadMarker, point: NSPoint) -> Option<Retained<NSScreen>> {
    NSScreen::screens(mtm)
        .iter()
        .find(|screen| NSPointInRect(NSPoint::new(point.x, point.y - 1.0), screen.frame()))
        .or_else(|| NSScreen::mainScreen(mtm))
}

/// where the bead hangs at rest from the menu bar at `anchor`, under the icon
pub fn hanging_bead(anchor: NSPoint) -> NSRect {
    NSRect::new(
        NSPoint::new(anchor.x - BEAD / 2.0, anchor.y - THREAD - BEAD),
        NSSize::new(BEAD, BEAD),
    )
}

/// "9s", or "2:05" from 100 seconds up
fn fmt_seconds(seconds: u64) -> String {
    if seconds >= 100 {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_counts_in_seconds_then_minutes() {
        assert_eq!(fmt_seconds(9), "9s");
        assert_eq!(fmt_seconds(99), "99s");
        assert_eq!(fmt_seconds(100), "1:40");
        assert_eq!(fmt_seconds(600), "10:00");
    }
}
