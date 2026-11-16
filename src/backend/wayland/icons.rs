//! the tray icons, from the same SVGs as the macOS menu-bar icon. macOS tints a template
//! image to suit the menu bar. a tray takes pixels, so omma fills the shapes light for a
//! dark desktop and dark for a light one.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

use crate::app::TrayIcon;

const NORMAL: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tray-icon.svg"));
const INCOMING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tray-icon-incoming.svg"
));
const DISABLED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tray-icon-disabled.svg"
));

/// the sizes trays ask for most, so each gets crisp pixels
const SIZES: [u32; 5] = [16, 22, 24, 32, 48];
/// the SVGs' view box: 44 units square from (-22, -22)
const VIEW: f32 = 44.0;

pub struct Pixels {
    pub size: u32,
    /// ARGB32 in network byte order, unpremultiplied, as StatusNotifierItem takes it
    pub argb: Vec<u8>,
}

/// the icon at each tray size, in a light color on dark or a dark one on light
pub fn render(icon: TrayIcon, dark: bool) -> Vec<Pixels> {
    let svg = match icon {
        TrayIcon::Normal => NORMAL,
        TrayIcon::Incoming => INCOMING,
        TrayIcon::Disabled => DISABLED,
    };
    let shade = if dark { 0xf2 } else { 0x26 };
    let paths = paths(svg);
    SIZES
        .iter()
        .filter_map(|&size| {
            let mut pixmap = Pixmap::new(size, size)?;
            let mut paint = Paint::default();
            paint.set_color_rgba8(shade, shade, shade, 255);
            paint.anti_alias = true;
            let scale = size as f32 / VIEW;
            let transform =
                Transform::from_translate(VIEW / 2.0, VIEW / 2.0).post_scale(scale, scale);
            for (path, rule) in &paths {
                pixmap.fill_path(path, &paint, *rule, transform, None);
            }
            let argb = pixmap
                .pixels()
                .iter()
                .flat_map(|px| {
                    let px = px.demultiply();
                    [px.alpha(), px.red(), px.green(), px.blue()]
                })
                .collect();
            Some(Pixels { size, argb })
        })
        .collect()
}

/// each `<path>` in the SVG, with its fill rule. the icons use absolute M, L, C and Z alone
fn paths(svg: &str) -> Vec<(tiny_skia::Path, FillRule)> {
    svg.split("<path")
        .skip(1)
        .filter_map(|element| {
            let element = element.split("/>").next()?;
            let rule = if element.contains("fill-rule=\"evenodd\"") {
                FillRule::EvenOdd
            } else {
                FillRule::Winding
            };
            let d = element.split(" d=\"").nth(1)?.split('"').next()?;
            Some((parse(d)?, rule))
        })
        .collect()
}

fn parse(d: &str) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();
    let mut tokens = d
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .flat_map(split_command)
        .peekable();
    let mut command = 'M';
    while let Some(token) = tokens.peek() {
        if let Some(c) = token.chars().next().filter(|c| c.is_ascii_alphabetic()) {
            command = c;
            tokens.next();
            if command == 'Z' {
                builder.close();
                continue;
            }
        }
        let mut number = || tokens.next()?.parse::<f32>().ok();
        match command {
            'M' => {
                builder.move_to(number()?, number()?);
                // more pairs after a move draw lines, as SVG reads them
                command = 'L';
            }
            'L' => builder.line_to(number()?, number()?),
            'C' => {
                let (x1, y1, x2, y2, x, y) = (
                    number()?,
                    number()?,
                    number()?,
                    number()?,
                    number()?,
                    number()?,
                );
                builder.cubic_to(x1, y1, x2, y2, x, y);
            }
            _ => return None,
        }
    }
    builder.finish()
}

/// "Z" stuck to a number, as in "0Z", comes apart
fn split_command(token: &str) -> Vec<&str> {
    match token.find(|c: char| c.is_ascii_alphabetic()) {
        Some(0) if token.len() > 1 => vec![&token[..1], &token[1..]],
        Some(i) if i > 0 => vec![&token[..i], &token[i..i + 1]]
            .into_iter()
            .chain((i + 1 < token.len()).then(|| &token[i + 1..]))
            .collect(),
        _ => vec![token],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses_into_its_paths() {
        assert_eq!(paths(NORMAL).len(), 2);
        assert_eq!(paths(INCOMING).len(), 3);
        assert_eq!(paths(DISABLED).len(), 1);
    }

    #[test]
    fn icons_have_ink_in_the_middle_and_none_in_the_corners() {
        for icon in [TrayIcon::Normal, TrayIcon::Incoming, TrayIcon::Disabled] {
            for pixels in render(icon, true) {
                let size = pixels.size as usize;
                let alpha = |x: usize, y: usize| pixels.argb[(y * size + x) * 4];
                assert_eq!(alpha(0, size - 1), 0);
                let inked = pixels
                    .argb
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|px| px[0] > 0)
                    .count();
                assert!(inked > size * size / 10, "{icon:?} at {size}");
            }
        }
    }
}
