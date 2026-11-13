//! native Liquid Glass refracts the mati's cobalt body. translucent rings, the pupil and
//! count sit in its content view. during a break the body's outline drifts.

use std::f64::consts::TAU;

use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{NSColor, NSGlassEffectView, NSGlassEffectViewStyle, NSView};
use objc2_core_foundation::{CFArray, CFRetained, CGSize};
use objc2_core_graphics::{CGColor, CGMutablePath};
use objc2_foundation::{MainThreadMarker, NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{
    CAGradientLayer, CAKeyframeAnimation, CALayer, CAMediaTiming, CAShapeLayer,
    kCAGradientLayerRadial,
};

use super::flip::FlipCount;
use super::panel::{Fill, set_fill};

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
    },
    Disc {
        dome: Some(&SKY),
        light: (0.40, 0.62),
        radius: 0.55,
    },
    Disc {
        dome: None,
        light: (0.5, 0.5),
        radius: 0.42,
    },
];

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
}

impl Outline {
    fn radius(self, angle: f64, phase: f64, swell: f64) -> f64 {
        let pigment = outline_radius(angle, phase, swell);
        match self {
            Self::Pigment => pigment,
            Self::Glass => GLASS + GLASS_MORPH * (pigment - LIQUID),
        }
    }
}

/// adds the outline to `path` as a closed curve through `OUTLINE_POINTS` points, for a bead
/// of diameter `d`. every outline has the same points in the same order, which lets Core
/// Animation move one into another
fn add_outline(path: &CGMutablePath, d: f64, shape: Outline, phase: f64, swell: f64) {
    let r = d / 2.0;
    let point = |i: usize| {
        let turn = (i % OUTLINE_POINTS) as f64 / OUTLINE_POINTS as f64;
        let angle = TAU * turn;
        let reach = r * shape.radius(angle, phase, swell);
        (r + reach * angle.cos(), r + reach * angle.sin())
    };
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

fn outline_path(d: f64, shape: Outline, phase: f64, swell: f64) -> CFRetained<CGMutablePath> {
    let path = CGMutablePath::new();
    add_outline(&path, d, shape, phase, swell);
    path
}

/// a shape layer holding the outline at rest, a circle
fn outlined(d: f64, shape: Outline, scale: f64) -> Retained<CAShapeLayer> {
    let layer = CAShapeLayer::new();
    layer.setFrame(square(d));
    layer.setContentsScale(scale);
    layer.setPath(Some(&outline_path(d, shape, 0.0, 0.0)));
    layer
}

struct Follower {
    layer: Retained<CALayer>,
    key: &'static str,
    shape: Outline,
}

pub struct Bead {
    /// what goes in the view hierarchy, `diameter` square
    pub view: Retained<NSView>,
    diameter: f64,
    /// the colour beneath the glass, blurred separately during entrances and exits
    content: Retained<NSView>,
    /// every layer shaped by the body's outline, which `drift` sets moving
    followers: Vec<Follower>,
    count: FlipCount,
}

impl Bead {
    /// `scale` is the backing scale of the display it shows on. the pupil shows a number
    /// that `set_count` changes
    pub fn new(mtm: MainThreadMarker, diameter: f64, scale: f64) -> Bead {
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
            layer.setShadowPath(Some(&outline_path(d, Outline::Glass, 0.0, 0.0)));
            follow(&layer, "shadowPath", Outline::Glass);
        }

        // the material samples the colour below it
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        content.setWantsLayer(true);
        view.addSubview(&content);
        let foreground = NSView::initWithFrame(NSView::alloc(mtm), frame);
        foreground.setWantsLayer(true);
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setCornerRadius(r);
        glass.setStyle(NSGlassEffectViewStyle::Regular);
        // only contentView is guaranteed to sit above the native material
        glass.setContentView(Some(&foreground));
        view.addSubview(&glass);
        glass.setWantsLayer(true);
        if let Some(layer) = glass.layer() {
            let outline = outlined(d, Outline::Glass, scale);
            follow(&outline, "path", Outline::Glass);
            // SAFETY: the mask is a fresh layer with no superlayer of its own
            unsafe { layer.setMask(Some(&outline)) };
        }
        let root = content.layer().expect("a view that wants a layer has one");

        // the pigment keeps the mati's colours; AppKit supplies its reflections and refraction
        let body = CALayer::new();
        body.setFrame(frame);
        let cut = outlined(d, Outline::Pigment, scale);
        // SAFETY: the mask is a fresh layer with no superlayer of its own
        unsafe { body.setMask(Some(&cut)) };
        follow(&cut, "path", Outline::Pigment);
        let cobalt = dome(d, BODY_LIGHT, &BODY, scale);
        cobalt.setPosition(centre);
        body.addSublayer(&cobalt);
        root.addSublayer(&body);

        // the eye is content inside the native glass, with its colour showing through the rings
        let discs = DISCS.each_ref().map(|disc| {
            let side = d * disc.radius;
            let layer = match disc.dome {
                Some(stops) => Retained::into_super(dome(side, disc.light, stops, scale)),
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
        let front = foreground
            .layer()
            .expect("a view that wants a layer has one");
        for disc in &discs {
            front.addSublayer(disc);
        }

        let count = FlipCount::new(mtm, d * COUNT_SIZE, PUPIL, centre);
        foreground.addSubview(&count.view);

        Bead {
            view,
            diameter: d,
            content,
            followers,
            count,
        }
    }

    /// sets the body's outline drifting from `begin`, for as long as the bead lives: it
    /// swells out of its circle, and from then on never holds a shape
    pub fn drift(&self, begin: f64) {
        let d = self.diameter;
        for Follower { layer, key, shape } in &self.followers {
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
                        outline_path(d, *shape, TAU * along, swell)
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
        self.count.set(text, spoken, flip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_body_stays_inside_the_glass_and_around_the_rings() {
        let mut nearest: f64 = 1.0;
        for frame in 0..360 {
            for step in 0..360 {
                let at = |n: usize| TAU * n as f64 / 360.0;
                let reach = outline_radius(at(step), at(frame), 1.0);
                let glass = Outline::Glass.radius(at(step), at(frame), 1.0);
                // the pigment stays inside the shell throughout the morph
                assert!(glass > reach && glass < 1.0);
                nearest = nearest.min(reach);
            }
        }
        // and always contains the pale ring
        assert!(nearest > DISCS[0].radius);
        // and at rest it's a circle
        assert_eq!(outline_radius(1.0, 2.0, 0.0), LIQUID);
    }
}
