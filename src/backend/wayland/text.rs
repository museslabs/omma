//! text, laid out with the system's fonts and rasterized into textures. macOS draws in San
//! Francisco. here, omma prefers Inter, which is closest to it, and otherwise the desktop's
//! own sans-serif.

use std::collections::HashMap;
use std::process::Command;

use cosmic_text::{
    Align, Attrs, Buffer, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Shaping,
    SwashCache, SwashContent, Weight as FontWeight, Wrap,
};

use super::gl::{Gl, Texture};

/// families that look most like San Francisco, before the desktop's sans-serif
const PREFERRED: [&str; 5] = [
    "Inter",
    "Inter Variable",
    "Inter Display",
    "SF Pro Text",
    "SF Pro Display",
];
/// common sans-serifs, for when fontconfig names none that is installed
const FALLBACKS: [&str; 4] = ["Noto Sans", "Cantarell", "DejaVu Sans", "Liberation Sans"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Weight {
    Medium,
    Semibold,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// in logical pixels, as macOS sizes text in points
    pub size: f32,
    pub weight: Weight,
    /// digits all as wide as each other, so that a count does not jiggle
    pub tabular: bool,
}

impl Style {
    pub const fn new(size: f32, weight: Weight) -> Style {
        Style {
            size,
            weight,
            tabular: false,
        }
    }

    pub const fn tabular(self) -> Style {
        Style {
            tabular: true,
            ..self
        }
    }

    /// as tall as macOS makes a line of this size, in logical pixels
    pub fn line_height(&self) -> f32 {
        (self.size * 1.25).ceil()
    }
}

/// text rasterized for a scale, white on transparent, to tint when drawn
pub struct Text {
    pub texture: Texture,
    /// in logical pixels: the widest line, and every line's height together
    pub width: f32,
    pub height: f32,
    /// from the top to the last line's baseline
    pub last_baseline: f32,
    /// the ink's top and bottom, from the top. digits' ink is what a count centers
    pub ink: (f32, f32),
}

pub struct Fonts {
    system: FontSystem,
    cache: SwashCache,
    family: String,
    /// the width of each text measured before, by what it says and its style
    widths: HashMap<(String, StyleKey), f32>,
}

/// a `Style` that can key a map
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct StyleKey(u32, Weight, bool);

impl From<Style> for StyleKey {
    fn from(style: Style) -> StyleKey {
        StyleKey(style.size.to_bits(), style.weight, style.tabular)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
}

/// how text fits its space: wrapped at `width` onto at most `lines` lines, or on one line
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub width: Option<f32>,
    pub lines: usize,
    pub alignment: Alignment,
}

impl Fit {
    /// one line, as long as it is
    pub const LINE: Fit = Fit {
        width: None,
        lines: 1,
        alignment: Alignment::Left,
    };
}

impl Fonts {
    /// loads the system's fonts, which takes a moment
    pub fn new() -> Fonts {
        let system = FontSystem::new();
        let installed = |name: &str| {
            system
                .db()
                .faces()
                .any(|face| face.families.iter().any(|(family, _)| family == name))
        };
        let desktop = desktop_sans_serif();
        let family = PREFERRED
            .iter()
            .map(|name| name.to_string())
            .chain(desktop)
            .chain(FALLBACKS.iter().map(|name| name.to_string()))
            .find(|name| installed(name))
            .unwrap_or_default();
        Fonts {
            system,
            cache: SwashCache::new(),
            family,
            widths: HashMap::new(),
        }
    }

    fn attrs(family: &str, style: Style) -> Attrs<'_> {
        let family = if family.is_empty() {
            Family::SansSerif
        } else {
            Family::Name(family)
        };
        let weight = match style.weight {
            Weight::Medium => FontWeight::MEDIUM,
            Weight::Semibold => FontWeight::SEMIBOLD,
        };
        let mut attrs = Attrs::new().family(family).weight(weight);
        if style.tabular {
            let mut features = FontFeatures::new();
            features.enable(FeatureTag::new(b"tnum"));
            attrs = attrs.font_features(features);
        }
        attrs
    }

    fn layout(
        &mut self,
        text: &str,
        style: Style,
        max_width: Option<f32>,
        alignment: Alignment,
        scale: f32,
    ) -> Buffer {
        let metrics = Metrics::new(style.size * scale, style.line_height() * scale);
        let mut buffer = Buffer::new(&mut self.system, metrics);
        let attrs = Fonts::attrs(&self.family, style);
        let align = match alignment {
            Alignment::Left => Align::Left,
            Alignment::Center => Align::Center,
        };
        {
            let mut buffer = buffer.borrow_with(&mut self.system);
            buffer.set_wrap(if max_width.is_some() {
                Wrap::WordOrGlyph
            } else {
                Wrap::None
            });
            buffer.set_size(max_width.map(|w| w * scale), None);
            buffer.set_text(text, &attrs, Shaping::Advanced, Some(align));
            buffer.shape_until_scroll(true);
        }
        buffer
    }

    /// how wide `text` is on one line, in logical pixels
    pub fn width(&mut self, text: &str, style: Style) -> f32 {
        let key = (text.to_owned(), StyleKey::from(style));
        if let Some(&width) = self.widths.get(&key) {
            return width;
        }
        let buffer = self.layout(text, style, None, Alignment::Left, 1.0);
        let width = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max);
        self.widths.insert(key, width);
        width
    }

    /// `text` in `style` at `scale` pixels per logical pixel, fitted as `fit` says
    pub fn render(&mut self, gl: &Gl, text: &str, style: Style, fit: Fit, scale: f32) -> Text {
        let Fit {
            width: max_width,
            lines: max_lines,
            alignment,
        } = fit;
        let buffer = self.layout(text, style, max_width, alignment, scale);
        let line_height = style.line_height() * scale;
        let runs: Vec<_> = buffer.layout_runs().take(max_lines.max(1)).collect();
        let lines = runs.len().max(1);
        let content_w = runs.iter().map(|run| run.line_w).fold(0.0, f32::max);
        let width_px = max_width
            .map(|w| w * scale)
            .filter(|_| alignment == Alignment::Center)
            .unwrap_or(content_w)
            .ceil()
            .max(1.0) as u32
            + 2;
        let height_px = (line_height * lines as f32).ceil().max(1.0) as u32 + 2;
        let mut pixels = vec![0u8; (width_px * height_px * 4) as usize];
        let mut ink = (f32::MAX, f32::MIN);
        let mut last_baseline = 0.0;
        for (i, run) in runs.iter().enumerate() {
            let top = i as f32 * line_height;
            let baseline = top + (run.line_y - run.line_top);
            last_baseline = baseline;
            for glyph in run.glyphs {
                let physical = glyph.physical((1.0, baseline + 1.0), 1.0);
                let Some(image) = self
                    .cache
                    .get_image(&mut self.system, physical.cache_key)
                    .as_ref()
                else {
                    continue;
                };
                let left = physical.x + image.placement.left;
                let top = physical.y - image.placement.top;
                let (w, h) = (image.placement.width as i32, image.placement.height as i32);
                if w > 0 && h > 0 {
                    ink.0 = ink.0.min(top as f32);
                    ink.1 = ink.1.max((top + h) as f32);
                }
                for y in 0..h {
                    for x in 0..w {
                        let (px, py) = (left + x, top + y);
                        if px < 0 || py < 0 || px >= width_px as i32 || py >= height_px as i32 {
                            continue;
                        }
                        let at = ((py as u32 * width_px + px as u32) * 4) as usize;
                        let src = (y * w + x) as usize;
                        let rgba = match image.content {
                            SwashContent::Mask => {
                                let a = image.data[src];
                                [a, a, a, a]
                            }
                            SwashContent::Color => {
                                let c = &image.data[src * 4..src * 4 + 4];
                                let a = u16::from(c[3]);
                                let mul = |v: u8| ((u16::from(v) * a + 127) / 255) as u8;
                                [mul(c[0]), mul(c[1]), mul(c[2]), c[3]]
                            }
                            SwashContent::SubpixelMask => {
                                let c = &image.data[src * 4..src * 4 + 4];
                                let a = c[0].max(c[1]).max(c[2]);
                                [a, a, a, a]
                            }
                        };
                        // glyphs that touch add up, as ink does
                        let dst = &mut pixels[at..at + 4];
                        for (d, s) in dst.iter_mut().zip(rgba) {
                            *d = d.saturating_add(s);
                        }
                    }
                }
            }
        }
        if ink.0 > ink.1 {
            ink = (0.0, 0.0);
        }
        let texture = gl.upload(width_px, height_px, &pixels, true);
        Text {
            texture,
            width: width_px as f32 / scale,
            height: height_px as f32 / scale,
            last_baseline: (last_baseline + 1.0) / scale,
            ink: (ink.0 / scale, ink.1 / scale),
        }
    }
}

/// the family fontconfig picks for sans-serif, if fc-match is there to ask
fn desktop_sans_serif() -> Option<String> {
    let output = Command::new("fc-match")
        .args(["--format=%{family[0]}", "sans-serif"])
        .output()
        .ok()?;
    let family = String::from_utf8(output.stdout).ok()?;
    let family = family.trim();
    (output.status.success() && !family.is_empty()).then(|| family.to_owned())
}
