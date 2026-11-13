//! the countdown as a flip clock: a digit that changes turns over like a leaf, its top half
//! falling away and the new bottom half landing with a little bounce.

use std::cell::RefCell;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityStaticTextRole, NSColor, NSFont, NSFontWeightMedium,
    NSTextAlignment, NSTextField, NSView,
};
use objc2_core_graphics::CGColor;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CACurrentMediaTime, CALayer, CAMediaTiming, CAMediaTimingFunction};

use super::bead::Rgba;
use super::panel::{Fill, LabelStyle, POP, after, fade, make_label, property, spring};

/// how long the old top half takes to fall edge-on
const FALL: f64 = 0.15;
/// how dark a leaf gets as it turns away from the light
const SHADE: f64 = 0.45;

pub struct FlipCount {
    /// what goes in the view hierarchy, sized to the count and centred where `new` was told
    pub view: Retained<NSView>,
    mtm: MainThreadMarker,
    font: Retained<NSFont>,
    /// the colour of the leaves, which hides what is under a turning one
    ground: Rgba,
    centre: NSPoint,
    /// a digit's size, and a colon's width
    digit: NSSize,
    colon: f64,
    /// what a label adds around its text, split between both sides
    padding: f64,
    cells: RefCell<Vec<Cell>>,
}

struct Cell {
    shows: char,
    view: Retained<NSView>,
    top: Retained<NSTextField>,
    bottom: Retained<NSTextField>,
}

impl FlipCount {
    /// digits `size` points tall on leaves of the colour `ground`, centred on `centre` in the
    /// superview
    pub fn new(mtm: MainThreadMarker, size: f64, ground: Rgba, centre: NSPoint) -> FlipCount {
        // SAFETY: an immutable AppKit constant
        let medium = unsafe { NSFontWeightMedium };
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, medium);
        let measure = |text: &str| {
            let label = label(mtm, &font, text, NSRect::ZERO);
            label.sizeToFit();
            label.frame().size
        };
        let one = measure("0");
        let advance = measure("00").width - one.width;
        let colon = measure(":0").width - one.width;
        // an even height, so the fold between the halves falls on a whole point
        let height = (one.height / 2.0).ceil() * 2.0;
        // digits have no descender, so the count sits half of one low to centre it by eye
        let centre = NSPoint::new(centre.x, centre.y + font.descender() / 2.0);
        let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        view.setWantsLayer(true);
        // VoiceOver gets the count as one piece of text, which `set` words. the halves of
        // each digit stay out of its way
        view.setAccessibilityElement(true);
        // SAFETY: an immutable AppKit constant
        view.setAccessibilityRole(Some(unsafe { NSAccessibilityStaticTextRole }));
        FlipCount {
            view,
            mtm,
            font,
            ground,
            centre,
            digit: NSSize::new(advance.ceil(), height),
            colon: colon.ceil(),
            padding: one.width - advance,
            cells: RefCell::new(Vec::new()),
        }
    }

    /// shows `text`, which VoiceOver reads as `spoken`. with `flip`, each character that
    /// differs from the one in its place turns over; a count of another length is laid out
    /// afresh
    pub fn set(&self, text: &str, spoken: &str, flip: bool) {
        self.view
            .setAccessibilityLabel(Some(&NSString::from_str(spoken)));
        let mut cells = self.cells.borrow_mut();
        if cells.len() != text.chars().count() {
            self.lay_out(&mut cells, text);
            return;
        }
        for (cell, ch) in cells.iter_mut().zip(text.chars()) {
            if cell.shows == ch {
                continue;
            }
            let old = std::mem::replace(&mut cell.shows, ch);
            if flip {
                self.turn(cell, old, ch);
            } else {
                let text = NSString::from_str(&ch.to_string());
                cell.top.setStringValue(&text);
                cell.bottom.setStringValue(&text);
            }
        }
    }

    fn width(&self, ch: char) -> f64 {
        if ch == ':' {
            self.colon
        } else {
            self.digit.width
        }
    }

    fn lay_out(&self, cells: &mut Vec<Cell>, text: &str) {
        for cell in cells.drain(..) {
            cell.view.removeFromSuperview();
        }
        let height = self.digit.height;
        let total: f64 = text.chars().map(|ch| self.width(ch)).sum();
        self.view.setFrame(NSRect::new(
            NSPoint::new(
                (self.centre.x - total / 2.0).round(),
                (self.centre.y - height / 2.0).round(),
            ),
            NSSize::new(total, height),
        ));
        let mut x = 0.0;
        for ch in text.chars() {
            let width = self.width(ch);
            let view = NSView::initWithFrame(
                NSView::alloc(self.mtm),
                NSRect::new(NSPoint::new(x, 0.0), NSSize::new(width, height)),
            );
            let (upper, top) = self.half(ch, width, true);
            let (lower, bottom) = self.half(ch, width, false);
            upper.setFrameOrigin(NSPoint::new(0.0, height / 2.0));
            view.addSubview(&upper);
            view.addSubview(&lower);
            self.view.addSubview(&view);
            cells.push(Cell {
                shows: ch,
                view,
                top,
                bottom,
            });
            x += width;
        }
    }

    /// the upper or lower half of `ch` on its leaf, at the origin, and the label in it
    fn half(&self, ch: char, width: f64, upper: bool) -> (Retained<NSView>, Retained<NSTextField>) {
        let height = self.digit.height;
        let leaf = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height / 2.0)),
        );
        leaf.setWantsLayer(true);
        if let Some(layer) = leaf.layer() {
            let (r, g, b, a) = self.ground;
            layer.setBackgroundColor(Some(&CGColor::new_srgb(r, g, b, a)));
            layer.setMasksToBounds(true);
        }
        // the whole character, of which the leaf shows its half
        let y = if upper { -height / 2.0 } else { 0.0 };
        let label = label(
            self.mtm,
            &self.font,
            &ch.to_string(),
            NSRect::new(
                NSPoint::new(-self.padding / 2.0, y),
                NSSize::new(width + self.padding, height),
            ),
        );
        leaf.addSubview(&label);
        (leaf, label)
    }

    /// turns `cell` over from `old` to `ch`: the old top half falls onto the fold over the new
    /// one, then the new bottom half swings down from the fold over the old one
    fn turn(&self, cell: &Cell, old: char, ch: char) {
        let width = self.width(ch);
        let height = self.digit.height;
        let now = CACurrentMediaTime();

        // each leaf hangs in a view of no height on the fold, which it scales toward: seen
        // flat on, a leaf turning about the fold only gets shorter
        let hinge = |leaf: &NSView, y: f64| {
            let hinge = NSView::initWithFrame(
                NSView::alloc(self.mtm),
                NSRect::new(NSPoint::new(0.0, height / 2.0), NSSize::new(width, 0.0)),
            );
            hinge.setWantsLayer(true);
            leaf.setFrameOrigin(NSPoint::new(0.0, y));
            hinge.addSubview(leaf);
            cell.view.addSubview(&hinge);
            hinge
        };
        let (falling, _) = self.half(old, width, true);
        let (landing, _) = self.half(ch, width, false);
        let fall = hinge(&falling, 0.0);
        let land = hinge(&landing, -height / 2.0);

        let text = NSString::from_str(&ch.to_string());
        cell.top.setStringValue(&text);

        if let Some(layer) = fall.layer() {
            let turn = property("transform.scale.y", 1.0, 0.0, now, FALL, Fill::Exit);
            // it gathers speed as it falls
            turn.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(
                0.5, 0.0, 1.0, 1.0,
            )));
            layer.addAnimation_forKey(&turn, None);
        }
        if let Some(shade) = shade(&falling) {
            shade.setOpacity(0.0);
            shade.addAnimation_forKey(&fade(0.0, SHADE, now, FALL, Fill::Exit), None);
        }
        let mut settled = now + FALL;
        if let Some(layer) = land.layer() {
            let turn = spring(
                "transform.scale.y",
                0.0,
                1.0,
                now + FALL,
                POP,
                Fill::Entrance,
            );
            settled += turn.duration();
            layer.addAnimation_forKey(&turn, None);
        }
        if let Some(shade) = shade(&landing) {
            shade.setOpacity(0.0);
            shade.addAnimation_forKey(&fade(SHADE, 0.0, now + FALL, 0.18, Fill::Entrance), None);
        }

        // once the new half has landed it's the cell's own, and the leaves go
        let bottom = cell.bottom.clone();
        after(self.mtm, settled - now, move || {
            bottom.setStringValue(&text);
            fall.removeFromSuperview();
            land.removeFromSuperview();
        });
    }
}

/// a black layer over `leaf`, whose opacity is how far the leaf has turned from the light
fn shade(leaf: &NSView) -> Option<Retained<CALayer>> {
    let layer = leaf.layer()?;
    let shade = CALayer::new();
    shade.setFrame(layer.bounds());
    shade.setBackgroundColor(Some(&CGColor::new_generic_gray(0.0, 1.0)));
    shade.setZPosition(1.0);
    layer.addSublayer(&shade);
    Some(shade)
}

fn label(mtm: MainThreadMarker, font: &NSFont, text: &str, frame: NSRect) -> Retained<NSTextField> {
    let label = make_label(
        mtm,
        text,
        frame,
        &LabelStyle {
            font,
            color: &NSColor::whiteColor(),
            alignment: NSTextAlignment::Center,
            max_lines: 1,
        },
    );
    label.setAccessibilityElement(false);
    label
}
