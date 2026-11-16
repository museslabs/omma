//! the bead's shape: the liquid outline that drifts during a break, the flat top against the
//! menu bar, and the liquid level that drains. also the eye's movements: the blinks and the
//! annoyed lid

use std::f64::consts::{PI, TAU};

/// a blink squashes the pupil flat like a closing lid, a little wider as it goes: its
/// scale across and up when shut
pub const BLINK_SHUT: (f64, f64) = (1.12, 0.10);
/// it springs open a touch too tall and narrow, then settles
pub const BLINK_BOUNCE: (f64, f64) = (0.96, 1.08);
/// closing is quick, opening a little slower, and the settle slowest
pub const BLINK_CLOSE: f64 = 0.09;
pub const BLINK_OPEN: f64 = 0.14;
pub const BLINK_SETTLE: f64 = 0.16;
/// the blinks come round again after this long. each starts this many seconds in. the gaps
/// between them are uneven, and the last two form a double, so the round does not read as a
/// beat
pub const BLINK_PERIOD: f64 = 9.0;
pub const BLINKS: [f64; 3] = [2.4, 5.9, 6.34];
/// when the bead is annoyed, an upper lid comes down over this share of the pupil, at its
/// middle. the lid slopes by this many radians, so it reads as a frown and never as a blink.
/// when the lid is open, its edge waits this far above the pupil, as a share of the pupil
pub const LID_DROP: f64 = 0.45;
pub const LID_TILT: f64 = 0.2;
pub const LID_OPEN: f64 = 1.1;
/// when something above the bead presses on it, the bead's top gives way to a straight edge.
/// the edge curves into the rest of the outline over this share of the radius. while the
/// press is shallower than that, it curves over the press itself, so the top still reaches
/// the edge
pub const FLAT_ROUNDING: f64 = 0.25;
/// the top flattens no further than this share of the radius. past that, the rings reach its
/// edge
pub const FLAT_MOST: f64 = 0.4;
/// as the top flattens, the rings sink by this much of the press, and the pupil sinks by more.
/// the press seems to push them down through the liquid, and they stay clear of the edge
pub const IRIS_SINK: f64 = 0.5;
pub const PUPIL_SINK: f64 = 0.6;

/// the body's radius at rest, as a share of the bead's
pub const LIQUID: f64 = 0.92;
/// how far the body's outline strays from that as it drifts, each way
pub const MORPH: f64 = 0.058;
/// the shell leaves room around the pigment, and moves less so that gap stays open
pub const GLASS: f64 = 0.965;
pub const GLASS_MORPH: f64 = 0.5;
/// how long the drifting outline takes to come round to the same shape
pub const MORPH_PERIOD: f64 = 11.0;
/// the shapes in one round, and the points on each outline, where an outline is drawn as
/// points
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const MORPH_FRAMES: usize = 44;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const OUTLINE_POINTS: usize = 64;
/// the outline swells from a circle over this share of its first round
const MORPH_SWELL: f64 = 0.22;
/// the count's digits, as a share of the diameter
pub const COUNT_SIZE: f64 = 0.12;
/// how long the liquid takes to reach the level the next second puts it at. a little over
/// the second, so it keeps flowing until a late tick gives it the next level
pub const FLOW: f64 = 1.1;
/// the wave on the liquid's surface, as shares of the diameter, and how long one wave takes
/// to pass
pub const WAVE_HEIGHT: f64 = 0.011;
pub const WAVE_LENGTH: f64 = 0.8;
pub const WAVE_PERIOD: f64 = 2.8;
/// the wave layers are this many diameters wide, too wide for a passing wave to show an end
pub const WAVE_SPAN: f64 = 4.0;

/// how far the outline is from the bead's center at `angle`, as a share of the radius,
/// `phase` (0…τ) of the way round its drift. two slow lobes and three quicker ones turn
/// against each other and swell in turn. this keeps a shape from coming back before the
/// round is over. `swell` (0…1) is how far it grew from a circle
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn outline_radius(angle: f64, phase: f64, swell: f64) -> f64 {
    let two = (0.50 + 0.12 * (2.0 * phase + 0.7).sin()) * (2.0 * angle + phase).sin();
    let three = (0.38 + 0.10 * phase.cos()) * (3.0 * angle - phase + 1.3).sin();
    LIQUID + MORPH * swell * (two + three)
}

/// how far the drift swells from a circle, `along` (0…1) its first round
pub fn swell_at(along: f64) -> f64 {
    let t = (along / MORPH_SWELL).min(1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub enum Outline {
    Pigment,
    Glass,
    /// the bead's own edge, where the glass ends
    Rim,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Outline {
    pub fn radius(self, angle: f64, phase: f64, swell: f64) -> f64 {
        let pigment = outline_radius(angle, phase, swell);
        match self {
            Self::Pigment => pigment,
            Self::Glass => GLASS + GLASS_MORPH * (pigment - LIQUID),
            Self::Rim => 1.0,
        }
    }

    /// how far the top is from the center at rest, as a share of the radius
    pub fn top(self) -> f64 {
        self.radius(PI / 2.0, 0.0, 0.0)
    }
}

/// the lower of `a` and `b`, rounded off where they come within `k` (above 0) of each other
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn soft_min(a: f64, b: f64, k: f64) -> f64 {
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k / 4.0
}

/// point `i` of the outline's `OUTLINE_POINTS`, counted round from the right, for a bead of
/// diameter `d`, with y up from its bottom. a `flat` above 0 (a share of the radius) cuts the
/// top off straight that far below where it is at rest. it also rounds the cut into the curve
/// on either side
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn outline_point(
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

/// the furthest the body's outline gets from the center, as a share of the bead's radius
fn liquid_reach() -> f64 {
    LIQUID + MORPH * 1.1
}

/// where the wave's crests sit for a fill `height` (0…1 of the liquid's height), measured
/// up from the bead's bottom. when full, even the troughs clear the liquid's top wherever its
/// outline drifts. when empty, the crests are under its bottom
pub fn level_y(diameter: f64, height: f64) -> f64 {
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

/// the level that leaves `left` (0…1) of the disc's area colored. the color then drains at
/// an even rate though the disc is wider in the middle
pub fn height_for(left: f64) -> f64 {
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
    use super::super::colour::{DISCS, Disc};
    use super::*;

    #[test]
    fn the_level_leaves_the_asked_share_of_the_area() {
        assert_eq!(height_for(0.0), 0.0);
        assert_eq!(height_for(1.0), 1.0);
        assert!((height_for(0.5) - 0.5).abs() < 1e-6);
        for left in [0.05, 0.25, 0.6, 0.9] {
            assert!((area_below(height_for(left)) - left).abs() < 1e-6);
        }
        // a sliver of color needs more than a sliver of height, because the bottom is narrow
        assert!(height_for(0.1) > 0.1);
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
                // unpressed, it is the circle
                assert_eq!(at(i, shape, 0.0), round);
                // pressed, nothing reaches above the cut or moves sideways, and the lower
                // half stays as it was
                let pressed = at(i, shape, FLAT_MOST);
                assert!(pressed.1 <= cut + 1e-9);
                assert_eq!(pressed.0, round.0);
                if round.1 <= r {
                    assert_eq!(pressed, round);
                }
                // over the middle third, it is a straight line along the cut
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
        // the drift does rise above the resting top, so a cut there shows
        assert!(highest > 0.0);
    }

    #[test]
    fn the_eye_sinks_clear_of_a_flattened_top() {
        // shares of the radius above the center, at the flattest press. the rings and pupil
        // sink in step with the press, so if they are clear there, they are clear all the way
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
        // and at rest it is a circle
        assert_eq!(outline_radius(1.0, 2.0, 0.0), LIQUID);
    }

    #[test]
    fn the_drift_swells_smoothly_and_then_holds() {
        assert_eq!(swell_at(0.0), 0.0);
        assert_eq!(swell_at(MORPH_SWELL), 1.0);
        assert_eq!(swell_at(1.0), 1.0);
        assert!((swell_at(MORPH_SWELL / 2.0) - 0.5).abs() < 1e-12);
    }
}
