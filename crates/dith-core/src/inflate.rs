//! The `inflate` generator: puff a flat shape up like a pillow and light it like `sphere`.
//!
//! Each filled shape becomes a signed distance field; several shapes merge with a smooth union
//! (`blend` pixels wide) so a head and ears read as one soft body. The height of the surface
//! follows the distance to the outline through a circular profile, so a disc inflates to exactly a
//! hemisphere, and that height field is smoothed, turned into normals and lit with the same diffuse,
//! specular and rim terms as `sphere`.

use std::path::Path;

use image::RgbaImage;
use image::imageops::{self, FilterType};
use serde::{Deserialize, Serialize};

use crate::detmath;
use crate::recipe::RecipeError;

/// Which channel of a shape image marks its filled area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum ShapeChannel {
    /// Opaque pixels are filled (SVG shapes, transparent PNGs).
    #[default]
    Alpha,
    /// Light pixels are filled (white-on-black masks).
    Luminance,
    /// Dark pixels are filled (black-on-white drawings).
    Darkness,
}

impl ShapeChannel {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Alpha => "alpha",
            Self::Luminance => "luminance",
            Self::Darkness => "darkness",
        }
    }

    fn coverage(self, px: image::Rgba<u8>) -> f32 {
        let a = f32::from(px[3]) / 255.0;
        let luma =
            (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2]))
                / 255.0;
        match self {
            Self::Alpha => a,
            Self::Luminance => luma * a,
            Self::Darkness => (1.0 - luma) * a,
        }
    }
}

/// Lighting and form controls for [`render_inflate`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InflateParams {
    /// Height of the puffed surface (`1.0` = a disc becomes a hemisphere).
    pub depth: f32,
    /// Strength of the glossy highlight.
    pub specular: f32,
    /// Strength of the rim light along the outline.
    pub rim: f32,
    /// Floor brightness of surfaces facing away from the light.
    pub ambient: f32,
    /// Width in pixels of the smooth union that melts several shapes into one body (`0` = hard).
    pub blend: f32,
    /// Darkness of a soft elliptical shadow under the shape (`0` = none).
    pub ground_shadow: f32,
}

impl Default for InflateParams {
    fn default() -> Self {
        Self {
            depth: 1.0,
            specular: 0.42,
            rim: 0.28,
            ambient: 0.08,
            blend: 0.0,
            ground_shadow: 0.0,
        }
    }
}

impl InflateParams {
    /// Check every control is inside its range.
    ///
    /// # Errors
    /// Returns [`RecipeError::Validation`] naming the first value out of range.
    pub fn validate(&self) -> Result<(), RecipeError> {
        for (field, value, lo, hi) in [
            ("depth", self.depth, 0.05, 4.0),
            ("specular", self.specular, 0.0, 2.0),
            ("rim", self.rim, 0.0, 2.0),
            ("ambient", self.ambient, 0.0, 1.0),
            ("blend", self.blend, 0.0, 512.0),
            ("ground-shadow", self.ground_shadow, 0.0, 1.0),
        ] {
            if !(lo..=hi).contains(&value) {
                return Err(RecipeError::Validation {
                    field,
                    reason: format!("must be in {lo:?}..={hi:?} (got {value})"),
                });
            }
        }
        Ok(())
    }
}

/// Shapes ready to inflate on a `width` x `height` canvas: one signed distance field per shape
/// (negative inside, in pixels) and an optional surface paint (`1.0` = unpainted).
#[derive(Debug, Clone, PartialEq)]
pub struct InflateShape {
    pub width: u32,
    pub height: u32,
    pub fields: Vec<Vec<f32>>,
    pub paint: Option<Vec<f32>>,
}

impl InflateShape {
    /// Build from per-shape coverage images already at canvas size.
    #[must_use]
    pub fn from_images(images: &[RgbaImage], channel: ShapeChannel) -> Self {
        let (width, height) = images.first().map_or((1, 1), RgbaImage::dimensions);
        let fields = images
            .iter()
            .map(|img| {
                let coverage: Vec<f32> = img.pixels().map(|&p| channel.coverage(p)).collect();
                signed_distance(&coverage, width as usize, height as usize)
            })
            .collect();
        Self {
            width,
            height,
            fields,
            paint: None,
        }
    }

    /// A centred ellipse filling 72% of the shorter side, used when no shape is given.
    #[must_use]
    pub fn default_disc(width: u32, height: u32) -> Self {
        let r = width.min(height) as f32 * 0.36;
        let (cx, cy) = ((width as f32 - 1.0) * 0.5, (height as f32 - 1.0) * 0.5);
        let field = (0..height)
            .flat_map(|y| (0..width).map(move |x| detmath::hypot(x as f32 - cx, y as f32 - cy) - r))
            .collect();
        Self {
            width,
            height,
            fields: vec![field],
            paint: None,
        }
    }

    /// Load shape files at canvas size: every top-level element of an SVG is its own shape, a
    /// raster file is one shape. `paint`'s luminance (over white) tints the lit surface.
    ///
    /// # Errors
    /// Returns [`RecipeError::InvalidAsset`] when a file is missing, unreadable or empty.
    pub fn load(
        shapes: &[&Path],
        channel: ShapeChannel,
        paint: Option<&Path>,
        width: u32,
        height: u32,
    ) -> Result<Self, RecipeError> {
        let mut images = Vec::new();
        for &path in shapes {
            images.extend(load_shape_images(path, width, height)?);
        }
        if images.is_empty() {
            return Err(RecipeError::InvalidAsset {
                kind: "shape",
                path: shapes
                    .first()
                    .map_or_else(String::new, |p| p.display().to_string()),
                reason: "holds no filled shapes".to_string(),
            });
        }
        let mut shape = Self::from_images(&images, channel);
        if let Some(path) = paint {
            let img = load_asset("paint", path, width, height)?;
            // Paint over white: transparent = unpainted (1.0), opaque black = fully dark (0.0).
            shape.paint = Some(
                img.pixels()
                    .map(|&p| 1.0 - ShapeChannel::Darkness.coverage(p))
                    .collect(),
            );
        }
        Ok(shape)
    }
}

fn asset_error(kind: &'static str, path: &Path, reason: impl std::fmt::Display) -> RecipeError {
    RecipeError::InvalidAsset {
        kind,
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

fn load_asset(
    kind: &'static str,
    path: &Path,
    width: u32,
    height: u32,
) -> Result<RgbaImage, RecipeError> {
    if crate::input::is_svg_path(path) {
        #[cfg(feature = "svg")]
        {
            let bytes = std::fs::read(path).map_err(|e| asset_error(kind, path, e))?;
            return crate::svg::rasterize(&bytes, width, height)
                .map_err(|e| asset_error(kind, path, format!("{e:#}")));
        }
        #[cfg(not(feature = "svg"))]
        return Err(asset_error(kind, path, "this build has no SVG support"));
    }
    let img = crate::input::load_image(path, None)
        .map_err(|e| asset_error(kind, path, format!("{e:#}")))?;
    Ok(if img.dimensions() == (width, height) {
        img
    } else {
        imageops::resize(&img, width, height, FilterType::Triangle)
    })
}

fn load_shape_images(path: &Path, width: u32, height: u32) -> Result<Vec<RgbaImage>, RecipeError> {
    #[cfg(feature = "svg")]
    if crate::input::is_svg_path(path) {
        let bytes = std::fs::read(path).map_err(|e| asset_error("shape", path, e))?;
        return crate::svg::rasterize_parts(&bytes, width, height)
            .map_err(|e| asset_error("shape", path, format!("{e:#}")));
    }
    Ok(vec![load_asset("shape", path, width, height)?])
}

/// Exact Euclidean distance (in pixels) from every pixel to the nearest `seed` pixel
/// (Felzenszwalb & Huttenlocher's separable squared-distance transform).
fn distance_to(seed: &[bool], w: usize, h: usize) -> Vec<f32> {
    const INF: f32 = 1e20;
    let mut grid: Vec<f32> = seed.iter().map(|&s| if s { 0.0 } else { INF }).collect();
    let mut line = vec![0.0f32; w.max(h)];
    let mut out = vec![0.0f32; w.max(h)];
    let mut v = vec![0usize; w.max(h)];
    let mut z = vec![0.0f32; w.max(h) + 1];
    let mut pass = |f: &[f32], d: &mut [f32], n: usize| {
        let mut k = 0usize;
        v[0] = 0;
        z[0] = -INF;
        z[1] = INF;
        for q in 1..n {
            let mut s;
            loop {
                let p = v[k];
                s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32))
                    / (2.0 * (q as f32 - p as f32));
                if s <= z[k] && k > 0 {
                    k -= 1;
                } else {
                    break;
                }
            }
            if s <= z[k] {
                v[0] = q;
                z[0] = -INF;
                z[1] = INF;
                k = 0;
                continue;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = INF;
        }
        k = 0;
        for (q, dq) in d.iter_mut().enumerate().take(n) {
            while z[k + 1] < q as f32 {
                k += 1;
            }
            let p = v[k];
            *dq = (q as f32 - p as f32).powi(2) + f[p];
        }
    };
    for x in 0..w {
        for y in 0..h {
            line[y] = grid[y * w + x];
        }
        pass(&line[..h], &mut out[..h], h);
        for y in 0..h {
            grid[y * w + x] = out[y];
        }
    }
    for y in 0..h {
        line[..w].copy_from_slice(&grid[y * w..(y + 1) * w]);
        pass(&line[..w], &mut out[..w], w);
        grid[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    grid.into_iter().map(f32::sqrt).collect()
}

/// Signed distance (negative inside) of an anti-aliased coverage mask, refined to sub-pixel
/// accuracy along the outline from the coverage itself.
fn signed_distance(coverage: &[f32], w: usize, h: usize) -> Vec<f32> {
    let inside: Vec<bool> = coverage.iter().map(|&c| c >= 0.5).collect();
    let outside: Vec<bool> = inside.iter().map(|&i| !i).collect();
    let to_inside = distance_to(&inside, w, h);
    let to_outside = distance_to(&outside, w, h);
    coverage
        .iter()
        .zip(&inside)
        .zip(to_inside.iter().zip(&to_outside))
        .map(|((&c, &is_in), (&di, &d_out))| {
            let d = if is_in { 0.5 - d_out } else { di - 0.5 };
            if d.abs() <= 1.0 && c > 0.0 && c < 1.0 {
                0.5 - c
            } else {
                d
            }
        })
        .collect()
}

fn smooth_min(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

/// Three box blurs approximate a Gaussian of `sigma` pixels.
fn blur(values: &mut [f32], w: usize, h: usize, sigma: f32) {
    let radius = ((sigma * sigma * 4.0 + 1.0).sqrt() * 0.5).round() as usize;
    if radius == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; values.len()];
    let norm = 1.0 / (2 * radius + 1) as f32;
    for _ in 0..3 {
        for y in 0..h {
            let row = &values[y * w..(y + 1) * w];
            let mut acc: f32 = (0..=2 * radius)
                .map(|i| row[i.saturating_sub(radius).min(w - 1)])
                .sum();
            for x in 0..w {
                tmp[y * w + x] = acc * norm;
                acc += row[(x + radius + 1).min(w - 1)] - row[x.saturating_sub(radius)];
            }
        }
        for x in 0..w {
            let mut acc: f32 = (0..=2 * radius)
                .map(|i| tmp[i.saturating_sub(radius).min(h - 1) * w + x])
                .sum();
            for y in 0..h {
                values[y * w + x] = acc * norm;
                acc += tmp[(y + radius + 1).min(h - 1) * w + x]
                    - tmp[y.saturating_sub(radius) * w + x];
            }
        }
    }
}

/// Render an inflated, lit shape as a tone map (`0.0` dark ..= `1.0` light). The background is
/// `1.0`, darkened only by the optional ground shadow. `phase` orbits the light like `sphere`.
#[must_use]
pub fn inflate_tones(
    shape: &InflateShape,
    angle_deg: f32,
    phase: f32,
    amp: f32,
    params: &InflateParams,
) -> Vec<f32> {
    let (w, h) = (shape.width as usize, shape.height as usize);
    let n = w * h;
    let field: Vec<f32> = (0..n)
        .map(|i| {
            shape
                .fields
                .iter()
                .map(|f| f[i])
                .reduce(|a, b| smooth_min(a, b, params.blend))
                .unwrap_or(f32::INFINITY)
        })
        .collect();

    let mut height: Vec<f32> = field.iter().map(|&d| (-d).max(0.0)).collect();
    let thickness = height.iter().copied().fold(0.0f32, f32::max);
    let mut tones = vec![1.0f32; n];
    if thickness <= 0.0 {
        return tones;
    }
    blur(&mut height, w, h, (thickness * 0.18).max(1.0));
    let peak = height.iter().copied().fold(0.0f32, f32::max).max(1e-6);
    for z in &mut height {
        let t = (*z / peak).min(1.0);
        *z = params.depth * thickness * (1.0 - (1.0 - t) * (1.0 - t)).sqrt();
    }

    let rad = (angle_deg + phase * 360.0).to_radians();
    let tilt = if phase == 0.0 {
        0.65
    } else {
        (0.65 + 0.14 * amp * detmath::sin(phase * std::f32::consts::TAU)).clamp(0.15, 0.92)
    };
    let (lx, ly) = (detmath::cos(rad) * tilt, -detmath::sin(rad) * tilt);
    let lz = (1.0 - lx * lx - ly * ly).max(0.1).sqrt();
    let hz = lz + 1.0;
    let inv_hlen = 1.0 / (lx * lx + ly * ly + hz * hz).sqrt();

    // Ground shadow: a soft ellipse centred under the shape's footprint.
    let (mut x0, mut x1, mut y1) = (w, 0usize, 0usize);
    for (i, _) in field.iter().enumerate().filter(|&(_, &d)| d < 0.0) {
        x0 = x0.min(i % w);
        x1 = x1.max(i % w);
        y1 = y1.max(i / w);
    }
    let span = (x1.saturating_sub(x0) + 1) as f32;
    let (scx, scy) = ((x0 + x1) as f32 * 0.5, y1 as f32 - span * 0.02);
    let (srx, sry) = (span * 0.46, span * 0.11);

    let at = |x: usize, y: usize| height[y.min(h - 1) * w + x.min(w - 1)];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut background = 1.0f32;
            if params.ground_shadow > 0.0 {
                let (u, v) = ((x as f32 - scx) / srx, (y as f32 - scy) / sry);
                background -= params.ground_shadow * detmath::exp(-(u * u + v * v) * 1.6);
            }
            let coverage = (0.5 - field[i]).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                tones[i] = background;
                continue;
            }
            let slope_x = (at(x + 1, y) - at(x.saturating_sub(1), y)) * 0.5;
            let slope_y = (at(x, y + 1) - at(x, y.saturating_sub(1))) * 0.5;
            let inv = 1.0 / (slope_x * slope_x + slope_y * slope_y + 1.0).sqrt();
            let (nx, ny, nz) = (-slope_x * inv, -slope_y * inv, inv);
            let diffuse = (nx * lx + ny * ly + nz * lz).max(0.0);
            let spec = detmath::powi(((nx * lx + ny * ly + nz * hz) * inv_hlen).max(0.0), 28);
            let rim = detmath::powf((1.0 - nz).max(0.0), 2.6);
            let lit = crate::generate::highlight_shoulder(
                params.ambient + diffuse * 0.78 + spec * params.specular + rim * params.rim,
            );
            let surface = lit * shape.paint.as_ref().map_or(1.0, |p| p[i]);
            tones[i] = background + (surface - background) * coverage;
        }
    }
    tones
}

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::*;

    fn disc(w: u32, h: u32, cx: f32, cy: f32, r: f32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let d = (x as f32 - cx).hypot(y as f32 - cy) - r;
            Rgba([0, 0, 0, ((0.5 - d).clamp(0.0, 1.0) * 255.0) as u8])
        })
    }

    #[test]
    fn distance_field_matches_a_disc() {
        let shape =
            InflateShape::from_images(&[disc(64, 64, 32.0, 32.0, 20.0)], ShapeChannel::Alpha);
        let f = &shape.fields[0];
        assert!((f[32 * 64 + 32] + 20.0).abs() < 1.0, "{}", f[32 * 64 + 32]);
        assert!((f[32 * 64 + 2] - 10.0).abs() < 1.0, "{}", f[32 * 64 + 2]);
        assert!(f[32 * 64 + 12].abs() < 0.6, "{}", f[32 * 64 + 12]);
    }

    #[test]
    fn inflated_disc_is_lit_from_the_light_side_and_casts_a_shadow() {
        let shape =
            InflateShape::from_images(&[disc(96, 96, 48.0, 44.0, 30.0)], ShapeChannel::Alpha);
        let params = InflateParams {
            ground_shadow: 0.4,
            ..InflateParams::default()
        };
        let t = inflate_tones(&shape, 135.0, 0.0, 1.0, &params);
        let at = |x: usize, y: usize| t[y * 96 + x];
        assert!(
            at(30, 26) > at(66, 62) + 0.3,
            "top-left is lit: {} vs {}",
            at(30, 26),
            at(66, 62)
        );
        assert!((at(2, 2) - 1.0).abs() < 1e-3, "background stays paper");
        assert!(at(48, 76) < 0.85, "shadow under the body: {}", at(48, 76));
        assert_eq!(
            t,
            inflate_tones(&shape, 135.0, 0.0, 1.0, &params),
            "deterministic"
        );
    }

    #[test]
    fn smooth_union_fills_the_crease_between_shapes() {
        let parts = [
            disc(80, 60, 28.0, 30.0, 14.0),
            disc(80, 60, 52.0, 30.0, 14.0),
        ];
        let hard = InflateShape::from_images(&parts, ShapeChannel::Alpha);
        let union = |k: f32| {
            let t = inflate_tones(
                &hard,
                135.0,
                0.0,
                1.0,
                &InflateParams {
                    blend: k,
                    ..InflateParams::default()
                },
            );
            t[18 * 80 + 40]
        };
        // Above the waist where the discs meet: background when hard, filled when blended.
        assert!((union(0.0) - 1.0).abs() < 1e-3);
        assert!(union(16.0) < 0.95, "{}", union(16.0));
    }

    #[test]
    fn params_validate_ranges() {
        assert!(InflateParams::default().validate().is_ok());
        let bad = InflateParams {
            depth: 0.0,
            ..InflateParams::default()
        };
        assert_eq!(bad.validate().unwrap_err().code(), "INVALID_ARGUMENT");
    }
}
