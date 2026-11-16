//! the bead's colors. they are in oklch, because steps along it look even and a shade keeps
//! its color instead of graying. each display gets them in its own gamut: Display P3 on
//! macOS, sRGB on Wayland

/// sRGB and alpha
pub type Rgba = (f64, f64, f64, f64);
/// lightness, chroma and hue in degrees, as css writes `oklch()`
pub type Oklch = (f64, f64, f64);
/// how far along a gradient (0…1), the color there and its alpha
pub type Stop = (f64, Oklch, f64);

pub struct Disc {
    /// its colors from where it is lightest out to its far edge, or none for the one flat
    /// `PUPIL`
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub dome: Option<&'static [Stop]>,
    /// where it is lightest, in its layer's unit space with y up
    pub light: (f64, f64),
    /// as a share of the bead's radius
    pub radius: f64,
    /// how far it shifts toward the pointer, as a share of the bead's diameter. the pupil
    /// moves most. the discs shift by different amounts, so they read as layers at different
    /// depths
    pub depth: (f64, f64),
}

/// the pupil is one flat color, which the count's leaves match: `oklch(0.17 0.04 268)`, a
/// blue too dark to read as one but not a gray
pub const PUPIL: Rgba = (0.0315, 0.0555, 0.1283, 1.0);

/// the pale ring: white where the light falls on it, at the upper left. it takes on the sky's
/// blue as it turns away
pub const WHITE: [Stop; 5] = [
    (0.00, (0.985, 0.008, 240.0), 0.83),
    (0.40, (0.960, 0.020, 242.0), 0.81),
    (0.70, (0.900, 0.055, 246.0), 0.77),
    (0.90, (0.830, 0.090, 249.0), 0.72),
    (1.00, (0.770, 0.115, 251.0), 0.68),
];

/// the sky ring inside it, lit from the upper left. toward the lower right it deepens to
/// the bright blue that rims the pupil
pub const SKY: [Stop; 5] = [
    (0.00, (0.860, 0.085, 236.0), 0.60),
    (0.40, (0.800, 0.115, 240.0), 0.64),
    (0.70, (0.710, 0.155, 247.0), 0.70),
    (0.90, (0.620, 0.190, 255.0), 0.80),
    (1.00, (0.560, 0.210, 259.0), 0.88),
];

/// the white ring, the sky ring and the pupil, from the outside in
pub const DISCS: [Disc; 3] = [
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

/// the cobalt: deep and saturated. it is lightest where the light falls on it at the top
/// left, and darkens to its far edge. its hue turns toward violet as it darkens, which keeps
/// the dark a blue
pub const BODY: [Stop; 5] = [
    (0.00, (0.50, 0.250, 264.0), 0.88),
    (0.35, (0.43, 0.265, 265.0), 0.88),
    (0.65, (0.36, 0.232, 266.0), 0.90),
    (0.85, (0.30, 0.195, 267.0), 0.92),
    (1.00, (0.26, 0.165, 268.0), 0.94),
];
/// where the body is lightest, in its layer's unit space with y up
pub const BODY_LIGHT: (f64, f64) = (0.36, 0.68);

/// how far a dome's gradient reaches from its light, as a share of its square's side. it
/// reaches the far side of the circle in that square. the Wayland shader calculates it itself
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn dome_reach(light: (f64, f64)) -> f64 {
    0.5 + (light.0 - 0.5).hypot(light.1 - 0.5)
}

/// linear sRGB for an oklch color, by Björn Ottosson's matrices. out of sRGB's gamut a
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

/// the transfer curve that sRGB and Display P3 share, from linear light to a stored value
fn encode(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// the color in a gamut whose primaries `to_gamut` maps linear sRGB onto. a color that the
/// gamut cannot hold gives up chroma until it can, which keeps its lightness and hue.
/// clamping the channels instead shifts both
fn in_gamut((lightness, chroma, hue): Oklch, to_gamut: fn([f64; 3]) -> [f64; 3]) -> [f64; 3] {
    let linear = |chroma| to_gamut(oklch_to_linear_srgb((lightness, chroma, hue)));
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

/// Display P3 for an oklch color
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn display_p3(oklch: Oklch) -> [f64; 3] {
    in_gamut(oklch, linear_srgb_to_p3)
}

/// sRGB for an oklch color, for displays that take sRGB
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn srgb(oklch: Oklch) -> [f64; 3] {
    in_gamut(oklch, |rgb| rgb)
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
    fn oklch_lands_on_known_srgb_colours() {
        assert!(close(srgb((0.627_96, 0.257_68, 29.234)), [1.0, 0.0, 0.0]));
        assert!(close(srgb((0.452_01, 0.313_21, 264.052)), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn a_colour_the_display_cannot_show_loses_chroma_not_hue() {
        // far outside any display
        let vivid = (0.7, 0.4, 145.0);
        for clipped in [display_p3(vivid), srgb(vivid)] {
            assert!(clipped.iter().all(|c| (0.0..=1.0).contains(c)));
            // still a green, and one channel is at the gamut's edge
            assert!(clipped[1] > clipped[0] && clipped[1] > clipped[2]);
            assert!(clipped.iter().any(|&c| !(1e-3..=1.0 - 1e-3).contains(&c)));
        }
        // one that it can show comes through untouched
        let (l, c, h) = BODY[0].1;
        let straight = linear_srgb_to_p3(oklch_to_linear_srgb((l, c, h))).map(encode);
        assert!(close(display_p3((l, c, h)), straight));
    }

    #[test]
    fn every_stop_is_in_order_and_inside_display_p3() {
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
    fn a_dome_reaches_the_far_side_of_its_circle() {
        assert_eq!(dome_reach((0.5, 0.5)), 0.5);
        assert!((dome_reach(BODY_LIGHT) - (0.5 + 0.14f64.hypot(0.18))).abs() < 1e-12);
    }
}
