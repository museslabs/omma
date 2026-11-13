//! native Liquid Glass refracts the mati's cobalt body. translucent rings, the pupil and
//! count sit in its content view. during a break the colour drifts and drains under a slow
//! wave, leaving the clear material and the pupil behind. when the user asks for solid
//! surfaces, a flat disc stands in for the glass.

use std::cell::Cell;
use std::f64::consts::{PI, TAU};

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

/// sRGB and alpha
pub type Rgba = (f64, f64, f64, f64);
/// lightness, chroma and hue in degrees, as css writes `oklch()`. the bead's colours are
/// kept in it because steps along it look even, and a shade keeps its colour instead of
/// greying
type Oklch = (f64, f64, f64);
/// how far along a gradient (0…1), the colour there and its alpha
type Stop = (f64, Oklch, f64);

struct Disc {
    /// its colours from where it's lightest out to its far edge, or none for the one flat
    /// `PUPIL`
    dome: Option<&'static [Stop]>,
    /// where it's lightest, in its layer's unit space with y up
    light: (f64, f64),
    /// as a share of the bead's radius
    radius: f64,
    /// how far it shifts toward the pointer, as a share of the bead's diameter. the pupil
    /// moves most, and shifting by different amounts the discs read as layers at different
    /// depths
    depth: (f64, f64),
}

/// the pupil is one flat colour, which the count's leaves match: `oklch(0.17 0.04 268)`, a
/// blue too dark to read as one but not a grey
const PUPIL: Rgba = (0.0315, 0.0555, 0.1283, 1.0);

/// the pale ring: white where the light falls on it, at the upper left, and taking on the
/// sky's blue as it turns away
const WHITE: [Stop; 5] = [
    (0.00, (0.985, 0.008, 240.0), 0.83),
    (0.40, (0.960, 0.020, 242.0), 0.81),
    (0.70, (0.900, 0.055, 246.0), 0.77),
    (0.90, (0.830, 0.090, 249.0), 0.72),
    (1.00, (0.770, 0.115, 251.0), 0.68),
];

/// the sky ring inside it, lit from the upper left. toward the lower right it deepens to
/// the bright blue that rims the pupil
const SKY: [Stop; 5] = [
    (0.00, (0.860, 0.085, 236.0), 0.60),
    (0.40, (0.800, 0.115, 240.0), 0.64),
    (0.70, (0.710, 0.155, 247.0), 0.70),
    (0.90, (0.620, 0.190, 255.0), 0.80),
    (1.00, (0.560, 0.210, 259.0), 0.88),
];

const DISCS: [Disc; 3] = [
    Disc {
        dome: Some(&WHITE),
        light: (0.36, 0.68),
        radius: 0.68,
        depth: (0.016, 0.012),
    },
    Disc {
        dome: Some(&SKY),
        light: (0.40, 0.62),
        radius: 0.55,
        depth: (0.040, 0.031),
    },
    Disc {
        dome: None,
        light: (0.5, 0.5),
        radius: 0.42,
        depth: (0.075, 0.058),
    },
];

/// a blink squashes the pupil flat like a closing lid, a little wider as it goes: its
/// scale across and up when shut
const BLINK_SHUT: (f64, f64) = (1.12, 0.10);
/// it springs open a touch too tall and narrow, then settles
const BLINK_BOUNCE: (f64, f64) = (0.96, 1.08);
/// closing is quick, opening a little slower, and the settle slowest
const BLINK_CLOSE: f64 = 0.09;
const BLINK_OPEN: f64 = 0.14;
const BLINK_SETTLE: f64 = 0.16;
/// the blinks come round again after this long. each starts this many seconds in. they're
/// spaced unevenly, the last two a double, to keep the round from reading as a beat
const BLINK_PERIOD: f64 = 9.0;
const BLINKS: [f64; 3] = [2.4, 5.9, 6.34];
/// annoyed, an upper lid comes down over this share of the pupil, at its middle, and slopes
/// by this many radians, so it reads as a frown and never as a blink. open, its edge waits
/// this far above the pupil, as a share of it
const LID_DROP: f64 = 0.45;
const LID_TILT: f64 = 0.2;
const LID_OPEN: f64 = 1.1;
/// pressed against something above it, the bead's top gives way to a straight edge, which
/// curves into the rest of the outline over this share of the radius, or over the press
/// itself while that's shallower, so the top still reaches the edge
const FLAT_ROUNDING: f64 = 0.25;
/// the top flattens no further than this share of the radius, past which the rings would
/// reach its edge
pub const FLAT_MOST: f64 = 0.4;
/// as the top flattens, the rings sink by this much of the press and the pupil by more, as if
/// pushed down through the liquid, so they stay clear of the edge
const IRIS_SINK: f64 = 0.5;
const PUPIL_SINK: f64 = 0.6;

/// the cobalt: deep and saturated, lightest where the light falls on it at the top left and
/// darkening to its far edge. its hue turns toward violet as it darkens, which keeps the
/// dark a blue
const BODY: [Stop; 5] = [
    (0.00, (0.50, 0.250, 264.0), 0.88),
    (0.35, (0.43, 0.265, 265.0), 0.88),
    (0.65, (0.36, 0.232, 266.0), 0.90),
    (0.85, (0.30, 0.195, 267.0), 0.92),
    (1.00, (0.26, 0.165, 268.0), 0.94),
];
/// where the body is lightest, in its layer's unit space with y up
const BODY_LIGHT: (f64, f64) = (0.36, 0.68);

/// the body's radius at rest, as a share of the bead's
const LIQUID: f64 = 0.92;
/// how far the body's outline strays from that as it drifts, each way
const MORPH: f64 = 0.058;
/// the shell leaves room around the pigment, and moves less so that gap stays open
const GLASS: f64 = 0.965;
const GLASS_MORPH: f64 = 0.5;
/// how long the drifting outline takes to come round to the same shape
const MORPH_PERIOD: f64 = 11.0;
/// the shapes in one round, and the points on each outline
const MORPH_FRAMES: usize = 44;
const OUTLINE_POINTS: usize = 64;
/// the outline swells from a circle over this share of its first round
const MORPH_SWELL: f64 = 0.22;
/// the count's digits, as a share of the diameter
const COUNT_SIZE: f64 = 0.12;
/// how long the liquid takes to reach the level the next second puts it at. a little over
/// the second, to keep it flowing until a late tick gives it the next level
const FLOW: f64 = 1.1;
/// the wave on the liquid's surface, as shares of the diameter, and how long one wave takes
/// to pass
const WAVE_HEIGHT: f64 = 0.011;
const WAVE_LENGTH: f64 = 0.8;
const WAVE_PERIOD: f64 = 2.8;
/// the wave layers are this many diameters wide, too wide for a passing wave to show an end
const WAVE_SPAN: f64 = 4.0;

/// linear sRGB for an oklch colour, by Björn Ottosson's matrices. out of sRGB's gamut a
/// channel is under 0 or over 1
fn oklch_to_linear_srgb((lightness, chroma, hue): Oklch) -> [f64; 3] {
    let (sin, cos) = hue.to_radians().sin_cos();
    let (a, b) = (chroma * cos, chroma * sin);
    let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ]
}

/// the same light in Display P3's wider primaries
fn linear_srgb_to_p3([r, g, b]: [f64; 3]) -> [f64; 3] {
    [
        0.822_462_1 * r + 0.177_538_0 * g,
        0.033_194_1 * r + 0.966_805_8 * g,
        0.017_082_7 * r + 0.072_397_4 * g + 0.910_519_9 * b,
    ]
}

/// the transfer curve sRGB and Display P3 share, from linear light to a stored value
fn encode(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// Display P3 for an oklch colour. one the display can't show gives up chroma until it can,
/// which keeps its lightness and hue; clamping the channels instead would shift both
fn display_p3((lightness, chroma, hue): Oklch) -> [f64; 3] {
    let linear = |chroma| linear_srgb_to_p3(oklch_to_linear_srgb((lightness, chroma, hue)));
    let shows = |rgb: [f64; 3]| rgb.iter().all(|c| (-1e-4..=1.0 + 1e-4).contains(c));
    let mut rgb = linear(chroma);
    if !shows(rgb) {
        let (mut low, mut high) = (0.0, chroma);
        for _ in 0..24 {
            let mid = (low + high) / 2.0;
            if shows(linear(mid)) {
                low = mid;
            } else {
                high = mid;
            }
        }
        rgb = linear(low);
    }
    rgb.map(|c| encode(c.clamp(0.0, 1.0)))
}

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
    // type CAGradientLayer requires; setColors copies the array and retains its colors.
    unsafe {
        let colors = &*std::ptr::from_ref(&*colors).cast::<NSArray>();
        layer.setColors(Some(colors));
    }
}

fn square(side: f64) -> NSRect {
    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side))
}

/// a gradient that runs outward from `centre`, a point in the layer's unit space with y up,
/// and ends `reach` of the layer's width and height from it
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

/// a square of coloured glass that is lightest at `light` and darkens from there to the far
/// side of the circle in it, where its last stop lands
fn dome(side: f64, light: (f64, f64), stops: &[Stop], scale: f64) -> Retained<CAGradientLayer> {
    let reach = 0.5 + (light.0 - 0.5).hypot(light.1 - 0.5);
    radial(square(side), light, reach, stops, scale)
}

/// how far the outline is from the bead's centre at `angle`, as a share of the radius,
/// `phase` (0…τ) of the way round its drift. two slow lobes and three quicker ones turn
/// against each other and swell in turn, which keeps a shape from coming back before the
/// round is over. `swell` (0…1) is how far it has grown from a circle
fn outline_radius(angle: f64, phase: f64, swell: f64) -> f64 {
    let two = (0.50 + 0.12 * (2.0 * phase + 0.7).sin()) * (2.0 * angle + phase).sin();
    let three = (0.38 + 0.10 * phase.cos()) * (3.0 * angle - phase + 1.3).sin();
    LIQUID + MORPH * swell * (two + three)
}

#[derive(Clone, Copy)]
enum Outline {
    Pigment,
    Glass,
    /// the bead's own edge, where the native glass ends
    Rim,
}

impl Outline {
    fn radius(self, angle: f64, phase: f64, swell: f64) -> f64 {
        let pigment = outline_radius(angle, phase, swell);
        match self {
            Self::Pigment => pigment,
            Self::Glass => GLASS + GLASS_MORPH * (pigment - LIQUID),
            Self::Rim => 1.0,
        }
    }

    /// how far the top is from the centre at rest, as a share of the radius
    fn top(self) -> f64 {
        self.radius(PI / 2.0, 0.0, 0.0)
    }
}

/// the lower of `a` and `b`, rounded off where they come within `k` (above 0) of each other
fn soft_min(a: f64, b: f64, k: f64) -> f64 {
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k / 4.0
}

/// point `i` of the outline's `OUTLINE_POINTS`, going round from the right, for a bead of
/// diameter `d`. a `flat` above 0 (a share of the radius) cuts the top off straight that far
/// below where it is at rest, and rounds the cut into the curve either side
fn outline_point(
    i: usize,
    d: f64,
    shape: Outline,
    (phase, swell): (f64, f64),
    flat: f64,
) -> (f64, f64) {
    let r = d / 2.0;
    let turn = (i % OUTLINE_POINTS) as f64 / OUTLINE_POINTS as f64;
    let angle = TAU * turn;
    let reach = r * shape.radius(angle, phase, swell);
    let mut y = reach * angle.sin();
    // a drifting outline rises past its resting top, so only a press cuts it
    if flat > 0.0 {
        y = soft_min(y, r * (shape.top() - flat), r * flat.min(FLAT_ROUNDING));
    }
    (r + reach * angle.cos(), r + y)
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

/// puts `mask` on `layer` while `on` and takes it off otherwise, so a layer that's masked
/// only now and then doesn't pay for the mask the rest of the time. `layer` has no other
/// mask; AppKit puts none on the native glass
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
    /// the native glass's layer, which `rim` cuts flat while the top is pressed. None with
    /// no glass, or with a glass that keeps a mask of its own because its outline drifts
    glass: Option<Retained<CALayer>>,
    rim: Retained<CAShapeLayer>,
    /// how flat `press` last left the top, as a share of the radius
    flat: Cell<f64>,
    diameter: f64,
    /// the colour beneath the glass, blurred separately during entrances and exits
    content: Retained<NSView>,
    /// the two rings, which sink as the top flattens
    iris: Retained<CALayer>,
    /// the two rings, and the socket the pupil sits in, which `look` moves and `press` sinks
    discs: [Retained<CALayer>; 3],
    /// the socket's mask, an upper lid that `annoy` lowers over the pupil
    lid: Retained<CALayer>,
    /// the pupil, which blinks inside its socket
    pupil: Retained<CALayer>,
    /// masks for the pigment and rings, sharing one moving liquid level
    surface: Option<[Retained<CAShapeLayer>; 2]>,
    /// every layer shaped by the body's outline, which `drift` sets moving
    followers: Vec<Follower>,
    count: Option<FlipCount>,
}

impl Bead {
    /// `scale` is the backing scale of the display it shows on. with `counts`, the pupil
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

        // the material samples the colour below it, including the moving liquid surface
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
            // only contentView is guaranteed to sit above the native material
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

        // the pigment keeps the mati's colours; AppKit's glass, when there is one, supplies
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

        // the eye is content inside the native glass, with its colour showing through the
        // rings. on a solid surface there's no glass, and the rings are opaque
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
        // the socket takes the look and the sink, its lid the annoyance, and the pupil
        // inside blinks on its own
        let socket = CALayer::new();
        socket.setBounds(pupil.bounds());
        socket.setPosition(centre);
        let side = pupil.bounds().size.width;
        pupil.setPosition(NSPoint::new(side / 2.0, side / 2.0));
        socket.addSublayer(&pupil);
        // the socket's mask while it's annoyed. whatever the mask covers shows, so the lid's
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
            // whatever the mask covers shows; its top edge is the surface
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
                        let swell = if swells {
                            let t = (along / MORPH_SWELL).min(1.0);
                            t * t * (3.0 - 2.0 * t)
                        } else {
                            1.0
                        };
                        outline_path(d, *shape, TAU * along, swell, 0.0)
                    })
                    .collect();
                let paths = CFArray::from_retained_objects(&paths);
                let anim =
                    CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
                // SAFETY: CFArray is toll-free bridged with NSArray. its elements are
                // CGPaths, the type both path and shadowPath animate, and it's only read
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

    /// the pigment layer a blur goes on, leaving the native glass and its content intact
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

    /// brings the count up from nothing over `duration`, from `begin`
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
    /// glass only while it's flat, and leaves its edge alone the rest of the time
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
    /// flat about its own centre, wherever it's looking, and bounces open again
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
            // SAFETY: an array of NSNumbers is an array of objects, and it's only read.
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

    /// shows `left` (0…1) of the liquid from `begin`, a Core Animation time, flowing there
    /// over `FLOW` from wherever it is on screen. without `animate` the level steps instead,
    /// for reduced motion.
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

/// the furthest the body's outline gets from the centre, as a share of the bead's radius
fn liquid_reach() -> f64 {
    LIQUID + MORPH * 1.1
}

/// where the wave's crests sit for a fill `height` (0…1 of the liquid's height), measured
/// up from the bead's bottom. full, even the troughs clear the liquid's top wherever its
/// outline has drifted; empty, the crests are under its bottom
fn level_y(diameter: f64, height: f64) -> f64 {
    let r = diameter / 2.0;
    let liquid = r * liquid_reach();
    let swell = 2.0 * diameter * WAVE_HEIGHT;
    r - liquid - 2.0 + height * (2.0 * liquid + swell + 4.0)
}

/// the share of a disc's area under a level `height` (0…1 of the diameter) up from the bottom
fn area_below(height: f64) -> f64 {
    let d = 1.0 - 2.0 * height;
    (d.acos() - d * (1.0 - d * d).sqrt()) / PI
}

/// the level that leaves `left` (0…1) of the disc's area coloured. the colour then drains at
/// an even rate though the disc is wider in the middle
fn height_for(left: f64) -> f64 {
    if left <= 0.0 {
        return 0.0;
    }
    if left >= 1.0 {
        return 1.0;
    }
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..32 {
        let mid = (low + high) / 2.0;
        if area_below(mid) < left {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_leaves_the_asked_share_of_the_area() {
        assert_eq!(height_for(0.0), 0.0);
        assert_eq!(height_for(1.0), 1.0);
        assert!((height_for(0.5) - 0.5).abs() < 1e-6);
        for left in [0.05, 0.25, 0.6, 0.9] {
            assert!((area_below(height_for(left)) - left).abs() < 1e-6);
        }
        // a sliver of colour needs more than a sliver of height, the bottom being narrow
        assert!(height_for(0.1) > 0.1);
    }

    fn close(got: [f64; 3], want: [f64; 3]) -> bool {
        got.iter()
            .zip(want)
            .all(|(got, want)| (got - want).abs() < 2e-3)
    }

    #[test]
    fn oklch_lands_on_known_display_p3_colours() {
        assert!(close(display_p3((1.0, 0.0, 0.0)), [1.0, 1.0, 1.0]));
        assert!(close(display_p3((0.0, 0.0, 0.0)), [0.0, 0.0, 0.0]));
        // sRGB's red and blue, as css gives them in oklch and in display-p3
        let red = display_p3((0.627_96, 0.257_68, 29.234));
        assert!(close(red, [0.9175, 0.2003, 0.1386]));
        let blue = display_p3((0.452_01, 0.313_21, 264.052));
        assert!(close(blue, [0.0, 0.0, 0.9595]));
    }

    #[test]
    fn a_colour_the_display_cannot_show_loses_chroma_not_hue() {
        // far outside any display
        let vivid = (0.7, 0.4, 145.0);
        let clipped = display_p3(vivid);
        assert!(clipped.iter().all(|c| (0.0..=1.0).contains(c)));
        // still a green, and one channel has reached the gamut's edge
        assert!(clipped[1] > clipped[0] && clipped[1] > clipped[2]);
        assert!(clipped.iter().any(|&c| !(1e-3..=1.0 - 1e-3).contains(&c)));
        // one it can show comes through untouched
        let (l, c, h) = BODY[0].1;
        let straight = linear_srgb_to_p3(oklch_to_linear_srgb((l, c, h))).map(encode);
        assert!(close(display_p3((l, c, h)), straight));
    }

    #[test]
    fn every_stop_is_in_order_and_inside_the_display() {
        for stops in [&BODY[..], &WHITE[..], &SKY[..]] {
            assert_eq!(stops[0].0, 0.0);
            assert_eq!(stops[stops.len() - 1].0, 1.0);
            assert!(stops.windows(2).all(|pair| pair[0].0 < pair[1].0));
            // each darkens outward from the light
            assert!(stops.windows(2).all(|pair| pair[0].1.0 > pair[1].1.0));
            for &(_, oklch, _) in stops {
                let straight = linear_srgb_to_p3(oklch_to_linear_srgb(oklch));
                assert!(straight.iter().all(|c| (0.0..=1.0).contains(c)));
            }
        }
    }

    #[test]
    fn the_pupil_is_the_oklch_colour_its_comment_names() {
        let srgb = oklch_to_linear_srgb((0.17, 0.04, 268.0)).map(encode);
        assert!(close([PUPIL.0, PUPIL.1, PUPIL.2], srgb));
    }

    #[test]
    fn each_disc_stays_inside_the_one_around_it_wherever_it_looks() {
        for pair in DISCS.windows(2) {
            let (outer, inner) = (&pair[0], &pair[1]);
            // radii are shares of the bead's radius, depths of its diameter
            let room = (outer.radius - inner.radius) / 2.0;
            assert!(inner.depth.0 - outer.depth.0 < room);
            assert!(inner.depth.1 - outer.depth.1 < room);
        }
    }

    #[test]
    fn a_pressed_outline_runs_straight_along_the_top_and_keeps_the_rest() {
        let (d, r) = (64.0, 32.0);
        let at = |i: usize, shape: Outline, flat: f64| outline_point(i, d, shape, (0.0, 0.0), flat);
        for shape in [Outline::Pigment, Outline::Glass, Outline::Rim] {
            let cut = r + r * (shape.top() - FLAT_MOST);
            for i in 0..OUTLINE_POINTS {
                let angle = TAU * i as f64 / OUTLINE_POINTS as f64;
                let reach = r * shape.radius(angle, 0.0, 0.0);
                let round = (r + reach * angle.cos(), r + reach * angle.sin());
                // unpressed, it's the circle
                assert_eq!(at(i, shape, 0.0), round);
                // pressed, nothing reaches above the cut or moves sideways, and the lower
                // half stays as it was
                let pressed = at(i, shape, FLAT_MOST);
                assert!(pressed.1 <= cut + 1e-9);
                assert_eq!(pressed.0, round.0);
                if round.1 <= r {
                    assert_eq!(pressed, round);
                }
                // over the middle third it's a straight line along the cut
                if (pressed.0 - r).abs() < d / 6.0 && pressed.1 > r {
                    assert!((pressed.1 - cut).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn an_unpressed_outline_drifts_past_its_resting_top_uncut() {
        let (d, r) = (64.0, 32.0);
        let mut highest: f64 = 0.0;
        for shape in [Outline::Pigment, Outline::Glass] {
            for frame in 0..MORPH_FRAMES {
                let phase = TAU * frame as f64 / MORPH_FRAMES as f64;
                for i in 0..OUTLINE_POINTS {
                    let angle = TAU * i as f64 / OUTLINE_POINTS as f64;
                    let reach = r * shape.radius(angle, phase, 1.0);
                    let free = (r + reach * angle.cos(), r + reach * angle.sin());
                    assert_eq!(outline_point(i, d, shape, (phase, 1.0), 0.0), free);
                    highest = highest.max((free.1 - r) / r - shape.top());
                }
            }
        }
        // the drift does rise above the resting top, so a cut there would show
        assert!(highest > 0.0);
    }

    #[test]
    fn the_eye_sinks_clear_of_a_flattened_top() {
        // shares of the radius above the centre, at the flattest press: the rings and pupil
        // sink in step with the press, so if they're clear there they're clear all the way
        let edge = Outline::Pigment.top() - FLAT_MOST;
        let reach = |disc: &Disc, sink: f64| disc.radius + 2.0 * disc.depth.1 - sink * FLAT_MOST;
        assert!(reach(&DISCS[0], IRIS_SINK) < edge);
        assert!(reach(&DISCS[1], IRIS_SINK) < edge);
        assert!(reach(&DISCS[2], PUPIL_SINK) < edge);
    }

    #[test]
    fn an_annoyed_lid_never_shuts_the_eye_and_an_open_one_clears_it() {
        // the share of the pupil under the sloping lid's low end: well over a third shows
        let lowest = LID_DROP + LID_TILT.tan() / 2.0;
        assert!(lowest < 0.6);
        // open, the lid's edge stays above the pupil
        const { assert!(LID_OPEN > 1.0) };
    }

    #[test]
    fn the_blinks_follow_one_another_inside_the_round() {
        let length = BLINK_CLOSE + BLINK_OPEN + BLINK_SETTLE;
        assert!(BLINKS.windows(2).all(|pair| pair[0] + length < pair[1]));
        assert!(BLINKS[BLINKS.len() - 1] + length < BLINK_PERIOD);
    }

    #[test]
    fn full_and_empty_leave_no_surface_in_the_liquid() {
        let (d, r) = (300.0, 150.0);
        let troughs = level_y(d, 1.0) - 2.0 * d * WAVE_HEIGHT;
        assert!(troughs > r + r * liquid_reach());
        assert!(level_y(d, 0.0) < r - r * liquid_reach());
    }

    #[test]
    fn the_liquid_stays_inside_the_glass_and_around_the_rings() {
        let mut furthest: f64 = 0.0;
        let mut nearest: f64 = 1.0;
        for frame in 0..360 {
            for step in 0..360 {
                let at = |n: usize| TAU * n as f64 / 360.0;
                let reach = outline_radius(at(step), at(frame), 1.0);
                let glass = Outline::Glass.radius(at(step), at(frame), 1.0);
                // the draining pigment starts inside the shell throughout the morph
                assert!(glass > reach && glass < 1.0);
                furthest = furthest.max(reach);
                nearest = nearest.min(reach);
            }
        }
        // the liquid fits inside the glass and always contains the pale ring
        assert!(furthest <= liquid_reach() && liquid_reach() < 1.0);
        assert!(nearest > DISCS[0].radius);
        // and at rest it's a circle
        assert_eq!(outline_radius(1.0, 2.0, 0.0), LIQUID);
    }
}
