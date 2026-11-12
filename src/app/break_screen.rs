//! one fullscreen overlay per display in liquid glass style. buttons are
//! NSGlassEffectView capsules wrapping a borderless NSButton.

use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBox, NSBoxType, NSColor, NSFont,
    NSFontWeightHeavy, NSFontWeightRegular, NSGlassEffectView, NSImage, NSScreen, NSTextAlignment,
    NSTextField, NSTitlePosition, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CACurrentMediaTime, CALayer, kCAGravityResizeAspectFill};

use super::glass_button::{
    Buttons, GlassButtonStyle, GlassButtonWidth, Shortcut, make_glass_button,
};
use super::panel::{
    Fill, LabelStyle, Panel, after, attach_blur, blur_radius, detach_blur_after, fade, make_label,
    make_panel, scale, spring_y,
};
use crate::queue::Break;

/// dim scrim over the blur, enough to even out a busy desktop into a uniformly dark backdrop
const SCRIM_ALPHA: f64 = 0.25;
const BUTTON_HEIGHT: f64 = 46.0;
/// foreground entry/exit blur radius, the "materialise from blur" look
const BLUR_RADIUS: f64 = 16.0;
/// resting blur on a custom background image, eyeballed to match the FullScreenUI desktop blur
const IMAGE_BLUR_RADIUS_RESTING: f64 = 16.0;
/// image enters from this radius and animates down to resting, sharpening with the foreground
const IMAGE_BLUR_RADIUS_START: f64 = 60.0;
/// below 1.0 the dark void bleeds through and darkens the image so text stays legible
const IMAGE_OPACITY: f32 = 0.75;
const EXIT_DURATION: f64 = 0.32;
const BUTTON_STYLE: GlassButtonStyle = GlassButtonStyle {
    height: BUTTON_HEIGHT,
    width: GlassButtonWidth::Fit {
        min_width: 120.0,
        horizontal_padding: 48.0,
    },
    title_font_size: 16.0,
    hint_font_size: 13.0,
};

pub struct BreakScreen {
    mtm: MainThreadMarker,
    /// one per display while the break shows; empty from the moment `hide` starts
    overlays: Vec<Overlay>,
    /// off when the user has asked macOS to reduce motion
    animate: bool,
}

struct Overlay {
    window: Retained<Panel>,
    /// desktop blur, image and scrim, faded as one on exit
    backdrop: Retained<NSView>,
    /// message, countdown and buttons, blurred and faded as one on exit
    foreground: Retained<NSView>,
    countdown: Retained<NSTextField>,
}

impl BreakScreen {
    pub fn new(mtm: MainThreadMarker) -> BreakScreen {
        BreakScreen {
            mtm,
            overlays: Vec::new(),
            animate: true,
        }
    }

    /// covers every display with `brk`, counting down from `remaining`
    pub fn show(&mut self, brk: &Break, remaining: u64, buttons: Buttons, target: &AnyObject) {
        self.teardown_now();
        let mtm = self.mtm;
        let weight_regular = unsafe { NSFontWeightRegular };
        let weight_heavy = unsafe { NSFontWeightHeavy };

        // reduce motion: skip every blur, slide and cross-fade
        self.animate = !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        let animate = self.animate;
        let now = CACurrentMediaTime();

        // monospaced so the digits don't shift as they tick
        let count_font = NSFont::monospacedDigitSystemFontOfSize_weight(100.0, weight_heavy);

        let screens = NSScreen::screens(mtm);
        for i in 0..screens.count() {
            let screen = screens.objectAtIndex(i);
            let frame = screen.frame();
            let w = frame.size.width;
            let h = frame.size.height;
            let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
            let cx = w / 2.0;
            let cy = h / 2.0;

            // keys reach a key window only in the active app: `show` activates omma
            let window = make_panel(mtm, frame, NSWindowStyleMask::Borderless, true);
            let Some(content) = window.contentView() else {
                continue;
            };
            content.setWantsLayer(true);

            // backdrop: blur + scrim in one container so they fade as a group
            let backdrop = NSView::initWithFrame(NSView::alloc(mtm), full);
            backdrop.setWantsLayer(true);
            content.addSubview(&backdrop);

            let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
            blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            blur.setState(NSVisualEffectState::Active);
            backdrop.addSubview(&blur);

            // optional custom image, between blur and scrim. a file that isn't an image is skipped.
            // a permanent gaussian blur keeps it a soft backdrop instead of a sharp photo.
            let image_view = brk
                .image
                .as_deref()
                .and_then(|p| make_image_view(mtm, p, full, screen.backingScaleFactor()));
            if let Some(view) = &image_view {
                backdrop.addSubview(view);
            }

            let scrim = NSBox::initWithFrame(NSBox::alloc(mtm), full);
            scrim.setBoxType(NSBoxType::Custom);
            scrim.setTitlePosition(NSTitlePosition::NoTitle);
            scrim.setBorderWidth(0.0);
            scrim.setCornerRadius(0.0);
            scrim.setFillColor(&NSColor::colorWithWhite_alpha(0.0, SCRIM_ALPHA));
            backdrop.addSubview(&scrim);

            // foreground: message, countdown, buttons. one gaussian blur on this layer lets
            // the group materialise from blur on the way in and dissolve back on the way out.
            let foreground = NSView::initWithFrame(NSView::alloc(mtm), full);
            foreground.setWantsLayer(true);
            content.addSubview(&foreground);

            // wraps to two lines
            let msg_w = (w - 160.0).min(1000.0);
            let title_font = NSFont::systemFontOfSize_weight(28.0, weight_regular);
            let msg_color = NSColor::colorWithWhite_alpha(1.0, 0.92);
            let name_label = make_label(
                mtm,
                &brk.name,
                NSRect::new(
                    NSPoint::new(cx - msg_w / 2.0, cy + 44.0),
                    NSSize::new(msg_w, 80.0),
                ),
                &LabelStyle {
                    font: &title_font,
                    color: &msg_color,
                    alignment: NSTextAlignment::Center,
                    max_lines: 2,
                },
            );
            foreground.addSubview(&name_label);

            // each tick cross-fades via update_countdown
            let count_label = make_label(
                mtm,
                &remaining.to_string(),
                NSRect::new(
                    NSPoint::new(cx - 250.0, cy - 90.0),
                    NSSize::new(500.0, 130.0),
                ),
                &LabelStyle {
                    font: &count_font,
                    color: &NSColor::whiteColor(),
                    alignment: NSTextAlignment::Center,
                    max_lines: 1,
                },
            );
            foreground.addSubview(&count_label);

            let gap = 16.0;
            let buttons: Vec<Retained<NSGlassEffectView>> = [
                (buttons.skip, "Skip", sel!(skip:)),
                (buttons.postpone, "Postpone", sel!(postpone:)),
            ]
            .into_iter()
            .filter_map(|(button, title, action)| {
                let key = button?.key.map(Shortcut::Local);
                Some(make_glass_button(
                    mtm,
                    title,
                    key,
                    target,
                    action,
                    &BUTTON_STYLE,
                ))
            })
            .collect();
            if !buttons.is_empty() {
                let widths: Vec<f64> = buttons.iter().map(|b| b.frame().size.width).collect();
                let total: f64 = widths.iter().sum::<f64>() + gap * (buttons.len() as f64 - 1.0);
                let by = 150.0;
                let mut x = cx - total / 2.0;
                for (button, bw) in buttons.iter().zip(widths.iter()) {
                    button.setFrame(NSRect::new(
                        NSPoint::new(x, by),
                        NSSize::new(*bw, BUTTON_HEIGHT),
                    ));
                    foreground.addSubview(button);
                    x += bw + gap;
                }
            }

            // entrance choreography: backdrop fades up, foreground resolves out of blur, message
            // slides down from above, buttons slide up from below. countdown just sharpens.
            if animate {
                if let Some(layer) = backdrop.layer() {
                    layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.5, Fill::Entrance), None);
                }
                if let Some(layer) = foreground.layer() {
                    let begin = now + 0.06;
                    attach_unclipped_blur(&layer, 0.0);
                    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.55, Fill::Entrance), None);
                    layer.addAnimation_forKey(
                        &blur_radius(BLUR_RADIUS, 0.0, begin, 0.55, Fill::Entrance),
                        None,
                    );
                    // drop the transition blur once the entrance settles so the break renders crisp
                    detach_blur_after(mtm, layer, 0.75);
                }
                // image holds at its resting radius after the sharpen-in, no detach
                if let Some(layer) = image_view.as_ref().and_then(|v| v.layer()) {
                    let begin = now + 0.06;
                    layer.addAnimation_forKey(
                        &blur_radius(
                            IMAGE_BLUR_RADIUS_START,
                            IMAGE_BLUR_RADIUS_RESTING,
                            begin,
                            0.55,
                            Fill::Entrance,
                        ),
                        None,
                    );
                }
                let label_begin = now + 0.68;
                let buttons_begin = label_begin + 0.28;
                if let Some(layer) = name_label.layer() {
                    animate_delayed_entrance(mtm, layer, 28.0, label_begin, now);
                }
                for button in &buttons {
                    if let Some(layer) = button.layer() {
                        animate_delayed_entrance(mtm, layer, -28.0, buttons_begin, now);
                    }
                }
            }

            window.orderFrontRegardless();
            self.overlays.push(Overlay {
                window,
                backdrop,
                foreground,
                countdown: count_label,
            });
        }

        // only the active app can own a key window. cooperative activate() is throttled for a
        // background app waking on a timer, so without forcing it the overlay never becomes key
        // and skip/postpone keys are dead until the user clicks. promote to regular and force
        // activate so the panel becomes key, hide() drops us back to accessory.
        let app = NSApplication::sharedApplication(mtm);
        let Some(first) = self.overlays.first() else {
            return;
        };
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        force_activate(&app);
        first.window.makeKeyAndOrderFront(None);
    }

    pub fn is_visible(&self) -> bool {
        !self.overlays.is_empty()
    }

    pub fn update_countdown(&self, remaining: u64) {
        let text = NSString::from_str(&remaining.to_string());
        for overlay in &self.overlays {
            let label = &overlay.countdown;
            if *label.stringValue() == *text {
                continue;
            }
            if self.animate {
                animate_countdown_swap(self.mtm, &overlay.foreground, label, &text);
            } else {
                label.setStringValue(&text);
            }
        }
    }

    pub fn hide(&mut self) {
        if self.overlays.is_empty() {
            return;
        }
        // reduce motion: tear down immediately
        if !self.animate {
            self.teardown_now();
            return;
        }

        // reverse of the entrance. animations hold their final state so the overlay stays
        // faded/blurred right up to teardown.
        let now = CACurrentMediaTime();
        let overlays = std::mem::take(&mut self.overlays);
        for overlay in &overlays {
            if let Some(layer) = overlay.backdrop.layer() {
                layer.addAnimation_forKey(&fade(1.0, 0.0, now, EXIT_DURATION, Fill::Exit), None);
            }
            if let Some(layer) = overlay.foreground.layer() {
                attach_unclipped_blur(&layer, 0.0);
                layer.addAnimation_forKey(&fade(1.0, 0.0, now, EXIT_DURATION, Fill::Exit), None);
                layer.addAnimation_forKey(
                    &blur_radius(0.0, BLUR_RADIUS, now, EXIT_DURATION, Fill::Exit),
                    None,
                );
            }
        }

        // the overlays move into the timer so they stay on screen for the animation, then it
        // orders them out and restores accessory mode
        let mtm = self.mtm;
        after(mtm, EXIT_DURATION + 0.05, move || {
            for overlay in &overlays {
                overlay.window.orderOut(None);
            }
            NSApplication::sharedApplication(mtm)
                .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        });
    }

    /// no exit animation. order every window out, forget the views, drop back to accessory.
    /// used when reopening and when reduce motion is on.
    fn teardown_now(&mut self) {
        let had_windows = !self.overlays.is_empty();
        for overlay in self.overlays.drain(..) {
            overlay.window.orderOut(None);
        }

        if had_windows {
            let app = NSApplication::sharedApplication(self.mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }
}

#[expect(
    deprecated,
    reason = "cooperative activate() is throttled for a background app woken by a timer, so the panel never becomes key"
)]
fn force_activate(app: &NSApplication) {
    app.activateIgnoringOtherApps(true);
}

fn make_image_view(
    mtm: MainThreadMarker,
    path: &Path,
    frame: NSRect,
    backing_scale: f64,
) -> Option<Retained<NSView>> {
    let ns_path = NSString::from_str(path.to_str()?);
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path)?;
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setWantsLayer(true);
    let layer = view.layer()?;
    let image_obj: &AnyObject = &image;
    unsafe {
        layer.setContents(Some(image_obj));
        layer.setContentsGravity(kCAGravityResizeAspectFill);
    }
    layer.setContentsScale(backing_scale);
    layer.setOpacity(IMAGE_OPACITY);
    attach_unclipped_blur(&layer, IMAGE_BLUR_RADIUS_RESTING);
    Some(view)
}

/// `attach_blur`, letting the blur bleed past the layer's edge instead of being clipped
fn attach_unclipped_blur(layer: &CALayer, resting: f64) {
    attach_blur(layer, resting);
    layer.setMasksToBounds(false);
}

/// invisible until `begin`, then springs in and resolves from blur to sharp
fn animate_delayed_entrance(
    mtm: MainThreadMarker,
    layer: Retained<CALayer>,
    from_y: f64,
    begin: f64,
    now: f64,
) {
    attach_unclipped_blur(&layer, 0.0);
    layer.addAnimation_forKey(&fade(0.0, 1.0, begin, 0.18, Fill::Entrance), None);
    layer.addAnimation_forKey(&blur_radius(12.0, 0.0, begin, 0.42, Fill::Entrance), None);
    layer.addAnimation_forKey(&spring_y(from_y, begin), None);
    detach_blur_after(mtm, layer, begin - now + 0.5);
}

/// old digit shrinks/blurs out, new digit starts larger/blurred and resolves in place.
/// `parent` holds `label`.
fn animate_countdown_swap(
    mtm: MainThreadMarker,
    parent: &NSView,
    label: &NSTextField,
    text: &NSString,
) {
    let old_text = label.stringValue();
    let outgoing = NSTextField::labelWithString(&old_text, mtm);
    outgoing.setWantsLayer(true);
    outgoing.setFrame(label.frame());
    outgoing.setAlignment(NSTextAlignment::Center);
    outgoing.setMaximumNumberOfLines(1);
    if let Some(font) = label.font() {
        outgoing.setFont(Some(&font));
    }
    if let Some(color) = label.textColor() {
        outgoing.setTextColor(Some(&color));
    }

    parent.addSubview(&outgoing);

    label.setStringValue(text);

    let now = CACurrentMediaTime();
    if let Some(layer) = outgoing.layer() {
        attach_unclipped_blur(&layer, 0.0);
        layer.addAnimation_forKey(&fade(1.0, 0.0, now, 0.3, Fill::Exit), None);
        layer.addAnimation_forKey(&scale(1.0, 0.88, now, 0.3, Fill::Exit), None);
        layer.addAnimation_forKey(&blur_radius(0.0, 10.0, now, 0.3, Fill::Exit), None);
    }
    if let Some(layer) = label.layer() {
        attach_unclipped_blur(&layer, 0.0);
        layer.addAnimation_forKey(&fade(0.0, 1.0, now, 0.3, Fill::Entrance), None);
        layer.addAnimation_forKey(&scale(1.12, 1.0, now, 0.3, Fill::Entrance), None);
        layer.addAnimation_forKey(&blur_radius(10.0, 0.0, now, 0.3, Fill::Entrance), None);
        detach_blur_after(mtm, layer, 0.34);
    }
    after(mtm, 0.34, move || outgoing.removeFromSuperview());
}
