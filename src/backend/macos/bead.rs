//! native Liquid Glass refracts the mati's cobalt body. translucent rings, the pupil and the
//! count sit in its content view. during a break, the color drifts and drains under a slow
//! wave. the clear material and the pupil stay behind. when the user asks for solid surfaces, a
//! flat disc replaces the glass

use std::cell::Cell;
use std::f64::consts::TAU;

use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{NSColor, NSGlassEffectView, NSGlassEffectViewStyle, NSView};
use objc2_core_foundation::{CFArray, CFRetained, CGAffineTransform, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath, CGPath};
use objc2_foundation::{MainThreadMarker, NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{
    CAGradientLayer, CAKeyframeAnimation, CALayer, CAMediaTiming, CAShapeLayer,
    kCAGradientLayerRadial,
};

use super::flip::FlipCount;
use super::panel::{
    Fill, arrive, fade, linear, presented, property, set_fill, solid_surfaces, travel,
    without_actions,
};
use crate::mati::colour::{
    BODY, BODY_LIGHT, DISCS, Oklch, PUPIL, Rgba, Stop, display_p3, dome_reach,
};
use crate::mati::form::{
    BLINK_BOUNCE, BLINK_CLOSE, BLINK_OPEN, BLINK_PERIOD, BLINK_SETTLE, BLINK_SHUT, BLINKS,
    COUNT_SIZE, FLOW, IRIS_SINK, LID_DROP, LID_OPEN, LID_TILT, MORPH_FRAMES, MORPH_PERIOD,
    OUTLINE_POINTS, Outline, PUPIL_SINK, WAVE_HEIGHT, WAVE_LENGTH, WAVE_PERIOD, WAVE_SPAN,
    height_for, level_y, outline_point, swell_at,
};

fn color((r, g, b, a): Rgba) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn p3_color(oklch: Oklch, alpha: f64) -> CFRetained<CGColor> {
    let [r, g, b] = display_p3(oklch);
    NSColor::colorWithDisplayP3Red_green_blue_alpha(r, g, b, alpha)
        .CGColor()
        .into()
}

fn set_colors(layer: &CAGradientLayer, colors: &[CFRetained<CGColor>]) {
    let colors = CFArray::from_retained_objects(colors);
    // SAFETY: CFArray is toll-free bridged with NSArray. its elements are CGColors, the
    // type CAGradientLayer requires. setColors copies the array and retains its colors
    unsafe {
        let colors = &*std::ptr::from_ref(&*colors).cast::<NSArray>();
        layer.setColors(Some(colors));
    }
}

fn square(side: f64) -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side))
}

/// a gradient that runs outward from `centre`, a point in the layer's unit space with y up. it
/// ends `reach` of the layer's width and height from `centre`
fn radial(
    bounds: NSRect,
    centre: (f64, f64),
    reach: f64,
    stops: &[Stop],
    scale: f64,
) -> Retained<CAGradientLayer> {
    let layer = CAGradientLayer::new();
    layer.setBounds(bounds);
    layer.setContentsScale(scale);
    // SAFETY: the constant is one of the gradient types Core Animation defines
    layer.setType(unsafe { kCAGradientLayerRadial });
    layer.setStartPoint(NSPoint::new(centre.0, centre.1));
    layer.setEndPoint(NSPoint::new(centre.0 + reach, centre.1 + reach));
    let colors: Vec<_> = stops
        .iter()
        .map(|&(_, oklch, alpha)| p3_color(oklch, alpha))
        .collect();
    set_colors(&layer, &colors);
    let locations: Vec<_> = stops
        .iter()
        .map(|&(location, ..)| NSNumber::new_f64(location))
        .collect();
    layer.setLocations(Some(&NSArray::from_retained_slice(&locations)));
    layer
}

/// a square of colored glass that is lightest at `light`. it darkens from there to the far
/// side of the circle in the square, where its last stop lands
fn dome(side: f64, light: (f64, f64), stops: &[Stop], scale: f64) -> Retained<CAGradientLayer> {
    radial(square(side), light, dome_reach(light), stops, scale)
}

/// adds the outline to `path` as a closed curve through `OUTLINE_POINTS` points, for a bead
/// of diameter `d`, `flat` as `outline_point` takes it. every outline has the same points in
/// the same order, which lets Core Animation move one into another
fn add_outline(path: &CGMutablePath, d: f64, shape: Outline, drift: (f64, f64), flat: f64) {
    let point = |i: usize| outline_point(i, d, shape, drift, flat);
    let between = |a: (f64, f64), b: (f64, f64)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    // each point pulls the curve toward it between the midpoints either side, which makes
    // one smooth line with no corner anywhere
    let start = between(point(OUTLINE_POINTS - 1), point(0));
    // SAFETY: a null transform means none
    unsafe {
        let none = std::ptr::null();
        CGMutablePath::move_to_point(Some(path), none, start.0, start.1);
        for i in 0..OUTLINE_POINTS {
            let (pull, to) = (point(i), between(point(i), point(i + 1)));
            CGMutablePath::add_quad_curve_to_point(Some(path), none, pull.0, pull.1, to.0, to.1);
        }
        CGMutablePath::close_subpath(Some(path));
    }
}

/// the outline `phase` (0…τ) of the way round its drift, `swell` (0…1) grown from a circle,
/// and with its top `flat` as `outline_point` takes it
fn outline_path(
    d: f64,
    shape: Outline,
    phase: f64,
    swell: f64,
    flat: f64,
) -> CFRetained<CGMutablePath> {
    let path = CGMutablePath::new();
    add_outline(&path, d, shape, (phase, swell), flat);
    path
}

/// a shape layer holding the outline at rest, a circle
fn outlined(d: f64, shape: Outline, scale: f64) -> Retained<CAShapeLayer> {
    let layer = CAShapeLayer::new();
    layer.setFrame(square(d));
    layer.setContentsScale(scale);
    layer.setPath(Some(&outline_path(d, shape, 0.0, 0.0, 0.0)));
    layer
}

struct Follower {
    layer: Retained<CALayer>,
    key: &'static str,
    shape: Outline,
}

/// puts `mask` on `layer` while `on` and takes it off otherwise. this way, a layer that needs
/// the mask only now and then does not pay for it the rest of the time. `layer` has no other
/// mask. AppKit puts none on the native glass
fn mask_while(layer: &CALayer, mask: &CALayer, on: bool) {
    if on != layer.mask().is_some() {
        // SAFETY: `mask` is only ever this layer's mask, and has no superlayer of its own
        unsafe { layer.setMask(on.then_some(mask)) };
    }
}

impl Follower {
    /// gives the layer `path` to keep while no animation runs
    fn hold(&self, path: &CGPath) {
        if self.key == "shadowPath" {
            self.layer.setShadowPath(Some(path));
        } else {
            self.layer
                .downcast_ref::<CAShapeLayer>()
                .expect("a layer that follows by its path is a shape layer")
                .setPath(Some(path));
        }
    }
}

pub struct Bead {
    /// what goes in the view hierarchy, `diameter` square
    pub view: Retained<NSView>,
    /// the native glass's layer, which `rim` cuts flat while a press flattens the top. None
    /// with no glass, or with a glass that keeps a mask of its own because its outline drifts
    glass: Option<Retained<CALayer>>,
    rim: Retained<CAShapeLayer>,
    /// how flat `press` last left the top, as a share of the radius
    flat: Cell<f64>,
    diameter: f64,
    /// the color beneath the glass, blurred separately during entrances and exits
    content: Retained<NSView>,
    /// the two rings, which sink as the top flattens
    iris: Retained<CALayer>,
    /// the two rings, and the socket that the pupil sits in, which `look` moves and `press` sinks
    discs: [Retained<CALayer>; 3],
    /// the socket's mask, an upper lid that `annoy` lowers over the pupil
    lid: Retained<CALayer>,
    /// the pupil, which blinks inside its socket
    pupil: Retained<CALayer>,
    /// masks for the pigment and the rings. the two masks share one moving liquid level
    surface: Option<[Retained<CAShapeLayer>; 2]>,
    /// every layer shaped by the body's outline, which `drift` sets moving
    followers: Vec<Follower>,
    count: Option<FlipCount>,
}

impl Bead {
    /// `scale` is the backing scale of the display that it shows on. with `counts`, the pupil
    /// shows a number that `set_count` changes, the liquid can drain and the outline can
    /// drift
    pub fn new(mtm: MainThreadMarker, diameter: f64, scale: f64, counts: bool) -> Bead {
        let d = diameter;
        let r = d / 2.0;
        let centre = NSPoint::new(r, r);
        let frame = square(d);
        let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
        view.setWantsLayer(true);
        let mut followers = Vec::new();
        let mut follow = |layer: &CALayer, key, shape| {
            followers.push(Follower {
                layer: layer.retain(),
                key,
                shape,
            });
        };
        if let Some(layer) = view.layer() {
            layer.setShadowColor(Some(&CGColor::new_generic_gray(0.0, 1.0)));
            layer.setShadowOpacity(0.20);
            layer.setShadowRadius(d * 0.035);
            layer.setShadowOffset(CGSize::new(0.0, -d * 0.025));
            layer.setShadowPath(Some(&outline_path(d, Outline::Glass, 0.0, 0.0, 0.0)));
            follow(&layer, "shadowPath", Outline::Glass);
        }

        let solid = solid_surfaces();
        if solid {
            // a shape rather than a rounded layer, so a press flattens the disc, border and all
            let disc = outlined(d, Outline::Rim, scale);
            disc.setFillColor(Some(&CGColor::new_generic_gray(0.14, 1.0)));
            disc.setStrokeColor(Some(&CGColor::new_generic_gray(1.0, 0.35)));
            disc.setLineWidth(1.0);
            follow(&disc, "path", Outline::Rim);
            let host = NSView::initWithFrame(NSView::alloc(mtm), frame);
            host.setLayer(Some(&disc));
            host.setWantsLayer(true);
            view.addSubview(&host);
        }

        // the material samples the color below it, including the moving liquid surface
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        content.setWantsLayer(true);
        view.addSubview(&content);
        let foreground = NSView::initWithFrame(NSView::alloc(mtm), frame);
        foreground.setWantsLayer(true);
        let mut cuttable = None;
        if solid {
            view.addSubview(&foreground);
        } else {
            let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
            glass.setCornerRadius(r);
            glass.setStyle(NSGlassEffectViewStyle::Regular);
            // AppKit promises to put only contentView above the native material
            glass.setContentView(Some(&foreground));
            view.addSubview(&glass);
            glass.setWantsLayer(true);
            let layer = glass.layer().expect("a view that wants a layer has one");
            if counts {
                let outline = outlined(d, Outline::Glass, scale);
                follow(&outline, "path", Outline::Glass);
                // SAFETY: the mask is a fresh layer with no superlayer of its own
                unsafe { layer.setMask(Some(&outline)) };
            } else {
                cuttable = Some(layer);
            }
        }
        let root = content.layer().expect("a view that wants a layer has one");

        let liquid = CALayer::new();
        liquid.setFrame(frame);
        root.addSublayer(&liquid);

        // the pigment keeps the mati's colors. AppKit's glass, when there is one, supplies
        // its reflections and refraction
        let body = CALayer::new();
        body.setFrame(frame);
        let cut = outlined(d, Outline::Pigment, scale);
        // SAFETY: the mask is a fresh layer with no superlayer of its own
        unsafe { body.setMask(Some(&cut)) };
        follow(&cut, "path", Outline::Pigment);
        let cobalt = BODY.map(|(at, oklch, alpha)| (at, oklch, if solid { 1.0 } else { alpha }));
        let cobalt = dome(d, BODY_LIGHT, &cobalt, scale);
        cobalt.setPosition(centre);
        body.addSublayer(&cobalt);
        liquid.addSublayer(&body);

        // the eye is content inside the native glass, and its color shows through the rings.
        // on a solid surface, there is no glass and the rings are opaque
        let discs = DISCS.each_ref().map(|disc| {
            let side = d * disc.radius;
            let layer = match disc.dome {
                Some(stops) => {
                    let stops: Vec<_> = stops
                        .iter()
                        .map(|&(at, oklch, alpha)| (at, oklch, if solid { 1.0 } else { alpha }))
                        .collect();
                    Retained::into_super(dome(side, disc.light, &stops, scale))
                }
                None => {
                    let layer = CALayer::new();
                    layer.setBounds(square(side));
                    layer.setContentsScale(scale);
                    layer.setBackgroundColor(Some(&color(PUPIL)));
                    layer
                }
            };
            layer.setCornerRadius(side / 2.0);
            layer.setMasksToBounds(true);
            layer.setPosition(centre);
            layer
        });
        let [white, sky, pupil] = discs;
        let front = foreground
            .layer()
            .expect("a view that wants a layer has one");
        let iris = CALayer::new();
        iris.setFrame(frame);
        iris.addSublayer(&white);
        iris.addSublayer(&sky);
        front.addSublayer(&iris);
        // the socket takes the look and the sink. its lid takes the annoyance, and the pupil
        // inside blinks on its own
        let socket = CALayer::new();
        socket.setBounds(pupil.bounds());
        socket.setPosition(centre);
        let side = pupil.bounds().size.width;
        pupil.setPosition(NSPoint::new(side / 2.0, side / 2.0));
        socket.addSublayer(&pupil);
        // the socket's mask while it is annoyed. whatever the mask covers shows, so the lid's
        // edge is its top. it pivots about the middle of that edge
        let lid = CALayer::new();
        lid.setBounds(square(3.0 * side));
        lid.setAnchorPoint(NSPoint::new(0.5, 1.0));
        lid.setBackgroundColor(Some(&CGColor::new_generic_gray(0.0, 1.0)));

        let surface = counts.then(|| {
            let wave = || {
                let layer = CAShapeLayer::new();
                layer.setBounds(NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(WAVE_SPAN * d, 2.0 * d),
                ));
                layer.setAnchorPoint(NSPoint::new(0.5, 1.0));
                layer.setPosition(NSPoint::new(r, level_y(d, 1.0)));
                layer.setContentsScale(scale);
                layer.setPath(Some(&wave_path(d)));
                layer
            };
            // whatever the mask covers shows. its top edge is the surface
            let mask = wave();
            // SAFETY: the mask is a fresh layer with no superlayer of its own
            unsafe { liquid.setMask(Some(&mask)) };
            let iris_mask = wave();
            // SAFETY: each masked layer owns a separate, fresh mask
            unsafe { iris.setMask(Some(&iris_mask)) };

            [mask, iris_mask]
        });
        front.addSublayer(&socket);

        let count = counts.then(|| {
            let count = FlipCount::new(mtm, d * COUNT_SIZE, PUPIL, centre);
            foreground.addSubview(&count.view);
            count
        });

        Bead {
            view,
            glass: cuttable,
            rim: outlined(d, Outline::Rim, scale),
            flat: Cell::new(0.0),
            diameter: d,
            content,
            iris,
            discs: [white, sky, socket],
            lid,
            pupil,
            surface,
            followers,
            count,
        }
    }

    /// sets the body's outline drifting from `begin`, for as long as the bead lives: it
    /// swells out of its circle, and from then on never holds a shape
    pub fn drift(&self, begin: f64) {
        let d = self.diameter;
        for Follower { layer, key, shape } in &self.followers {
            // the rim is a circle whatever the drift does
            if matches!(shape, Outline::Rim) {
                continue;
            }
            let round = |swells: bool| {
                let paths: Vec<_> = (0..=MORPH_FRAMES)
                    .map(|frame| {
                        let along = frame as f64 / MORPH_FRAMES as f64;
                        let swell = if swells { swell_at(along) } else { 1.0 };
                        outline_path(d, *shape, TAU * along, swell, 0.0)
                    })
                    .collect();
                let paths = CFArray::from_retained_objects(&paths);
                let anim =
                    CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
                // SAFETY: CFArray is toll-free bridged with NSArray. its elements are
                // CGPaths, the type both path and shadowPath animate, and nothing writes to it
                unsafe {
                    let paths = &*std::ptr::from_ref(&*paths).cast::<NSArray>();
                    anim.setValues(Some(paths));
                }
                anim.setDuration(MORPH_PERIOD);
                anim
            };
            // the first round grows out of the circle and ends on the shape, and at the
            // speed, that every later round starts with
            let first = round(true);
            first.setBeginTime(begin);
            set_fill(&first, Fill::Exit);
            layer.addAnimation_forKey(&first, Some(&NSString::from_str("swell")));
            let rounds = round(false);
            rounds.setBeginTime(begin + MORPH_PERIOD);
            rounds.setRepeatCount(f32::INFINITY);
            layer.addAnimation_forKey(&rounds, Some(&NSString::from_str("drift")));
        }
    }

    /// the pigment layer that a blur goes on. the blur leaves the native glass and its content
    /// intact
    pub fn content_layer(&self) -> Option<Retained<CALayer>> {
        self.content.layer()
    }

    /// the number in the pupil, and how VoiceOver reads it. with `flip`, the digits that
    /// change turn over like the leaves of a flip clock
    pub fn set_count(&self, text: &str, spoken: &str, flip: bool) {
        if let Some(count) = &self.count {
            count.set(text, spoken, flip);
        }
    }

    pub fn reveal_count(&self, begin: f64, duration: f64) {
        if let Some(layer) = self.count.as_ref().and_then(|count| count.view.layer()) {
            layer.addAnimation_forKey(&fade(0.0, 1.0, begin, duration, Fill::Entrance), None);
        }
    }

    /// shifts the inner discs toward a point `x`, `y` in -1…1, with y up. the pupil moves most
    pub fn look(&self, x: f64, y: f64) {
        let d = self.diameter;
        without_actions(|| {
            for (layer, disc) in self.discs.iter().zip(&DISCS) {
                layer.setPosition(NSPoint::new(
                    d / 2.0 + x * d * disc.depth.0,
                    d / 2.0 + y * d * disc.depth.1,
                ));
            }
        });
    }

    /// presses the top flat, `depth` points down from where it is at rest, as against a wall
    /// above, up to `FLAT_MOST` of the radius. the eye sinks as the top flattens
    pub fn press(&self, depth: f64) {
        let r = self.diameter / 2.0;
        let flat = depth.max(0.0) / r;
        if flat == self.flat.replace(flat) {
            return;
        }
        let sink = |by: f64| CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: -by * flat * r,
        };
        without_actions(|| {
            self.flatten(flat);
            self.iris.setAffineTransform(sink(IRIS_SINK));
            self.discs[2].setAffineTransform(sink(PUPIL_SINK));
        });
    }

    /// lowers the upper lid over the pupil by `amount`, from open at 0 to as annoyed as it
    /// gets at 1. `slope` 1 tips the lid down on the left, -1 on the right
    pub fn annoy(&self, amount: f64, slope: f64) {
        let annoyed = amount > 0.0;
        without_actions(|| {
            if annoyed {
                let side = self.pupil.bounds().size.width;
                let edge = side * (LID_OPEN - amount * (LID_OPEN - 1.0 + LID_DROP));
                let (sin, cos) = (amount * slope * LID_TILT).sin_cos();
                self.lid.setPosition(NSPoint::new(side / 2.0, edge));
                self.lid.setAffineTransform(CGAffineTransform {
                    a: cos,
                    b: sin,
                    c: -sin,
                    d: cos,
                    tx: 0.0,
                    ty: 0.0,
                });
            }
            mask_while(&self.discs[2], &self.lid, annoyed);
        });
    }

    /// reshapes every outline with its top `flat`, a share of the radius. the rim cuts the
    /// glass only while it is flat, and leaves its edge alone the rest of the time
    fn flatten(&self, flat: f64) {
        let d = self.diameter;
        for follower in &self.followers {
            follower.hold(&outline_path(d, follower.shape, 0.0, 0.0, flat));
        }
        if let Some(glass) = &self.glass {
            if flat > 0.0 {
                self.rim
                    .setPath(Some(&outline_path(d, Outline::Rim, 0.0, 0.0, flat)));
            }
            mask_while(glass, &self.rim, flat > 0.0);
        }
    }

    /// sets the pupil blinking, for as long as the bead lives: now and then it squashes
    /// flat about its own center, wherever it looks, and bounces open again
    pub fn blink(&self) {
        let numbers = |numbers: &[f64]| {
            let numbers: Vec<_> = numbers.iter().map(|&n| NSNumber::new_f64(n)).collect();
            NSArray::from_retained_slice(&numbers)
        };
        for (key, shut, bounce) in [
            ("transform.scale.x", BLINK_SHUT.0, BLINK_BOUNCE.0),
            ("transform.scale.y", BLINK_SHUT.1, BLINK_BOUNCE.1),
        ] {
            let mut values = vec![1.0];
            let mut times = vec![0.0];
            let mut timings = Vec::new();
            for start in BLINKS {
                let shut_at = start + BLINK_CLOSE;
                let open_at = shut_at + BLINK_OPEN;
                for (at, value, timing) in [
                    (start, 1.0, linear()),
                    (shut_at, shut, travel()),
                    (open_at, bounce, arrive()),
                    (open_at + BLINK_SETTLE, 1.0, travel()),
                ] {
                    values.push(value);
                    times.push(at / BLINK_PERIOD);
                    timings.push(timing);
                }
            }
            values.push(1.0);
            times.push(1.0);
            timings.push(linear());

            let anim = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
            // SAFETY: an array of NSNumbers is an array of objects, and nothing writes to it.
            // NSNumbers are the value type these key paths animate
            unsafe {
                let values: Retained<NSArray> = Retained::cast_unchecked(numbers(&values));
                anim.setValues(Some(&values));
            }
            anim.setKeyTimes(Some(&numbers(&times)));
            anim.setTimingFunctions(Some(&NSArray::from_retained_slice(&timings)));
            anim.setDuration(BLINK_PERIOD);
            anim.setRepeatCount(f32::INFINITY);
            self.pupil
                .addAnimation_forKey(&anim, Some(&NSString::from_str(key)));
        }
    }

    /// eases the inner discs back to the middle from where `look(x, y)` puts them, over
    /// `duration` from `begin`. until then they stay where that look left them
    pub fn look_away_from(&self, x: f64, y: f64, begin: f64, duration: f64) {
        let d = self.diameter;
        for (layer, disc) in self.discs.iter().zip(&DISCS) {
            for (key, lean) in [
                ("position.x", x * d * disc.depth.0),
                ("position.y", y * d * disc.depth.1),
            ] {
                let back = property(
                    key,
                    d / 2.0 + lean,
                    d / 2.0,
                    begin,
                    duration,
                    Fill::Entrance,
                );
                back.setTimingFunction(Some(&arrive()));
                layer.addAnimation_forKey(&back, None);
            }
        }
    }

    /// sets the wave on the liquid's surface going, for as long as the bead lives
    pub fn flow(&self) {
        let Some(surface) = &self.surface else {
            return;
        };
        let r = self.diameter / 2.0;
        let pass = property(
            "position.x",
            r,
            r + self.diameter * WAVE_LENGTH,
            0.0,
            WAVE_PERIOD,
            Fill::Entrance,
        );
        pass.setTimingFunction(Some(&linear()));
        pass.setRepeatCount(f32::INFINITY);
        for layer in surface {
            layer.addAnimation_forKey(&pass, Some(&NSString::from_str("wave")));
        }
    }

    /// shows `left` (0…1) of the liquid from `begin`, a Core Animation time. the level flows
    /// there over `FLOW` from wherever it is on screen. without `animate`, the level steps
    /// instead, for reduced motion
    pub fn drain_to(&self, left: f64, begin: f64, animate: bool) {
        let Some(surface) = &self.surface else {
            return;
        };
        let y = level_y(self.diameter, height_for(left.clamp(0.0, 1.0)));
        for layer in surface {
            let from = presented(layer, "position.y").unwrap_or(layer.position().y);
            without_actions(|| layer.setPosition(NSPoint::new(self.diameter / 2.0, y)));
            if animate {
                let flow = property("position.y", from, y, begin, FLOW, Fill::Entrance);
                flow.setTimingFunction(Some(&linear()));
                layer.addAnimation_forKey(&flow, Some(&NSString::from_str("level")));
            }
        }
    }
}

/// a filled mask below a sine wave, in a `WAVE_SPAN` by two diameters box
fn wave_path(d: f64) -> CFRetained<CGMutablePath> {
    let (width, height) = (WAVE_SPAN * d, 2.0 * d);
    let swell = d * WAVE_HEIGHT;
    let y = |x: f64| height - swell + swell * (TAU * x / (d * WAVE_LENGTH)).sin();
    let steps = (WAVE_SPAN / WAVE_LENGTH * 24.0).ceil() as usize;
    let path = CGMutablePath::new();
    // SAFETY: a null transform means none
    unsafe {
        let none = std::ptr::null();
        CGMutablePath::move_to_point(Some(&path), none, 0.0, 0.0);
        CGMutablePath::add_line_to_point(Some(&path), none, 0.0, y(0.0));
        for step in 1..=steps {
            let x = width * step as f64 / steps as f64;
            CGMutablePath::add_line_to_point(Some(&path), none, x, y(x));
        }
        CGMutablePath::add_line_to_point(Some(&path), none, width, 0.0);
        CGMutablePath::close_subpath(Some(&path));
    }
    path
}
