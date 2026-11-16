//! the countdown as a flip clock: a digit that changes turns over like a leaf. its top half
//! falls away and the new bottom half lands with a little bounce.

use std::collections::HashMap;

use super::anim::{Anim, EASE, Fill, GATHER, Prop};
use super::gl::{Affine, Gl, Rect, Shape, premul};
use super::text::{Fit, Fonts, Style, Text, Weight};
use crate::mati::colour::Rgba;
use crate::mati::motion::POP;

/// how long the old top half takes to fall edge-on
const FALL: f64 = 0.15;
/// how dark a leaf gets as it turns away from the light
const SHADE: f32 = 0.45;
/// how long the shade takes to leave the landed half
const UNSHADE: f64 = 0.18;

pub struct FlipCount {
    style: Style,
    /// the leaves' color, which hides what is under a turning one
    ground: [f32; 4],
    cells: Vec<Cell>,
    /// each character, rasterized for `scale`
    glyphs: HashMap<char, Text>,
    scale: f32,
}

struct Cell {
    /// what its top half shows, and its bottom half until a turn lands
    top: char,
    bottom: char,
    turn: Option<Turn>,
}

struct Turn {
    old: char,
    new: char,
    /// the old top half falls from 1 to 0, and the new bottom half lands from 0 to 1
    fall: Prop,
    land: Prop,
    shade: Prop,
    unshade: Prop,
    settled: f64,
}

impl FlipCount {
    /// digits `size` logical pixels tall on leaves of the color `ground`
    pub fn new(size: f32, (r, g, b, a): Rgba) -> FlipCount {
        FlipCount {
            style: Style::new(size, Weight::Medium).tabular(),
            ground: [r as f32, g as f32, b as f32, a as f32],
            cells: Vec::new(),
            glyphs: HashMap::new(),
            scale: 0.0,
        }
    }

    /// shows `text`. with `flip`, each character that differs from the one in its place turns
    /// over. a count of another length is laid out again
    pub fn set(&mut self, text: &str, flip: bool, now: f64) {
        if self.cells.len() != text.chars().count() {
            self.cells = text
                .chars()
                .map(|ch| Cell {
                    top: ch,
                    bottom: ch,
                    turn: None,
                })
                .collect();
            return;
        }
        for (cell, ch) in self.cells.iter_mut().zip(text.chars()) {
            if cell.top == ch {
                continue;
            }
            let old = std::mem::replace(&mut cell.top, ch);
            if !flip {
                cell.bottom = ch;
                cell.turn = None;
                continue;
            }
            // a turn still under way lands at once under the new one
            if let Some(turn) = cell.turn.take() {
                cell.bottom = turn.new;
            }
            let mut fall = Prop::new(0.0);
            fall.add(Anim::tween(1.0, 0.0, now, FALL, GATHER, Fill::Exit));
            let mut land = Prop::new(1.0);
            let landing = Anim::spring(0.0, 1.0, now + FALL, POP, Fill::Entrance);
            let settled = landing.end();
            land.add(landing);
            let mut shade = Prop::new(0.0);
            shade.add(Anim::tween(
                0.0,
                f64::from(SHADE),
                now,
                FALL,
                EASE,
                Fill::Exit,
            ));
            let mut unshade = Prop::new(0.0);
            unshade.add(Anim::tween(
                f64::from(SHADE),
                0.0,
                now + FALL,
                UNSHADE,
                EASE,
                Fill::Entrance,
            ));
            cell.turn = Some(Turn {
                old,
                new: ch,
                fall,
                land,
                shade,
                unshade,
                settled,
            });
        }
    }

    /// deletes the textures of its glyphs. it draws them again when it next needs them
    pub fn delete(&mut self, gl: &Gl) {
        for (_, text) in self.glyphs.drain() {
            text.texture.delete(gl);
        }
    }

    pub fn moving(&self, t: f64) -> bool {
        self.cells
            .iter()
            .any(|cell| cell.turn.as_ref().is_some_and(|turn| t < turn.settled))
    }

    fn glyph(&mut self, gl: &Gl, fonts: &mut Fonts, ch: char) {
        if !self.glyphs.contains_key(&ch) {
            let text = fonts.render(gl, &ch.to_string(), self.style, Fit::LINE, self.scale);
            self.glyphs.insert(ch, text);
        }
    }

    /// draws the count centered on `centre`, faded by `opacity`
    pub fn draw(
        &mut self,
        gl: &mut Gl,
        fonts: &mut Fonts,
        t: f64,
        centre: (f32, f32),
        opacity: f32,
    ) {
        if opacity <= 0.0 || self.cells.is_empty() {
            return;
        }
        let scale = gl.scale();
        if (scale - self.scale).abs() > 1e-3 {
            self.delete(gl);
            self.scale = scale;
        }
        // settled turns hand their new half to the cell
        for cell in &mut self.cells {
            if cell.turn.as_ref().is_some_and(|turn| t >= turn.settled) {
                let turn = cell.turn.take().expect("checked above");
                cell.bottom = turn.new;
            }
        }
        let chars: Vec<char> = self
            .cells
            .iter()
            .flat_map(|cell| {
                let turning = cell.turn.as_ref().map(|turn| [turn.old, turn.new]);
                [cell.top, cell.bottom]
                    .into_iter()
                    .chain(turning.into_iter().flatten())
            })
            .chain(['0'])
            .collect();
        for ch in chars {
            self.glyph(gl, fonts, ch);
        }

        let digit_w = fonts.width("0", self.style).ceil();
        let colon_w = fonts.width(":", self.style).ceil();
        let width = |ch: char| if ch == ':' { colon_w } else { digit_w };
        // an even height, so the fold between the halves falls on a whole pixel
        let height = (self.style.line_height() / 2.0).ceil() * 2.0;
        let total: f32 = self.cells.iter().map(|cell| width(cell.top)).sum();
        let left = (centre.0 - total / 2.0).round();
        let top = (centre.1 - height / 2.0).round();
        // every glyph sits on one baseline, which puts the digits' ink in the middle
        let zero = &self.glyphs[&'0'];
        let ink_mid = (zero.ink.0 + zero.ink.1) / 2.0;
        let baseline = top + height / 2.0 + (zero.last_baseline - ink_mid);
        let fold = top + height / 2.0;
        let ground = premul(self.ground, opacity);

        let mut x = left;
        for cell in &self.cells {
            let w = width(cell.top);
            let upper = Rect::new(x, top, w, height / 2.0);
            let lower = Rect::new(x, fold, w, height / 2.0);
            let half = |gl: &Gl, ch: char, clip: Rect, shade: f32| {
                gl.shape(clip, &Shape::filled(ground, 0.0), 1.0);
                let text = &self.glyphs[&ch];
                let at = Rect::new(
                    x + (w - text.width) / 2.0,
                    baseline - text.last_baseline,
                    text.width,
                    text.height,
                );
                draw_clipped(gl, text, at, clip, opacity);
                if shade > 0.0 {
                    gl.shape(
                        clip,
                        &Shape::filled([0.0, 0.0, 0.0, shade * opacity], 0.0),
                        1.0,
                    );
                }
            };
            half(gl, cell.top, upper, 0.0);
            half(gl, cell.bottom, lower, 0.0);
            if let Some(turn) = &cell.turn {
                let fall = turn.fall.at(t) as f32;
                if fall > 0.0 {
                    gl.with(Affine::scale_about(1.0, fall, x, fold), |gl| {
                        half(gl, turn.old, upper, turn.shade.at(t) as f32);
                    });
                }
                let land = turn.land.at(t) as f32;
                if land > 0.0 {
                    gl.with(Affine::scale_about(1.0, land, x, fold), |gl| {
                        half(gl, turn.new, lower, turn.unshade.at(t) as f32);
                    });
                }
            }
            x += w;
        }
    }
}

/// the part of `text`, drawn over `at`, that falls inside `clip`
fn draw_clipped(gl: &Gl, text: &Text, at: Rect, clip: Rect, opacity: f32) {
    let x0 = at.x.max(clip.x);
    let y0 = at.y.max(clip.y);
    let x1 = (at.x + at.w).min(clip.x + clip.w);
    let y1 = (at.y + at.h).min(clip.y + clip.h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let uv = [
        (x0 - at.x) / at.w,
        (y0 - at.y) / at.h,
        (x1 - at.x) / at.w,
        (y1 - at.y) / at.h,
    ];
    gl.image(
        &text.texture,
        Rect::new(x0, y0, x1 - x0, y1 - y0),
        [opacity; 4],
        uv,
    );
}
