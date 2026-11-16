//! a break's background image, softened once into a small bitmap, as the macOS backend does
//! with Core Image. then no shader has to blur it for as long as the break lasts.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use image::RgbaImage;
use image::imageops::FilterType;

/// how soft the image is, in logical pixels on the display
const SOFTNESS: f64 = 16.0;
/// a pixel of the bitmap covers this many logical pixels, which a blur that wide hides
const BITMAP_PIXEL: f64 = 2.0;

/// an image file as it was, and the display size it is softened for
type Key = (PathBuf, Option<SystemTime>, (u32, u32));

/// the images softened lately, since breaks come round to the same images
#[derive(Default)]
pub struct Pictures {
    recent: Vec<(Key, RgbaImage)>,
}

/// as many as there can be displays of different sizes
const KEEP: usize = 4;

impl Pictures {
    /// `path` softened to fill a display of `size` logical pixels, or None when it is not an
    /// image that omma can read
    pub fn softened(&mut self, path: &Path, size: (u32, u32)) -> Option<RgbaImage> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let key = (path.to_path_buf(), modified, size);
        if let Some((_, image)) = self.recent.iter().find(|(k, _)| *k == key) {
            return Some(image.clone());
        }
        let image = soften(image::open(path).ok()?.into_rgba8(), size)?;
        if self.recent.len() >= KEEP {
            self.recent.remove(0);
        }
        self.recent.push((key, image.clone()));
        Some(image)
    }
}

/// how much to shrink a `width` by `height` image for its softened bitmap, and the blur to
/// give that bitmap, in its pixels
fn bitmap_for(width: f64, height: f64, size: (u32, u32)) -> (f64, f64) {
    // logical pixels on the display to a pixel of the image, once it fills the display
    let shown = (f64::from(size.0) / width).max(f64::from(size.1) / height);
    let shrink = (shown / BITMAP_PIXEL).min(1.0);
    (shrink, SOFTNESS / shown * shrink)
}

fn soften(image: RgbaImage, size: (u32, u32)) -> Option<RgbaImage> {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let (shrink, sigma) = bitmap_for(f64::from(w), f64::from(h), size);
    let small = if shrink < 1.0 {
        let sw = ((f64::from(w) * shrink).round() as u32).max(1);
        let sh = ((f64::from(h) * shrink).round() as u32).max(1);
        image::imageops::resize(&image, sw, sh, FilterType::Triangle)
    } else {
        image
    };
    Some(gaussian(small, sigma))
}

/// a gaussian blur of `sigma` pixels, as three box blurs, with the edge repeated outward so
/// that the edges do not fade
fn gaussian(image: RgbaImage, sigma: f64) -> RgbaImage {
    let (w, h) = image.dimensions();
    let mut data: Vec<f32> = image.into_raw().into_iter().map(f32::from).collect();
    for radius in box_radii(sigma) {
        box_pass(&mut data, w as usize, h as usize, radius, true);
        box_pass(&mut data, w as usize, h as usize, radius, false);
    }
    let bytes = data
        .into_iter()
        .map(|v| v.round().clamp(0.0, 255.0) as u8)
        .collect();
    RgbaImage::from_raw(w, h, bytes).expect("the same size it came in")
}

/// the radii of three box blurs that together come close to a gaussian of `sigma`
fn box_radii(sigma: f64) -> [usize; 3] {
    let ideal = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let mut lower = ideal.floor() as usize;
    if lower.is_multiple_of(2) {
        lower = lower.saturating_sub(1);
    }
    let upper = lower + 2;
    let wl = lower as f64;
    let m = ((12.0 * sigma * sigma - 3.0 * wl * wl - 12.0 * wl - 9.0) / (-4.0 * wl - 4.0)).round();
    std::array::from_fn(|i| if (i as f64) < m { lower / 2 } else { upper / 2 })
}

/// one running-sum box blur along rows, or along columns
fn box_pass(data: &mut [f32], w: usize, h: usize, radius: usize, rows: bool) {
    if radius == 0 {
        return;
    }
    let (lines, len) = if rows { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize| {
        if rows {
            (line * w + i) * 4
        } else {
            (i * w + line) * 4
        }
    };
    let span = (2 * radius + 1) as f32;
    let mut line_buf = vec![0.0f32; len * 4];
    for line in 0..lines {
        for c in 0..4 {
            let get = |i: isize| data[at(line, i.clamp(0, len as isize - 1) as usize) + c];
            let mut sum: f32 = (-(radius as isize)..=radius as isize).map(get).sum();
            for i in 0..len {
                line_buf[i * 4 + c] = sum / span;
                sum += get(i as isize + radius as isize + 1) - get(i as isize - radius as isize);
            }
        }
        for i in 0..len {
            data[at(line, i)..at(line, i) + 4].copy_from_slice(&line_buf[i * 4..i * 4 + 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_image_shrinks_to_a_bitmap_of_two_pixel_pixels() {
        let display = (1440, 900);
        // twice the display each way: half a pixel to a pixel, so it shrinks to a quarter
        let (shrink, sigma) = bitmap_for(2880.0, 1800.0, display);
        assert_eq!(shrink, 0.25);
        assert_eq!(sigma, SOFTNESS / BITMAP_PIXEL);
        // a small one is left as it is, and blurred by as many of its pixels as make up the
        // softness
        let (shrink, sigma) = bitmap_for(360.0, 225.0, display);
        assert_eq!(shrink, 1.0);
        assert_eq!(sigma, SOFTNESS / 4.0);
    }

    #[test]
    fn a_blur_keeps_a_flat_image_flat_and_spreads_a_point() {
        let flat = RgbaImage::from_pixel(9, 7, image::Rgba([10, 20, 30, 255]));
        assert_eq!(gaussian(flat.clone(), 3.0), flat);
        let mut dot = RgbaImage::new(21, 21);
        dot.put_pixel(10, 10, image::Rgba([255, 255, 255, 255]));
        let blurred = gaussian(dot, 2.0);
        assert!(blurred.get_pixel(10, 10)[3] < 255);
        assert!(blurred.get_pixel(12, 10)[3] > 0);
    }

    #[test]
    fn three_boxes_approach_the_gaussian() {
        for sigma in [1.0, 4.0, 8.0, 16.0] {
            let radii = box_radii(sigma);
            let variance: f64 = radii
                .iter()
                .map(|&r| {
                    let w = (2 * r + 1) as f64;
                    (w * w - 1.0) / 12.0
                })
                .sum();
            assert!((variance.sqrt() - sigma).abs() < 0.6, "{sigma}: {radii:?}");
        }
    }
}
