//! Faster equivalents of `image::imageops::resize` for the two filters dith resizes with.
//!
//! `image` resamples in two passes through a full-size `f32` intermediate image, and its
//! horizontal pass walks the intermediate column by column. Here every output row is produced on
//! its own (in parallel): its vertical taps are summed into one row of `f32` column sums, then
//! its horizontal taps are summed from that row. Window bounds, weights and the order of every
//! `f32` addition are the ones `image` 0.25 uses. The one difference is the Lanczos sine, which
//! comes from [`crate::detmath`] instead of the platform's libm so every platform gets the same
//! bytes; nearest-neighbour output is byte for byte `image`'s. The tests compare the two directly.

use std::f32::consts::PI;

use image::RgbaImage;
use rayon::prelude::*;

use crate::detmath;

/// The window of one output coordinate: the first source index and the normalized weights.
type Taps = (usize, Vec<f32>);

/// `image`'s Lanczos kernel with a window of 3.
fn lanczos3(x: f32) -> f32 {
    fn sinc(t: f32) -> f32 {
        let a = t * PI;
        if t == 0.0 { 1.0 } else { detmath::sin(a) / a }
    }
    if x.abs() < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// Lanczos3 windows for resampling `src_len` pixels to `dst_len`, as `image` computes them.
fn lanczos3_taps(src_len: u32, dst_len: u32) -> Vec<Taps> {
    let ratio = src_len as f32 / dst_len as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let src_support = 3.0 * sratio;
    (0..dst_len)
        .map(|out| {
            let input = (out as f32 + 0.5) * ratio;
            let left = ((input - src_support).floor() as i64).clamp(0, i64::from(src_len) - 1);
            let right = ((input + src_support).ceil() as i64).clamp(left + 1, i64::from(src_len));
            let input = input - 0.5;
            let mut weights: Vec<f32> = (left..right)
                .map(|i| lanczos3((i as f32 - input) / sratio))
                .collect();
            let mut sum = 0.0;
            for &w in &weights {
                sum += w;
            }
            for w in &mut weights {
                *w /= sum;
            }
            (left as usize, weights)
        })
        .collect()
}

/// `imageops::resize(src, width, height, FilterType::Lanczos3)` with a deterministic sine.
#[must_use]
pub fn resize_lanczos3(src: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if src.width() == 0 || src.height() == 0 || (width, height) == src.dimensions() {
        return image::imageops::resize(src, width, height, image::imageops::FilterType::Lanczos3);
    }
    let rows = lanczos3_taps(src.height(), height);
    let columns = lanczos3_taps(src.width(), width);
    let src_stride = src.width() as usize * 4;
    let raw = src.as_raw();
    let mut out = vec![0u8; width as usize * height as usize * 4];
    out.par_chunks_mut(width as usize * 4)
        .zip(&rows)
        .for_each_init(
            || vec![0.0f32; src_stride],
            |sums, (out_row, (top, row_weights))| {
                sums.fill(0.0);
                for (k, &w) in row_weights.iter().enumerate() {
                    let src_row = &raw[(top + k) * src_stride..][..src_stride];
                    for (sum, &v) in sums.iter_mut().zip(src_row) {
                        *sum += f32::from(v) * w;
                    }
                }
                for (px, (left, column_weights)) in
                    out_row.as_chunks_mut::<4>().0.iter_mut().zip(&columns)
                {
                    let mut t = [0.0f32; 4];
                    for (k, &w) in column_weights.iter().enumerate() {
                        let s = &sums[(left + k) * 4..][..4];
                        t[0] += s[0] * w;
                        t[1] += s[1] * w;
                        t[2] += s[2] * w;
                        t[3] += s[3] * w;
                    }
                    *px = t.map(|v| v.clamp(0.0, 255.0).round() as u8);
                }
            },
        );
    RgbaImage::from_raw(width, height, out).unwrap_or_else(|| RgbaImage::new(width, height))
}

/// The single source index `image`'s nearest filter reads for each output coordinate.
fn nearest_indices(src_len: u32, dst_len: u32) -> Vec<usize> {
    let ratio = src_len as f32 / dst_len as f32;
    (0..dst_len)
        .map(|out| {
            let input = (out as f32 + 0.5) * ratio;
            (input.floor() as i64).clamp(0, i64::from(src_len) - 1) as usize
        })
        .collect()
}

/// `imageops::resize(src, width, height, FilterType::Nearest)`: with a box kernel of zero support
/// every output pixel reads exactly one source pixel with weight 1, so this is a copy.
#[must_use]
pub fn resize_nearest(src: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if src.width() == 0 || src.height() == 0 || (width, height) == src.dimensions() {
        return image::imageops::resize(src, width, height, image::imageops::FilterType::Nearest);
    }
    let rows = nearest_indices(src.height(), height);
    let columns = nearest_indices(src.width(), width);
    let src_pixels = src.as_raw().as_chunks::<4>().0;
    let src_width = src.width() as usize;
    let mut out = vec![0u8; width as usize * height as usize * 4];
    out.par_chunks_mut(width as usize * 4)
        .zip(&rows)
        .for_each(|(out_row, &y)| {
            let src_row = &src_pixels[y * src_width..][..src_width];
            for (px, &x) in out_row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
                *px = src_row[x];
            }
        });
    RgbaImage::from_raw(width, height, out).unwrap_or_else(|| RgbaImage::new(width, height))
}

#[cfg(test)]
mod tests {
    use image::imageops::{FilterType, resize};

    use super::*;
    use crate::palette::SplitMix64;

    fn noise(width: u32, height: u32, seed: u64) -> RgbaImage {
        let mut rng = SplitMix64::new(seed);
        RgbaImage::from_fn(width, height, |x, y| {
            let v = rng.next_u64().to_le_bytes();
            // Smooth gradients with noise on top, and some fully transparent pixels.
            let base = ((x * 7 + y * 3) % 256) as u8;
            image::Rgba([
                base.wrapping_add(v[0] % 32),
                v[1],
                255 - base,
                if v[3] < 20 { 0 } else { v[4] },
            ])
        })
    }

    /// `image` computes its Lanczos weights with the platform's `sinf` and this module with
    /// [`detmath::sin`], so a weight can differ in its last bit and move a rounded byte by one.
    fn assert_lanczos_matches(ours: &RgbaImage, theirs: &RgbaImage, what: &str) {
        assert_eq!(ours.dimensions(), theirs.dimensions(), "{what}");
        let mut changed = 0;
        for (a, b) in ours.as_raw().iter().zip(theirs.as_raw()) {
            assert!(a.abs_diff(*b) <= 1, "{what}: {a} vs {b}");
            changed += usize::from(a != b);
        }
        assert!(
            changed * 1000 <= ours.as_raw().len(),
            "{what}: {changed} bytes differ"
        );
    }

    #[test]
    fn resizes_match_image_crate() {
        let sizes = [
            (1, 1),
            (1, 7),
            (9, 1),
            (2, 3),
            (13, 11),
            (64, 48),
            (101, 67),
            (240, 160),
            (333, 517),
        ];
        for (seed, &(sw, sh)) in (1..).zip(&sizes) {
            let src = noise(sw, sh, seed);
            for &(dw, dh) in &sizes {
                for (dw, dh) in [(dw, dh), (dw * 3, dh), (dw, dh * 2 + 1)] {
                    assert_lanczos_matches(
                        &resize_lanczos3(&src, dw, dh),
                        &resize(&src, dw, dh, FilterType::Lanczos3),
                        &format!("lanczos3 {sw}x{sh} -> {dw}x{dh}"),
                    );
                    assert_eq!(
                        resize_nearest(&src, dw, dh).as_raw(),
                        resize(&src, dw, dh, FilterType::Nearest).as_raw(),
                        "nearest {sw}x{sh} -> {dw}x{dh}"
                    );
                }
            }
        }
    }

    #[test]
    fn large_ratio_resizes_match_image_crate() {
        let src = noise(1500, 997, 77);
        for (dw, dh) in [
            (500, 332),
            (375, 249),
            (1499, 996),
            (1501, 998),
            (3000, 1994),
            (17, 1000),
        ] {
            assert_lanczos_matches(
                &resize_lanczos3(&src, dw, dh),
                &resize(&src, dw, dh, FilterType::Lanczos3),
                &format!("lanczos3 -> {dw}x{dh}"),
            );
            assert_eq!(
                resize_nearest(&src, dw, dh).as_raw(),
                resize(&src, dw, dh, FilterType::Nearest).as_raw(),
                "nearest -> {dw}x{dh}"
            );
        }
    }
}
