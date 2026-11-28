//! Solid ink and clean paper: force the darkest tones and the source's outlines to the darkest
//! palette color, and the lightest tones to the lightest, on top of any dither algorithm.
//!
//! The masks are computed from the prepared (pre-dither) image in integer fixed point, so they
//! depend only on the source pixels and these settings: renders are byte-identical run to run,
//! and animation frames with unchanged pixels get identical masks whatever the temporal mode.

use image::{Rgba, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color::Rgb;
use crate::detmath;
use crate::palette::Palette;

/// Long side, in pixels, at which outline detection blurs with a 3 px Gaussian.
const REFERENCE_LONG_SIDE: f64 = 3024.0;
/// Fixed-point scale for blur weights and blurred luminance.
const FIXED_ONE: u64 = 1 << 16;

/// Solid ink and paper settings layered over the dither. Every field is off when unset.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SolidInk {
    /// Tones darker than this luminance (`0.0..=1.0`) become solid ink, the darkest palette color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ink_below: Option<f32>,
    /// Tones lighter than this luminance (`0.0..=1.0`) stay clean paper, the lightest palette color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_above: Option<f32>,
    /// Outline sensitivity (`0.0..=1.0`, `0` = off): pixels clearly darker than their blurred
    /// surroundings become solid ink. Higher values pick up fainter lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ink_lines: Option<f32>,
}

impl SolidInk {
    /// No solid ink, no forced paper, no outlines.
    pub const OFF: Self = Self {
        ink_below: None,
        paper_above: None,
        ink_lines: None,
    };

    /// Whether any setting would change a render.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.ink_below.is_some_and(|t| t > 0.0)
            || self.paper_above.is_some_and(|t| t < 1.0)
            || self.ink_lines.is_some_and(|s| s > 0.0)
    }
}

/// Row-major per-pixel decisions for one image; `ink` wins over `paper` where both are set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InkMasks {
    pub ink: Vec<bool>,
    pub paper: Vec<bool>,
}

impl InkMasks {
    /// Compute the ink mask (dark tones OR outlines, cleaned with a 3x3 median) and the paper
    /// mask (light tones) from the prepared pre-dither image.
    #[must_use]
    pub fn compute(img: &RgbaImage, settings: &SolidInk) -> Self {
        let w = img.width() as usize;
        let h = img.height() as usize;
        let luma: Vec<u8> = img
            .as_raw()
            .par_chunks(4)
            .map(|px| luma_u8(Rgba([px[0], px[1], px[2], px[3]])))
            .collect();

        let mut ink = match settings.ink_below {
            Some(t) if t > 0.0 => luma.iter().map(|&l| f32::from(l) < t * 255.0).collect(),
            _ => vec![false; luma.len()],
        };
        if let Some(sensitivity) = settings.ink_lines.filter(|&s| s > 0.0) {
            for (dst, line) in ink.iter_mut().zip(outline_mask(&luma, w, h, sensitivity)) {
                *dst |= line;
            }
        }
        if ink.contains(&true) {
            ink = median3(&ink, w, h);
        }

        let paper = match settings.paper_above {
            Some(t) if t < 1.0 => luma.iter().map(|&l| f32::from(l) > t * 255.0).collect(),
            _ => vec![false; luma.len()],
        };

        Self { ink, paper }
    }
}

/// Force masked pixels of `dithered` to ink or paper. `source` is the image that was dithered.
pub fn apply_solid_ink(
    source: &RgbaImage,
    dithered: &mut RgbaImage,
    palette: &Palette,
    settings: &SolidInk,
) {
    if !settings.is_active() || palette.is_empty() {
        return;
    }
    let masks = InkMasks::compute(source, settings);
    let (ink_color, paper_color) = ink_and_paper(palette);
    let width = dithered.width() as usize;
    dithered
        .as_mut()
        .par_chunks_mut(width * 4)
        .zip(
            masks
                .ink
                .par_chunks(width)
                .zip(masks.paper.par_chunks(width)),
        )
        .for_each(|(row, (ink_row, paper_row))| {
            for ((px, &is_ink), &is_paper) in row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(ink_row)
                .zip(paper_row)
            {
                if px[3] == 0 {
                    continue;
                }
                let color = if is_ink {
                    ink_color
                } else if is_paper {
                    paper_color
                } else {
                    continue;
                };
                px[0] = color.r;
                px[1] = color.g;
                px[2] = color.b;
            }
        });
}

/// The darkest and lightest palette colors by Oklab lightness.
#[must_use]
pub fn ink_and_paper(palette: &Palette) -> (Rgb, Rgb) {
    let by_lightness = |a: &&Rgb, b: &&Rgb| a.lightness().total_cmp(&b.lightness());
    let colors = palette.colors();
    let ink = colors
        .iter()
        .min_by(by_lightness)
        .copied()
        .unwrap_or(Rgb::BLACK);
    let paper = colors
        .iter()
        .max_by(by_lightness)
        .copied()
        .unwrap_or(Rgb::WHITE);
    (ink, paper)
}

fn luma_u8(px: Rgba<u8>) -> u8 {
    let weighted = 2126 * u32::from(px[0]) + 7152 * u32::from(px[1]) + 722 * u32::from(px[2]);
    ((weighted + 5000) / 10_000) as u8
}

/// Pixels whose Gaussian-blurred surroundings are brighter than themselves by more than a
/// threshold that falls from 40/255 (sensitivity near 0) to 8/255 (sensitivity 1).
///
/// The blur is separable and clamp-to-edge in integer fixed point: the horizontal pass weighs
/// luminance by weights summing to [`FIXED_ONE`] (at most `255 * FIXED_ONE`, so it fits a
/// `u32`), and the vertical pass weighs those sums again and rounds back to one factor of
/// [`FIXED_ONE`]. Each output row is blurred and thresholded on its own.
fn outline_mask(luma: &[u8], w: usize, h: usize, sensitivity: f32) -> Vec<bool> {
    let sigma = (3.0 * w.max(h) as f64 / REFERENCE_LONG_SIDE).max(1.0);
    let kernel = gaussian_kernel(sigma);
    let radius = kernel.len() / 2;
    let threshold = 40.0 - 32.0 * f64::from(sensitivity.clamp(0.0, 1.0));
    let threshold_fixed = (threshold * FIXED_ONE as f64).round() as u64;

    let mut horizontal = vec![0u32; w * h];
    horizontal
        .par_chunks_mut(w)
        .zip(luma.par_chunks(w))
        .for_each(|(dst, row)| {
            for (x, out) in dst.iter_mut().enumerate() {
                let sum: u64 = kernel
                    .iter()
                    .enumerate()
                    .map(|(k, &wt)| wt * u64::from(row[(x + k).saturating_sub(radius).min(w - 1)]))
                    .sum();
                *out = sum as u32;
            }
        });

    let mut mask = vec![false; w * h];
    mask.par_chunks_mut(w).enumerate().for_each_init(
        || vec![0u64; w],
        |sums, (y, dst)| {
            sums.fill(0);
            for (k, &wt) in kernel.iter().enumerate() {
                let src = &horizontal[(y + k).saturating_sub(radius).min(h - 1) * w..][..w];
                for (sum, &v) in sums.iter_mut().zip(src) {
                    *sum += wt * u64::from(v);
                }
            }
            for ((out, &sum), &l) in dst.iter_mut().zip(sums.iter()).zip(&luma[y * w..][..w]) {
                let blurred = (sum + FIXED_ONE / 2) / FIXED_ONE;
                *out = blurred > u64::from(l) * FIXED_ONE + threshold_fixed;
            }
        },
    );
    mask
}

/// Integer Gaussian weights summing exactly to [`FIXED_ONE`].
fn gaussian_kernel(sigma: f64) -> Vec<u64> {
    let radius = (3.0 * sigma).ceil() as i64;
    let raw: Vec<f64> = (-radius..=radius)
        .map(|i| detmath::exp_f64(-((i * i) as f64) / (2.0 * sigma * sigma)))
        .collect();
    let total: f64 = raw.iter().sum();
    let mut weights: Vec<u64> = raw
        .iter()
        .map(|v| (v / total * FIXED_ONE as f64).round() as u64)
        .collect();
    let sum: u64 = weights.iter().sum();
    let center = radius as usize;
    weights[center] = weights[center] + FIXED_ONE - sum;
    weights
}

/// 3x3 clamp-to-edge median of a binary mask: set where at least 5 of the 9 cells are set.
fn median3(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut out = vec![false; w * h];
    out.par_chunks_mut(w).enumerate().for_each_init(
        || vec![0u8; w],
        |columns, (y, dst)| {
            // Set cells per column over the three clamped rows (an edge row counts twice).
            let rows = [y.saturating_sub(1), y, (y + 1).min(h - 1)].map(|r| &mask[r * w..][..w]);
            for (x, count) in columns.iter_mut().enumerate() {
                *count = u8::from(rows[0][x]) + u8::from(rows[1][x]) + u8::from(rows[2][x]);
            }
            for (x, out) in dst.iter_mut().enumerate() {
                let set = columns[x.saturating_sub(1)] + columns[x] + columns[(x + 1).min(w - 1)];
                *out = set >= 5;
            }
        },
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let v = f(x, y);
            Rgba([v, v, v, 255])
        })
    }

    fn horizontal_gradient() -> RgbaImage {
        gray(256, 32, |x, _| x as u8)
    }

    fn column(mask: &[bool], w: usize, x: usize) -> Vec<bool> {
        mask.iter().skip(x).step_by(w).copied().collect()
    }

    #[test]
    fn ink_below_inks_exactly_the_dark_end_of_a_gradient() {
        let settings = SolidInk {
            ink_below: Some(0.25),
            ..SolidInk::OFF
        };
        let masks = InkMasks::compute(&horizontal_gradient(), &settings);
        for x in 0..256 {
            let expected = (x as f32) < 0.25 * 255.0;
            assert!(
                column(&masks.ink, 256, x).iter().all(|&v| v == expected),
                "x={x}"
            );
        }
        assert!(!masks.paper.contains(&true));
    }

    #[test]
    fn paper_above_clears_exactly_the_light_end_of_a_gradient() {
        let settings = SolidInk {
            paper_above: Some(0.9),
            ..SolidInk::OFF
        };
        let masks = InkMasks::compute(&horizontal_gradient(), &settings);
        for x in 0..256 {
            let expected = (x as f32) > 0.9 * 255.0;
            assert!(
                column(&masks.paper, 256, x).iter().all(|&v| v == expected),
                "x={x}"
            );
        }
        assert!(!masks.ink.contains(&true));
    }

    #[test]
    fn tone_masks_follow_a_hard_edge() {
        let img = gray(64, 16, |x, _| if x < 32 { 20 } else { 200 });
        let settings = SolidInk {
            ink_below: Some(0.5),
            paper_above: Some(0.5),
            ..SolidInk::OFF
        };
        let masks = InkMasks::compute(&img, &settings);
        for x in 0..64 {
            assert!(
                column(&masks.ink, 64, x).iter().all(|&v| v == (x < 32)),
                "x={x}"
            );
            assert!(
                column(&masks.paper, 64, x).iter().all(|&v| v == (x >= 32)),
                "x={x}"
            );
        }
    }

    #[test]
    fn ink_lines_pick_up_a_dark_line_on_grey() {
        let img = gray(96, 64, |x, _| if (46..48).contains(&x) { 0 } else { 150 });
        let settings = SolidInk {
            ink_lines: Some(0.5),
            ..SolidInk::OFF
        };
        let masks = InkMasks::compute(&img, &settings);
        for x in 0..96 {
            let expected = (46..48).contains(&x);
            assert!(
                column(&masks.ink, 96, x).iter().all(|&v| v == expected),
                "x={x}"
            );
        }
    }

    #[test]
    fn ink_lines_ignore_a_smooth_gradient() {
        let settings = SolidInk {
            ink_lines: Some(1.0),
            ..SolidInk::OFF
        };
        assert!(
            !InkMasks::compute(&horizontal_gradient(), &settings)
                .ink
                .contains(&true)
        );
        let vertical = gray(32, 256, |_, y| y as u8);
        assert!(!InkMasks::compute(&vertical, &settings).ink.contains(&true));
    }

    #[test]
    fn higher_ink_lines_pick_up_fainter_lines() {
        let faint = gray(96, 64, |x, _| if (46..48).contains(&x) { 110 } else { 150 });
        let detects = |sensitivity: f32| {
            let settings = SolidInk {
                ink_lines: Some(sensitivity),
                ..SolidInk::OFF
            };
            InkMasks::compute(&faint, &settings).ink.contains(&true)
        };
        assert!(!detects(0.1));
        assert!(detects(0.9));
    }

    #[test]
    fn median_removes_isolated_specks() {
        let img = gray(32, 32, |x, y| if (x, y) == (10, 10) { 0 } else { 200 });
        let settings = SolidInk {
            ink_below: Some(0.5),
            ..SolidInk::OFF
        };
        assert!(!InkMasks::compute(&img, &settings).ink.contains(&true));
    }

    #[test]
    fn ink_and_paper_are_darkest_and_lightest_by_oklab_lightness() {
        let palette = Palette::new(
            "test",
            "test",
            vec![
                Rgb::from_u32(0x2649B2),
                Rgb::from_u32(0xF5EFE6),
                Rgb::from_u32(0x111111),
                Rgb::from_u32(0xE04040),
            ],
        )
        .unwrap();
        assert_eq!(
            ink_and_paper(&palette),
            (Rgb::from_u32(0x111111), Rgb::from_u32(0xF5EFE6))
        );
    }

    #[test]
    fn neutral_settings_are_inactive_and_unset_ones_serialize_to_nothing() {
        assert!(!SolidInk::OFF.is_active());
        let neutral = SolidInk {
            ink_below: Some(0.0),
            paper_above: Some(1.0),
            ink_lines: Some(0.0),
        };
        assert!(!neutral.is_active());
        assert_eq!(serde_json::to_string(&SolidInk::OFF).unwrap(), "{}");
    }
}
