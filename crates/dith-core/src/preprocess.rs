//! Image resizing, tonal adjustment, sharpening/blurring, and alpha handling before dithering.

use image::RgbaImage;
use std::borrow::Cow;

use image::imageops;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color::{Rgb, linear_to_srgb_u8, srgb_u8_to_linear};
use crate::detmath;
use crate::resample::{resize_lanczos3, resize_nearest};

/// How to fit an image when both `--width` and `--height` are specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum FitMode {
    /// Fit inside target dimensions while preserving aspect ratio.
    #[default]
    Contain,
    /// Fill target dimensions by center-cropping while preserving aspect ratio.
    Cover,
    /// Stretch directly to exact target dimensions.
    Stretch,
}

/// How to treat transparent pixels during dithering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum AlphaMode {
    /// Keep original alpha channel intact; fully transparent pixels do not diffuse error.
    #[default]
    Preserve,
    /// Composite transparent pixels onto a solid background color before dithering.
    Flatten,
    /// Hard-threshold alpha at 50% (crisp 1-bit transparency mask).
    Threshold,
    /// Dither the alpha channel itself using an ordered Bayer 4x4 screen for stippled fadeouts!
    Dither,
}

/// Complete pre-processing configuration applied before the dithering pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreprocessOptions {
    /// Target output width in pixels (before pixel-size quantization).
    pub width: Option<u32>,
    /// Target output height in pixels (before pixel-size quantization).
    pub height: Option<u32>,
    /// Cap the longest side to at most this many pixels while preserving aspect ratio.
    pub max_dim: Option<u32>,
    /// Uniform scale multiplier (e.g. `0.5` or `2.0`).
    pub scale: Option<f32>,
    /// Fit behavior when both `width` and `height` are specified.
    pub fit: FitMode,
    /// Chunky pixel scale factor (`1` = native pixels, `2..=32` = dither at 1/N then nearest upscale).
    pub pixel_size: u32,
    /// Brightness shift in `[-1.0, 1.0]`.
    pub brightness: f32,
    /// Contrast adjustment in `[-1.0, 1.0]`.
    pub contrast: f32,
    /// Gamma exponent (`> 0.0`, `1.0` = neutral, `< 1.0` brightens midtones, `> 1.0` darkens).
    pub gamma: f32,
    /// Saturation multiplier (`0.0` = grayscale, `1.0` = neutral, `2.0` = vivid).
    pub saturation: f32,
    /// Unsharp mask sharpening intensity (`0.0` = off, `0.5..=2.0` = crisp edges for 1-bit dithers).
    pub sharpen: f32,
    /// Pre-dither Gaussian blur sigma (`0.0` = off).
    pub blur: f32,
    /// Stretch luminance histogram (1st..99th percentile) so shadows and highlights breathe.
    pub auto_levels: bool,
    /// Convert to linear luminance grayscale before dithering.
    pub grayscale: bool,
    /// Invert tonal values before dithering.
    pub invert: bool,
    /// Alpha handling mode.
    pub alpha_mode: AlphaMode,
    /// Background color used when `alpha_mode == AlphaMode::Flatten`.
    pub background: Rgb,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            max_dim: None,
            scale: None,
            fit: FitMode::Contain,
            pixel_size: 1,
            brightness: 0.0,
            contrast: 0.0,
            gamma: 1.0,
            saturation: 1.0,
            sharpen: 0.0,
            blur: 0.0,
            auto_levels: false,
            grayscale: false,
            invert: false,
            alpha_mode: AlphaMode::Preserve,
            background: Rgb::WHITE,
        }
    }
}

/// Output of pre-processing: the working image to dither plus final output dimensions.
#[derive(Debug, Clone)]
pub struct PreparedImage {
    /// Image ready for the dithering pass (possibly downscaled by `pixel_size`).
    pub working: RgbaImage,
    /// Final output dimensions after any `pixel_size` nearest-neighbor upscale.
    pub final_width: u32,
    pub final_height: u32,
}

/// Compute target final dimensions without actually allocating or resizing pixels (used by `--dry-run`).
#[must_use]
pub fn plan_dimensions(src_w: u32, src_h: u32, opts: &PreprocessOptions) -> (u32, u32) {
    let (mut w, mut h) = (src_w.max(1), src_h.max(1));

    if let Some(scale) = opts.scale {
        let s = scale.clamp(0.01, 16.0);
        w = ((w as f32) * s).round().max(1.0) as u32;
        h = ((h as f32) * s).round().max(1.0) as u32;
    }

    match (opts.width, opts.height) {
        (Some(tw), Some(th)) => match opts.fit {
            FitMode::Stretch | FitMode::Cover => {
                w = tw.max(1);
                h = th.max(1);
            }
            FitMode::Contain => {
                let ratio = (tw as f32 / w as f32).min(th as f32 / h as f32);
                w = ((w as f32) * ratio).round().max(1.0) as u32;
                h = ((h as f32) * ratio).round().max(1.0) as u32;
            }
        },
        (Some(tw), None) => {
            let ratio = tw as f32 / w as f32;
            w = tw.max(1);
            h = ((h as f32) * ratio).round().max(1.0) as u32;
        }
        (None, Some(th)) => {
            let ratio = th as f32 / h as f32;
            h = th.max(1);
            w = ((w as f32) * ratio).round().max(1.0) as u32;
        }
        (None, None) => {}
    }

    if let Some(max_d) = opts.max_dim {
        let max_d = max_d.max(1);
        let longest = w.max(h);
        if longest > max_d {
            let ratio = max_d as f32 / longest as f32;
            w = ((w as f32) * ratio).round().max(1.0) as u32;
            h = ((h as f32) * ratio).round().max(1.0) as u32;
        }
    }

    (w.max(1), h.max(1))
}

/// Resize, apply alpha flattening, tonal adjustments, blur/sharpen, and downscale for `pixel_size`.
#[must_use]
pub fn prepare_image(src: &RgbaImage, opts: &PreprocessOptions) -> PreparedImage {
    prepare(Cow::Borrowed(src), opts)
}

/// [`prepare_image`], working in place on `src` when no resize is needed instead of copying it.
pub(crate) fn prepare(src: Cow<'_, RgbaImage>, opts: &PreprocessOptions) -> PreparedImage {
    let (final_w, final_h) = plan_dimensions(src.width(), src.height(), opts);

    // Resize to final target first (handling cover crop cleanly).
    let mut img = if final_w == src.width() && final_h == src.height() {
        src.into_owned()
    } else if matches!(
        (opts.width, opts.height, opts.fit),
        (Some(_), Some(_), FitMode::Cover)
    ) {
        resize_cover(&src, final_w, final_h)
    } else {
        resize_lanczos3(&src, final_w, final_h)
    };

    // If pixel_size > 1, downscale to the coarse dither grid before tonal adjustments & dithering.
    let ps = opts.pixel_size.max(1);
    if ps > 1 {
        let work_w = (final_w / ps).max(1);
        let work_h = (final_h / ps).max(1);
        img = resize_lanczos3(&img, work_w, work_h);
    }

    // Flatten alpha early so tonal/spatial filters don't bleed undefined RGB from transparent areas.
    if opts.alpha_mode == AlphaMode::Flatten {
        flatten_alpha(&mut img, opts.background);
    }

    if opts.blur > 0.01 {
        img = gaussian_blur(&img, opts.blur.clamp(0.1, 20.0));
    }

    if opts.sharpen > 0.01 {
        apply_unsharp_mask(&mut img, opts.sharpen.clamp(0.0, 5.0));
    }

    if opts.auto_levels {
        apply_auto_levels(&mut img);
    }

    let needs_tonal = opts.brightness.abs() > 1e-4
        || opts.contrast.abs() > 1e-4
        || (opts.gamma - 1.0).abs() > 1e-4
        || (opts.saturation - 1.0).abs() > 1e-4
        || opts.grayscale
        || opts.invert;

    if needs_tonal {
        apply_tonal_adjustments(&mut img, opts);
    }

    PreparedImage {
        working: img,
        final_width: final_w,
        final_height: final_h,
    }
}

/// Apply post-dither alpha mode and nearest-neighbor upscale when `pixel_size > 1`.
#[must_use]
pub fn finalize_image(
    mut dithered: RgbaImage,
    prepared: &PreparedImage,
    opts: &PreprocessOptions,
) -> RgbaImage {
    match opts.alpha_mode {
        AlphaMode::Preserve | AlphaMode::Flatten => {}
        AlphaMode::Threshold => {
            for px in dithered.pixels_mut() {
                px[3] = if px[3] >= 128 { 255 } else { 0 };
            }
        }
        AlphaMode::Dither => {
            // Crisp 4x4 Bayer screen over the alpha channel.
            const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
            for (x, y, px) in dithered.enumerate_pixels_mut() {
                let threshold = (f32::from(BAYER4[((y & 3) * 4 + (x & 3)) as usize]) + 0.5) / 16.0;
                let a = f32::from(px[3]) / 255.0;
                px[3] = if a >= threshold { 255 } else { 0 };
            }
        }
    }

    if dithered.width() != prepared.final_width || dithered.height() != prepared.final_height {
        resize_nearest(&dithered, prepared.final_width, prepared.final_height)
    } else {
        dithered
    }
}

fn resize_cover(src: &RgbaImage, target_w: u32, target_h: u32) -> RgbaImage {
    let ratio = (target_w as f32 / src.width() as f32).max(target_h as f32 / src.height() as f32);
    let scaled_w = ((src.width() as f32) * ratio).ceil().max(target_w as f32) as u32;
    let scaled_h = ((src.height() as f32) * ratio).ceil().max(target_h as f32) as u32;
    let scaled = resize_lanczos3(src, scaled_w, scaled_h);
    let x = (scaled_w - target_w) / 2;
    let y = (scaled_h - target_h) / 2;
    imageops::crop_imm(&scaled, x, y, target_w, target_h).to_image()
}

/// Separable Gaussian blur with clamped edges: the kernel size, tap order and rounding of
/// `image::imageops::blur`, but with its kernel from [`detmath::exp`] and a separate multiply and
/// add, where `image` uses the platform's `expf` and fuses the multiply-add on some targets only.
fn gaussian_blur(img: &RgbaImage, sigma: f32) -> RgbaImage {
    let kernel = gaussian_kernel(sigma);
    let len = kernel.len();
    let half = len / 2;
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return img.clone();
    }
    let stride = w * 4;

    let mut rows = vec![0.0f32; stride * h];
    rows.par_chunks_exact_mut(stride)
        .zip(img.as_raw().par_chunks_exact(stride))
        .for_each_init(
            || vec![0.0f32; stride + 8 * half],
            |padded, (dst, src)| {
                for (x, px) in padded.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let from = &src[x.saturating_sub(half).min(w - 1) * 4..][..4];
                    *px = [from[0], from[1], from[2], from[3]].map(f32::from);
                }
                for (out, &v) in dst.iter_mut().zip(&padded[half * 4..]) {
                    *out = v * kernel[half];
                }
                for (i, &k) in kernel[..half].iter().enumerate() {
                    let fw = &padded[i * 4..];
                    let bw = &padded[(len - 1 - i) * 4..];
                    for ((out, &a), &b) in dst.iter_mut().zip(fw).zip(bw) {
                        *out += (a + b) * k;
                    }
                }
            },
        );

    let row = |y: usize| &rows[y.saturating_sub(half).min(h - 1) * stride..][..stride];
    let mut out = vec![0u8; stride * h];
    out.par_chunks_exact_mut(stride).enumerate().for_each_init(
        || vec![0.0f32; stride],
        |acc, (y, dst)| {
            for (a, &v) in acc.iter_mut().zip(row(y + half)) {
                *a = v * kernel[half];
            }
            for (i, &k) in kernel[..half].iter().enumerate() {
                for ((a, &f), &b) in acc.iter_mut().zip(row(y + i)).zip(row(y + len - 1 - i)) {
                    *a += (f + b) * k;
                }
            }
            for (px, &v) in dst.iter_mut().zip(acc.iter()) {
                *px = v.round().clamp(0.0, 255.0) as u8;
            }
        },
    );
    RgbaImage::from_raw(img.width(), img.height(), out).unwrap_or_else(|| img.clone())
}

/// Normalized Gaussian kernel, sized and scaled as `image` 0.25 sizes and scales its own.
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let size = (((((sigma - 0.8) / 0.3) + 1.0) * 2.0) + 1.0).max(3.0) as usize | 1;
    let scale = 1.0 / ((2.0 * std::f32::consts::PI).sqrt() * sigma);
    let mean = (size / 2) as f32;
    let mut kernel: Vec<f32> = (0..size)
        .map(|x| {
            let d = (x as f32 - mean) / sigma;
            detmath::exp(-0.5 * (d * d)) * scale
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    if sum != 0.0 {
        let inv = 1.0 / sum;
        for k in &mut kernel {
            *k *= inv;
        }
    }
    kernel
}

fn flatten_alpha(img: &mut RgbaImage, bg: Rgb) {
    let bg_lin = bg.to_linear();
    img.as_mut().par_chunks_mut(4 * 4096).for_each(|chunk| {
        for px in chunk.as_chunks_mut::<4>().0 {
            let a = f32::from(px[3]) / 255.0;
            if a >= 0.999 {
                px[3] = 255;
                continue;
            }
            let inv = 1.0 - a;
            let r = srgb_u8_to_linear(px[0]) * a + bg_lin.r * inv;
            let g = srgb_u8_to_linear(px[1]) * a + bg_lin.g * inv;
            let b = srgb_u8_to_linear(px[2]) * a + bg_lin.b * inv;
            *px = [
                linear_to_srgb_u8(r),
                linear_to_srgb_u8(g),
                linear_to_srgb_u8(b),
                255,
            ];
        }
    });
}

fn apply_unsharp_mask(img: &mut RgbaImage, amount: f32) {
    // Fast separable 3x3 Gaussian-like tent blur ([1, 2, 1] / 4) in integer math (~20x faster than float blur).
    let w = img.width() as usize;
    let h = img.height() as usize;
    if w < 3 || h < 3 {
        return;
    }
    let src = img.as_raw();
    let mut horiz = vec![0u8; w * h * 3];
    horiz
        .par_chunks_mut(w * 3)
        .zip(src.par_chunks(w * 4))
        .for_each(|(row_dst, row_src)| {
            for x in 0..w {
                let i0 = x.saturating_sub(1) * 4;
                let i1 = x * 4;
                let i2 = (x + 1).min(w - 1) * 4;
                for c in 0..3 {
                    let sum = u16::from(row_src[i0 + c])
                        + u16::from(row_src[i1 + c]) * 2
                        + u16::from(row_src[i2 + c]);
                    row_dst[x * 3 + c] = ((sum + 2) >> 2) as u8;
                }
            }
        });

    // The sharpened value depends only on the original and blurred bytes, so it is tabulated with
    // exactly the per-pixel expression: `sharpened[orig * 256 + blurred]`.
    let sharpened: Vec<u8> = (0..=u16::MAX)
        .map(|i| {
            let [orig, blurred] = i.to_be_bytes().map(f32::from);
            let sharp = orig + (orig - blurred) * amount;
            sharp.clamp(0.0, 255.0).round() as u8
        })
        .collect();

    img.as_mut()
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let r0 = &horiz[y.saturating_sub(1) * w * 3..][..w * 3];
            let r1 = &horiz[y * w * 3..][..w * 3];
            let r2 = &horiz[(y + 1).min(h - 1) * w * 3..][..w * 3];
            for x in 0..w {
                for c in 0..3 {
                    let h = x * 3 + c;
                    let blurred =
                        (u16::from(r0[h]) + u16::from(r1[h]) * 2 + u16::from(r2[h]) + 2) >> 2;
                    let d = x * 4 + c;
                    row[d] = sharpened[usize::from(row[d]) << 8 | usize::from(blurred)];
                }
            }
        });
}

fn apply_auto_levels(img: &mut RgbaImage) {
    let hist = img
        .as_raw()
        .par_chunks(4 * 16_384)
        .fold(
            || [0u32; 256],
            |mut hist, chunk| {
                for px in chunk.as_chunks::<4>().0 {
                    if px[3] < 16 {
                        continue;
                    }
                    let luma = (0.2126 * f32::from(px[0])
                        + 0.7152 * f32::from(px[1])
                        + 0.0722 * f32::from(px[2]))
                    .round()
                    .clamp(0.0, 255.0) as usize;
                    hist[luma] += 1;
                }
                hist
            },
        )
        .reduce(
            || [0u32; 256],
            |mut a, b| {
                for (into, from) in a.iter_mut().zip(b) {
                    *into += from;
                }
                a
            },
        );
    let total: u32 = hist.iter().sum();
    if total == 0 {
        return;
    }
    let low_cut = (total / 100).max(1);
    let high_cut = total.saturating_sub(low_cut);

    let mut acc = 0u32;
    let mut lo = 0u8;
    let mut hi = 255u8;
    for (i, &count) in hist.iter().enumerate() {
        acc += count;
        if acc >= low_cut {
            lo = i as u8;
            break;
        }
    }
    acc = 0;
    for (i, &count) in hist.iter().enumerate() {
        acc += count;
        if acc >= high_cut {
            hi = i as u8;
            break;
        }
    }
    if hi <= lo.saturating_add(4) {
        return;
    }

    let lo_f = f32::from(lo) / 255.0;
    let inv_span = 255.0 / f32::from(hi - lo);
    let levels: [u8; 256] = std::array::from_fn(|v| {
        let v = (f32::from(v as u8) / 255.0 - lo_f) * inv_span;
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    });
    apply_channel_table(img, &levels);
}

/// Replace the R, G and B bytes of every pixel through `table`.
fn apply_channel_table(img: &mut RgbaImage, table: &[u8; 256]) {
    img.as_mut().par_chunks_mut(4 * 16_384).for_each(|chunk| {
        for px in chunk.as_chunks_mut::<4>().0 {
            px[0] = table[usize::from(px[0])];
            px[1] = table[usize::from(px[1])];
            px[2] = table[usize::from(px[2])];
        }
    });
}

fn apply_tonal_adjustments(img: &mut RgbaImage, opts: &PreprocessOptions) {
    // Contrast curve factor: smooth perceptual contrast in [-1.0, 1.0].
    let c = opts.contrast.clamp(-1.0, 1.0);
    let contrast_factor = if c >= 0.0 {
        1.0 + c * 2.5
    } else {
        1.0 / (1.0 - c * 2.5)
    };
    let brightness = opts.brightness.clamp(-1.0, 1.0);
    let gamma = opts.gamma.clamp(0.1, 5.0);
    let sat = if opts.grayscale {
        0.0
    } else {
        opts.saturation.clamp(0.0, 3.0)
    };

    // Brightness, contrast and gamma act on each channel alone, so they are tabulated per byte.
    let curve: [f32; 256] = std::array::from_fn(|v| {
        let v = f32::from(v as u8) / 255.0;
        // Brightness + contrast around 0.5 midpoint.
        let v = ((v - 0.5) * contrast_factor + 0.5 + brightness).clamp(0.0, 1.0);
        // Gamma.
        if (gamma - 1.0).abs() > 1e-4 {
            detmath::powf(v, gamma)
        } else {
            v
        }
    });
    let finish = |v: f32| {
        let v = if opts.invert { 1.0 - v } else { v };
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };

    if (sat - 1.0).abs() <= 1e-4 {
        apply_channel_table(img, &curve.map(finish));
        return;
    }

    // Saturation / grayscale in linear light.
    let linear = curve.map(crate::color::srgb_f32_to_linear);
    let encode = SteppedByteMap::new(|v| finish(crate::color::linear_to_srgb_f32(v)));
    img.as_mut().par_chunks_mut(4 * 4096).for_each(|chunk| {
        for px in chunk.as_chunks_mut::<4>().0 {
            let lr = linear[usize::from(px[0])];
            let lg = linear[usize::from(px[1])];
            let lb = linear[usize::from(px[2])];
            let y = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
            if sat == 0.0 {
                // `y + (l - y) * 0.0` is exactly `y` for every channel: one conversion serves all three.
                let v = encode.get(y);
                px[0] = v;
                px[1] = v;
                px[2] = v;
            } else {
                px[0] = encode.get(y + (lr - y) * sat);
                px[1] = encode.get(y + (lg - y) * sat);
                px[2] = encode.get(y + (lb - y) * sat);
            }
        }
    });
}

/// A map from `f32` to `u8` that is monotonic on `[0, 1]`, constant below `2^-14` and above `1`,
/// and moves by at most one level inside each bucket of its table, which `new` checks. A bucket
/// spans `2^16` consecutive `f32` bit patterns (1/128 of an octave) and holds its first value and
/// the first input where the value steps, so a lookup is one load and one comparison and returns
/// exactly what the map returns.
struct SteppedByteMap {
    buckets: Vec<(f32, u8)>,
    rising: bool,
}

impl SteppedByteMap {
    const SHIFT: u32 = 16;
    /// Below this input the map must equal its value at zero.
    const LOWEST: f32 = 1.0 / 16_384.0;

    fn new(map: impl Fn(f32) -> u8) -> Self {
        let first = Self::LOWEST.to_bits() >> Self::SHIFT;
        let one = 1.0f32.to_bits();
        let rising = map(1.0) >= map(0.0);
        assert_eq!(
            map(Self::LOWEST),
            map(0.0),
            "map must be flat below {}",
            Self::LOWEST
        );
        let buckets = (first..=one >> Self::SHIFT)
            .map(|bucket| {
                let start = bucket << Self::SHIFT;
                let end = (start | ((1 << Self::SHIFT) - 1)).min(one);
                let value = map(f32::from_bits(start));
                let last = map(f32::from_bits(end));
                if last == value {
                    return (f32::INFINITY, value);
                }
                assert_eq!(
                    last.abs_diff(value),
                    1,
                    "more than one step in bucket {bucket:#x}"
                );
                assert_eq!(
                    last > value,
                    rising,
                    "map is not monotonic in bucket {bucket:#x}"
                );
                let (mut lo, mut hi) = (start, end);
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    if map(f32::from_bits(mid)) == value {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                (f32::from_bits(lo), value)
            })
            .collect();
        Self { buckets, rising }
    }

    #[inline]
    fn get(&self, v: f32) -> u8 {
        let v = v.clamp(f32::MIN_POSITIVE, 1.0);
        let index =
            (v.to_bits() >> Self::SHIFT).saturating_sub(Self::LOWEST.to_bits() >> Self::SHIFT);
        let (step, value) = self.buckets[index as usize];
        let stepped = u8::from(v >= step);
        if self.rising {
            value + stepped
        } else {
            value - stepped
        }
    }
}

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::*;

    #[test]
    fn stepped_byte_map_matches_the_srgb_encoding_it_replaces() {
        for invert in [false, true] {
            let direct = |v: f32| {
                let s = crate::color::linear_to_srgb_f32(v);
                let s = if invert { 1.0 - s } else { s };
                (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            };
            let map = SteppedByteMap::new(direct);
            let one = 1.0f32.to_bits();
            let mut probes: Vec<f32> = (0..=1_000_000u32)
                .map(|i| f32::from_bits((u64::from(one) * u64::from(i) / 1_000_000) as u32))
                .collect();
            for &(step, _) in map.buckets.iter().filter(|(s, _)| s.is_finite()) {
                probes.extend((0..5).map(|d| f32::from_bits(step.to_bits() + d - 2)));
            }
            probes.extend([-1.0, -0.0, 1.5, 3.0, f32::MIN_POSITIVE, 1e-30]);
            for v in probes {
                assert_eq!(map.get(v), direct(v), "invert {invert}, v {v:e}");
            }
        }
    }

    #[test]
    fn gaussian_blur_tracks_image_crate_blur() {
        let mut img = RgbaImage::new(37, 23);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = (x * 7 + y * 13) as u8;
            *px = Rgba([v, v.wrapping_mul(3), 255 - v, 200 + (x % 50) as u8]);
        }
        for sigma in [0.1, 0.8, 1.5, 2.37, 5.0, 12.0, 20.0] {
            let ours = gaussian_blur(&img, sigma);
            let theirs = imageops::blur(&img, sigma);
            for (a, b) in ours.as_raw().iter().zip(theirs.as_raw()) {
                assert!(a.abs_diff(*b) <= 1, "sigma {sigma}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn plan_dimensions_respects_aspect_and_max_dim() {
        let opts = PreprocessOptions {
            max_dim: Some(200),
            ..Default::default()
        };
        assert_eq!(plan_dimensions(800, 400, &opts), (200, 100));

        let opts_w = PreprocessOptions {
            width: Some(300),
            ..Default::default()
        };
        assert_eq!(plan_dimensions(600, 400, &opts_w), (300, 200));
    }

    #[test]
    fn pixel_size_downscales_and_restores_dimensions() {
        let src = RgbaImage::from_pixel(64, 48, Rgba([120, 130, 140, 255]));
        let opts = PreprocessOptions {
            pixel_size: 4,
            ..Default::default()
        };
        let prep = prepare_image(&src, &opts);
        assert_eq!(prep.working.dimensions(), (16, 12));
        let out = finalize_image(prep.working.clone(), &prep, &opts);
        assert_eq!(out.dimensions(), (64, 48));
    }
}
