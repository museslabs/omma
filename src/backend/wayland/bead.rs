//! the mati bead on Wayland: the state the macOS backend keeps in Core Animation layers,
//! evaluated each frame and drawn by `shaders/bead.frag`. there is no Liquid Glass to refract
//! the pigment, so the shader paints a clear shell with a rim and a highlight over it.

use std::f64::consts::TAU;

use super::anim::{ARRIVE, Anim, Curve, Fill, Prop, TRAVEL};
use super::flip::FlipCount;
use super::gl::{Affine, Gl, Rect, Rgba};
use super::text::Fonts;
use crate::mati::colour::{BODY, BODY_LIGHT, DISCS, PUPIL, SKY, Stop, WHITE, srgb};
use crate::mati::form::{
    BLINK_BOUNCE, BLINK_CLOSE, BLINK_OPEN, BLINK_PERIOD, BLINK_SETTLE, BLINK_SHUT, BLINKS,
    COUNT_SIZE, FLAT_ROUNDING, FLOW, GLASS, GLASS_MORPH, IRIS_SINK, LID_DROP, LID_OPEN, LID_TILT,
    LIQUID, MORPH, MORPH_PERIOD, PUPIL_SINK, WAVE_HEIGHT, WAVE_LENGTH, WAVE_PERIOD, WAVE_SPAN,
    height_for, level_y, swell_at,
};

/// the shadow under the glass, as the macOS layer casts it. the drop and the softness are
/// shares of the diameter
const SHADOW_DROP: f64 = 0.025;
const SHADOW_SOFTNESS: f64 = 0.0175;
const SHADOW_OPACITY: f64 = 0.20;
/// the blue that the glass casts around itself, in place of the light that Liquid Glass bends.
/// the reach is a share of the diameter, and the opacity is how strong it starts
const GLOW_REACH: f64 = 0.035;
const GLOW_OPACITY: f32 = 0.32;
/// the glass's bright rim, in logical pixels
const RIM_WIDTH: f64 = 1.5;
/// room around the bead for its shadow and glow, as a share of the diameter
const PAD: f32 = 0.16;

/// the parts of the bead, as bits of the shader's `u_parts`
const SHADOW: i32 = 1;
const DISC: i32 = 2;
const PIGMENT: i32 = 4;
const GLASS_PART: i32 = 8;
const EYE: i32 = 16;

/// the shader's numbers, from the shared ones, so that the two cannot drift apart
pub fn shader_defines() -> String {
    let [white, sky, pupil] = &DISCS;
    let defines = [
        ("LIQUID", LIQUID),
        ("MORPH", MORPH),
        ("GLASS", GLASS),
        ("GLASS_MORPH", GLASS_MORPH),
        ("FLAT_ROUNDING", FLAT_ROUNDING),
        ("IRIS_SINK", IRIS_SINK),
        ("PUPIL_SINK", PUPIL_SINK),
        ("WAVE_HEIGHT", WAVE_HEIGHT),
        ("WAVE_LENGTH", WAVE_LENGTH),
        ("WAVE_SPAN", WAVE_SPAN),
        ("LID_OPEN", LID_OPEN),
        ("LID_DROP", LID_DROP),
        ("LID_TILT", LID_TILT),
        ("WHITE_RADIUS", white.radius),
        ("SKY_RADIUS", sky.radius),
        ("PUPIL_RADIUS", pupil.radius),
        ("SHADOW_DROP", SHADOW_DROP),
        ("SHADOW_SOFTNESS", SHADOW_SOFTNESS),
        ("SHADOW_OPACITY", SHADOW_OPACITY),
        ("GLOW_REACH", GLOW_REACH),
        ("RIM_WIDTH", RIM_WIDTH),
    ];
    let mut text: String = defines
        .iter()
        .map(|(name, value)| format!("#define {name} {value:.6}\n"))
        .collect();
    for (name, (x, y)) in [
        ("BODY_LIGHT", BODY_LIGHT),
        ("WHITE_LIGHT", white.light),
        ("SKY_LIGHT", sky.light),
    ] {
        text.push_str(&format!("#define {name} vec2({x:.6}, {y:.6})\n"));
    }
    text
}

/// a gradient's colors, straight sRGB and alpha, and where each stop sits
fn stops(stops: &[Stop; 5]) -> ([f32; 20], [f32; 5]) {
    let mut colours = [0.0; 20];
    let mut at = [0.0; 5];
    for (i, &(position, oklch, alpha)) in stops.iter().enumerate() {
        let [r, g, b] = srgb(oklch);
        colours[i * 4..i * 4 + 4].copy_from_slice(&[r as f32, g as f32, b as f32, alpha as f32]);
        at[i] = position as f32;
    }
    (colours, at)
}

pub struct Bead {
    pub diameter: f32,
    counts: bool,
    solid: bool,
    /// its square's top left, where it rests, in its surface's logical pixels
    pub origin: (f32, f32),
    /// the move it makes as a whole: a scale about its center, then a shift
    pub scale: Prop,
    pub shift_x: Prop,
    pub shift_y: Prop,
    pub opacity: Prop,
    /// the blur on its pigment alone, in logical pixels
    pub blur: Prop,
    count_opacity: Prop,
    /// how far each disc leans toward the pointer, x then y, in logical pixels with y up
    look: [[Prop; 2]; 3],
    /// where the liquid's wave crests sit, up from the bottom, in logical pixels
    level: Prop,
    /// how far the top is pressed flat, as a share of the radius
    flat: f64,
    /// how annoyed it is (0…1), and which way the lid slopes
    annoyed: (f64, f64),
    blink_from: Option<f64>,
    drift_from: Option<f64>,
    flow_from: Option<f64>,
    hide_at: Option<f64>,
    blink: [Prop; 2],
    count: Option<FlipCount>,
    body: ([f32; 20], [f32; 5]),
    white: ([f32; 20], [f32; 5]),
    sky: ([f32; 20], [f32; 5]),
}

impl Bead {
    /// `diameter` logical pixels across. with `counts`, its pupil shows a number that
    /// `set_count` changes, its liquid can drain and its outline can drift. `solid` puts a
    /// flat disc in place of the glass, for reduced transparency or increased contrast
    pub fn new(diameter: f32, counts: bool, solid: bool) -> Bead {
        let d = f64::from(diameter);
        let opaque = |stops: &[Stop; 5]| -> [Stop; 5] {
            stops.map(|(at, oklch, alpha)| (at, oklch, if solid { 1.0 } else { alpha }))
        };
        Bead {
            diameter,
            counts,
            solid,
            origin: (0.0, 0.0),
            scale: Prop::new(1.0),
            shift_x: Prop::new(0.0),
            shift_y: Prop::new(0.0),
            opacity: Prop::new(1.0),
            blur: Prop::new(0.0),
            count_opacity: Prop::new(1.0),
            look: Default::default(),
            level: Prop::new(level_y(d, 1.0)),
            flat: 0.0,
            annoyed: (0.0, 1.0),
            blink_from: None,
            drift_from: None,
            flow_from: None,
            hide_at: None,
            blink: [Prop::new(1.0), Prop::new(1.0)],
            count: counts.then(|| FlipCount::new(diameter * COUNT_SIZE as f32, PUPIL)),
            body: stops(&opaque(&BODY)),
            white: stops(&opaque(&WHITE)),
            sky: stops(&opaque(&SKY)),
        }
    }

    /// its square where it rests
    pub fn frame(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, self.diameter, self.diameter)
    }

    pub fn centre(&self) -> (f32, f32) {
        self.frame().center()
    }

    /// the scale and the shifts that make it look like it fills `rect`
    pub fn toward(&self, rect: Rect) -> (f64, f64, f64) {
        let (cx, cy) = self.centre();
        let (tx, ty) = rect.center();
        (
            f64::from(rect.w / self.diameter),
            f64::from(tx - cx),
            f64::from(ty - cy),
        )
    }

    /// its whole move at `t`
    pub fn transform(&self, t: f64) -> Affine {
        let (cx, cy) = self.centre();
        let s = self.scale.at(t) as f32;
        Affine::scale_about(s, s, cx, cy).then(Affine::translate(
            self.shift_x.at(t) as f32,
            self.shift_y.at(t) as f32,
        ))
    }

    /// sets the outline drifting from `begin`, for as long as it lives
    pub fn drift(&mut self, begin: f64) {
        self.drift_from = Some(begin);
    }

    /// sets the pupil blinking from `now`, for as long as it lives
    pub fn blink(&mut self, now: f64) {
        self.blink_from = Some(now);
        for (prop, (shut, bounce)) in self.blink.iter_mut().zip([
            (BLINK_SHUT.0, BLINK_BOUNCE.0),
            (BLINK_SHUT.1, BLINK_BOUNCE.1),
        ]) {
            let mut values = vec![1.0];
            let mut times = vec![0.0];
            let mut curves = Vec::new();
            for start in BLINKS {
                let shut_at = start + BLINK_CLOSE;
                let open_at = shut_at + BLINK_OPEN;
                for (at, value, curve) in [
                    (start, 1.0, Curve::Linear),
                    (shut_at, shut, TRAVEL),
                    (open_at, bounce, ARRIVE),
                    (open_at + BLINK_SETTLE, 1.0, TRAVEL),
                ] {
                    values.push(value);
                    times.push(at / BLINK_PERIOD);
                    curves.push(curve);
                }
            }
            values.push(1.0);
            times.push(1.0);
            curves.push(Curve::Linear);
            prop.set(1.0);
            prop.add(Anim::keys(
                &values,
                &times,
                &curves,
                0.0,
                BLINK_PERIOD,
                Fill::Entrance,
            ));
        }
    }

    /// sets the wave on the liquid's surface going from `now`
    pub fn flow(&mut self, now: f64) {
        self.flow_from = Some(now);
    }

    /// hides it from `at` on, for good
    pub fn hide_at(&mut self, at: f64) {
        self.hide_at = Some(at);
    }

    /// leans the discs toward a point `x`, `y` in -1…1, with y up. the pupil moves most
    pub fn look(&mut self, x: f64, y: f64) {
        let d = f64::from(self.diameter);
        for (props, disc) in self.look.iter_mut().zip(&DISCS) {
            props[0].set(x * d * disc.depth.0);
            props[1].set(y * d * disc.depth.1);
        }
    }

    /// eases the discs back to the middle from where `look(x, y)` puts them, over `duration`
    /// from `begin`. until then they stay where that look left them
    pub fn look_away_from(&mut self, x: f64, y: f64, begin: f64, duration: f64) {
        let d = f64::from(self.diameter);
        for (props, disc) in self.look.iter_mut().zip(&DISCS) {
            for (prop, lean) in props
                .iter_mut()
                .zip([x * d * disc.depth.0, y * d * disc.depth.1])
            {
                prop.set(0.0);
                prop.add(Anim::tween(
                    lean,
                    0.0,
                    begin,
                    duration,
                    ARRIVE,
                    Fill::Entrance,
                ));
            }
        }
    }

    /// presses the top flat, `depth` logical pixels down from where it rests
    pub fn press(&mut self, depth: f64) {
        self.flat = depth.max(0.0) / (f64::from(self.diameter) / 2.0);
    }

    /// lowers the upper lid over the pupil by `amount`, from open at 0 to as annoyed as it
    /// gets at 1. `slope` 1 tips the lid down on the left, -1 on the right
    pub fn annoy(&mut self, amount: f64, slope: f64) {
        self.annoyed = (amount, slope);
    }

    /// shows `left` (0…1) of the liquid from `begin`, and flows there over `FLOW` from
    /// wherever it is on screen. without `animate`, the level steps instead
    pub fn drain_to(&mut self, left: f64, begin: f64, animate: bool) {
        let d = f64::from(self.diameter);
        let y = level_y(d, height_for(left.clamp(0.0, 1.0)));
        let from = self.level.at(begin);
        self.level.set(y);
        if animate {
            self.level.add(
                Anim::tween(from, y, begin, FLOW, Curve::Linear, Fill::Entrance).keyed("level"),
            );
        }
    }

    /// the number in the pupil. with `flip`, the digits that change turn over
    pub fn set_count(&mut self, text: &str, flip: bool, now: f64) {
        if let Some(count) = &mut self.count {
            count.set(text, flip, now);
        }
    }

    /// brings the count up from nothing over `duration`, from `begin`
    pub fn reveal_count(&mut self, begin: f64, duration: f64) {
        self.count_opacity.set(1.0);
        self.count_opacity.add(Anim::tween(
            0.0,
            1.0,
            begin,
            duration,
            super::anim::EASE,
            Fill::Entrance,
        ));
    }

    /// whether it changes from frame to frame at `t`
    pub fn moving(&self, t: f64) -> bool {
        self.blink_from.is_some()
            || self.drift_from.is_some()
            || self.flow_from.is_some()
            || self.hide_at.is_some_and(|at| t < at)
            || [
                &self.scale,
                &self.shift_x,
                &self.shift_y,
                &self.opacity,
                &self.blur,
            ]
            .iter()
            .any(|prop| prop.moving(t))
            || self.count_opacity.moving(t)
            || self.level.moving(t)
            || self.look.iter().flatten().any(|prop| prop.moving(t))
            || self.count.as_ref().is_some_and(|count| count.moving(t))
    }

    /// the logical area that it can draw on, shadow and glow included, before its move
    pub fn bounds(&self) -> Rect {
        let pad = self.diameter * PAD;
        self.frame().inset(-pad)
    }

    /// draws it at `t`, through the transform in effect
    pub fn draw(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        if self.hide_at.is_some_and(|at| t >= at) {
            return Ok(());
        }
        let opacity = self.opacity.at(t) as f32;
        if opacity <= 0.002 {
            return Ok(());
        }
        let transform = self.transform(t);
        if opacity < 0.998 {
            // fades as one, as a layer does, rather than part by part
            let (layer, back) = gl.begin_layer(self.bounds())?;
            self.draw_parts(gl, fonts, t)?;
            gl.end_layer(back);
            gl.with(transform, |gl| gl.composite(layer, opacity, 0.0))
        } else {
            let outer = gl.transform();
            gl.set_transform(transform.then(outer));
            let drawn = self.draw_parts(gl, fonts, t);
            gl.set_transform(outer);
            drawn
        }
    }

    fn draw_parts(&mut self, gl: &mut Gl, fonts: &mut Fonts, t: f64) -> anyhow::Result<()> {
        let blur = self.blur.at(t) as f32;
        if blur > 0.05 {
            self.shade(gl, t, SHADOW | DISC);
            let (layer, back) = gl.begin_layer(self.bounds())?;
            self.shade(gl, t, PIGMENT);
            gl.end_layer(back);
            gl.composite(layer, 1.0, blur)?;
            self.shade(gl, t, GLASS_PART | EYE);
        } else {
            self.shade(gl, t, SHADOW | DISC | PIGMENT | GLASS_PART | EYE);
        }
        let centre = self.frame().center();
        let opacity = self.count_opacity.at(t) as f32;
        if let Some(count) = &mut self.count {
            count.draw(gl, fonts, t, centre, opacity);
        }
        Ok(())
    }

    fn shade(&self, gl: &Gl, t: f64, parts: i32) {
        let d = f64::from(self.diameter);
        let r = d / 2.0;
        let drift = match self.drift_from {
            Some(begin) if t > begin => {
                let along = (t - begin) / MORPH_PERIOD;
                if along < 1.0 {
                    (TAU * along, swell_at(along))
                } else {
                    (TAU * along.fract(), 1.0)
                }
            }
            _ => (0.0, 0.0),
        };
        let blink = match self.blink_from {
            Some(begin) => {
                let into = (t - begin).rem_euclid(BLINK_PERIOD);
                [self.blink[0].at(into), self.blink[1].at(into)]
            }
            None => [1.0, 1.0],
        };
        let wave = match self.flow_from {
            Some(begin) => r + d * WAVE_LENGTH * ((t - begin) / WAVE_PERIOD).rem_euclid(1.0),
            None => r,
        };
        let look: Vec<f32> = self
            .look
            .iter()
            .flat_map(|[x, y]| [x.at(t) as f32, y.at(t) as f32])
            .collect();
        let pupil = [
            PUPIL.0 as f32,
            PUPIL.1 as f32,
            PUPIL.2 as f32,
            PUPIL.3 as f32,
        ];
        let [br, bg, bb, _] = [self.body.0[4], self.body.0[5], self.body.0[6], 0.0];
        let glow: Rgba = [
            br * GLOW_OPACITY,
            bg * GLOW_OPACITY,
            bb * GLOW_OPACITY,
            GLOW_OPACITY,
        ];
        let pad = self.diameter * PAD;
        gl.bead(self.frame(), pad, |glow_gl, loc| {
            use glow::HasContext;
            // SAFETY: plain GL calls on the current context, with the bead program in use
            unsafe {
                glow_gl.uniform_1_f32(loc("u_d").as_ref(), self.diameter);
                glow_gl.uniform_1_i32(loc("u_parts").as_ref(), parts);
                glow_gl.uniform_1_f32(loc("u_solid").as_ref(), f32::from(u8::from(self.solid)));
                glow_gl.uniform_1_f32(loc("u_counts").as_ref(), f32::from(u8::from(self.counts)));
                glow_gl.uniform_2_f32(loc("u_drift").as_ref(), drift.0 as f32, drift.1 as f32);
                glow_gl.uniform_1_f32(loc("u_flat").as_ref(), self.flat as f32);
                glow_gl.uniform_2_f32_slice(loc("u_look").as_ref(), &look);
                glow_gl.uniform_2_f32(loc("u_blink").as_ref(), blink[0] as f32, blink[1] as f32);
                glow_gl.uniform_2_f32(
                    loc("u_lid").as_ref(),
                    self.annoyed.0 as f32,
                    self.annoyed.1 as f32,
                );
                glow_gl.uniform_1_f32(loc("u_level").as_ref(), self.level.at(t) as f32);
                glow_gl.uniform_1_f32(loc("u_wave").as_ref(), wave as f32);
                glow_gl.uniform_4_f32_slice(loc("u_body").as_ref(), &self.body.0);
                glow_gl.uniform_1_f32_slice(loc("u_body_at").as_ref(), &self.body.1);
                glow_gl.uniform_4_f32_slice(loc("u_white").as_ref(), &self.white.0);
                glow_gl.uniform_4_f32_slice(loc("u_sky").as_ref(), &self.sky.0);
                glow_gl.uniform_1_f32_slice(loc("u_rings_at").as_ref(), &self.white.1);
                glow_gl.uniform_4_f32_slice(loc("u_pupil").as_ref(), &pupil);
                glow_gl.uniform_4_f32_slice(loc("u_glow").as_ref(), &glow);
            }
        });
    }

    /// forgets animations that are over
    /// deletes the textures of its count
    pub fn delete(&mut self, gl: &Gl) {
        if let Some(count) = &mut self.count {
            count.delete(gl);
        }
    }

    pub fn prune(&mut self, t: f64) {
        for prop in [
            &mut self.scale,
            &mut self.shift_x,
            &mut self.shift_y,
            &mut self.opacity,
            &mut self.blur,
            &mut self.count_opacity,
            &mut self.level,
        ] {
            prop.prune(t);
        }
        for prop in self.look.iter_mut().flatten() {
            prop.prune(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_gets_every_number_it_names() {
        let defines = shader_defines();
        let code: String = include_str!("shaders/bead.frag")
            .lines()
            .map(|line| line.split("//").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        for word in code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            let constant = word.len() > 2
                && word.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                && word != "TAU";
            if constant {
                assert!(
                    defines.contains(&format!("#define {word} ")),
                    "{word} has no #define"
                );
            }
        }
    }

    #[test]
    fn white_and_sky_share_their_stop_positions() {
        assert_eq!(stops(&WHITE).1, stops(&SKY).1);
    }

    #[test]
    fn a_bead_moved_toward_a_rect_fills_it() {
        let mut bead = Bead::new(100.0, false, false);
        bead.origin = (50.0, 50.0);
        let (scale, x, y) = bead.toward(Rect::new(0.0, 0.0, 50.0, 50.0));
        assert_eq!((scale, x, y), (0.5, -75.0, -75.0));
        bead.scale.set(scale);
        bead.shift_x.set(x);
        bead.shift_y.set(y);
        let moved = bead.transform(0.0);
        assert_eq!(moved.apply(50.0, 50.0), (0.0, 0.0));
        assert_eq!(moved.apply(150.0, 150.0), (50.0, 50.0));
    }
}
