//! Curated artistic color palettes, custom palette parsing, and Oklab k-means++ extraction.

use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::color::{ColorSpace, LinearRgb, Oklab, ParseColorError, Rgb};
use crate::detmath;
use crate::nearest::{Matcher, NearestGrid};

/// Error returned when resolving, parsing, or loading a color palette fails.
#[derive(Debug, Error)]
pub enum PaletteError {
    #[error("unknown built-in palette '{name}'")]
    UnknownPalette { name: String },
    #[error("palette must contain at least 2 colors (got {count})")]
    TooFewColors { count: usize },
    #[error("failed to parse palette color: {0}")]
    InvalidColor(#[from] ParseColorError),
    #[error("failed to read palette file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse palette file '{path}': {reason}")]
    ParseFile { path: String, reason: String },
}

/// Static metadata for a built-in curated palette.
#[derive(Debug, Clone, Serialize)]
pub struct BuiltinPaletteInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub tags: &'static [&'static str],
    pub colors: Vec<String>,
}

/// Resolved palette with precomputed color-space lookup tables for fast matching.
#[derive(Debug, Clone)]
pub struct Palette {
    pub name: String,
    pub description: String,
    rgb: Vec<Rgb>,
    srgb_f32: Vec<[f32; 3]>,
    linear: Vec<LinearRgb>,
    oklab: Vec<Oklab>,
    /// Per-channel spread estimate used to calibrate ordered/stochastic dither amplitude.
    ordered_spread_linear: [f32; 3],
    ordered_spread_srgb: [f32; 3],
    /// Lazily built nearest-color grids, one per color space, shared by clones of this palette.
    nearest_grids: [OnceLock<Arc<NearestGrid>>; 3],
}

impl Palette {
    /// Create a palette from a list of 8-bit sRGB colors.
    ///
    /// # Errors
    /// Returns [`PaletteError::TooFewColors`] if fewer than 2 colors are supplied.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        colors: Vec<Rgb>,
    ) -> Result<Self, PaletteError> {
        if colors.len() < 2 {
            return Err(PaletteError::TooFewColors {
                count: colors.len(),
            });
        }
        let srgb_f32: Vec<[f32; 3]> = colors.iter().map(|c| c.to_srgb_f32()).collect();
        let linear: Vec<LinearRgb> = colors.iter().map(|c| c.to_linear()).collect();
        let oklab: Vec<Oklab> = linear.iter().map(|c| c.to_oklab()).collect();

        let ordered_spread_linear = estimate_channel_spread(
            &linear
                .iter()
                .map(|c| c.to_array())
                .collect::<Vec<[f32; 3]>>(),
        );
        let ordered_spread_srgb = estimate_channel_spread(&srgb_f32);

        Ok(Self {
            name: name.into(),
            description: description.into(),
            rgb: colors,
            srgb_f32,
            linear,
            oklab,
            ordered_spread_linear,
            ordered_spread_srgb,
            nearest_grids: Default::default(),
        })
    }

    /// Number of colors in the palette.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rgb.len()
    }

    /// Whether the palette is empty (always false for valid palettes).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rgb.is_empty()
    }

    /// Borrow the 8-bit sRGB entries.
    #[must_use]
    pub fn colors(&self) -> &[Rgb] {
        &self.rgb
    }

    /// Return lowercase `#rrggbb` hex strings for every entry.
    #[must_use]
    pub fn hex_colors(&self) -> Vec<String> {
        self.rgb.iter().map(|c| c.to_hex()).collect()
    }

    /// Borrow linear RGB entries.
    #[must_use]
    pub fn linear_colors(&self) -> &[LinearRgb] {
        &self.linear
    }

    /// Borrow normalized sRGB entries.
    #[must_use]
    pub fn srgb_colors(&self) -> &[[f32; 3]] {
        &self.srgb_f32
    }

    pub(crate) fn oklab_colors(&self) -> &[Oklab] {
        &self.oklab
    }

    /// Fast lookups that return exactly what [`Self::nearest_index_from_linear`] (Oklab and
    /// linear) or [`Self::nearest_index_from_srgb`] (sRGB) return for the same sample.
    pub(crate) fn matcher(&self, space: ColorSpace) -> Matcher<'_> {
        let slot = match space {
            ColorSpace::Oklab => 0,
            ColorSpace::Linear => 1,
            ColorSpace::Srgb => 2,
        };
        let grid = (self.len() <= Matcher::MAX_PALETTE)
            .then(|| &**self.nearest_grids[slot].get_or_init(|| Arc::new(NearestGrid::new())));
        Matcher::new(self, space, grid)
    }

    /// Calibrated per-channel dither spread in the given color space.
    #[must_use]
    pub const fn ordered_spread(&self, space: ColorSpace) -> [f32; 3] {
        match space {
            ColorSpace::Oklab | ColorSpace::Linear => self.ordered_spread_linear,
            ColorSpace::Srgb => self.ordered_spread_srgb,
        }
    }

    /// Look up the index of the closest palette color for a linear RGB sample.
    #[inline]
    #[must_use]
    pub fn nearest_index_from_linear(&self, sample: LinearRgb, space: ColorSpace) -> usize {
        match space {
            ColorSpace::Oklab => {
                let target = sample.to_oklab();
                if self.oklab.len() == 2 {
                    return usize::from(
                        target.dist_sq(self.oklab[1]) < target.dist_sq(self.oklab[0]),
                    );
                }
                let mut best_idx = 0;
                let mut best_dist = f32::INFINITY;
                for (i, &cand) in self.oklab.iter().enumerate() {
                    let d = target.dist_sq(cand);
                    if d < best_dist {
                        best_dist = d;
                        best_idx = i;
                    }
                }
                best_idx
            }
            ColorSpace::Linear => {
                if self.linear.len() == 2 {
                    return usize::from(
                        sample.dist_sq(self.linear[1]) < sample.dist_sq(self.linear[0]),
                    );
                }
                let mut best_idx = 0;
                let mut best_dist = f32::INFINITY;
                for (i, &cand) in self.linear.iter().enumerate() {
                    let d = sample.dist_sq(cand);
                    if d < best_dist {
                        best_dist = d;
                        best_idx = i;
                    }
                }
                best_idx
            }
            ColorSpace::Srgb => {
                let rgb = sample.to_rgb().to_srgb_f32();
                self.nearest_index_from_srgb(rgb)
            }
        }
    }

    /// Look up the index of the closest palette color for a normalized sRGB `[0, 1]` sample.
    #[inline]
    #[must_use]
    pub fn nearest_index_from_srgb(&self, sample: [f32; 3]) -> usize {
        let mut best_idx = 0;
        let mut best_dist = f32::INFINITY;
        for (i, &cand) in self.srgb_f32.iter().enumerate() {
            let dr = sample[0] - cand[0];
            let dg = sample[1] - cand[1];
            let db = sample[2] - cand[2];
            let d = dr * dr + dg * dg + db * db;
            if d < best_dist {
                best_dist = d;
                best_idx = i;
            }
        }
        best_idx
    }

    /// Look up a built-in palette by slug (case-insensitive, underscores accepted as hyphens).
    ///
    /// # Errors
    /// Returns [`PaletteError::UnknownPalette`] if no built-in palette matches `name`.
    pub fn by_name(name: &str) -> Result<Self, PaletteError> {
        let key = normalize_slug(name);
        if let Some(def) = BUILTIN_PALETTES.iter().find(|p| p.id == key) {
            let colors = def.hexes.iter().map(|&h| Rgb::from_u32(h)).collect();
            return Self::new(def.id, def.description, colors);
        }
        if key == "rgb332" {
            return Self::rgb332();
        }
        if key == "web216" {
            return Self::web216();
        }
        if let Some(levels) = key
            .strip_prefix("gray")
            .and_then(|n| n.parse::<usize>().ok())
            && (2..=64).contains(&levels)
        {
            return Self::grayscale(levels);
        }
        Err(PaletteError::UnknownPalette {
            name: name.to_string(),
        })
    }

    /// Parse a comma- or whitespace-separated list of hex colors (e.g. `"#111111,#f5efe6"`).
    ///
    /// # Errors
    /// Returns an error if any color fails to parse or fewer than 2 colors are given.
    pub fn from_hex_list(spec: &str) -> Result<Self, PaletteError> {
        let colors: Vec<Rgb> = spec
            .split(|c: char| c == ',' || c.is_whitespace() || c == ';')
            .filter(|tok| !tok.is_empty())
            .map(str::parse::<Rgb>)
            .collect::<Result<_, _>>()?;
        Self::new("custom", "Custom hex color palette", colors)
    }

    /// Load a palette from a `.hex`, `.txt`, `.gpl` (GIMP Palette), or `.json` file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or contains fewer than 2 valid colors.
    pub fn from_file(path: &Path) -> Result<Self, PaletteError> {
        let raw = fs::read_to_string(path).map_err(|source| PaletteError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let display_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file-palette")
            .to_string();

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map_or_default(str::to_ascii_lowercase);

        let colors = if ext == "json" {
            parse_json_palette(&raw).map_err(|reason| PaletteError::ParseFile {
                path: path.display().to_string(),
                reason,
            })?
        } else if ext == "gpl" || raw.starts_with("GIMP Palette") {
            parse_gpl_palette(&raw).map_err(|reason| PaletteError::ParseFile {
                path: path.display().to_string(),
                reason,
            })?
        } else {
            raw.lines()
                .map(|line| line.split("//").next().unwrap_or(line).trim())
                .filter(|line| !line.is_empty() && !line.starts_with(';'))
                .flat_map(|line| line.split(|c: char| c == ',' || c.is_whitespace()))
                .filter(|tok| !tok.is_empty())
                .map(str::parse::<Rgb>)
                .collect::<Result<Vec<_>, _>>()?
        };

        Self::new(
            display_name,
            format!("Loaded from {}", path.display()),
            colors,
        )
    }

    /// Generate an evenly spaced perceptual grayscale ramp with `levels` tones.
    ///
    /// # Errors
    /// Returns [`PaletteError::TooFewColors`] if `levels < 2`.
    pub fn grayscale(levels: usize) -> Result<Self, PaletteError> {
        if levels < 2 {
            return Err(PaletteError::TooFewColors { count: levels });
        }
        let colors = (0..levels)
            .map(|i| {
                let v = ((i as f32 / (levels - 1) as f32) * 255.0 + 0.5) as u8;
                Rgb::new(v, v, v)
            })
            .collect();
        Self::new(
            format!("gray{levels}"),
            format!("{levels}-tone monochrome grayscale ramp"),
            colors,
        )
    }

    /// Classic 8-bit 3-3-2 RGB color cube (256 colors).
    ///
    /// # Errors
    /// Never fails in practice.
    pub fn rgb332() -> Result<Self, PaletteError> {
        let mut colors = Vec::with_capacity(256);
        for r in 0..8u8 {
            for g in 0..8u8 {
                for b in 0..4u8 {
                    colors.push(Rgb::new(
                        (u16::from(r) * 255 / 7) as u8,
                        (u16::from(g) * 255 / 7) as u8,
                        (u16::from(b) * 255 / 3) as u8,
                    ));
                }
            }
        }
        Self::new("rgb332", "8-bit 3-3-2 RGB color cube (256 colors)", colors)
    }

    /// Classic 216-color web-safe 6x6x6 cube.
    ///
    /// # Errors
    /// Never fails in practice.
    pub fn web216() -> Result<Self, PaletteError> {
        let steps = [0u8, 51, 102, 153, 204, 255];
        let mut colors = Vec::with_capacity(216);
        for &r in &steps {
            for &g in &steps {
                for &b in &steps {
                    colors.push(Rgb::new(r, g, b));
                }
            }
        }
        Self::new(
            "web216",
            "Classic 6x6x6 web-safe palette (216 colors)",
            colors,
        )
    }
}

/// Extract an adaptive `count`-color palette from an image using deterministic k-means++ in Oklab.
///
/// # Errors
/// Returns [`PaletteError::TooFewColors`] if `count < 2`.
pub fn extract_adaptive_palette(
    img: &RgbaImage,
    count: usize,
    seed: u64,
) -> Result<Palette, PaletteError> {
    extract_adaptive_palette_from_frames(std::slice::from_ref(img), count, seed)
}

/// Extract a single shared `count`-color palette across multiple animation frames so colors never
/// flicker or pop between frames.
///
/// # Errors
/// Returns [`PaletteError::TooFewColors`] if `count < 2`.
pub fn extract_adaptive_palette_from_frames(
    frames: &[RgbaImage],
    count: usize,
    seed: u64,
) -> Result<Palette, PaletteError> {
    let frames: Vec<&RgbaImage> = frames.iter().collect();
    extract_adaptive_palette_from_images(&frames, count, seed)
}

/// [`extract_adaptive_palette_from_frames`] over borrowed frames.
pub(crate) fn extract_adaptive_palette_from_images(
    frames: &[&RgbaImage],
    count: usize,
    seed: u64,
) -> Result<Palette, PaletteError> {
    if count < 2 {
        return Err(PaletteError::TooFewColors { count });
    }

    // Collect up to ~8192 strided opaque samples evenly distributed across all frames.
    let total_pixels: usize = frames
        .iter()
        .map(|img| (img.width() as usize) * (img.height() as usize))
        .sum();
    let stride = (total_pixels / 8192).max(1);
    let mut samples: Vec<Oklab> = Vec::with_capacity(total_pixels.min(8192));
    // Every pixel whose index across all frames is a multiple of `stride`, in order.
    let mut frame_start = 0usize;
    for img in frames {
        let pixels = img.as_raw().as_chunks::<4>().0;
        let first = (stride - frame_start % stride) % stride;
        for px in pixels.iter().skip(first).step_by(stride) {
            if px[3] >= 16 {
                samples.push(Rgb::new(px[0], px[1], px[2]).to_oklab());
            }
        }
        frame_start += pixels.len();
    }
    if samples.is_empty() {
        return Palette::by_name("bw");
    }

    let k = count.min(samples.len());
    let mut rng = SplitMix64::new(seed ^ 0x9E37_79B9_7F4A_7C15);

    // k-means++ initialization in Oklab.
    let mut centroids: Vec<Oklab> = Vec::with_capacity(k);
    let first = (rng.next_u64() % samples.len() as u64) as usize;
    let mut last_centroid = samples[first];
    centroids.push(last_centroid);

    let mut min_dists = vec![f32::INFINITY; samples.len()];
    for _ in 1..k {
        let mut sum_dist = 0.0f64;
        for (s, d) in samples.iter().zip(min_dists.iter_mut()) {
            let cand = s.dist_sq(last_centroid);
            if cand < *d {
                *d = cand;
            }
            sum_dist += f64::from(*d);
        }
        if sum_dist <= 1e-12 {
            break;
        }
        let threshold = rng.next_f64() * sum_dist;
        let mut acc = 0.0f64;
        let mut chosen = samples.len() - 1;
        for (idx, &d) in min_dists.iter().enumerate() {
            acc += f64::from(d);
            if acc >= threshold {
                chosen = idx;
                break;
            }
        }
        last_centroid = samples[chosen];
        centroids.push(last_centroid);
    }

    // Lloyd iterations.
    let mut sums = vec![[0.0f64; 3]; centroids.len()];
    let mut counts = vec![0usize; centroids.len()];
    for _ in 0..14 {
        sums.fill([0.0; 3]);
        counts.fill(0);

        for &sample in &samples {
            let mut best = 0;
            let mut best_d = f32::INFINITY;
            for (ci, &c) in centroids.iter().enumerate() {
                let d = sample.dist_sq(c);
                if d < best_d {
                    best_d = d;
                    best = ci;
                }
            }
            sums[best][0] += f64::from(sample.l);
            sums[best][1] += f64::from(sample.a);
            sums[best][2] += f64::from(sample.b);
            counts[best] += 1;
        }

        let mut max_shift = 0.0f32;
        for (ci, centroid) in centroids.iter_mut().enumerate() {
            if counts[ci] > 0 {
                let inv = 1.0 / (counts[ci] as f64);
                let next = Oklab::new(
                    (sums[ci][0] * inv) as f32,
                    (sums[ci][1] * inv) as f32,
                    (sums[ci][2] * inv) as f32,
                );
                max_shift = max_shift.max(centroid.dist_sq(next));
                *centroid = next;
            }
        }
        if max_shift < 1e-6 {
            break;
        }
    }

    // Sort dark-to-light by Oklab L and deduplicate identical 8-bit outputs.
    centroids.sort_by(|a, b| a.l.total_cmp(&b.l));
    let mut rgb_colors: Vec<Rgb> = Vec::with_capacity(centroids.len());
    for c in centroids {
        let rgb = c.to_rgb();
        if !rgb_colors.contains(&rgb) {
            rgb_colors.push(rgb);
        }
    }
    if rgb_colors.len() < 2 {
        rgb_colors = vec![Rgb::BLACK, Rgb::WHITE];
    }

    Palette::new(
        format!("auto-{count}"),
        format!(
            "Adaptive {}-color palette extracted via Oklab k-means++",
            rgb_colors.len()
        ),
        rgb_colors,
    )
}

/// Return metadata for every built-in curated palette.
#[must_use]
pub fn list_builtin_palettes() -> Vec<BuiltinPaletteInfo> {
    let mut list: Vec<BuiltinPaletteInfo> = BUILTIN_PALETTES
        .iter()
        .map(|p| BuiltinPaletteInfo {
            id: p.id,
            name: p.name,
            category: p.category,
            description: p.description,
            tags: p.tags,
            colors: p.hexes.iter().map(|&h| Rgb::from_u32(h).to_hex()).collect(),
        })
        .collect();

    list.push(BuiltinPaletteInfo {
        id: "gray4",
        name: "Grayscale 4",
        category: "monochrome",
        description: "4-tone evenly spaced grayscale ramp (any grayN for N=2..64 works)",
        tags: &["monochrome", "ramp", "print"],
        colors: Palette::grayscale(4).map_or_default(|p| p.hex_colors()),
    });
    list.push(BuiltinPaletteInfo {
        id: "gray8",
        name: "Grayscale 8",
        category: "monochrome",
        description: "8-tone monochrome grayscale ramp",
        tags: &["monochrome", "ramp", "photo"],
        colors: Palette::grayscale(8).map_or_default(|p| p.hex_colors()),
    });
    list.push(BuiltinPaletteInfo {
        id: "rgb332",
        name: "RGB 3-3-2",
        category: "cube",
        description: "Classic 256-color 8-bit 3-3-2 RGB color cube",
        tags: &["retro", "full-color", "cube"],
        colors: vec!["#000000".into(), "...254 more...".into(), "#ffffff".into()],
    });
    list.push(BuiltinPaletteInfo {
        id: "web216",
        name: "Web-Safe 216",
        category: "cube",
        description: "Classic 6x6x6 web-safe color cube",
        tags: &["retro", "web", "cube"],
        colors: vec!["#000000".into(), "...214 more...".into(), "#ffffff".into()],
    });

    list
}

/// Return all valid built-in palette IDs (used for agent error hints).
#[must_use]
pub fn builtin_palette_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = BUILTIN_PALETTES.iter().map(|p| p.id).collect();
    ids.extend(["gray4", "gray8", "gray16", "rgb332", "web216"]);
    ids
}

fn estimate_channel_spread(entries: &[[f32; 3]]) -> [f32; 3] {
    let n = entries.len().max(2) as f32;
    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        let mut vals: Vec<f32> = entries.iter().map(|c| c[ch]).collect();
        vals.sort_by(f32::total_cmp);
        let range = (vals[vals.len() - 1] - vals[0]).max(0.0);
        if range < 1e-4 {
            out[ch] = 0.0;
            continue;
        }
        // Estimate average step size along this axis. For multi-color palettes (like 16-color
        // palettes), the 3D color density scales roughly as N^(1/3), while for 1D ramps
        // (duotone / grayscale / Game Boy) it scales as N - 1. Blend using distinct gap median.
        let mut gaps: Vec<f32> = vals
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|&g| g > 0.015)
            .collect();
        if gaps.is_empty() {
            out[ch] = range / ((detmath::cbrt(f64::from(n)) as f32).max(1.0));
        } else {
            gaps.sort_by(f32::total_cmp);
            let median_gap = gaps[gaps.len() / 2];
            let cube_step = range / ((detmath::cbrt(f64::from(n)) as f32).max(1.0));
            out[ch] = median_gap.max(cube_step * 0.65).min(range);
        }
    }
    out
}

fn normalize_slug(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('_', "-")
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonPaletteFile {
    List(Vec<String>),
    Object { colors: Vec<String> },
}

fn parse_json_palette(raw: &str) -> Result<Vec<Rgb>, String> {
    let parsed: JsonPaletteFile =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON palette: {e}"))?;
    let hexes = match parsed {
        JsonPaletteFile::List(v) | JsonPaletteFile::Object { colors: v } => v,
    };
    hexes
        .iter()
        .map(|s| s.parse::<Rgb>().map_err(|e| e.to_string()))
        .collect()
}

fn parse_gpl_palette(raw: &str) -> Result<Vec<Rgb>, String> {
    let mut colors = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("GIMP")
            || trimmed.starts_with("Name:")
            || trimmed.starts_with("Columns:")
        {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let (Some(rs), Some(gs), Some(bs)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let r: u8 = rs
            .parse()
            .map_err(|_| format!("invalid red channel '{rs}'"))?;
        let g: u8 = gs
            .parse()
            .map_err(|_| format!("invalid green channel '{gs}'"))?;
        let b: u8 = bs
            .parse()
            .map_err(|_| format!("invalid blue channel '{bs}'"))?;
        colors.push(Rgb::new(r, g, b));
    }
    if colors.len() < 2 {
        return Err("fewer than 2 RGB entries found in GPL file".to_string());
    }
    Ok(colors)
}

/// Deterministic fast PRNG (`SplitMix64`) used for reproducible sampling and noise.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[must_use]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / ((1u64 << 53) as f64))
    }

    #[must_use]
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * (1.0 / ((1u32 << 24) as f32))
    }
}

struct BuiltinPaletteDef {
    id: &'static str,
    name: &'static str,
    category: &'static str,
    description: &'static str,
    tags: &'static [&'static str],
    hexes: &'static [u32],
}

const BUILTIN_PALETTES: &[BuiltinPaletteDef] = &[
    BuiltinPaletteDef {
        id: "bw",
        name: "1-Bit Black & White",
        category: "monochrome",
        description: "Pure crisp #000000 and #ffffff for classic 1-bit dithers",
        tags: &["1-bit", "monochrome", "classic", "high-contrast"],
        hexes: &[0x000000, 0xFFFFFF],
    },
    BuiltinPaletteDef {
        id: "mac-1bit",
        name: "Macintosh 1-Bit",
        category: "monochrome",
        description: "Warm CRT off-white and deep charcoal inspired by the 1984 Macintosh",
        tags: &["1-bit", "retro", "mac", "editorial"],
        hexes: &[0x141416, 0xEEF0E8],
    },
    BuiltinPaletteDef {
        id: "newsprint",
        name: "Newsprint Ink",
        category: "editorial",
        description: "Warm archival newsprint stock with rich charcoal press ink",
        tags: &["editorial", "print", "warm", "duotone"],
        hexes: &[0x1B1917, 0xF3ECE0],
    },
    BuiltinPaletteDef {
        id: "ink-cream",
        name: "India Ink & Cream",
        category: "editorial",
        description: "Deep obsidian ink on soft museum cream with a warm midtone wash",
        tags: &["editorial", "print", "tritone", "warm"],
        hexes: &[0x141210, 0x7A7065, 0xF7F1E3],
    },
    BuiltinPaletteDef {
        id: "risograph-blue",
        name: "Risograph Federal Blue",
        category: "print",
        description: "Classic Risograph soy blue ink printed on warm cream paper",
        tags: &["risograph", "print", "duotone", "editorial"],
        hexes: &[0x1D3B8B, 0xF6F1E5],
    },
    BuiltinPaletteDef {
        id: "risograph-red",
        name: "Risograph Bright Red",
        category: "print",
        description: "Vibrant Risograph vermilion-red ink on warm uncoated paper",
        tags: &["risograph", "print", "duotone", "bold"],
        hexes: &[0xD93829, 0xF7F2E7],
    },
    BuiltinPaletteDef {
        id: "risograph-duo",
        name: "Risograph Blue & Fluo Pink",
        category: "print",
        description: "Two-drum Risograph overprint: cobalt blue, fluorescent pink, deep plum, paper",
        tags: &["risograph", "print", "editorial", "vivid"],
        hexes: &[0x1A1B41, 0x2649B2, 0xFF4F8B, 0xF7F2E7],
    },
    BuiltinPaletteDef {
        id: "blueprint",
        name: "Cyanotype Blueprint",
        category: "editorial",
        description: "Architectural Prussian blue cyanotype with crisp chalk white",
        tags: &["duotone", "technical", "blue", "editorial"],
        hexes: &[0x0B2C5F, 0xF2F7FC],
    },
    BuiltinPaletteDef {
        id: "sepia",
        name: "Archival Sepia",
        category: "editorial",
        description: "4-tone darkroom sepia print from espresso shadow to parchment",
        tags: &["vintage", "photo", "warm", "editorial"],
        hexes: &[0x231710, 0x62432F, 0xAD8A68, 0xF4EAD8],
    },
    BuiltinPaletteDef {
        id: "amber-crt",
        name: "Amber Monochrome CRT",
        category: "retro",
        description: "4-level warm P3 amber phosphor terminal glow",
        tags: &["crt", "terminal", "retro", "dark"],
        hexes: &[0x120B03, 0x5C3304, 0xC97A08, 0xFFBE3B],
    },
    BuiltinPaletteDef {
        id: "phosphor-green",
        name: "Green Phosphor CRT",
        category: "retro",
        description: "4-level P1 green phosphor oscilloscope/terminal screen",
        tags: &["crt", "terminal", "retro", "dark"],
        hexes: &[0x051408, 0x134D21, 0x2FA34B, 0x8BFF9C],
    },
    BuiltinPaletteDef {
        id: "gameboy",
        name: "Game Boy DMG",
        category: "retro",
        description: "Authentic 4-shade olive-green original Game Boy LCD",
        tags: &["handheld", "retro", "pixel-art", "4-color"],
        hexes: &[0x0F380F, 0x306230, 0x8BAC0F, 0x9BBC0F],
    },
    BuiltinPaletteDef {
        id: "gameboy-pocket",
        name: "Game Boy Pocket",
        category: "retro",
        description: "Crisp silver-gray 4-shade Game Boy Pocket monochrome screen",
        tags: &["handheld", "retro", "pixel-art", "4-color"],
        hexes: &[0x1C1E1B, 0x575B54, 0x9CA397, 0xD8DFD1],
    },
    BuiltinPaletteDef {
        id: "cobalt-sand",
        name: "Cobalt & Sand",
        category: "editorial",
        description: "High-fashion midnight ink, electric cobalt, and warm desert sand",
        tags: &["editorial", "tritone", "modern", "web"],
        hexes: &[0x0F172A, 0x2563EB, 0xF5EFE6],
    },
    BuiltinPaletteDef {
        id: "vermilion-paper",
        name: "Vermilion & Sumi",
        category: "editorial",
        description: "Japanese woodblock palette: sumi black, vermilion seal red, washi paper",
        tags: &["editorial", "print", "tritone", "bold"],
        hexes: &[0x161618, 0xE03E2D, 0xF4EFE6],
    },
    BuiltinPaletteDef {
        id: "forest-mint",
        name: "Botanical Forest & Mint",
        category: "editorial",
        description: "Deep pine shadow, moss midtone, fresh mint highlight",
        tags: &["nature", "editorial", "green", "modern"],
        hexes: &[0x0C221A, 0x245D44, 0x7BC49B, 0xEBF7EE],
    },
    BuiltinPaletteDef {
        id: "sunset-dusk",
        name: "Sunset Dusk",
        category: "editorial",
        description: "Warm atmospheric twilight gradient from plum to apricot gold",
        tags: &["warm", "gradient", "moody", "artistic"],
        hexes: &[0x1A102B, 0x5E2450, 0xC94C4C, 0xF29E52, 0xFCE5B2],
    },
    BuiltinPaletteDef {
        id: "cyber-magenta",
        name: "Cyber Magenta",
        category: "modern",
        description: "Deep midnight violet, hot magenta, electric cyan, ice highlight",
        tags: &["neon", "synth", "dark", "vibrant"],
        hexes: &[0x0D071E, 0x5A189A, 0xFF2E97, 0x3BF4FB, 0xF8FAFC],
    },
    BuiltinPaletteDef {
        id: "cga",
        name: "IBM CGA Mode 4",
        category: "retro",
        description: "Iconic 4-color IBM CGA black, cyan, magenta, white",
        tags: &["retro", "pc", "4-color", "vivid"],
        hexes: &[0x000000, 0x55FFFF, 0xFF55FF, 0xFFFFFF],
    },
    BuiltinPaletteDef {
        id: "c64",
        name: "Commodore 64",
        category: "retro",
        description: "Classic 16-color Commodore 64 Pepto palette",
        tags: &["retro", "pixel-art", "16-color"],
        hexes: &[
            0x000000, 0xFFFFFF, 0x813338, 0x75CEC8, 0x8E3C97, 0x56AC4D, 0x2E2C9B, 0xEDF171,
            0x8E5029, 0x553800, 0xC46C71, 0x4A4A4A, 0x7B7B7B, 0xA9FF9F, 0x706DEB, 0xB2B2B2,
        ],
    },
    BuiltinPaletteDef {
        id: "pico8",
        name: "PICO-8",
        category: "retro",
        description: "Beloved 16-color PICO-8 fantasy console palette",
        tags: &["retro", "pixel-art", "16-color", "vibrant"],
        hexes: &[
            0x000000, 0x1D2B53, 0x7E2553, 0x008751, 0xAB5236, 0x5F574F, 0xC2C3C7, 0xFFF1E8,
            0xFF004D, 0xFFA300, 0xFFEC27, 0x00E436, 0x29ADFF, 0x83769C, 0xFF77A8, 0xFFCCAA,
        ],
    },
    BuiltinPaletteDef {
        id: "nord",
        name: "Nord",
        category: "theme",
        description: "Arctic cool-toned 12-color Nord design system palette",
        tags: &["theme", "cool", "modern", "ui"],
        hexes: &[
            0x2E3440, 0x3B4252, 0x4C566A, 0xD8DEE9, 0xECEFF4, 0x8FBCBB, 0x88C0D0, 0x81A1C1,
            0x5E81AC, 0xBF616A, 0xD08770, 0xEBCB8B, 0xA3BE8C, 0xB48EAD,
        ],
    },
    BuiltinPaletteDef {
        id: "gruvbox",
        name: "Gruvbox Warm",
        category: "theme",
        description: "Retro earthy warm Gruvbox palette with ochre, terracotta, and sage",
        tags: &["theme", "warm", "retro", "editorial"],
        hexes: &[
            0x1D2021, 0x3C3836, 0x665C54, 0xBDAE93, 0xFBF1C7, 0xCC241D, 0xD65D0E, 0xD79921,
            0x98971A, 0x689D6A, 0x458588, 0xB16286,
        ],
    },
    BuiltinPaletteDef {
        id: "catppuccin",
        name: "Catppuccin Mocha",
        category: "theme",
        description: "Soft pastel-on-espresso Catppuccin Mocha palette",
        tags: &["theme", "pastel", "modern", "dark"],
        hexes: &[
            0x11111B, 0x1E1E2E, 0x45475A, 0xBAC2DE, 0xCDD6F4, 0xF38BA8, 0xFAB387, 0xF9E2AF,
            0xA6E3A1, 0x89DCEB, 0x89B4FA, 0xCBA6F7,
        ],
    },
    BuiltinPaletteDef {
        id: "cmyk",
        name: "CMYK Process Print",
        category: "print",
        description: "Process printing inks: cyan, magenta, yellow, key black, and paper white",
        tags: &["print", "halftone", "bold", "graphic"],
        hexes: &[0x111111, 0x00AEEF, 0xEC008C, 0xFFF200, 0xFAFAFA],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_builtin_and_dynamic_palettes() {
        assert_eq!(Palette::by_name("bw").unwrap().len(), 2);
        assert_eq!(Palette::by_name("GAMEBOY").unwrap().len(), 4);
        assert_eq!(Palette::by_name("pico8").unwrap().len(), 16);
        assert_eq!(Palette::by_name("gray6").unwrap().len(), 6);
        assert_eq!(Palette::by_name("rgb332").unwrap().len(), 256);
        assert_eq!(Palette::by_name("web216").unwrap().len(), 216);
        assert!(Palette::by_name("does-not-exist").is_err());
    }

    #[test]
    fn parses_hex_list_and_requires_two_colors() {
        let p = Palette::from_hex_list("#111, #f5efe6, #2563eb").unwrap();
        assert_eq!(p.len(), 3);
        assert!(Palette::from_hex_list("#111111").is_err());
    }

    #[test]
    fn extracts_adaptive_palette_deterministically() {
        let mut img = RgbaImage::new(32, 32);
        for (x, y, px) in img.enumerate_pixels_mut() {
            if x < 16 && y < 16 {
                *px = image::Rgba([20, 40, 180, 255]);
            } else if x >= 16 && y < 16 {
                *px = image::Rgba([230, 60, 40, 255]);
            } else {
                *px = image::Rgba([240, 235, 220, 255]);
            }
        }
        let p1 = extract_adaptive_palette(&img, 3, 42).unwrap();
        let p2 = extract_adaptive_palette(&img, 3, 42).unwrap();
        assert_eq!(p1.colors(), p2.colors());
        assert_eq!(p1.len(), 3);
    }
}
