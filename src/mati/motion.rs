//! springs, and the swing of the warning's bead on its thread. the swing is a pendulum. when
//! the user grabs the bead, it follows the pointer. when the user lets go, the swing keeps the
//! release velocity. the menu bar above it is a wall that flattens its top and annoys it

/// a spring of unit mass. each of these overshoots a little, a quarter of the way to ringing
/// at most, and settles
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
}

/// text and controls easing into place
pub const SETTLE: Spring = Spring {
    stiffness: 180.0,
    damping: 22.0,
};
/// a surface popping open
pub const POP: Spring = Spring {
    stiffness: 300.0,
    damping: 26.0,
};
/// the bead's flight across the display
pub const FLIGHT: Spring = Spring {
    stiffness: 150.0,
    damping: 20.0,
};

/// ω² of the swing, per second squared
pub const STIFFNESS: f64 = 42.0;
/// light while it hangs in the warning, so a swing rings on. heavy as it settles while the
/// warning goes out
pub const DAMPING_FREE: f64 = 1.7;
pub const DAMPING_SETTLE: f64 = 14.0;
/// the swing it drops in with, in radians per second: about 18° to one side
pub const DROP_KICK: f64 = 2.0;
/// after it hangs still this long, it stirs again with a smaller swing, to the other side
/// each time
pub const STIR_AFTER: f64 = 3.5;
pub const STIR_KICK: f64 = 1.1;
/// below this angle and speed it counts as hanging still
pub const STILL_ANGLE: f64 = 0.035;
pub const STILL_SPEED: f64 = 0.2;
/// the charm swells in from this scale like a bubble, thread and bead as one, and shrinks back
/// to it. the bead's color blurs this much as it does
pub const BUBBLE_FROM: f64 = 0.95;
pub const BUBBLE_BLUR: f64 = 8.0;
/// how much of a grab around the bead counts, beyond its edge
pub const GRAB_SLOP: f64 = 6.0;
/// the look's springs, critically damped
pub const LOOK: Spring = Spring {
    stiffness: 190.0,
    damping: 27.0,
};
/// the pointer this far from the bead, or further, makes it look all the way over
pub const LOOK_REACH: f64 = 140.0;
/// how hard the menu bar pushes back on a bead swung into it, per radian past where they
/// meet. the damping is how much of the swing the bar soaks up. the bead drags against the
/// bar and comes off at well under half the speed it hit
pub const WALL_STIFFNESS: f64 = 250.0;
pub const WALL_DAMPING: f64 = 20.0;
/// a drag presses the bead into the bar less and less far, up to this many radians
const PRESS_LIMIT: f64 = 0.18;
/// the top flattens against the bar, and the annoyed lid comes down, on a brisk spring that
/// barely overshoots
pub const BRISK: Spring = Spring {
    stiffness: 900.0,
    damping: 51.0,
};
/// the top rounds out again slower, as a drop of liquid does, and never past round
pub const ROUNDING: Spring = Spring {
    stiffness: 300.0,
    damping: 35.0,
};
/// when the bar presses the bead in this many points, the bead gets annoyed, and stays
/// annoyed this long after
pub const ANNOY_AT: f64 = 3.0;
pub const ANNOY_HOLD: f64 = 0.6;
/// the lid lifts slower than it came down, with a little bounce
pub const RAISING: Spring = Spring {
    stiffness: 300.0,
    damping: 22.0,
};

/// a drag to `angle`, with the bar at `wall`. past the bar, each further radian presses the
/// bead in less, toward `PRESS_LIMIT`
pub fn press(angle: f64, wall: f64) -> f64 {
    let over = angle.abs() - wall;
    if over <= 0.0 {
        return angle;
    }
    angle.signum() * (wall + PRESS_LIMIT * over / (PRESS_LIMIT + over))
}

/// how many points a bead of `radius` on a swing `length` long, at `theta`, reaches past the
/// pivot's height. the swing never nears half a turn, where this stops holding
pub fn pressed(theta: f64, radius: f64, length: f64) -> f64 {
    (radius - length * theta.cos()).max(0.0)
}

/// moves `pos`, which goes at `vel`, `dt` seconds on toward `goal` on a spring of unit mass.
/// it steps a 240th of a second at a time at most. with one long step after each dropped
/// frame, a spring as stiff as `BRISK` overshoots more each step instead of settling. once it
/// is within a thousandth of `goal` and nearly still, it stops there exactly. then whatever it
/// drives can tell that it stopped changing
pub fn chase(pos: &mut f64, vel: &mut f64, goal: f64, spring: Spring, dt: f64) {
    let steps = (dt * 240.0).ceil().max(1.0);
    let step = dt / steps;
    for _ in 0..steps as usize {
        *vel += (spring.stiffness * (goal - *pos) - spring.damping * *vel) * step;
        *pos += *vel * step;
    }
    if (goal - *pos).abs() < 1e-3 && vel.abs() < 1e-2 {
        *pos = goal;
        *vel = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_presses_into_the_bar_less_the_further_it_goes() {
        let wall = 1.1;
        assert_eq!(press(0.5, wall), 0.5);
        assert_eq!(press(-wall, wall), -wall);
        let a = press(wall + 0.5, wall) - wall;
        let b = press(wall + 1.0, wall) - press(wall + 0.5, wall);
        assert!(a > 0.0 && b > 0.0 && b < a);
        assert!(press(wall + 100.0, wall) < wall + PRESS_LIMIT);
        assert_eq!(press(-2.0, wall), -press(2.0, wall));
    }

    #[test]
    fn the_bead_reaches_into_the_bar_only_past_the_wall() {
        let (radius, length): (f64, f64) = (32.0, 74.0);
        let wall = (radius / length).acos();
        assert_eq!(pressed(0.0, radius, length), 0.0);
        assert_eq!(pressed(wall - 0.01, radius, length), 0.0);
        assert!(pressed(wall, radius, length) < 1e-9);
        let near = pressed(wall + 0.05, radius, length);
        let hardest = pressed(-(wall + PRESS_LIMIT), radius, length);
        assert!(near > 0.0 && hardest > near);
        // the hardest a drag can press annoys the bead, and flattens most of its top
        assert!(hardest > ANNOY_AT && hardest > radius / 3.0);
    }

    #[test]
    fn every_spring_comes_to_rest_on_its_goal_even_at_the_longest_frame() {
        for spring in [LOOK, BRISK, ROUNDING, RAISING] {
            let (mut pos, mut vel) = (0.0, 0.0);
            // a second of frames, each as long as a frame can be
            for _ in 0..32 {
                chase(&mut pos, &mut vel, 1.0, spring, 0.032);
            }
            assert_eq!((pos, vel), (1.0, 0.0));
        }
    }

    #[test]
    fn the_top_rounds_out_without_dipping_past_round() {
        let (mut depth, mut speed) = (12.0, 0.0);
        for _ in 0..120 {
            chase(&mut depth, &mut speed, 0.0, ROUNDING, 1.0 / 120.0);
            assert!(depth >= 0.0);
        }
        assert_eq!(depth, 0.0);
    }
}
