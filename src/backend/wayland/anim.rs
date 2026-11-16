//! a small stand-in for Core Animation, so the warning and the break screen move as they do on
//! macOS. a property keeps its own value and the animations laid over it, and gives the value
//! on screen at any moment. times are seconds on `Clock`.

use std::time::Instant;

use crate::mati::motion::Spring;

/// seconds since omma started, as Core Animation's media time counts
#[derive(Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
    pub fn new() -> Clock {
        Clock(Instant::now())
    }

    pub fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

/// how an animation moves between its ends
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Linear,
    /// a css cubic-bezier, from (0, 0) to (1, 1)
    Bezier(f64, f64, f64, f64),
}

/// Core Animation's ease-in-ease-out
pub const EASE: Curve = Curve::Bezier(0.42, 0.0, 0.58, 1.0);
/// for things that arrive: fast at the start, then a long, gentle settle
pub const ARRIVE: Curve = Curve::Bezier(0.32, 0.72, 0.0, 1.0);
/// slow at the start, fastest midway and slow into the end, for a move between two held
/// states such as a blink's open and shut
pub const TRAVEL: Curve = Curve::Bezier(0.65, 0.0, 0.35, 1.0);
/// gathers speed to the end, as a falling leaf does
pub const GATHER: Curve = Curve::Bezier(0.5, 0.0, 1.0, 1.0);

impl Curve {
    /// the eased progress at `x` (0…1) of the way through
    pub fn at(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let Curve::Bezier(x1, y1, x2, y2) = self else {
            return x;
        };
        let bezier = |s: f64, a: f64, b: f64| {
            let r = 1.0 - s;
            3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
        };
        // solves bezier(s, x1, x2) = x for s, by bisection, which always converges because x
        // rises with s for every curve here
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..40 {
            let mid = (low + high) / 2.0;
            if bezier(mid, x1, x2) < x {
                low = mid;
            } else {
                high = mid;
            }
        }
        bezier((low + high) / 2.0, y1, y2)
    }
}

/// what an animation leaves behind
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// shows the start value from now until it begins, so entrances can be staggered, then
    /// hands back to the property's own value
    Entrance,
    /// keeps the end value until the property goes away
    Exit,
}

#[derive(Clone, Debug)]
enum Motion {
    Tween(Curve),
    Spring(Spring),
    /// values at times (0…1 of the duration), each segment on its own curve
    Keys {
        values: Vec<f64>,
        times: Vec<f64>,
        curves: Vec<Curve>,
    },
}

#[derive(Clone, Debug)]
pub struct Anim {
    from: f64,
    to: f64,
    begin: f64,
    duration: f64,
    motion: Motion,
    fill: Fill,
    key: Option<&'static str>,
}

impl Anim {
    pub fn tween(from: f64, to: f64, begin: f64, duration: f64, curve: Curve, fill: Fill) -> Anim {
        Anim {
            from,
            to,
            begin,
            duration: duration.max(1e-6),
            motion: Motion::Tween(curve),
            fill,
            key: None,
        }
    }

    /// on `spring`, for as long as it takes to settle
    pub fn spring(from: f64, to: f64, begin: f64, spring: Spring, fill: Fill) -> Anim {
        Anim {
            from,
            to,
            begin,
            duration: settling(spring),
            motion: Motion::Spring(spring),
            fill,
            key: None,
        }
    }

    /// through `values` at `times`, with `curves[i]` from value i to i + 1
    pub fn keys(
        values: &[f64],
        times: &[f64],
        curves: &[Curve],
        begin: f64,
        duration: f64,
        fill: Fill,
    ) -> Anim {
        Anim {
            from: values[0],
            to: values[values.len() - 1],
            begin,
            duration: duration.max(1e-6),
            motion: Motion::Keys {
                values: values.to_vec(),
                times: times.to_vec(),
                curves: curves.to_vec(),
            },
            fill,
            key: None,
        }
    }

    /// replaces any animation with the same key on the property that gets this one
    pub fn keyed(mut self, key: &'static str) -> Anim {
        self.key = Some(key);
        self
    }

    pub fn end(&self) -> f64 {
        self.begin + self.duration
    }

    fn value(&self, t: f64) -> f64 {
        let x = ((t - self.begin) / self.duration).clamp(0.0, 1.0);
        match &self.motion {
            Motion::Tween(curve) => self.from + (self.to - self.from) * curve.at(x),
            Motion::Spring(spring) => {
                if x >= 1.0 {
                    return self.to;
                }
                self.to + (self.from - self.to) * displacement(*spring, t - self.begin)
            }
            Motion::Keys {
                values,
                times,
                curves,
            } => {
                let i = times
                    .windows(2)
                    .position(|pair| x <= pair[1])
                    .unwrap_or(times.len() - 2);
                let span = (times[i + 1] - times[i]).max(1e-9);
                let local = ((x - times[i]) / span).clamp(0.0, 1.0);
                let eased = curves.get(i).copied().unwrap_or(Curve::Linear).at(local);
                values[i] + (values[i + 1] - values[i]) * eased
            }
        }
    }

    /// the value it gives at `t`, or None when it no longer applies
    fn sample(&self, t: f64) -> Option<f64> {
        if t < self.begin {
            return Some(self.from);
        }
        if t >= self.end() {
            return match self.fill {
                Fill::Exit => Some(self.to),
                Fill::Entrance => None,
            };
        }
        Some(self.value(t))
    }
}

/// the share of a spring's starting displacement left `t` seconds in, with unit mass and no
/// starting speed
fn displacement(spring: Spring, t: f64) -> f64 {
    let w0 = spring.stiffness.sqrt();
    let zeta = spring.damping / (2.0 * w0);
    if zeta < 1.0 {
        let wd = w0 * (1.0 - zeta * zeta).sqrt();
        (-zeta * w0 * t).exp() * ((wd * t).cos() + zeta * w0 / wd * (wd * t).sin())
    } else if zeta == 1.0 {
        (-w0 * t).exp() * (1.0 + w0 * t)
    } else {
        let root = (zeta * zeta - 1.0).sqrt();
        let (r1, r2) = (-w0 * (zeta - root), -w0 * (zeta + root));
        (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
    }
}

/// how long a spring takes to come within a thousandth of its goal, at most 1.2 s as the
/// macOS backend caps it
fn settling(spring: Spring) -> f64 {
    let w0 = spring.stiffness.sqrt();
    let zeta = (spring.damping / (2.0 * w0)).min(0.999);
    let decay = zeta * w0;
    let wd = w0 * (1.0 - zeta * zeta).sqrt();
    let envelope = (1.0 + (decay / wd).powi(2)).sqrt();
    ((envelope / 1e-3).ln() / decay).min(1.2)
}

/// a number on screen: its own value, and the animations over it, the latest on top
#[derive(Clone, Debug, Default)]
pub struct Prop {
    pub model: f64,
    anims: Vec<Anim>,
}

impl Prop {
    pub fn new(model: f64) -> Prop {
        Prop {
            model,
            anims: Vec::new(),
        }
    }

    pub fn add(&mut self, anim: Anim) {
        if let Some(key) = anim.key {
            self.anims.retain(|other| other.key != Some(key));
        }
        self.anims.push(anim);
    }

    /// drops every animation and holds `value`
    pub fn set(&mut self, value: f64) {
        self.model = value;
        self.anims.clear();
    }

    /// the value on screen at `t`
    pub fn at(&self, t: f64) -> f64 {
        self.anims
            .iter()
            .fold(self.model, |value, anim| anim.sample(t).unwrap_or(value))
    }

    pub fn moving(&self, t: f64) -> bool {
        self.anims.iter().any(|anim| t < anim.end())
    }

    /// forgets entrances that are over, which no longer change anything
    pub fn prune(&mut self, t: f64) {
        self.anims
            .retain(|anim| anim.fill == Fill::Exit || t < anim.end());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mati::motion::{FLIGHT, POP, SETTLE};

    #[test]
    fn curves_start_and_end_on_their_ends() {
        for curve in [Curve::Linear, EASE, ARRIVE, TRAVEL, GATHER] {
            assert!(curve.at(0.0).abs() < 1e-9);
            assert!((curve.at(1.0) - 1.0).abs() < 1e-9);
            assert!(curve.at(0.25) < curve.at(0.75));
        }
        // ease-in-ease-out is symmetric about the middle
        assert!((EASE.at(0.5) - 0.5).abs() < 1e-6);
        // arrive covers most of the way early
        assert!(ARRIVE.at(0.3) > 0.6);
    }

    #[test]
    fn an_entrance_shows_its_start_early_and_hands_back_after() {
        let mut prop = Prop::new(1.0);
        prop.add(Anim::tween(
            0.0,
            1.0,
            2.0,
            1.0,
            Curve::Linear,
            Fill::Entrance,
        ));
        assert_eq!(prop.at(1.0), 0.0);
        assert!((prop.at(2.5) - 0.5).abs() < 1e-9);
        assert_eq!(prop.at(3.5), 1.0);
        prop.model = 0.7;
        assert_eq!(prop.at(3.5), 0.7);
        assert!(prop.moving(2.9) && !prop.moving(3.0));
        prop.prune(3.0);
        assert!(prop.anims.is_empty());
    }

    #[test]
    fn an_exit_holds_its_end_and_a_key_replaces_its_namesake() {
        let mut prop = Prop::new(1.0);
        prop.add(Anim::tween(1.0, 0.0, 0.0, 1.0, EASE, Fill::Exit).keyed("opacity"));
        assert_eq!(prop.at(5.0), 0.0);
        prop.prune(5.0);
        assert_eq!(prop.at(5.0), 0.0);
        prop.add(Anim::tween(0.0, 0.5, 5.0, 1.0, EASE, Fill::Exit).keyed("opacity"));
        assert_eq!(prop.at(9.0), 0.5);
        assert_eq!(prop.anims.len(), 1);
    }

    #[test]
    fn springs_overshoot_a_little_and_settle_on_their_goal() {
        for spring in [SETTLE, POP, FLIGHT] {
            let anim = Anim::spring(0.0, 1.0, 0.0, spring, Fill::Entrance);
            let peak = (0..200)
                .map(|i| anim.value(i as f64 / 200.0 * anim.duration))
                .fold(f64::MIN, f64::max);
            assert!(peak > 1.0 && peak < 1.25, "{peak}");
            assert_eq!(anim.value(anim.end()), 1.0);
            assert!((anim.value(anim.end() - 1e-6) - 1.0).abs() < 2e-3);
            assert!(anim.duration > 0.3 && anim.duration <= 1.2);
        }
    }

    #[test]
    fn keyframes_follow_their_times_and_curves() {
        let anim = Anim::keys(
            &[0.0, 7.0, 0.0],
            &[0.0, 0.3, 1.0],
            &[Curve::Linear, Curve::Linear],
            0.0,
            1.0,
            Fill::Entrance,
        );
        assert!(anim.value(0.0).abs() < 1e-9);
        assert!((anim.value(0.3) - 7.0).abs() < 1e-9);
        assert!((anim.value(0.15) - 3.5).abs() < 1e-9);
        assert!((anim.value(0.65) - 3.5).abs() < 1e-9);
        assert!(anim.value(1.0).abs() < 1e-9);
    }
}
