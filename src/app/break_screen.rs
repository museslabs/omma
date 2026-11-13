//! one fullscreen overlay per display in liquid glass style. buttons are
//! NSGlassEffectView capsules wrapping a borderless NSButton.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadOnly, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSFont, NSFontWeightHeavy, NSFontWeightRegular, NSImage, NSScreen,
    NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::CGImage;
use objc2_core_image::{CIContext, CIImage};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CACurrentMediaTime, CALayer, kCAGravityResizeAspectFill};

use super::glass_button::{
    ButtonHost, Buttons, GlassButtonStyle, Shortcut, Surface, make_glass_button,
};
use super::panel::{
    CROSS_FADE, Fill, LabelStyle, Panel, after, animations_on, attach_blur, blur_out, blur_radius,
    detach_blur_after, fade, keep_dark, make_label, make_panel, presented, scale, spring_y,
};
use crate::queue::Break;

/// dim scrim over the blur, enough to even out a busy desktop into a uniformly dark backdrop
const SCRIM_ALPHA: f64 = 0.25;
const BUTTON_HEIGHT: f64 = 46.0;
/// the foreground's blur radius as it enters and leaves
const BLUR_RADIUS: f64 = 16.0;
/// how soft a custom background image is, in points on the display, eyeballed to match the
/// FullScreenUI desktop blur. the image is blurred this much once, into a bitmap
const IMAGE_SOFTNESS: f64 = 16.0;
/// a pixel of that bitmap covers this many points, which a blur that wide hides
const IMAGE_BITMAP_PIXEL: f64 = 2.0;
/// the image enters with this much blur on top, 60 in all, and sharpens to its own softness
/// as the screen settles
const IMAGE_BLUR_ENTRANCE: f64 = 58.0;
/// below 1.0 the dark void bleeds through and darkens the image so text stays legible
const IMAGE_OPACITY: f32 = 0.75;
/// the foreground starts to resolve from blur this long after the screen appears
const FOREGROUND_DELAY: f64 = 0.06;
/// and takes this long to come into focus
const FOREGROUND_ENTRANCE: f64 = 0.55;
/// when the foreground is in focus, from the moment the screen appears
const FOREGROUND_SETTLED: f64 = FOREGROUND_DELAY + FOREGROUND_ENTRANCE;
/// the message starts its entrance this long after the screen appears
const LABEL_BEGINS: f64 = FOREGROUND_SETTLED + 0.07;
/// and the buttons this long
const BUTTONS_BEGIN: f64 = LABEL_BEGINS + 0.28;
const EXIT_DURATION: f64 = 0.32;
const BUTTON_STYLE: GlassButtonStyle = GlassButtonStyle {
    surface: Surface::Glass,
    height: BUTTON_HEIGHT,
    min_width: 120.0,
    horizontal_padding: 48.0,
    title_font_size: 16.0,
    hint_font_size: 13.0,
};

/// a break screen that `show` put up
pub struct Built {
    /// how long after `show` returned the countdown comes into focus: the entrance, or the
    /// cross-fade under reduced motion
    pub settling: Duration,
    /// whether the break's image loaded, or why not. None when the break has no image
    pub image: Option<Result<(), String>>,
}

pub struct BreakScreen {
    mtm: MainThreadMarker,
    /// one per display while the break shows; empty from the moment `hide` starts
    overlays: Vec<Overlay>,
    /// `animations_on()` at the latest `show`, so the countdown and `hide` animate only if
    /// the entrance did
    animate: bool,
    /// true from the latest `show` until `hide` starts or the overlays are torn down. a
    /// timer from that `show` checks it, since `hide` may have run first.
    up: Rc<Cell<bool>>,
}

struct Overlay {
    window: Retained<Panel>,
    /// desktop blur, image and scrim, faded as one on exit
    backdrop: Retained<NSView>,
    /// message, countdown and buttons, blurred and faded as one on exit
    foreground: Retained<NSView>,
    countdown: Retained<NSTextField>,
    buttons: Vec<Retained<ButtonHost>>,
}

impl BreakScreen {
    pub fn new(mtm: MainThreadMarker) -> BreakScreen {
        BreakScreen {
            mtm,
            overlays: Vec::new(),
            animate: true,
            up: Rc::new(Cell::new(false)),
        }
    }

    /// covers every display with `brk`, counting down from `remaining`. None when there was
    /// no display to show it on.
    pub fn show(
        &mut self,
        brk: &Break,
        remaining: u64,
        buttons: Buttons,
        target: &AnyObject,
    ) -> Option<Built> {
        self.teardown_now();
        let mtm = self.mtm;
        // SAFETY: immutable AppKit constants
        let (weight_regular, weight_heavy) = unsafe { (NSFontWeightRegular, NSFontWeightHeavy) };

        self.animate = animations_on();
        let animate = self.animate;
        let now = CACurrentMediaTime();

        // monospaced so the digits don't shift as they tick
        let count_font = NSFont::monospacedDigitSystemFontOfSize_weight(100.0, weight_heavy);
        // with no display, sync tries again on its next tick
        let screens = NSScreen::screens(mtm);
        if screens.count() == 0 {
            return None;
        }
        let (image, mut loaded) = if let Some(path) = brk.image.as_deref() {
            match load_image(path) {
                Some(image) => (Some(image), Some(Ok(()))),
                None => (None, Some(Err(cant_show(path)))),
            }
        } else {
            (None, None)
        };

        for i in 0..screens.count() {
            let screen = screens.objectAtIndex(i);
            let frame = screen.frame();
            let w = frame.size.width;
            let h = frame.size.height;
            let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
            let cx = w / 2.0;
            let cy = h / 2.0;

            let window = make_panel(mtm, frame, true);
            // white text over a black scrim, in light mode too
            keep_dark(&window);
            let Some(content) = window.contentView() else {
                continue;
            };
            content.setWantsLayer(true);

            let backdrop = NSView::initWithFrame(NSView::alloc(mtm), full);
            backdrop.setWantsLayer(true);
            content.addSubview(&backdrop);

            let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), full);
            blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
            blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            blur.setState(NSVisualEffectState::Active);
            backdrop.addSubview(&blur);

            // optional custom image, between blur and scrim, blurred into a soft backdrop
            // instead of a sharp photo
            let image_view = image
                .as_deref()
                .and_then(|image| make_image_view(mtm, image, full));
            if let Some(view) = &image_view {
                backdrop.addSubview(view);
            } else if image.is_some()
                && let Some(path) = brk.image.as_deref()
            {
                // Core Image couldn't soften it
                loaded = Some(Err(cant_show(path)));
            }

            let scrim = NSBox::initWithFrame(NSBox::alloc(mtm), full);
            scrim.setBoxType(NSBoxType::Custom);
            scrim.setTitlePosition(NSTitlePosition::NoTitle);
            scrim.setBorderWidth(0.0);
            scrim.setCornerRadius(0.0);
            scrim.setFillColor(&NSColor::colorWithWhite_alpha(0.0, SCRIM_ALPHA));
            backdrop.addSubview(&scrim);

            // one gaussian blur on this layer lets the message, countdown and buttons
            // materialise from blur on the way in and dissolve back on the way out.
            let foreground = NSView::initWithFrame(NSView::alloc(mtm), full);
            foreground.setWantsLayer(true);
            content.addSubview(&foreground);

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
            let buttons: Vec<Retained<ButtonHost>> = [
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
                    let begin = now + FOREGROUND_DELAY;
                    attach_unclipped_blur(&layer, 0.0);
                    layer.addAnimation_forKey(
                        &fade(0.0, 1.0, begin, FOREGROUND_ENTRANCE, Fill::Entrance),
                        None,
                    );
                    layer.addAnimation_forKey(
                        &blur_radius(BLUR_RADIUS, 0.0, begin, FOREGROUND_ENTRANCE, Fill::Entrance),
                        None,
                    );
                    // drop the transition blur once the entrance settles so the break renders crisp
                    detach_blur_after(mtm, layer, FOREGROUND_SETTLED + 0.14);
                }
                // the image sharpens to its own softness, and nothing blurs it from then on
                if let Some(layer) = image_view.as_ref().and_then(|v| v.layer()) {
                    attach_unclipped_blur(&layer, 0.0);
                    layer.addAnimation_forKey(
                        &blur_radius(
                            IMAGE_BLUR_ENTRANCE,
                            0.0,
                            now + FOREGROUND_DELAY,
                            FOREGROUND_ENTRANCE,
                            Fill::Entrance,
                        ),
                        None,
                    );
                    detach_blur_after(mtm, layer, FOREGROUND_SETTLED + 0.05);
                }
                if let Some(layer) = name_label.layer() {
                    animate_delayed_entrance(mtm, layer, 28.0, now + LABEL_BEGINS, now);
                }
                for button in &buttons {
                    // takes neither clicks nor its shortcut until it shows, so keys typed as
                    // the break starts don't skip it
                    button.set_awake(false);
                    if let Some(layer) = button.layer() {
                        animate_delayed_entrance(mtm, layer, -28.0, now + BUTTONS_BEGIN, now);
                    }
                }
            } else {
                // reduced motion: the screen fades up as one, and nothing moves
                for layer in [backdrop.layer(), foreground.layer()].into_iter().flatten() {
                    layer.addAnimation_forKey(
                        &fade(0.0, 1.0, now, CROSS_FADE, Fill::Entrance),
                        None,
                    );
                }
            }

            window.orderFrontRegardless();
            self.overlays.push(Overlay {
                window,
                backdrop,
                foreground,
                countdown: count_label,
                buttons,
            });
        }

        // the skip and postpone keys go to the key window
        let first = self.overlays.first()?;
        first.window.makeKeyWindow();
        self.up = Rc::new(Cell::new(true));
        if animate {
            let up = Rc::clone(&self.up);
            let buttons: Vec<_> = self
                .overlays
                .iter()
                .flat_map(|overlay| overlay.buttons.clone())
                .collect();
            let delay = (now + BUTTONS_BEGIN - CACurrentMediaTime()).max(0.0);
            after(mtm, delay, move || {
                if up.get() {
                    for button in &buttons {
                        button.set_awake(true);
                    }
                }
            });
        }
        let settled = if animate {
            FOREGROUND_SETTLED
        } else {
            CROSS_FADE
        };
        let settling = (now + settled - CACurrentMediaTime()).max(0.0);
        Some(Built {
            settling: Duration::from_secs_f64(settling),
            image: loaded,
        })
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
        self.up.set(false);
        let animate = self.animate;
        // under reduced motion the screen fades away as one
        let fading = if animate { EXIT_DURATION } else { CROSS_FADE };

        // reverse of the entrance. animations hold their final state so the overlay stays
        // faded/blurred right up to teardown.
        let now = CACurrentMediaTime();
        let overlays = std::mem::take(&mut self.overlays);
        for overlay in &overlays {
            // inert while it fades. a sleeping button takes neither clicks nor its shortcut,
            // and the overlay still catches clicks, so none reach the app behind it
            for button in &overlay.buttons {
                button.set_awake(false);
            }
            let layers = [overlay.backdrop.layer(), overlay.foreground.layer()];
            for layer in layers.into_iter().flatten() {
                let opacity = presented(&layer, "opacity").unwrap_or(1.0);
                layer.addAnimation_forKey(&fade(opacity, 0.0, now, fading, Fill::Exit), None);
            }
            if animate && let Some(layer) = overlay.foreground.layer() {
                blur_out(&layer, BLUR_RADIUS, now, EXIT_DURATION);
            }
        }

        // the overlays move into the timer so they stay on screen for the animation
        after(self.mtm, fading + 0.05, move || {
            for overlay in &overlays {
                overlay.window.orderOut(None);
            }
        });
    }

    /// orders every window out and forgets the views, with no exit animation. used when
    /// reopening.
    fn teardown_now(&mut self) {
        self.up.set(false);
        for overlay in self.overlays.drain(..) {
            overlay.window.orderOut(None);
        }
    }
}

/// the problem a break reports for an image it can't show
fn cant_show(path: &Path) -> String {
    format!("can't show {} as an image", path.display())
}

/// None when the file isn't an image AppKit can read
fn load_image(path: &Path) -> Option<Retained<CGImage>> {
    let path = NSString::from_str(path.to_str()?);
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &path)?;
    // SAFETY: a null rect asks for the image at its own size, and it needs no context or
    // hints for that
    unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }
}

/// `image`, softened, filling `frame`. None when Core Image can't render it
fn make_image_view(
    mtm: MainThreadMarker,
    image: &CGImage,
    frame: NSRect,
) -> Option<Retained<NSView>> {
    let soft = soften(image, frame.size)?;
    let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
    view.setWantsLayer(true);
    let layer = view.layer()?;
    let contents: &AnyObject = soft.as_ref();
    // SAFETY: a layer's contents can be a CGImage, and the gravity is an immutable Core
    // Animation constant
    unsafe {
        layer.setContents(Some(contents));
        layer.setContentsGravity(kCAGravityResizeAspectFill);
    }
    layer.setOpacity(IMAGE_OPACITY);
    Some(view)
}

/// `image` blurred once, to look `IMAGE_SOFTNESS` points soft when it fills a display of
/// `size`. no filter then has to blur it for as long as the break lasts. the bitmap is only
/// as fine as that softness can show.
fn soften(image: &CGImage, size: NSSize) -> Option<Retained<CGImage>> {
    let width = CGImage::width(Some(image)) as f64;
    let height = CGImage::height(Some(image)) as f64;
    if width < 1.0 || height < 1.0 {
        return None;
    }
    let (shrink, sigma) = bitmap_for(width, height, size);
    // SAFETY: the image is a valid CGImage, and Core Image only reads it. clamping repeats
    // its edge outward, so the blur doesn't fade the edges into nothing
    unsafe {
        let small = CIImage::imageWithCGImage(image).imageByApplyingTransform(CGAffineTransform {
            a: shrink,
            b: 0.0,
            c: 0.0,
            d: shrink,
            tx: 0.0,
            ty: 0.0,
        });
        let blurred = small
            .imageByClampingToExtent()
            .imageByApplyingGaussianBlurWithSigma(sigma);
        CIContext::context().createCGImage_fromRect(&blurred, small.extent())
    }
}

/// how much to shrink a `width` by `height` image for its softened bitmap, and the blur to
/// give that bitmap, in its pixels
fn bitmap_for(width: f64, height: f64, size: NSSize) -> (f64, f64) {
    // points on the display to a pixel of the image, once it fills the display
    let shown = (size.width / width).max(size.height / height);
    let shrink = (shown / IMAGE_BITMAP_PIXEL).min(1.0);
    (shrink, IMAGE_SOFTNESS / shown * shrink)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_image_shrinks_to_a_bitmap_of_two_point_pixels() {
        let display = NSSize::new(1440.0, 900.0);
        // twice the display each way: half a point to a pixel, so it shrinks to a quarter
        let (shrink, sigma) = bitmap_for(2880.0, 1800.0, display);
        assert_eq!(shrink, 0.25);
        assert_eq!(sigma, IMAGE_SOFTNESS / IMAGE_BITMAP_PIXEL);
        // a small one is left as it is, and blurred by as many of its pixels as make up
        // the softness
        let (shrink, sigma) = bitmap_for(360.0, 225.0, display);
        assert_eq!(shrink, 1.0);
        assert_eq!(sigma, IMAGE_SOFTNESS / 4.0);
    }

    #[test]
    fn an_image_file_softens_to_a_bitmap() {
        // a one pixel png
        const PNG: [u8; 70] = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96,
            248, 95, 15, 0, 2, 135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        let path = std::env::temp_dir().join(format!("omma-soften-{}.png", std::process::id()));
        std::fs::write(&path, PNG).unwrap();
        let image = load_image(&path);
        std::fs::remove_file(&path).unwrap();
        let soft = soften(&image.unwrap(), NSSize::new(100.0, 50.0)).unwrap();
        assert_eq!(
            (CGImage::width(Some(&soft)), CGImage::height(Some(&soft))),
            (1, 1)
        );
    }
}
