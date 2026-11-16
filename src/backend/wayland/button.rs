//! Skip, Postpone and Dismiss: capsules with a title and, when they have one, their
//! shortcut's hint. under the pointer a glow fades up over a button, quicker than it fades
//! away. asleep, a button takes neither clicks nor its key, and looks no different.

use super::anim::{ARRIVE, Anim, Fill, Prop};
use super::gl::{Gl, Rect, Shape, premul};
use super::text::{Fit, Fonts, Style, Text, Weight};
use crate::app::Action;
use crate::config::{Key, KeySpec};

/// under the pointer a glow over a button comes up to this opacity: white on dark, black on
/// light. coming is quicker than going
const HOVER_GLOW: f64 = 0.10;
const HOVER_IN: f64 = 0.15;
const HOVER_OUT: f64 = 0.22;
/// between a title and its hint
const HINT_GAP: f32 = 6.0;

/// what a button is made of, for the surface it sits on
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// a capsule of see-through glass, on the break screen
    Glass,
    /// a filled push button, for buttons already on a surface such as the warning's
    Fill,
}

#[derive(Clone, Copy, Debug)]
pub struct ButtonStyle {
    pub surface: Surface,
    pub height: f32,
    /// the button is as wide as its title plus `padding`, and no narrower than `min_width`
    pub min_width: f32,
    pub padding: f32,
    pub title: f32,
    pub hint: f32,
}

pub struct Button {
    /// where it sits, in its surface's logical pixels, before any move
    pub rect: Rect,
    pub action: Action,
    /// the key that presses it, while it is awake. a global shortcut only shows its hint
    pub key: Option<KeySpec>,
    title: String,
    hint: Option<String>,
    style: ButtonStyle,
    glow: Prop,
    hovered: bool,
    pub awake: bool,
    pub opacity: Prop,
    /// how far below its place it is, for an entrance that rises
    pub drop: Prop,
    texts: Option<(f32, Text, Option<Text>)>,
}

impl Button {
    /// a button with `title`, and `hint` after it. `key` presses it on the break screen
    pub fn new(
        fonts: &mut Fonts,
        title: &str,
        key: Option<KeySpec>,
        hint: Option<String>,
        action: Action,
        style: ButtonStyle,
    ) -> Button {
        let mut width = fonts.width(title, Style::new(style.title, Weight::Medium));
        if let Some(hint) = &hint {
            width += HINT_GAP + fonts.width(hint, Style::new(style.hint, Weight::Medium));
        }
        Button {
            rect: Rect::new(
                0.0,
                0.0,
                (width.ceil() + style.padding).max(style.min_width),
                style.height,
            ),
            action,
            key,
            title: title.into(),
            hint,
            style,
            glow: Prop::new(0.0),
            hovered: false,
            awake: true,
            opacity: Prop::new(1.0),
            drop: Prop::new(0.0),
            texts: None,
        }
    }

    /// whether a point, in the surface's logical pixels, is on it
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.awake && self.rect.contains(x, y)
    }

    /// eases the glow up or away, from where it is now
    pub fn hover(&mut self, on: bool, now: f64) {
        let on = on && self.awake;
        if on == self.hovered {
            return;
        }
        self.hovered = on;
        let from = self.glow.at(now);
        let (to, duration) = if on {
            (HOVER_GLOW, HOVER_IN)
        } else {
            (0.0, HOVER_OUT)
        };
        self.glow.set(to);
        self.glow
            .add(Anim::tween(from, to, now, duration, ARRIVE, Fill::Entrance));
    }

    pub fn sleep(&mut self, now: f64) {
        self.hover(false, now);
        self.awake = false;
    }

    pub fn moving(&self, t: f64) -> bool {
        self.glow.moving(t) || self.opacity.moving(t) || self.drop.moving(t)
    }

    /// draws it at `t` on a dark surface or a light one, faded by `opacity`
    pub fn draw(&mut self, gl: &Gl, fonts: &mut Fonts, t: f64, dark: bool, opacity: f32) {
        let opacity = opacity * self.opacity.at(t) as f32;
        if opacity <= 0.0 {
            return;
        }
        let scale = gl.scale();
        if self
            .texts
            .as_ref()
            .is_none_or(|(s, ..)| (s - scale).abs() > 1e-3)
        {
            if let Some((_, title, hint)) = self.texts.take() {
                title.texture.delete(gl);
                if let Some(hint) = hint {
                    hint.texture.delete(gl);
                }
            }
            let render = |fonts: &mut Fonts, text: &str, size: f32| {
                fonts.render(gl, text, Style::new(size, Weight::Medium), Fit::LINE, scale)
            };
            let title = render(fonts, &self.title, self.style.title);
            let hint = self
                .hint
                .as_deref()
                .map(|hint| render(fonts, hint, self.style.hint));
            self.texts = Some((scale, title, hint));
        }
        let rect = Rect {
            y: self.rect.y + self.drop.at(t) as f32,
            ..self.rect
        };
        let radius = rect.h / 2.0;
        let shape = match (self.style.surface, dark) {
            (Surface::Glass, _) => Shape {
                fill: premul([1.0, 1.0, 1.0, 0.13], 1.0),
                border: premul([1.0, 1.0, 1.0, 0.20], 1.0),
                border_width: 1.0,
                radius,
            },
            (Surface::Fill, true) => Shape {
                fill: premul([1.0, 1.0, 1.0, 0.14], 1.0),
                border: premul([1.0, 1.0, 1.0, 0.06], 1.0),
                border_width: 1.0,
                radius,
            },
            (Surface::Fill, false) => Shape {
                fill: premul([1.0, 1.0, 1.0, 1.0], 1.0),
                border: premul([0.0, 0.0, 0.0, 0.10], 1.0),
                border_width: 1.0,
                radius,
            },
        };
        if self.style.surface == Surface::Fill && !dark {
            gl.shadow(
                Rect {
                    y: rect.y + 1.0,
                    ..rect
                },
                radius,
                1.5,
                premul([0.0, 0.0, 0.0, 0.10], opacity),
            );
        }
        gl.shape(rect, &shape, opacity);
        let glow = self.glow.at(t) as f32;
        if glow > 0.0 {
            let tint = if dark { 1.0 } else { 0.0 };
            gl.shape(
                rect,
                &Shape::filled(premul([tint, tint, tint, glow], 1.0), radius),
                opacity,
            );
        }

        let Some((_, title, hint)) = &self.texts else {
            return;
        };
        let label = if dark || self.style.surface == Surface::Glass {
            [1.0, 1.0, 1.0, 0.88]
        } else {
            [0.0, 0.0, 0.0, 0.85]
        };
        let secondary = [label[0], label[1], label[2], 0.5];
        let title_w = fonts.width(&self.title, Style::new(self.style.title, Weight::Medium));
        let hint_w = self.hint.as_deref().map_or(0.0, |h| {
            HINT_GAP + fonts.width(h, Style::new(self.style.hint, Weight::Medium))
        });
        let x = rect.x + ((rect.w - title_w - hint_w) / 2.0).round();
        // the title's ink sits in the middle, and the hint on the title's baseline
        let ink_mid = (title.ink.0 + title.ink.1) / 2.0;
        let baseline = (rect.y + rect.h / 2.0 + (title.last_baseline - ink_mid)).round();
        gl.image(
            &title.texture,
            Rect::new(x, baseline - title.last_baseline, title.width, title.height),
            premul(label, opacity),
            [0.0, 0.0, 1.0, 1.0],
        );
        if let Some(hint) = hint {
            gl.image(
                &hint.texture,
                Rect::new(
                    x + title_w + HINT_GAP,
                    baseline - hint.last_baseline,
                    hint.width,
                    hint.height,
                ),
                premul(secondary, opacity),
                [0.0, 0.0, 1.0, 1.0],
            );
        }
    }

    pub fn delete(self, gl: &Gl) {
        if let Some((_, title, hint)) = self.texts {
            title.texture.delete(gl);
            if let Some(hint) = hint {
                hint.texture.delete(gl);
            }
        }
    }
}

/// the hint a button shows for its shortcut, as Linux desktops write keys
pub fn hint(spec: KeySpec) -> String {
    let key = match spec.key {
        Key::Escape => "Esc".to_owned(),
        Key::Space => "Space".to_owned(),
        Key::Enter => "Enter".to_owned(),
        Key::Tab => "Tab".to_owned(),
        Key::Delete => "Backspace".to_owned(),
        Key::Char(c) => c.to_uppercase().collect(),
    };
    let mods = spec.mods;
    [
        (mods.ctrl, "Ctrl+"),
        (mods.alt, "Alt+"),
        (mods.shift, "Shift+"),
        (mods.cmd, "Super+"),
    ]
    .into_iter()
    .filter_map(|(held, name)| held.then_some(name))
    .chain([key.as_str()])
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_read_as_linux_desktops_write_keys() {
        let h = |spec: &str| hint(spec.parse().unwrap());
        assert_eq!(h("esc"), "Esc");
        assert_eq!(h("space"), "Space");
        assert_eq!(h("return"), "Enter");
        assert_eq!(h("backspace"), "Backspace");
        assert_eq!(h("s"), "S");
        assert_eq!(h("super+shift+alt+ctrl+s"), "Ctrl+Alt+Shift+Super+S");
    }
}
